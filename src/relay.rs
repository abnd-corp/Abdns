//! Relais DNS local (UDP et TCP) : chaque requête va à dnsproxy (chiffré) ou, en dernier
//! recours, au DNS du réseau. Le choix est fait par la garde, jamais requête par requête :
//! un SERVFAIL (signature DNSSEC cassée, par exemple) ne fait pas basculer en clair.
//! Les noms du réseau local (`nas.lan`, inverse de 192.168.x.x…) vont toujours au DNS du
//! réseau, sans être signalés.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::dns;

/// Requêtes ou connexions traitées en même temps ; au-delà, les nouvelles sont ignorées
/// (le client réessaie).
const MAX_ACTIVE: usize = 256;
/// En attente d'affichage : au-delà, les noms ne sont plus listés (le journal les garde).
const MAX_PENDING: usize = 1000;
const PROXY_TIMEOUT: Duration = Duration::from_secs(6);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(2);

/// Le réseau vu par le relais, tenu à jour par la garde.
#[derive(Default, Clone)]
pub struct Lan {
    pub servers: Vec<SocketAddr>,
    pub domain: Option<String>,
}

pub struct Shared {
    pub proxy: SocketAddr,
    /// Vrai : DNS du réseau pour tout (dernier recours).
    pub local_mode: RwLock<bool>,
    pub lan: RwLock<Lan>,
    /// Domaines demandés en clair depuis le dernier relevé.
    pub clear: Mutex<Vec<String>>,
    active: AtomicUsize,
}

struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Shared {
    pub fn new(proxy: SocketAddr) -> Arc<Self> {
        Arc::new(Self {
            proxy,
            local_mode: RwLock::new(false),
            lan: RwLock::new(Lan::default()),
            clear: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
        })
    }

    fn slot(&self) -> Option<Slot<'_>> {
        if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_ACTIVE {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(&self.active))
    }

    fn targets(&self, query: &[u8]) -> (Vec<SocketAddr>, Duration) {
        let lan = self.lan.read().map(|l| l.clone()).unwrap_or_default();
        let local_mode = self.local_mode.read().map(|m| *m).unwrap_or(false);
        let name = dns::question(query);
        let local_name = name.as_deref().is_some_and(|n| dns::is_local(n, lan.domain.as_deref()));
        if !lan.servers.is_empty() && (local_name || local_mode) {
            if !local_name
                && let (Some(name), Ok(mut clear)) = (name, self.clear.lock())
                && clear.len() < MAX_PENDING
            {
                clear.push(name);
            }
            return (lan.servers, NETWORK_TIMEOUT);
        }
        (vec![self.proxy], PROXY_TIMEOUT)
    }

    fn resolve(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        let (targets, timeout) = self.targets(query);
        targets.into_iter().find_map(|server| exchange(server, query, tcp, timeout).ok())
    }
}

pub fn exchange(server: SocketAddr, query: &[u8], tcp: bool, timeout: Duration) -> Result<Vec<u8>> {
    if tcp { exchange_tcp(server, query, timeout) } else { exchange_udp(server, query, timeout) }
}

fn exchange_udp(server: SocketAddr, query: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let local: SocketAddr = if server.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
    let socket = UdpSocket::bind(local)?;
    socket.connect(server)?;
    socket.set_read_timeout(Some(timeout))?;
    socket.send(query)?;
    let mut buf = vec![0; 65_535];
    loop {
        let n = socket.recv(&mut buf).with_context(|| format!("{server} did not answer"))?;
        // Réponse d'une autre requête : ignorée (protection contre les réponses forgées).
        if n >= 12 && dns::id(&buf[..n]) == dns::id(query) {
            buf.truncate(n);
            return Ok(buf);
        }
    }
}

fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut len = [0; 2];
    stream.read_exact(&mut len)?;
    let mut msg = vec![0; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut msg)?;
    Ok(msg)
}

fn write_frame(stream: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
    let len = u16::try_from(msg.len()).map_err(|_| std::io::Error::other("message too long"))?;
    let mut frame = Vec::with_capacity(msg.len() + 2);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(msg);
    stream.write_all(&frame)
}

fn exchange_tcp(server: SocketAddr, query: &[u8], timeout: Duration) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&server, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write_frame(&mut stream, query)?;
    let reply = read_frame(&mut stream)?;
    anyhow::ensure!(dns::id(&reply) == dns::id(query), "{server}: unexpected id");
    Ok(reply)
}

fn serve_udp(socket: UdpSocket, shared: Arc<Shared>) {
    let socket = Arc::new(socket);
    let mut buf = vec![0; 65_535];
    loop {
        let Ok((n, client)) = socket.recv_from(&mut buf) else { continue };
        if n < 12 {
            continue;
        }
        let query = buf[..n].to_vec();
        let (socket, shared) = (socket.clone(), shared.clone());
        thread::spawn(move || {
            let Some(_slot) = shared.slot() else { return };
            if let Some(reply) = shared.resolve(&query, false).or_else(|| dns::servfail(&query)) {
                socket.send_to(&reply, client).ok();
            }
        });
    }
}

fn serve_tcp(listener: TcpListener, shared: Arc<Shared>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let shared = shared.clone();
        thread::spawn(move || {
            let Some(_slot) = shared.slot() else { return };
            stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
            while let Ok(query) = read_frame(&mut stream) {
                let Some(reply) = shared.resolve(&query, true).or_else(|| dns::servfail(&query)) else { return };
                if write_frame(&mut stream, &reply).is_err() {
                    return;
                }
            }
        });
    }
}

/// Ouvre toutes les adresses d'écoute avant de rendre la main : une adresse prise fait
/// échouer le démarrage (la garde remet alors le DNS du réseau).
pub fn start(addresses: &[SocketAddr], shared: &Arc<Shared>) -> Result<()> {
    let mut sockets = Vec::new();
    for &addr in addresses {
        let udp = UdpSocket::bind(addr).with_context(|| format!("cannot listen on UDP {addr}"))?;
        let tcp = TcpListener::bind(addr).with_context(|| format!("cannot listen on TCP {addr}"))?;
        sockets.push((udp, tcp));
    }
    for (udp, tcp) in sockets {
        let (a, b) = (shared.clone(), shared.clone());
        thread::spawn(move || serve_udp(udp, a));
        thread::spawn(move || serve_tcp(tcp, b));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared(local_mode: bool) -> Arc<Shared> {
        let shared = Shared::new(SocketAddr::from(([127, 0, 0, 1], 5300)));
        *shared.local_mode.write().unwrap() = local_mode;
        *shared.lan.write().unwrap() = Lan { servers: vec![SocketAddr::from(([192, 168, 1, 1], 53))], domain: Some("home".into()) };
        shared
    }

    #[test]
    fn routing() {
        let lan = SocketAddr::from(([192, 168, 1, 1], 53));
        let encrypted = shared(false);
        assert_eq!(encrypted.targets(&dns::query(1, "example.com", 1)).0, vec![encrypted.proxy]);
        assert_eq!(encrypted.targets(&dns::query(1, "12.1.168.192.in-addr.arpa", 12)).0, vec![lan]);
        assert_eq!(encrypted.targets(&dns::query(1, "nas.home", 1)).0, vec![lan]);
        assert!(encrypted.clear.lock().unwrap().is_empty());

        let local = shared(true);
        assert_eq!(local.targets(&dns::query(1, "example.com", 1)).0, vec![lan]);
        assert_eq!(local.targets(&dns::query(1, "printer.local", 1)).0, vec![lan]);
        assert_eq!(*local.clear.lock().unwrap(), vec!["example.com".to_owned()]);
    }

    #[test]
    fn no_network_dns_stays_encrypted() {
        let shared = Shared::new(SocketAddr::from(([127, 0, 0, 1], 5300)));
        assert_eq!(shared.targets(&dns::query(1, "nas.lan", 1)).0, vec![shared.proxy]);
    }
}
