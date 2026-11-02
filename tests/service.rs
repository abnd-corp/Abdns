//! Bout en bout : le vrai service `abdnsd` est lancé (`--no-system`, sans toucher au DNS du poste),
//! interrogé comme le ferait un programme du poste, piloté par l'outil `abdns` par la socket de
//! commande, puis arrêté par SIGTERM.
//!
//! Le serveur DoH de test est un port fermé : aucune réponse chiffrée n'arrive jamais, ce qui
//! exerce le chemin d'échec complet (SERVFAIL au client, serveur marqué muet, aucun repli en clair).
//! Un serveur DoH factice demanderait un certificat de confiance : voir docs/tests.md.
#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{Dir, SERVICE, free_port, is_response, query, rcode, reply_id, run, wait_for, write_config};

/// Le service est arrêté quoi qu'il arrive au test.
struct Service(Child);

impl Drop for Service {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

fn start(config: &Path, stderr: &Path) -> Service {
    let child = Command::new(SERVICE)
        .arg("--config")
        .arg(config)
        .arg("--no-system")
        // Sans PATH : pas de commande système (réseau, notification) ; le test ne dépend pas du poste.
        .env("PATH", "/nonexistent")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(stderr).unwrap())
        .spawn()
        .expect("service starts");
    Service(child)
}

fn udp_reply(port: u16, id: u16) -> Option<Vec<u8>> {
    let socket = UdpSocket::bind("127.0.0.1:0").ok()?;
    socket.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    socket.send_to(&query(id, "example.com"), ("127.0.0.1", port)).ok()?;
    let mut buf = vec![0; 4096];
    let n = socket.recv(&mut buf).ok()?;
    buf.truncate(n);
    Some(buf)
}

fn tcp_reply(port: u16, id: u16) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let msg = query(id, "example.com");
    stream.write_all(&(msg.len() as u16).to_be_bytes()).ok()?;
    stream.write_all(&msg).ok()?;
    let mut len = [0; 2];
    stream.read_exact(&mut len).ok()?;
    let mut reply = vec![0; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut reply).ok()?;
    Some(reply)
}

/// `abdns --json status`, par la socket du service.
fn status(config: &Path) -> Option<serde_json::Value> {
    let out = run(config, &["--json", "status"], "");
    out.status.success().then(|| serde_json::from_slice(&out.stdout).ok()).flatten()
}

fn log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn service_lifecycle() {
    let dir = Dir::new("service");
    let port = free_port();
    let config = write_config(&dir, port, "dead");
    let stderr = dir.path("service.log");
    let mut service = start(&config, &stderr);

    // 1. Le relais répond tout de suite, en UDP et en TCP : SERVFAIL, puisqu'aucun serveur ne répond.
    let reply = wait_for(15, || udp_reply(port, 0x4242)).expect("the relay answers over UDP");
    assert_eq!(reply_id(&reply), 0x4242);
    assert!(is_response(&reply));
    assert_eq!(rcode(&reply), 2, "SERVFAIL expected");
    let reply = tcp_reply(port, 0x0707).expect("the relay answers over TCP");
    assert_eq!((reply_id(&reply), rcode(&reply)), (0x0707, 2));
    // Un paquet trop court est ignoré sans faire tomber le relais.
    UdpSocket::bind("127.0.0.1:0").unwrap().send_to(&[1, 2, 3], ("127.0.0.1", port)).unwrap();
    assert!(udp_reply(port, 0x0001).is_some());

    // 2. La garde écrit l'état (après l'attente de démarrage de 15 s) ; l'outil le lit par la socket.
    let state = wait_for(40, || status(&config)).expect("abdns status answers");
    assert_eq!(state["mode"], "encrypted");
    assert_eq!(state["forced"], "auto");
    assert_eq!(state["local_fallback"], false);
    assert_eq!(state["servers"][0]["name"], "dead");
    assert_eq!(state["servers"][0]["up"], false);
    assert!(state["answering_server"].is_null());
    assert_eq!(state["clear_names"], 0);
    assert!(dir.path("state/control.sock").exists());
    let human = run(&config, &["status"], "");
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("✗ dead"));

    // 3. `abdns test` interroge chaque serveur : celui-ci est muet, la commande le dit et échoue.
    let tested = run(&config, &["test"], "");
    assert!(!tested.status.success());
    assert!(String::from_utf8_lossy(&tested.stdout).contains("✗ dead"), "{}", String::from_utf8_lossy(&tested.stdout));

    // 4. Le mode forcé passe par la socket. « local » n'est pas affirmé sur le mode atteint : il
    //    dépend de la présence d'un DNS du réseau sur la machine d'essai ; « encrypted » est
    //    déterministe et ramène toujours au chiffré.
    assert!(run(&config, &["mode", "local"], "").status.success());
    wait_for(15, || status(&config).filter(|s| s["forced"] == "local")).expect("forced local is seen");
    assert!(run(&config, &["mode", "encrypted"], "").status.success());
    let forced = wait_for(15, || status(&config).filter(|s| s["forced"] == "encrypted")).expect("forced encrypted is seen");
    assert_eq!(forced["mode"], "encrypted");
    assert!(run(&config, &["mode", "auto"], "").status.success());
    wait_for(15, || status(&config).filter(|s| s["forced"] == "auto")).expect("auto is back");

    // 5. `abdns reload` relit la configuration tout de suite : un nouveau nom de serveur apparaît.
    write_config(&dir, port, "renamed");
    assert!(run(&config, &["reload"], "").status.success());
    wait_for(10, || log(&stderr).contains("configuration reloaded; servers: renamed").then_some(())).expect("reloaded");
    wait_for(15, || status(&config).filter(|s| s["servers"][0]["name"] == "renamed")).expect("new server in status");

    // 6. Une configuration invalide est ignorée (relue d'après la date du fichier), l'ancienne reste.
    std::fs::write(&config, "this is [ not valid").unwrap();
    wait_for(20, || log(&stderr).contains("configuration ignored").then_some(())).expect("invalid config reported");
    assert!(udp_reply(port, 0x0002).is_some(), "the relay still answers");
    let socket_state = std::fs::read_to_string(dir.path("state/status.json")).unwrap();
    assert!(socket_state.contains("\"name\": \"renamed\""), "{socket_state}");

    // 7. SIGTERM : arrêt propre, code 0, la socket de commande est retirée.
    let pid = service.0.id().to_string();
    assert!(Command::new("kill").args(["-TERM", &pid]).status().unwrap().success());
    let exit = wait_for(15, || service.0.try_wait().ok().flatten()).expect("the service stops");
    assert!(exit.success(), "clean exit, got {exit}");
    assert!(log(&stderr).contains("stopped"));
    assert!(!dir.path("state/control.sock").exists(), "the control socket is removed");
}

#[test]
fn address_already_in_use_stops_the_service() {
    let dir = Dir::new("busy");
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = udp.local_addr().unwrap().port();
    let _tcp = TcpListener::bind(("127.0.0.1", port)).ok();
    let config = write_config(&dir, port, "dead");
    let out = Command::new(SERVICE).arg("--config").arg(&config).arg("--no-system").env("PATH", "/nonexistent").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot listen"));
}

#[test]
fn service_arguments_are_checked() {
    let out = Command::new(SERVICE).arg("--bogus").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown argument"));
    let out = Command::new(SERVICE).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage: abdnsd"));
}

#[test]
fn ready_line_names_addresses_and_servers() {
    let dir = Dir::new("ready");
    let port = free_port();
    let config = write_config(&dir, port, "dead");
    let stderr = dir.path("service.log");
    let _service = start(&config, &stderr);
    wait_for(15, || udp_reply(port, 1)).expect("up");
    let ready = wait_for(30, || log(&stderr).lines().find(|l| l.contains("ready on")).map(str::to_owned)).expect("ready line");
    assert!(ready.contains(&format!("127.0.0.1:{port}")) && ready.contains("dead"), "{ready}");
}
