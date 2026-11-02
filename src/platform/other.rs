//! Linux et Windows : pas encore pris en charge. Le relais et la garde fonctionnent
//! (essais avec `--no-system`), mais le poste n'est ni réglé ni notifié.

use std::path::Path;

use anyhow::{Result, bail};

use super::Network;
use crate::config::Config;
use crate::logbook;

pub const CONFIG_PATH: &str = "/etc/abdns/config.toml";

pub fn network() -> Option<Network> {
    None
}

pub fn set_system_dns(_config: &Config, _relay: bool) -> Result<()> {
    bail!("setting the system DNS is not supported on this platform yet")
}

pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
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
