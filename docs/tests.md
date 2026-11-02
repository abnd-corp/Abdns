# Tests

`make check` lance tout ce que la CI lance : formatage, Clippy (avertissements bloquants) et `cargo test`. Les tests s'exécutent sans être root et sans toucher au DNS du poste.

| Niveau | Où | Ce qu'il vérifie |
|---|---|---|
| **Unitaire** | `#[cfg(test)]` dans `src/*.rs` | Fonctions pures : messages DNS, cache et durées de vie, configuration, routage des noms, limiteur de notifications, analyse des sorties des outils système (`ip`, `nmcli`, `resolvectl`, `ipconfig`…), unité systemd, plist launchd |
| **Intégration** | `src/relay.rs` (tests), `tests/cli.rs` | Le relais en processus avec un faux DNS du réseau (UDP, TCP, réponse forgée ignorée, délai dépassé, noms locaux non signalés, SERVFAIL sans fuite) ; l'outil `abdns` lancé pour de vrai (`version`, `help`, `check`, `status` et son repli sur le fichier d'état, `token`, `completions`, erreurs quand le service ne tourne pas) |
| **Bout en bout** | `tests/service.rs` | Le vrai service `abdnsd` (`--no-system`) est démarré, interrogé en UDP et en TCP, piloté par l'outil `abdns` par la socket de commande (`status`, `test`, `mode`, `reload`) et par sa configuration (rechargée à chaud, invalide ignorée), puis arrêté par SIGTERM (socket retirée) |

## Lancer

```sh
make check                      # tout
cargo test --test cli           # l'outil abdns (rapide)
cargo test --test service       # abdnsd de bout en bout (~30 s : 15 s d'attente de démarrage)
cargo test relay::              # le relais
```

Les tests d'intégration sont réservés aux systèmes Unix (socket de commande, `kill`) ; ils lancent les programmes avec `PATH` vidé : aucune commande système (réseau, notification) n'est appelée, le résultat ne dépend pas du poste. Le serveur DoH de test est un port fermé.

## Ce qui n'est pas couvert

- **Un serveur DoH qui répond.** Il demande un faux serveur TLS, donc une autorité de confiance pour le client (qui utilise le magasin de certificats du système). À prévoir : une option de test pour ajouter un certificat racine, décidée dans une issue.
- **La machine d'états de la garde** (bascule en clair, retour, portail, notifications). Elle mélange décisions et effets (réseau, fichiers, système) : il faut séparer la décision de ses effets pour la tester sans réseau.
- **Les fonctions qui modifient le système** (`set_system_dns`, `install`, `uninstall`, `notify`, `open`) : seules leurs parties pures sont testées. À vérifier à la main sur un vrai poste, ou dans une machine virtuelle.
- **Windows** : pas encore pris en charge.
