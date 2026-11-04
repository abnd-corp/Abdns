# Tests

`make check` lance tout ce que la CI lance : formatage, Clippy (avertissements bloquants) et `cargo test`. Les tests s'exécutent sans être root et sans toucher au DNS du poste.

| Niveau | Où | Ce qu'il vérifie |
|---|---|---|
| **Unitaire** | `#[cfg(test)]` dans les modules de `src/` | Fonctions pures : messages DNS (dont 200 000 messages aléatoires ou abîmés), verrous repris après une panique, cache et durées de vie, configuration, routage des noms, limiteur de notifications, analyse des sorties des outils système (`ip`, `nmcli`, `resolvectl`, `ipconfig`…), unité systemd, plist launchd |
| **Intégration** | `src/relay.rs` (tests), `tests/cli.rs` | Le relais en processus avec un faux DNS du réseau (UDP, TCP, réponse forgée ignorée, délai dépassé, noms locaux non signalés, SERVFAIL sans fuite) ; l'outil `abdns` lancé pour de vrai (`version`, `help`, `check`, `status` et son repli sur le fichier d'état, `token`, `completions`, erreurs quand le service ne tourne pas) |
| **Bout en bout** | `tests/service.rs` | Le vrai service `abdnsd` (`--no-system`) est démarré, interrogé en UDP et en TCP, piloté par l'outil `abdns` par la socket de commande (`status`, `test`, `mode`, `reload`) et par sa configuration (rechargée à chaud, invalide ignorée), puis arrêté par SIGTERM (socket retirée) |

## Lancer

```sh
make check                      # tout
cargo test --test cli           # l'outil abdns (rapide)
cargo test --test service       # abdnsd de bout en bout (~30 s : 15 s d'attente de démarrage)
cargo test relay::              # le relais
sudo cargo test --lib -- --ignored netfilter   # Linux, root : la table du pare-feu sur le vrai noyau
```

Le test `netfilter` est le seul qui demande root : il pose la table `abdns` sur le vrai noyau, vérifie qu'une autre socket ne peut pas la retirer et que le noyau la retire avec la socket qui l'a créée. La CI le lance sous Linux (`sudo` comme lanceur des tests).

Les tests d'intégration sont réservés aux systèmes Unix (socket de commande, `kill`) ; ils lancent les programmes avec `PATH` vidé : aucune commande système (réseau, notification) n'est appelée, le résultat ne dépend pas du poste. Le serveur DoH de test est un port fermé.

## Banc de mesure

`make bench` compile en release et lance `tests/bench.py` : abdnsd en `--no-system` sur le port 5399, avec deux serveurs DoH publics (Internet requis). Il mesure la taille du binaire, la mémoire et les fils au repos, puis le débit et le processeur sur des réponses en cache, sur 1 500 noms inédits et sur une rafale de 30 noms. Les chiffres dépendent du poste et du réseau : on compare avant et après un changement, sur la même machine.

Référence (MacBook arm64, Wi-Fi) :

| Mesure | 0.2.0 | Profil de taille (#36) | hyper et runtime à un fil (#37) |
|---|---|---|---|
| Binaire `abdnsd` | 4,27 Mio | 2,20 Mio | 1,82 Mio |
| Binaire `abdns` | 0,90 Mio | 0,58 Mio | 0,58 Mio |
| Mémoire au repos (RSS) | 11,6 Mio | 10,1 Mio | 9,2 Mio |
| Fils au repos | 9 | 9 | 5 |
| Cache : processeur par réponse | 54 µs | 56 µs | 15 µs |
| Noms inédits : processeur par requête | 0,54 ms | 0,42 ms | 0,27 ms |
| Rafale de 30 noms inédits | 284 ms | 173 ms | 130 à 160 ms |

Le débit et la rafale varient d'une mesure à l'autre avec le réseau ; seuls les écarts nets comptent.

## Ce qui n'est pas couvert

- **Un serveur DoH qui répond.** Il demande un faux serveur TLS, donc une autorité de confiance pour le client (qui utilise le magasin de certificats du système). À prévoir : une option de test pour ajouter un certificat racine, décidée dans une issue.
- **La machine d'états de la garde** (bascule en clair, retour, portail, notifications). Elle mélange décisions et effets (réseau, fichiers, système) : il faut séparer la décision de ses effets pour la tester sans réseau.
- **Les fonctions qui modifient le système** (`set_system_dns`, `install`, `uninstall`, `notify`, `open`) : seules leurs parties pures sont testées. À vérifier à la main sur un vrai poste, ou dans une machine virtuelle.
- **Windows** : pas encore pris en charge.
