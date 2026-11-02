//! Linux et Windows : pas encore pris en charge. Le relais et la garde fonctionnent
//! (essais avec `--no-system`), mais le poste n'est ni réglé ni notifié.

use std::path::Path;

use anyhow::{Result, bail};

use super::Network;
use crate::config::Config;
use crate::logbook;

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
    None
}

pub fn set_system_dns(_config: &Config, _relay: bool) -> Result<()> {
    bail!("setting the system DNS is not supported on this platform yet")
}

pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
}

/// `abdns logs` : journal de systemd.
pub fn logs(follow: bool, lines: u32) -> std::process::Command {
    let mut command = std::process::Command::new("journalctl");
    command.args(["-u", "abdnsd", "-n", &lines.to_string()]);
    if follow {
        command.arg("-f");
    }
    command
}

pub fn open(url: &str) {
    logbook::log(&format!("to open: {url}"));
}

pub fn install(_config: &Path) -> Result<()> {
    bail!("install is not supported on this platform yet")
}

pub fn uninstall(_config: Option<&Config>) -> Result<()> {
    bail!("uninstall is not supported on this platform yet")
}
