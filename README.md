# Moria

Jeu multijoueur à la Dwarf Fortress : chaque joueur dirige une forteresse dans sa montagne, avec un héros et des nains autonomes (service d'IA séparé).

Architecture : `server` (TCP 7777, simulation 10 Hz), `dwarf-ai` (IA des nains), `inventory` (inventaire des joueurs, voir « Inventaire »), `worldgen` (génération du monde, une seule fois). Les services communiquent par le RabbitMQ de la pile Infra (`rabbitmq:5672` sur `infra-net`, vhost `moria`, utilisateur `moria`) ; en local, l'override lance un RabbitMQ jetable.

## Premier démarrage

```sh
export MORIA_INVITE='un-code-secret'   # obligatoire, voir « Code d'invitation »
export MORIA_AMQP_PASSWORD=dev         # obligatoire ; en local n'importe quelle valeur (l'override utilise guest)
docker compose up -d
docker compose logs server    # attendre « serveur prêt sur 0.0.0.0:7777 »
```

Toutes les commandes `docker compose` exigent `MORIA_INVITE` et `MORIA_AMQP_PASSWORD` (variables exportées ou fichier `.env` non commité) : sans elles, Compose refuse de démarrer.

Le monde est généré automatiquement au premier démarrage (service `worldgen`, qui s'arrête ensuite). Aux démarrages suivants il détecte le monde existant (« monde déjà présent, rien à faire ») et ne l'écrase jamais.

Pour régénérer un monde : arrêter la stack puis supprimer le volume `moria-data` (`docker compose down -v`). **Cela détruit définitivement le monde et toutes les forteresses.** Réinitialiser le monde (suppression de `world.bin` ou `down -v`) doit s'accompagner de la suppression de `inventory.bin` / du volume `moria-inventory` : sinon l'inventaire conserve le stock et les drapeaux `seeded` de l'ancien monde et les ressources de départ du nouveau sont ignorées.

## Déploiement avec Portainer (192.168.2.10)

1. Portainer > Stacks > Add stack > **Repository**.
2. Repository URL : `https://github.com/nicolaslallier/Moria`, branche `main`, Compose path : `docker-compose.yml`. Si le dépôt est privé, activer « Authentication » et fournir un jeton GitHub (token) à Portainer.
3. Dans « Environment variables », ajouter `MORIA_INVITE` (**obligatoire**, voir ci-dessous) et `MORIA_AMQP_PASSWORD` (**obligatoire**, mot de passe de l'utilisateur RabbitMQ `moria`, URL-safe : lettres et chiffres seulement, car il est inséré dans `AMQP_URL`) : sans elles, le déploiement échoue.
4. Déployer : les images sont construites, le monde est généré, puis le serveur démarre. Il n'est plus publié directement : le nginx de la pile Infra expose `192.168.2.10:7777` (proxy TCP, `nginx/stream.d/moria.conf` dans Infra, limite de 4 connexions par IP) et joint le serveur sur `infra-net`. Ordre : d'abord le changement Infra qui crée le vhost et l'utilisateur `moria` sur RabbitMQ, puis cette stack, puis le reste d'Infra qui prend le port 7777. Au premier déploiement, le pull de l'image `inventory` peut échouer et être réessayé quelques minutes, jusqu'à ce que le commit du bot CI épingle le vrai tag. Le port `9464` (métriques Prometheus, lues par la stack Infra) est aussi publié, pour le réseau local uniquement. `amqp://moria:moria@rabbitmq:5672/moria` n'est que le repli des binaires quand `AMQP_URL` n'est pas défini ; le compose le définit toujours, à partir de `MORIA_AMQP_PASSWORD`. Après la migration depuis NATS, un conteneur `nats` orphelin de l'ancienne pile peut subsister sur l'hôte (Portainer ne supprime pas toujours les orphelins) : le retirer avec `docker rm`.
5. Logs : l'Alloy de la pile Infra collecte le stdout des conteneurs de cet hôte ; dans Grafana (https://grafana.infra.famillelallier.net), Explore > Loki, requête `{compose_project="moria"}` (filtrer par `compose_service`). Le nom de stack Portainer doit rester `moria`.

## Mise à jour vers le dehors et le bois

Le monde est régénéré (carte 176×96×12 avec forêt, rivière, lac et collines) : les anciennes sauvegardes sont refusées avec « format de sauvegarde v1 non supporté ». Il faut donc repartir d'un monde neuf, et chaque joueur recrée son compte.

- En local : `make reset-world` (ou `docker compose down -v`), puis `make up`.
- Sur Portainer, **ne jamais supprimer la stack** `moria` : elle perdrait sa variable `MORIA_INVITE`, son réglage GitOps (mise à jour automatique) et son Id. La stack suit `main` (toutes les 5 minutes) et le monde vit dans le volume Docker `moria-data`. Marche à suivre :
  1. Laisser la nouvelle version se déployer. Tant que l'ancien monde est là, le serveur redémarre en boucle (crash-loop) avec « format de sauvegarde v1 non supporté » dans ses logs : c'est attendu, il n'écrase jamais l'ancien monde.
  2. Sur l'hôte, supprimer uniquement le fichier de l'ancien monde, sans toucher au volume ni à la stack : `docker run --rm -v moria-data:/data alpine rm -f /data/world.bin`
  3. Dans Portainer, **Pull and redeploy** la stack (ou la redémarrer) : le service worldgen régénère le monde au démarrage, puis le serveur démarre.
  4. Vérifier la version : un Hello brut envoyé au port 7777 doit recevoir `00 00 00 03 01 00 04` (serveur en protocole v4). Par exemple : `printf '\x00\x00\x00\x02\x00\x01' | nc -w 2 192.168.2.10 7777 | xxd`
  5. Chaque joueur recrée son compte.

**Remise à zéro de l'inventaire.** Supprimer `inventory.bin` pendant que l'inventaire tourne ne sert à rien : il réécrit son état en mémoire au lot suivant ou au SIGTERM. Marche à suivre : (1) arrêter le conteneur (`docker stop`, par ex. `docker stop moria-inventory-1`, ou « Stop » dans Portainer) ; (2) `docker run --rm -v moria-inventory:/data alpine rm -f /data/inventory.bin` ; (3) vérifier la file `inventory` dans RabbitMQ et la purger si elle contient des lots : une ancienne graine encore en file l'emporterait sur la nouvelle ; (4) redéployer. `make reset-world` et `docker compose down -v` suppriment déjà le volume `moria-inventory`.

**Attention :** le volume du compose s'appelle `moria-data` globalement, quel que soit le nom du projet. `make reset-world`, `docker compose down -v` ou la suppression du volume `moria-data` détruit donc ce monde sur la machine, même depuis un autre projet Compose.

Nouvelles commandes : Couper (X : creuser, escalier, couper, mur ; Espace vers un arbre le coupe), les pentes changent de niveau, et le menu de construction (touche 3) propose le mobilier en bois (lit 3, table 4, chaise 2).

## Code d'invitation

`MORIA_INVITE` est une variable de stack Portainer **obligatoire** : `docker-compose.yml` refuse de démarrer si elle est absente ou vide (sinon n'importe qui sur Internet pourrait créer un compte et occuper une des 10 montagnes). Ne jamais la commiter. Le code n'est demandé qu'à la création d'un compte ; les comptes existants se connectent sans. Partagez-le uniquement avec vos amis.

`MORIA_MAX_PER_IP` (optionnelle, défaut 4) : connexions simultanées max par adresse IP ; à augmenter si plusieurs amis partagent la même IP (NAT). 0 est refusé.

## Accès depuis Internet

Sur le routeur, rediriger le port **7777/TCP** vers `192.168.2.10:7777`. Aucun autre port ne doit être ouvert (surtout pas `9464`, les métriques).

## Supervision

`moria-server` expose ses métriques sur `:9464/metrics` (`MORIA_METRICS_BIND`, défaut `0.0.0.0:9464`) : joueurs et connexions, durée des ticks et dépassements des 100 ms, vues et assignations échangées avec `dwarf-ai`, état du pont bus et de l'inventaire (le nom de métrique `moria_nats_connected` est conservé volontairement), échecs de sauvegarde. Prometheus (stack Infra, job `moria`) les lit ; tableau de bord Grafana **Moria**.

## Pour les amis

1. **Télécharger le client** : page *Releases* du dépôt GitHub (`https://github.com/nicolaslallier/Moria/releases`), dernière version `v…`. Prendre le fichier de votre système :
   - Windows : `moria-client-windows.exe`
   - Linux : `moria-client-linux`
   - macOS : `moria-client-macos` (Mac Apple Silicon M1/M2/M3… uniquement : build arm64)
2. **Linux et macOS** : rendre le fichier exécutable, dans un terminal :
   ```sh
   chmod +x moria-client-linux     # ou moria-client-macos
   ```
3. **macOS** : le client n'est pas signé, macOS le bloque au premier lancement. Clic droit sur le fichier → **Ouvrir**, puis confirmer « Ouvrir ». Ou, dans un terminal :
   ```sh
   xattr -d com.apple.quarantine moria-client-macos
   ```
4. **Se connecter** : dans le champ « Serveur », taper l'adresse IP publique de Nicolas (ou son nom DDNS) suivie de `:7777`, par exemple `exemple.duckdns.org:7777`. Nicolas vous donne l'adresse exacte.
5. **Premier lancement** : choisir un nom et un mot de passe (8 caractères minimum), saisir le **code d'invitation** donné par Nicolas, puis cliquer **« Créer un compte »**. Le code ne sert qu'à cette étape.
6. **Les fois suivantes** : même nom et mot de passe, bouton **« Se connecter »** ; le code d'invitation n'est plus nécessaire. Le nom ne tient pas compte des majuscules.

## Inventaire

Le service `inventory` possède l'inventaire des joueurs et le sauvegarde dans `/data/inventory.bin` (`MORIA_INVENTORY_FILE`), sur le volume `moria-inventory`, avec une sauvegarde finale à l'arrêt. `server` en garde un cache alimenté par le bus. S'il tombe, le jeu continue sur ce cache ; les règles de nourriture et de faim sont inchangées.

## Plusieurs services d'IA

Les instances de `dwarf-ai` partagent une file RabbitMQ : on peut en ajouter.

```sh
docker compose up -d --scale dwarf-ai=3
```

ou `deploy.replicas: 3` dans `docker-compose.yml`. Sans service d'IA, le jeu continue : les nains finissent leur tâche puis attendent.

## Licence et attribution

Voir [ATTRIBUTION.md](ATTRIBUTION.md) (SRD 5.1, CC-BY-4.0).
