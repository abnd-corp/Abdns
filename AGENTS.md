# Agents

Les règles pour contribuer sont dans [CONTRIBUTING.md](CONTRIBUTING.md). La référence est la [charte Abnd](https://github.com/abnd-corp/gouvernance/blob/main/CHARTE.md).

À retenir : vous proposez une PR, vous ne la mergez jamais (R15) ; vous ne lisez ni n'écrivez de secret (R18) ; aucune signature d'IA dans l'historique (R16).

## Objectif : un applicatif propre et minimaliste

abdns tourne en permanence : chaque octet, chaque fil et chaque réveil du processeur comptent, même quand c'est déjà peu.

- **Mesurer avant et après** : taille des binaires, mémoire, processeur, latence. Un gain qui ne se mesure pas ne justifie pas du code en plus, et aucune fonction ni latence ne doit régresser.
- **Le système d'abord** : ce que fournissent la bibliothèque standard et l'OS (changements de réseau, certificats, service, journal) plutôt qu'une dépendance ; une dépendance éprouvée plutôt que du code maison pour un protocole (TLS, HTTP/2).
- **Chaque dépendance se justifie** (R19), sans ses fonctions par défaut, avec le seul nécessaire activé.
- **Pas de sondage** quand le système sait prévenir, pas de fil ni de processus lancé à chaque requête.
- **Un module, un sujet** : fichiers courts, noms clairs, pas de code mort, des commentaires seulement quand ils apprennent quelque chose.

## Portabilité : rien à installer

abdns doit tourner sur un macOS, un Windows ou un Linux tel qu'il est installé, et plus tard sur les BSD, sans paquet ni programme en plus.

- **Aucune dépendance d'exécution** : seulement ce que le système livre d'office. Une crate, compilée dans le binaire, n'en est pas une, mais elle reste à justifier (R19).
- **Le noyau et l'API du système d'abord** : sous Linux, appels système, netlink, `/proc`, `/sys` et `/etc/resolv.conf`, présents partout ; pas systemd-resolved, NetworkManager ni iproute2, qui ne le sont pas. Sous macOS et Windows, l'API du système, ou à défaut un outil livré avec lui. Un composant facultatif peut enrichir le résultat s'il est là, jamais être nécessaire. Sous Linux, systemd (service, session des notifications) est la dernière dépendance de ce genre : ne pas l'étendre.
- **Ce qu'abdns change dans le système se défait sans lui** : à l'arrêt, s'il est tué net (par le noyau, comme la table du pare-feu à propriétaire) ou au démarrage suivant. Le poste n'est jamais laissé sans DNS.
- **Un système, un dossier** dans `src/platform/`, derrière les mêmes fonctions : ajouter un système ne touche pas au reste.

## `unsafe` : seulement à la frontière du système

- Aucun `unsafe` hors de `src/platform/`.
- Dans `src/platform/`, seulement pour un appel au noyau ou à une API C sans équivalent sûr dans la bibliothèque standard : bloc le plus court possible, enveloppé dans une fonction sûre, précédé d'un commentaire `// SAFETY:` qui dit pourquoi l'appel est correct (tampons et tailles, durée de vie, descripteur possédé). Clippy le vérifie (`undocumented_unsafe_blocks`).
- Une fonction sûre de la bibliothèque standard l'emporte quand elle existe, même un peu plus longue à écrire.

## Priorités, dans cet ordre

1. **Sécurité** : rien ne part en clair sans que l'utilisateur le sache ; aucun secret ni historique lisible par un autre compte ; aucune entrée venue du réseau ne fait planter.
2. **Fiabilité** : jamais de poste sans DNS ; la panne d'une partie (un serveur, une requête, une notification) n'arrête pas le reste.
3. **Performance à l'exécution** : latence de chaque requête, réveils du processeur.
4. **Mémoire**.
5. **Taille** du binaire et du code.

En cas de conflit, la priorité la plus haute l'emporte, et la PR le dit.
