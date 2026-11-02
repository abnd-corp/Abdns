//! DNS du poste par systemd-resolved, sur les interfaces physiques ; à l'arrêt, chaque interface retrouve le DNS que NetworkManager connaît (ADR 0004).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Mutex;

use anyhow::{Result, bail, ensure};

use super::network::default_routes;
use super::{output, parse_link_dns, physical, succeeds};
use crate::config::Config;
use crate::logbook;

/// Réglage d'une interface avant abdns : DNS et domaines de routage de resolved.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Saved {
    dns: Vec<String>,
    domains: Vec<String>,
}

/// Interfaces réglées sur le relais par ce processus, avec leur réglage d'avant (s'il était connu).
pub(super) static TOUCHED: Mutex<BTreeMap<String, Option<Saved>>> = Mutex::new(BTreeMap::new());

pub(super) fn listen_ips(config: &Config) -> Vec<String> {
    let mut ips: Vec<String> = config.listen.iter().map(|a| a.ip().to_string()).collect();
    ips.dedup();
    ips
}

/// Interfaces par lesquelles sort le poste, sans doublon.
pub(super) fn uplinks() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for route in default_routes().into_iter().filter(|r| physical(&r.interface)) {
        if !names.contains(&route.interface) {
            names.push(route.interface);
        }
    }
    names
}

/// Mots de « Link 3 (wlo1): 192.168.1.1 fe80::1%3 » après les deux-points, tels que resolved les écrit.
pub(super) fn link_words(text: &str) -> Vec<String> {
    text.split_once("): ").map_or("", |(_, list)| list).split_whitespace().map(str::to_owned).collect()
}

/// Valeurs d'une clé de `nmcli -t` (« IP4.DNS[1]:192.168.1.1 ») ; `:` et `\` y sont protégés par `\`.
pub(super) fn nm_values(text: &str, key: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.split('[').next() == Some(key)).then(|| {
                let mut plain = String::new();
                let mut chars = value.chars();
                while let Some(c) = chars.next() {
                    plain.push(if c == '\\' { chars.next().unwrap_or(c) } else { c });
                }
                plain
            })
        })
        .collect()
}

/// Une adresse de lien local n'a de sens qu'avec son interface.
pub(super) fn scoped(address: &str, interface: &str) -> String {
    match address.parse::<std::net::Ipv6Addr>() {
        Ok(ip) if ip.segments()[0] & 0xffc0 == 0xfe80 => format!("{address}%{interface}"),
        _ => address.to_owned(),
    }
}

pub(super) fn is_loopback(word: &str) -> bool {
    word.split(['%', '#']).next().and_then(|a| a.parse::<IpAddr>().ok()).is_some_and(|ip| ip.is_loopback())
}

/// Ce que NetworkManager sait du réseau de l'interface : à jour, et inchangé par le réglage d'abdns.
pub(super) fn nm_view(interface: &str) -> Saved {
    let text = output("nmcli", &["-t", "-f", "IP4.DNS,IP6.DNS,IP4.DOMAIN,IP6.DOMAIN", "device", "show", interface]).unwrap_or_default();
    let dns = ["IP4.DNS", "IP6.DNS"]
        .iter()
        .flat_map(|key| nm_values(&text, key))
        .filter(|a| !is_loopback(a))
        .map(|a| scoped(&a, interface))
        .collect();
    let domains = ["IP4.DOMAIN", "IP6.DOMAIN"].iter().flat_map(|key| nm_values(&text, key)).collect();
    Saved { dns, domains }
}

/// Le réglage actuel de resolved pour l'interface.
pub(super) fn link_state(interface: &str) -> Saved {
    let words = |what: &str| link_words(&output("resolvectl", &[what, interface]).unwrap_or_default());
    Saved { dns: words("dns"), domains: words("domain") }
}

/// Ce que l'interface doit retrouver : ce que NetworkManager sait, sinon le réglage d'avant abdns
/// (sans adresse de boucle locale ni `~.` : ce serait le relais).
pub(super) fn restore_target(nm: Saved, saved: Option<&Saved>) -> Saved {
    if !nm.dns.is_empty() {
        return nm;
    }
    match saved {
        Some(saved) => Saved {
            dns: saved.dns.iter().filter(|w| !is_loopback(w)).cloned().collect(),
            domains: saved.domains.iter().filter(|d| *d != "~.").cloned().collect(),
        },
        None => nm,
    }
}

/// L'interface envoie son DNS au relais (par ce processus ou par un précédent qui s'est arrêté net).
pub(super) fn points_to_relay(interface: &str, relay: &[String]) -> bool {
    let current = parse_link_dns(&output("resolvectl", &["dns", interface]).unwrap_or_default());
    current.iter().any(|ip| relay.contains(&ip.to_string()))
}

/// Le réglage d'abdns est en place : DNS sur le relais et toutes les requêtes passant par lui (`~.`).
pub(super) fn relay_applied(interface: &str, relay: &[String]) -> bool {
    points_to_relay(interface, relay)
        && link_words(&output("resolvectl", &["domain", interface]).unwrap_or_default()).iter().any(|d| d == "~.")
}

pub(super) fn apply(interface: &str, relay: &[String]) -> bool {
    let mut dns = vec!["dns", interface];
    dns.extend(relay.iter().map(String::as_str));
    succeeds("resolvectl", &dns) && succeeds("resolvectl", &["domain", interface, "~."])
}

pub(super) fn restore(interface: &str, saved: Option<&Saved>) -> bool {
    let target = restore_target(nm_view(interface), saved);
    if target.dns.is_empty() {
        return succeeds("resolvectl", &["revert", interface]);
    }
    let mut dns = vec!["dns", interface];
    dns.extend(target.dns.iter().map(String::as_str));
    // Une chaîne vide vide la liste des domaines.
    let mut domain = vec!["domain", interface];
    domain.extend(if target.domains.is_empty() { vec![""] } else { target.domains.iter().map(String::as_str).collect() });
    succeeds("resolvectl", &dns) && succeeds("resolvectl", &domain)
}

/// `relay` : DNS des interfaces sur le relais, toutes les requêtes passant par lui (`~.`) ;
/// sinon, chaque interface retrouve le DNS que NetworkManager connaît pour son réseau (ou, sans
/// lui, son réglage d'avant). `network_services` (macOS) est sans objet ici.
pub fn set_system_dns(config: &Config, relay: bool) -> Result<()> {
    if output("resolvectl", &["--version"]).is_none() {
        bail!("systemd-resolved (resolvectl) is required to set the system DNS");
    }
    let ips = listen_ips(config);
    let mut touched = TOUCHED.lock().map_err(|_| anyhow::anyhow!("interface list lock"))?;
    let mut failures = Vec::new();
    if relay {
        for interface in uplinks() {
            let saved = (!points_to_relay(&interface, &ips)).then(|| link_state(&interface));
            if apply(&interface, &ips) {
                touched.entry(interface).or_insert(saved);
            } else {
                failures.push(interface);
            }
        }
    } else {
        let mut interfaces = std::mem::take(&mut *touched);
        for interface in uplinks().into_iter().filter(|i| points_to_relay(i, &ips)) {
            interfaces.entry(interface).or_insert(None);
        }
        for (interface, saved) in interfaces {
            if !restore(&interface, saved.as_ref()) {
                failures.push(interface);
            }
        }
    }
    succeeds("resolvectl", &["flush-caches"]);
    ensure!(failures.is_empty(), "DNS not set for: {}", failures.join(", "));
    Ok(())
}

/// Appelé par la garde : NetworkManager, un nouveau réseau ou un administrateur ont pu changer le
/// réglage d'une interface ; abdns le refait.
pub fn keep_system_dns(config: &Config) -> Result<()> {
    if output("resolvectl", &["--version"]).is_none() {
        return Ok(());
    }
    let ips = listen_ips(config);
    let lost: Vec<String> = uplinks().into_iter().filter(|i| !relay_applied(i, &ips)).collect();
    if lost.is_empty() {
        return Ok(());
    }
    logbook::log(&format!("system DNS changed on {}: setting it again", lost.join(", ")));
    set_system_dns(config, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_manager_values_are_unescaped() {
        let text = "IP4.DNS[1]:192.168.1.1\nIP6.DNS[1]:fd00\\:\\:1\nIP4.DOMAIN[1]:lan\nIP4.DNS[2]:10.0.0.53\n";
        assert_eq!(nm_values(text, "IP4.DNS"), ["192.168.1.1", "10.0.0.53"]);
        assert_eq!(nm_values(text, "IP6.DNS"), ["fd00::1"]);
        assert_eq!(nm_values(text, "IP4.DOMAIN"), ["lan"]);
        assert!(nm_values(text, "IP6.DOMAIN").is_empty());
    }

    #[test]
    fn link_words_and_scopes() {
        assert_eq!(link_words("Link 3 (wlo1): ~. lan\n"), ["~.", "lan"]);
        assert!(link_words("Link 3 (wlo1): \n").is_empty());
        assert_eq!(scoped("fe80::1", "wlo1"), "fe80::1%wlo1");
        assert_eq!(scoped("2001:db8::1", "wlo1"), "2001:db8::1");
        assert_eq!(scoped("192.168.1.1", "wlo1"), "192.168.1.1");
        assert!(is_loopback("127.0.0.1") && is_loopback("::1") && !is_loopback("192.168.1.1"));
    }

    #[test]
    fn restore_prefers_what_network_manager_knows() {
        let nm = Saved { dns: vec!["192.168.1.1".into()], domains: vec![] };
        let saved = Saved { dns: vec!["10.9.9.9".into()], domains: vec!["old".into()] };
        // NetworkManager est à jour (le réseau a pu changer depuis le réglage) : il l'emporte.
        assert_eq!(restore_target(nm.clone(), Some(&saved)), nm);
        // Sans NetworkManager : le réglage d'avant, sans le relais ni `~.`.
        let empty = Saved { dns: vec![], domains: vec![] };
        let before = Saved { dns: vec!["10.9.9.9".into(), "127.0.0.1".into(), "::1".into()], domains: vec!["~.".into(), "old".into()] };
        assert_eq!(restore_target(empty.clone(), Some(&before)), Saved { dns: vec!["10.9.9.9".into()], domains: vec!["old".into()] });
        // Rien du tout : vide, resolved remet ses valeurs par défaut.
        assert_eq!(restore_target(empty.clone(), None), empty);
    }
}
