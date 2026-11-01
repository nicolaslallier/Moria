# Moria — raccourcis. `make` seul affiche l'aide.
# Docker : MORIA_INVITE doit être exportée ou dans .env (lu automatiquement par Compose).
.DEFAULT_GOAL := help
.PHONY: help up down build restart logs ps scale-ai reset-world client test lint e2e bots

AI ?= 1
SERVER ?= 127.0.0.1:7777

help: ## Affiche cette aide
	@grep -E '^[a-z0-9-]+:.*## ' $(MAKEFILE_LIST) | awk -F':.*## ' '{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

# --- Docker -----------------------------------------------------------------
# Créé une seule fois si absent (ignoré par git). Pour changer le code : éditer .env.
.env:
	@echo "MORIA_INVITE=$$(openssl rand -hex 8)" > $@
	@echo "MORIA_AMQP_PASSWORD=dev" >> $@
	@echo "Créé .env avec un code d'invitation aléatoire : $$(cut -d= -f2 $@)"

up: .env ## Construit et démarre la stack (rabbitmq local, worldgen, server, dwarf-ai, inventory)
	docker compose up -d --build
	@echo "Attendre « serveur prêt » : make logs"

down: .env ## Arrête la stack (le monde est conservé)
	docker compose down

build: .env ## Reconstruit les images sans démarrer
	docker compose build

restart: .env ## Redémarre le serveur
	docker compose restart server

logs: .env ## Suit les logs du serveur
	docker compose logs -f server

ps: .env ## État des conteneurs
	docker compose ps

scale-ai: .env ## Nombre d'instances d'IA : make scale-ai AI=3
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

e2e: ## Tests e2e avec un RabbitMQ jetable
	docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine
	AMQP_TEST_URL=amqp://guest:guest@127.0.0.1:5672 cargo test -p moria-server --test e2e -- --ignored --test-threads=1; \
	  s=$$?; docker stop moria-amqp-test >/dev/null; exit $$s

bots: ## Test de charge, SERVEUR DE TEST UNIQUEMENT : make bots SERVER=host:7777
	cargo run --release -p moria-server --example bots -- $(SERVER)
