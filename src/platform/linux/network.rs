//! Réseau actif (route par défaut, bail DHCP de NetworkManager, DNS de resolved) et abonnement netlink à ses changements.

use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use super::{external, output, parse_link_dns};
use crate::platform::Network;

#[derive(Debug, PartialEq)]
pub(super) struct Route {
    pub(super) interface: String,
    gateway: Option<String>,
    metric: u32,
}

/// Routes par défaut de `ip route show default`, la plus prioritaire (métrique la plus basse) d'abord :
/// « default via 192.168.1.1 dev wlo1 proto dhcp src 192.168.1.95 metric 600 ».
pub(super) fn parse_routes(text: &str) -> Vec<Route> {
    let mut routes: Vec<Route> = text
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let after = |key: &str| words.iter().position(|w| *w == key).and_then(|i| words.get(i + 1)).copied();
            Some(Route {
                interface: after("dev")?.to_owned(),
                gateway: after("via").map(str::to_owned),
                metric: after("metric").and_then(|m| m.parse().ok()).unwrap_or(0),
            })
        })
        .collect();
    routes.sort_by_key(|r| r.metric);
    routes
}

/// IPv4 d'abord ; IPv6 seulement si le poste n'a pas de route IPv4.
pub(super) fn default_routes() -> Vec<Route> {
    let v4 = parse_routes(&output("ip", &["-4", "route", "show", "default"]).unwrap_or_default());
    if v4.is_empty() { parse_routes(&output("ip", &["-6", "route", "show", "default"]).unwrap_or_default()) } else { v4 }
}

/// Valeur d'une option du bail dans `nmcli -t -f DHCP4.OPTION device show` :
/// « DHCP4.OPTION[5]:domain_name_servers = 192.168.1.1 ».
pub(super) fn lease_option<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        let (_, option) = l.split_once(':')?;
        option.strip_prefix(name)?.trim_start().strip_prefix('=').map(str::trim)
    })
}

pub(super) fn lease_dns(lease: &str) -> Vec<IpAddr> {
    external(lease_option(lease, "domain_name_servers").unwrap_or("").split_whitespace().filter_map(|a| a.parse().ok()))
}

/// Domaine du bail, en minuscules, s'il est un nom plausible.
pub(super) fn lease_domain(lease: &str) -> Option<String> {
    let domain = lease_option(lease, "domain_name")?.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')).then_some(domain)
}

/// Le bail de NetworkManager ne change pas quand abdns règle le DNS de resolved : il reste la
/// source du DNS du réseau. Sans NetworkManager, le DNS que resolved a pour l'interface.
pub(super) fn network_dns(interface: &str, lease: &str) -> Vec<IpAddr> {
    let dns = lease_dns(lease);
    if !dns.is_empty() {
        return dns;
    }
    external(parse_link_dns(&output("resolvectl", &["dns", interface]).unwrap_or_default()))
}

pub fn network() -> Option<Network> {
    let route = default_routes().into_iter().next()?;
    let interface = route.interface;
    let lease = output("nmcli", &["-t", "-f", "DHCP4.OPTION", "device", "show", &interface]).unwrap_or_default();
    let label = output("nmcli", &["-g", "GENERAL.CONNECTION", "device", "show", &interface])
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name != "--")
        .unwrap_or_else(|| interface.clone());
    Some(Network {
        dns: network_dns(&interface, &lease),
        domain: lease_domain(&lease),
        label,
        gateway: route.gateway.unwrap_or_else(|| "-".to_owned()),
        interface,
    })
}

/// Abonnement netlink aux changements de liens, d'adresses et de routes. Présent dans tout
/// noyau Linux, conteneurs compris : aucun paquet à installer. Ne voit pas un changement de DNS
/// seul (NetworkManager, resolved) : la garde garde un contrôle lent pour ce cas.
pub struct Watch(OwnedFd);

impl Watch {
    pub fn new() -> Option<Self> {
        // SAFETY : appels système sur un descripteur que nous possédons, adresse initialisée.
        unsafe {
            let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, libc::NETLINK_ROUTE);
            if fd < 0 {
                return None;
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut address: libc::sockaddr_nl = std::mem::zeroed();
            address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            address.nl_groups = (libc::RTMGRP_LINK
                | libc::RTMGRP_IPV4_IFADDR
                | libc::RTMGRP_IPV6_IFADDR
                | libc::RTMGRP_IPV4_ROUTE
                | libc::RTMGRP_IPV6_ROUTE) as u32;
            let size = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            (libc::bind(fd.as_raw_fd(), (&raw const address).cast(), size) == 0).then_some(Self(fd))
        }
    }

    /// Vrai si le noyau a signalé un changement depuis le dernier appel.
    pub fn changed(&mut self) -> bool {
        let mut any = false;
        let mut buf = [0u8; 8192];
        loop {
            // SAFETY : tampon valide de la taille donnée, descripteur non bloquant.
            let n = unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            match n {
                1.. => any = true,
                // Messages perdus (tampon plein) : le réseau a changé quand même.
                _ if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOBUFS) => any = true,
                _ => return any,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netlink_watch_never_blocks() {
        let mut watch = Watch::new().expect("netlink socket");
        let started = std::time::Instant::now();
        watch.changed();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn default_routes_by_metric() {
        let text = "default via 192.168.1.1 dev wlo1 proto dhcp src 192.168.1.95 metric 600\n\
default via 10.0.0.1 dev enp2s0 proto dhcp src 10.0.0.9 metric 100\ndefault dev wg0 scope link\n";
        let routes = parse_routes(text);
        assert_eq!(routes.iter().map(|r| r.interface.as_str()).collect::<Vec<_>>(), ["wg0", "enp2s0", "wlo1"]);
        assert_eq!(routes[0], Route { interface: "wg0".into(), gateway: None, metric: 0 });
        assert_eq!(routes[2].gateway.as_deref(), Some("192.168.1.1"));
        assert!(parse_routes("").is_empty());
    }

    #[test]
    fn lease() {
        let lease = "DHCP4.OPTION[4]:dhcp_server_identifier = 192.168.1.1\nDHCP4.OPTION[5]:domain_name = Home.\n\
DHCP4.OPTION[6]:domain_name_servers = 192.168.1.1 127.0.0.1 10.0.0.53\nDHCP4.OPTION[7]:requested_domain_name = 1\n";
        assert_eq!(lease_dns(lease), vec!["192.168.1.1".parse::<IpAddr>().unwrap(), "10.0.0.53".parse().unwrap()]);
        assert_eq!(lease_domain(lease).as_deref(), Some("home"));
        assert!(lease_dns("").is_empty());
        assert_eq!(lease_domain("DHCP4.OPTION[1]:domain_name = a b\n"), None);
        // « domain_name » ne se confond pas avec « domain_name_servers ».
        assert_eq!(lease_domain("DHCP4.OPTION[6]:domain_name_servers = 192.168.1.1\n"), None);
    }
}
