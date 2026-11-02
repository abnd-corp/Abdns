# 0005 — hyper sur un runtime à un fil à la place de reqwest bloquant

Date : 2026-11-02 · Statut : accepté · Remplace en partie : [0002](0002-client-doh-integre.md)

## Contexte

Le client DoH utilisait reqwest en mode bloquant, et le relais créait un fil par requête UDP ou connexion TCP. Mesures de la 0.2.0 ([#33](https://github.com/abnd-corp/Abdns/issues/33), `make bench`) :

- environ 55 µs de processeur pour une réponse déjà en cache, presque tout dans la création du fil ;
- 9 fils au repos, et un fil de plus par serveur (le runtime caché de reqwest) ;
- 145 crates, dont `url`, `idna` et les données Unicode d'ICU, alors que le client ne parle qu'à des adresses fixes.

tokio était déjà dans le binaire, apporté par reqwest.

## Décision

- **Un runtime tokio à un fil** (`rt.rs`), lancé au premier usage. Il porte le relais UDP et TCP, le client DoH et l'attente des signaux d'arrêt ; chaque requête y est une tâche. La garde et le canal de commande gardent leurs fils et l'utilisent par `block_on`.
- **Réponse en cache envoyée directement** par la boucle UDP, sans tâche.
- **Client DoH sur hyper** (`hyper-util`, `hyper-rustls`) : HTTP/2, ou HTTP/1.1 si le serveur ne propose que lui (ALPN). rustls avec ring, certificats vérifiés par le système, comme avant.
- **Aucune résolution de nom par le client.** Son résolveur (`Fixed`) rend l'adresse de la configuration et refuse tout le reste ; un serveur donné par son adresse IP ne passe pas par lui.
- **Réponse limitée à 64 Kio**, la taille maximale d'un message DNS.
- **Signaux** SIGINT, SIGTERM et SIGHUP par `tokio::signal` (Ctrl+C sous Windows), à la place de `ctrlc`.
- **Le crochet de panique ne remet le DNS du réseau que si la garde plante** (fil `main`), puisque le service s'arrête alors. Une panique dans une tâche ou dans le canal de commande ne coûte que ce qu'elle traitait : tokio l'arrête là, le service continue.
- **TLS 1.2 reste accepté.** Le retirer ne fait gagner que 17 Kio, et un serveur personnel ancien cesserait de répondre. Tous les serveurs publics essayés (Google, Cloudflare, Quad9, Mullvad, DNS.SB, Digitale Gesellschaft) parlent déjà TLS 1.3.

## Alternatives écartées

- **ureq** (HTTP/1.1 seul, 1,05 Mio contre 1,18 pour hyper dans un client minimal) : une rafale de 30 noms prend environ 240 ms contre 57 ms en HTTP/2, et ouvre jusqu'à 30 connexions vers le serveur.
- **HTTP écrit à la main sur rustls** : ce serait recoder un protocole.
- **DNS-over-TLS** : pas de chemin dans l'adresse, donc pas de chemin secret (`token_file`).
- **Un pool de fils** à la place du runtime : moins de code changé, mais reqwest et son runtime caché restaient là.

## Mesures

`make bench`, même machine (le temps des noms inédits et de la rafale dépend surtout du réseau) :

| Mesure | Avant | Après |
|---|---|---|
| Binaire `abdnsd` | 2,20 Mio | 1,82 Mio |
| Crates | 145 | 72 |
| Fils au repos | 9 | 5 |
| Mémoire au repos (RSS) | 10,1 Mio | 9,2 Mio |
| Cache : processeur par réponse | 56 µs | 15 µs |
| Noms inédits : processeur par requête | 0,42 ms | 0,27 ms |

## Conséquences

- Une seule pile réseau pour tout le service, et plus de fil créé par requête.
- `exchange` (DNS du réseau) et les sondes de la garde passent par `block_on` : ils ne doivent jamais être appelés depuis une tâche du runtime.
- Dépendances directes : tokio, hyper, hyper-util, hyper-rustls, http-body-util, tower-service, à la place de reqwest et ctrlc. Suivies par Dependabot et `docs/stack.md`.
