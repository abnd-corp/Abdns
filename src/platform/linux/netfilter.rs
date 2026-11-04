//! Pare-feu du noyau (nf_tables), parlé directement par netlink, sans outil : une table `abdns`
//! envoie au relais toute requête DNS (port 53) qui sort par une carte Ethernet ou Wi-Fi.
//! La table appartient à la socket qui l'a créée (Linux 5.12) : le noyau la retire dès que le
//! processus s'arrête, même tué net.
//!
//! Format des messages : `include/uapi/linux/netfilter/nf_tables.h` et `nfnetlink.h` (absents
//! de la crate libc). Les en-têtes netlink sont dans l'ordre de la machine, les nombres des
//! attributs nf_tables en ordre réseau.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Marque des sockets d'abdns vers le DNS du réseau : la table les laisse passer.
pub(super) const MARK: u32 = 0xabd5;
const TABLE: &str = "abdns";
const CHAIN: &str = "output";

const NFNL_SUBSYS_NFTABLES: u16 = 10;
const BATCH_BEGIN: u16 = 16;
const BATCH_END: u16 = 17;
const NEWTABLE: u16 = 0;
const GETTABLE: u16 = 1;
const DELTABLE: u16 = 2;
const NEWCHAIN: u16 = 3;
const NEWRULE: u16 = 6;

const REQUEST: u16 = libc::NLM_F_REQUEST as u16;
const ACK: u16 = libc::NLM_F_ACK as u16;
const CREATE: u16 = libc::NLM_F_CREATE as u16;
const APPEND: u16 = libc::NLM_F_APPEND as u16;
const NESTED: u16 = libc::NLA_F_NESTED as u16;
const INET: u8 = libc::NFPROTO_INET as u8;

const TABLE_NAME: u16 = 1;
const TABLE_FLAGS: u16 = 2;
const TABLE_OWNER: u32 = 2;
const CHAIN_TABLE: u16 = 1;
const CHAIN_NAME: u16 = 3;
const CHAIN_HOOK: u16 = 4;
const CHAIN_TYPE: u16 = 7;
const HOOK_NUMBER: u16 = 1;
const HOOK_PRIORITY: u16 = 2;
/// Priorité de la traduction d'adresse de destination (`NF_IP_PRI_NAT_DST`).
const PRIORITY_NAT_DST: i32 = -100;
const RULE_TABLE: u16 = 1;
const RULE_CHAIN: u16 = 2;
const RULE_EXPRESSIONS: u16 = 4;
const LIST_ELEM: u16 = 1;
const EXPR_NAME: u16 = 1;
const EXPR_DATA: u16 = 2;
const DATA_VALUE: u16 = 1;
const DATA_VERDICT: u16 = 2;
const VERDICT_CODE: u16 = 1;
/// Premier attribut de meta, payload et immediate : registre de destination ; de cmp : de source.
const REGISTER: u16 = 1;
const META_KEY: u16 = 2;
const CMP_OP: u16 = 2;
const CMP_DATA: u16 = 3;
const PAYLOAD_BASE: u16 = 2;
const PAYLOAD_OFFSET: u16 = 3;
const PAYLOAD_LEN: u16 = 4;
const IMMEDIATE_DATA: u16 = 2;
const REDIR_PORT_MIN: u16 = 1;

const META_MARK: u32 = 3;
const META_OIFTYPE: u32 = 9;
const META_NFPROTO: u32 = 15;
const META_L4PROTO: u32 = 16;
const EQ: u32 = 0;
const NEQ: u32 = 1;
const TRANSPORT_HEADER: u32 = 2;
/// Registre de 16 octets utilisé par toutes les expressions (`NFT_REG_1`).
const REG: u32 = 1;
const VERDICT_REG: u32 = 0;

/// Attribut netlink : longueur, type, valeur, bourrage jusqu'à 4 octets.
fn attr(buf: &mut Vec<u8>, kind: u16, value: &[u8]) {
    buf.extend_from_slice(&u16::try_from(4 + value.len()).unwrap_or(u16::MAX).to_ne_bytes());
    buf.extend_from_slice(&kind.to_ne_bytes());
    buf.extend_from_slice(value);
    buf.resize(buf.len().next_multiple_of(4), 0);
}

fn text(buf: &mut Vec<u8>, kind: u16, value: &str) {
    attr(buf, kind, format!("{value}\0").as_bytes());
}

fn be32(buf: &mut Vec<u8>, kind: u16, value: u32) {
    attr(buf, kind, &value.to_be_bytes());
}

fn nest(buf: &mut Vec<u8>, kind: u16, inner: impl FnOnce(&mut Vec<u8>)) {
    let start = buf.len();
    buf.extend_from_slice(&[0; 4]);
    inner(buf);
    let len = u16::try_from(buf.len() - start).unwrap_or(u16::MAX);
    buf[start..start + 2].copy_from_slice(&len.to_ne_bytes());
    buf[start + 2..start + 4].copy_from_slice(&(kind | NESTED).to_ne_bytes());
}

/// Messages envoyés en une fois, et le nombre d'accusés attendus.
#[derive(Default)]
struct Messages {
    buf: Vec<u8>,
    seq: u32,
    acks: usize,
}

impl Messages {
    fn message(&mut self, kind: u16, flags: u16, family: u8, attrs: impl FnOnce(&mut Vec<u8>)) {
        let batch = matches!(kind, BATCH_BEGIN | BATCH_END);
        let start = self.buf.len();
        self.seq += 1;
        self.buf.extend_from_slice(&[0; 4]);
        self.buf.extend_from_slice(&(if batch { kind } else { (NFNL_SUBSYS_NFTABLES << 8) | kind }).to_ne_bytes());
        self.buf.extend_from_slice(&(REQUEST | flags).to_ne_bytes());
        self.buf.extend_from_slice(&self.seq.to_ne_bytes());
        self.buf.extend_from_slice(&0u32.to_ne_bytes());
        // nfgenmsg : famille, version 0, identifiant de ressource (le sous-système, pour un lot).
        self.buf.extend_from_slice(&[family, 0]);
        self.buf.extend_from_slice(&(if batch { NFNL_SUBSYS_NFTABLES } else { 0 }).to_be_bytes());
        attrs(&mut self.buf);
        let len = u32::try_from(self.buf.len() - start).unwrap_or(u32::MAX);
        self.buf[start..start + 4].copy_from_slice(&len.to_ne_bytes());
        self.acks += usize::from(flags & ACK != 0);
    }

    /// Lot appliqué en entier ou pas du tout.
    fn batch(build: impl FnOnce(&mut Self)) -> Self {
        let mut messages = Self::default();
        messages.message(BATCH_BEGIN, 0, 0, |_| {});
        build(&mut messages);
        messages.message(BATCH_END, 0, 0, |_| {});
        messages
    }
}

/// Expressions d'une règle, chacune chargeant le registre ou le comparant.
#[derive(Default)]
struct Rule(Vec<u8>);

impl Rule {
    fn expr(mut self, name: &str, data: impl FnOnce(&mut Vec<u8>)) -> Self {
        nest(&mut self.0, LIST_ELEM, |b| {
            text(b, EXPR_NAME, name);
            nest(b, EXPR_DATA, data);
        });
        self
    }

    fn meta(self, key: u32) -> Self {
        self.expr("meta", |b| {
            be32(b, REGISTER, REG);
            be32(b, META_KEY, key);
        })
    }

    fn cmp(self, op: u32, value: &[u8]) -> Self {
        self.expr("cmp", |b| {
            be32(b, REGISTER, REG);
            be32(b, CMP_OP, op);
            nest(b, CMP_DATA, |d| attr(d, DATA_VALUE, value));
        })
    }

    fn payload(self, base: u32, offset: u32, len: u32) -> Self {
        self.expr("payload", |b| {
            be32(b, REGISTER, REG);
            be32(b, PAYLOAD_BASE, base);
            be32(b, PAYLOAD_OFFSET, offset);
            be32(b, PAYLOAD_LEN, len);
        })
    }

    fn accept(self) -> Self {
        self.expr("immediate", |b| {
            be32(b, REGISTER, VERDICT_REG);
            nest(b, IMMEDIATE_DATA, |d| nest(d, DATA_VERDICT, |v| be32(v, VERDICT_CODE, libc::NF_ACCEPT as u32)));
        })
    }

    /// Vers le même port de la boucle locale, `port` en ordre réseau.
    fn redirect(self, port: u16) -> Self {
        self.expr("immediate", |b| {
            be32(b, REGISTER, REG);
            nest(b, IMMEDIATE_DATA, |d| attr(d, DATA_VALUE, &port.to_be_bytes()));
        })
        .expr("redir", |b| be32(b, REDIR_PORT_MIN, REG))
    }
}

/// Ports du relais sur la boucle locale, en IPv4 et en IPv6.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Ports {
    pub(super) v4: Option<u16>,
    pub(super) v6: Option<u16>,
}

fn rules(ports: Ports) -> Vec<Rule> {
    let ether = libc::ARPHRD_ETHER.to_ne_bytes();
    let mut rules = vec![
        // Requêtes d'abdns lui-même (DNS du réseau, portail) : laissées passer.
        Rule::default().meta(META_MARK).cmp(EQ, &MARK.to_ne_bytes()).accept(),
        // Ni UDP ni TCP : rien à voir.
        Rule::default().meta(META_L4PROTO).cmp(NEQ, &[libc::IPPROTO_UDP as u8]).cmp(NEQ, &[libc::IPPROTO_TCP as u8]).accept(),
    ];
    // Port 53 vers une carte Ethernet ou Wi-Fi : la boucle locale (stub de resolved, relais) et
    // les VPN (tun, WireGuard) gardent leur DNS.
    for (family, port) in [(libc::NFPROTO_IPV4, ports.v4), (libc::NFPROTO_IPV6, ports.v6)] {
        if let Some(port) = port {
            rules.push(
                Rule::default()
                    .meta(META_NFPROTO)
                    .cmp(EQ, &[family as u8])
                    .meta(META_OIFTYPE)
                    .cmp(EQ, &ether)
                    .payload(TRANSPORT_HEADER, 2, 2)
                    .cmp(EQ, &53u16.to_be_bytes())
                    .redirect(port),
            );
        }
    }
    rules
}

/// Lot qui remplace la table : celle d'un abdns précédent, sans propriétaire (noyau ancien ou
/// `owner` faux), est retirée dans le même lot.
fn install_batch(ports: Ports, owner: bool) -> Messages {
    Messages::batch(|m| {
        m.message(NEWTABLE, CREATE | ACK, INET, |b| text(b, TABLE_NAME, TABLE));
        m.message(DELTABLE, ACK, INET, |b| text(b, TABLE_NAME, TABLE));
        m.message(NEWTABLE, CREATE | ACK, INET, |b| {
            text(b, TABLE_NAME, TABLE);
            if owner {
                be32(b, TABLE_FLAGS, TABLE_OWNER);
            }
        });
        m.message(NEWCHAIN, CREATE | ACK, INET, |b| {
            text(b, CHAIN_TABLE, TABLE);
            text(b, CHAIN_NAME, CHAIN);
            nest(b, CHAIN_HOOK, |h| {
                be32(h, HOOK_NUMBER, libc::NF_INET_LOCAL_OUT as u32);
                be32(h, HOOK_PRIORITY, PRIORITY_NAT_DST.cast_unsigned());
            });
            text(b, CHAIN_TYPE, "nat");
        });
        for rule in rules(ports) {
            m.message(NEWRULE, CREATE | APPEND | ACK, INET, |b| {
                text(b, RULE_TABLE, TABLE);
                text(b, RULE_CHAIN, CHAIN);
                nest(b, RULE_EXPRESSIONS, |e| e.extend_from_slice(&rule.0));
            });
        }
    })
}

/// Socket netlink vers nf_tables. Tant qu'elle reste ouverte, la table qu'elle a créée existe.
pub(super) struct Netfilter(OwnedFd);

impl Netfilter {
    pub(super) fn open() -> io::Result<Self> {
        // SAFETY : appel système sans pointeur ; le descripteur rendu n'appartient qu'à nous.
        let fd = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_NETFILTER) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY : descripteur valide, ouvert ci-dessus et confié à OwnedFd seul.
        let socket = Self(unsafe { OwnedFd::from_raw_fd(fd) });
        // Le noyau répond toujours ; le délai évite seulement d'attendre sans fin.
        let timeout = libc::timeval { tv_sec: 2, tv_usec: 0 };
        let size = std::mem::size_of::<libc::timeval>() as libc::socklen_t;
        // SAFETY : `timeout` vit pendant l'appel et `size` est sa taille.
        let set = unsafe { libc::setsockopt(socket.0.as_raw_fd(), libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&raw const timeout).cast(), size) };
        if set != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(socket)
    }

    fn send(&self, data: &[u8]) -> io::Result<()> {
        // SAFETY : structure C sans invariant, tout à zéro : l'adresse du noyau.
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        let size = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
        // SAFETY : tampon et adresse valides pendant l'appel, tailles exactes.
        let sent = unsafe { libc::sendto(self.0.as_raw_fd(), data.as_ptr().cast(), data.len(), 0, (&raw const kernel).cast(), size) };
        if usize::try_from(sent).is_ok_and(|n| n == data.len()) { Ok(()) } else { Err(io::Error::last_os_error()) }
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY : tampon valide de la taille donnée.
        let n = unsafe { libc::recv(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        usize::try_from(n).map_err(|_| io::Error::last_os_error())
    }

    /// Envoie les messages, puis attend un accusé par message qui en demande un ; la première
    /// erreur du noyau est rendue telle quelle (`ENOENT`, `EPERM`…).
    fn exchange(&self, messages: &Messages) -> io::Result<()> {
        self.send(&messages.buf)?;
        let mut pending = messages.acks;
        let mut buf = vec![0u8; 16 * 1024];
        while pending > 0 {
            let n = self.recv(&mut buf)?;
            for code in acks(&buf[..n]) {
                if code != 0 {
                    return Err(io::Error::from_raw_os_error(-code));
                }
                pending = pending.saturating_sub(1);
            }
        }
        Ok(())
    }

    /// Pose la table. Un module du noyau chargé à la demande fait rejouer le lot (`EAGAIN`) ;
    /// un noyau sans tables à propriétaire (avant 5.12) la reçoit sans (`EOPNOTSUPP`).
    pub(super) fn install(&self, ports: Ports) -> io::Result<bool> {
        let mut owner = true;
        let mut tries = 0;
        loop {
            match self.exchange(&install_batch(ports, owner)) {
                Ok(()) => return Ok(owner),
                Err(e) if e.raw_os_error() == Some(libc::EAGAIN) && tries < 3 => tries += 1,
                Err(e) if e.raw_os_error() == Some(libc::EOPNOTSUPP) && owner => owner = false,
                Err(e) => return Err(e),
            }
        }
    }

    pub(super) fn exists(&self) -> io::Result<bool> {
        let mut get = Messages::default();
        get.message(GETTABLE, ACK, INET, |b| text(b, TABLE_NAME, TABLE));
        match self.exchange(&get) {
            Ok(()) => Ok(true),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Retire la table ; absente, rien à faire.
    pub(super) fn remove(&self) -> io::Result<()> {
        let delete = Messages::batch(|m| m.message(DELTABLE, ACK, INET, |b| text(b, TABLE_NAME, TABLE)));
        match self.exchange(&delete) {
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => Ok(()),
            other => other,
        }
    }
}

/// Codes des accusés (`NLMSG_ERROR`, 0 en cas de succès) contenus dans une réponse du noyau.
fn acks(data: &[u8]) -> Vec<i32> {
    let mut codes = Vec::new();
    let mut rest = data;
    while rest.len() >= 16 {
        let len = u32::from_ne_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let kind = u16::from_ne_bytes([rest[4], rest[5]]);
        if len < 16 || len > rest.len() {
            break;
        }
        if i32::from(kind) == libc::NLMSG_ERROR && len >= 20 {
            codes.push(i32::from_ne_bytes([rest[16], rest[17], rest[18], rest[19]]));
        }
        rest = &rest[len.next_multiple_of(4).min(rest.len())..];
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Attributs (type sans drapeau, valeur) au premier niveau d'un tampon.
    fn attrs(mut data: &[u8]) -> Vec<(u16, &[u8])> {
        let mut out = Vec::new();
        while data.len() >= 4 {
            let len = usize::from(u16::from_ne_bytes([data[0], data[1]]));
            out.push((u16::from_ne_bytes([data[2], data[3]]) & !NESTED, &data[4..len]));
            data = &data[len.next_multiple_of(4).min(data.len())..];
        }
        out
    }

    #[test]
    fn attributes_are_padded_and_nested() {
        let mut buf = Vec::new();
        text(&mut buf, TABLE_NAME, TABLE);
        assert_eq!(buf, [10, 0, 1, 0, b'a', b'b', b'd', b'n', b's', 0, 0, 0]);
        let mut buf = Vec::new();
        nest(&mut buf, CHAIN_HOOK, |h| be32(h, HOOK_PRIORITY, PRIORITY_NAT_DST.cast_unsigned()));
        assert_eq!(buf, [12, 0, 4, 0x80, 8, 0, 2, 0, 0xff, 0xff, 0xff, 0x9c]);
    }

    #[test]
    fn batch_framing() {
        let batch = install_batch(Ports { v4: Some(53), v6: Some(53) }, true);
        // Début et fin de lot, deux tables et une suppression, une chaîne, quatre règles.
        let mut kinds = Vec::new();
        let mut rest = batch.buf.as_slice();
        while !rest.is_empty() {
            let len = u32::from_ne_bytes(rest[..4].try_into().unwrap()) as usize;
            assert_eq!(len % 4, 0);
            kinds.push(u16::from_ne_bytes([rest[4], rest[5]]));
            rest = &rest[len..];
        }
        let nft = |m: u16| (NFNL_SUBSYS_NFTABLES << 8) | m;
        assert_eq!(kinds, [BATCH_BEGIN, nft(0), nft(2), nft(0), nft(3), nft(6), nft(6), nft(6), nft(6), BATCH_END]);
        assert_eq!(batch.acks, 8);
        // Le début de lot désigne le sous-système nf_tables (identifiant de ressource, ordre réseau).
        assert_eq!(&batch.buf[16..20], &[0, 0, 0, 10]);
    }

    #[test]
    fn redirect_rule_reads_the_port_and_the_card_type() {
        let rules = rules(Ports { v4: Some(53), v6: None });
        assert_eq!(rules.len(), 3);
        let names: Vec<&[u8]> = attrs(&rules[2].0).iter().map(|(_, e)| attrs(e)[0].1).collect();
        let expected: [&[u8]; 8] = [b"meta\0", b"cmp\0", b"meta\0", b"cmp\0", b"payload\0", b"cmp\0", b"immediate\0", b"redir\0"];
        assert_eq!(names, expected);
        // Le port de destination comparé est 53, en ordre réseau.
        let port = attrs(attrs(&rules[2].0)[5].1)[1].1;
        assert_eq!(attrs(attrs(port)[2].1)[0].1, [0, 53]);
    }

    #[test]
    fn acknowledgements() {
        let mut reply = Vec::new();
        for code in [0i32, -libc::ENOENT] {
            reply.extend_from_slice(&36u32.to_ne_bytes());
            reply.extend_from_slice(&(libc::NLMSG_ERROR as u16).to_ne_bytes());
            reply.extend_from_slice(&[0; 10]);
            reply.extend_from_slice(&code.to_ne_bytes());
            reply.extend_from_slice(&[0; 16]);
        }
        assert_eq!(acks(&reply), [0, -libc::ENOENT]);
        assert!(acks(&[1, 2, 3]).is_empty());
    }

    /// Contre le vrai noyau : `sudo cargo test -- --ignored netfilter` (root, Linux 5.12 ou plus).
    #[test]
    #[ignore = "root et noyau Linux"]
    fn netfilter_table_lives_with_its_socket() {
        let owner = Netfilter::open().unwrap();
        assert!(owner.install(Ports { v4: Some(53), v6: Some(53) }).unwrap(), "table à propriétaire");
        let other = Netfilter::open().unwrap();
        assert!(other.exists().unwrap());
        // Une autre socket ne peut pas la retirer ; reposée par la même, elle reste.
        assert_eq!(other.remove().unwrap_err().raw_os_error(), Some(libc::EPERM));
        owner.install(Ports { v4: Some(53), v6: None }).unwrap();
        drop(owner);
        assert!(!other.exists().unwrap(), "retirée par le noyau avec sa socket");
        other.remove().unwrap();
    }
}
