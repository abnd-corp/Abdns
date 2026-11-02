//! Cache des réponses chiffrées : clé par question, durées de vie décomptées à la lecture,
//! réponse périmée gardée un jour pour le dernier recours (RFC 8767).

use std::collections::HashMap;
use std::sync::Mutex;

use crate::{dns, lock, logbook};

const CACHE_MAX: usize = 20_000;
/// Une réponse périmée sert encore jusqu'à un jour si aucun serveur ne répond.
const STALE_FOR: u64 = 86_400;
const STALE_TTL: u32 = 30;

struct Entry {
    reply: Vec<u8>,
    stored: u64,
    ttl: u32,
}

#[derive(Default)]
pub(super) struct Cache(Mutex<HashMap<Vec<u8>, Entry>>);

impl Cache {
    /// Réponse encore valide ; avec `stale`, aussi une réponse périmée depuis moins d'un jour.
    pub(super) fn get(&self, key: &[u8], stale: bool) -> Option<Vec<u8>> {
        let cache = lock(&self.0);
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

    pub(super) fn store(&self, key: Vec<u8>, reply: &[u8]) {
        let Some(ttl) = dns::cache_ttl(reply).filter(|&t| t > 0) else { return };
        let mut cache = lock(&self.0);
        if cache.len() >= CACHE_MAX {
            let now = logbook::now();
            cache.retain(|_, e| now.saturating_sub(e.stored) < u64::from(e.ttl) + STALE_FOR);
            if cache.len() >= CACHE_MAX {
                cache.clear();
            }
        }
        cache.insert(key, Entry { reply: reply.to_vec(), stored: logbook::now(), ttl });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(ttl: u32) -> Vec<u8> {
        let mut r = dns::query(0, "example.com", 1);
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 1;
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
        r.extend_from_slice(&ttl.to_be_bytes());
        r.extend_from_slice(&[0, 4, 93, 184, 216, 34]);
        r
    }

    #[test]
    fn survives_a_panic_while_locked() {
        let cache = Cache::default();
        let key = dns::cache_key(&dns::query(1, "example.com", 1)).unwrap();
        cache.store(key.clone(), &answer(300));
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = lock(&cache.0);
            panic!("request thread panics");
        }));
        assert!(poisoned.is_err() && cache.0.is_poisoned());
        assert!(cache.get(&key, false).is_some(), "the cache still answers");
    }

    #[test]
    fn lifetimes_and_stale_answers() {
        let cache = Cache::default();
        let key = dns::cache_key(&dns::query(1, "example.com", 1)).unwrap();
        cache.store(key.clone(), &answer(300));
        assert_eq!(dns::cache_ttl(&cache.get(&key, false).unwrap()), Some(300));
        // Expirée : plus servie normalement, encore servie en dernier recours.
        cache.0.lock().unwrap().get_mut(&key).unwrap().stored -= 400;
        assert!(cache.get(&key, false).is_none());
        assert_eq!(dns::cache_ttl(&cache.get(&key, true).unwrap()), Some(STALE_TTL));
    }
}
