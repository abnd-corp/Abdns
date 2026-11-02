//! abdns : DNS chiffré du poste, avec son propre serveur DoH d'abord, des secours chiffrés,
//! et le DNS du réseau seulement en dernier recours (portails captifs).

mod config;
mod dns;
mod guard;
mod i18n;
mod logbook;
mod platform;
mod proxy;
mod relay;

use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail, ensure};

use config::Config;
use i18n::Text;

const USAGE: &str = "Usage: abdns [--config FILE] <command>
  service [--no-system]        run the relay and the guard (as root);
                               --no-system leaves the system DNS untouched (testing)
  status                       current mode, network, names looked up in clear
  mode auto|encrypted|local    force a mode (as root); auto by default
  token                        store the upstream token read from standard input (as root)
  install                      install and start the system service (as root, macOS)
  uninstall                    stop and remove the system service, restore the network DNS (as root)
  version                      print the version";

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

fn service(config: Config, system: bool) -> Result<()> {
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
    let stop = Arc::new(AtomicBool::new(false));
    let shared = relay::Shared::new(config.proxy());
    let mut supervisor = None;
    let started = proxy::prepare(&config).and_then(|()| proxy::supervise(config.clone(), stop.clone())).and_then(|handle| {
        supervisor = Some(handle);
        relay::start(&config.listen, &shared)
    });
    if let Err(error) = started {
        stop.store(true, Ordering::SeqCst);
        if let Some(handle) = supervisor {
            handle.join().ok();
        }
        if system {
            platform::set_system_dns(&config, false).ok();
            notify_failure(&config, &error);
        }
        return Err(error);
    }
    std::fs::remove_file(config.state_dir.join("failure-notified")).ok();
    let supervisor = supervisor.context("dnsproxy supervisor missing")?;
    if !guard::wait_encrypted(&config, 15) {
        logbook::log("dnsproxy does not answer yet: the guard will decide");
    }
    if system && let Err(error) = platform::set_system_dns(&config, true) {
        logbook::log(&format!("{error:#}"));
        platform::notify("abdns", &format!("{error:#}"));
    }
    let fallbacks: Vec<&str> = config.fallback.iter().map(|s| s.name.as_str()).collect();
    logbook::log(&format!("relay ready on {:?}; fallbacks: {}", config.listen, fallbacks.join(", ")));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst)).context("cannot handle stop signals")?;
    }
    guard::Guard::new(config.clone(), shared).run(&stop);
    if system {
        // Arrêt voulu : le poste repasse sur le DNS du réseau.
        platform::set_system_dns(&config, false).ok();
    }
    supervisor.join().ok();
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

/// Jeton seul, ou adresse DoH complète dont il est le dernier segment.
fn store_token(config: &Config) -> Result<()> {
    let Some(path) = &config.upstream.token_file else { bail!("upstream.token_file is not set in the configuration") };
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let token = input.trim().rsplit('/').next().unwrap_or_default();
    ensure!(config::valid_token(token), "no valid token on standard input");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    proxy::write_private(path, &format!("{token}\n"))?;
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
        ["service"] => service(Config::read(&config_path)?, true),
        ["service", "--no-system"] => service(Config::read(&config_path)?, false),
        ["status"] => status(&Config::read(&config_path)?),
        ["mode", mode] => set_mode(&Config::read(&config_path)?, mode),
        ["token"] => store_token(&Config::read(&config_path)?),
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
