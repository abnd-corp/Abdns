//! Notifications de la garde : limitées en nombre, nouveaux domaines en clair seulement,
//! serveur de secours signalé après une minute.

use std::collections::{BTreeSet, VecDeque};

use super::Guard;
use crate::resolver::Resolver;
use crate::{lock, logbook, platform};

/// Secours utilisé depuis au moins une minute avant de le signaler.
const FALLBACK_NOTICE_AFTER: u64 = 60;
const NOTICE_WINDOW: u64 = 600;
const NOTICES_PER_WINDOW: usize = 6;
/// Noms cités dans une notification ; les suivants sont comptés.
const NAMES_SHOWN: usize = 4;

/// Noms pas encore signalés, ordre d'arrivée gardé ; `reported` retient les signalés.
pub(super) fn new_names(names: &[String], reported: &mut BTreeSet<String>) -> Vec<String> {
    names.iter().filter(|n| reported.insert((*n).clone())).cloned().collect()
}

/// (noms cités, nombre de noms en plus).
pub(super) fn summary(names: &[String]) -> (String, usize) {
    (names[..names.len().min(NAMES_SHOWN)].join(", "), names.len().saturating_sub(NAMES_SHOWN))
}

/// Au plus `NOTICES_PER_WINDOW` notifications par fenêtre ; les autres vont au journal seul.
#[derive(Default)]
pub(super) struct Limiter {
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

impl Guard {
    pub(super) fn notify(&mut self, now: u64, title: &str, body: &str) {
        if self.limiter.allow(now) {
            platform::notify(title, body);
        } else {
            logbook::log(&format!("notification held back: {title} — {body}"));
        }
    }

    /// Domaines partis en clair depuis le dernier relevé : tous au journal, les nouveaux en
    /// notification.
    pub(super) fn report_clear(&mut self, now: u64) {
        let names = std::mem::take(&mut *lock(&self.shared.clear));
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
    pub(super) fn follow_servers(&mut self, now: u64, answering: Option<usize>, resolver: &Resolver) {
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
}
