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

#[cfg(test)]
mod tests {
    use super::*;

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
