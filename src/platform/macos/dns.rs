//! DNS du poste : services réseau pointés vers le relais, ou rendus au DHCP.

use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

use super::output;
use crate::config::Config;

/// Le réglage est attaché au service réseau (« Wi-Fi »), pas au réseau rejoint : rien à refaire.
pub fn keep_system_dns(_config: &Config) -> Result<()> {
    Ok(())
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
