//! Client DNS-over-HTTPS (RFC 8484) : serveurs essayés dans l'ordre de la configuration, cache
//! qui respecte les durées de vie, réponse périmée en dernier recours (RFC 8767).
//!
//! Un serveur qui échoue est mis de côté (30 s, puis jusqu'à 5 min) : les requêtes suivantes
//! passent directement au suivant, et la garde le réessaie à chaque contrôle.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE};

use crate::config::{self, Config};
use crate::{dns, logbook};

const MEDIA_TYPE: &str = "application/dns-message";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const DOWN_FIRST: u64 = 30;
const DOWN_MAX: u64 = 300;
const CACHE_MAX: usize = 20_000;
/// Une réponse périmée sert encore jusqu'à un jour si aucun serveur ne répond.
const STALE_FOR: u64 = 86_400;
const STALE_TTL: u32 = 30;
const NONE: usize = usize::MAX;

struct Upstream {
    name: String,
    url: String,
    client: Client,
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

fn client(server: &config::Server) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(Duration::from_secs(90))
        .tcp_keepalive(Duration::from_secs(30))
        .https_only(true)
        .no_proxy();
    if let (Some(ip), false) = (server.ip, server.host_is_ip()) {
        // Nom du certificat gardé, adresse imposée : aucune résolution en clair.
        let url = reqwest::Url::parse(&server.url).with_context(|| format!("{}: invalid url", server.name))?;
        let host = url.host_str().context("url without host")?.to_owned();
        builder = builder.resolve(&host, SocketAddr::new(ip, url.port_or_known_default().unwrap_or(443)));
    }
    builder.build().with_context(|| format!("{}: cannot build the HTTPS client", server.name))
}

/// Même réponse, identifiant de la requête du client.
fn with_id(mut reply: Vec<u8>, query: &[u8]) -> Vec<u8> {
    reply[..2].copy_from_slice(&query[..2]);
    reply
}

impl Resolver {
    pub fn new(config: &Config) -> Result<Self> {
        // ring : se compile partout (Windows et musl compris) sans outil externe. Déjà installé
        // lors d'une relecture de la configuration : rien à faire.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let upstreams = config
            .server
            .iter()
            .map(|s| Ok(Upstream { name: s.name.clone(), url: s.full_url()?, client: client(s)? }))
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
        let health = self.health.lock().map(|h| h.clone()).unwrap_or_default();
        self.upstreams.iter().zip(health).map(|(u, h)| (u.name.clone(), h.down_until <= now)).collect()
    }

    fn exchange(&self, index: usize, query: &[u8]) -> Result<Vec<u8>> {
        let upstream = &self.upstreams[index];
        // Identifiant 0 (RFC 8484, §4.1) : mieux mis en cache par les serveurs.
        let mut body = query.to_vec();
        body[..2].fill(0);
        let response = upstream
            .client
            .post(&upstream.url)
            .header(CONTENT_TYPE, MEDIA_TYPE)
            .header(ACCEPT, MEDIA_TYPE)
            .body(body)
            .send()
            .map_err(|e| anyhow::anyhow!("{}", e.without_url()))?;
        ensure!(response.status().is_success(), "HTTP {}", response.status().as_u16());
        let reply = response.bytes().map_err(|e| anyhow::anyhow!("{}", e.without_url()))?.to_vec();
        ensure!(reply.len() >= 12, "short answer");
        Ok(reply)
    }

    fn succeeded(&self, index: usize) {
        if let Ok(mut health) = self.health.lock() {
            if health[index].failures > 0 {
                logbook::log(&format!("{} answers again", self.name(index)));
            }
            health[index] = Health::default();
        }
        self.last.store(index, Ordering::Relaxed);
    }

    fn failed(&self, index: usize, error: &anyhow::Error) {
        if let Ok(mut health) = self.health.lock() {
            let h = &mut health[index];
            if h.failures == 0 {
                logbook::log(&format!("{} unreachable: {error:#}", self.name(index)));
            }
            h.failures += 1;
            let wait = DOWN_FIRST.saturating_mul(1 << (h.failures - 1).min(4)).min(DOWN_MAX);
            h.down_until = logbook::now() + wait;
        }
    }

    /// Serveurs disponibles dans l'ordre ; s'ils sont tous de côté, tous dans l'ordre.
    fn order(&self) -> Vec<usize> {
        let now = logbook::now();
        let health = self.health.lock().map(|h| h.clone()).unwrap_or_default();
        let up: Vec<usize> = (0..self.upstreams.len()).filter(|&i| health.get(i).is_none_or(|h| h.down_until <= now)).collect();
        if up.is_empty() { (0..self.upstreams.len()).collect() } else { up }
    }

    fn cached(&self, key: &[u8], stale: bool) -> Option<Vec<u8>> {
        let cache = self.cache.lock().ok()?;
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
        let Ok(mut cache) = self.cache.lock() else { return };
        if cache.len() >= CACHE_MAX {
            let now = logbook::now();
            cache.retain(|_, e| now.saturating_sub(e.stored) < u64::from(e.ttl) + STALE_FOR);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(key, Entry { reply: reply.to_vec(), stored: logbook::now(), ttl });
    }

    /// Réponse chiffrée à une requête, ou `None` si aucun serveur ne répond et que le cache
    /// n'a rien, même périmé.
    pub fn resolve(&self, query: &[u8]) -> Option<Vec<u8>> {
        let key = dns::cache_key(query);
        if let Some(reply) = key.as_deref().and_then(|k| self.cached(k, false)) {
            return Some(with_id(reply, query));
        }
        for index in self.order() {
            match self.exchange(index, query) {
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

    /// Contrôle de la garde : un nom jamais demandé, chaque serveur dans l'ordre, même mis de
    /// côté (c'est ainsi qu'il revient). Rend le premier qui répond (NOERROR ou NXDOMAIN).
    pub fn probe(&self, name: &str) -> Option<usize> {
        let query = dns::query(0, name, 1);
        // Tous en parallèle : quelques secondes au plus, même quand aucun ne répond.
        let results: Vec<Result<()>> = std::thread::scope(|scope| {
            let probes: Vec<_> = (0..self.upstreams.len())
                .map(|index| {
                    let query = &query;
                    scope.spawn(move || {
                        let reply = self.exchange(index, query)?;
                        ensure!(matches!(dns::rcode(&reply), Some(0 | dns::NXDOMAIN)), "rcode {:?}", dns::rcode(&reply));
                        Ok(())
                    })
                })
                .collect();
            probes.into_iter().map(|p| p.join().unwrap_or_else(|_| Err(anyhow::anyhow!("probe panicked")))).collect()
        });
        let mut first = None;
        for (index, result) in results.iter().enumerate() {
            match result {
                Ok(()) => {
                    if first.is_none() {
                        first = Some(index);
                    } else if let Ok(mut health) = self.health.lock() {
                        health[index] = Health::default();
                    }
                }
                Err(error) => self.failed(index, error),
            }
        }
        // Celui qu'on préfère parmi ceux qui répondent.
        if let Some(index) = first {
            self.succeeded(index);
        }
        first
    }

    /// `abdns test` : chaque serveur interrogé en parallèle, avec son temps de réponse ;
    /// l'état des serveurs n'est pas modifié.
    pub fn test(&self, name: &str) -> Vec<(String, std::result::Result<Duration, String>)> {
        let query = dns::query(0, name, 1);
        std::thread::scope(|scope| {
            let runs: Vec<_> = (0..self.upstreams.len())
                .map(|index| {
                    let query = &query;
                    scope.spawn(move || {
                        let started = std::time::Instant::now();
                        let reply = self.exchange(index, query).map_err(|e| format!("{e:#}"))?;
                        match dns::rcode(&reply) {
                            Some(0 | dns::NXDOMAIN) => Ok(started.elapsed()),
                            rcode => Err(format!("rcode {rcode:?}")),
                        }
                    })
                })
                .collect();
            runs.into_iter()
                .enumerate()
                .map(|(index, run)| (self.name(index).to_owned(), run.join().unwrap_or_else(|_| Err("panicked".to_owned()))))
                .collect()
        })
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
