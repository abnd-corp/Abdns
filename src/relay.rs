//! Relais DNS local (UDP et TCP) : chaque requête va au client DoH (chiffré) ou, en dernier
//! recours, au DNS du réseau. Le choix est fait par la garde, jamais requête par requête :
//! un SERVFAIL (signature DNSSEC cassée, par exemple) ne fait pas basculer en clair.
//! Les noms du réseau local (`nas.lan`, inverse de 192.168.x.x…) vont toujours au DNS du
//! réseau, sans être signalés.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, ensure};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::timeout;

use crate::resolver::Resolver;
use crate::{dns, lock, read, rt};

/// Requêtes ou connexions traitées en même temps ; au-delà, les nouvelles sont ignorées
/// (le client réessaie).
const MAX_ACTIVE: usize = 256;
/// En attente d'affichage : au-delà, les noms ne sont plus listés (le journal les garde).
const MAX_PENDING: usize = 1000;
const NETWORK_TIMEOUT: Duration = Duration::from_secs(2);
/// Connexion TCP d'un client sans nouvelle requête : fermée.
const TCP_IDLE: Duration = Duration::from_secs(10);

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
    pub lan: RwLock<Arc<Lan>>,
    /// Domaines demandés en clair depuis le dernier relevé.
    pub clear: Mutex<Vec<String>>,
    active: AtomicUsize,
}

/// Place prise parmi les `MAX_ACTIVE`, rendue à la fin de la tâche.
struct Slot(Arc<Shared>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

enum Route {
    Encrypted,
    Network(Arc<Lan>),
}

impl Shared {
    pub fn new(resolver: Resolver) -> Arc<Self> {
        Arc::new(Self {
            resolver: RwLock::new(Arc::new(resolver)),
            local_mode: RwLock::new(false),
            lan: RwLock::new(Arc::default()),
            clear: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
        })
    }

    pub fn resolver(&self) -> Arc<Resolver> {
        read(&self.resolver).clone()
    }

    fn slot(self: &Arc<Self>) -> Option<Slot> {
        if self.active.fetch_add(1, Ordering::SeqCst) >= MAX_ACTIVE {
            self.active.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(self.clone()))
    }

    fn route(&self, query: &[u8]) -> Route {
        let lan = read(&self.lan).clone();
        let local_mode = *read(&self.local_mode);
        let name = dns::question(query);
        let local_name = name.as_deref().is_some_and(|n| dns::is_local(n, lan.domain.as_deref()));
        if lan.servers.is_empty() || !(local_name || local_mode) {
            return Route::Encrypted;
        }
        if !local_name && let Some(name) = name {
            let mut clear = lock(&self.clear);
            if clear.len() < MAX_PENDING {
                clear.push(name);
            }
        }
        Route::Network(lan)
    }

    async fn answer(&self, route: Route, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        match route {
            Route::Encrypted => self.resolver().resolve(query).await,
            Route::Network(lan) => {
                for &server in &lan.servers {
                    if let Ok(reply) = exchange_async(server, query, tcp, NETWORK_TIMEOUT).await {
                        return Some(reply);
                    }
                }
                None
            }
        }
    }
}

/// Requête au DNS du réseau, depuis un fil hors du runtime (garde, tests).
pub fn exchange(server: SocketAddr, query: &[u8], tcp: bool, limit: Duration) -> Result<Vec<u8>> {
    rt::block_on(exchange_async(server, query, tcp, limit))
}

async fn exchange_async(server: SocketAddr, query: &[u8], tcp: bool, limit: Duration) -> Result<Vec<u8>> {
    let reply = if tcp { timeout(limit, exchange_tcp(server, query)).await } else { timeout(limit, exchange_udp(server, query)).await };
    reply.map_err(|_| anyhow!("{server} did not answer"))?
}

async fn exchange_udp(server: SocketAddr, query: &[u8]) -> Result<Vec<u8>> {
    let local: SocketAddr = if server.is_ipv4() { ([0, 0, 0, 0], 0).into() } else { ([0u16; 8], 0).into() };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(server).await?;
    socket.send(query).await?;
    let mut buf = vec![0; 65_535];
    loop {
        let n = socket.recv(&mut buf).await.with_context(|| format!("{server} did not answer"))?;
        // Réponse d'une autre requête : ignorée (protection contre les réponses forgées).
        if n >= 12 && dns::id(&buf[..n]) == dns::id(query) {
            buf.truncate(n);
            return Ok(buf);
        }
    }
}

async fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let len = stream.read_u16().await?;
    let mut msg = vec![0; usize::from(len)];
    stream.read_exact(&mut msg).await?;
    Ok(msg)
}

async fn write_frame(stream: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
    let len = u16::try_from(msg.len()).map_err(|_| std::io::Error::other("message too long"))?;
    let mut frame = Vec::with_capacity(msg.len() + 2);
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(msg);
    stream.write_all(&frame).await
}

async fn exchange_tcp(server: SocketAddr, query: &[u8]) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect(server).await?;
    write_frame(&mut stream, query).await?;
    let reply = read_frame(&mut stream).await?;
    ensure!(dns::id(&reply) == dns::id(query), "{server}: unexpected id");
    Ok(reply)
}

async fn serve_udp(socket: UdpSocket, shared: Arc<Shared>) {
    let socket = Arc::new(socket);
    let mut buf = vec![0; 65_535];
    loop {
        let Ok((n, client)) = socket.recv_from(&mut buf).await else { continue };
        if n < 12 {
            continue;
        }
        let query = &buf[..n];
        let route = shared.route(query);
        // Déjà en cache : réponse tout de suite, sans tâche.
        if let Route::Encrypted = route
            && let Some(reply) = shared.resolver().from_cache(query)
            && reply.len() <= dns::udp_limit(query)
        {
            socket.send_to(&reply, client).await.ok();
            continue;
        }
        let Some(slot) = shared.slot() else { continue };
        let (socket, query) = (socket.clone(), query.to_vec());
        tokio::spawn(async move {
            let reply = slot.0.answer(route, &query, false).await.or_else(|| dns::servfail(&query));
            // Réponse DoH plus grande que ce que le client accepte en UDP : il repassera en TCP.
            let reply = reply.and_then(|r| if r.len() > dns::udp_limit(&query) { dns::truncated(&r) } else { Some(r) });
            if let Some(reply) = reply {
                socket.send_to(&reply, client).await.ok();
            }
        });
    }
}

async fn serve_tcp(listener: TcpListener, shared: Arc<Shared>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else { continue };
        let Some(slot) = shared.slot() else { continue };
        tokio::spawn(async move {
            while let Ok(Ok(query)) = timeout(TCP_IDLE, read_frame(&mut stream)).await {
                let route = slot.0.route(&query);
                let Some(reply) = slot.0.answer(route, &query, true).await.or_else(|| dns::servfail(&query)) else { return };
                if write_frame(&mut stream, &reply).await.is_err() {
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
        let udp = std::net::UdpSocket::bind(addr).with_context(|| format!("cannot listen on UDP {addr}"))?;
        let tcp = std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on TCP {addr}"))?;
        udp.set_nonblocking(true)?;
        tcp.set_nonblocking(true)?;
        sockets.push((udp, tcp));
    }
    let _runtime = rt::handle().enter();
    for (udp, tcp) in sockets {
        let (udp, tcp) = (UdpSocket::from_std(udp)?, TcpListener::from_std(tcp)?);
        rt::handle().spawn(serve_udp(udp, shared.clone()));
        rt::handle().spawn(serve_tcp(tcp, shared.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream, UdpSocket};
    use std::thread;

    fn shared(local_mode: bool, lan: bool) -> Arc<Shared> {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        let shared = Shared::new(Resolver::new(&config).unwrap());
        *shared.local_mode.write().unwrap() = local_mode;
        if lan {
            *shared.lan.write().unwrap() =
                Arc::new(Lan { servers: vec![SocketAddr::from(([192, 168, 1, 1], 53))], domain: Some("home".into()) });
        }
        shared
    }

    fn network(route: Route) -> Option<Vec<SocketAddr>> {
        match route {
            Route::Encrypted => None,
            Route::Network(lan) => Some(lan.servers.clone()),
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

    fn read_frame(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
        let mut len = [0; 2];
        stream.read_exact(&mut len)?;
        let mut msg = vec![0; u16::from_be_bytes(len) as usize];
        stream.read_exact(&mut msg)?;
        Ok(msg)
    }

    fn write_frame(stream: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
        stream.write_all(&u16::try_from(msg.len()).unwrap().to_be_bytes())?;
        stream.write_all(msg)
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
        *shared.lan.write().unwrap() = Arc::new(Lan { servers: vec![fake_network_dns(false)], domain: Some("home".into()) });
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
        *shared.lan.write().unwrap() = Arc::new(Lan { servers: vec![SocketAddr::from(([192, 0, 2, 1], 53))], domain: None });
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
