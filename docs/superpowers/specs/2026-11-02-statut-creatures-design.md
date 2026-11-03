# Écran de statut des créatures — conception

## Intention

Le joueur veut cliquer (clic du milieu) sur un dwarf, un PNJ ou un animal et voir
un panneau avec : la faim, la santé, ce que la créature fait en ce moment, et des
statistiques détaillées. Aujourd'hui le jeu ne stocke presque rien sur les créatures,
donc le travail est de créer ces données, pas seulement de les afficher.

« PNJ » = les nains ouvriers (`Dwarf`). Le héros du joueur est aussi inspectable.

## État actuel (vérifié dans le code, branche main)

- `Hero` (`moria-core/src/world.rs`) : nom, race, classe, 6 caractéristiques, position. Ni faim ni santé.
- `Dwarf` (`world.rs`) : id, pos, tâche, `hungry: bool`. Ni nom, ni stats. Repas tous les `EAT_EVERY` ticks (`moria-server/src/sim.rs`).
- Vaches : `Tile::Cow`, de simples tuiles qui errent (`sim.rs`, `graze`), abattues par `Chop` (4 de nourriture). Pas d'identité.
- Protocole (`moria-core/src/protocol.rs`, `PROTOCOL_VERSION = 10`) : `DwarfState = (DwarfId, TilePos, Option<DwarfJob>)`, liste `hungry` complète à chaque tick. Positions de variants figées, testées par `frozen_positions`.
- Client (Bevy + bevy_egui) : `input.rs::mouse` gère clic gauche et droit seulement ; picking par tuile (`Drag.cursor`) ; fenêtre de statut existante (touche C, `ui.rs::status_window`) dont on réutilise le style ; `job_label` et `ability_rows` réutilisables ; textes via `tr(fr, en)`.

## Buts

1. Clic du milieu sur une créature ouvre un panneau de statut ; fermeture par Échap ou second clic.
2. Le panneau montre faim, santé, activité courante et statistiques détaillées, pour héros, nains et vaches.
3. Les données sont réelles (simulées côté serveur), pas inventées côté client.

## Hors-buts

- Aucune cause de dégâts autre que la faim (ni combat, ni ennemis, ni chutes). À ajouter plus tard.
- Pas d'inventaire par créature, pas d'âge, pas de compétences évolutives sauf si le builder les juge nécessaires aux « stats détaillées » (voir « Laissé au builder »).

## Anti-buts

- Alourdir chaque tick réseau avec des données que personne ne regarde.
- Casser la compatibilité figée du protocole sans bump de `PROTOCOL_VERSION` ni mise à jour des tests.
- Un panneau qui ment (valeurs calculées côté client différentes du serveur).

## Découpage décidé par l'utilisateur (dans cet ordre, les trois sont voulus)

### Étape 1 — Clic du milieu et panneau minimal
- `MouseButton::Middle` dans `input.rs::mouse`, en respectant `wants_pointer_input()` et la pause, sans toucher à `Drag`.
- Ressource d'inspection (cible sélectionnée) résolue depuis la tuile sous le curseur contre `Game.dwarves` (même niveau z) et la position du héros.
- Fenêtre egui calquée sur `status_window`. Contenu avec les données déjà côté client : activité (`job_label`), faim oui/non (« Affamé / Rassasié »), caractéristiques du héros (`ability_rows`).

### Étape 2 — Santé, jauge de faim, stats détaillées (héros et nains)
- Choix de l'utilisateur : la santé ne baisse que par la faim prolongée ; elle remonte quand la créature est nourrie.
- Ajouter faim en jauge, santé et les stats détaillées à `Dwarf` et `Hero`, les faire évoluer dans `sim.rs`, les exposer au client.
- Transport : message `Inspect(DwarfId)` avec réponse à la demande **ou** struct `DwarfView` dans le snapshot/delta. Pas décidé : voir « Laissé au builder » (indication : la limite de 1024 nains et le coût par tick plaident pour la réponse à la demande).
- Bump de `PROTOCOL_VERSION`, positions figées préservées, `frozen_positions` à jour.

### Étape 3 — Vaches comme entités
- Remplacer `Tile::Cow` par des entités avec identité et état (faim, santé), sélectionnables, avec le même panneau.
- Garder le comportement existant : errance (`graze`), abattage `Chop` = 4 de nourriture, interaction avec le conteneur de nourriture (`moria-food`) et la limite de troupeau.

## Contraintes

- TDD obligatoire pour chaque tâche : test RED puis implémentation GREEN (exigence de l'utilisateur sur Moria).
- Textes en FR/EN via `tr(fr, en)`. Tests unitaires egui existants comme modèle.
- Serveur autoritaire ; clippy, fmt et tests du workspace verts.
- Chaque étape doit être livrable seule.

## Choix par défaut que j'ai faits (à confirmer ou corriger)

- Clic du milieu = ouverture ; Échap ou second clic = fermeture.
- Le héros est inspectable comme les autres.
- Un clic du milieu sur une case sans créature ne fait rien.

## Mort (décidé par l'utilisateur : « il meurt »)

- Un nain dont la santé atteint 0 par la faim **meurt** : il est retiré de la forteresse, sa tâche est annulée et la désignation redevient disponible, son id n'est jamais réutilisé, le client le retire de l'affichage, et un `Inspect` sur lui répond « introuvable ».
- Mes précisions (à corriger si besoin) : le héros et les vaches **ne meurent pas** dans ce périmètre. Le héros n'a pas de repas aujourd'hui, donc sa santé reste pleine ; sa jauge de faim et sa santé s'affichent mais ne tuent pas. Les vaches broutent, donc elles restent nourries et en pleine santé. Étendre la mort à eux plus tard est possible.
- La mort d'un nain doit apparaître dans le journal ou un message à l'écran (forme laissée au builder), et réduire le compteur de nains de la barre du haut.

## Autres choix de ma part (suite à la relecture, à corriger si besoin)

- **Faim** : le gameplay actuel ne change pas. `hungry` (repas toutes les `EAT_EVERY` ticks) reste la source de vérité et continue de piloter tâches et consommation de nourriture. La jauge s'ajoute à côté : elle se vide entre deux repas et sert à l'affichage ; la santé baisse quand le nain est `hungry` depuis longtemps. Les tests existants de `sim.rs` doivent rester verts sans modification.
- **Cible d'inspection** : un enum `InspectTarget { Hero, Dwarf(id), Cow(id) }`. Un seul héros par forteresse (le sien) ; on ne clique pas les héros d'autres joueurs.
- **Rafraîchissement** : le client demande une fois à l'ouverture, puis redemande environ chaque seconde tant que le panneau est ouvert. La réponse a une variante « introuvable » ; le panneau affiche alors « Disparu » et se ferme.
- **Vaches (étape 3)** : une liste `Vec<Cow>` avec ids dans la forteresse ; `Tile::Cow` disparaît de la grille. Leurs positions voyagent dans le snapshot et les deltas comme celles des nains ; seuls faim, santé et stats passent par `Inspect`. Les montagnes sauvegardées sont migrées au chargement (chaque ancienne tuile vache devient une entité).
- **Plusieurs créatures sur la même case** : priorité héros, puis nain, puis vache.

## Laissé au builder

- Forme exacte des « statistiques détaillées » pour un nain et une vache (valeurs, noms), en restant simples.
- Constantes d'équilibrage : vitesse de baisse/remontée de la santé, seuils de la jauge de faim.
- Choix du transport (`Inspect` à la demande ou struct dans les deltas), sous les contraintes ci-dessus.
- Migration des sauvegardes existantes (valeurs par défaut pour les anciens nains).
- Représentation des vaches en entités et migration des vaches existantes dans les montagnes sauvegardées.
