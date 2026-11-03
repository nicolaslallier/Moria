# Le roi nain — design

Date : 2026-11-02. Statut : **à valider par Nicolas** avant le plan.

## Intention

Chaque montagne a une salle du trône où siège un roi nain. Le joueur s'en approche, parle avec
lui en texte libre, et une IA répond en restant dans le personnage. Le roi sert à deux choses :

- **donner des quêtes** que le jeu sait suivre et valider ;
- **transmettre du savoir** sur le jeu et sur l'état réel de la forteresse du joueur.

Le but est de donner une voix au monde, pas d'ajouter un chatbot : tout ce qui change l'état du
jeu passe par le serveur, jamais par l'IA.

## Décisions de Nicolas

- Le roi vit dans une **salle du trône dans la montagne**. **[Décidé]**
- On parle au roi en s'approchant et en appuyant sur **E**. **[Décidé]**
- Les conversations passent par **un container dédié, connecté à RabbitMQ**. **[Décidé]**
- Le modèle tourne sur **Ollama, à `192.168.2.35`**. **[Décidé]** Modèles présents le 2026-11-02 :
  `llama3.2:1b`, `qwen3.8:27b-mlx`, `gemma4:26b-a4b-it-qat`.
- Le roi **n'offre que des quêtes que le jeu sait valider**. **[Décidé]**
- Récompenses : **objets forgés et ressources dans l'inventaire**. **[Décidé]** Aujourd'hui, les
  seuls « objets forgés » sont `iron` (lingot) et `iron_door` (porte en fer), tous deux dans
  `Resources` ; il n'existe ni inventaire par joueur ni objet individuel.

## Buts, non-buts, anti-buts

**Buts**
- Parler au roi dans la langue du joueur (fr ou en) et obtenir une réponse en quelques secondes.
- Obtenir, accepter, accomplir et se faire récompenser d'une quête, de bout en bout.
- Que le roi connaisse les règles du jeu et l'état réel de la forteresse.

**Non-buts (v1)**
- Quêtes improvisées par l'IA, sans validation automatique.
- Voix, réponses en flux (streaming), roi qui se déplace, autres PNJ parlants.
- Historique de conversation sauvegardé entre deux sessions.
- Objets uniques (armes avec statistiques) : il n'y a pas de système pour les porter.

**Anti-buts (ce qui rend la fonction ratée même si elle « marche »)**
- Le roi **ne doit jamais ralentir ni bloquer le tick** du serveur.
- L'IA **ne décide jamais** d'une récompense, d'un stock ou de l'acceptation d'une quête.
- Si le container ou Ollama tombe, **le jeu continue** ; le roi est seulement muet.
- Le roi ne doit pas inventer de faux faits sur le jeu au point de tromper le joueur.

## Contraintes du dépôt (relevées dans le code)

- **Une montagne privée par joueur** : le roi n'est donc pas partagé.
- **`E` est déjà « descendre »** (`moria-client/src/input.rs::keyboard_intent`, avec `>`).
- **Variantes postcard : on ajoute uniquement à la fin** (`ClientMsg`, `ServerMsg`, `Tile`,
  `FortressView`…), sauf les positions gelées du test `frozen_positions`. `PROTOCOL_VERSION`
  passe de 10 à 11. `SAVE_VERSION` passe de 5 à 6 (le monde change de forme).
- Le serveur a **un seul pont AMQP** : `moria-server/src/bridge.rs`. Les entrées du bus y sont
  validées (taille, montagne connue). Les clés du roi s'y ajoutent.
- Toute modification de `Fortress.resources` se fait **entre `begin_tick` et `end_tick`**
  (`moria-server/src/inventory.rs`), donc dans `sim::apply_command` / `step`, jamais dans la
  boucle async.
- Le container suit le modèle de `moria-dwarf-ai` : crate `crates/moria-king`, binaire
  `moria-king`, `Dockerfile` générique (`ARG BIN`), entrée dans la matrice de
  `.github/workflows/server.yml`, services dans `docker-compose.yml` et
  `docker-compose.override.yml`, mentions dans `README.md` et `Makefile`.
- Aucun client HTTP dans `Cargo.lock` : `moria-king` en ajoute un (petit, au choix du builder).
- **TDD obligatoire** : RED puis GREEN pour chaque tâche. Commits en français, style
  conventionnel. Tout texte du jeu est bilingue via `moria-client/src/lang.rs`.

## Conception

### 1. La salle du trône et le roi

- `add_throne_room(w)` dans `moria-core/src/world.rs`, appelée par `generate`, **avec son propre
  RNG** (comme `add_iron_and_coal`) pour ne pas changer le reste du monde. Elle creuse une salle
  dans la roche, la décore, et la **relie à l'entrée par un couloir** de `Floor`.
- Test : la salle est **atteignable à pied depuis l'entrée** (`path.rs`), sur plusieurs graines.
- Le roi est une **position fixe** (`Fortress.king`, ou déduite de la génération), **séparée de
  `Fortress.dwarves`** : cette liste alimente l'IA des nains et le rendu, le roi n'y entre pas.
- Le client voit le roi via le snapshot de connexion (`FortressView`, champ ajouté en fin) et
  l'affiche avec une tuile ou un sprite simple. Tuile ajoutée à la fin de l'enum `Tile` si besoin.
- Les mondes existants n'ont pas de salle : `SAVE_VERSION` 6 force `moria-worldgen` à régénérer.
  **[Mon choix]** : pas de migration, comme pour les versions précédentes.

### 2. Parler : touche et protocole

- **Touche E contextuelle [mon choix]** : si le roi est adjacent, `E` ouvre le dialogue ; sinon
  `E` garde son rôle (descendre). `>` descend toujours. Alternative : réserver `E` au dialogue.
- `ClientMsg::Talk { text }` et `ClientMsg::AcceptQuest` ajoutés à la fin ;
  `ServerMsg::KingSay { text, offer }` à la fin aussi.
- Le serveur **refuse** (silencieusement) un `Talk` si le héros n'est pas adjacent au roi, si le
  texte dépasse **280 caractères** ou s'il y a déjà une question en cours pour ce joueur. Cadence
  maximale : une question toutes les **3 s** par joueur. **[Mon choix pour les chiffres]**
- Le client ouvre une fenêtre egui sur le modèle de `order_menu`/`order_window` : champ de saisie
  (les touches ne doivent pas déplacer le héros pendant la frappe), journal des échanges, bouton
  **Accepter** quand le roi propose une quête, indicateur « le roi réfléchit… ».

### 3. Le container `moria-king`

- Sans état, branché sur RabbitMQ comme `moria-dwarf-ai`. Clés dans `moria-core/src/king.rs` :
  `king.ask` (serveur → container, file durable partagée, plusieurs instances possibles) et
  `king.reply` (container → serveur, file exclusive du serveur).
- **`Ask`** : montagne, numéro de séquence, langue, texte du joueur, **les 6 derniers échanges**
  (gardés en mémoire par le serveur, non sauvegardés), un résumé de l'état réel (ressources,
  nombre de nains, profondeur atteinte), la quête active et les quêtes encore offertes.
- **`Reply`** : montagne, séquence, texte, et `offer: Option<QuestId>`.
- Le container appelle Ollama (`/api/chat`, **sans réflexion cachée** pour la latence) avec :
  un prompt système (persona du roi, règles du jeu en un fichier texte livré dans l'image,
  consigne de ne pas inventer), puis l'état et l'historique. Il demande une sortie structurée
  `{ "say": "...", "offer": "<id du catalogue ou null>" }`.
- Réglages par variables d'environnement : `AMQP_URL`, `OLLAMA_URL`
  (défaut `http://192.168.2.35:11434`), `OLLAMA_MODEL` (défaut `gemma4:26b-a4b-it-qat`
  **[mon choix : MoE à ~4 G de paramètres actifs, rapide, bon en français]**),
  délai maximal **60 s** **[mon choix]**.
- Si Ollama échoue ou dépasse le délai, ou si la sortie est invalide : réponse de repli fixe
  (« Le roi est perdu dans ses pensées. ») sans offre. Si **aucune réponse** n'arrive au serveur
  après 70 s, le serveur libère la question en cours et envoie ce même repli.
- Le **serveur** (pas le container) vérifie `offer` contre le catalogue, la taille du texte
  (plafonnée) et la montagne connue, comme `decode_assignments` / `decode_state`.

### 4. Les quêtes (le jeu valide, l'IA raconte)

Catalogue fixe dans `moria-core`, identifiant stable, texte bilingue, condition, récompense :

| Quête | Condition (vérifiée par le serveur) | Récompense (exemple, à ajuster) |
|-------|-------------------------------------|---------------------------------|
| Forger des lingots | `resources.iron` augmente de N depuis l'acceptation | pierre, or |
| Forger des portes | `resources.iron_door` augmente de N | lingots, or |
| Rapporter de la nourriture | remise au roi de N `food` (le serveur la retire) | ressources, lingots |
| Creuser en profondeur | le héros atteint la profondeur z ≥ Z | or, mithril |

- **Une seule quête active à la fois [mon choix]**, enregistrée dans `Fortress.quest`
  `{ id, baseline }` : donc sauvegardée avec le monde (`SAVE_VERSION` 6).
- Le flux : le joueur parle → l'IA peut **proposer** une quête (`offer`) → le joueur clique
  **Accepter** (`AcceptQuest`) → le serveur vérifie qu'elle est dans le catalogue, offerte et
  qu'aucune n'est active, puis l'enregistre. Le joueur a donc le dernier mot, et un texte hostile
  ne peut rien déclencher.
- La **validation** se fait à chaque tick de la montagne (ou à la parole pour la remise de
  nourriture) ; la **récompense** est ajoutée à `Fortress.resources` par le serveur, dans
  `sim::apply_command` / `step`, entre `begin_tick` et `end_tick`.
- La profondeur maximale n'est pas suivie aujourd'hui : à ajouter si `hero.pos.z` seul ne suffit pas.

### 5. Le savoir

Le roi sait ce que le prompt lui donne, et rien d'autre : règles du jeu (fichier texte versionné
dans le dépôt) + l'état réel du joueur envoyé à chaque question. La précision des faits n'est
pas vérifiée automatiquement ; c'est le point faible de tout modèle local. Atténuation :
consigne explicite « si tu ne sais pas, dis-le ».

### 6. Déploiement

- `docker-compose.yml` : service `king` (image `ghcr.io/nicolaslallier/moria-king:sha-…`,
  `AMQP_URL`, `OLLAMA_URL`, `OLLAMA_MODEL`, `restart: unless-stopped`, pas de volume). Le
  container doit pouvoir joindre `192.168.2.35:11434` depuis le réseau de la stack.
- `docker-compose.override.yml` (dev local) : image `moria-king:local`.
- CI : `moria-king` dans la matrice ; le job `pin` fonctionne déjà pour tout `moria-*`.
- Aucune clé secrète : Ollama n'a pas d'authentification sur le LAN **[à confirmer]**.

### 7. Sécurité

- Le texte du joueur est une **donnée**, jamais une instruction : le prompt système le dit, et
  surtout **le seul pouvoir de la sortie de l'IA est du texte et le choix d'une quête du
  catalogue**, que le joueur confirme.
- Plafonds de taille et de cadence à chaque frontière (client → serveur, serveur → bus,
  bus → serveur). Le texte de la réponse est tronqué (ex. 1 000 caractères).
- Le serveur ne journalise pas le contenu des conversations au niveau `info`.

## Précisions après relecture (mes choix, à vérifier)

- **« Adjacent »** : distance de Chebyshev ≤ 1, même `z`, entre le héros et le roi. Une même
  fonction `king_in_reach` sert au serveur (refus de `Talk`) et au client (`E` contextuel).
- **Le serveur répond toujours** à un `Talk` accepté ou refusé : `KingSay` avec le texte, ou un
  `KingSay` de repli (hors de portée, trop rapide, délai dépassé). Le client a en plus son propre
  délai de 75 s qui lève l'indicateur « le roi réfléchit… », pour ne jamais rester bloqué.
- **Remise de nourriture** : action **explicite**, `ClientMsg::TurnIn`, jamais un effet de bord
  d'un `Talk`. Le serveur vérifie la portée et le stock, retire la nourriture et verse la
  récompense.
- **Forge** : on compte la **production brute** depuis l'acceptation (compteur dans
  `Fortress.quest`, incrémenté quand la sim produit l'objet), pas le stock net, pour que
  consommer des lingots en cours de quête ne bloque personne.
- **Quêtes offertes** : `Ask` contient les quêtes du catalogue non encore terminées. Voir la
  question ouverte plus bas sur le caractère unique ou répétable.
- **Messages `king.ask`** : non persistants avec expiration de 70 s ; un redémarrage du serveur
  ne rejoue donc pas de vieilles questions. `Reply` est ignorée si `seq` ne correspond pas à la
  question en cours de la montagne (réponse tardive, reconnexion).
- **Langue** : champ `lang` dans `Talk` (« fr » ou « en »), pas dans le handshake.
- **Monde existant** : `SAVE_VERSION` 6 → `reset_if_outdated` met l'ancien monde de côté et
  `moria-worldgen` régénère, comme pour les versions précédentes.
- **Ollama non vérifié** : le 2026-11-02, `/api/tags` répondait, mais mes appels à `/api/chat`
  (gemma4, avec `format` et `think:false`) ont reçu une connexion coupée ou refusée, et le serveur
  a mis quelques secondes à revenir. Latence, sortie structurée et `think:false` sont donc **non
  confirmés**. La première tâche du plan est un test réel contre cet Ollama ; si la sortie
  structurée échoue, repli sur `{"say":…}` en JSON simple, voire texte seul sans offre.
- **Délais** : 60 s est un plafond, pas un objectif ; le but reste quelques secondes. À ramener
  autour de 20 s si les mesures le permettent.

## Décision : quêtes uniques **[Décidé par Nicolas, 2026-11-02]**

Chaque quête ne se fait qu'**une fois par montagne**. `Fortress` sauvegarde donc la liste des
quêtes terminées (avec `SAVE_VERSION` 6), et `Ask` n'offre que celles qui ne le sont pas. Le
design est **approuvé** par Nicolas.

## Tests attendus (RED d'abord)

- Génération : salle atteignable à pied depuis l'entrée ; le reste du monde est inchangé.
- Protocole : `frozen_positions` toujours vert ; aller-retour des nouveaux messages.
- Quêtes : acceptation (refus si déjà active ou hors catalogue), validation de chacun des quatre
  types, récompense versée une seule fois, persistance dans la sauvegarde.
- Serveur : `Talk` refusé hors de portée, trop long, trop rapide ; délai de repli.
- `moria-king` : prompt construit avec l'état ; sortie invalide → repli ; Ollama simulé (serveur
  HTTP de test) ; test d'intégration broker `#[ignore]` + `AMQP_TEST_URL`.
- Client : `E` contextuel ; la saisie ne déplace pas le héros ; la fenêtre ne prend pas le focus
  par erreur (calqué sur les tests de `ui.rs`).

## Laissé au builder

Format exact des messages, nom des champs, choix du client HTTP, tuile ou sprite du roi, taille
et décor de la salle, valeurs exactes des quêtes et des récompenses, découpage en tâches.
