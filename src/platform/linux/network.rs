//! Réseau actif, lu dans les fichiers du noyau (`/proc`, `/sys`) et dans `resolv.conf`, sans
//! outil ; abonnement netlink à ses changements.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

use super::{external, physical};
use crate::platform::Network;

const RTF_UP: u32 = 0x1;
const RTF_REJECT: u32 = 0x200;

/// Fichiers au format resolv.conf, dans l'ordre : celui du système, puis, derrière le stub de
/// systemd-resolved (127.0.0.53), la liste que resolved ou NetworkManager tiennent du réseau.
const RESOLV_FILES: &[&str] = &["/etc/resolv.conf", "/run/systemd/resolve/resolv.conf", "/run/NetworkManager/no-stub-resolv.conf"];

#[derive(Debug, PartialEq)]
pub(super) struct Route {
    pub(super) interface: String,
    gateway: Option<IpAddr>,
    metric: u32,
}

/// Routes par défaut de `/proc/net/route`, adresses en hexadécimal dans l'ordre de la machine :
/// « wlo1 00000000 0101A8C0 0003 0 0 600 00000000 0 0 0 ».
pub(super) fn parse_routes_v4(text: &str) -> Vec<Route> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let [interface, destination, gateway, flags, _, _, metric, mask, ..] = line.split_whitespace().collect::<Vec<_>>()[..] else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            (destination == "00000000" && mask == "00000000" && flags & RTF_UP != 0).then_some(())?;
            let gateway = Ipv4Addr::from(u32::from_str_radix(gateway, 16).ok()?.to_ne_bytes());
            Some(Route {
                interface: interface.to_owned(),
                gateway: (!gateway.is_unspecified()).then_some(IpAddr::V4(gateway)),
                metric: metric.parse().ok()?,
            })
        })
        .collect()
}

/// Routes par défaut de `/proc/net/ipv6_route` : destination, longueur, source, longueur,
/// passerelle, métrique, compteurs, drapeaux et interface, en hexadécimal.
pub(super) fn parse_routes_v6(text: &str) -> Vec<Route> {
    text.lines()
        .filter_map(|line| {
            let [destination, length, _, _, gateway, metric, _, _, flags, interface] = line.split_whitespace().collect::<Vec<_>>()[..]
            else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            let default = destination.bytes().all(|b| b == b'0') && length == "00";
            (default && flags & RTF_UP != 0 && flags & RTF_REJECT == 0 && interface != "lo").then_some(())?;
            let gateway = Ipv6Addr::from(u128::from_str_radix(gateway, 16).ok()?);
            Some(Route {
                interface: interface.to_owned(),
                gateway: (!gateway.is_unspecified()).then_some(IpAddr::V6(gateway)),
                metric: u32::from_str_radix(metric, 16).ok()?,
            })
        })
        .collect()
}

/// IPv4 d'abord ; IPv6 seulement si le poste n'a pas de route IPv4. La plus prioritaire
/// (métrique la plus basse) d'abord.
pub(super) fn default_routes() -> Vec<Route> {
    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
    let mut routes = parse_routes_v4(&read("/proc/net/route"));
    if routes.is_empty() {
        routes = parse_routes_v6(&read("/proc/net/ipv6_route"));
    }
    routes.sort_by_key(|r| r.metric);
    routes
}

/// Domaine en minuscules, s'il est un nom plausible.
fn domain(name: &str) -> Option<String> {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')).then_some(name)
}

/// Serveurs et premier domaine de recherche d'un fichier resolv.conf. Sans la boucle locale (le
/// stub de resolved, le relais) ni les adresses de lien local, inutilisables sans leur interface.
pub(super) fn parse_resolv(text: &str) -> (Vec<IpAddr>, Option<String>) {
    let mut servers = Vec::new();
    let mut search = None;
    for line in text.lines() {
        let mut words = line.split(['#', ';']).next().unwrap_or("").split_whitespace();
        match (words.next(), words.next()) {
            (Some("nameserver"), Some(address)) => servers.extend(address.parse::<IpAddr>().ok()),
            (Some("search" | "domain"), Some(name)) if search.is_none() => search = domain(name),
            _ => {}
        }
    }
    let servers = external(servers).into_iter().filter(|ip| !matches!(ip, IpAddr::V6(v6) if v6.is_unicast_link_local())).collect();
    (servers, search)
}

/// DNS du réseau : abdns ne change pas ces fichiers, ils gardent ce que le réseau a annoncé.
fn network_dns() -> (Vec<IpAddr>, Option<String>) {
    let mut found: (Vec<IpAddr>, Option<String>) = (Vec::new(), None);
    for path in RESOLV_FILES {
        let (servers, search) = parse_resolv(&std::fs::read_to_string(path).unwrap_or_default());
        found.1 = found.1.or(search);
        if !servers.is_empty() {
            found.0 = servers;
            break;
        }
    }
    found
}

/// Nom montré à l'utilisateur : « Wi-Fi », « Ethernet », sinon celui de l'interface (VPN…).
fn label(interface: &str) -> String {
    if Path::new("/sys/class/net").join(interface).join("wireless").exists() {
        "Wi-Fi".to_owned()
    } else if physical(interface) {
        "Ethernet".to_owned()
    } else {
        interface.to_owned()
    }
}

pub fn network() -> Option<Network> {
    let route = default_routes().into_iter().next()?;
    let (dns, domain) = network_dns();
    Some(Network {
        dns,
        domain,
        label: label(&route.interface),
        gateway: route.gateway.map_or_else(|| "-".to_owned(), |g| g.to_string()),
        interface: route.interface,
    })
}

/// Abonnement netlink aux changements de liens, d'adresses et de routes. Présent dans tout
/// noyau Linux, conteneurs compris : aucun paquet à installer. Ne voit pas un changement de DNS
/// seul (`resolv.conf`) : la garde garde un contrôle lent pour ce cas.
pub struct Watch(OwnedFd);

impl Watch {
    pub fn new() -> Option<Self> {
        // SAFETY: appels système sur un descripteur que nous possédons, adresse initialisée.
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
            // SAFETY: tampon valide de la taille donnée, descripteur non bloquant.
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
    fn ipv4_default_routes() {
        let text = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
wlo1\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
wlo1\t0001A8C0\t00000000\t0001\t0\t0\t600\t00FFFFFF\t0\t0\t0\n\
wg0\t00000000\t00000000\t0001\t0\t0\t50\t00000000\t0\t0\t0\n";
        let routes = parse_routes_v4(text);
        assert_eq!(routes.len(), 2);
        let gateway = Ipv4Addr::from(0x0101_A8C0u32.to_ne_bytes());
        assert_eq!(routes[0], Route { interface: "wlo1".into(), gateway: Some(gateway.into()), metric: 600 });
        assert_eq!(routes[1], Route { interface: "wg0".into(), gateway: None, metric: 50 });
        assert!(parse_routes_v4("").is_empty());
    }

    #[test]
    fn ipv6_default_routes() {
        let text = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000001 00000000 00450003 wlo1\n\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n\
20010db8000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 wlo1\n";
        let routes = parse_routes_v6(text);
        assert_eq!(routes, [Route { interface: "wlo1".into(), gateway: Some("fe80::1".parse().unwrap()), metric: 1024 }]);
    }

    #[test]
    fn resolv_files() {
        let stub = "# Généré par resolved\nnameserver 127.0.0.53\noptions edns0 trust-ad\nsearch lan\n";
        assert_eq!(parse_resolv(stub), (vec![], Some("lan".to_owned())));
        let upstream = "nameserver 192.168.1.1 ; box\nnameserver fe80::1%wlo1\nnameserver fe80::1\nnameserver 2001:db8::53\ndomain Home.\n";
        assert_eq!(
            parse_resolv(upstream),
            (vec!["192.168.1.1".parse().unwrap(), "2001:db8::53".parse().unwrap()], Some("home".to_owned()))
        );
        assert_eq!(parse_resolv("search a$b\n").1, None);
        assert_eq!(parse_resolv(""), (vec![], None));
    }
}
