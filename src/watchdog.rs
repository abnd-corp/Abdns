//! Garde-fou du service : un second processus (le même programme, `--after-exit`), relié par un
//! tube que le noyau ferme quoi qu'il arrive au service. Le service y écrit un battement toutes
//! les 5 s, seulement si la garde et le fil du relais répondent ; avant un arrêt normal, il écrit
//! « arrêté ».
//!
//! - Tube fermé sans ce mot (tué net, plantage) : le DNS du réseau est rendu là où le système ne
//!   le fait pas seul (macOS), et l'arrêt est notifié.
//! - Plus de battement depuis 30 s (service vivant mais bloqué) : de même, puis le service est
//!   tué pour que launchd ou systemd le relance.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

use anyhow::Result;

use crate::config::Config;
use crate::i18n::Text;
use crate::{logbook, platform, rt};

const BEAT_EVERY: u64 = 5;
/// Plus long que le plus long tour de la garde (sondes et portail, une quinzaine de secondes).
const SILENCE: Duration = Duration::from_secs(30);
const BEAT: u8 = b'.';
const STOPPED: u8 = b'S';

pub struct Watchdog {
    pipe: ChildStdin,
    next: u64,
}

impl Watchdog {
    /// Lance le garde-fou, dans son propre groupe de processus pour que launchd ne l'arrête pas
    /// avec le service.
    pub fn spawn(config_path: &Path) -> Option<Self> {
        let spawned = std::env::current_exe().and_then(|program| {
            let mut command = Command::new(program);
            command.arg("--config").arg(config_path).arg("--after-exit").stdin(Stdio::piped());
            #[cfg(unix)]
            std::os::unix::process::CommandExt::process_group(&mut command, 0);
            command.spawn()
        });
        match spawned {
            Ok(mut child) => child.stdin.take().map(|pipe| Self { pipe, next: 0 }),
            Err(error) => {
                logbook::log(&format!("watchdog unavailable: {error}"));
                None
            }
        }
    }

    /// Appelé par la garde à chaque tour. Une tâche attendue sur le runtime prouve que le fil du
    /// relais tourne : bloqué, il bloque la garde, et le battement cesse. Erreur : garde-fou parti.
    pub fn beat(&mut self, now: u64) -> std::io::Result<()> {
        if now < self.next {
            return Ok(());
        }
        self.next = now + BEAT_EVERY;
        if rt::block_on(rt::handle().spawn(async {})).is_err() {
            return Ok(());
        }
        self.pipe.write_all(&[BEAT])
    }

    pub fn stopped(mut self) {
        self.pipe.write_all(&[STOPPED]).ok();
    }
}

/// `abdnsd --after-exit` : écoute le tube jusqu'à l'arrêt normal, la fermeture ou le silence.
pub fn run(config_path: &Path) -> Result<()> {
    let (heard, listen) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 64];
        let mut stdin = std::io::stdin();
        // Tube fermé ou illisible : le fil s'arrête, `recv_timeout` le voit (Disconnected).
        while let Ok(n @ 1..) = stdin.read(&mut buf) {
            if heard.send(buf[..n].contains(&STOPPED)).is_err() {
                return;
            }
        }
    });
    loop {
        match listen.recv_timeout(SILENCE) {
            Ok(false) => {}
            Ok(true) => return Ok(()),
            Err(RecvTimeoutError::Disconnected) => return ended(config_path, false),
            Err(RecvTimeoutError::Timeout) => return ended(config_path, true),
        }
    }
}

fn ended(config_path: &Path, hung: bool) -> Result<()> {
    let config = Config::read(config_path);
    if platform::RESTORE_AFTER_EXIT
        && let Ok(config) = &config
        && let Err(error) = platform::set_system_dns(config, false)
    {
        logbook::log(&format!("{error:#}"));
    }
    if hung {
        logbook::log("abdnsd stopped responding: network DNS restored, restarting it");
        kill_service();
    } else {
        logbook::log("abdnsd ended abruptly: network DNS restored until it restarts");
    }
    let text = Text(config.map(|c| c.language).unwrap_or_default());
    let title = if hung { text.hung_title() } else { text.crashed_title() };
    platform::notify(&title, &text.crashed_body());
    Ok(())
}

/// Le service est le parent du garde-fou ; `kill` est livré avec tout Unix.
fn kill_service() {
    #[cfg(unix)]
    {
        let service = std::os::unix::process::parent_id();
        // Parent déjà mort : le garde-fou a été rattaché au processus 1, qu'il ne faut pas tuer.
        if service > 1 {
            Command::new("/bin/kill").args(["-KILL", &service.to_string()]).status().ok();
        }
    }
}
