//! Sondes en clair vers le DNS du réseau : répond-il, et cache-t-il un portail captif ?

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::dns;
use crate::relay::exchange;

pub(super) const PORTAL_PROBE: &str = "http://captive.apple.com/hotspot-detect.html";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Nom jamais demandé (donc jamais en cache) d'une zone signée : NXDOMAIN attendu.
pub fn probe_name() -> String {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("abdns-{nanos:x}{:x}.example.org", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Vrai si le serveur en clair répond (NOERROR ou NXDOMAIN) dans le délai.
pub(super) fn answers(server: SocketAddr, timeout: Duration) -> bool {
    let query = dns::query(0x6764, &probe_name(), 1);
    exchange(server, &query, false, timeout).is_ok_and(|r| matches!(dns::rcode(&r), Some(0 | dns::NXDOMAIN)))
}

pub(super) fn sockets(dns: &[IpAddr]) -> Vec<SocketAddr> {
    dns.iter().map(|&ip| SocketAddr::new(ip, 53)).collect()
}

/// Portail captif : la page d'essai d'Apple, demandée par le DNS du réseau, ne répond pas
/// « Success ». Sans réponse du tout, on ne conclut rien.
pub(super) fn portal(dns: &[IpAddr]) -> bool {
    let query = dns::query(0x7074, "captive.apple.com", 1);
    let Some(ip) = sockets(dns)
        .into_iter()
        .find_map(|s| exchange(s, &query, false, Duration::from_secs(2)).ok())
        .and_then(|r| dns::ipv4_answers(&r).first().copied())
    else {
        return false;
    };
    let fetch = || -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&SocketAddr::new(IpAddr::V4(ip), 80), Duration::from_secs(3))?;
        stream.set_read_timeout(Some(Duration::from_secs(4)))?;
        stream.write_all(b"GET /hotspot-detect.html HTTP/1.0\r\nHost: captive.apple.com\r\nUser-Agent: CaptiveNetworkSupport\r\n\r\n")?;
        let mut body = Vec::new();
        stream.take(65_536).read_to_end(&mut body)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    };
    fetch().is_ok_and(|page| !page.contains("Success"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_probe_names() {
        assert_ne!(probe_name(), probe_name());
    }
}
