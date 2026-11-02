# Nom du projet

> Modèle de projet Abnd (charte R21). Cliquer sur « Use this template » pour créer un dépôt, puis remplacer ce texte.

## Pour démarrer

1. Remplacer ce README (description, licence choisie au cahier des charges, R20).
2. Brancher `make build`, `make test` et `make lint` sur les vrais outils du projet. Les trois cibles doivent rester.
3. Créer les branches et les rulesets : `scripts/appliquer-flux.sh <depot>` dans le dépôt [gouvernance](https://github.com/abnd-corp/gouvernance), puis lier le dépôt au [kanban](https://github.com/orgs/abnd-corp/projects/2).
4. Noter dans ce README tout écart avec le standard.

## Ce que le modèle apporte

- `Makefile` avec `build`, `test`, `lint` et `check`.
- Contrôles de gouvernance (titre de PR, branche source) dans `.github/workflows/`.
- Dependabot vers `dev`, avec quarantaine de 7 jours.
- `CONTRIBUTING.md`, `AGENTS.md` et modèle de PR.
