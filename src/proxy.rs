//! dnsproxy : le DNS chiffré du poste (serveur principal, puis secours), lancé et surveillé
//! par abdns. Ses serveurs sont passés par fichiers (0600) : le jeton n'apparaît ni dans la
//! liste des processus, ni dans les journaux.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::IpAddr;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::logbook;

const UPSTREAM_FILE: &str = "upstream.txt";
const FALLBACK_FILE: &str = "fallback.txt";

/// Tampon DNS (DNS Stamps) d'un serveur DoH dont l'adresse IP est fixée : dnsproxy s'y
/// connecte directement, sans résoudre le nom en clair.
pub fn stamp(url: &str, ip: IpAddr) -> Result<String> {
    let Some(rest) = url.strip_prefix("https://") else { bail!("{url}: https:// expected") };
    let (host, path) = rest.split_once('/').map_or((rest, "/"), |(h, p)| (h, p));
    let path = format!("/{path}");
    let addr = match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let mut raw = vec![0x02];
    raw.extend_from_slice(&0u64.to_le_bytes());
    for (i, part) in [addr.as_str(), "", host, &path].into_iter().enumerate() {
        if i == 1 {
            raw.push(0); // aucune empreinte de certificat imposée
            continue;
        }
        raw.push(u8::try_from(part.len()).context("stamp field too long")?);
        raw.extend_from_slice(part.as_bytes());
    }
    Ok(format!("sdns://{}", base64url(&raw)))
}

fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn write_private(path: &Path, content: &str) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(content.as_bytes())?;
    Ok(())
}

/// Fichiers de serveurs de dnsproxy, réécrits à chaque démarrage.
pub fn prepare(config: &Config) -> Result<()> {
    let url = config.upstream_url()?;
    let upstream = match config.upstream.ip {
        Some(ip) if !config.upstream_is_ip() => stamp(&url, ip)?,
        _ => url,
    };
    write_private(&config.state_dir.join(UPSTREAM_FILE), &format!("{upstream}\n"))?;
    let mut fallback = String::new();
    for server in &config.fallback {
        fallback.push_str(&stamp(&server.url, server.ip)?);
        fallback.push('\n');
    }
    write_private(&config.state_dir.join(FALLBACK_FILE), &fallback)
}

fn spawn(config: &Config) -> Result<Child> {
    let mut command = Command::new(&config.dnsproxy);
    command
        .args(["--listen", "127.0.0.1", "--port", &config.dnsproxy_port.to_string()])
        .arg("--upstream")
        .arg(config.state_dir.join(UPSTREAM_FILE));
    if !config.fallback.is_empty() {
        command.arg("--fallback").arg(config.state_dir.join(FALLBACK_FILE));
    }
    command
        // Aucune adresse à résoudre en clair : un amorçage injoignable le garantit.
        .args(["--bootstrap", "127.0.0.1:9"])
        .args(["--timeout", "2s", "--cache", "--cache-optimistic", "--cache-size", "4194304"])
        .arg("--pending-requests-enabled")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("cannot start {}", config.dnsproxy.display()))
}

/// Recopie la sortie de dnsproxy dans le journal, jeton masqué.
fn forward_output(stream: impl std::io::Read + Send + 'static, secret: Option<String>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let line = match &secret {
                Some(secret) => line.replace(secret, "<token>"),
                None => line,
            };
            logbook::log(&format!("dnsproxy: {line}"));
        }
    });
}

/// Lance dnsproxy et le relance s'il s'arrête, jusqu'à `stop`.
pub fn supervise(config: Arc<Config>, stop: Arc<AtomicBool>) -> Result<thread::JoinHandle<()>> {
    let secret = config.token()?;
    let mut child = spawn(&config)?;
    Ok(thread::spawn(move || {
        loop {
            if let Some(out) = child.stdout.take() {
                forward_output(out, secret.clone());
            }
            if let Some(err) = child.stderr.take() {
                forward_output(err, secret.clone());
            }
            while !stop.load(Ordering::SeqCst) {
                match child.try_wait() {
                    Ok(None) => thread::sleep(Duration::from_millis(500)),
                    Ok(Some(status)) => {
                        logbook::log(&format!("dnsproxy exited ({status}), restarting"));
                        break;
                    }
                    Err(error) => {
                        logbook::log(&format!("dnsproxy: {error}"));
                        break;
                    }
                }
            }
            if stop.load(Ordering::SeqCst) {
                child.kill().ok();
                child.wait().ok();
                return;
            }
            thread::sleep(Duration::from_secs(2));
            match spawn(&config) {
                Ok(new) => child = new,
                Err(error) => logbook::log(&format!("{error:#}")),
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(stamp: &str) -> Vec<u8> {
        let body = stamp.strip_prefix("sdns://").unwrap();
        let value =
            |c: u8| b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".iter().position(|&a| a == c).unwrap() as u32;
        let mut out = Vec::new();
        for chunk in body.as_bytes().chunks(4) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &c)| n | value(c) << (18 - 6 * i));
            for i in 0..chunk.len() - 1 {
                out.push((n >> (16 - 8 * i)) as u8);
            }
        }
        out
    }

    #[test]
    fn base64() {
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn doh_stamp() {
        let raw = decode(&stamp("https://dns.mullvad.net/dns-query", "194.242.2.2".parse().unwrap()).unwrap());
        let mut expected = vec![2, 0, 0, 0, 0, 0, 0, 0, 0, 11];
        expected.extend_from_slice(b"194.242.2.2");
        expected.push(0);
        expected.push(15);
        expected.extend_from_slice(b"dns.mullvad.net");
        expected.push(10);
        expected.extend_from_slice(b"/dns-query");
        assert_eq!(raw, expected);
        let v6 = decode(&stamp("https://x/q", "2a07:e340::2".parse().unwrap()).unwrap());
        assert_eq!(&v6[9..24], b"\x0e[2a07:e340::2]");
    }
}
