//! Messages DNS (RFC 1035) : juste ce qu'il faut au relais, sans dépendance.

use std::net::Ipv4Addr;

pub const SERVFAIL: u8 = 2;
pub const NXDOMAIN: u8 = 3;
const HEADER: usize = 12;

/// Suffixes qui n'existent que sur le réseau local (RFC 6761, RFC 8375, usages courants).
const LOCAL_SUFFIXES: &[&str] = &["local", "lan", "home.arpa", "internal", "localdomain", "localhost"];

pub fn id(msg: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes([*msg.first()?, *msg.get(1)?]))
}

pub fn rcode(msg: &[u8]) -> Option<u8> {
    msg.get(3).map(|b| b & 0x0f)
}

/// Saute un nom (étiquettes ou pointeur de compression) ; rend la position qui suit.
fn skip_name(msg: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *msg.get(pos)? as usize;
        match len {
            0 => return Some(pos + 1),
            l if l & 0xc0 == 0xc0 => return Some(pos + 2),
            l => pos += 1 + l,
        }
    }
}

/// Nom de la première question, en minuscules ; tout caractère inattendu devient « ? »
/// (le nom finit dans une notification).
pub fn question(msg: &[u8]) -> Option<String> {
    if u16::from_be_bytes([*msg.get(4)?, *msg.get(5)?]) == 0 {
        return None;
    }
    let mut pos = HEADER;
    let mut labels = Vec::new();
    loop {
        let len = *msg.get(pos)? as usize;
        if len == 0 {
            break;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        let label = msg.get(pos + 1..pos + 1 + len)?;
        labels.push(
            label
                .iter()
                .map(|&b| match b.to_ascii_lowercase() {
                    c @ (b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_') => c as char,
                    _ => '?',
                })
                .collect::<String>(),
        );
        pos += 1 + len;
    }
    Some(if labels.is_empty() { ".".to_owned() } else { labels.join(".") })
}

/// Préfixe IPv4 privé, de boucle locale, lien-local ou CGNAT.
fn private_v4(octets: &[u8]) -> bool {
    match octets {
        [10, ..] | [127, ..] | [192, 168, ..] | [169, 254, ..] => true,
        [172, b, ..] => (16..=31).contains(b),
        [100, b, ..] => (64..=127).contains(b),
        _ => false,
    }
}

/// Recherche inverse d'une adresse privée (`1.0.168.192.in-addr.arpa`, fc00::/7, fe80::/10,
/// ::1 en `ip6.arpa`) ; une zone partielle comme `168.192.in-addr.arpa` compte aussi.
fn private_reverse(name: &str) -> bool {
    if let Some(rest) = name.strip_suffix(".in-addr.arpa") {
        let octets: Option<Vec<u8>> = rest.split('.').rev().map(|o| o.parse().ok()).collect();
        return octets.is_some_and(|o| private_v4(&o));
    }
    if let Some(rest) = name.strip_suffix(".ip6.arpa") {
        let nibbles: String = rest.split('.').rev().collect();
        return ["fc", "fd", "fe8", "fe9", "fea", "feb"].iter().any(|p| nibbles.starts_with(p))
            || (nibbles.len() == 32 && nibbles.trim_end_matches('1').chars().all(|c| c == '0'));
    }
    false
}

/// Nom propre au réseau local : jamais envoyé aux serveurs publics, jamais signalé.
/// `domain` : domaine annoncé par le réseau (option DHCP), s'il y en a un.
pub fn is_local(name: &str, domain: Option<&str>) -> bool {
    let under = |suffix: &str| name == suffix || name.ends_with(&format!(".{suffix}"));
    (name != "." && !name.contains('.'))
        || LOCAL_SUFFIXES.iter().any(|s| under(s))
        || domain.is_some_and(|d| !d.is_empty() && under(d))
        || private_reverse(name)
}

/// Requête récursive (RD) d'un seul nom.
pub fn query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut msg = Vec::with_capacity(HEADER + name.len() + 6);
    msg.extend_from_slice(&id.to_be_bytes());
    msg.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.trim_end_matches('.').split('.').filter(|l| !l.is_empty()) {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0);
    msg.extend_from_slice(&qtype.to_be_bytes());
    msg.extend_from_slice(&1u16.to_be_bytes());
    msg
}

/// Réponse SERVFAIL à une requête : même identifiant, même question.
pub fn servfail(query: &[u8]) -> Option<Vec<u8>> {
    let header = query.get(..HEADER)?;
    let end = (u16::from_be_bytes([header[4], header[5]]) > 0).then(|| skip_name(query, HEADER).map(|p| p + 4)).flatten();
    let mut reply = header.to_vec();
    reply[2] = 0x80 | (header[2] & 0x79); // QR, opcode et RD repris
    reply[3] = 0x80 | SERVFAIL; // RA
    let question = match end {
        Some(end) if end <= query.len() => &query[HEADER..end],
        _ => &[][..],
    };
    reply[4..6].copy_from_slice(&u16::from(!question.is_empty()).to_be_bytes());
    reply[6..12].fill(0);
    reply.extend_from_slice(question);
    Some(reply)
}

/// Adresses IPv4 de la section réponse.
pub fn ipv4_answers(msg: &[u8]) -> Vec<Ipv4Addr> {
    let count = |at: usize| msg.get(at..at + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]));
    let mut found = Vec::new();
    let mut pos = HEADER;
    for _ in 0..count(4) {
        match skip_name(msg, pos) {
            Some(p) => pos = p + 4,
            None => return found,
        }
    }
    for _ in 0..count(6) {
        let Some(p) = skip_name(msg, pos) else { return found };
        let Some(fixed) = msg.get(p..p + 10) else { return found };
        let rtype = u16::from_be_bytes([fixed[0], fixed[1]]);
        let len = u16::from_be_bytes([fixed[8], fixed[9]]) as usize;
        let Some(data) = msg.get(p + 10..p + 10 + len) else { return found };
        if rtype == 1 && len == 4 {
            found.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        pos = p + 10 + len;
    }
    found
}

const OPT: u16 = 41;
const SOA: u16 = 6;
/// Durée de vie maximale en cache, et durée donnée à une réponse sans enregistrement.
const MAX_TTL: u32 = 86_400;
const EMPTY_TTL: u32 = 30;

fn count(msg: &[u8], at: usize) -> u16 {
    msg.get(at..at + 2).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]))
}

/// Enregistrement d'un message : type, classe, position de la durée de vie, données.
struct Record {
    rtype: u16,
    class: u16,
    ttl_at: usize,
    data: std::ops::Range<usize>,
}

/// Enregistrements des sections réponse, autorité et additionnelle ; `None` si le message
/// est tronqué ou mal formé.
fn records(msg: &[u8]) -> Option<Vec<Record>> {
    let mut pos = HEADER;
    for _ in 0..count(msg, 4) {
        pos = skip_name(msg, pos)? + 4;
    }
    let total = usize::from(count(msg, 6)) + usize::from(count(msg, 8)) + usize::from(count(msg, 10));
    let mut found = Vec::with_capacity(total);
    for _ in 0..total {
        let p = skip_name(msg, pos)?;
        let fixed = msg.get(p..p + 10)?;
        let len = usize::from(u16::from_be_bytes([fixed[8], fixed[9]]));
        let data = p + 10..p + 10 + len;
        msg.get(data.clone())?;
        found.push(Record {
            rtype: u16::from_be_bytes([fixed[0], fixed[1]]),
            class: u16::from_be_bytes([fixed[2], fixed[3]]),
            ttl_at: p + 4,
            data: data.clone(),
        });
        pos = data.end;
    }
    Some(found)
}

fn ttl(msg: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([msg[at], msg[at + 1], msg[at + 2], msg[at + 3]])
}

/// Clé de cache : question en minuscules, type, classe, bits DO (EDNS) et CD.
pub fn cache_key(query: &[u8]) -> Option<Vec<u8>> {
    if count(query, 4) != 1 {
        return None;
    }
    let end = skip_name(query, HEADER)? + 4;
    let mut key: Vec<u8> = query.get(HEADER..end)?.iter().map(u8::to_ascii_lowercase).collect();
    let dnssec_ok = records(query)?.iter().any(|r| r.rtype == OPT && ttl(query, r.ttl_at) & 0x8000 != 0);
    key.push(u8::from(dnssec_ok) | (query[3] & 0x10));
    Some(key)
}

/// Taille de réponse UDP acceptée par le client (EDNS), 512 octets sans EDNS.
pub fn udp_limit(query: &[u8]) -> usize {
    records(query).and_then(|r| r.iter().find(|r| r.rtype == OPT).map(|r| usize::from(r.class).max(512))).unwrap_or(512)
}

/// Réponse trop grande pour UDP : en-tête et question seuls, bit TC, le client repasse en TCP.
pub fn truncated(reply: &[u8]) -> Option<Vec<u8>> {
    let end = if count(reply, 4) > 0 { skip_name(reply, HEADER)? + 4 } else { HEADER };
    let mut short = reply.get(..end)?.to_vec();
    short[2] |= 0x02;
    short[6..12].fill(0);
    Some(short)
}

/// Durée de cache d'une réponse : la plus petite durée de vie (pour SOA, bornée par son
/// minimum, RFC 2308) ; `None` si elle ne se met pas en cache (erreur, tronquée).
pub fn cache_ttl(reply: &[u8]) -> Option<u32> {
    if reply.len() < HEADER || reply[2] & 0x02 != 0 || !matches!(rcode(reply), Some(0 | NXDOMAIN)) {
        return None;
    }
    let ttls = records(reply)?.into_iter().filter(|r| r.rtype != OPT).map(|r| {
        let t = ttl(reply, r.ttl_at);
        if r.rtype == SOA && r.data.len() >= 4 { t.min(ttl(reply, r.data.end - 4)) } else { t }
    });
    Some(ttls.min().unwrap_or(EMPTY_TTL).min(MAX_TTL))
}

/// Applique `f` à la durée de vie de chaque enregistrement (sauf OPT).
pub fn map_ttls(reply: &mut [u8], f: impl Fn(u32) -> u32) {
    for record in records(reply).unwrap_or_default() {
        if record.rtype != OPT {
            let new = f(ttl(reply, record.ttl_at));
            reply[record.ttl_at..record.ttl_at + 4].copy_from_slice(&new.to_be_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Messages aléatoires ou abîmés (venus d'un client ou d'un serveur) : jamais de panique.
    #[test]
    fn malformed_messages_never_panic() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let valid = answer(300);
        for round in 0..200_000 {
            let mut msg = if round % 2 == 0 { valid.clone() } else { vec![0; (next() % 80) as usize] };
            for _ in 0..1 + next() % 8 {
                if !msg.is_empty() {
                    let at = next() as usize % msg.len();
                    msg[at] = next() as u8;
                }
            }
            if next() % 4 == 0 {
                msg.truncate(next() as usize % (msg.len() + 1));
            }
            let _ =
                (question(&msg), cache_key(&msg), servfail(&msg), udp_limit(&msg), truncated(&msg), cache_ttl(&msg), ipv4_answers(&msg));
            map_ttls(&mut msg, |t| t / 2);
        }
    }

    /// Réponse A de `example.com` avec une durée de vie, éventuellement EDNS côté requête.
    fn answer(ttl: u32) -> Vec<u8> {
        let mut r = query(9, "example.com", 1);
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 1;
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1]);
        r.extend_from_slice(&ttl.to_be_bytes());
        r.extend_from_slice(&[0, 4, 93, 184, 216, 34]);
        r
    }

    fn with_opt(mut q: Vec<u8>, size: u16, dnssec_ok: bool) -> Vec<u8> {
        q[11] = 1;
        q.extend_from_slice(&[0, 0, 41]);
        q.extend_from_slice(&size.to_be_bytes());
        q.extend_from_slice(&[0, 0, if dnssec_ok { 0x80 } else { 0 }, 0, 0, 0]);
        q
    }

    #[test]
    fn cache_keys() {
        let plain = cache_key(&query(1, "Example.COM", 1)).unwrap();
        assert_eq!(plain, cache_key(&query(2, "example.com", 1)).unwrap());
        assert_ne!(plain, cache_key(&query(1, "example.com", 28)).unwrap());
        let dnssec = cache_key(&with_opt(query(1, "example.com", 1), 1232, true)).unwrap();
        assert_ne!(plain, dnssec);
        assert_eq!(plain, cache_key(&with_opt(query(1, "example.com", 1), 1232, false)).unwrap());
    }

    #[test]
    fn udp_sizes() {
        assert_eq!(udp_limit(&query(1, "example.com", 1)), 512);
        assert_eq!(udp_limit(&with_opt(query(1, "example.com", 1), 1232, false)), 1232);
        assert_eq!(udp_limit(&with_opt(query(1, "example.com", 1), 100, false)), 512);
        let short = truncated(&answer(60)).unwrap();
        assert_eq!(short[2] & 0x02, 0x02);
        assert_eq!(&short[12..], &query(9, "example.com", 1)[12..]);
        assert_eq!(count(&short, 6), 0);
    }

    #[test]
    fn ttls() {
        assert_eq!(cache_ttl(&answer(300)), Some(300));
        assert_eq!(cache_ttl(&answer(999_999)), Some(MAX_TTL));
        let mut r = answer(300);
        map_ttls(&mut r, |t| t - 100);
        assert_eq!(cache_ttl(&r), Some(200));
        let mut failed = answer(300);
        failed[3] = 0x82;
        assert_eq!(cache_ttl(&failed), None);
        assert_eq!(cache_ttl(&answer(300)[..answer(300).len() - 2]), None);
        // NXDOMAIN avec SOA : durée bornée par le minimum du SOA.
        let mut nx = query(4, "nope.example.com", 1);
        nx[2] = 0x81;
        nx[3] = 0x83;
        nx[9] = 1;
        nx.extend_from_slice(&[0xc0, 17, 0, 6, 0, 1, 0, 0, 0x0e, 0x10, 0, 22, 0, 0]);
        nx.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 60]);
        assert_eq!(cache_ttl(&nx), Some(60));
    }

    #[test]
    fn query_and_question() {
        let q = query(0xbeef, "Wifi.SNCF.", 1);
        assert_eq!(id(&q), Some(0xbeef));
        assert_eq!(question(&q).as_deref(), Some("wifi.sncf"));
        // Vecteur de la RFC 8484 : www.example.com, A, identifiant 0.
        assert_eq!(query(0, "www.example.com", 1), b"\0\0\x01\0\0\x01\0\0\0\0\0\0\x03www\x07example\x03com\0\0\x01\0\x01");
    }

    #[test]
    fn sanitized_name() {
        let mut q = query(1, "ab.fr", 1);
        q[13] = b'"';
        assert_eq!(question(&q).as_deref(), Some("?b.fr"));
    }

    #[test]
    fn servfail_keeps_question() {
        let q = query(7, "example.org", 28);
        let r = servfail(&q).unwrap();
        assert_eq!(id(&r), Some(7));
        assert_eq!(rcode(&r), Some(SERVFAIL));
        assert_eq!(r[2] & 0x80, 0x80);
        assert_eq!(&r[12..], &q[12..]);
        assert!(servfail(&q[..5]).is_none());
    }

    #[test]
    fn answer_addresses() {
        // captive.apple.com : un CNAME compressé puis une adresse.
        let mut r = query(3, "captive.apple.com", 1);
        r[2] = 0x81;
        r[3] = 0x80;
        r[7] = 2;
        r.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xc0, 12]);
        r.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 17, 253, 109, 201]);
        assert_eq!(ipv4_answers(&r), vec![Ipv4Addr::new(17, 253, 109, 201)]);
        assert!(ipv4_answers(&r[..r.len() - 2]).is_empty());
    }

    #[test]
    fn local_names() {
        for name in [
            "nas",
            "printer.local",
            "box.lan",
            "router.home.arpa",
            "12.1.168.192.in-addr.arpa",
            "168.192.in-addr.arpa",
            "5.0.0.10.in-addr.arpa",
            "9.0.20.172.in-addr.arpa",
            "1.0.64.100.in-addr.arpa",
            "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.d.f.ip6.arpa",
            "0.8.e.f.ip6.arpa",
            "1.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.ip6.arpa",
        ] {
            assert!(is_local(name, None), "{name}");
        }
        for name in ["example.com", "lemonde.fr", "8.8.8.8.in-addr.arpa", "9.0.32.172.in-addr.arpa", "8.b.d.0.1.0.0.2.ip6.arpa", "."] {
            assert!(!is_local(name, None), "{name}");
        }
        assert!(is_local("nas.maison", Some("maison")));
        assert!(!is_local("maisonnette.fr", Some("maison")));
    }
}
