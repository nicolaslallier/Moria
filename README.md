# Moria

Jeu multijoueur à la Dwarf Fortress : chaque joueur dirige une forteresse dans sa montagne, avec un héros et des nains autonomes (service d'IA séparé).

Architecture : `server` (TCP 7777, simulation 10 Hz), `dwarf-ai` (IA des nains via NATS), `nats` (interne), `worldgen` (génération du monde, une seule fois).

## Premier démarrage

```sh
docker compose up -d
docker compose logs server    # attendre « serveur prêt sur 0.0.0.0:7777 »
```

Le monde est généré automatiquement au premier démarrage (service `worldgen`, qui s'arrête ensuite). Aux démarrages suivants il détecte le monde existant (« monde déjà présent, rien à faire ») et ne l'écrase jamais.

Pour régénérer un monde : arrêter la stack puis supprimer le volume `moria-data` (`docker compose down -v`). **Cela détruit définitivement le monde et toutes les forteresses.**

## Déploiement avec Portainer (192.168.2.10)

1. Portainer > Stacks > Add stack > **Repository**.
2. Repository URL : `https://github.com/nicolaslallier/Moria`, branche `main`, Compose path : `docker-compose.yml`. Si le dépôt est privé, activer « Authentication » et fournir un jeton GitHub (token) à Portainer.
3. Dans « Environment variables », ajouter `MORIA_INVITE` (voir ci-dessous).
4. Déployer : les images sont construites, le monde est généré, puis le serveur démarre. Il écoute sur `192.168.2.10:7777`. Seul ce port est publié ; NATS reste sur le réseau interne.

## Code d'invitation

`MORIA_INVITE` est une variable de stack Portainer : ne jamais la commiter. Si elle est définie, il faut ce code pour créer un compte ; les comptes existants se connectent sans. Vide ou absente : inscription libre. Partagez le code uniquement avec vos amis.

## Accès depuis Internet

Sur le routeur, rediriger le port **7777/TCP** vers `192.168.2.10:7777`. Aucun autre port ne doit être ouvert.

## Plusieurs services d'IA

Les instances de `dwarf-ai` partagent une queue NATS : on peut en ajouter.

```sh
docker compose up -d --scale dwarf-ai=3
```

ou `deploy.replicas: 3` dans `docker-compose.yml`. Sans service d'IA, le jeu continue : les nains finissent leur tâche puis attendent.

## Licence et attribution

Voir [ATTRIBUTION.md](ATTRIBUTION.md) (SRD 5.1, CC-BY-4.0).
