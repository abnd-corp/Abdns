//! abdns : DNS chiffré du poste, avec son propre serveur DoH d'abord, des secours chiffrés dans
//! l'ordre, et le DNS du réseau seulement en dernier recours (portails captifs).

mod config;
mod dns;
mod doh;
mod guard;
mod i18n;
mod logbook;
mod platform;
mod relay;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};

use config::Config;
use i18n::Text;

const USAGE: &str = "Usage: abdns [--config FILE] <command>
  service [--no-system]        run the relay and the guard (as root);
                               --no-system leaves the system DNS untouched (testing)
  status                       current mode, servers, network, names looked up in clear
  mode auto|encrypted|local    force a mode (as root); auto by default
  token [SERVER]               store a server token read from standard input (as root)
  install                      install and start the system service (as root, macOS)
  uninstall                    stop and remove the system service, restore the network DNS (as root)
  version                      print the version

The configuration file is read again whenever it changes: no restart needed.";

/// Relancé toutes les 30 s par le système après un échec : une notification par heure suffit.
fn notify_failure(config: &Config, error: &anyhow::Error) {
    let marker = config.state_dir.join("failure-notified");
    let recent = std::fs::metadata(&marker).and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 3600));
    if !recent {
        std::fs::write(&marker, b"").ok();
        let text = Text(config.language);
        platform::notify(&text.stopped_title(), &text.stopped_body(&format!("{error:#}")));
    }
}

fn service(config_path: &Path, system: bool) -> Result<()> {
    let config = Config::read(config_path)?;
    std::fs::create_dir_all(&config.state_dir).with_context(|| format!("cannot create {}", config.state_dir.display()))?;
    if let Some(parent) = config.clear_log.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let config = Arc::new(config);
    if system {
        // Quoi qu'il arrive au programme, le poste garde un DNS.
        let config = config.clone();
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            platform::set_system_dns(&config, false).ok();
            default(info);
        }));
    }
    let started = doh::Resolver::new(&config).and_then(|resolver| {
        let shared = relay::Shared::new(resolver);
        relay::start(&config.listen, &shared)?;
        Ok(shared)
    });
    let shared = match started {
        Ok(shared) => shared,
        Err(error) => {
            if system {
                platform::set_system_dns(&config, false).ok();
                notify_failure(&config, &error);
            }
            return Err(error);
        }
    };
    std::fs::remove_file(config.state_dir.join("failure-notified")).ok();
    if !guard::wait_encrypted(&shared.resolver(), 15) {
        logbook::log("no encrypted server answers yet: the guard will decide");
    }
    if system && let Err(error) = platform::set_system_dns(&config, true) {
        logbook::log(&format!("{error:#}"));
        platform::notify("abdns", &format!("{error:#}"));
    }
    let servers: Vec<&str> = config.server.iter().map(|s| s.name.as_str()).collect();
    logbook::log(&format!("relay ready on {:?}; servers: {}", config.listen, servers.join(", ")));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).context("cannot handle stop signals")?;
    }
    guard::Guard::new(config.clone(), config_path.to_path_buf(), system, shared).run(&stop);
    if system {
        // Arrêt voulu : le poste repasse sur le DNS du réseau.
        platform::set_system_dns(&config, false).ok();
    }
    logbook::log("stopped");
    Ok(())
}

fn status(config: &Config) -> Result<()> {
    let path = guard::state_file(config);
    let text = std::fs::read_to_string(&path).with_context(|| format!("cannot read {}: is the service running?", path.display()))?;
    print!("{text}");
    Ok(())
}

fn set_mode(config: &Config, mode: &str) -> Result<()> {
    let Some(force) = guard::Force::parse(mode) else { bail!("unknown mode: {mode}\n{USAGE}") };
    let path = guard::force_file(config);
    if force == guard::Force::Auto {
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        }
    } else {
        std::fs::write(&path, mode).with_context(|| format!("cannot write {}", path.display()))?;
    }
    println!("{}", Text(config.language).mode_set(mode));
    Ok(())
}

/// Jeton seul, ou adresse DoH complète dont il est le dernier segment ; pour le serveur nommé,
/// ou le seul serveur qui a un `token_file`.
fn store_token(config: &Config, server: Option<&str>) -> Result<()> {
    let candidates: Vec<_> = config.server.iter().filter(|s| s.token_file.is_some() && server.is_none_or(|n| s.name == n)).collect();
    let target = match candidates.as_slice() {
        [one] => *one,
        [] => bail!("no server with token_file{}", server.map(|n| format!(" named {n}")).unwrap_or_default()),
        _ => bail!("several servers have a token_file: name one (abdns token \"NAME\")"),
    };
    let path = target.token_file.as_ref().context("token_file")?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let token = input.trim().rsplit('/').next().unwrap_or_default();
    ensure!(config::valid_token(token), "no valid token on standard input");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut f| f.write_all(format!("{token}\n").as_bytes()))
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("{}", Text(config.language).token_saved(&path.display().to_string()));
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = PathBuf::from(platform::CONFIG_PATH);
    if let Some(i) = args.iter().position(|a| a == "--config") {
        ensure!(i + 1 < args.len(), "--config needs a file\n{USAGE}");
        config_path = PathBuf::from(args.remove(i + 1));
        args.remove(i);
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["service"] => service(&config_path, true),
        ["service", "--no-system"] => service(&config_path, false),
        ["status"] => status(&Config::read(&config_path)?),
        ["mode", mode] => set_mode(&Config::read(&config_path)?, mode),
        ["token"] => store_token(&Config::read(&config_path)?, None),
        ["token", server] => store_token(&Config::read(&config_path)?, Some(server)),
        ["install"] => {
            Config::read(&config_path)?;
            platform::install(&config_path)
        }
        ["uninstall"] => platform::uninstall(Config::read(&config_path).ok().as_ref()),
        ["version" | "--version" | "-V"] => {
            println!("abdns {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
