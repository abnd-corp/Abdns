//! abdnsd : le service. Relais DNS local, client DoH, garde et canal de commande.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use abdns::config::{self, Config};
use abdns::i18n::Text;
use abdns::{control, doh, guard, logbook, platform, relay, rt};
use anyhow::{Context, Result, ensure};

const USAGE: &str = "Usage: abdnsd [--config FILE] [--no-system]
  Run the abdns service (as root). The configuration is read again whenever it changes.
  --no-system   leave the system DNS untouched (testing)
See abdnsd(8) and abdns(1).";

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

fn run(config_path: &Path, system: bool) -> Result<()> {
    let config = Config::read(config_path)?;
    std::fs::create_dir_all(&config.state_dir).with_context(|| format!("cannot create {}", config.state_dir.display()))?;
    if let Some(parent) = config.clear_log.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let config = Arc::new(config);
    if system {
        // Si la garde plante, le service s'arrête : le poste repasse sur le DNS du réseau. Une
        // panique ailleurs (une requête, le canal de commande) ne coûte que ce qu'elle traitait.
        let config = config.clone();
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() == Some("main") {
                platform::set_system_dns(&config, false).ok();
            }
            default(info);
        }));
    }
    let signals = Arc::new(control::Signals::default());
    let started = doh::Resolver::new(&config).and_then(|resolver| {
        let shared = relay::Shared::new(resolver);
        relay::start(&config.listen, &shared)?;
        // Sans canal de commande, le DNS doit continuer : `abdns status` lit alors le fichier d'état.
        if let Err(error) = control::serve(config.clone(), shared.clone(), signals.clone()) {
            logbook::log(&format!("control channel unavailable: {error:#}"));
        }
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
    logbook::log(&format!("abdnsd {} ready on {:?}; servers: {}", env!("CARGO_PKG_VERSION"), config.listen, servers.join(", ")));
    let stop = Arc::new(AtomicBool::new(false));
    rt::on_stop(&stop).context("cannot handle stop signals")?;
    guard::Guard::new(config.clone(), config_path.to_path_buf(), system, shared, signals).run(&stop);
    if system {
        // Arrêt voulu : le poste repasse sur le DNS du réseau.
        platform::set_system_dns(&config, false).ok();
    }
    std::fs::remove_file(control::socket_path(&config)).ok();
    logbook::log("stopped");
    Ok(())
}

fn main() -> Result<()> {
    let mut config_path = config::default_path();
    let mut system = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = PathBuf::from(args.next().context("--config needs a file")?),
            "--no-system" => system = false,
            "--version" | "-V" => {
                println!("abdnsd {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => {
                ensure!(false, "unknown argument: {other}\n{USAGE}");
            }
        }
    }
    run(&config_path, system)
}
