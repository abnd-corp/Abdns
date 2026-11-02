//! Session de l'utilisateur connecté : notifications et page du portail.

use std::process::{Command, Stdio};

use super::output;
use crate::logbook;

/// Utilisateur de la session graphique : (uid, nom). Personne à l'écran de connexion.
pub(super) fn console_user() -> Option<(String, String)> {
    let uid = output("/usr/bin/stat", &["-f", "%u", "/dev/console"])?.trim().to_owned();
    let name = output("/usr/bin/stat", &["-f", "%Su", "/dev/console"])?.trim().to_owned();
    (uid != "0").then_some((uid, name))
}

/// Lance un programme dans la session de l'utilisateur (le service tourne en root).
pub(super) fn as_console_user(program: &str, args: &[&str]) {
    let Some((uid, name)) = console_user() else { return };
    let mut command = Command::new("/bin/launchctl");
    command.args(["asuser", &uid, "/usr/bin/sudo", "-u", &name, program]).args(args);
    if let Err(error) = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status() {
        logbook::log(&format!("{program}: {error}"));
    }
}

/// Titre et texte passés en arguments à osascript : aucun échappement à faire.
pub fn notify(title: &str, body: &str) {
    logbook::log(&format!("notification: {title} — {body}"));
    as_console_user(
        "/usr/bin/osascript",
        &["-e", "on run argv", "-e", "display notification (item 2 of argv) with title (item 1 of argv)", "-e", "end run", title, body],
    );
}

pub fn open(url: &str) {
    as_console_user("/usr/bin/open", &[url]);
}
