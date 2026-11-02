# Cibles communes de l'organisation (charte R21) : build, test, lint. Un projet peut en ajouter, pas en retirer.
.PHONY: help build test lint check

help: ## Liste les commandes
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*##/ :/'

build: ## À remplacer par le build du projet
	@echo "rien à compiler"

test: ## À remplacer par les tests du projet
	@echo "aucun test pour l'instant"

lint: ## À remplacer par la vérification de style du projet
	@echo "aucun lint pour l'instant"

check: lint test ## Ce qui doit passer avant d'ouvrir une PR (R4)
