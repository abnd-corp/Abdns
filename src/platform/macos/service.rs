//! Service launchd, rotation du journal (newsyslog), installation et désinstallation.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, ensure};

use super::set_system_dns;
use crate::config::Config;

pub(super) const LABEL: &str = "com.abnd.abdns";

pub(super) const PLIST: &str = "/Library/LaunchDaemons/com.abnd.abdns.plist";

pub(super) const NEWSYSLOG: &str = "/etc/newsyslog.d/abdns.conf";

pub(super) const LOG_DIR: &str = "/var/log/abdns";

/// `abdns logs` : le journal écrit par launchd (installation manuelle, Homebrew ou Nix).
pub fn logs(follow: bool, lines: u32) -> Command {
    let mut command = Command::new("/usr/bin/tail");
    command.arg("-n").arg(lines.to_string());
    if follow {
        command.arg("-F");
    }
    command.arg(format!("{LOG_DIR}/abdns.log"));
    command
}

pub(super) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(super) fn plist(program: &Path, config: &Path) -> String {
    let arguments = [program.to_string_lossy().into_owned(), "--config".into(), config.to_string_lossy().into_owned()]
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\t<key>Label</key>\n\t<string>{LABEL}</string>\n\t<key>ProgramArguments</key>\n\t<array>\n{arguments}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>KeepAlive</key>\n\t<true/>\n\t<key>ThrottleInterval</key>\n\t<integer>30</integer>\n\
\t<key>StandardOutPath</key>\n\t<string>{LOG_DIR}/abdns.log</string>\n\t<key>StandardErrorPath</key>\n\t<string>{LOG_DIR}/abdns.log</string>\n</dict>\n</plist>\n"
    )
}

pub(super) fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).stdin(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// Service launchd sans gestionnaire de paquets : abdnsd installé à côté d'abdns.
pub fn install(config: &Path) -> Result<()> {
    let program = std::env::current_exe()?.canonicalize()?.with_file_name("abdnsd");
    ensure!(program.exists(), "{} not found next to abdns", program.display());
    let config = config.canonicalize().with_context(|| format!("{} not found", config.display()))?;
    std::fs::create_dir_all(LOG_DIR)?;
    std::fs::write(PLIST, plist(&program, &config)).with_context(|| format!("cannot write {PLIST} (run with sudo)"))?;
    // Rotation à 1 Mio, 3 générations compressées.
    std::fs::write(NEWSYSLOG, format!("{LOG_DIR}/abdns.log 644 3 1024 * Z\n"))?;
    launchctl(&["bootout", &format!("system/{LABEL}")]);
    ensure!(launchctl(&["bootstrap", "system", PLIST]), "launchctl bootstrap {PLIST} failed");
    Ok(())
}

pub fn uninstall(config: Option<&Config>) -> Result<()> {
    launchctl(&["bootout", &format!("system/{LABEL}")]);
    for path in [PLIST, NEWSYSLOG] {
        if Path::new(path).exists() {
            std::fs::remove_file(path).with_context(|| format!("cannot remove {path} (run with sudo)"))?;
        }
    }
    if let Some(config) = config {
        set_system_dns(config, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launchd_plist() {
        let p = plist(Path::new("/usr/local/bin/abdnsd"), Path::new("/etc/abdns/config.toml"));
        assert!(p.contains("<string>com.abnd.abdns</string>"));
        assert!(p.contains(
            "<string>/usr/local/bin/abdnsd</string>\n\t\t<string>--config</string>\n\t\t<string>/etc/abdns/config.toml</string>\n\t</array>"
        ));
    }
}
