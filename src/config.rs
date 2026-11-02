//! Configuration : un fichier TOML modifiable à tout moment (relu à chaud par la garde), sans
//! secret ; le jeton éventuel d'un serveur vit dans un fichier lisible par l'administrateur seul.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::i18n::Lang;

#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub language: Lang,
    /// Adresses où les programmes du poste envoient leurs requêtes (redémarrage pour changer).
    #[serde(default = "default_listen")]
    pub listen: Vec<SocketAddr>,
    /// Serveurs DoH, essayés dans cet ordre.
    pub server: Vec<Server>,
    /// Dernier recours : le DNS du réseau quand aucun serveur chiffré ne répond (portails captifs).
    #[serde(default = "yes")]
    pub local_fallback: bool,
    /// État et consigne de mode.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    /// Domaines demandés au DNS du réseau.
    #[serde(default = "default_clear_log")]
    pub clear_log: PathBuf,
    /// macOS : services réseau dont le DNS est réglé sur le relais (sans objet sous Linux).
    #[serde(default = "default_network_services")]
    pub network_services: Vec<String>,
}

/// Serveur DoH : `url` complète, ou préfixe auquel s'ajoute le jeton de `token_file`.
#[derive(Deserialize, Clone, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub url: String,
    /// Obligatoire si `url` contient un nom : le serveur est joint sans résolution en clair.
    pub ip: Option<IpAddr>,
    pub token_file: Option<PathBuf>,
}

/// Emplacements habituels, dans l'ordre : système, Homebrew (Apple Silicon puis Intel).
const CONFIG_PATHS: &[&str] = &["/etc/abdns/config.toml", "/opt/homebrew/etc/abdns/config.toml", "/usr/local/etc/abdns/config.toml"];

/// Fichier de configuration par défaut : le premier qui existe, sinon `/etc/abdns/config.toml`.
pub fn default_path() -> PathBuf {
    CONFIG_PATHS.iter().map(PathBuf::from).find(|p| p.exists()).unwrap_or_else(|| PathBuf::from(CONFIG_PATHS[0]))
}

fn yes() -> bool {
    true
}

fn default_listen() -> Vec<SocketAddr> {
    vec![SocketAddr::from(([127, 0, 0, 1], 53)), SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 53))]
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

/// Hôte d'une adresse `https://hôte[:port]/chemin`, crochets IPv6 retirés.
pub fn host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split('/').next().unwrap_or(rest);
    Some(match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(v6),
        None => authority.split(':').next().unwrap_or(authority),
    })
}

impl Server {
    pub fn host_is_ip(&self) -> bool {
        host(&self.url).is_some_and(|h| h.parse::<IpAddr>().is_ok())
    }

    /// Jeton du serveur, s'il en a un : ne jamais l'écrire dans un journal.
    pub fn token(&self) -> Result<Option<String>> {
        let Some(path) = &self.token_file else { return Ok(None) };
        let token = std::fs::read_to_string(path).with_context(|| format!("{}: cannot read token {}", self.name, path.display()))?;
        let token = token.trim();
        ensure!(valid_token(token), "{}: invalid token in {}", self.name, path.display());
        Ok(Some(token.to_owned()))
    }

    /// Adresse complète, jeton compris.
    pub fn full_url(&self) -> Result<String> {
        Ok(match self.token()? {
            Some(token) => format!("{}{token}", self.url),
            None => self.url.clone(),
        })
    }
}

impl Config {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(&text).with_context(|| format!("invalid {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)?;
        ensure!(!config.listen.is_empty(), "listen: at least one address is required");
        ensure!(!config.server.is_empty(), "at least one [[server]] is required");
        for server in &config.server {
            let Some(_) = host(&server.url) else { bail!("{}: url must start with https://", server.name) };
            ensure!(
                server.host_is_ip() || server.ip.is_some(),
                "{}: ip is required when url uses a host name (no plaintext lookup is ever made)",
                server.name
            );
            if server.token_file.is_some() {
                ensure!(server.url.ends_with('/'), "{}: url must end with / when token_file is set", server.name);
            }
        }
        Ok(config)
    }
}

pub fn valid_token(token: &str) -> bool {
    token.len() >= 16 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token() {
        assert!(valid_token("abcdefghijklmnopqrstuvwxyz012345-_"));
        assert!(!valid_token("short"));
        assert!(!valid_token("abcdefghijklmnop/../q"));
    }

    #[test]
    fn example_parses() {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        assert_eq!(config.server.len(), 2);
        assert!(config.local_fallback);
        assert_eq!(config.listen.len(), 2);
        assert!(config.server.iter().all(Server::host_is_ip));
    }

    #[test]
    fn minimal_config_gets_defaults() {
        let config = Config::parse("[[server]]\nname = \"a\"\nurl = \"https://203.0.113.5/dns-query\"\n").unwrap();
        assert!(config.local_fallback);
        assert_eq!(config.language, Lang::En);
        assert_eq!(config.server[0].full_url().unwrap(), "https://203.0.113.5/dns-query");
    }

    #[test]
    fn invalid_configs() {
        assert!(Config::parse("local_fallback = true\n").is_err());
        assert!(Config::parse("[[server]]\nname = \"a\"\nurl = \"https://dns.example.com/dns-query\"\n").is_err());
        assert!(Config::parse("[[server]]\nname = \"a\"\nurl = \"https://dns.example.com/q\"\nip = \"203.0.113.5\"\n").is_ok());
        assert!(Config::parse("[[server]]\nname = \"a\"\nurl = \"https://203.0.113.5/q\"\ntoken_file = \"/t\"\n").is_err());
        assert!(Config::parse("[[server]]\nname = \"a\"\nurl = \"http://203.0.113.5/\"\n").is_err());
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://[2a07:e340::2]/dns-query"), Some("2a07:e340::2"));
        assert_eq!(host("https://dns.example.com:8443/q"), Some("dns.example.com"));
        assert_eq!(host("https://1.1.1.1/dns-query"), Some("1.1.1.1"));
    }
}
