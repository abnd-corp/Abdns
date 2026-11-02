//! Linux : systemd (`systemctl`, `loginctl`), systemd-resolved (`resolvectl`) pour régler le DNS,
//! NetworkManager (`nmcli`) pour lire le bail DHCP quand il est là, `ip` pour la route, et une
//! socket netlink (noyau) pour être prévenu des changements de réseau.

mod dns;
mod network;
mod service;
mod session;

use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};

pub use dns::{keep_system_dns, set_system_dns};
pub use network::{Watch, network};
pub use service::{install, logs, uninstall};
pub use session::{notify, open};

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Retire les adresses de boucle locale : le DNS du réseau ne peut pas être le relais.
fn external(dns: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    dns.into_iter().filter(|ip| !ip.is_loopback() && !ip.is_unspecified()).collect()
}

/// Adresses de « Link 3 (wlo1): 192.168.1.1 fe80::1%3#nom », sans zone ni nom de serveur.
fn parse_link_dns(text: &str) -> Vec<IpAddr> {
    let list = text.split_once("): ").map_or("", |(_, list)| list);
    list.split_whitespace().filter_map(|a| a.split(['%', '#']).next()?.parse().ok()).collect()
}

/// Groupe des administrateurs (`sudo`, `wheel` ou `admin` selon la distribution).
pub fn admin_gid() -> Option<u32> {
    let groups = std::fs::read_to_string("/etc/group").ok()?;
    ["sudo", "wheel", "admin"].iter().find_map(|name| {
        groups.lines().find_map(|line| {
            let mut fields = line.split(':');
            (fields.next() == Some(name)).then(|| fields.nth(1)?.parse().ok())?
        })
    })
}

/// Interface physique (carte réseau, Wi-Fi) : un VPN, un pont ou un conteneur garde son propre DNS.
fn physical(interface: &str) -> bool {
    Path::new("/sys/class/net").join(interface).join("device").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_dns() {
        let dns = |s: &str| parse_link_dns(s).iter().map(IpAddr::to_string).collect::<Vec<_>>();
        assert_eq!(dns("Link 3 (wlo1): 192.168.1.1 fe80::1%3#box.lan\n"), ["192.168.1.1", "fe80::1"]);
        assert!(dns("Link 2 (enp2s0): \n").is_empty());
        assert!(dns("").is_empty());
    }
}
