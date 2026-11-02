# Cibles communes de l'organisation (charte R21) : build, test, lint. Un projet peut en ajouter, pas en retirer.
.PHONY: help build release test lint fmt check bench install uninstall clean bpmn test-bpmn

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

bench: release ## Mesure taille, mémoire, fils et processeur d'abdnsd (Internet requis, voir docs/tests.md)
	python3 -I tests/bench.py target/release/abdnsd

install: ## Installe abdns, abdnsd, les pages de manuel et le service (macOS et Linux, sudo, après `make release`)
	@test -x target/release/abdnsd || { echo "run make release first (without sudo)"; exit 1; }
	install -d $(PREFIX)/bin $(PREFIX)/share/man/man1 $(PREFIX)/share/man/man8
	install -m 755 target/release/abdns target/release/abdnsd $(PREFIX)/bin/
	install -m 644 man/abdns.1 $(PREFIX)/share/man/man1/
	install -m 644 man/abdnsd.8 $(PREFIX)/share/man/man8/
	$(PREFIX)/bin/abdns --config $(CONFIG) install

uninstall: ## Arrête et retire le service, remet le DNS du réseau (sudo)
	$(PREFIX)/bin/abdns --config $(CONFIG) uninstall
	rm -f $(PREFIX)/bin/abdns $(PREFIX)/bin/abdnsd $(PREFIX)/share/man/man1/abdns.1 $(PREFIX)/share/man/man8/abdnsd.8

bpmn: ## Génère les PNG des diagrammes docs/bpmn/*.toml (Python, Pillow)
	python3 docs/bpmn/bpmn.py

test-bpmn: ## Teste le générateur de diagrammes
	python3 -m unittest discover -s docs/bpmn

clean: ## Supprime les artefacts de compilation
	$(CARGO) clean
