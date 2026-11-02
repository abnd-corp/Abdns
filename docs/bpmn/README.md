# Diagrammes BPMN

Ce dossier contient les diagrammes de processus du projet : une source `.toml` lisible et versionnée, et le `.png` généré à côté.

| Fichier | Contenu |
|---|---|
| `bascule-portail.toml` / `.png` | Comment la garde passe du DNS chiffré au DNS du réseau (portail captif) |
| `bpmn.py` | Le générateur |
| `test_bpmn.py` | Ses tests |

## Générer

```sh
pip install -r docs/bpmn/requirements.txt   # Pillow, seule dépendance
make bpmn                                   # tous les docs/bpmn/*.toml → PNG à côté
python3 docs/bpmn/bpmn.py a.toml --out /tmp # un fichier, ailleurs
```

Les PNG sont versionnés : après avoir modifié un `.toml`, relancez `make bpmn` et commitez les deux.

## Ce que le générateur garantit

Il s'arrête avec une erreur plutôt que de dessiner un diagramme qui enfreint une de ces règles :

- **un titre** en haut de l'image (obligatoire) ;
- **aucune flèche n'en croise une autre**, ni ne traverse un élément ;
- **aucun libellé ne chevauche** un autre libellé, un élément ou une flèche.

Il place les éléments de gauche à droite, essaie plusieurs dispositions des branches (haut ou bas) et garde la première qui respecte les règles. Si aucune ne convient, il liste les défauts : ajoutez alors `row` ou `col` à quelques nœuds, ou simplifiez le flux. `--force` dessine malgré les défauts.

## Format d'une source

```toml
title = "Titre du diagramme"        # obligatoire

[[node]]
id = "start"                        # unique
kind = "start"                      # start | end | task | gateway (task par défaut)
label = "Réseau rejoint"            # obligatoire sauf pour une passerelle
# marker = "xor"                    # passerelle : xor (défaut) ou parallel
# col = 3                           # position forcée (optionnel)
# row = 1

[[flow]]
from = "start"
to = "probe"
label = "Oui"                       # optionnel
```

- Le **premier** flux sortant d'un nœud reste sur sa ligne ; les suivants partent vers le haut ou le bas.
- Un flux qui revient vers un nœud plus ancien (boucle) est détecté tout seul et passe sous (ou sur) le diagramme.
- Un `\n` dans un libellé force un retour à la ligne.
