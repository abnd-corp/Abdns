//! Linux : le noyau pour le réseau et le DNS du poste (netlink, `/proc`, `/sys`, pare-feu
//! nf_tables) et `resolv.conf`, présents partout, sans outil ; systemd (`systemctl`,
//! `loginctl`) pour le service et la session des notifications.

mod dns;
mod netfilter;
mod network;
mod service;
mod session;

use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};

pub use dns::{exempt, keep_system_dns, set_system_dns};
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

/// Interface physique (carte réseau, Wi-Fi), et non un VPN, un pont ou un conteneur.
fn physical(interface: &str) -> bool {
    Path::new("/sys/class/net").join(interface).join("device").exists()
}
