# 0006 — Support de Windows

Date : 2026-11-04 · Statut : proposé

## Contexte

macOS et Linux sont pris en charge ([0003](0003-service-et-outil.md), [0004](0004-support-linux.md)). Sous Windows, le code se compile (CI) mais `src/platform/other.rs` ne règle rien : ni DNS, ni notification, ni service, et le canal de commande n'existe pas hors Unix (#9). L'issue #55 liste en plus des points que #9 ne couvre pas : chemins, droits du fichier de jeton, service en session 0, restauration après un arrêt brutal, tests, journal, signature.

Comme pour Linux, tout ce qui dépend du système reste dans `src/platform/` (un dossier `windows/` avec les mêmes sujets : `network`, `dns`, `service`, `session`). Le relais et la garde ne changent pas.

## Décision

- **Cible** : Windows 10 et 11, `x86_64-pc-windows-msvc` pour la publication ; la CI compile déjà sous Windows. Essais de développement possibles depuis WSL avec `x86_64-pc-windows-gnu`.
- **Chemins** (sous `%ProgramData%\abdns`) : `config.toml`, `state\` (état, canal, jeton) et `logs\` (`abdnsd.log`, `clear.log`). Le dossier est créé à l'installation avec une ACL sans héritage : SYSTEM et Administrateurs en contrôle total, personne d'autre. Le fichier de jeton y hérite donc de droits équivalents au mode `0600` d'Unix, sans dépendance.
- **Lecture du réseau** : PowerShell (`Get-NetIPConfiguration`, `Get-NetAdapter -Physical`, sortie `ConvertTo-Json`), plus fiable à analyser que `ipconfig`, y compris selon la langue du système. Route par défaut, passerelle, DNS et suffixe annoncés par le DHCP, **sans** le relais lui-même. Sondage toutes les 10 s, faute d'équivalent simple de netlink sans dépendance ; à revoir si le coût mesuré est trop élevé.
- **Réglage du DNS : `netsh`** (`netsh interface ip|ipv6 set dnsservers name=<interface> static <relais> validate=no`), par cohérence avec macOS et Linux, qui n'appellent que des outils du système. Il ne touche qu'aux **cartes physiques** par lesquelles le poste sort : VPN, pont, Hyper-V et WSL gardent leur DNS. Avant de changer, abdns enregistre dans `state\` l'état d'origine de chaque carte (DHCP ou liste statique) ; à l'arrêt il le remet (`source=dhcp` ou les adresses d'origine). La garde revérifie le réglage toutes les 5 s (`keep_system_dns`), car un VPN ou Windows peut le défaire.
- **Après un arrêt brutal** : le service est configuré pour redémarrer seul (actions de reprise du gestionnaire de services). Au démarrage, `abdnsd` lit le fichier d'état : s'il y trouve un réglage non restauré, il remet d'abord l'état d'origine, puis règle de nouveau si besoin. Une coupure de courant est couverte de la même façon au démarrage suivant.
- **Canal de commande** : canal nommé `\\.\pipe\abdns` (`tokio::net::windows::named_pipe`, déjà dans la fonctionnalité `net` de tokio), clients distants refusés, même protocole JSON d'une ligne que la socket Unix (`control::answer` ne change pas). Le descripteur de sécurité par défaut d'un canal donne le contrôle total à SYSTEM, aux Administrateurs et au créateur, et la lecture seule aux autres : seuls les administrateurs peuvent donc envoyer une requête. *À vérifier sur un vrai poste.*
- **Service** : `abdnsd` s'enregistre auprès du gestionnaire de services (démarrage automatique, redémarrage sur échec). Cela exige la crate `windows-service` : un programme ne peut pas être lancé comme service sans parler le protocole du gestionnaire, et `sc.exe` seul ne suffit pas. C'est la **seule dépendance ajoutée** (R19) ; elle ne se compile que sous Windows. Arrêt : le gestionnaire envoie un ordre d'arrêt, traité comme le SIGTERM d'Unix (le DNS du réseau est remis avant de sortir). `abdns install` et `uninstall` demandent une élévation (UAC) et refusent clairement sans elle.
- **Compte du service** : `LocalSystem`, comme la plupart des services réseau Windows. Le port 53 et le réglage du DNS l'exigent ; un compte de service dédié est écarté car le réglage de la carte demande de toute façon un privilège administrateur.
- **Notifications et page du portail** : un service tourne en session 0 et ne peut ni afficher de notification ni ouvrir un navigateur chez l'utilisateur. Piste retenue : une petite tâche planifiée de session (`abdns session`, lancée à l'ouverture de session de chaque utilisateur) lit une file de messages dans `state\` et affiche la notification (toast) et ouvre la page. *Choix à confirmer à l'étape correspondante, après essai réel ; l'alternative est `WTSQueryUserToken` + `CreateProcessAsUser`, qui ajoute une dépendance.*
- **Journal** : le service écrit dans `logs\abdnsd.log` (rotation par taille) ; `abdns logs` le lit (`-f` suit le fichier). Pas de journal des événements Windows. `clear.log` suit la même rotation.
- **Tests** : les tests d'intégration deviennent portables (`tests/service.rs`, `tests/cli.rs`) : démarrage du service avec `--no-system`, commande par le canal, arrêt par ctrl-c. Les fonctions qui modifient le système ne sont couvertes que par les tests de leurs parties pures (analyse des sorties PowerShell, génération des commandes `netsh`).
- **Publication** : archive `.zip` avec les deux `.exe` et la somme SHA-256, puis manifeste winget. **Signature de code (Authenticode) : non décidée ici** ; sans elle, SmartScreen avertit à l'installation. À traiter dans une issue à part, car elle suppose un certificat.

## Alternatives écartées

- **API IP Helper / `windows-sys`** pour le réseau et le DNS : plus robuste que d'analyser une sortie d'outil, mais une dépendance de plus et du code `unsafe` ; macOS et Linux ont fait le choix inverse.
- **Tâche planifiée au démarrage à la place d'un service** : aucune dépendance, mais pas de redémarrage ni d'arrêt maîtrisés, et le DNS ne serait pas remis en cas d'arrêt par le système.
- **Journal des événements Windows** : crate supplémentaire et outils d'analyse propres ; un fichier suffit et se lit sur les trois systèmes de la même façon.
- **Écrire dans le registre** (`Tcpip\Parameters\Interfaces`) : contourne `netsh` mais n'est pas pris en compte sans redémarrage de la carte, et défait l'état que Windows gère.

## Conséquences

- Une dépendance (`windows-service`), limitée à Windows, à ajouter dans `docs/stack.md` avec son changelog quand elle entre dans le code.
- Windows 11 propose un DoH natif et des règles NRPT qui peuvent contourner le relais ; à vérifier sur un vrai poste avant de déclarer la prise en charge (voir #55, point 5).
- Les fonctions qui modifient le système (`set_system_dns`, `install`, `uninstall`, `notify`, `open`) exigent une vérification manuelle sur un vrai poste Windows avant merge, et d'abord sur une carte de test, jamais sur l'Ethernet d'un poste de travail.
- Les étapes suivantes se livrent en PR séparées : socle portable (chemins, droits, tests), canal nommé, réseau et DNS, service, session (notifications, portail, journal), publication.
