# 0004 — Support de Linux par systemd-resolved

Date : 2026-11-02 · Statut : remplacé en partie par [0007](0007-linux-pare-feu-du-noyau.md) (réglage du DNS, lecture du réseau)

## Contexte

Seul macOS était pris en charge. Tout ce qui dépend du système est isolé dans `src/platform/` (réseau actif et son DNS, réglage du DNS du poste, notifications, ouverture d'une page, installation du service). Le relais et la garde sont déjà indépendants du système.

Sous Linux, il n'y a pas de réglage unique du DNS : `/etc/resolv.conf` est géré par systemd-resolved, par NetworkManager, par dhcpcd ou à la main, selon la distribution.

## Décision

- **Réglage du DNS : systemd-resolved** (`resolvectl dns <interface> <relais>` et `resolvectl domain <interface> ~.`), le plus répandu (Ubuntu, Fedora, Debian récent, Arch). Le réglage est propre à l'interface et d'autres services (NetworkManager surtout) peuvent le défaire : la garde le vérifie toutes les 5 s (`keep_system_dns`) et le refait, ce qui couvre aussi un nouveau réseau. Il ne touche qu'aux **interfaces physiques** par lesquelles le poste sort (présence de `/sys/class/net/<i>/device`) : un VPN, un pont ou un conteneur garde son DNS. `network_services` (macOS) est sans objet.
- **Arrêt** : chaque interface reçoit explicitement le DNS que NetworkManager connaît pour son réseau (`nmcli -t -f IP4.DNS,IP6.DNS,… device show`, inchangé par le réglage d'abdns), sinon le réglage d'avant abdns, sinon `resolvectl revert`. Les interfaces restées sur le relais après un arrêt brutal sont aussi remises. *Essai réel :* `resolvectl revert` suivi de `nmcli device reapply` ne remettait pas le DNS (NetworkManager ne le repousse pas s'il croit n'avoir rien changé) : le poste restait sans DNS. D'où la remise explicite.
- **DNS du réseau** : lu dans le bail DHCP de NetworkManager (`nmcli … DHCP4.OPTION`), qui ne change pas quand abdns règle resolved ; sinon le DNS que resolved a pour l'interface, sans les adresses de boucle locale. Route par défaut : `ip route show default`, IPv4 d'abord, plus basse métrique.
- **Notifications et page du portail** : le service tourne en root ; `loginctl` trouve la session graphique active, `runuser` lance `notify-send` ou `xdg-open` avec le bus et l'écran de l'utilisateur. Hors root (essais avec `--no-system`), le programme est lancé directement. `make install` ajoute `/usr/share/applications/abdns.desktop` (sans menu) et les notifications portent l'indice `desktop-entry` : sans cela, GNOME les attribue à une « application inconnue ».
- **Service** : unité systemd `abdnsd.service` (`Restart=always`, `RestartSec=30`) installée par `abdns install` à côté du binaire `abdnsd` ; la sortie d'erreur va au journal (`abdns logs`, soit `journalctl -u abdnsd`), qui assure la rotation. L'outil `abdns` pilote le service par la socket de commande, dont le groupe est `sudo`, `wheel` ou `admin` ([0003](0003-service-et-outil.md)).
- Aucune dépendance Rust ajoutée : uniquement des outils du système, comme sur macOS.

## Alternatives écartées

- **Écrire `/etc/resolv.conf`** : écrasé par resolved ou NetworkManager, et casse leur gestion.
- **NetworkManager seul** (`nmcli connection modify … ipv4.dns`) : change aussi le bail lu par `nmcli`, et exclut les postes sans NetworkManager.
- **Ajouter le relais sur l'interface `lo`** : resolved continuerait d'envoyer aussi aux DNS des autres interfaces, sans garantie d'ordre, donc des fuites en clair.

## Conséquences

- Les postes sans systemd-resolved (dhcpcd, resolvconf, Alpine…) ne sont pas pris en charge : `set_system_dns` répond par une erreur claire. D'autres méthodes pourront s'ajouter derrière les mêmes fonctions.
- Le DNS du réseau est celui du bail IPv4 de NetworkManager ; sans lui, celui de resolved. Les réseaux IPv6 seuls restent limités (comme sur macOS).
- Après un SIGKILL, le poste reste réglé sur abdns jusqu'au redémarrage par systemd (30 s). À traiter à part : une commande de restauration appelée par `ExecStopPost`.
- Les fonctions qui modifient le système (`set_system_dns`, `install`, `uninstall`, `notify`, `open`) ne sont couvertes que par des tests de leurs parties pures (analyse des sorties de `ip`, `nmcli`, `resolvectl`, `loginctl`, génération de l'unité). Elles demandent une vérification manuelle sur un vrai poste avant d'être déclarées sûres.
