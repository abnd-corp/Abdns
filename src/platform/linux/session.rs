//! Session graphique de l'utilisateur (le service tourne en root) : notifications et page du portail.

use std::path::Path;
use std::process::{Command, Stdio};

use super::output;
use super::service::DESKTOP_ID;
use crate::logbook;

/// Utilisateur de la session graphique active : (uid, nom). Personne sans écran (ssh, console).
pub(super) fn graphical_session(sessions: &str, show: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    sessions.lines().find_map(|line| {
        // « SESSION UID USER SEAT TTY… » ; un siège « - » est une session à distance.
        let [id, uid, name, seat, ..] = line.split_whitespace().collect::<Vec<_>>()[..] else { return None };
        if !seat.starts_with("seat") {
            return None;
        }
        let details = show(id)?;
        let value = |key: &str| details.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
        (value("Active") == Some("yes") && matches!(value("Type"), Some("x11" | "wayland" | "mir")))
            .then(|| (uid.to_owned(), name.to_owned()))
    })
}

pub(super) fn is_root() -> bool {
    // SAFETY : sans argument ni effet.
    unsafe { libc::geteuid() == 0 }
}

/// Commande qui lance `program` dans la session de l'utilisateur (le service tourne en root) :
/// `runuser` avec le bus et l'écran de sa session.
pub(super) fn session_command(uid: &str, name: &str, wayland: bool, program: &str, args: &[&str]) -> Vec<String> {
    let runtime = format!("/run/user/{uid}");
    let mut command: Vec<String> = ["runuser", "-u", name, "--", "env"].map(str::to_owned).into();
    command.push(format!("XDG_RUNTIME_DIR={runtime}"));
    command.push(format!("DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus"));
    if wayland {
        command.push("WAYLAND_DISPLAY=wayland-0".to_owned());
    }
    command.push("DISPLAY=:0".to_owned());
    command.push(program.to_owned());
    command.extend(args.iter().map(|a| (*a).to_owned()));
    command
}

/// En root : dans la session graphique. Sans être root (essais avec `--no-system`) : tel quel.
pub(super) fn as_session_user(program: &str, args: &[&str]) {
    let command = if is_root() {
        let sessions = output("loginctl", &["list-sessions", "--no-legend"]).unwrap_or_default();
        let show = |id: &str| output("loginctl", &["show-session", id, "-p", "Type", "-p", "Active"]);
        let Some((uid, name)) = graphical_session(&sessions, show) else { return };
        let wayland = Path::new("/run/user").join(&uid).join("wayland-0").exists();
        session_command(&uid, &name, wayland, program, args)
    } else {
        std::iter::once(program).chain(args.iter().copied()).map(str::to_owned).collect()
    };
    let status = Command::new(&command[0]).args(&command[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    if let Err(error) = status {
        logbook::log(&format!("{program}: {error}"));
    }
}

pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
    let hint = format!("--hint=string:desktop-entry:{DESKTOP_ID}");
    as_session_user("notify-send", &["--app-name=abdns", &hint, "--", title, body]);
}

pub fn open(url: &str) {
    as_session_user("xdg-open", &[url]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graphical_sessions() {
        let sessions = "1 1000 alice - - active no -\n3 1001 bob seat0 tty2 active no -\n";
        let show = |id: &str| Some(format!("Type={}\nActive=yes\n", if id == "3" { "wayland" } else { "tty" }));
        assert_eq!(graphical_session(sessions, show), Some(("1001".to_owned(), "bob".to_owned())));
        assert_eq!(graphical_session(sessions, |_| Some("Type=tty\nActive=yes\n".to_owned())), None);
        assert_eq!(graphical_session(sessions, |_| Some("Type=x11\nActive=no\n".to_owned())), None);
        assert_eq!(graphical_session("", show), None);
    }

    #[test]
    fn session_commands() {
        let command = session_command("1000", "rxdy", true, "notify-send", &["--", "t"]);
        assert_eq!(&command[..5], ["runuser", "-u", "rxdy", "--", "env"]);
        assert!(command.contains(&"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus".to_owned()));
        assert!(command.contains(&"WAYLAND_DISPLAY=wayland-0".to_owned()));
        assert_eq!(&command[command.len() - 3..], ["notify-send", "--", "t"]);
        assert!(!session_command("1000", "rxdy", false, "xdg-open", &[]).iter().any(|a| a.starts_with("WAYLAND")));
    }
}
