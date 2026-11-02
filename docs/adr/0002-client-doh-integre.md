# 0002 — Client DoH intégré à la place de dnsproxy

Date : 2026-11-02 · Statut : accepté · Remplace en partie : [0001](0001-relais-local-et-dnsproxy.md)

## Contexte

La v1 confiait le chiffrement à dnsproxy, lancé par abdns. Trois limites : deux programmes à installer (dnsproxy n'est pas dans Homebrew), seulement deux niveaux de serveurs (principal, secours répartis selon leur vitesse) au lieu d'un ordre strict, et aucun moyen de savoir quel serveur répond pour le signaler. La configuration devait aussi devenir un fichier modifiable sans redémarrer.

## Décision

- abdns parle DoH lui-même (RFC 8484, POST, identifiant 0) avec **reqwest** en mode bloquant, **rustls** et HTTP/2, certificats vérifiés par le magasin du système. Un client par serveur ; l'adresse IP d'un serveur nommé est imposée (`resolve`) : aucune résolution en clair.
- **Ordre strict** : les serveurs de `[[server]]` sont essayés dans l'ordre. Un serveur qui échoue est mis de côté 30 s, puis jusqu'à 5 min ; la garde les contrôle tous en parallèle toutes les 30 s, c'est ainsi qu'ils reviennent.
- **Cache** par question (nom en minuscules, type, classe, bits DO et CD), durée de vie des réponses respectée (SOA borné par son minimum pour les réponses négatives), décomptée à la lecture ; réponse périmée servie jusqu'à un jour si aucun serveur ne répond (RFC 8767).
- Réponse plus grande que la taille UDP du client : renvoyée tronquée (bit TC), le client repasse en TCP.
- **Configuration relue à chaud** (date de modification surveillée toutes les 5 s) ; fichier invalide ignoré et signalé.
- **Notifications limitées** : nouveaux domaines en clair au plus toutes les 5 min, rappel toutes les 30 min, « secours utilisé » après une minute, au plus 6 par 10 min.
- Les erreurs HTTP sont journalisées sans l'adresse (`without_url`) : le jeton d'un chemin secret n'apparaît jamais.

## Dépendances ajoutées

reqwest 0.13 (`blocking`, `rustls`, `http2`, sans options par défaut) et, par lui, rustls, hyper et aws-lc-rs. Écrire un client HTTP/2 et TLS à la main serait plus risqué que d'utiliser ces bibliothèques largement éprouvées. `system-proxy` reste désactivé.

## Conséquences

- Un seul programme à installer ; dnsproxy n'est plus nécessaire.
- Binaire plus gros (TLS et HTTP/2 embarqués) et chaîne de dépendances plus longue, suivie par Dependabot et `docs/stack.md`.
- La validation DNSSEC reste faite par les serveurs (le client relaie le SERVFAIL).
