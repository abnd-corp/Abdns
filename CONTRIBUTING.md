# Contribuer

Ces règles valent pour tous les contributeurs d'Abnd, humains et agents (R17). La référence est la [charte](CHARTE.md).

## En résumé

1. **Une issue d'abord** (R3). Pas d'issue, pas de PR.
2. **Une branche**, jamais de push direct sur `main` (R2).
3. **`make check` passe en local** avant d'ouvrir la PR (R4).
4. **Titre de PR en Conventional Commits** : `feat:`, `fix:`, `chore:`, `docs:`… (R5). Le titre devient le message du squash.
5. **La description contient `Closes #N`** (R3).
6. **Aucune signature d'IA** dans les commits, titres et descriptions : pas de co-auteur, pas de mention d'outil (R16).
7. **Aucun secret**, ni dans le code ni dans un message (R9, R18).
8. **Une nouvelle dépendance se justifie dans la PR** (R19).
9. **Une décision technique importante s'écrit** dans `docs/adr/` (R25).
10. **Une review humaine approuve** avant le merge, en squash (R4, R15). Sur la sécurité, l'infra, les accès et la facturation : deux personnes plus une review par IA (R35).

## Pour les agents

- Un agent propose une PR, il ne la merge jamais (R15).
- Il ne lit et n'écrit jamais `.env`, clé ou token (R18). Au moindre doute, signaler et faire tourner le secret.
- Avant de travailler sur un projet, il lit `docs/stack.md` et les changelogs des versions utilisées, et ne se fie pas à sa mémoire (R39).
- Il ne modifie pas les règles de gouvernance sans PR relue par un humain.

## Faire évoluer les règles

Une issue qui explique pourquoi, puis une PR sur `CHARTE.md` (R33). Voir la section « Faire évoluer la charte ».
