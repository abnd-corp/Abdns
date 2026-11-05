//! abdns : l'outil de l'utilisateur. Pilote abdnsd par son canal local, vérifie la
//! configuration, montre les journaux ; installe le service quand aucun gestionnaire de
//! paquets ne s'en charge.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use abdns::config::{self, Config};
use abdns::control::{self, Request};
use abdns::guard;
use abdns::i18n::{Lang, Text};
use abdns::platform;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

const USAGE: &str = "Usage: abdns [--config FILE] [--json] <command>
  status                       mode, servers, network, names sent in clear
  test                         query every server now and show its response time
  mode auto|encrypted|local    force a mode (administrators); auto by default
  reload                       read the configuration again now (administrators)
  check                        validate the configuration file, like sshd -t
  logs [-f] [-n LINES]         show the service log (-f: follow)
  token [SERVER]               store a server token, pasted or piped as one line (as root)
  install                      install and start abdnsd without a package manager (as root, macOS and Linux)
  uninstall                    stop and remove abdnsd, restore the network DNS (as root, macOS and Linux)
  completions bash|zsh|fish    print a shell completion script
  version                      print the version
See abdns(1) and abdnsd(8).";

fn say(lang: Lang, en: &str, fr: &str) -> String {
    if lang == Lang::Fr { fr.to_owned() } else { en.to_owned() }
}

/// Le service par son canal ; pour `status` sans droit sur le canal, le fichier d'état.
fn ask(config: &Config, request: &Request) -> Result<Value> {
    match control::request(&control::socket_path(config), request) {
        Ok(value) => Ok(value),
        Err(error) if *request == Request::Status => {
            let path = guard::state_file(config);
            let text = std::fs::read_to_string(&path).with_context(|| format!("{error:#}; {} unreadable too", path.display()))?;
            Ok(serde_json::from_str(&text)?)
        }
        Err(error) => Err(error.context(say(
            config.language,
            "is abdnsd running, and are you an administrator?",
            "abdnsd tourne-t-il, et êtes-vous administrateur ?",
        ))),
    }
}

fn print_status(lang: Lang, status: &Value) {
    let mode = match (status["mode"].as_str(), status["forced"].as_str()) {
        (Some("local"), Some("local")) => say(lang, "network DNS, in clear (forced)", "DNS du réseau, en clair (imposé)"),
        (Some("local"), _) => say(lang, "network DNS, in clear", "DNS du réseau, en clair"),
        (_, Some("encrypted")) => say(lang, "encrypted (forced)", "chiffré (imposé)"),
        _ => say(lang, "encrypted", "chiffré"),
    };
    println!("{:<12}{mode}", "Mode");
    let answering = status["answering_server"].as_str();
    let servers: Vec<String> = status["servers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            let name = s["name"].as_str().unwrap_or("?");
            let mark = if Some(name) == answering {
                "●"
            } else if s["up"] == true {
                "✓"
            } else {
                "✗"
            };
            format!("{mark} {name}")
        })
        .collect();
    println!("{:<12}{}", say(lang, "Servers", "Serveurs"), servers.join("   "));
    if let Some(network) = status["network"].as_object() {
        let dns: Vec<&str> = network["dns"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        println!(
            "{:<12}{} ({}), DNS {}",
            say(lang, "Network", "Réseau"),
            network["name"].as_str().unwrap_or("?"),
            network["interface"].as_str().unwrap_or("?"),
            if dns.is_empty() { "-".to_owned() } else { dns.join(", ") }
        );
    }
    if let Some(since) = status["clear_since"].as_str() {
        println!("{:<12}{since}", say(lang, "In clear", "En clair"));
    }
    println!("{:<12}{}", say(lang, "Names", "Domaines"), status["clear_names"]);
    println!("{:<12}{}", say(lang, "Checked", "Contrôle"), status["checked"].as_str().unwrap_or("-"));
}

fn print_test(results: &Value) -> bool {
    let mut all_ok = true;
    for server in results.as_array().into_iter().flatten() {
        let name = server["name"].as_str().unwrap_or("?");
        if server["ok"] == true {
            println!("✓ {name:<28}{:>6} ms", server["ms"]);
        } else {
            all_ok = false;
            println!("✗ {name:<28}{}", server["error"].as_str().unwrap_or("error"));
        }
    }
    all_ok
}

/// `abdns check` : la configuration se lit, et chaque jeton existe.
fn check(path: &Path) -> Result<()> {
    let config = Config::read(path)?;
    let lang = config.language;
    println!("{} {}", say(lang, "Configuration OK:", "Configuration valide :"), path.display());
    for (index, server) in config.server.iter().enumerate() {
        let token = match &server.token_file {
            None => String::new(),
            Some(file) => match std::fs::metadata(file) {
                Ok(_) => format!(" ({} {})", say(lang, "token", "jeton"), file.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    bail!("{}: {} {}", server.name, say(lang, "missing token file", "fichier de jeton absent :"), file.display())
                }
                Err(_) => format!(" ({} {})", say(lang, "token, readable by root only:", "jeton, lisible par root seul :"), file.display()),
            },
        };
        println!("  {}. {}  {}{token}", index + 1, server.name, server.url.trim_end_matches('/'));
    }
    let fallback = if config.local_fallback { say(lang, "yes", "oui") } else { say(lang, "no", "non") };
    println!("  {}: {fallback}", say(lang, "local fallback", "dernier recours local"));
    Ok(())
}

/// Jeton seul, ou adresse DoH complète dont il est le dernier segment ; pour le serveur nommé,
/// ou le seul serveur qui a un `token_file`. Une ligne suffit ; service en marche : relu aussitôt.
fn store_token(config: &Config, server: Option<&str>) -> Result<()> {
    let candidates: Vec<_> = config.server.iter().filter(|s| s.token_file.is_some() && server.is_none_or(|n| s.name == n)).collect();
    let target = match candidates.as_slice() {
        [one] => *one,
        [] => bail!("no server with token_file{}", server.map(|n| format!(" named {n}")).unwrap_or_default()),
        _ => bail!("several servers have a token_file: name one (abdns token \"NAME\")"),
    };
    let path = target.token_file.as_ref().context("token_file")?;
    let text = Text(config.language);
    if std::io::stdin().is_terminal() {
        eprint!("{} ", text.token_prompt());
        std::io::stderr().flush().ok();
    }
    let input = platform::read_secret_line()?;
    let token = input.trim().trim_end_matches('/').rsplit('/').next().unwrap_or_default();
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
    println!("{}", text.token_saved(&path.display().to_string()));
    if control::request(&control::socket_path(config), &Request::Reload).is_ok() {
        println!("{}", text.token_reloaded());
    }
    Ok(())
}

fn completions(shell: &str) -> Result<()> {
    let script = match shell {
        "bash" => include_str!("../../completions/abdns.bash"),
        "zsh" => include_str!("../../completions/_abdns"),
        "fish" => include_str!("../../completions/abdns.fish"),
        other => bail!("unknown shell: {other} (bash, zsh, fish)"),
    };
    print!("{script}");
    Ok(())
}

fn logs(args: &[&str]) -> Result<()> {
    let mut follow = false;
    let mut lines = 50;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "-f" | "--follow" => follow = true,
            "-n" => lines = args.next().context("-n needs a number")?.parse()?,
            other => bail!("unknown option: {other}"),
        }
    }
    let status = platform::logs(follow, lines).status().context("cannot show the log")?;
    std::process::exit(status.code().unwrap_or(1));
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path: Option<PathBuf> = None;
    if let Some(i) = args.iter().position(|a| a == "--config") {
        ensure!(i + 1 < args.len(), "--config needs a file\n{USAGE}");
        config_path = Some(PathBuf::from(args.remove(i + 1)));
        args.remove(i);
    }
    let json = args.iter().position(|a| a == "--json").map(|i| args.remove(i)).is_some();
    let path = config_path.unwrap_or_else(config::default_path);
    let read = || Config::read(&path);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["status"] => {
            let config = read()?;
            let status = ask(&config, &Request::Status)?;
            if json {
                println!("{status:#}");
            } else {
                print_status(config.language, &status);
            }
            Ok(())
        }
        ["test"] => {
            let config = read()?;
            let results = ask(&config, &Request::Test)?;
            if json {
                println!("{results:#}");
            } else if !print_test(&results) {
                std::process::exit(1);
            }
            Ok(())
        }
        ["mode", mode] => {
            let config = read()?;
            guard::Force::parse(mode).with_context(|| format!("unknown mode: {mode} (auto, encrypted, local)"))?;
            ask(&config, &Request::Mode { mode: (*mode).to_owned() })?;
            println!("{}", Text(config.language).mode_set(mode));
            Ok(())
        }
        ["reload"] => {
            let config = read()?;
            ask(&config, &Request::Reload)?;
            println!("{}", say(config.language, "reload requested", "relecture demandée"));
            Ok(())
        }
        ["check"] => check(&path),
        ["logs", rest @ ..] => logs(rest),
        ["token"] => store_token(&read()?, None),
        ["token", server] => store_token(&read()?, Some(server)),
        ["install"] => {
            read()?;
            platform::install(&path)
        }
        ["uninstall"] => platform::uninstall(read().ok().as_ref()),
        ["completions", shell] => completions(shell),
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
