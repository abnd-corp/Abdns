//! Réseau actif (route par défaut, bail DHCP, nom du port) et abonnement à ses changements.

use std::ffi::{CStr, c_char, c_int};
use std::net::IpAddr;

use super::output;
use crate::platform::Network;

/// Valeur d'une ligne « clé: valeur » de `route -n get default`.
pub(super) fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(':').map(str::trim))
}

/// Option d'un bail DHCP affiché par `ipconfig getpacket` : « nom (type): valeur ».
pub(super) fn dhcp_option<'a>(packet: &'a str, name: &str) -> Option<&'a str> {
    packet.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix(name)?.trim_start();
        rest.starts_with('(').then(|| rest.split_once(':').map(|(_, v)| v.trim()))?
    })
}

/// Retire les adresses de boucle locale : le DNS du réseau ne peut pas être le relais.
pub(super) fn external(dns: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    dns.into_iter().filter(|ip| !ip.is_loopback() && !ip.is_unspecified()).collect()
}

/// DNS du bail : « domain_name_server (ip_mult): {a, b} ».
pub(super) fn dhcp_dns(packet: &str) -> Vec<IpAddr> {
    let list = dhcp_option(packet, "domain_name_server").and_then(|v| v.strip_prefix('{')?.strip_suffix('}')).unwrap_or("");
    external(list.split(',').filter_map(|a| a.trim().parse().ok()))
}

/// Domaine du bail, en minuscules, s'il est un nom plausible.
pub(super) fn dhcp_domain(packet: &str) -> Option<String> {
    let domain = dhcp_option(packet, "domain_name")?.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')).then_some(domain)
}

unsafe extern "C" {
    fn notify_register_check(name: *const c_char, token: *mut c_int) -> u32;
    fn notify_check(token: c_int, check: *mut c_int) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

/// `kNotifySCNetworkChange` (notify_keys.h) : publié par configd à chaque changement de la
/// configuration réseau (interface, route, DNS).
pub(super) const NETWORK_CHANGE: &CStr = c"com.apple.system.config.network_change";

/// Abonnement aux changements de réseau. `changed` lit une mémoire partagée : rien n'est lancé.
pub struct Watch(c_int);

impl Watch {
    pub fn new() -> Option<Self> {
        Self::named(NETWORK_CHANGE)
    }

    fn named(name: &CStr) -> Option<Self> {
        let mut token = 0;
        // SAFETY : nom terminé par un zéro, jeton écrit par la fonction.
        (unsafe { notify_register_check(name.as_ptr(), &raw mut token) } == 0).then_some(Self(token))
    }

    /// Vrai si le réseau a changé depuis le dernier appel (toujours vrai au premier).
    pub fn changed(&mut self) -> bool {
        let mut check = 0;
        // SAFETY : jeton rendu par notify_register_check, encore valide.
        unsafe { notify_check(self.0, &raw mut check) == 0 && check != 0 }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY : jeton rendu par notify_register_check, annulé une seule fois.
        unsafe { notify_cancel(self.0) };
    }
}

pub fn network() -> Option<Network> {
    let route = output("/sbin/route", &["-n", "get", "default"])?;
    let interface = field(&route, "interface")?.to_owned();
    let gateway = field(&route, "gateway").unwrap_or("-").to_owned();
    // Un VPN (utun) n'a pas de bail DHCP : pas de DNS du réseau à proposer.
    let packet = output("/usr/sbin/ipconfig", &["getpacket", &interface]).unwrap_or_default();
    let ports = output("/usr/sbin/networksetup", &["-listallhardwareports"]).unwrap_or_default();
    let label = hardware_port(&ports, &interface).unwrap_or(&interface).to_owned();
    Some(Network { dns: dhcp_dns(&packet), domain: dhcp_domain(&packet), label, interface, gateway })
}

/// Nom du port matériel d'une interface : « Hardware Port: Wi-Fi » suivi de « Device: en0 ».
pub(super) fn hardware_port<'a>(ports: &'a str, interface: &str) -> Option<&'a str> {
    let mut port = None;
    for line in ports.lines() {
        if let Some(name) = line.strip_prefix("Hardware Port: ") {
            port = Some(name.trim());
        } else if line.strip_prefix("Device: ").map(str::trim) == Some(interface) {
            return port;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn notify_post(name: *const c_char) -> u32;
    }

    #[test]
    fn watch_sees_a_notification() {
        let name = c"com.abnd.abdns.test";
        let mut watch = Watch::named(name).expect("notify registration");
        watch.changed();
        assert!(!watch.changed(), "nothing posted yet");
        // SAFETY : nom terminé par un zéro ; nom propre à ce test, sans effet sur le système.
        assert_eq!(unsafe { notify_post(name.as_ptr()) }, 0);
        // Remise asynchrone par notifyd : quelques millisecondes.
        let seen = (0..200).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            watch.changed()
        });
        assert!(seen, "the notification arrives");
        assert!(!watch.changed());
        assert!(Watch::new().is_some(), "network change notifications available");
    }

    #[test]
    fn default_route() {
        let route =
            "   route to: default\ndestination: default\n    gateway: 172.20.10.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE>\n";
        assert_eq!(field(route, "interface"), Some("en0"));
        assert_eq!(field(route, "gateway"), Some("172.20.10.1"));
    }

    #[test]
    fn lease() {
        let packet = "op = BOOTREPLY\nrouter (ip_mult): {172.20.10.1}\ndomain_name_server (ip_mult): {172.20.10.1, 127.0.0.1, 10.0.0.53}\ndomain_name (string): Home.\nend (none):\n";
        assert_eq!(dhcp_dns(packet), vec!["172.20.10.1".parse::<IpAddr>().unwrap(), "10.0.0.53".parse().unwrap()]);
        assert_eq!(dhcp_domain(packet).as_deref(), Some("home"));
        assert!(dhcp_dns("op = BOOTREPLY\n").is_empty());
        assert_eq!(dhcp_domain("domain_name (string): a b\n"), None);
    }

    #[test]
    fn hardware_ports() {
        let ports = "\nHardware Port: Ethernet Adapter (en4)\nDevice: en4\nEthernet Address: x\n\nHardware Port: Wi-Fi\nDevice: en0\nEthernet Address: y\n";
        assert_eq!(hardware_port(ports, "en0"), Some("Wi-Fi"));
        assert_eq!(hardware_port(ports, "en4"), Some("Ethernet Adapter (en4)"));
        assert_eq!(hardware_port(ports, "utun3"), None);
    }
}
