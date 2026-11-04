# 0007 — Linux : DNS du poste par le pare-feu du noyau

Date : 2026-11-04 · Statut : accepté (Hugo), relecture de Rudy demandée · Remplace en partie [0004](0004-support-linux.md)

## Contexte

L'ADR 0004 règle le DNS du poste par systemd-resolved (`resolvectl`) et lit le réseau par `ip` et `nmcli`. Trois défauts :

- **Composants à installer** : systemd-resolved est absent par défaut sur Debian, coupé sur Arch, inutilisé par openSUSE. L'objectif du projet est de tourner sur un système tel qu'installé, sans paquet (AGENTS.md).
- **Bataille avec NetworkManager** : il défait le réglage ; abdns le refait toutes les 5 s, et le retour au DNS d'avant à l'arrêt a demandé des contournements.
- **Arrêt brutal** : après un SIGKILL, le poste pointe vers un relais mort jusqu'au redémarrage du service, jusqu'à 30 s (#21).

Le seul point commun à tous les Linux est `/etc/resolv.conf`, mais le programme qui l'écrit varie. Le pare-feu du noyau (nf_tables), lui, est partout.

## Décision

- **Une table `inet abdns` du pare-feu**, chaîne `nat` au crochet `output` (priorité -100), redirige vers le relais toute requête UDP ou TCP vers le port 53 qui sort par une carte de type Ethernet (Ethernet, Wi-Fi). La boucle locale (stub de resolved en 127.0.0.53, relais) et les VPN (tun, WireGuard) ne sont pas touchés. Rien n'est écrit dans `resolv.conf`, resolved ou NetworkManager.
- **Parlée directement au noyau par netlink** (`NETLINK_NETFILTER`, messages en lot), sans `nft` ni `iptables` : environ 300 lignes, dont les tests, dans `src/platform/linux/netfilter.rs`. L'interface nf_tables du noyau est stable.
- **Table à propriétaire** (`NFT_TABLE_F_OWNER`, Linux 5.12) : elle appartient à la socket netlink que garde le service ; le noyau la retire dès que le processus s'arrête, même tué net (#21). Une autre socket ne peut ni la changer ni la retirer. Noyau plus ancien : table sans propriétaire, retirée à l'arrêt et remplacée au démarrage suivant.
- **Requêtes d'abdns** vers le DNS du réseau (mode local, noms locaux, sondes) : marquées (`SO_MARK`), laissées passer par la première règle. Une socket non marquée alors que la table est en place est refusée plutôt que de boucler vers le relais.
- **Réseau lu sans outil** : route par défaut dans `/proc/net/route` (et `/proc/net/ipv6_route`), DNS et domaine du réseau dans `/etc/resolv.conf`, inchangé par abdns ; derrière le stub de resolved, dans `/run/systemd/resolve/resolv.conf` puis `/run/NetworkManager/no-stub-resolv.conf`. Nom montré : « Wi-Fi » (`/sys/class/net/<if>/wireless`), « Ethernet » ou le nom de l'interface.
- **Contrôle** : toutes les 5 s, la garde vérifie que la table existe (une lecture netlink) et la repose sinon.

## Alternatives écartées

- **`nft` ou `iptables`** : ~100 lignes au lieu de ~300, mais un programme à trouver sur le poste (absent par défaut d'Ubuntu 26.04 en machine minimale, par exemple) et sa sortie à lire.
- **Écrire `/etc/resolv.conf`** : réécrit par resolved ou NetworkManager à chaque changement de réseau ; ne convient qu'aux Linux sans gestionnaire.
- **eBPF (crochets `connect` des cgroups)** : plus fin, mais plus de code, des droits particuliers et des noyaux récents pour rien de plus ici.

## Conséquences

- Plus de systemd-resolved, de NetworkManager ni d'iproute2 nécessaires ; `resolvectl`, `nmcli` et `ip` ne sont plus appelés. systemd reste utilisé pour le service et la session des notifications (à traiter à part).
- Les programmes qui interrogent un serveur DNS directement (`dig @8.8.8.8`) passent eux aussi par abdns, sur les cartes Ethernet et Wi-Fi (une partie de #48).
- Le DNS du réseau est la liste globale de `resolv.conf` (ou de resolved), plus celle d'une seule interface.
- Une carte d'un autre type (PPP, modem 4G en IP brute) n'est pas redirigée.
- Essais : tests unitaires des messages ; test contre le vrai noyau (`--ignored netfilter`, root), lancé par la CI sous Linux ; essai réel sur Ubuntu 26.04 sans resolved, NetworkManager ni `nft` (redirection UDP et TCP, mode local, arrêt normal, `kill -9`).
