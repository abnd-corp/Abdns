//! La garde : choisit entre le DNS chiffré et le DNS du réseau, signale ce qui part en clair,
//! ouvre la page d'un portail captif et recharge la configuration quand elle change.
//!
//! Bascule en clair seulement si tous les serveurs chiffrés échouent deux fois de suite alors
//! que le DNS du réseau répond : c'est la signature d'un portail ou d'un réseau qui bloque le
//! DNS chiffré. Retour au chiffré dès qu'il répond de nouveau.
//!
//! Notifications limitées : un changement de mode à la fois, les nouveaux domaines en clair au
//! plus toutes les 5 min, un rappel toutes les 30 min, au plus 6 notifications par 10 min.

mod network;
mod notice;
mod probe;
mod reload;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::json;

use crate::config::Config;
use crate::control::Signals;
use crate::i18n::Text;
use crate::platform::{self, Network};
use crate::relay::Shared;
use crate::resolver::Resolver;
use crate::{logbook, write};

use notice::Limiter;
pub use probe::probe_name;
use probe::{PORTAL_PROBE, answers, portal, sockets};
use reload::stamp;

/// Relecture du réseau sans abonnement du système (ou s'il échoue).
const NETWORK_EVERY: u64 = 5;
/// Avec abonnement : contrôle de sécurité, pour un changement de DNS seul qu'il ne verrait pas.
const NETWORK_SAFETY_EVERY: u64 = 60;
/// Date du fichier de configuration, et réglage du DNS du poste (défait par un autre service).
const CONFIG_EVERY: u64 = 5;
const CHECK_EVERY: u64 = 30;
const RETRY_AFTER: u64 = 3;
const LOCAL_CHECK_EVERY: u64 = 10;
const FIRST_NAMES_AFTER: u64 = 60;
const NAMES_EVERY: u64 = 300;
const PORTAL_EVERY: u64 = 60;
const REMINDER_EVERY: u64 = 30 * 60;
const FAILURES_BEFORE_LOCAL: u32 = 2;

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

pub struct Guard {
    config: Arc<Config>,
    config_path: PathBuf,
    config_stamp: Option<SystemTime>,
    signals: Arc<Signals>,
    system: bool,
    shared: Arc<Shared>,
    text: Text,
    network: Option<Network>,
    watch: Option<platform::Watch>,
    local: bool,
    failures: u32,
    next_check: u64,
    next_network: u64,
    next_config: u64,
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
            watch: platform::Watch::new(),
            local: false,
            failures: 0,
            next_check: 0,
            next_network: 0,
            next_config: 0,
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

    fn force(&self) -> Force {
        std::fs::read_to_string(force_file(&self.config)).ok().and_then(|t| Force::parse(&t)).unwrap_or(Force::Auto)
    }

    fn set_local_mode(&self, local: bool) {
        *write(&self.shared.local_mode) = local;
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

    fn tick(&mut self, now: u64) {
        // `abdns reload` : relecture même si la date du fichier n'a pas changé.
        if self.signals.reload.swap(false, Ordering::SeqCst) {
            self.config_stamp = None;
            self.next_network = now;
            self.next_config = now;
        }
        if self.signals.check.swap(false, Ordering::SeqCst) {
            self.next_check = now;
        }
        // Le système prévient d'un changement : plus besoin de relire le réseau toutes les 5 s.
        let changed = self.watch.as_mut().is_some_and(platform::Watch::changed);
        if changed || now >= self.next_network {
            self.next_network = now + if self.watch.is_some() { NETWORK_SAFETY_EVERY } else { NETWORK_EVERY };
            self.update_network(now);
        }
        if now >= self.next_config {
            self.next_config = now + CONFIG_EVERY;
            self.reload(now);
            self.keep_system_dns();
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
pub fn wait_encrypted(resolver: &Arc<Resolver>, secs: u64) -> bool {
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
    fn forced_mode() {
        assert_eq!(Force::parse("local\n"), Some(Force::Local));
        assert_eq!(Force::parse("encrypted"), Some(Force::Encrypted));
        assert!(Force::parse("whatever").is_none());
    }
}
