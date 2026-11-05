//! Ce qui dépend du système : réseau actif et son DNS, réglage du DNS du poste,
//! notifications, ouverture d'une page, installation du service. macOS et Linux ; Windows
//! suivra derrière les mêmes fonctions.

use std::net::IpAddr;

use crate::config::Config;
use crate::logbook;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod other;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub use other::*;

/// Commun aux systèmes POSIX : macOS, Linux, et les BSD plus tard.
#[cfg(unix)]
mod tty;
#[cfg(unix)]
pub use tty::read_secret_line;

/// Vrai là où le DNS du poste ne se défait pas seul si abdns est tué net : le garde-fou le rend
/// au réseau (macOS). Sous Linux, le noyau retire la table du pare-feu avec le processus.
pub const RESTORE_AFTER_EXIT: bool = cfg!(target_os = "macos");

/// Faux là où le réseau n'est pas encore lu (`network()` rend toujours `None`).
pub const SEES_NETWORK: bool = cfg!(any(target_os = "macos", target_os = "linux"));

/// Notification, sauf si `[notify] enabled = false` : le journal la garde dans tous les cas.
pub fn notice(config: &Config, title: &str, body: &str) {
    if config.notify.enabled {
        notify(title, body);
    } else {
        logbook::log(&format!("notification (off): {title} — {body}"));
    }
}

/// Réseau par lequel sort le poste.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Network {
    pub interface: String,
    pub gateway: String,
    /// DNS annoncés par le réseau (DHCP), jamais le relais lui-même.
    pub dns: Vec<IpAddr>,
    /// Domaine local annoncé par le réseau (DHCP), s'il y en a un.
    pub domain: Option<String>,
    /// Nom montré à l'utilisateur (« Wi-Fi »), sans adresse.
    pub label: String,
}

impl Network {
    /// Identité du réseau pour détecter un changement (le DNS annoncé peut arriver après).
    pub fn key(&self) -> String {
        format!("{} via {}", self.interface, self.gateway)
    }
}
