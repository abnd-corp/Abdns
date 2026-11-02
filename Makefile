# Cibles communes de l'organisation (charte R21) : build, test, lint. Un projet peut en ajouter, pas en retirer.
.PHONY: help build release test lint fmt check install uninstall clean

CARGO ?= cargo
PREFIX ?= /usr/local
CONFIG ?= /etc/abdns/config.toml

help: ## Liste les commandes
	@grep -E '^[a-z-]+:.*##' $(MAKEFILE_LIST) | sed 's/:.*##/ :/'

build: ## Compile en mode développement
	$(CARGO) build --locked

release: ## Compile en mode release
	$(CARGO) build --release --locked

test: ## Lance les tests
	$(CARGO) test --locked

lint: ## Formatage et Clippy, avertissements bloquants
	$(CARGO) fmt --all --check
	$(CARGO) clippy --all-targets --locked -- -D warnings

fmt: ## Formate le code
	$(CARGO) fmt --all

check: lint test ## Ce qui doit passer avant d'ouvrir une PR (R4)

install: ## Installe le binaire de `make release` et le service (macOS, sudo)
	@test -x target/release/abdns || { echo "run make release first (without sudo)"; exit 1; }
	install -d $(PREFIX)/bin
	install -m 755 target/release/abdns $(PREFIX)/bin/abdns
	$(PREFIX)/bin/abdns --config $(CONFIG) install

uninstall: ## Arrête et retire le service, remet le DNS du réseau (sudo)
	$(PREFIX)/bin/abdns --config $(CONFIG) uninstall
	rm -f $(PREFIX)/bin/abdns

clean: ## Supprime les artefacts de compilation
	$(CARGO) clean
