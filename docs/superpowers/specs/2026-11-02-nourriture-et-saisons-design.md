# Moria : le calendrier des saisons et le conteneur nourriture, document de conception

*Date : 2026-11-02 · Auteur : Nicolas Lallier (avec Claude) · Specs liées : `2026-11-01-inventaire-design.md` (le bus RabbitMQ, l'inventaire, le motif « cache + état périodique »), `2026-11-01-lumieres-design.md` (le cycle du jour)*

**Légende :**
- **[Décidé]** : choix de Nicolas.
- **[Proposé]** : choix de Claude, à confirmer par Nicolas à la relecture.
- **[Au constructeur]** : laissé à celui qui construit.

---

## 1. Intention

La nourriture (vaches, champs) va devenir grosse. Nicolas la veut dans **son propre conteneur**, branché sur le bus d'événements de l'Infra, comme `dwarf-ai` et l'inventaire. Ce conteneur simule **des troupeaux qui se reproduisent, des cultures et des saisons**.

Les saisons, elles, touchent **tout le monde** (nourriture, lumière, affichage, nains). Le calendrier ne peut donc pas appartenir au conteneur nourriture : c'est un fait du monde, partagé.

## 2. Découpage

- **[Décidé]** Trois morceaux, dans cet ordre :
  1. **Le calendrier** : une petite fonction de `moria-core` (§3). Pas de conteneur.
  2. **Le conteneur nourriture** (§4 à §8), qui s'appuie sur le calendrier.
  3. **Les autres effets des saisons**, un par un, plus tard : longueur du jour et couleur du sol (lumières), affichage de la saison, comportement des nains en hiver. Hors de ce document : chacun sera sa propre petite modification posée sur le calendrier.

## 3. Le calendrier

- **[Décidé]** 4 saisons de 7 jours de jeu. Un jour de jeu = `DAY_TICKS` = 36 000 ticks (une heure réelle, comme dans les lumières). Une saison = 252 000 ticks (7 heures réelles), une année = 1 008 000 ticks (28 heures réelles).
- **[Proposé]** Dans `moria-core`, un module `calendar` : `enum Season { Spring, Summer, Autumn, Winter }`, et des fonctions pures qui prennent le tick du monde : `season(tick) -> Season`, `day_of_season(tick) -> u8` (0 à 6), `year(tick) -> u64`. Le tick 0 est le premier jour du printemps (le tick 0 est déjà « le matin à 6 h » dans les lumières).
- **[Proposé]** Rien n'est sauvegardé ni synchronisé : le serveur, le client et le conteneur nourriture appellent la même fonction avec le tick qu'ils connaissent. Un redémarrage ne peut donc pas dériver. Les noms affichés (Printemps, Été, Automne, Hiver) viennent avec l'affichage, plus tard.

## 4. Ce que possède le conteneur nourriture

- **[Décidé]** Le conteneur **possède la simulation** : le troupeau (vaches) et les cultures (champs). Il envoie des événements au serveur.
- **[Décidé]** Le **serveur décide tout de suite** des actions des joueurs et des nains (abattre une vache, récolter, semer, creuser un champ) et le dit ensuite au conteneur. Les rendements (4 de nourriture pour une vache, 2 pour une culture mûre) sont ajoutés à l'inventaire par le serveur, comme aujourd'hui. Le conteneur nourriture ne parle donc **pas** à l'inventaire.
- **[Proposé]** Partage de la vérité :
  - **la grille de tuiles du serveur** (`Tile::Cow`, `Field`, `Crop`, déjà sauvegardée dans `world.bin`) dit **où** sont les vaches et les champs, et de quel type ;
  - **le conteneur** possède l'état **caché** : âge et délai de reproduction des vaches, progression de la croissance des champs, et il le sauvegarde dans son propre fichier `food.bin` (même écriture atomique que l'inventaire).
- **[Proposé]** Les règles (déplacement, reproduction, croissance) sont des **fonctions pures** testables sans courtier, dans `moria-core` (module `food`) ; le conteneur ne fait que les appeler et parler au bus.

## 5. Le dialogue serveur / conteneur

- **[Proposé]** Tout est **à envoyer sans garantie** (pas de file durable) : la source de vérité des positions est la grille du serveur, et un **instantané complet** rattrape toute perte (ci-dessous).
- **Serveur → nourriture** : `food.action`, des événements « vache abattue (case) », « champ semé », « champ récolté », « champ retiré » (creusé). Le conteneur les applique de façon idempotente ; si les deux avis divergent, **celui du serveur gagne**.
- **Nourriture → serveur** : `food.events`, par tick et par montagne, **en un seul message** : déplacements de vaches (de, vers), naissances, cultures mûres. Le serveur applique chaque événement à sa grille s'il est encore valable (une case occupée entre-temps, un mur construit : l'événement est refusé et le prochain instantané recale le conteneur).
- **[Proposé]** `food.state` : battement de cœur du conteneur, chaque seconde. Le serveur le considère arrêté après 5 s sans nouvelle (même seuil que l'inventaire).
- **[Proposé]** **Instantané serveur** : au démarrage du conteneur (et quand le battement revient après une panne), puis toutes les 5 minutes (anti-entropie), le serveur envoie `food.world` : toutes les positions de vaches et de champs de chaque montagne. Le conteneur **adopte** ces positions : il garde l'état caché des entités qu'il connaît, crée un état par défaut pour celles qu'il ne connaît pas (c'est la migration des mondes existants, sans graine à gérer), et **oublie** celles qui ne sont plus sur la carte. Un conteneur qui démarre à vide ne supprime donc **jamais** de vaches de la carte.
- **[Au constructeur]** Les noms exacts des clés de routage, les formats, la taille maximale d'un message (borner avant de décoder, comme pour l'inventaire), le découpage de l'instantané s'il dépasse la limite d'un message.

## 6. Quand le conteneur est arrêté

- **[Décidé]** La montagne continue de tourner et **la nourriture gèle** : les vaches ne bougent plus, les champs ne poussent plus.
- **[Décidé]** **La faim est aussi mise en pause** (les repas des nains ne tombent plus) jusqu'au retour du conteneur.
- **[Décidé]** Abattre, récolter, semer continuent de marcher (le serveur décide seul) ; le conteneur rattrape au retour grâce à l'instantané.
- **[Proposé]** Le client voit une bannière « nourriture indisponible », comme prévu avec l'inventaire. Sans bus configuré (développement), le serveur considère la nourriture toujours disponible et exécute les mêmes règles pures **en interne**, pour que `cargo run` et les tests de simulation continuent de marcher.

## 7. Les règles du jeu

Le serveur supprime ses fonctions `graze` et `grow` de `sim.rs` au profit des règles pures du module `food`, appelées par le conteneur (ou en interne sans bus).

- **[Décidé]** Les troupeaux **se reproduisent**.
- **[Proposé]** Reproduction : seulement au printemps et en été ; une naissance apparaît sur une case d'herbe libre voisine d'une vache assez âgée ; un délai entre deux naissances ; **plafond de troupeau par montagne** (valeur au constructeur) pour qu'un troupeau ne gonfle pas sans fin et ne ralentisse pas les ticks ; quand on a abattu des vaches, le troupeau repousse tout seul sous le plafond.
- **[Décidé]** Les cultures et les saisons comptent. **[Proposé]** Un champ semé ne pousse qu'au printemps, en été et en automne ; il ne pousse pas en hiver ; il pousse plus vite en été. Une culture mûre reste mûre jusqu'à la récolte.
- **[Proposé]** Les vaches continuent de brouter (se déplacer d'une case d'herbe voisine) comme aujourd'hui, plus lentement en hiver.
- **[Au constructeur]** Tous les nombres (durées, cotes, plafond, âge de reproduction), avec une contrainte : en été, **un champ doit continuer de nourrir à peu près deux nains** (c'est l'équilibre actuel de `GROW_EVERY` / `RIPEN_ODDS`). Le hasard reste déterministe à partir d'une graine sauvegardée.

## 8. Déploiement et tests

- **[Proposé]** Une image `moria-food` construite par `server.yml` et épinglée comme `dwarf-ai` et l'inventaire ; volume `moria-food` pour `food.bin` ; réseau `infra-net` ; l'utilisateur RabbitMQ `moria` a déjà les droits sur le vhost.
- **[Proposé]** Aucun changement de `world.bin` ni du protocole client (les tuiles `Cow`, `Field`, `Crop` existent déjà). Le serveur passe la saison au client **dans un autre changement** (§2, morceau 3).
- **[Décidé]** Chaque tâche suit RED puis GREEN.
- **[Proposé]** Les règles pures se testent sans courtier (reproduction, plafond, croissance par saison, déterminisme). Les tests avec un vrai RabbitMQ (`#[ignore]`) couvrent : panne et retour du conteneur (la faim se met en pause puis reprend, les vaches sont recalées), un conteneur démarré à vide qui ne supprime rien, et le redémarrage du serveur.

## 8 bis. Précisions issues de la relecture du constructeur

Toutes ces réponses sont des choix de Claude, **[Proposé]**.

- **L'horloge.** Le tick du monde appartient au serveur. Il publie un message minuscule `food.tick` (le numéro du tick) **à chaque tick**, sans garantie de livraison. Le conteneur n'a pas d'horloge propre : il avance sa simulation **un pas par `food.tick` reçu** (déplacement des vaches tous les `COW_EVERY` ticks, croissance tous les `GROW_EVERY` ticks, saison calculée avec la fonction du §3 sur ce tick). Il est donc synchrone avec le serveur et déterministe. Pendant une panne il **rate** des ticks et, au retour, reprend au tick courant **sans rattrapage** (c'est le « gel » du §6).
- **Vaches immobilisées.** Le serveur refuse un déplacement vers une case occupée ou dont la vache est visée par une désignation, un nain ou le héros (c'est le `pinned` / `occupied` d'aujourd'hui). Il répond alors `food.reject` (de, vers) et le conteneur **annule tout de suite** le déplacement, sans attendre l'instantané. L'instantané de 5 minutes reste le filet de sécurité.
- **Naissances.** Une naissance n'existe pour le conteneur qu'après un refus éventuel : si le serveur la refuse (`food.reject`), le conteneur supprime la naissance. Le plafond du troupeau se compte sur le **nombre de vaches de la grille connue du conteneur** après traitement des refus.
- **Récolte et famine.** La désignation `Chop` d'une culture mûre **reste dans le serveur** : il la crée quand il applique l'événement « culture mûre » et la **ré-ajoute à chaque intervalle de croissance** pour les cultures déjà mûres, même si le conteneur est arrêté (sinon, en famine, les nains ne travaillent que pour la nourriture et n'auraient plus rien à faire). Seul le passage `Field` → `Crop` (et l'état caché) déménage dans le module `food`.
- **Mode sans bus.** Quand `amqp_url` est absent, le serveur fait tourner **les mêmes règles pures** en mémoire, une instance par montagne, reconstruite depuis la grille au démarrage avec l'état caché par défaut (même règle d'adoption que le conteneur). Cet état n'est **pas sauvegardé** : à chaque redémarrage de développement, âges et progressions de croissance repartent de zéro. `world.bin` ne change pas. Quand `amqp_url` est présent, la nourriture vient du conteneur ; s'il est arrêté, elle gèle.
- **Pause de la faim.** Le repas tombe aux ticks multiples de `EAT_EVERY`. Un repas dont le tick tombe pendant une panne du conteneur est **simplement sauté** (pas de repas manqué, pas de rattrapage au retour). Les nains déjà affamés le restent.
- **Ordre des messages.** Chaque message nourriture porte le **tick du serveur** auquel il se rapporte. Le conteneur ignore une action `food.action` plus ancienne que le dernier instantané `food.world` qu'il a adopté (l'instantané la contient déjà). Le serveur ignore un événement du conteneur trop ancien (de plus de quelques dizaines de ticks) ou qui contredit sa grille. **[Au constructeur]** : la fenêtre exacte, et les identifiants d'entité si le tick seul ne suffit pas à distinguer deux événements sur une même case.

## 9. Objectifs, non-objectifs, anti-objectifs

**Non-objectifs** : prédateurs, chasse, pêche, autres cultures ou animaux ; les autres effets des saisons (lumière, affichage, nains) ; plusieurs serveurs de jeu.

**Anti-objectifs (un échec même si « ça marche »)** :
- Abattre ou récolter bloqué parce que le conteneur est arrêté.
- Des vaches qui disparaissent, se dupliquent ou se téléportent après un redémarrage du conteneur.
- Un troupeau qui explose et fait déborder les ticks.
- Un conteneur qui démarre vide et efface la nourriture de la carte.
