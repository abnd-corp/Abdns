//! Un serveur DoH : client HTTP/2 (ou HTTP/1.1 si le serveur ne propose que lui) par hyper,
//! sur le runtime d'`rt`, une connexion partagée par toutes les requêtes en cours.

use std::net::{IpAddr, SocketAddr};
use std::sync::RwLock;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE};
use hyper::{Request, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::{config, read, write};

const MEDIA_TYPE: &str = "application/dns-message";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Plus grand message DNS possible : au-delà, la réponse n'en est pas une.
const MAX_ANSWER: usize = 65_535;

type Https = Client<HttpsConnector<HttpConnector<Fixed>>, Full<Bytes>>;

/// Adresse d'un serveur nommé, prise dans la configuration : le client HTTPS ne fait jamais de
/// requête DNS (elle partirait en clair, ou reviendrait à abdns lui-même).
#[derive(Clone)]
struct Fixed(Option<IpAddr>);

impl tower_service::Service<Name> for Fixed {
    type Response = std::iter::Once<SocketAddr>;
    type Error = std::io::Error;
    type Future = std::future::Ready<std::io::Result<Self::Response>>;

    fn poll_ready(&mut self, _: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        // Port 0 : remplacé par celui de l'adresse du serveur.
        std::future::ready(
            self.0.map(|ip| std::iter::once(SocketAddr::new(ip, 0))).ok_or_else(|| std::io::Error::other(format!("no ip for {name}"))),
        )
    }
}

pub(super) struct Upstream {
    pub(super) name: String,
    url: Uri,
    ip: Option<IpAddr>,
    /// Remplacé quand le réseau change : les connexions de l'ancien sont mortes.
    client: RwLock<Https>,
    /// Jeton illisible, adresse invalide : le serveur reste dans la liste, mis de côté.
    problem: Option<String>,
}

impl Upstream {
    pub(super) fn new(server: &config::Server) -> Result<Self> {
        let client = RwLock::new(client(&server.name, server.ip)?);
        let url = server.full_url().and_then(|url| url.parse::<Uri>().with_context(|| format!("{}: invalid url", server.name)));
        let (url, problem) = match url {
            Ok(url) => (url, None),
            Err(error) => (Uri::from_static("https://invalid./"), Some(format!("{error:#}"))),
        };
        Ok(Self { name: server.name.clone(), url, ip: server.ip, client, problem })
    }

    pub(super) fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// Connexions neuves pour les requêtes suivantes ; celles en cours finissent sur l'ancien.
    pub(super) fn reconnect(&self) {
        if let Ok(client) = client(&self.name, self.ip) {
            *write(&self.client) = client;
        }
    }

    pub(super) async fn exchange(&self, query: &[u8]) -> Result<Vec<u8>> {
        if let Some(problem) = &self.problem {
            bail!("{problem}");
        }
        // Identifiant 0 (RFC 8484, §4.1) : mieux mis en cache par les serveurs.
        let mut body = query.to_vec();
        body[..2].fill(0);
        let request = Request::post(self.url.clone())
            .header(CONTENT_TYPE, MEDIA_TYPE)
            .header(ACCEPT, MEDIA_TYPE)
            .body(Full::new(Bytes::from(body)))
            .context("invalid request")?;
        // Les erreurs de hyper ne citent jamais l'adresse : le chemin secret reste hors du journal.
        let client = read(&self.client).clone();
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let response = client.request(request).await?;
            ensure!(response.status().is_success(), "HTTP {}", response.status().as_u16());
            let body = Limited::new(response.into_body(), MAX_ANSWER).collect().await.map_err(|e| anyhow!("{e}"))?;
            Ok(body.to_bytes().to_vec())
        })
        .await
        .map_err(|_| anyhow!("no answer within {} s", REQUEST_TIMEOUT.as_secs()))??;
        ensure!(reply.len() >= 12, "short answer");
        Ok(reply)
    }
}

fn client(name: &str, ip: Option<IpAddr>) -> Result<Https> {
    let mut http = HttpConnector::new_with_resolver(Fixed(ip));
    http.enforce_http(false);
    http.set_connect_timeout(Some(CONNECT_TIMEOUT));
    http.set_keepalive(Some(Duration::from_secs(30)));
    http.set_nodelay(true);
    // ring : se compile partout (Windows et musl compris) sans outil externe ; certificats
    // vérifiés par le système (trousseau, magasin de Windows, certificats de la distribution).
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_provider_and_platform_verifier(rustls::crypto::ring::default_provider())
        .with_context(|| format!("{name}: cannot build the TLS client"))?
        .https_only()
        .enable_all_versions()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_timer(TokioTimer::new())
        .timer(TokioTimer::new())
        .build(https))
}
