//! Canal de commande entre `abdns` (l'outil) et `abdnsd` (le service) : socket Unix locale,
//! une requête JSON par ligne, une réponse JSON par ligne. Seul le service écrit dans son
//! dossier d'état ; l'outil n'a jamais besoin d'être administrateur pour le piloter.
//!
//! Droits : la socket appartient au groupe des administrateurs du poste (0660) ; les autres
//! utilisateurs lisent seulement l'état (`status.json`, lisible par tous).

#![cfg_attr(not(unix), allow(unused_imports))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::Config;
use crate::guard::{self, Force};
use crate::relay::Shared;
use crate::{logbook, platform};

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "command", rename_all = "lowercase")]
pub enum Request {
    Status,
    Mode { mode: String },
    Reload,
    Test,
}

/// Demandes du canal à la garde, prises en compte à la seconde suivante.
#[derive(Default)]
pub struct Signals {
    pub reload: AtomicBool,
    pub check: AtomicBool,
}

pub fn socket_path(config: &Config) -> PathBuf {
    config.state_dir.join("control.sock")
}

fn status(config: &Config) -> Result<Value> {
    let path = guard::state_file(config);
    let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn set_mode(config: &Config, mode: &str) -> Result<Value> {
    let Some(force) = Force::parse(mode) else { bail!("unknown mode: {mode} (auto, encrypted, local)") };
    let path = guard::force_file(config);
    if force == Force::Auto {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    } else {
        std::fs::write(&path, mode)?;
    }
    Ok(json!({ "mode": mode }))
}

/// Chaque serveur interrogé directement, avec son temps de réponse.
fn test(shared: &Shared) -> Value {
    let resolver = shared.resolver();
    let results = resolver.test(&guard::probe_name());
    Value::Array(
        results
            .into_iter()
            .map(|(name, result)| match result {
                Ok(took) => json!({ "name": name, "ok": true, "ms": took.as_millis() }),
                Err(error) => json!({ "name": name, "ok": false, "error": error }),
            })
            .collect(),
    )
}

/// Réponse à une requête, quel que soit le canal (socket Unix, canal nommé sous Windows).
pub fn answer(request: Request, config: &Config, shared: &Shared, signals: &Signals) -> Result<Value> {
    match request {
        Request::Status => status(config),
        Request::Mode { mode } => {
            let reply = set_mode(config, &mode)?;
            signals.check.store(true, Ordering::SeqCst);
            Ok(reply)
        }
        Request::Reload => {
            signals.reload.store(true, Ordering::SeqCst);
            Ok(json!({ "reload": "requested" }))
        }
        Request::Test => Ok(test(shared)),
    }
}

#[cfg(unix)]
pub fn serve(config: Arc<Config>, shared: Arc<Shared>, signals: Arc<Signals>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    let path = socket_path(&config);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("cannot listen on {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))?;
    if let Some(gid) = platform::admin_gid()
        && let Err(error) = std::os::unix::fs::chown(&path, None, Some(gid))
    {
        // Service lancé sans être administrateur (essais) : la socket reste à son propriétaire.
        logbook::log(&format!("control socket kept to its owner: {error}"));
    }
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (config, shared, signals) = (config.clone(), shared.clone(), signals.clone());
            thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                let mut line = String::new();
                let Ok(mut writer) = stream.try_clone() else { return };
                // Une ligne d'au plus 64 Kio.
                if BufReader::new(std::io::Read::take(stream, 65_536)).read_line(&mut line).is_err() {
                    return;
                }
                let reply = serde_json::from_str::<Request>(&line)
                    .map_err(anyhow::Error::from)
                    .and_then(|request| answer(request, &config, &shared, &signals));
                let reply = match reply {
                    Ok(value) => json!({ "ok": true, "result": value }),
                    Err(error) => json!({ "ok": false, "error": format!("{error:#}") }),
                };
                writeln!(writer, "{reply}").ok();
            });
        }
    });
    Ok(())
}

#[cfg(not(unix))]
pub fn serve(_config: Arc<Config>, _shared: Arc<Shared>, _signals: Arc<Signals>) -> Result<()> {
    logbook::log("control channel not available on this platform yet");
    Ok(())
}

/// Envoie une requête au service et rend son résultat.
#[cfg(unix)]
pub fn request(socket: &Path, request: &Request) -> Result<Value> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(socket).with_context(|| format!("cannot reach abdnsd at {}", socket.display()))?;
    let timeout = if *request == Request::Test { 30 } else { 10 };
    stream.set_read_timeout(Some(Duration::from_secs(timeout)))?;
    writeln!(stream, "{}", serde_json::to_string(request)?)?;
    let started = Instant::now();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).with_context(|| format!("no answer after {:?}", started.elapsed()))?;
    let reply: Value = serde_json::from_str(&line).context("invalid answer from abdnsd")?;
    if reply["ok"] == true { Ok(reply["result"].clone()) } else { bail!("{}", reply["error"].as_str().unwrap_or("error")) }
}

#[cfg(not(unix))]
pub fn request(_socket: &Path, _request: &Request) -> Result<Value> {
    bail!("control channel not available on this platform yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_on_the_wire() {
        assert_eq!(serde_json::to_string(&Request::Status).unwrap(), r#"{"command":"status"}"#);
        assert_eq!(serde_json::to_string(&Request::Mode { mode: "local".into() }).unwrap(), r#"{"command":"mode","mode":"local"}"#);
        assert_eq!(serde_json::from_str::<Request>(r#"{"command":"reload"}"#).unwrap(), Request::Reload);
        assert!(serde_json::from_str::<Request>(r#"{"command":"rm"}"#).is_err());
    }
}
