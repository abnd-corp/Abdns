//! Relais DNS local (UDP et TCP) : chaque requête va au client DoH (chiffré) ou, en dernier
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
use crate::doh::Resolver;

/// Requêtes ou connexions traitées en même temps ; au-delà, les nouvelles sont ignorées
/// (le client réessaie).
const MAX_ACTIVE: usize = 256;
/// En attente d'affichage : au-delà, les noms ne sont plus listés (le journal les garde).
const MAX_PENDING: usize = 1000;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(2);

/// Le réseau vu par le relais, tenu à jour par la garde.
#[derive(Default, Clone)]
pub struct Lan {
    pub servers: Vec<SocketAddr>,
    pub domain: Option<String>,
}

pub struct Shared {
    /// Remplacé à chaud quand la configuration change.
    pub resolver: RwLock<Arc<Resolver>>,
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

enum Route {
    Encrypted,
    Network(Vec<SocketAddr>),
}

impl Shared {
    pub fn new(resolver: Resolver) -> Arc<Self> {
        Arc::new(Self {
            resolver: RwLock::new(Arc::new(resolver)),
            local_mode: RwLock::new(false),
            lan: RwLock::new(Lan::default()),
            clear: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
        })
    }

    pub fn resolver(&self) -> Arc<Resolver> {
        self.resolver.read().map(|r| r.clone()).expect("resolver lock")
    }

    fn slot(&self) -> Option<Slot<'_>> {
        if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_ACTIVE {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(&self.active))
    }

    fn route(&self, query: &[u8]) -> Route {
        let lan = self.lan.read().map(|l| l.clone()).unwrap_or_default();
        let local_mode = self.local_mode.read().map(|m| *m).unwrap_or(false);
        let name = dns::question(query);
        let local_name = name.as_deref().is_some_and(|n| dns::is_local(n, lan.domain.as_deref()));
        if lan.servers.is_empty() || !(local_name || local_mode) {
            return Route::Encrypted;
        }
        if !local_name
            && let (Some(name), Ok(mut clear)) = (name, self.clear.lock())
            && clear.len() < MAX_PENDING
        {
            clear.push(name);
        }
        Route::Network(lan.servers)
    }

    fn resolve(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        match self.route(query) {
            Route::Encrypted => self.resolver().resolve(query),
            Route::Network(servers) => servers.into_iter().find_map(|s| exchange(s, query, tcp, NETWORK_TIMEOUT).ok()),
        }
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
            let reply = shared.resolve(&query, false).or_else(|| dns::servfail(&query));
            // Réponse DoH plus grande que ce que le client accepte en UDP : il repassera en TCP.
            let reply = reply.and_then(|r| if r.len() > dns::udp_limit(&query) { dns::truncated(&r) } else { Some(r) });
            if let Some(reply) = reply {
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
/// échouer le démarrage (le DNS du réseau est alors remis).
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
    use crate::config::Config;

    fn shared(local_mode: bool, lan: bool) -> Arc<Shared> {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        let shared = Shared::new(Resolver::new(&config).unwrap());
        *shared.local_mode.write().unwrap() = local_mode;
        if lan {
            *shared.lan.write().unwrap() = Lan { servers: vec![SocketAddr::from(([192, 168, 1, 1], 53))], domain: Some("home".into()) };
        }
        shared
    }

    fn network(route: Route) -> Option<Vec<SocketAddr>> {
        match route {
            Route::Encrypted => None,
            Route::Network(servers) => Some(servers),
        }
    }

    #[test]
    fn routing() {
        let lan = Some(vec![SocketAddr::from(([192, 168, 1, 1], 53))]);
        let encrypted = shared(false, true);
        assert_eq!(network(encrypted.route(&dns::query(1, "example.com", 1))), None);
        assert_eq!(network(encrypted.route(&dns::query(1, "12.1.168.192.in-addr.arpa", 12))), lan);
        assert_eq!(network(encrypted.route(&dns::query(1, "nas.home", 1))), lan);
        assert!(encrypted.clear.lock().unwrap().is_empty());

        let local = shared(true, true);
        assert_eq!(network(local.route(&dns::query(1, "example.com", 1))), lan);
        assert_eq!(network(local.route(&dns::query(1, "printer.local", 1))), lan);
        assert_eq!(*local.clear.lock().unwrap(), vec!["example.com".to_owned()]);
    }

    #[test]
    fn no_network_dns_stays_encrypted() {
        assert_eq!(network(shared(true, false).route(&dns::query(1, "nas.lan", 1))), None);
    }

    /// Faux DNS du réseau : répond à chaque requête UDP en la renvoyant avec le bit « réponse ».
    /// Si `forged_first`, envoie d'abord une réponse d'un autre identifiant (NXDOMAIN).
    fn fake_network_dns(forged_first: bool) -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap();
        thread::spawn(move || {
            let mut buf = [0; 512];
            while let Ok((n, client)) = socket.recv_from(&mut buf) {
                let mut reply = buf[..n].to_vec();
                reply[2] |= 0x80;
                if forged_first {
                    let mut forged = reply.clone();
                    forged[1] ^= 0xff;
                    forged[3] = (forged[3] & 0xf0) | dns::NXDOMAIN;
                    socket.send_to(&forged, client).ok();
                }
                socket.send_to(&reply, client).ok();
            }
        });
        addr
    }

    fn free_udp_port() -> u16 {
        UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    #[test]
    fn forged_answers_are_ignored() {
        let server = fake_network_dns(true);
        let query = dns::query(0x1234, "example.com", 1);
        let reply = exchange(server, &query, false, Duration::from_secs(2)).unwrap();
        assert_eq!(dns::id(&reply), Some(0x1234));
        assert_eq!(dns::rcode(&reply), Some(0), "the forged NXDOMAIN must not be taken");
    }

    #[test]
    fn silent_server_times_out() {
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let query = dns::query(1, "example.com", 1);
        assert!(exchange(silent.local_addr().unwrap(), &query, false, Duration::from_millis(200)).is_err());
    }

    #[test]
    fn tcp_framing_and_identifier_check() {
        for (answer_id_xor, ok) in [(0, true), (1, false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut msg = read_frame(&mut stream).unwrap();
                msg[0] ^= answer_id_xor;
                write_frame(&mut stream, &msg).unwrap();
            });
            let query = dns::query(0x0a0b, "example.com", 1);
            assert_eq!(exchange(addr, &query, true, Duration::from_secs(2)).is_ok(), ok);
        }
    }

    #[test]
    fn local_mode_relays_to_the_network_dns_and_reports_only_foreign_names() {
        let shared = shared(true, false);
        *shared.lan.write().unwrap() = Lan { servers: vec![fake_network_dns(false)], domain: Some("home".into()) };
        let listen = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
        start(&[listen], &shared).unwrap();

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let ask = |id: u16, name: &str| {
            client.send_to(&dns::query(id, name, 1), listen).unwrap();
            let mut buf = [0; 512];
            let n = client.recv(&mut buf).expect("the relay answers");
            assert_eq!(dns::id(&buf[..n]), Some(id));
            assert!(buf[2] & 0x80 != 0, "a response");
        };
        ask(1, "example.com");
        ask(2, "nas.home");
        ask(3, "printer.local");
        assert_eq!(*shared.clear.lock().unwrap(), vec!["example.com".to_owned()]);
    }

    #[test]
    fn encrypted_failure_answers_servfail_without_leaking() {
        // Mode chiffré, serveur DoH injoignable : SERVFAIL au client, rien en clair.
        let dead = Config::parse("[[server]]\nname = \"dead\"\nurl = \"https://127.0.0.1:9/dns-query\"\n").unwrap();
        let shared = Shared::new(Resolver::new(&dead).unwrap());
        *shared.lan.write().unwrap() = Lan { servers: vec![SocketAddr::from(([192, 0, 2, 1], 53))], domain: None };
        let listen = SocketAddr::from(([127, 0, 0, 1], free_udp_port()));
        start(&[listen], &shared).unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        client.send_to(&dns::query(9, "example.com", 1), listen).unwrap();
        let mut buf = [0; 512];
        let n = client.recv(&mut buf).expect("the relay answers");
        assert_eq!((dns::id(&buf[..n]), dns::rcode(&buf[..n])), (Some(9), Some(2)));
        assert!(shared.clear.lock().unwrap().is_empty());
    }
}
