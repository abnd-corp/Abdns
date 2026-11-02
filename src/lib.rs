//! abdns : DNS chiffré du poste, avec son propre serveur DoH d'abord, des secours chiffrés dans
//! l'ordre, et le DNS du réseau seulement en dernier recours (portails captifs).
//!
//! Deux programmes, comme `sshd` et `ssh` : `abdnsd`, le service (administrateur), et `abdns`,
//! l'outil de l'utilisateur, qui le pilote par le canal de `control`.

pub mod config;
pub mod control;
pub mod dns;
pub mod doh;
pub mod guard;
pub mod i18n;
pub mod logbook;
pub mod platform;
pub mod relay;
pub mod rt;

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

// Verrous repris même si un fil a paniqué en les tenant : une requête qui plante ne doit pas
// laisser le cache ou le résolveur inutilisables jusqu'au redémarrage.

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}
