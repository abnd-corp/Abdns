# Stack

| Élément | Version | Rôle | Changelog |
|---|---|---|---|
| Rust (édition 2024) | MSRV 1.88 | langage | https://releases.rs |
| anyhow | 1 | erreurs avec contexte | https://github.com/dtolnay/anyhow/releases |
| ctrlc (`termination`) | 3 | arrêt propre sur SIGTERM, SIGINT et événements console Windows | https://github.com/Detegr/rust-ctrlc/releases |
| serde, serde_json | 1 | configuration, fichier d'état | https://github.com/serde-rs/serde/releases |
| toml | 0.9 | lecture de la configuration | https://github.com/toml-rs/toml/blob/main/crates/toml/CHANGELOG.md |
| dnsproxy (AdGuard), programme externe | 0.86 essayé | DoH sortant, cache, secours | https://github.com/AdguardTeam/dnsproxy/releases |
| nixpkgs (flake, facultatif) | unstable | paquet et module nix-darwin | https://github.com/NixOS/nixpkgs |

Options de dnsproxy utilisées : `--listen`, `--port`, `--upstream` et `--fallback` (fichiers), `--bootstrap`, `--timeout`, `--cache`, `--cache-optimistic`, `--cache-size`, `--pending-requests-enabled`. À revérifier à chaque version majeure de dnsproxy.

Outils système macOS appelés : `route`, `ipconfig`, `networksetup`, `osascript`, `launchctl`, `stat`, `open`.
