//! Client DNS-over-HTTPS (RFC 8484) : serveurs essayés dans l'ordre de la configuration, cache
//! qui respecte les durées de vie, réponse périmée en dernier recours (RFC 8767).
//!
//! Un serveur qui échoue est mis de côté (30 s, puis jusqu'à 5 min) : les requêtes suivantes
//! passent directement au suivant, et la garde le réessaie à chaque contrôle.
//!
//! HTTP/2 (ou HTTP/1.1 si le serveur ne le propose pas) par hyper, sur le runtime d'`rt` : une
//! connexion par serveur, partagée par toutes les requêtes en cours.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Bytes;
use hyper::header::{ACCEPT, CONTENT_TYPE};
use hyper::{Request, Uri};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::rt::{TokioExecutor, TokioTimer};

use crate::config::{self, Config};
use crate::{dns, lock, logbook, rt};

const MEDIA_TYPE: &str = "application/dns-message";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Plus grand message DNS possible : au-delà, la réponse n'en est pas une.
const MAX_ANSWER: usize = 65_535;
const DOWN_FIRST: u64 = 30;
const DOWN_MAX: u64 = 300;
const CACHE_MAX: usize = 20_000;
/// Une réponse périmée sert encore jusqu'à un jour si aucun serveur ne répond.
const STALE_FOR: u64 = 86_400;
const STALE_TTL: u32 = 30;
const NONE: usize = usize::MAX;

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

struct Upstream {
    name: String,
    url: Uri,
    client: Https,
}

#[derive(Default, Clone, Copy)]
struct Health {
    failures: u32,
    down_until: u64,
}

struct Entry {
    reply: Vec<u8>,
    stored: u64,
    ttl: u32,
}

pub struct Resolver {
    upstreams: Vec<Upstream>,
    health: Mutex<Vec<Health>>,
    cache: Mutex<HashMap<Vec<u8>, Entry>>,
    /// Dernier serveur qui a répondu, `NONE` avant la première réponse.
    last: AtomicUsize,
}

fn client(server: &config::Server) -> Result<Https> {
    let mut http = HttpConnector::new_with_resolver(Fixed(server.ip));
    http.enforce_http(false);
    http.set_connect_timeout(Some(CONNECT_TIMEOUT));
    http.set_keepalive(Some(Duration::from_secs(30)));
    http.set_nodelay(true);
    // ring : se compile partout (Windows et musl compris) sans outil externe ; certificats
    // vérifiés par le système (trousseau, magasin de Windows, certificats de la distribution).
    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_provider_and_platform_verifier(rustls::crypto::ring::default_provider())
        .with_context(|| format!("{}: cannot build the TLS client", server.name))?
        .https_only()
        .enable_all_versions()
        .wrap_connector(http);
    Ok(Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_timer(TokioTimer::new())
        .timer(TokioTimer::new())
        .build(https))
}

/// Même réponse, identifiant de la requête du client.
fn with_id(mut reply: Vec<u8>, query: &[u8]) -> Vec<u8> {
    reply[..2].copy_from_slice(&query[..2]);
    reply
}

impl Resolver {
    pub fn new(config: &Config) -> Result<Self> {
        let upstreams = config
            .server
            .iter()
            .map(|s| {
                let url = s.full_url()?.parse().with_context(|| format!("{}: invalid url", s.name))?;
                Ok(Upstream { name: s.name.clone(), url, client: client(s)? })
            })
            .collect::<Result<Vec<_>>>()?;
        let health = Mutex::new(vec![Health::default(); upstreams.len()]);
        Ok(Self { upstreams, health, cache: Mutex::new(HashMap::new()), last: AtomicUsize::new(NONE) })
    }

    pub fn name(&self, index: usize) -> &str {
        self.upstreams.get(index).map_or("?", |u| u.name.as_str())
    }

    /// Serveur qui a répondu en dernier.
    pub fn active(&self) -> Option<usize> {
        Some(self.last.load(Ordering::Relaxed)).filter(|&i| i != NONE)
    }

    /// (nom, disponible) pour chaque serveur, dans l'ordre.
    pub fn snapshot(&self) -> Vec<(String, bool)> {
        let now = logbook::now();
        let health = lock(&self.health).clone();
        self.upstreams.iter().zip(health).map(|(u, h)| (u.name.clone(), h.down_until <= now)).collect()
    }

    async fn exchange(&self, index: usize, query: &[u8]) -> Result<Vec<u8>> {
        let upstream = &self.upstreams[index];
        // Identifiant 0 (RFC 8484, §4.1) : mieux mis en cache par les serveurs.
        let mut body = query.to_vec();
        body[..2].fill(0);
        let request = Request::post(upstream.url.clone())
            .header(CONTENT_TYPE, MEDIA_TYPE)
            .header(ACCEPT, MEDIA_TYPE)
            .body(Full::new(Bytes::from(body)))
            .context("invalid request")?;
        // Les erreurs de hyper ne citent jamais l'adresse : le chemin secret reste hors du journal.
        let reply = tokio::time::timeout(REQUEST_TIMEOUT, async {
            let response = upstream.client.request(request).await?;
            ensure!(response.status().is_success(), "HTTP {}", response.status().as_u16());
            let body = Limited::new(response.into_body(), MAX_ANSWER).collect().await.map_err(|e| anyhow!("{e}"))?;
            Ok(body.to_bytes().to_vec())
        })
        .await
        .map_err(|_| anyhow!("no answer within {} s", REQUEST_TIMEOUT.as_secs()))??;
        ensure!(reply.len() >= 12, "short answer");
        Ok(reply)
    }

    fn succeeded(&self, index: usize) {
        let mut health = lock(&self.health);
        if health[index].failures > 0 {
            logbook::log(&format!("{} answers again", self.name(index)));
        }
        health[index] = Health::default();
        drop(health);
        self.last.store(index, Ordering::Relaxed);
    }

    fn failed(&self, index: usize, error: &anyhow::Error) {
        let mut health = lock(&self.health);
        let h = &mut health[index];
        if h.failures == 0 {
            logbook::log(&format!("{} unreachable: {error:#}", self.name(index)));
        }
        h.failures += 1;
        let wait = DOWN_FIRST.saturating_mul(1 << (h.failures - 1).min(4)).min(DOWN_MAX);
        h.down_until = logbook::now() + wait;
    }

    /// Serveurs disponibles dans l'ordre ; s'ils sont tous de côté, tous dans l'ordre.
    fn order(&self) -> Vec<usize> {
        let now = logbook::now();
        let health = lock(&self.health);
        let up: Vec<usize> = (0..self.upstreams.len()).filter(|&i| health.get(i).is_none_or(|h| h.down_until <= now)).collect();
        if up.is_empty() { (0..self.upstreams.len()).collect() } else { up }
    }

    fn cached(&self, key: &[u8], stale: bool) -> Option<Vec<u8>> {
        let cache = lock(&self.cache);
        let entry = cache.get(key)?;
        let age = logbook::now().saturating_sub(entry.stored);
        let mut reply = entry.reply.clone();
        if age < u64::from(entry.ttl) {
            let age = age as u32;
            dns::map_ttls(&mut reply, |t| t.saturating_sub(age));
        } else if stale && age < u64::from(entry.ttl) + STALE_FOR {
            dns::map_ttls(&mut reply, |_| STALE_TTL);
        } else {
            return None;
        }
        Some(reply)
    }

    fn store(&self, key: Vec<u8>, reply: &[u8]) {
        let Some(ttl) = dns::cache_ttl(reply).filter(|&t| t > 0) else { return };
        let mut cache = lock(&self.cache);
        if cache.len() >= CACHE_MAX {
            let now = logbook::now();
            cache.retain(|_, e| now.saturating_sub(e.stored) < u64::from(e.ttl) + STALE_FOR);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(key, Entry { reply: reply.to_vec(), stored: logbook::now(), ttl });
    }

    /// Réponse encore valide du cache, avec l'identifiant de la requête.
    pub fn from_cache(&self, query: &[u8]) -> Option<Vec<u8>> {
        dns::cache_key(query).and_then(|key| self.cached(&key, false)).map(|reply| with_id(reply, query))
    }

    /// Réponse chiffrée à une requête, ou `None` si aucun serveur ne répond et que le cache
    /// n'a rien, même périmé.
    pub async fn resolve(&self, query: &[u8]) -> Option<Vec<u8>> {
        let key = dns::cache_key(query);
        if let Some(reply) = key.as_deref().and_then(|k| self.cached(k, false)) {
            return Some(with_id(reply, query));
        }
        for index in self.order() {
            match self.exchange(index, query).await {
                Ok(reply) => {
                    self.succeeded(index);
                    if let Some(key) = key {
                        self.store(key, &reply);
                    }
                    return Some(with_id(reply, query));
                }
                Err(error) => self.failed(index, &error),
            }
        }
        key.as_deref().and_then(|k| self.cached(k, true)).map(|r| with_id(r, query))
    }

    /// Un nom jamais demandé, posé à tous les serveurs en même temps (même mis de côté) : un
    /// résultat par serveur, dans l'ordre, avec le temps de réponse. Depuis un fil hors du
    /// runtime (garde, canal de commande).
    fn ask_all(self: &Arc<Self>, name: &str) -> Vec<Result<Duration>> {
        let query = dns::query(0, name, 1);
        rt::block_on(async {
            let runs: Vec<_> = (0..self.upstreams.len())
                .map(|index| {
                    let (resolver, query) = (self.clone(), query.clone());
                    rt::handle().spawn(async move {
                        let started = Instant::now();
                        let reply = resolver.exchange(index, &query).await?;
                        match dns::rcode(&reply) {
                            Some(0 | dns::NXDOMAIN) => Ok(started.elapsed()),
                            rcode => Err(anyhow!("rcode {rcode:?}")),
                        }
                    })
                })
                .collect();
            let mut results = Vec::with_capacity(runs.len());
            for run in runs {
                results.push(run.await.unwrap_or_else(|_| Err(anyhow!("probe panicked"))));
            }
            results
        })
    }

    /// Contrôle de la garde : chaque serveur, même mis de côté (c'est ainsi qu'il revient).
    /// Rend le premier dans l'ordre qui répond (NOERROR ou NXDOMAIN).
    pub fn probe(self: &Arc<Self>, name: &str) -> Option<usize> {
        let mut first = None;
        for (index, result) in self.ask_all(name).iter().enumerate() {
            match result {
                Ok(_) if first.is_none() => first = Some(index),
                Ok(_) => lock(&self.health)[index] = Health::default(),
                Err(error) => self.failed(index, error),
            }
        }
        // Celui qu'on préfère parmi ceux qui répondent.
        if let Some(index) = first {
            self.succeeded(index);
        }
        first
    }

    /// `abdns test` : chaque serveur avec son temps de réponse ; l'état des serveurs n'est pas
    /// modifié.
    pub fn test(self: &Arc<Self>, name: &str) -> Vec<(String, std::result::Result<Duration, String>)> {
        self.ask_all(name)
            .into_iter()
            .enumerate()
            .map(|(index, result)| (self.name(index).to_owned(), result.map_err(|e| format!("{e:#}"))))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> Resolver {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        Resolver::new(&config).unwrap()
    }

    fn answer(id: u16, ttl: u32) -> Vec<u8> {
        let mut r = dns::query(id, "example.com", 1);
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 1;
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
        r.extend_from_slice(&ttl.to_be_bytes());
        r.extend_from_slice(&[0, 4, 93, 184, 216, 34]);
        r
    }

    #[test]
    fn cache_survives_a_panic_while_locked() {
        let r = resolver();
        let key = dns::cache_key(&dns::query(1, "example.com", 1)).unwrap();
        r.store(key.clone(), &answer(0, 300));
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = lock(&r.cache);
            panic!("request thread panics");
        }));
        assert!(poisoned.is_err() && r.cache.is_poisoned());
        assert!(r.cached(&key, false).is_some(), "the cache still answers");
    }

    #[test]
    fn cache_and_ids() {
        let r = resolver();
        let key = dns::cache_key(&dns::query(1, "example.com", 1)).unwrap();
        r.store(key.clone(), &answer(0, 300));
        let hit = with_id(r.cached(&key, false).unwrap(), &dns::query(0xabcd, "example.com", 1));
        assert_eq!(dns::id(&hit), Some(0xabcd));
        assert_eq!(dns::cache_ttl(&hit), Some(300));
        // Expirée : plus servie normalement, encore servie en dernier recours.
        r.cache.lock().unwrap().get_mut(&key).unwrap().stored -= 400;
        assert!(r.cached(&key, false).is_none());
        assert_eq!(dns::cache_ttl(&r.cached(&key, true).unwrap()), Some(STALE_TTL));
    }

    #[test]
    fn failing_servers_are_skipped() {
        let r = resolver();
        assert_eq!(r.order(), vec![0, 1]);
        r.failed(0, &anyhow::anyhow!("test"));
        assert_eq!(r.order(), vec![1]);
        r.failed(1, &anyhow::anyhow!("test"));
        assert_eq!(r.order(), vec![0, 1]);
        r.succeeded(0);
        assert_eq!(r.order(), vec![0]);
        assert_eq!(r.active(), Some(0));
        assert_eq!(r.snapshot(), vec![("Google".to_owned(), true), ("Cloudflare".to_owned(), false)]);
    }

    #[test]
    fn backoff_grows() {
        let r = resolver();
        for _ in 0..10 {
            r.failed(0, &anyhow::anyhow!("test"));
        }
        let down = r.health.lock().unwrap()[0].down_until - logbook::now();
        assert!((DOWN_MAX - 2..=DOWN_MAX).contains(&down));
    }
}
