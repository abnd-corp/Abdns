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
