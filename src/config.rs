//! Configuration (fichier TOML, sans secret) ; le jeton éventuel vit dans un fichier à part,
//! lisible par l'administrateur seulement.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::i18n::Lang;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub language: Lang,
    /// Adresses où les programmes du poste envoient leurs requêtes.
    #[serde(default = "default_listen")]
    pub listen: Vec<SocketAddr>,
    /// Programme dnsproxy (DNS chiffré, cache, secours).
    #[serde(default = "default_dnsproxy")]
    pub dnsproxy: PathBuf,
    /// Port local de dnsproxy, sur 127.0.0.1.
    #[serde(default = "default_dnsproxy_port")]
    pub dnsproxy_port: u16,
    pub upstream: Upstream,
    /// Secours chiffrés, adresse IP fixée : aucune requête en clair pour les trouver.
    #[serde(default = "default_fallbacks")]
    pub fallback: Vec<Server>,
    /// État, fichiers de dnsproxy (dont le jeton), consigne de mode.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// Domaines demandés au DNS du réseau.
    #[serde(default = "default_clear_log")]
    pub clear_log: PathBuf,
    /// macOS : services réseau dont le DNS est réglé sur le relais.
    #[serde(default = "default_network_services")]
    pub network_services: Vec<String>,
}

/// Serveur DoH principal : `url` complète, ou préfixe auquel s'ajoute le jeton de `token_file`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub url: String,
    /// Obligatoire si `url` contient un nom : dnsproxy s'y connecte sans le résoudre.
    pub ip: Option<IpAddr>,
    pub token_file: Option<PathBuf>,
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub url: String,
    pub ip: IpAddr,
}

fn default_listen() -> Vec<SocketAddr> {
    vec![SocketAddr::from(([127, 0, 0, 1], 53)), SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 53))]
}

fn default_dnsproxy() -> PathBuf {
    ["/opt/homebrew/bin/dnsproxy", "/usr/local/bin/dnsproxy", "/usr/bin/dnsproxy"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("dnsproxy"))
}

fn default_dnsproxy_port() -> u16 {
    5300
}

/// Sans journaux, sans filtre, hors des pays qui imposent des blocages aux résolveurs publics.
pub fn default_fallbacks() -> Vec<Server> {
    let server = |name: &str, url: &str, ip: [u8; 4]| Server { name: name.to_owned(), url: url.to_owned(), ip: IpAddr::from(ip) };
    vec![
        server("Mullvad", "https://dns.mullvad.net/dns-query", [194, 242, 2, 2]),
        server("DNS.SB", "https://doh.dns.sb/dns-query", [185, 222, 222, 222]),
        server("Digitale Gesellschaft", "https://dns.digitale-gesellschaft.ch/dns-query", [185, 95, 218, 42]),
    ]
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/run/abdns")
}

fn default_clear_log() -> PathBuf {
    PathBuf::from("/var/log/abdns/clear.log")
}

fn default_network_services() -> Vec<String> {
    vec!["Wi-Fi".to_owned()]
}

fn host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    Some(rest.split('/').next().unwrap_or(rest))
}

impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        let config: Self = toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        ensure!(!config.listen.is_empty(), "listen: at least one address is required");
        let Some(upstream_host) = host(&config.upstream.url) else { bail!("upstream.url must start with https://") };
        ensure!(
            upstream_host.parse::<IpAddr>().is_ok() || config.upstream.ip.is_some(),
            "upstream.ip is required when upstream.url uses a host name (no plaintext lookup is ever made)"
        );
        if config.upstream.token_file.is_some() {
            ensure!(config.upstream.url.ends_with('/'), "upstream.url must end with / when token_file is set");
        }
        for server in &config.fallback {
            ensure!(host(&server.url).is_some(), "fallback {}: url must start with https://", server.name);
        }
        Ok(config)
    }

    pub fn proxy(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.dnsproxy_port))
    }

    /// Jeton du serveur principal, s'il y en a un : ne jamais l'écrire dans un journal.
    pub fn token(&self) -> Result<Option<String>> {
        let Some(path) = &self.upstream.token_file else { return Ok(None) };
        let token = std::fs::read_to_string(path).with_context(|| format!("cannot read token {}", path.display()))?;
        let token = token.trim();
        ensure!(valid_token(token), "invalid token in {}", path.display());
        Ok(Some(token.to_owned()))
    }

    /// Adresse complète du serveur principal (jeton compris).
    pub fn upstream_url(&self) -> Result<String> {
        Ok(match self.token()? {
            Some(token) => format!("{}{token}", self.upstream.url),
            None => self.upstream.url.clone(),
        })
    }

    pub fn upstream_is_ip(&self) -> bool {
        host(&self.upstream.url).is_some_and(|h| h.trim_matches(['[', ']']).parse::<IpAddr>().is_ok())
    }
}

pub fn valid_token(token: &str) -> bool {
    token.len() >= 16 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        let dir = std::env::temp_dir().join(format!("abdns-config-{}-{}", std::process::id(), text.len()));
        std::fs::write(&dir, text)?;
        let config = Config::read(&dir);
        std::fs::remove_file(&dir).ok();
        config
    }

    #[test]
    fn token() {
        assert!(valid_token("abcdefghijklmnopqrstuvwxyz012345-_"));
        assert!(!valid_token("short"));
        assert!(!valid_token("abcdefghijklmnop/../q"));
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let config = parse("[upstream]\nurl = \"https://203.0.113.5/dns-query\"\n").unwrap();
        assert_eq!(config.listen.len(), 2);
        assert_eq!(config.fallback.len(), 3);
        assert!(config.upstream_is_ip());
        assert_eq!(config.upstream_url().unwrap(), "https://203.0.113.5/dns-query");
        assert_eq!(config.language, Lang::En);
    }

    #[test]
    fn host_name_needs_an_address() {
        assert!(parse("[upstream]\nurl = \"https://dns.example.com/dns-query\"\n").is_err());
        let config = parse("[upstream]\nurl = \"https://dns.example.com/dns-query\"\nip = \"203.0.113.5\"\n").unwrap();
        assert!(!config.upstream_is_ip());
        assert!(parse("[upstream]\nurl = \"https://203.0.113.5/dns-query\"\ntoken_file = \"/t\"\n").is_err());
        assert!(parse("language = \"fr\"\n[upstream]\nurl = \"http://203.0.113.5/\"\n").is_err());
    }
}
