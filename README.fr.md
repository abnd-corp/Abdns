# abdns — DNS chiffré pour votre ordinateur, compatible avec les portails captifs

[![CI](https://github.com/abnd-corp/Abdns/actions/workflows/ci.yml/badge.svg?branch=dev)](https://github.com/abnd-corp/Abdns/actions/workflows/ci.yml)

[English](README.md) · **Français**

**abdns est un relais DNS-over-HTTPS (DoH) local pour macOS : il envoie chaque requête DNS d'abord à votre propre résolveur, passe à des résolveurs chiffrés respectueux de la vie privée s'il ne répond pas, et n'utilise le DNS du réseau qu'en dernier recours, par exemple pour se connecter au portail captif du Wi-Fi d'un train, d'un hôtel ou d'un aéroport. Chaque domaine envoyé en clair vous est signalé.**

Il est écrit en Rust, tourne comme un petit service système et s'appuie sur [AdGuard dnsproxy](https://github.com/AdguardTeam/dnsproxy) pour la partie chiffrée. Linux et Windows sont prévus.

## Pourquoi

- **Votre résolveur, pas celui d'un autre.** Le poste interroge votre résolveur DoH auto-hébergé (Unbound, Knot Resolver, AdGuard Home, Pi-hole derrière un frontal DoH…), même joignable par sa seule adresse IP.
- **Pas de censure des résolveurs publics.** Dans plusieurs pays, les grands résolveurs publics appliquent des blocages ordonnés par la justice ou se retirent : en France, Google, Cloudflare, Cisco OpenDNS et DNS4EU ont reçu des ordres de blocage, et Quad9 cesse de servir les utilisateurs français le 24 novembre 2026.
- **Des secours qui restent chiffrés.** Si votre résolveur tombe, les requêtes vont à des résolveurs sans journaux ni filtre, hors de ces juridictions, joints par adresse IP fixe : aucune requête en clair pour les trouver.
- **Les portails captifs fonctionnent.** Les réglages de DNS chiffré natifs cassent la plupart des portails. abdns détecte la situation, utilise le DNS du réseau juste le temps nécessaire, ouvre la page de connexion, puis revient au chiffré dès qu'il répond.
- **Rien en clair à votre insu.** Tant que le DNS du réseau est utilisé, chaque domaine demandé est affiché dans une notification et écrit dans un journal.

## Fonctionnement

```
applications ──► abdns (127.0.0.1:53) ──► dnsproxy ──DoH──► votre résolveur
                    │                         └──DoH──► Mullvad · DNS.SB · Digitale Gesellschaft
                    ├── noms locaux (.local, .lan, inverse de 192.168.x.x…) ──► DNS du réseau, sans notification
                    └── dernier recours : DNS du réseau (DHCP), en clair, signalé
```

| Situation | Réaction d'abdns |
|---|---|
| Votre résolveur répond | Toutes les requêtes y vont en DoH |
| Votre résolveur est en panne | dnsproxy passe aux secours chiffrés |
| Tous les serveurs chiffrés échouent deux fois de suite **et** le DNS du réseau répond (portail captif, DoH bloqué) | Bascule sur le DNS du réseau, vous prévient, signale les domaines toutes les 30 s, ouvre la page de connexion si un portail est détecté |
| Le DNS chiffré répond de nouveau | Retour au chiffré, avec la durée passée en clair et le nombre de domaines envoyés |
| Toujours en clair après 15 min | Rappel toutes les 15 min |
| Nom du réseau local (`nas.lan`, `imprimante.local`, `box.home.arpa`, domaine DHCP, recherche inverse d'une adresse privée) | Toujours envoyé au DNS du réseau, jamais signalé |
| Échec DNSSEC (SERVFAIL) | Reste une erreur : jamais une raison de passer en clair |
| abdns s'arrête ou plante | Le DNS du poste est remis sur celui du réseau : la machine n'est jamais sans DNS |

## Installation (macOS)

### Sans Nix

1. Installer dnsproxy depuis ses [versions officielles](https://github.com/AdguardTeam/dnsproxy/releases) (`darwin-arm64` pour Apple Silicon, `darwin-amd64` pour Intel) :
   ```sh
   curl -fsSLO https://github.com/AdguardTeam/dnsproxy/releases/download/v0.86.0/dnsproxy-darwin-arm64-v0.86.0.tar.gz
   tar -xzf dnsproxy-darwin-arm64-v0.86.0.tar.gz
   sudo install -m 755 darwin-arm64/dnsproxy /usr/local/bin/dnsproxy
   ```
2. Écrire la configuration, au minimum votre résolveur :
   ```sh
   sudo mkdir -p /etc/abdns
   sudo cp config.example.toml /etc/abdns/config.toml
   sudo nano /etc/abdns/config.toml     # renseigner [upstream] url
   ```
3. Compiler et installer le service (Rust nécessaire) :
   ```sh
   make release
   sudo make install
   ```

`sudo abdns uninstall` arrête et retire le service, et remet le DNS du réseau.

### Avec nix-darwin

```nix
# flake.nix
inputs.abdns = { url = "github:abnd-corp/Abdns"; inputs.nixpkgs.follows = "nixpkgs"; };
# modules
abdns.darwinModules.default
{
  services.abdns = {
    enable = true;
    language = "fr";
    upstream.url = "https://203.0.113.5/dns-query";
    networkServices = [ "Wi-Fi" "USB 10/100/1000 LAN" ];
  };
}
```

### Chemin DoH secret (facultatif)

Si votre résolveur protège son adresse DoH par un chemin secret (`https://203.0.113.5/dns-query/<jeton>`), gardez le jeton hors de la configuration : `upstream.url` reçoit le préfixe terminé par `/`, `upstream.token_file` (`tokenFile` avec Nix) le fichier du jeton, puis :

```sh
printf '%s' 'https://203.0.113.5/dns-query/<jeton>' | sudo abdns token
```

Le jeton est rangé lisible par l'administrateur seul, transmis à dnsproxy par un fichier protégé (jamais sur la ligne de commande) et masqué dans les journaux.

## Utilisation

```sh
abdns status                      # mode, réseau, DNS du réseau, domaines envoyés en clair
sudo abdns mode local             # imposer le DNS du réseau (portail récalcitrant)
sudo abdns mode encrypted         # interdire le DNS du réseau
sudo abdns mode auto              # revenir au choix automatique
tail -f /var/log/abdns/abdns.log  # journal du service
cat /var/log/abdns/clear.log      # chaque domaine envoyé en clair
```

Les notifications passent par `osascript` : autoriser **Éditeur de script** dans Réglages › Notifications. Notifications en français : `language = "fr"`.

## Configuration

Voir [`config.example.toml`](config.example.toml). Seul `[upstream]` est obligatoire ; le tableau des clés est dans le [README anglais](README.md#configuration).

## Secours chiffrés par défaut

| Résolveur | Pays | Pourquoi |
|---|---|---|
| [Mullvad DNS](https://mullvad.net/fr/help/dns-over-https-and-dns-over-tls) (sans filtre) | Suède | Sans journaux, sans filtre |
| [DNS.SB](https://dns.sb) | Allemagne | Sans journaux, rapide en Europe |
| [Digitale Gesellschaft](https://www.digitale-gesellschaft.ch/dns/) | Suisse | Association, sans journaux |

dnsproxy répartit les requêtes entre les secours selon leur rapidité ; ils ne servent que si votre résolveur ne répond pas.

## Questions fréquentes

**Comment utiliser mon propre serveur DNS-over-HTTPS sur macOS avec un secours ?**
Les profils DNS chiffrés de macOS n'acceptent qu'un serveur, sans secours. abdns règle le DNS du système sur un relais local qui utilise votre serveur d'abord, des secours chiffrés ensuite.

**Le DNS chiffré fonctionne-t-il derrière un portail captif (Wi-Fi SNCF, hôtel, aéroport) ?**
Pas avec les réglages natifs : le portail bloque le serveur chiffré avant la connexion. abdns constate que tous les serveurs chiffrés échouent alors que le DNS du réseau répond, utilise le DNS du réseau pour la connexion, ouvre la page du portail et revient au chiffré juste après.

**Quels résolveurs DNS ne sont pas censurés en France ?**
Votre propre résolveur, ou des résolveurs sans journaux hors de la juridiction française comme Mullvad, DNS.SB ou Digitale Gesellschaft. Google, Cloudflare, Cisco et DNS4EU appliquent les ordres de blocage français, et Quad9 quitte la France le 24 novembre 2026.

**Les noms locaux comme `nas.lan` ou les recherches inverses de 192.168.x.x partent-ils sur Internet ?**
Non. Les noms du réseau local vont toujours au DNS du réseau, sans notification, dans tous les modes.

**abdns fonctionne-t-il avec un VPN ?**
Oui. Un VPN comme Proton VPN ou WireGuard impose son propre DNS tant qu'il est connecté ; abdns reprend dès la déconnexion.

**Que se passe-t-il si abdns plante ?**
Le DNS du poste est remis sur celui du réseau, et launchd relance abdns en moins de 30 secondes.

**Faut-il Nix ?**
Non. Nix est une façon d'installer parmi d'autres ; `make install` fonctionne sans.

## Limites

- macOS seulement pour l'instant ; Linux et Windows sont prévus (`src/platform/`).
- En mode local, les réponses ne sont pas vérifiées par DNSSEC, et un réseau qui bloque exprès le DNS chiffré obtient le mode local. Les notifications le montrent ; `abdns mode encrypted` l'interdit.
- Le DNS du réseau est celui du bail DHCP (IPv4).
- Les navigateurs ou applications réglés sur leur propre DNS sécurisé, et le Relais privé iCloud, contournent le DNS du système. Régler le DNS sécurisé de Firefox et Chrome sur « système ».

## Développement

```sh
make check                      # formatage, Clippy (avertissements bloquants), tests : ce que la CI exige
make build
```

Pour essayer abdns sans toucher au DNS du système : un port `listen` au-dessus de 1024, `cargo run -- --config essai.toml service --no-system`, puis `dig @127.0.0.1 -p <port> example.com`. Seul `src/platform/` dépend du système : Linux et Windows s'y ajoutent. Dépendances et changelogs : [`docs/stack.md`](docs/stack.md).

## Contribuer

Voir [CONTRIBUTING.md](CONTRIBUTING.md) et la [décision d'architecture](docs/adr/0001-relais-local-et-dnsproxy.md). Failles : [SECURITY.md](SECURITY.md).

## Licence

Voir [LICENSE](LICENSE).
