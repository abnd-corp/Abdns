# Stack

| Élément | Version | Rôle | Changelog |
|---|---|---|---|
| Rust (édition 2024) | MSRV 1.88 | langage | https://releases.rs |
| reqwest (`blocking`, `rustls-no-provider`, `http2`, sans les options par défaut) | 0.13 | client DoH : TLS par rustls avec le vérificateur de la plateforme (trousseau macOS), HTTP/2, adresse imposée par `resolve` | https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md |
| rustls (`ring`, `std`, `tls12`) | 0.23 | TLS ; fournisseur de chiffrement ring, installé au démarrage, qui se compile partout (Windows et musl compris) sans outil externe | https://github.com/rustls/rustls/releases |
| anyhow | 1 | erreurs avec contexte | https://github.com/dtolnay/anyhow/releases |
| ctrlc (`termination`) | 3 | arrêt propre sur SIGTERM, SIGINT et événements console Windows | https://github.com/Detegr/rust-ctrlc/releases |
| serde, serde_json | 1 | configuration, fichier d'état | https://github.com/serde-rs/serde/releases |
| toml | 0.9 | lecture de la configuration | https://github.com/toml-rs/toml/blob/main/crates/toml/CHANGELOG.md |
| nixpkgs (flake, facultatif) | unstable | paquet et module nix-darwin | https://github.com/NixOS/nixpkgs |

reqwest 0.13 : `rustls` est le TLS par défaut, avec aws-lc-rs ; `rustls-no-provider` laisse choisir ring (ancien `rustls-tls`), les options de certificats racines ont disparu au profit de `rustls-platform-verifier`, et `system-proxy` n'est pas activé (aucun proxy du système n'est utilisé).

Outils système macOS appelés : `route`, `ipconfig`, `networksetup`, `osascript`, `launchctl`, `stat`, `open`.

Outils système Linux appelés : `ip`, `resolvectl` (systemd-resolved), `nmcli` (NetworkManager, facultatif), `systemctl`, `loginctl`, `runuser`, `id`, `journalctl`, `notify-send` (libnotify), `xdg-open` (xdg-utils).

Outils de publication : `lipo` (binaire macOS universel), `gh` (versions GitHub), Homebrew (`packaging/homebrew/abdns.rb`), `installShellFiles` (Nix).
