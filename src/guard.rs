//! La garde : choisit entre le DNS chiffré et le DNS du réseau, signale ce qui part en clair
//! et ouvre la page d'un portail captif.
//!
//! Bascule en clair seulement si dnsproxy échoue deux fois de suite (serveur principal et
//! tous les secours) alors que le DNS du réseau répond : c'est la signature d'un portail ou
//! d'un réseau qui bloque le DNS chiffré. Retour au chiffré dès qu'il répond de nouveau.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::config::Config;
use crate::i18n::Text;
use crate::platform::{self, Network};
use crate::relay::{Lan, Shared, exchange};
use crate::{dns, logbook};

const NETWORK_EVERY: u64 = 5;
const CHECK_EVERY: u64 = 30;
const RETRY_AFTER: u64 = 3;
const LOCAL_CHECK_EVERY: u64 = 10;
const CLEAR_REPORT_EVERY: u64 = 30;
const PORTAL_EVERY: u64 = 60;
const REMINDER_EVERY: u64 = 15 * 60;
const FAILURES_BEFORE_LOCAL: u32 = 2;
/// Noms cités dans une notification ; les suivants sont comptés.
const NAMES_SHOWN: usize = 4;
const PORTAL_PROBE: &str = "http://captive.apple.com/hotspot-detect.html";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Force {
    Auto,
    Encrypted,
    Local,
}

impl Force {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "auto" => Some(Self::Auto),
            "encrypted" => Some(Self::Encrypted),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Encrypted => "encrypted",
            Self::Local => "local",
        }
    }
}

pub fn force_file(config: &Config) -> PathBuf {
    config.state_dir.join("mode")
}

pub fn state_file(config: &Config) -> PathBuf {
    config.state_dir.join("status.json")
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Nom jamais demandé (donc jamais en cache) d'une zone signée : NXDOMAIN attendu.
fn probe_name() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("abdns-{nanos:x}{:x}.example.org", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Vrai si le serveur répond (NOERROR ou NXDOMAIN) dans le délai.
fn answers(server: SocketAddr, timeout: Duration) -> bool {
    let query = dns::query(0x6764, &probe_name(), 1);
    exchange(server, &query, false, timeout).is_ok_and(|r| matches!(dns::rcode(&r), Some(0 | dns::NXDOMAIN)))
}

fn sockets(dns: &[IpAddr]) -> Vec<SocketAddr> {
    dns.iter().map(|&ip| SocketAddr::new(ip, 53)).collect()
}

/// Portail captif : la page d'essai d'Apple, demandée par le DNS du réseau, ne répond pas
/// « Success ». Sans réponse du tout, on ne conclut rien.
fn portal(dns: &[IpAddr]) -> bool {
    let query = dns::query(0x7074, "captive.apple.com", 1);
    let Some(ip) = sockets(dns)
        .into_iter()
        .find_map(|s| exchange(s, &query, false, Duration::from_secs(2)).ok())
        .and_then(|r| dns::ipv4_answers(&r).first().copied())
    else {
        return false;
    };
    let fetch = || -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), 80), Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(4)))?;
        stream.write_all(b"GET /hotspot-detect.html HTTP/1.0\r\nHost: captive.apple.com\r\nUser-Agent: CaptiveNetworkSupport\r\n\r\n")?;
        let mut body = Vec::new();
        stream.take(65_536).read_to_end(&mut body)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    };
    fetch().is_ok_and(|page| !page.contains("Success"))
}

/// Doublons retirés, ordre d'arrivée gardé : (noms cités, nombre de noms en plus).
fn summary(names: &[String]) -> (String, usize) {
    let mut seen = BTreeSet::new();
    let unique: Vec<&str> = names.iter().filter(|n| seen.insert(n.as_str())).map(String::as_str).collect();
    (unique[..unique.len().min(NAMES_SHOWN)].join(", "), unique.len().saturating_sub(NAMES_SHOWN))
}

pub struct Guard {
    config: Arc<Config>,
    shared: Arc<Shared>,
    text: Text,
    network: Option<Network>,
    local: bool,
    failures: u32,
    next_check: u64,
    next_network: u64,
    local_since: u64,
    clear_total: usize,
    next_report: u64,
    next_portal: u64,
    next_reminder: u64,
    portal_opened: Option<String>,
}

impl Guard {
    pub fn new(config: Arc<Config>, shared: Arc<Shared>) -> Self {
        let text = Text(config.language);
        Self {
            config,
            shared,
            text,
            network: None,
            local: false,
            failures: 0,
            next_check: 0,
            next_network: 0,
            local_since: 0,
            clear_total: 0,
            next_report: 0,
            next_portal: 0,
            next_reminder: 0,
            portal_opened: None,
        }
    }

    fn network_name(&self) -> String {
        self.network.as_ref().map_or_else(|| self.text.unknown_network(), Network::key)
    }

    fn network_dns(&self) -> Vec<IpAddr> {
        self.network.as_ref().map(|n| n.dns.clone()).unwrap_or_default()
    }

    fn force(&self) -> Force {
        std::fs::read_to_string(force_file(&self.config)).ok().and_then(|t| Force::parse(&t)).unwrap_or(Force::Auto)
    }

    fn set_local_mode(&self, local: bool) {
        if let Ok(mut mode) = self.shared.local_mode.write() {
            *mode = local;
        }
    }

    fn go_local(&mut self, now: u64, reason: &str) {
        if self.local {
            return;
        }
        self.set_local_mode(true);
        self.local = true;
        self.local_since = now;
        self.clear_total = 0;
        self.next_report = now + 10;
        self.next_portal = now;
        self.next_reminder = now + REMINDER_EVERY;
        let servers = self.network_dns().iter().map(IpAddr::to_string).collect::<Vec<_>>().join(", ");
        platform::notify(&self.text.clear_title(), &self.text.clear_body(reason, &self.network_name(), &servers));
    }

    fn go_encrypted(&mut self, now: u64) {
        self.set_local_mode(false);
        if !self.local {
            return;
        }
        self.report_clear();
        self.local = false;
        self.portal_opened = None;
        let minutes = now.saturating_sub(self.local_since).div_ceil(60);
        platform::notify(&self.text.restored_title(), &self.text.restored_body(minutes, self.clear_total));
    }

    /// Domaines partis en clair depuis le dernier relevé : journal et notification.
    fn report_clear(&mut self) {
        let names = self.shared.clear.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
        if names.is_empty() {
            return;
        }
        logbook::clear_names(&self.config.clear_log, &self.network_name().replace(' ', "_"), &names);
        self.clear_total += names.len();
        let (shown, more) = summary(&names);
        platform::notify(&self.text.names_title(), &self.text.names(&shown, more));
    }

    fn check(&mut self, now: u64) {
        let force = self.force();
        let encrypted_ok = answers(self.config.proxy(), Duration::from_secs(6));
        let has_network_dns = !self.network_dns().is_empty();
        match force {
            Force::Encrypted => self.go_encrypted(now),
            Force::Local if has_network_dns => self.go_local(now, &self.text.forced_local()),
            Force::Local => logbook::log("local mode forced, but the network announces no DNS"),
            Force::Auto if encrypted_ok => {
                self.failures = 0;
                self.go_encrypted(now);
            }
            Force::Auto => {
                self.failures += 1;
                if !self.local {
                    logbook::log(&format!("encrypted DNS silent ({} in a row) on {}", self.failures, self.network_name()));
                }
                if self.failures >= FAILURES_BEFORE_LOCAL && has_network_dns {
                    if sockets(&self.network_dns()).into_iter().any(|s| answers(s, Duration::from_secs(2))) {
                        self.go_local(now, &self.text.no_encrypted());
                    } else {
                        logbook::log("the network DNS does not answer either: staying encrypted");
                    }
                }
            }
        }
        let wait = if self.local {
            LOCAL_CHECK_EVERY
        } else if self.failures > 0 {
            RETRY_AFTER
        } else {
            CHECK_EVERY
        };
        self.next_check = now + wait;
        self.write_state(force, encrypted_ok);
    }

    fn write_state(&self, force: Force, encrypted_ok: bool) {
        let state = json!({
            "mode": if self.local { "local" } else { "encrypted" },
            "forced": force.name(),
            "encrypted_dns_answers": encrypted_ok,
            "network": self.network_name(),
            "network_dns": self.network_dns().iter().map(IpAddr::to_string).collect::<Vec<_>>(),
            "network_domain": self.network.as_ref().and_then(|n| n.domain.clone()),
            "clear_since": self.local.then(|| logbook::date(self.local_since)),
            "clear_names": self.clear_total,
            "checked": logbook::date(logbook::now()),
        });
        let path = state_file(&self.config);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, format!("{state:#}\n")).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
            logbook::log(&format!("cannot write {}", path.display()));
        }
    }

    fn update_network(&mut self, now: u64) {
        let current = platform::network();
        if current.as_ref().map(Network::key) != self.network.as_ref().map(Network::key) {
            let description = current.as_ref().map_or_else(|| "none".to_owned(), |n| format!("{} (DNS {:?})", n.key(), n.dns));
            logbook::log(&format!("network: {description}"));
            self.failures = 0;
            self.next_check = now;
        }
        if current != self.network {
            let lan = current.as_ref().map(|n| Lan { servers: sockets(&n.dns), domain: n.domain.clone() }).unwrap_or_default();
            if let Ok(mut shared) = self.shared.lan.write() {
                *shared = lan;
            }
            if self.local {
                self.next_check = now;
            }
        }
        self.network = current;
    }

    fn tick(&mut self, now: u64) {
        if now >= self.next_network {
            self.next_network = now + NETWORK_EVERY;
            self.update_network(now);
        }
        if now >= self.next_check {
            self.check(now);
        }
        if !self.local {
            return;
        }
        if now >= self.next_report {
            self.next_report = now + CLEAR_REPORT_EVERY;
            self.report_clear();
        }
        let key = self.network_name();
        if now >= self.next_portal && self.portal_opened.as_deref() != Some(key.as_str()) {
            self.next_portal = now + PORTAL_EVERY;
            if portal(&self.network_dns()) {
                self.portal_opened = Some(key);
                platform::notify(&self.text.portal_title(), &self.text.portal_body());
                platform::open(PORTAL_PROBE);
            }
        }
        if now >= self.next_reminder {
            self.next_reminder = now + REMINDER_EVERY;
            let minutes = now.saturating_sub(self.local_since) / 60;
            platform::notify(&self.text.still_clear_title(), &self.text.still_clear_body(minutes, &self.network_name()));
        }
    }

    pub fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            self.tick(logbook::now());
            thread::sleep(Duration::from_secs(1));
        }
        self.report_clear();
    }
}

/// Attend que dnsproxy réponde (démarrage), au plus `secs` secondes.
pub fn wait_encrypted(config: &Config, secs: u64) -> bool {
    (0..secs).any(|_| {
        let ok = answers(config.proxy(), Duration::from_secs(1));
        if !ok {
            thread::sleep(Duration::from_secs(1));
        }
        ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_summary() {
        let names: Vec<String> = ["wifi.sncf", "captive.apple.com", "wifi.sncf"].map(str::to_owned).to_vec();
        assert_eq!(summary(&names), ("wifi.sncf, captive.apple.com".to_owned(), 0));
        let many: Vec<String> = (0..7).map(|i| format!("n{i}.fr")).collect();
        assert_eq!(summary(&many), ("n0.fr, n1.fr, n2.fr, n3.fr".to_owned(), 3));
    }

    #[test]
    fn forced_mode() {
        assert_eq!(Force::parse("local\n"), Some(Force::Local));
        assert_eq!(Force::parse("encrypted"), Some(Force::Encrypted));
        assert!(Force::parse("whatever").is_none());
    }

    #[test]
    fn unique_probe_names() {
        assert_ne!(probe_name(), probe_name());
    }
}
