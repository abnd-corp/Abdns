//! macOS : outils du système uniquement (route, ipconfig, networksetup, osascript, launchctl).

use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

use super::{Network, external};
use crate::config::Config;
use crate::logbook;

pub const CONFIG_PATH: &str = "/etc/abdns/config.toml";
const LABEL: &str = "com.abnd.abdns";
const PLIST: &str = "/Library/LaunchDaemons/com.abnd.abdns.plist";
const NEWSYSLOG: &str = "/etc/newsyslog.d/abdns.conf";
const LOG_DIR: &str = "/var/log/abdns";

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Valeur d'une ligne « clé: valeur » de `route -n get default`.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(':').map(str::trim))
}

/// Option d'un bail DHCP affiché par `ipconfig getpacket` : « nom (type): valeur ».
fn dhcp_option<'a>(packet: &'a str, name: &str) -> Option<&'a str> {
    packet.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix(name)?.trim_start();
        rest.starts_with('(').then(|| rest.split_once(':').map(|(_, v)| v.trim()))?
    })
}

/// DNS du bail : « domain_name_server (ip_mult): {a, b} ».
fn dhcp_dns(packet: &str) -> Vec<IpAddr> {
    let list = dhcp_option(packet, "domain_name_server").and_then(|v| v.strip_prefix('{')?.strip_suffix('}')).unwrap_or("");
    external(list.split(',').filter_map(|a| a.trim().parse().ok()))
}

/// Domaine du bail, en minuscules, s'il est un nom plausible.
fn dhcp_domain(packet: &str) -> Option<String> {
    let domain = dhcp_option(packet, "domain_name")?.trim_end_matches('.').to_ascii_lowercase();
    (!domain.is_empty() && domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')).then_some(domain)
}

pub fn network() -> Option<Network> {
    let route = output("/sbin/route", &["-n", "get", "default"])?;
    let interface = field(&route, "interface")?.to_owned();
    let gateway = field(&route, "gateway").unwrap_or("-").to_owned();
    // Un VPN (utun) n'a pas de bail DHCP : pas de DNS du réseau à proposer.
    let packet = output("/usr/sbin/ipconfig", &["getpacket", &interface]).unwrap_or_default();
    Some(Network { interface, gateway, dns: dhcp_dns(&packet), domain: dhcp_domain(&packet) })
}

/// `relay` : DNS des services réseau sur le relais ; sinon, DNS du réseau (réglage vidé).
pub fn set_system_dns(config: &Config, relay: bool) -> Result<()> {
    let servers: Vec<String> = if relay {
        let mut ips: Vec<String> = config.listen.iter().map(|a| a.ip().to_string()).collect();
        ips.dedup();
        ips
    } else {
        vec!["Empty".to_owned()]
    };
    let known = output("/usr/sbin/networksetup", &["-listallnetworkservices"]).context("networksetup")?;
    let mut failures = Vec::new();
    for service in &config.network_services {
        // Service absent de ce Mac (adaptateur jamais branché) : rien à régler.
        if !known.lines().any(|l| l.trim_start_matches('*') == service) {
            continue;
        }
        let ok = Command::new("/usr/sbin/networksetup")
            .arg("-setdnsservers")
            .arg(service)
            .args(&servers)
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            failures.push(service.as_str());
        }
    }
    ensure!(failures.is_empty(), "DNS not set for: {}", failures.join(", "));
    Ok(())
}

/// Utilisateur de la session graphique : (uid, nom). Personne à l'écran de connexion.
fn console_user() -> Option<(String, String)> {
    let uid = output("/usr/bin/stat", &["-f", "%u", "/dev/console"])?.trim().to_owned();
    let name = output("/usr/bin/stat", &["-f", "%Su", "/dev/console"])?.trim().to_owned();
    (uid != "0").then_some((uid, name))
}

/// Lance un programme dans la session de l'utilisateur (le service tourne en root).
fn as_console_user(program: &str, args: &[&str]) {
    let Some((uid, name)) = console_user() else { return };
    let mut command = Command::new("/bin/launchctl");
    command.args(["asuser", &uid, "/usr/bin/sudo", "-u", &name, program]).args(args);
    if let Err(error) = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
        logbook::log(&format!("{program}: {error}"));
    }
}

/// Titre et texte passés en arguments à osascript : aucun échappement à faire.
pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
    as_console_user(
        "/usr/bin/osascript",
        &["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)", "-e", "end run", title, body],
    );
}

pub fn open(url: &str) {
    as_console_user("/usr/bin/open", &[url]);
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plist(program: &Path, config: &Path) -> String {
    let arguments = [program.to_string_lossy().into_owned(), "--config".into(), config.to_string_lossy().into_owned(), "service".into()]
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{LABEL}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n{arguments}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>KeepAlive</key>\n\t<true/>\n\t<key>ThrottleInterval</key>\n\t<integer>30</integer>\n\
\t<key>StandardOutPath</key>\n\t<string>{LOG_DIR}/abdns.log</string>\n\t<key>StandardErrorPath</key>\n\t<string>{LOG_DIR}/abdns.log</string>\n</dict>\n</plist>\n"
    )
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Service launchd sans Nix : le programme tel qu'il est installé, la configuration donnée.
pub fn install(config: &Path) -> Result<()> {
    let program = std::env::current_exe()?.canonicalize()?;
    let config = config.canonicalize().with_context(|| format!("{} not found", config.display()))?;
    std::fs::create_dir_all(LOG_DIR)?;
    std::fs::write(PLIST, plist(&program, &config)).with_context(|| format!("cannot write {PLIST} (run with sudo)"))?;
    // Rotation à 1 Mio, 3 générations compressées.
    std::fs::write(NEWSYSLOG, format!("{LOG_DIR}/abdns.log 644 3 1024 * Z\n"))?;
    launchctl(&["bootout", &format!("system/{LABEL}")]);
    ensure!(launchctl(&["bootstrap", "system", PLIST]), "launchctl bootstrap {PLIST} failed");
    Ok(())
}

pub fn uninstall(config: Option<&Config>) -> Result<()> {
    launchctl(&["bootout", &format!("system/{LABEL}")]);
    for path in [PLIST, NEWSYSLOG] {
        if Path::new(path).exists() {
            std::fs::remove_file(path).with_context(|| format!("cannot remove {path} (run with sudo)"))?;
        }
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
    fn launchd_plist() {
        let p = plist(Path::new("/usr/local/bin/abdns"), Path::new("/etc/abdns/config.toml"));
        assert!(p.contains("<string>com.abnd.abdns</string>"));
        assert!(p.contains("<string>/usr/local/bin/abdns</string>\n\t\t<string>--config</string>\n\t\t<string>/etc/abdns/config.toml</string>\n\t\t<string>service</string>"));
    }
}
