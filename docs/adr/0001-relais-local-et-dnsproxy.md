# 0001 — Relais local, dnsproxy pour le chiffrement, garde pour le dernier recours

Date : 2026-11-02 · Statut : remplacé en partie par [0002](0002-client-doh-integre.md) (dnsproxy retiré)

## Contexte

Les résolveurs publics se retirent ou filtrent dans certains pays (Quad9 quitte la France le 24/11/2026 ; Google, Cloudflare, Cisco et DNS4EU y appliquent des blocages ordonnés par la justice). On veut que le poste utilise d'abord son propre résolveur DoH, des secours chiffrés ensuite, et le DNS du réseau seulement quand rien d'autre ne marche, typiquement devant un portail captif (Wi-Fi de train, d'hôtel, d'aéroport).

Les profils DNS chiffrés natifs (macOS, Windows) n'acceptent qu'un serveur et n'ont pas de secours ; dnscrypt-proxy choisit le serveur le plus rapide plutôt que le préféré et ne gère pas les portails ; dnsproxy n'a que deux niveaux (principal, secours) et ne connaît pas le DNS du réseau.

## Décision

- Un **relais local** écrit en Rust (`127.0.0.1:53`, `[::1]:53`) reçoit toutes les requêtes du poste.
- Le chiffrement, le cache et les secours sont confiés à **dnsproxy**, lancé et surveillé par abdns, déjà éprouvé et disponible sur macOS, Linux et Windows. Ses serveurs sont passés par fichiers (0600) et joints par leur adresse IP (tampons DNS), avec un amorçage volontairement injoignable : aucune requête en clair pour les trouver.
- Une **garde** choisit le mode. Elle bascule sur le DNS du réseau seulement si dnsproxy échoue deux fois de suite alors que le DNS du réseau répond, jamais requête par requête : un SERVFAIL (DNSSEC cassé) reste une erreur. Chaque domaine envoyé en clair est notifié et journalisé.
- Les **noms du réseau local** (`.local`, `.lan`, `.home.arpa`, domaine DHCP, noms sans point, inverses d'adresses privées) vont toujours au DNS du réseau, sans notification.
- Tout ce qui dépend du système est isolé dans `src/platform/` ; macOS d'abord.

## Conséquences

- Deux programmes à installer (abdns, dnsproxy). Remplacer dnsproxy par un client DoH intégré (rustls, HTTP/2, cache) donnerait un binaire unique et un ordre strict entre secours ; c'est une évolution possible, à décider dans un nouvel ADR.
- En mode local, les réponses ne sont pas vérifiées par DNSSEC et un réseau hostile peut provoquer la bascule en bloquant le chiffré : les notifications le rendent visible, `abdns mode encrypted` l'interdit.
- Les dépendances Rust restent minimales (anyhow, ctrlc, serde, serde_json, toml) : le relais et l'analyse des messages DNS sont écrits à la main, sur la petite partie du format nécessaire.
