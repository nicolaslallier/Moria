# Moria — raccourcis. `make` seul affiche l'aide.
# Docker : MORIA_INVITE doit être exportée ou dans .env (lu automatiquement par Compose).
.DEFAULT_GOAL := help
.PHONY: help up down build restart logs ps scale-ai reset-world client test lint e2e bots

AI ?= 1
SERVER ?= 127.0.0.1:7777

help: ## Affiche cette aide
	@grep -E '^[a-z0-9-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

# --- Docker -----------------------------------------------------------------
up: ## Construit et démarre la stack (nats, worldgen, server, dwarf-ai)
	docker compose up -d --build
	@echo "Attendre « serveur prêt » : make logs"

down: ## Arrête la stack (le monde est conservé)
	docker compose down

build: ## Reconstruit les images sans démarrer
	docker compose build

restart: ## Redémarre le serveur
	docker compose restart server

logs: ## Suit les logs du serveur
	docker compose logs -f server

ps: ## État des conteneurs
	docker compose ps

scale-ai: ## Nombre d'instances d'IA : make scale-ai AI=3
	docker compose up -d --scale dwarf-ai=$(AI)

reset-world: ## DÉTRUIT le monde et toutes les forteresses (confirmation demandée)
	@read -p "Supprimer définitivement le volume moria-data ? Taper 'oui' : " r && [ "$$r" = oui ]
	docker compose down -v

# --- Jeu / dev --------------------------------------------------------------
client: ## Lance le client en local
	cargo run --release -p moria-client

test: ## Tests du workspace
	cargo test --workspace

lint: ## fmt + clippy (comme la CI)
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets -- -D warnings

e2e: ## Tests e2e avec un NATS jetable
	docker run -d --rm --name moria-nats-test -p 4222:4222 nats:2-alpine
	NATS_TEST_URL=nats://127.0.0.1:4222 cargo test -p moria-server --test e2e -- --ignored; \
	  s=$$?; docker stop moria-nats-test >/dev/null; exit $$s

bots: ## Test de charge, SERVEUR DE TEST UNIQUEMENT : make bots SERVER=host:7777
	cargo run --release -p moria-server --example bots -- $(SERVER)
