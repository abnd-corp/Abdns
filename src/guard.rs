//! La garde : choisit entre le DNS chiffré et le DNS du réseau, signale ce qui part en clair,
//! ouvre la page d'un portail captif et recharge la configuration quand elle change.
//!
//! Bascule en clair seulement si tous les serveurs chiffrés échouent deux fois de suite alors
//! que le DNS du réseau répond : c'est la signature d'un portail ou d'un réseau qui bloque le
//! DNS chiffré. Retour au chiffré dès qu'il répond de nouveau.
//!
//! Notifications limitées : un changement de mode à la fois, les nouveaux domaines en clair au
//! plus toutes les 5 min, un rappel toutes les 30 min, au plus 6 notifications par 10 min.

use std::collections::{BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::config::Config;
use crate::control::Signals;
use crate::doh::Resolver;
use crate::i18n::Text;
use crate::platform::{self, Network};
use crate::relay::{Lan, Shared, exchange};
use crate::{dns, logbook};

const NETWORK_EVERY: u64 = 5;
const CHECK_EVERY: u64 = 30;
const RETRY_AFTER: u64 = 3;
const LOCAL_CHECK_EVERY: u64 = 10;
const FIRST_NAMES_AFTER: u64 = 60;
const NAMES_EVERY: u64 = 300;
const PORTAL_EVERY: u64 = 60;
const REMINDER_EVERY: u64 = 30 * 60;
/// Secours utilisé depuis au moins une minute avant de le signaler.
const FALLBACK_NOTICE_AFTER: u64 = 60;
const NOTICE_WINDOW: u64 = 600;
const NOTICES_PER_WINDOW: usize = 6;
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
pub fn probe_name() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("abdns-{nanos:x}{:x}.example.org", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Vrai si le serveur en clair répond (NOERROR ou NXDOMAIN) dans le délai.
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

/// Noms pas encore signalés, ordre d'arrivée gardé ; `reported` retient les signalés.
fn new_names(names: &[String], reported: &mut BTreeSet<String>) -> Vec<String> {
    names.iter().filter(|n| reported.insert((*n).clone())).cloned().collect()
}

/// (noms cités, nombre de noms en plus).
fn summary(names: &[String]) -> (String, usize) {
    (names[..names.len().min(NAMES_SHOWN)].join(", "), names.len().saturating_sub(NAMES_SHOWN))
}

/// Au plus `NOTICES_PER_WINDOW` notifications par fenêtre ; les autres vont au journal seul.
#[derive(Default)]
struct Limiter {
    sent: VecDeque<u64>,
}

impl Limiter {
    fn allow(&mut self, now: u64) -> bool {
        while self.sent.front().is_some_and(|&t| now.saturating_sub(t) >= NOTICE_WINDOW) {
            self.sent.pop_front();
        }
        if self.sent.len() >= NOTICES_PER_WINDOW {
            return false;
        }
        self.sent.push_back(now);
        true
    }
}

pub struct Guard {
    config: Arc<Config>,
    config_path: PathBuf,
    config_stamp: Option<SystemTime>,
    signals: Arc<Signals>,
    system: bool,
    shared: Arc<Shared>,
    text: Text,
    network: Option<Network>,
    local: bool,
    failures: u32,
    next_check: u64,
    next_network: u64,
    local_since: u64,
    clear_total: usize,
    reported: BTreeSet<String>,
    next_report: u64,
    next_portal: u64,
    next_reminder: u64,
    portal_opened: Option<String>,
    fallback_since: Option<u64>,
    fallback_notified: bool,
    local_disabled_notified: bool,
    limiter: Limiter,
}

fn stamp(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Guard {
    pub fn new(config: Arc<Config>, config_path: PathBuf, system: bool, shared: Arc<Shared>, signals: Arc<Signals>) -> Self {
        let text = Text(config.language);
        Self {
            config_stamp: stamp(&config_path),
            signals,
            config,
            config_path,
            system,
            shared,
            text,
            network: None,
            local: false,
            failures: 0,
            next_check: 0,
            next_network: 0,
            local_since: 0,
            clear_total: 0,
            reported: BTreeSet::new(),
            next_report: 0,
            next_portal: 0,
            next_reminder: 0,
            portal_opened: None,
            fallback_since: None,
            fallback_notified: false,
            local_disabled_notified: false,
            limiter: Limiter::default(),
        }
    }

    fn notify(&mut self, now: u64, title: &str, body: &str) {
        if self.limiter.allow(now) {
            platform::notify(title, body);
        } else {
            logbook::log(&format!("notification held back: {title} — {body}"));
        }
    }

    fn network_label(&self) -> String {
        self.network.as_ref().map_or_else(|| self.text.unknown_network(), |n| n.label.clone())
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
        self.reported.clear();
        self.next_report = now + FIRST_NAMES_AFTER;
        self.next_portal = now;
        self.next_reminder = now + REMINDER_EVERY;
        let body = self.text.clear_body(reason, &self.network_label());
        self.notify(now, &self.text.clear_title(), &body);
    }

    fn go_encrypted(&mut self, now: u64) {
        self.set_local_mode(false);
        if !self.local {
            return;
        }
        self.report_clear(now);
        self.local = false;
        self.portal_opened = None;
        let minutes = now.saturating_sub(self.local_since).div_ceil(60);
        let body = self.text.restored_body(minutes, self.clear_total);
        self.notify(now, &self.text.restored_title(), &body);
    }

    /// Domaines partis en clair depuis le dernier relevé : tous au journal, les nouveaux en
    /// notification.
    fn report_clear(&mut self, now: u64) {
        let names = self.shared.clear.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
        if names.is_empty() {
            return;
        }
        logbook::clear_names(&self.config.clear_log, &self.network_label().replace(' ', "_"), &names);
        let fresh = new_names(&names, &mut self.reported);
        self.clear_total += fresh.len();
        if !fresh.is_empty() {
            let (shown, more) = summary(&fresh);
            let body = self.text.names(&shown, more);
            self.notify(now, &self.text.names_title(), &body);
        }
    }

    /// Serveur principal muet depuis une minute : un message ; de retour : un message.
    fn follow_servers(&mut self, now: u64, answering: Option<usize>, resolver: &Resolver) {
        match answering {
            Some(0) => {
                self.fallback_since = None;
                if self.fallback_notified {
                    self.fallback_notified = false;
                    let body = self.text.primary_back_body(resolver.name(0));
                    self.notify(now, &self.text.primary_back_title(), &body);
                }
            }
            Some(index) => {
                let since = *self.fallback_since.get_or_insert(now);
                if !self.fallback_notified && now.saturating_sub(since) >= FALLBACK_NOTICE_AFTER {
                    self.fallback_notified = true;
                    let body = self.text.fallback_body(resolver.name(0), resolver.name(index));
                    self.notify(now, &self.text.fallback_title(), &body);
                }
            }
            None => {}
        }
    }

    fn check(&mut self, now: u64) {
        let force = self.force();
        let resolver = self.shared.resolver();
        let answering = resolver.probe(&probe_name());
        self.follow_servers(now, answering, &resolver);
        let has_network_dns = !self.network_dns().is_empty();
        match force {
            Force::Encrypted => self.go_encrypted(now),
            Force::Local if has_network_dns => self.go_local(now, &self.text.forced_local()),
            Force::Local => logbook::log("local mode forced, but the network announces no DNS"),
            Force::Auto if answering.is_some() => {
                self.failures = 0;
                self.local_disabled_notified = false;
                self.go_encrypted(now);
            }
            Force::Auto => {
                self.failures += 1;
                if !self.local {
                    logbook::log(&format!("encrypted DNS silent ({} in a row) on {}", self.failures, self.network_label()));
                }
                if self.failures >= FAILURES_BEFORE_LOCAL && !self.local {
                    if !self.config.local_fallback {
                        if !self.local_disabled_notified {
                            self.local_disabled_notified = true;
                            let body = self.text.local_disabled_body(&self.network_label());
                            self.notify(now, &self.text.no_encrypted(), &body);
                        }
                    } else if has_network_dns && sockets(&self.network_dns()).into_iter().any(|s| answers(s, Duration::from_secs(2))) {
                        self.go_local(now, &self.text.no_encrypted());
                    } else if has_network_dns {
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
        self.write_state(force, answering, &resolver);
    }

    fn write_state(&self, force: Force, answering: Option<usize>, resolver: &Resolver) {
        let servers: Vec<_> = resolver.snapshot().into_iter().map(|(name, up)| json!({ "name": name, "up": up })).collect();
        let state = json!({
            "mode": if self.local { "local" } else { "encrypted" },
            "forced": force.name(),
            "answering_server": answering.map(|i| resolver.name(i)),
            "last_used_server": resolver.active().map(|i| resolver.name(i)),
            "servers": servers,
            "local_fallback": self.config.local_fallback,
            "network": self.network.as_ref().map(|n| json!({
                "name": n.label, "interface": n.interface, "gateway": n.gateway,
                "dns": n.dns.iter().map(IpAddr::to_string).collect::<Vec<_>>(), "domain": n.domain,
            })),
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
            let description = current.as_ref().map_or_else(|| "none".to_owned(), |n| format!("{} {} (DNS {:?})", n.label, n.key(), n.dns));
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

    /// Fichier de configuration modifié : relu ; s'il est valide, appliqué sans redémarrer.
    fn reload(&mut self, now: u64) {
        let current = stamp(&self.config_path);
        if current == self.config_stamp {
            return;
        }
        self.config_stamp = current;
        let applied = Config::read(&self.config_path).and_then(|new| Ok((Resolver::new(&new)?, new)));
        match applied {
            Ok((_, new)) if new == *self.config => {}
            Ok((resolver, new)) => {
                if new.listen != self.config.listen {
                    logbook::log("listen changed: restart abdns to apply it");
                }
                let services_changed = new.network_services != self.config.network_services;
                if let Ok(mut shared) = self.shared.resolver.write() {
                    *shared = Arc::new(resolver);
                }
                self.text = Text(new.language);
                self.config = Arc::new(new);
                if self.system
                    && services_changed
                    && let Err(error) = platform::set_system_dns(&self.config, true)
                {
                    logbook::log(&format!("{error:#}"));
                }
                self.fallback_since = None;
                self.fallback_notified = false;
                self.next_check = now;
                let names: Vec<&str> = self.config.server.iter().map(|s| s.name.as_str()).collect();
                logbook::log(&format!("configuration reloaded; servers: {}", names.join(", ")));
                let body = self.text.reloaded_body(&names.join(" → "));
                self.notify(now, &self.text.reloaded_title(), &body);
            }
            Err(error) => {
                logbook::log(&format!("configuration ignored: {error:#}"));
                let first_line = format!("{error:#}").lines().next().unwrap_or_default().to_owned();
                let body = self.text.invalid_config_body(&first_line);
                self.notify(now, &self.text.invalid_config_title(), &body);
            }
        }
    }

    fn tick(&mut self, now: u64) {
        // `abdns reload` : relecture même si la date du fichier n'a pas changé.
        if self.signals.reload.swap(false, Ordering::SeqCst) {
            self.config_stamp = None;
            self.next_network = now;
        }
        if self.signals.check.swap(false, Ordering::SeqCst) {
            self.next_check = now;
        }
        if now >= self.next_network {
            self.next_network = now + NETWORK_EVERY;
            self.update_network(now);
            self.reload(now);
        }
        if now >= self.next_check {
            self.check(now);
        }
        if !self.local {
            return;
        }
        if now >= self.next_report {
            self.next_report = now + NAMES_EVERY;
            self.report_clear(now);
        }
        let key = self.network.as_ref().map(Network::key);
        if now >= self.next_portal && self.portal_opened != key {
            self.next_portal = now + PORTAL_EVERY;
            if portal(&self.network_dns()) {
                self.portal_opened = key;
                self.notify(now, &self.text.portal_title(), &self.text.portal_body());
                platform::open(PORTAL_PROBE);
            }
        }
        if now >= self.next_reminder {
            self.next_reminder = now + REMINDER_EVERY;
            let minutes = now.saturating_sub(self.local_since) / 60;
            let body = self.text.still_clear_body(minutes, &self.network_label());
            self.notify(now, &self.text.still_clear_title(), &body);
        }
    }

    pub fn run(mut self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) {
            self.tick(logbook::now());
            thread::sleep(Duration::from_secs(1));
        }
        let now = logbook::now();
        self.report_clear(now);
    }
}

/// Attend qu'un serveur chiffré réponde (démarrage), au plus `secs` secondes.
pub fn wait_encrypted(resolver: &Resolver, secs: u64) -> bool {
    let start = logbook::now();
    while logbook::now().saturating_sub(start) < secs {
        if resolver.probe(&probe_name()).is_some() {
            return true;
        }
        thread::sleep(Duration::from_secs(1));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_new_names_are_reported() {
        let mut reported = BTreeSet::new();
        let first: Vec<String> = ["wifi.sncf", "captive.apple.com", "wifi.sncf"].map(str::to_owned).to_vec();
        assert_eq!(new_names(&first, &mut reported), vec!["wifi.sncf".to_owned(), "captive.apple.com".to_owned()]);
        let second: Vec<String> = ["wifi.sncf", "lemonde.fr"].map(str::to_owned).to_vec();
        assert_eq!(new_names(&second, &mut reported), vec!["lemonde.fr".to_owned()]);
        let many: Vec<String> = (0..7).map(|i| format!("n{i}.fr")).collect();
        assert_eq!(summary(&many), ("n0.fr, n1.fr, n2.fr, n3.fr".to_owned(), 3));
    }

    #[test]
    fn notifications_are_capped() {
        let mut limiter = Limiter::default();
        assert!((0..NOTICES_PER_WINDOW).all(|i| limiter.allow(100 + i as u64)));
        assert!(!limiter.allow(200));
        assert!(limiter.allow(100 + NOTICE_WINDOW));
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
