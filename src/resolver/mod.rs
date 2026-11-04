//! Client DNS-over-HTTPS (RFC 8484) : serveurs essayés dans l'ordre de la configuration, cache
//! qui respecte les durées de vie, réponse périmée en dernier recours (RFC 8767).
//!
//! Un serveur qui échoue est mis de côté (30 s, puis jusqu'à 5 min) : les requêtes suivantes
//! passent directement au suivant, et la garde le réessaie à chaque contrôle.

mod cache;
mod https;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, ensure};

use crate::config::Config;
use crate::{dns, lock, logbook, rt};
use cache::Cache;
use https::Upstream;

const DOWN_FIRST: u64 = 30;
const DOWN_MAX: u64 = 300;
const NONE: usize = usize::MAX;

#[derive(Default, Clone, Copy)]
struct Health {
    failures: u32,
    down_until: u64,
}

pub struct Resolver {
    upstreams: Vec<Upstream>,
    health: Mutex<Vec<Health>>,
    cache: Cache,
    /// Dernier serveur qui a répondu, `NONE` avant la première réponse.
    last: AtomicUsize,
}

/// Même réponse, identifiant de la requête du client.
fn with_id(mut reply: Vec<u8>, query: &[u8]) -> Vec<u8> {
    reply[..2].copy_from_slice(&query[..2]);
    reply
}

impl Resolver {
    pub fn new(config: &Config) -> Result<Self> {
        let upstreams = config.server.iter().map(Upstream::new).collect::<Result<Vec<_>>>()?;
        // Un serveur mal réglé n'arrête pas les autres ; aucun utilisable : le service ne démarre pas.
        let problems: Vec<&str> = upstreams.iter().filter_map(Upstream::problem).collect();
        ensure!(problems.len() < upstreams.len(), "{}", problems.join("; "));
        let health = Mutex::new(vec![Health::default(); upstreams.len()]);
        Ok(Self { upstreams, health, cache: Cache::default(), last: AtomicUsize::new(NONE) })
    }

    pub fn name(&self, index: usize) -> &str {
        self.upstreams.get(index).map_or("?", |u| u.name.as_str())
    }

    /// Nouveau réseau : connexions neuves vers chaque serveur, cache gardé.
    pub fn reconnect(&self) {
        self.upstreams.iter().for_each(Upstream::reconnect);
    }

    /// Serveurs mis de côté parce que mal réglés, avec la raison.
    pub fn problems(&self) -> Vec<String> {
        self.upstreams.iter().filter_map(Upstream::problem).map(str::to_owned).collect()
    }

    /// Serveur qui a répondu en dernier.
    pub fn active(&self) -> Option<usize> {
        Some(self.last.load(Ordering::Relaxed)).filter(|&i| i != NONE)
    }

    /// (nom, disponible) pour chaque serveur, dans l'ordre.
    pub fn snapshot(&self) -> Vec<(String, bool)> {
        let now = logbook::now();
        let health = lock(&self.health).clone();
        self.upstreams.iter().zip(health).map(|(u, h)| (u.name.clone(), u.problem().is_none() && h.down_until <= now)).collect()
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

    /// Serveurs disponibles dans l'ordre ; s'ils sont tous de côté, tous les serveurs bien
    /// réglés dans l'ordre.
    fn order(&self) -> Vec<usize> {
        let now = logbook::now();
        let health = lock(&self.health);
        let usable: Vec<usize> = (0..self.upstreams.len()).filter(|&i| self.upstreams[i].problem().is_none()).collect();
        let up: Vec<usize> = usable.iter().copied().filter(|&i| health.get(i).is_none_or(|h| h.down_until <= now)).collect();
        if up.is_empty() { usable } else { up }
    }

    /// Réponse encore valide du cache, avec l'identifiant de la requête.
    pub fn from_cache(&self, query: &[u8]) -> Option<Vec<u8>> {
        dns::cache_key(query).and_then(|key| self.cache.get(&key, false)).map(|reply| with_id(reply, query))
    }

    /// Réponse chiffrée à une requête, ou `None` si aucun serveur ne répond et que le cache
    /// n'a rien, même périmé.
    pub async fn resolve(&self, query: &[u8]) -> Option<Vec<u8>> {
        let key = dns::cache_key(query);
        if let Some(reply) = key.as_deref().and_then(|k| self.cache.get(k, false)) {
            return Some(with_id(reply, query));
        }
        for index in self.order() {
            match self.upstreams[index].exchange(query).await {
                Ok(reply) => {
                    self.succeeded(index);
                    if let Some(key) = key {
                        self.cache.store(key, &reply);
                    }
                    return Some(with_id(reply, query));
                }
                Err(error) => self.failed(index, &error),
            }
        }
        key.as_deref().and_then(|k| self.cache.get(k, true)).map(|r| with_id(r, query))
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
                        let reply = resolver.upstreams[index].exchange(&query).await?;
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
        let config = Config::parse(include_str!("../../config.example.toml")).unwrap();
        Resolver::new(&config).unwrap()
    }

    #[test]
    fn answers_take_the_query_identifier() {
        let reply = dns::query(0, "example.com", 1);
        assert_eq!(dns::id(&with_id(reply, &dns::query(0xabcd, "example.com", 1))), Some(0xabcd));
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
    fn misconfigured_server_is_set_aside() {
        let text = "[[server]]\nname = \"VPS\"\nurl = \"https://203.0.113.5/q/\"\ntoken_file = \"/nonexistent/token\"\n\n\
[[server]]\nname = \"Other\"\nurl = \"https://203.0.113.6/q\"\n";
        let r = Resolver::new(&Config::parse(text).unwrap()).unwrap();
        assert_eq!(r.problems().len(), 1);
        assert!(r.problems()[0].starts_with("VPS: cannot read token"));
        assert_eq!(r.order(), vec![1]);
        assert_eq!(r.snapshot(), vec![("VPS".to_owned(), false), ("Other".to_owned(), true)]);
        // Aucun serveur utilisable : le service ne démarre pas.
        let alone = "[[server]]\nname = \"VPS\"\nurl = \"https://203.0.113.5/q/\"\ntoken_file = \"/nonexistent/token\"\n";
        assert!(Resolver::new(&Config::parse(alone).unwrap()).is_err());
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
