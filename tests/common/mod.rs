//! Aides des tests d'intégration : dossier temporaire, configuration, lancement du programme.

#![allow(dead_code)]

use std::io::Write;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// L'outil de l'utilisateur et le service.
pub const TOOL: &str = env!("CARGO_BIN_EXE_abdns");
pub const SERVICE: &str = env!("CARGO_BIN_EXE_abdnsd");

/// Dossier temporaire supprimé à la fin du test.
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("abdns-test-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&path).ok();
        std::fs::create_dir_all(&path).expect("temporary directory");
        Self(path)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Port UDP libre sur la boucle locale (au-dessus de 1024 : pas besoin d'être root).
pub fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0").and_then(|s| s.local_addr()).expect("free port").port()
}

/// Configuration de test : un seul serveur DoH qui ne répond jamais (port fermé), aucun repli en clair.
pub fn write_config(dir: &Dir, port: u16, server_name: &str) -> PathBuf {
    let path = dir.path("config.toml");
    let text = format!(
        "listen = [\"127.0.0.1:{port}\"]\nlocal_fallback = false\nstate_dir = \"{state}\"\nclear_log = \"{log}\"\n\n\
[[server]]\nname = \"{server_name}\"\nurl = \"https://127.0.0.1:9/dns-query\"\n",
        state = dir.path("state").display(),
        log = dir.path("log/clear.log").display(),
    );
    std::fs::write(&path, text).expect("config");
    path
}

/// Lance `abdns --config CONFIG ARGS…` jusqu'à sa fin ; `stdin` est envoyé tel quel.
pub fn run(config: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(TOOL)
        .arg("--config")
        .arg(config)
        .args(args)
        .env("PATH", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("abdns starts");
    child.stdin.take().expect("stdin").write_all(stdin.as_bytes()).ok();
    child.wait_with_output().expect("abdns ends")
}

/// Rappelle `check` jusqu'à ce qu'elle rende une valeur, au plus `secs` secondes.
pub fn wait_for<T>(secs: u64, mut check: impl FnMut() -> Option<T>) -> Option<T> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(value) = check() {
            return Some(value);
        }
        if Instant::now() >= end {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Requête DNS (A, récursion demandée) pour `name`.
pub fn query(id: u16, name: &str) -> Vec<u8> {
    let mut msg = id.to_be_bytes().to_vec();
    msg.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.extend_from_slice(&[0, 0, 1, 0, 1]);
    msg
}

pub fn reply_id(reply: &[u8]) -> u16 {
    u16::from_be_bytes([reply[0], reply[1]])
}

pub fn is_response(reply: &[u8]) -> bool {
    reply[2] & 0x80 != 0
}

pub fn rcode(reply: &[u8]) -> u8 {
    reply[3] & 0x0f
}
