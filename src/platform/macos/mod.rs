//! macOS : outils du système uniquement (route, ipconfig, networksetup, osascript, launchctl), et
//! notify(3) de libSystem pour être prévenu des changements de réseau.

mod dns;
mod network;
mod service;
mod session;

use std::process::{Command, Stdio};

pub use dns::{keep_system_dns, set_system_dns};
pub use network::{Watch, network};
pub use service::{install, logs, uninstall};
pub use session::{notify, open};

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Groupe des administrateurs du Mac (`admin`), qui pilotent abdnsd sans sudo.
pub fn admin_gid() -> Option<u32> {
    Some(80)
}
