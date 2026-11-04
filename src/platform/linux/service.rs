//! Unité systemd, entrée de bureau pour les notifications, journal, installation et désinstallation.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, ensure};

use super::dns::set_system_dns;
use super::succeeds;
use crate::config::Config;

pub(super) const UNIT: &str = "abdnsd.service";

pub(super) const UNIT_PATH: &str = "/etc/systemd/system/abdnsd.service";

/// Sans fichier de ce nom, GNOME affiche « application inconnue » sur les notifications.
pub(super) const DESKTOP_PATH: &str = "/usr/share/applications/abdns.desktop";

pub(super) const DESKTOP_ID: &str = "abdns";

/// `abdns logs` : le journal de systemd.
pub fn logs(follow: bool, lines: u32) -> Command {
    let mut command = Command::new("journalctl");
    command.args(["-u", "abdnsd", "-n", &lines.to_string()]);
    if follow {
        command.arg("-f");
    }
    command
}

/// Argument de `ExecStart` : `%` et `$` doublés, guillemets si l'argument a un espace.
pub(super) fn unit_quote(arg: &str) -> String {
    let arg = arg.replace('%', "%%").replace('$', "$$");
    if arg.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg
    }
}

pub(super) fn unit(program: &Path, config: &Path) -> String {
    let start = [program.to_string_lossy(), "--config".into(), config.to_string_lossy()].map(|a| unit_quote(&a)).join(" ");
    format!(
        "[Unit]\nDescription=abdns: encrypted DNS (DoH) for this computer\nDocumentation=https://github.com/abnd-corp/Abdns\n\
After=network-online.target\nWants=network-online.target\n\n\
[Service]\nExecStart={start}\nRestart=always\nRestartSec=30\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

/// Entrée de bureau sans menu : elle donne son nom aux notifications du service.
pub(super) fn desktop_entry(program: &Path) -> String {
    let exec: String =
        program.to_string_lossy().chars().flat_map(|c| if matches!(c, '\\' | '"' | '`' | '$') { vec!['\\', c] } else { vec![c] }).collect();
    format!(
        "[Desktop Entry]\nType=Application\nName=abdns\nComment=Encrypted DNS (DoH) for this computer\nExec=\"{exec}\"\nIcon=network-server\nNoDisplay=true\nTerminal=false\n"
    )
}

/// Service systemd sans gestionnaire de paquets : abdnsd installé à côté d'abdns.
/// La sortie d'erreur du service va au journal : `abdns logs`.
pub fn install(config: &Path) -> Result<()> {
    let program = std::env::current_exe()?.canonicalize()?.with_file_name("abdnsd");
    ensure!(program.exists(), "{} not found next to abdns", program.display());
    let config = config.canonicalize().with_context(|| format!("{} not found", config.display()))?;
    std::fs::write(UNIT_PATH, unit(&program, &config)).with_context(|| format!("cannot write {UNIT_PATH} (run with sudo)"))?;
    std::fs::write(DESKTOP_PATH, desktop_entry(&program)).with_context(|| format!("cannot write {DESKTOP_PATH} (run with sudo)"))?;
    ensure!(succeeds("systemctl", &["daemon-reload"]), "systemctl daemon-reload failed");
    ensure!(succeeds("systemctl", &["enable", UNIT]), "systemctl enable {UNIT} failed");
    ensure!(succeeds("systemctl", &["restart", UNIT]), "systemctl restart {UNIT} failed");
    Ok(())
}

pub fn uninstall(config: Option<&Config>) -> Result<()> {
    // L'arrêt du service remet déjà le DNS du réseau.
    succeeds("systemctl", &["disable", "--now", UNIT]);
    if Path::new(UNIT_PATH).exists() {
        std::fs::remove_file(UNIT_PATH).with_context(|| format!("cannot remove {UNIT_PATH} (run with sudo)"))?;
        succeeds("systemctl", &["daemon-reload"]);
    }
    if Path::new(DESKTOP_PATH).exists() {
        std::fs::remove_file(DESKTOP_PATH).with_context(|| format!("cannot remove {DESKTOP_PATH} (run with sudo)"))?;
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
    fn desktop_entries() {
        let text = desktop_entry(Path::new("/usr/local/bin/abdns"));
        assert!(text.starts_with("[Desktop Entry]\nType=Application\nName=abdns\n"));
        assert!(text.contains("Exec=\"/usr/local/bin/abdns\"\n"));
        assert!(text.contains("NoDisplay=true\n"));
        assert!(desktop_entry(Path::new("/opt/a\"b$c/abdns")).contains("Exec=\"/opt/a\\\"b\\$c/abdns\"\n"));
    }

    #[test]
    fn systemd_unit() {
        let text = unit(Path::new("/usr/local/bin/abdnsd"), Path::new("/etc/abdns/config.toml"));
        assert!(text.contains("ExecStart=/usr/local/bin/abdnsd --config /etc/abdns/config.toml\n"));
        assert!(text.contains("Restart=always\nRestartSec=30\n"));
        assert!(
            unit(Path::new("/opt/my abdns/abdnsd"), Path::new("/etc/a%b.toml"))
                .contains("ExecStart=\"/opt/my abdns/abdnsd\" --config /etc/a%%b.toml\n")
        );
    }
}
