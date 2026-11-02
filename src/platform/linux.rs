//! Linux : systemd (`systemctl`, `loginctl`), systemd-resolved (`resolvectl`) pour régler le DNS,
//! NetworkManager (`nmcli`) pour lire le bail DHCP quand il est là, `ip` pour la route, et une
//! socket netlink (noyau) pour être prévenu des changements de réseau.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, bail, ensure};

use super::Network;
use crate::config::Config;
use crate::logbook;

const UNIT: &str = "abdnsd.service";
const UNIT_PATH: &str = "/etc/systemd/system/abdnsd.service";
/// Sans fichier de ce nom, GNOME affiche « application inconnue » sur les notifications.
const DESKTOP_PATH: &str = "/usr/share/applications/abdns.desktop";
const DESKTOP_ID: &str = "abdns";

/// Réglage d'une interface avant abdns : DNS et domaines de routage de resolved.
#[derive(Clone, Debug, PartialEq)]
struct Saved {
    dns: Vec<String>,
    domains: Vec<String>,
}

/// Interfaces réglées sur le relais par ce processus, avec leur réglage d'avant (s'il était connu).
static TOUCHED: Mutex<BTreeMap<String, Option<Saved>>> = Mutex::new(BTreeMap::new());

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn succeeds(program: &str, args: &[&str]) -> bool {
    Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

#[derive(Debug, PartialEq)]
struct Route {
    interface: String,
    gateway: Option<String>,
    metric: u32,
}

/// Routes par défaut de `ip route show default`, la plus prioritaire (métrique la plus basse) d'abord :
/// « default via 192.168.1.1 dev wlo1 proto dhcp src 192.168.1.95 metric 600 ».
fn parse_routes(text: &str) -> Vec<Route> {
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
fn default_routes() -> Vec<Route> {
    let v4 = parse_routes(&output("ip", &["-4", "route", "show", "default"]).unwrap_or_default());
    if v4.is_empty() { parse_routes(&output("ip", &["-6", "route", "show", "default"]).unwrap_or_default()) } else { v4 }
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

/// Valeur d'une option du bail dans `nmcli -t -f DHCP4.OPTION device show` :
/// « DHCP4.OPTION[5]:domain_name_servers = 192.168.1.1 ».
fn lease_option<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        let (_, option) = l.split_once(':')?;
        option.strip_prefix(name)?.trim_start().strip_prefix('=').map(str::trim)
    })
}

fn lease_dns(lease: &str) -> Vec<IpAddr> {
    external(lease_option(lease, "domain_name_servers").unwrap_or("").split_whitespace().filter_map(|a| a.parse().ok()))
}

/// Domaine du bail, en minuscules, s'il est un nom plausible.
fn lease_domain(lease: &str) -> Option<String> {
    let domain = lease_option(lease, "domain_name")?.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')).then_some(domain)
}

/// Le bail de NetworkManager ne change pas quand abdns règle le DNS de resolved : il reste la
/// source du DNS du réseau. Sans NetworkManager, le DNS que resolved a pour l'interface.
fn network_dns(interface: &str, lease: &str) -> Vec<IpAddr> {
    let dns = lease_dns(lease);
    if !dns.is_empty() {
        return dns;
    }
    external(parse_link_dns(&output("resolvectl", &["dns", interface]).unwrap_or_default()))
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

/// Interface physique (carte réseau, Wi-Fi) : un VPN, un pont ou un conteneur garde son propre DNS.
fn physical(interface: &str) -> bool {
    Path::new("/sys/class/net").join(interface).join("device").exists()
}

fn listen_ips(config: &Config) -> Vec<String> {
    let mut ips: Vec<String> = config.listen.iter().map(|a| a.ip().to_string()).collect();
    ips.dedup();
    ips
}

/// Interfaces par lesquelles sort le poste, sans doublon.
fn uplinks() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for route in default_routes().into_iter().filter(|r| physical(&r.interface)) {
        if !names.contains(&route.interface) {
            names.push(route.interface);
        }
    }
    names
}

/// Mots de « Link 3 (wlo1): 192.168.1.1 fe80::1%3 » après les deux-points, tels que resolved les écrit.
fn link_words(text: &str) -> Vec<String> {
    text.split_once("): ").map_or("", |(_, list)| list).split_whitespace().map(str::to_owned).collect()
}

/// Valeurs d'une clé de `nmcli -t` (« IP4.DNS[1]:192.168.1.1 ») ; `:` et `\` y sont protégés par `\`.
fn nm_values(text: &str, key: &str) -> Vec<String> {
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
fn scoped(address: &str, interface: &str) -> String {
    match address.parse::<std::net::Ipv6Addr>() {
        Ok(ip) if ip.segments()[0] & 0xffc0 == 0xfe80 => format!("{address}%{interface}"),
        _ => address.to_owned(),
    }
}

fn is_loopback(word: &str) -> bool {
    word.split(['%', '#']).next().and_then(|a| a.parse::<IpAddr>().ok()).is_some_and(|ip| ip.is_loopback())
}

/// Ce que NetworkManager sait du réseau de l'interface : à jour, et inchangé par le réglage d'abdns.
fn nm_view(interface: &str) -> Saved {
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
fn link_state(interface: &str) -> Saved {
    let words = |what: &str| link_words(&output("resolvectl", &[what, interface]).unwrap_or_default());
    Saved { dns: words("dns"), domains: words("domain") }
}

/// Ce que l'interface doit retrouver : ce que NetworkManager sait, sinon le réglage d'avant abdns
/// (sans adresse de boucle locale ni `~.` : ce serait le relais).
fn restore_target(nm: Saved, saved: Option<&Saved>) -> Saved {
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
fn points_to_relay(interface: &str, relay: &[String]) -> bool {
    let current = parse_link_dns(&output("resolvectl", &["dns", interface]).unwrap_or_default());
    current.iter().any(|ip| relay.contains(&ip.to_string()))
}

/// Le réglage d'abdns est en place : DNS sur le relais et toutes les requêtes passant par lui (`~.`).
fn relay_applied(interface: &str, relay: &[String]) -> bool {
    points_to_relay(interface, relay)
        && link_words(&output("resolvectl", &["domain", interface]).unwrap_or_default()).iter().any(|d| d == "~.")
}

fn apply(interface: &str, relay: &[String]) -> bool {
    let mut dns = vec!["dns", interface];
    dns.extend(relay.iter().map(String::as_str));
    succeeds("resolvectl", &dns) && succeeds("resolvectl", &["domain", interface, "~."])
}

fn restore(interface: &str, saved: Option<&Saved>) -> bool {
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

/// `abdns logs` : le journal de systemd.
pub fn logs(follow: bool, lines: u32) -> Command {
    let mut command = Command::new("journalctl");
    command.args(["-u", "abdnsd", "-n", &lines.to_string()]);
    if follow {
        command.arg("-f");
    }
    command
}

/// Utilisateur de la session graphique active : (uid, nom). Personne sans écran (ssh, console).
fn graphical_session(sessions: &str, show: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    sessions.lines().find_map(|line| {
        // « SESSION UID USER SEAT TTY… » ; un siège « - » est une session à distance.
        let [id, uid, name, seat, ..] = line.split_whitespace().collect::<Vec<_>>()[..] else { return None };
        if !seat.starts_with("seat") {
            return None;
        }
        let details = show(id)?;
        let value = |key: &str| details.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
        (value("Active") == Some("yes") && matches!(value("Type"), Some("x11" | "wayland" | "mir")))
            .then(|| (uid.to_owned(), name.to_owned()))
    })
}

fn is_root() -> bool {
    // SAFETY : sans argument ni effet.
    unsafe { libc::geteuid() == 0 }
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

/// Commande qui lance `program` dans la session de l'utilisateur (le service tourne en root) :
/// `runuser` avec le bus et l'écran de sa session.
fn session_command(uid: &str, name: &str, wayland: bool, program: &str, args: &[&str]) -> Vec<String> {
    let runtime = format!("/run/user/{uid}");
    let mut command: Vec<String> = ["runuser", "-u", name, "--", "env"].map(str::to_owned).into();
    command.push(format!("XDG_RUNTIME_DIR={runtime}"));
    command.push(format!("DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus"));
    if wayland {
        command.push("WAYLAND_DISPLAY=wayland-0".to_owned());
    }
    command.push("DISPLAY=:0".to_owned());
    command.push(program.to_owned());
    command.extend(args.iter().map(|a| (*a).to_owned()));
    command
}

/// En root : dans la session graphique. Sans être root (essais avec `--no-system`) : tel quel.
fn as_session_user(program: &str, args: &[&str]) {
    let command = if is_root() {
        let sessions = output("loginctl", &["list-sessions", "--no-legend"]).unwrap_or_default();
        let show = |id: &str| output("loginctl", &["show-session", id, "-p", "Type", "-p", "Active"]);
        let Some((uid, name)) = graphical_session(&sessions, show) else { return };
        let wayland = Path::new("/run/user").join(&uid).join("wayland-0").exists();
        session_command(&uid, &name, wayland, program, args)
    } else {
        std::iter::once(program).chain(args.iter().copied()).map(str::to_owned).collect()
    };
    let status = Command::new(&command[0]).args(&command[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    if let Err(error) = status {
        logbook::log(&format!("{program}: {error}"));
    }
}

pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
    let hint = format!("--hint=string:desktop-entry:{DESKTOP_ID}");
    as_session_user("notify-send", &["--app-name=abdns", &hint, "--", title, body]);
}

pub fn open(url: &str) {
    as_session_user("xdg-open", &[url]);
}

/// Argument de `ExecStart` : `%` et `$` doublés, guillemets si l'argument a un espace.
fn unit_quote(arg: &str) -> String {
    let arg = arg.replace('%', "%%").replace('$', "$$");
    if arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg
    }
}

fn unit(program: &Path, config: &Path) -> String {
    let start = [program.to_string_lossy(), "--config".into(), config.to_string_lossy()].map(|a| unit_quote(&a)).join(" ");
    format!(
        "[Unit]\nDescription=abdns: encrypted DNS (DoH) for this computer\nDocumentation=https://github.com/abnd-corp/Abdns\n\
After=network-online.target systemd-resolved.service NetworkManager.service\nWants=network-online.target\n\n\
[Service]\nExecStart={start}\nRestart=always\nRestartSec=30\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

/// Entrée de bureau sans menu : elle donne son nom aux notifications du service.
fn desktop_entry(program: &Path) -> String {
    let exec: String =
        program.to_string_lossy().chars().flat_map(|c| if matches!(c, '\\' | '"' | '`' | '$') { vec!['\\', c] } else { vec![c] }).collect();
    format!(
        "[Desktop Entry]\nType=Application\nName=abdns\nComment=Encrypted DNS (DoH) for this computer\nExec=\"{exec}\"\nIcon=network-server\nNoDisplay=true\nTerminal=false\n"
    )
}

/// Service systemd sans gestionnaire de paquets : abdnsd installé à côté d'abdns.
/// La sortie d'erreur du service va au journal : `abdns logs`.
pub fn install(config: &Path) -> Result<()> {
    let program = std::env::current_exe()?.canonicalize()?.with_file_name("abdnsd");
    ensure!(program.exists(), "{} not found next to abdns", program.display());
    let config = config.canonicalize().with_context(|| format!("{} not found", config.display()))?;
    std::fs::write(UNIT_PATH, unit(&program, &config)).with_context(|| format!("cannot write {UNIT_PATH} (run with sudo)"))?;
    std::fs::write(DESKTOP_PATH, desktop_entry(&program)).with_context(|| format!("cannot write {DESKTOP_PATH} (run with sudo)"))?;
    ensure!(succeeds("systemctl", &["daemon-reload"]), "systemctl daemon-reload failed");
    ensure!(succeeds("systemctl", &["enable", UNIT]), "systemctl enable {UNIT} failed");
    ensure!(succeeds("systemctl", &["restart", UNIT]), "systemctl restart {UNIT} failed");
    Ok(())
}

pub fn uninstall(config: Option<&Config>) -> Result<()> {
    // L'arrêt du service remet déjà le DNS du réseau.
    succeeds("systemctl", &["disable", "--now", UNIT]);
    if Path::new(UNIT_PATH).exists() {
        std::fs::remove_file(UNIT_PATH).with_context(|| format!("cannot remove {UNIT_PATH} (run with sudo)"))?;
        succeeds("systemctl", &["daemon-reload"]);
    }
    if Path::new(DESKTOP_PATH).exists() {
        std::fs::remove_file(DESKTOP_PATH).with_context(|| format!("cannot remove {DESKTOP_PATH} (run with sudo)"))?;
    }
    if let Some(config) = config {
        set_system_dns(config, false)?;
    }
    Ok(())
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
    fn link_dns() {
        let dns = |s: &str| parse_link_dns(s).iter().map(IpAddr::to_string).collect::<Vec<_>>();
        assert_eq!(dns("Link 3 (wlo1): 192.168.1.1 fe80::1%3#box.lan\n"), ["192.168.1.1", "fe80::1"]);
        assert!(dns("Link 2 (enp2s0): \n").is_empty());
        assert!(dns("").is_empty());
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

    #[test]
    fn graphical_sessions() {
        let sessions = "1 1000 alice - - active no -\n3 1001 bob seat0 tty2 active no -\n";
        let show = |id: &str| Some(format!("Type={}\nActive=yes\n", if id == "3" { "wayland" } else { "tty" }));
        assert_eq!(graphical_session(sessions, show), Some(("1001".to_owned(), "bob".to_owned())));
        assert_eq!(graphical_session(sessions, |_| Some("Type=tty\nActive=yes\n".to_owned())), None);
        assert_eq!(graphical_session(sessions, |_| Some("Type=x11\nActive=no\n".to_owned())), None);
        assert_eq!(graphical_session("", show), None);
    }

    #[test]
    fn session_commands() {
        let command = session_command("1000", "rxdy", true, "notify-send", &["--", "t"]);
        assert_eq!(&command[..5], ["runuser", "-u", "rxdy", "--", "env"]);
        assert!(command.contains(&"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus".to_owned()));
        assert!(command.contains(&"WAYLAND_DISPLAY=wayland-0".to_owned()));
        assert_eq!(&command[command.len() - 3..], ["notify-send", "--", "t"]);
        assert!(!session_command("1000", "rxdy", false, "xdg-open", &[]).iter().any(|a| a.starts_with("WAYLAND")));
    }

    #[test]
    fn desktop_entries() {
        let text = desktop_entry(Path::new("/usr/local/bin/abdns"));
        assert!(text.starts_with("[Desktop Entry]\nType=Application\nName=abdns\n"));
        assert!(text.contains("Exec=\"/usr/local/bin/abdns\"\n"));
        assert!(text.contains("NoDisplay=true\n"));
        assert!(desktop_entry(Path::new("/opt/a\"b$c/abdns")).contains("Exec=\"/opt/a\\\"b\\$c/abdns\"\n"));
    }

    #[test]
    fn systemd_unit() {
        let text = unit(Path::new("/usr/local/bin/abdnsd"), Path::new("/etc/abdns/config.toml"));
        assert!(text.contains("ExecStart=/usr/local/bin/abdnsd --config /etc/abdns/config.toml\n"));
        assert!(text.contains("Restart=always\nRestartSec=30\n"));
        assert!(
            unit(Path::new("/opt/my abdns/abdnsd"), Path::new("/etc/a%b.toml"))
                .contains("ExecStart=\"/opt/my abdns/abdnsd\" --config /etc/a%%b.toml\n")
        );
    }
}
