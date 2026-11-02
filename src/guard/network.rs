//! Le réseau vu par la garde : relu à chaque changement, transmis au relais ; réglage du DNS
//! du poste refait s'il a été défait.

use std::net::IpAddr;
use std::sync::Arc;

use super::Guard;
use super::probe::sockets;
use crate::platform::{self, Network};
use crate::relay::Lan;
use crate::{logbook, write};

impl Guard {
    pub(super) fn network_label(&self) -> String {
        self.network.as_ref().map_or_else(|| self.text.unknown_network(), |n| n.label.clone())
    }

    pub(super) fn network_dns(&self) -> Vec<IpAddr> {
        self.network.as_ref().map(|n| n.dns.clone()).unwrap_or_default()
    }

    pub(super) fn update_network(&mut self, now: u64) {
        let current = platform::network();
        if current.as_ref().map(Network::key) != self.network.as_ref().map(Network::key) {
            let description = current.as_ref().map_or_else(|| "none".to_owned(), |n| format!("{} {} (DNS {:?})", n.label, n.key(), n.dns));
            logbook::log(&format!("network: {description}"));
            self.failures = 0;
            self.next_check = now;
        }
        if current != self.network {
            let lan = current.as_ref().map(|n| Lan { servers: sockets(&n.dns), domain: n.domain.clone() }).unwrap_or_default();
            *write(&self.shared.lan) = Arc::new(lan);
            if self.local {
                self.next_check = now;
            }
        }
        self.network = current;
    }

    /// Le réglage du DNS du poste peut être défait par un autre service : il est refait.
    pub(super) fn keep_system_dns(&self) {
        if self.system
            && !self.network_dns().is_empty()
            && let Err(error) = platform::keep_system_dns(&self.config)
        {
            logbook::log(&format!("{error:#}"));
        }
    }
}
