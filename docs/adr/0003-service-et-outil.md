# 0003 — Un service et un outil, installation propre

Date : 2026-11-02 · Statut : accepté

## Contexte

Un seul programme faisait tout : service, commandes, installation. Les commandes qui changent l'état (`mode`) écrivaient elles-mêmes dans le dossier du service, donc en administrateur, et l'installation passait par Nix ou par `make`. On veut une application qui s'installe et se pilote comme OpenSSH ou ffmpeg.

## Décision

- **Deux programmes** : `abdnsd`, le service (administrateur : port 53, DNS du système), et `abdns`, l'outil de l'utilisateur. Le code commun est une bibliothèque (`src/lib.rs`).
- **Canal de commande** : socket Unix `state_dir/control.sock`, mode 0660, groupe des administrateurs (`admin` sur macOS ; `sudo`, `wheel` ou `admin` sur Linux), une requête JSON par ligne. Commandes : `status`, `mode`, `reload`, `test`. Un utilisateur ordinaire lit l'état dans `status.json`. Si la socket ne peut pas être créée, le DNS continue sans elle.
- **Commandes** : `check` (validation, comme `sshd -t`), `test` (temps de réponse de chaque serveur), `logs`, `completions` ; pages de manuel `abdns(1)` et `abdnsd(8)`.
- **TLS** : fournisseur de chiffrement ring plutôt qu'aws-lc-rs, pour compiler sur Windows et en musl sans CMake ni NASM.
- **Installation** : Homebrew (formule qui compile depuis les sources, service `brew services` en administrateur), archive macOS universelle publiée par la CI à chaque étiquette avec sa somme SHA-256, `make install`, module nix-darwin. Pas de paquet `.pkg` : sans compte développeur Apple, il ne serait ni signé ni notarié.

## Conséquences

- Les administrateurs pilotent le service sans `sudo` ; seuls `token`, `install` et `uninstall` en demandent.
- Les binaires ne sont pas notariés : téléchargés par un navigateur, macOS les met en quarantaine (`curl` ou Homebrew évitent le problème).
- Linux (systemd, `.deb`, `.rpm`) et Windows (service, winget) se branchent sur `src/platform/` et sur le canal de commande (canal nommé sous Windows).
