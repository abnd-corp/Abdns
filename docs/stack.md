# Stack

| Élément | Version | Rôle | Changelog |
|---|---|---|---|
| Rust (édition 2024) | MSRV 1.88 | langage | https://releases.rs |
| tokio (`rt`, `net`, `time`, `signal`, `io-util`) | 1 | runtime à un fil : relais UDP et TCP, client DoH, signaux d'arrêt ([ADR 0005](adr/0005-hyper-et-runtime-unique.md)) | https://github.com/tokio-rs/tokio/blob/master/tokio/CHANGELOG.md |
| hyper (`client`, `http1`, `http2`), hyper-util (`client-legacy`) | 1, 0.1 | client DoH : HTTP/2 ou HTTP/1.1 par ALPN, une connexion par serveur | https://github.com/hyperium/hyper/blob/master/CHANGELOG.md |
| hyper-rustls (`http1`, `http2`, `tls12`, `rustls-platform-verifier`) | 0.27 | TLS du client, certificats vérifiés par le système (trousseau macOS, magasin Windows, certificats de la distribution) | https://github.com/rustls/hyper-rustls/releases |
| http-body-util, tower-service | 0.1, 0.3 | corps de la requête et de la réponse (limitée à 64 Kio) ; résolveur à adresse fixe du client | https://github.com/hyperium/http-body/releases |
| rustls (`ring`, `std`, `tls12`) | 0.23 | TLS ; fournisseur de chiffrement ring, qui se compile partout (Windows et musl compris) sans outil externe | https://github.com/rustls/rustls/releases |
| anyhow | 1 | erreurs avec contexte | https://github.com/dtolnay/anyhow/releases |
| libc (Linux seulement) | 0.2 | socket netlink, `geteuid` | https://github.com/rust-lang/libc/releases |
| serde, serde_json | 1 | configuration, fichier d'état | https://github.com/serde-rs/serde/releases |
| toml | 0.9 | lecture de la configuration | https://github.com/toml-rs/toml/blob/main/crates/toml/CHANGELOG.md |
| nixpkgs (flake, facultatif) | unstable | paquet et module nix-darwin | https://github.com/NixOS/nixpkgs |

Toutes les dépendances sont prises sans leurs options par défaut : seul le nécessaire est activé. hyper-rustls choisirait sinon aws-lc-rs (plus gros, outil de compilation externe) et les certificats de Mozilla embarqués ; aucun proxy du système n'est utilisé.

Outils système macOS appelés : `route`, `ipconfig`, `networksetup`, `osascript`, `launchctl`, `stat`, `open`. Changements de réseau : notify(3) de libSystem (`kNotifySCNetworkChange`), sans dépendance ; `route`, `ipconfig` et `networksetup` ne tournent qu'à un changement, ou toutes les 60 s par sécurité.

Outils système Linux appelés : `ip`, `resolvectl` (systemd-resolved), `nmcli` (NetworkManager, facultatif), `systemctl`, `loginctl`, `runuser`, `journalctl`, `notify-send` (libnotify), `xdg-open` (xdg-utils). Changements de réseau : socket netlink du noyau (liens, adresses, routes), par libc ; `ip` et `nmcli` ne tournent qu'à un changement, ou toutes les 60 s par sécurité. `resolvectl dns` garde son contrôle toutes les 5 s : netlink ne voit pas NetworkManager défaire le réglage ([ADR 0004](adr/0004-support-linux.md)).

Outils de publication : `lipo` (binaire macOS universel), `gh` (versions GitHub), Homebrew (`packaging/homebrew/abdns.rb`), `installShellFiles` (Nix).
