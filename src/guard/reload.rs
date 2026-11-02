//! Configuration relue quand le fichier change (ou sur `abdns reload`), appliquée sans
//! redémarrer si elle est valide.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use super::Guard;
use crate::config::Config;
use crate::i18n::Text;
use crate::resolver::Resolver;
use crate::{logbook, platform, write};

pub(super) fn stamp(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Guard {
    /// Fichier de configuration modifié : relu ; s'il est valide, appliqué sans redémarrer.
    pub(super) fn reload(&mut self, now: u64) {
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
                *write(&self.shared.resolver) = Arc::new(resolver);
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
}
