//! DNS du poste : une table du pare-feu du noyau envoie au relais toute requête DNS qui sort par
//! une carte Ethernet ou Wi-Fi (ADR 0007). Rien n'est changé dans le système : ni
//! `resolv.conf`, ni systemd-resolved, ni NetworkManager ; la table disparaît avec abdns.

use std::io;
use std::os::fd::{AsFd, AsRawFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};

use super::netfilter::{MARK, Netfilter, Ports};
use crate::config::Config;
use crate::{lock, logbook};

/// Socket qui possède la table : la garder ouverte la garde en place.
static TABLE: Mutex<Option<Netfilter>> = Mutex::new(None);
static REDIRECTING: AtomicBool = AtomicBool::new(false);

/// Port du relais sur la boucle locale (ou sur toutes les adresses), par famille.
pub(super) fn ports(config: &Config) -> Ports {
    let port =
        |v6: bool| config.listen.iter().find(|a| a.is_ipv6() == v6 && (a.ip().is_loopback() || a.ip().is_unspecified())).map(|a| a.port());
    Ports { v4: port(false), v6: port(true) }
}

/// `relay` : la table est posée (ou remplacée) ; sinon retirée.
pub fn set_system_dns(config: &Config, relay: bool) -> Result<()> {
    let mut table = lock(&TABLE);
    let socket = match table.take() {
        Some(socket) => socket,
        None => Netfilter::open().context("netlink (nf_tables)")?,
    };
    if relay {
        let ports = ports(config);
        ensure!(ports.v4.is_some() || ports.v6.is_some(), "listen: no loopback address to send DNS queries to");
        let owned = socket.install(ports).context("cannot set the firewall redirection (nf_tables, run as root)")?;
        if !owned {
            logbook::log("kernel older than 5.12: the firewall redirection stays if abdnsd is killed, until it restarts");
        }
        *table = Some(socket);
        REDIRECTING.store(true, Ordering::SeqCst);
        return Ok(());
    }
    let removed = socket.remove();
    REDIRECTING.store(false, Ordering::SeqCst);
    match removed {
        Err(e) if e.raw_os_error() == Some(libc::EPERM) => bail!("the firewall redirection belongs to a running abdnsd"),
        other => other.context("cannot remove the firewall redirection (nf_tables)"),
    }
}

/// Appelé par la garde : une table retirée par un autre programme (`nft flush ruleset`,
/// redémarrage du pare-feu) est reposée.
pub fn keep_system_dns(config: &Config) -> Result<()> {
    let missing = lock(&TABLE).as_ref().is_some_and(|socket| socket.exists().is_ok_and(|present| !present));
    if missing {
        logbook::log("firewall redirection removed by another program: setting it again");
        set_system_dns(config, true)?;
    }
    Ok(())
}

/// Requête d'abdns vers le DNS du réseau : marquée pour que la table la laisse passer. Sans la
/// marque alors que la table est en place, elle reviendrait au relais : refusée.
pub fn exempt(socket: &impl AsFd) -> io::Result<()> {
    let mark = MARK;
    let size = std::mem::size_of::<u32>() as libc::socklen_t;
    // SAFETY : descripteur emprunté, vivant pendant l'appel ; valeur de la taille indiquée.
    let set = unsafe { libc::setsockopt(socket.as_fd().as_raw_fd(), libc::SOL_SOCKET, libc::SO_MARK, (&raw const mark).cast(), size) };
    let error = io::Error::last_os_error();
    if set == 0 || !REDIRECTING.load(Ordering::SeqCst) { Ok(()) } else { Err(error) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_ports() {
        let config = |listen: &str| {
            Config::parse(&format!("listen = {listen}\n[[server]]\nname = \"a\"\nurl = \"https://203.0.113.5/q\"\n")).unwrap()
        };
        assert_eq!(ports(&config(r#"["127.0.0.1:53", "[::1]:53"]"#)), Ports { v4: Some(53), v6: Some(53) });
        assert_eq!(ports(&config(r#"["0.0.0.0:5353"]"#)), Ports { v4: Some(5353), v6: None });
        assert_eq!(ports(&config(r#"["192.168.1.2:53"]"#)), Ports { v4: None, v6: None });
    }
}
