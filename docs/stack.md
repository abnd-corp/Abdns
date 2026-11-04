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

Linux, par le noyau seul (libc) : changements de réseau par netlink (liens, adresses, routes) ; route par défaut dans `/proc/net/route` et `/proc/net/ipv6_route` ; DNS du réseau dans `resolv.conf` ; DNS du poste par une table du pare-feu nf_tables, parlée par netlink et retirée par le noyau avec le processus ([ADR 0007](adr/0007-linux-pare-feu-du-noyau.md)). Outils appelés : `systemctl`, `loginctl`, `runuser`, `journalctl` (systemd), `notify-send` et `xdg-open` pour la session de l'utilisateur.

Outils de publication : `lipo` (binaire macOS universel), `gh` (versions GitHub), Homebrew (`packaging/homebrew/abdns.rb`), `installShellFiles` (Nix).

## Structure du code

Un module par sujet ; tout ce qui dépend du système est dans `platform/`.

```
src/
  bin/abdns.rs        l'outil : commandes et affichage
  bin/abdnsd.rs       le service : démarrage et arrêt
  config.rs           lecture et validation de la configuration
  control.rs          canal de commande entre abdns et abdnsd
  dns.rs              format des messages DNS
  i18n.rs             textes en anglais et en français
  logbook.rs          journal et domaines envoyés en clair
  relay.rs            écoute UDP et TCP, routage vers le chiffré ou le réseau
  rt.rs               runtime d'entrées-sorties à un fil, signaux d'arrêt
  resolver/           client DoH : ordre et santé des serveurs (mod), connexion HTTPS (https), cache
  guard/              la garde : chiffré ou en clair (mod), réseau, notifications, sondes, relecture
  platform/           mod.rs (interface), other.rs (Windows, à venir)
    macos/            réseau, DNS du poste, session de l'utilisateur, service launchd
    linux/            réseau (/proc, resolv.conf), DNS du poste (pare-feu nf_tables), session graphique, service systemd
```

