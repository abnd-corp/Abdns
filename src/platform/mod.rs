//! Ce qui dépend du système : réseau actif et son DNS, réglage du DNS du poste,
//! notifications, ouverture d'une page, installation du service. macOS d'abord ; Linux et
//! Windows suivront derrière les mêmes fonctions.

use std::net::IpAddr;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub use other::*;

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

/// Retire les adresses de boucle locale : le DNS du réseau ne peut pas être le relais.
pub fn external(dns: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    dns.into_iter().filter(|ip| !ip.is_loopback() && !ip.is_unspecified()).collect()
}
