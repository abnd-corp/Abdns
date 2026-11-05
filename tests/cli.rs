//! Intégration : les commandes de l'outil `abdns` telles qu'elles sont lancées, sans service en route.
#![cfg(unix)]

mod common;

use common::{Dir, free_port, run, write_config};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn version_and_help() {
    let dir = Dir::new("version");
    let config = write_config(&dir, free_port(), "a");
    let version = run(&config, &["version"], "");
    assert!(version.status.success());
    assert_eq!(text(&version.stdout).trim(), format!("abdns {}", env!("CARGO_PKG_VERSION")));
    for flag in ["help", "--help", "-h"] {
        let help = run(&config, &[flag], "");
        assert!(help.status.success(), "{flag}");
        assert!(text(&help.stdout).contains("Usage: abdns"), "{flag}");
    }
}

#[test]
fn the_service_has_its_own_version() {
    let out = std::process::Command::new(common::SERVICE).arg("--version").output().expect("abdnsd");
    assert!(out.status.success());
    assert_eq!(text(&out.stdout).trim(), format!("abdnsd {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn unknown_command_exits_with_2_and_shows_usage() {
    let dir = Dir::new("unknown");
    let config = write_config(&dir, free_port(), "a");
    let out = run(&config, &["frobnicate"], "");
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("Usage: abdns"));
}

#[test]
fn config_flag_needs_a_file() {
    let out = std::process::Command::new(common::TOOL).arg("--config").output().expect("abdns");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("--config needs a file"));
}

#[test]
fn check_accepts_a_valid_configuration_and_lists_the_servers() {
    let dir = Dir::new("check-ok");
    let config = write_config(&dir, free_port(), "mine");
    let out = run(&config, &["check"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    assert!(shown.contains("Configuration OK"), "{shown}");
    assert!(shown.contains("1. mine"), "{shown}");
}

#[test]
fn check_refuses_invalid_configurations_with_the_reason() {
    let dir = Dir::new("check-bad");
    let path = dir.path("config.toml");
    for (content, reason) in [
        ("local_fallback = true\n", "missing field `server`"),
        ("[[server]]\nname = \"a\"\nurl = \"https://dns.example.com/q\"\n", "ip is required"),
        ("[[server]]\nname = \"a\"\nurl = \"http://203.0.113.5/q\"\n", "https://"),
        ("[[server]]\nname = \"a\"\nurl = \"https://203.0.113.5/q\"\nbogus = 1\n", "bogus"),
        ("[[server]]\nname = \"a\"\nurl = \"https://203.0.113.5/q\"\ntoken_file = \"/t\"\n", "must end with /"),
        ("not toml at all [", "invalid"),
    ] {
        std::fs::write(&path, content).unwrap();
        let out = run(&path, &["check"], "");
        assert!(!out.status.success(), "{content}");
        assert!(text(&out.stderr).contains(reason), "{content}: {}", text(&out.stderr));
    }
}

#[test]
fn check_reports_a_missing_token_file_and_a_missing_config() {
    let dir = Dir::new("check-token");
    let path = dir.path("config.toml");
    let token = dir.path("absent.token");
    std::fs::write(
        &path,
        format!("[[server]]\nname = \"mine\"\nurl = \"https://203.0.113.5/dns-query/\"\ntoken_file = \"{}\"\n", token.display()),
    )
    .unwrap();
    let out = run(&path, &["check"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("missing token file"), "{}", text(&out.stderr));

    let out = run(&dir.path("nope.toml"), &["check"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("cannot read"));
}

#[test]
fn the_example_configuration_passes_check() {
    let dir = Dir::new("example");
    let example = dir.path("example.toml");
    std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.toml"), &example).unwrap();
    let out = run(&example, &["check"], "");
    assert!(out.status.success(), "{}", text(&out.stderr));
}

#[test]
fn commands_that_need_the_service_say_so_when_it_is_not_running() {
    let dir = Dir::new("no-service");
    let config = write_config(&dir, free_port(), "a");
    for args in [&["mode", "local"][..], &["reload"], &["test"]] {
        let out = run(&config, args, "");
        assert!(!out.status.success(), "{args:?}");
        assert!(text(&out.stderr).contains("is abdnsd running"), "{args:?}: {}", text(&out.stderr));
    }
    // `status` essaie aussi le fichier d'état avant d'abandonner.
    let out = run(&config, &["status"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("unreadable too"), "{}", text(&out.stderr));
}

#[test]
fn mode_rejects_an_unknown_mode_before_asking_the_service() {
    let dir = Dir::new("mode");
    let config = write_config(&dir, free_port(), "a");
    let out = run(&config, &["mode", "sideways"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("unknown mode"));
}

#[test]
fn status_falls_back_to_the_state_file_in_both_formats() {
    let dir = Dir::new("status-file");
    let config = write_config(&dir, free_port(), "a");
    std::fs::create_dir_all(dir.path("state")).unwrap();
    std::fs::write(
        dir.path("state/status.json"),
        r#"{"mode":"encrypted","forced":"auto","answering_server":"Mullvad","servers":[{"name":"Mullvad","up":true},{"name":"Cloudflare","up":false}],"clear_names":0,"checked":"2026-11-02 18:00:00 UTC"}"#,
    )
    .unwrap();
    let human = run(&config, &["status"], "");
    assert!(human.status.success(), "{}", text(&human.stderr));
    let shown = text(&human.stdout);
    assert!(shown.contains("encrypted") && shown.contains("● Mullvad") && shown.contains("✗ Cloudflare"), "{shown}");

    let json = run(&config, &["--json", "status"], "");
    let value: serde_json::Value = serde_json::from_str(&text(&json.stdout)).expect("JSON output");
    assert_eq!(value["answering_server"], "Mullvad");
}

fn token_config(dir: &Dir, servers: &[(&str, bool)]) -> std::path::PathBuf {
    let mut text = format!("state_dir = \"{}\"\n", dir.path("state").display());
    for (name, with_token) in servers {
        text += &format!("\n[[server]]\nname = \"{name}\"\nurl = \"https://203.0.113.5/dns-query/\"\n");
        if *with_token {
            text += &format!("token_file = \"{}\"\n", dir.path(&format!("{name}.token")).display());
        }
    }
    let path = dir.path("config.toml");
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn token_is_extracted_from_a_full_url_and_stored_privately() {
    let dir = Dir::new("token");
    let config = token_config(&dir, &[("mine", true)]);
    let out = run(&config, &["token"], "https://203.0.113.5/dns-query/abcdefghijklmnop0123456789\n");
    assert!(out.status.success(), "{}", text(&out.stderr));
    let file = dir.path("mine.token");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "abcdefghijklmnop0123456789\n");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    // Le jeton n'est jamais répété dans ce que le programme affiche.
    assert!(!text(&out.stdout).contains("abcdefghijklmnop0123456789"));
    assert!(!text(&out.stderr).contains("abcdefghijklmnop0123456789"));
}

#[test]
fn token_needs_one_line_not_the_end_of_input() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let dir = Dir::new("token-line");
    let config = token_config(&dir, &[("mine", true)]);
    let mut child =
        Command::new(common::TOOL).arg("--config").arg(&config).arg("token").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
    // Entrée laissée ouverte, comme un terminal après Entrée.
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"abcdefghijklmnop0123456789\n").unwrap();
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(5) {
            child.kill().ok();
            panic!("abdns token still waits for the end of input");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(dir.path("mine.token")).unwrap(), "abcdefghijklmnop0123456789\n");
}

#[test]
fn token_rejects_bad_input_and_ambiguous_servers() {
    let dir = Dir::new("token-bad");
    let config = token_config(&dir, &[("mine", true)]);
    for bad in ["", "short", "abcdefghijklmnop/../q\n"] {
        let out = run(&config, &["token"], bad);
        assert!(!out.status.success(), "{bad:?}");
        assert!(text(&out.stderr).contains("no valid token"), "{bad:?}");
    }
    assert!(!dir.path("mine.token").exists());

    let none = token_config(&dir, &[("plain", false)]);
    let out = run(&none, &["token"], "abcdefghijklmnop0123456789");
    assert!(text(&out.stderr).contains("no server with token_file"));

    let two = token_config(&dir, &[("one", true), ("two", true)]);
    let out = run(&two, &["token"], "abcdefghijklmnop0123456789");
    assert!(text(&out.stderr).contains("several servers"));
    let out = run(&two, &["token", "two"], "abcdefghijklmnop0123456789");
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(dir.path("two.token").exists() && !dir.path("one.token").exists());
}

#[test]
fn completions_exist_for_the_three_shells_and_reject_others() {
    let dir = Dir::new("completions");
    let config = write_config(&dir, free_port(), "a");
    for shell in ["bash", "zsh", "fish"] {
        let out = run(&config, &["completions", shell], "");
        assert!(out.status.success(), "{shell}");
        assert!(text(&out.stdout).contains("abdns"), "{shell}");
    }
    let out = run(&config, &["completions", "powershell"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("unknown shell"));
}

#[test]
fn logs_rejects_unknown_options() {
    let dir = Dir::new("logs");
    let config = write_config(&dir, free_port(), "a");
    let out = run(&config, &["logs", "--bogus"], "");
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("unknown option"));
}
