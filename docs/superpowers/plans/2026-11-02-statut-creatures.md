# Écran de statut des créatures — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Un clic du milieu sur le héros, un nain ou une vache ouvre un panneau avec faim, santé, activité et statistiques, alimenté par des données réellement simulées côté serveur.

**Architecture:** Trois étapes livrables seules. (1) Sélection client + panneau avec les données déjà connues. (2) Jauge de faim, santé, mort des nains et statistiques simulées dans `moria-server`, exposées à la demande par un nouveau message `Inspect` (pas dans chaque tick). (3) Les vaches deviennent des entités `Cow` avec id, dans la forteresse, et passent par le même panneau.

**Tech Stack:** Rust workspace (`moria-core`, `moria-server`, `moria-client`), Bevy 0.20 + bevy_egui 0.43, postcard.

**Spec:** `docs/superpowers/specs/2026-11-02-statut-creatures-design.md`

## Global Constraints

- TDD obligatoire à chaque tâche : écrire le test, le voir échouer (RED), implémenter, le voir passer (GREEN).
- Travailler dans un worktree dédié créé depuis `main` (une branche par étape de préférence), jamais directement sur `main`.
- Textes visibles via `tr(fr, en)` (voir `moria-client/src/ui.rs`).
- Le protocole garde `ClientMsg::Hello` = variant 0, `ServerMsg::Reject` = variant 1, `Reject::VersionMismatch` = variant 0. Les nouveaux variants s'ajoutent **à la fin** des enums. `PROTOCOL_VERSION` passe de 10 à 11 (étape 2) et `frozen_positions` (`moria-core/src/protocol.rs`) reste vert.
- Le gameplay de la faim ne change pas : `Dwarf.hungry` et `EAT_EVERY` (`moria-server/src/sim.rs`) restent la source de vérité. Les tests existants de `sim.rs` passent sans modification.
- Aucune cause de dégâts autre que la faim. Seuls les nains peuvent mourir (le héros et les vaches affichent faim et santé sans mourir).
- À la fin de chaque tâche : `cargo fmt`, `cargo clippy --workspace --all-targets`, `cargo test --workspace` verts.
- **Étape 3 : prérequis.** La branche `worktree-nourriture` (conteneur `moria-food`, qui lit les vaches via `Tile::Cow`) n'est pas fusionnée dans `main`. L'étape 3 ne démarre qu'après sa fusion, sinon les conflits portent sur `sim.rs`, `food.rs` et le protocole. Vérifier aussi `worktree-elevage` et `worktree-agriculture` pour tout code qui touche aux vaches.

## Review Focus

- Clic du milieu sur une case vide, hors carte ou sous l'interface egui : rien ne s'ouvre, aucune panique.
- Le nain inspecté meurt panneau ouvert : le panneau affiche « Disparu » puis se ferme.
- Un nain meurt en pleine tâche : la désignation redevient disponible pour un autre nain, et la règle de famine (« si quelqu'un a faim, tous cherchent à manger ») cesse quand le dernier affamé meurt.
- Le dernier nain meurt : zéro nain, la barre du haut et le panneau d'état (touche C) ne plantent pas.
- `Inspect` avec un id inconnu, déjà mort, ou envoyé sans héros créé : réponse « introuvable » ou ignoré, jamais de panique serveur ; répétition ~1/s par client sans impact sur le budget de tick (`tick_budget_ten_full_fortresses`).
- Ancienne sauvegarde : chaque nain chargé reçoit faim et santé pleines ; chaque ancienne tuile `Cow` devient une entité (étape 3).

---

## Étape 1 — Clic du milieu et panneau minimal

### Task 1: Cible d'inspection et résolution case → créature

**Files:**
- Modify: `crates/moria-core/src/protocol.rs` (le type, car partagé avec le futur `Inspect`)
- Modify: `crates/moria-client/src/game.rs`
- Test: `crates/moria-client/src/game.rs` (module `tests` existant)

**Interfaces:**
- Produces (core, réexporté comme les autres types du protocole) :
  `pub enum InspectTarget { Hero, Dwarf(DwarfId), Cow(CowId) }` avec `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]`. `CowId` n'existe pas encore : définir `pub struct CowId(pub u32)` dans `moria-core/src/world.rs` à côté de `DwarfId`, avec les mêmes derives que `DwarfId`, ainsi le variant `Cow` est figé dès maintenant. Le variant ne sert qu'à l'étape 3.
- Produces (client) : `impl Game { pub fn inspect_at(&self, tile: (i16, i16), z: i16) -> Option<InspectTarget> }` ; champ `pub inspect: Option<InspectTarget>` dans `Game`.

- [ ] **Step 1: Write the failing tests** `inspect_at_prefers_hero_over_dwarf`, `inspect_at_finds_dwarf_on_current_level`, `inspect_at_ignores_other_levels_and_empty_tiles` dans `game.rs`. Assertions : héros et nain sur la même case → `Some(InspectTarget::Hero)` ; nain seul → `Some(InspectTarget::Dwarf(id))` ; même (x, y) mais `z` différent → `None` ; case vide → `None`. Construire le `Game` comme les autres tests du module.
- [ ] **Step 2: Run** `cargo test -p moria-client inspect_at` — attendu : échec de compilation ou FAIL (fonction absente).
- [ ] **Step 3: Implement** `InspectTarget`, `CowId`, `Game.inspect` (init `None`) et `inspect_at` : héros d'abord, puis le premier `DwarfState` à cette position. Priorité héros > nain > vache (la vache viendra à l'étape 3).
- [ ] **Step 4: Run** `cargo test -p moria-client inspect_at` — attendu : PASS.
- [ ] **Step 5: Commit** `feat(client): InspectTarget et résolution case → créature`

### Task 2: Clic du milieu ouvre et ferme la sélection

**Files:**
- Modify: `crates/moria-client/src/input.rs` (`fn mouse`, ~ligne 306, et clavier pour Échap)
- Test: `crates/moria-client/src/input.rs` (module `tests`)

**Interfaces:**
- Consumes: `Game::inspect_at`, `Game.inspect` (Task 1), `Drag.cursor`.
- Produces: `pub fn middle_click(g: &mut Game, cursor: Option<(i16, i16)>)` : logique pure extraite pour être testable. Cible trouvée → `g.inspect = Some(cible)` ; même cible déjà ouverte → `None` (fermeture) ; aucune cible → ne change rien.

- [ ] **Step 1: Write the failing tests** `middle_click_opens_dwarf`, `middle_click_same_target_closes`, `middle_click_on_empty_tile_keeps_state`, `middle_click_without_cursor_does_nothing`.
- [ ] **Step 2: Run** `cargo test -p moria-client middle_click` — attendu : FAIL.
- [ ] **Step 3: Implement** `middle_click` puis l'appeler depuis `mouse` quand `buttons.just_pressed(MouseButton::Middle)`, après les gardes existantes (`!egui.wants_pointer_input()`, jeu non en pause) et sans toucher à `Drag.start`. Faire fermer `g.inspect` par Échap, à côté de la fermeture du statut (chercher où `g.status` est remis à `false` avec Échap).
- [ ] **Step 4: Run** `cargo test -p moria-client middle_click` puis `cargo test -p moria-client` — attendu : PASS.
- [ ] **Step 5: Commit** `feat(client): clic du milieu sélectionne une créature`

### Task 3: Fenêtre `inspect_window` (données déjà connues)

**Files:**
- Modify: `crates/moria-client/src/ui.rs` (calquer `status` / `status_window`, ~ligne 428 ; enregistrer le système dans `EguiPrimaryContextPass`, ~ligne 50)
- Test: `crates/moria-client/src/ui.rs` (modèle : les tests egui existants, ~lignes 800–1024)

**Interfaces:**
- Consumes: `Game.inspect`, `job_label`, `ability_rows`, `Game.hungry`, `tr`, `below_bar`.
- Produces: `fn inspect_window(ctx: &egui::Context, g: &Game)` ; fonction pure `pub fn inspect_lines(g: &Game, t: InspectTarget) -> Vec<(String, String)>` (libellé, valeur) pour les tester sans egui.

- [ ] **Step 1: Write the failing tests** `inspect_lines_for_hero_has_abilities_and_no_hunger_state`, `inspect_lines_for_dwarf_shows_activity_and_hungry_state` : un nain `hungry` donne « Affamé », sinon « Rassasié » (via `tr`) ; son activité vient de `job_label` ; un nain inconnu de `g.dwarves` donne une liste vide.
- [ ] **Step 2: Run** `cargo test -p moria-client inspect_lines` — attendu : FAIL.
- [ ] **Step 3: Implement** `inspect_lines`, la fenêtre (sans barre de titre, ancrée RIGHT_TOP sous la barre, décalée sous `status_window` si les deux sont ouvertes) et le système Bevy. Ligne d'aide : « Clic du milieu ou Échap : fermer ».
- [ ] **Step 4: Run** `cargo test -p moria-client` puis lancer le jeu (skill `run`) et vérifier à la main : clic du milieu sur le héros et sur un nain, fermeture au second clic et par Échap.
- [ ] **Step 5: Commit** `feat(client): panneau de statut d'une créature (étape 1)`

---

## Étape 2 — Santé, jauge de faim, mort, statistiques (héros et nains)

### Task 4: Données de vitalité dans le core + migration des sauvegardes

**Files:**
- Modify: `crates/moria-core/src/world.rs` (`Dwarf` ~ligne 71, `Hero` ~ligne 45, création des nains/héros)
- Test: `crates/moria-core/src/world.rs` (module `tests`)

**Interfaces:**
- Produces: constantes `pub const FOOD_MAX: u16`, `pub const HEALTH_MAX: u16` ; nouveaux champs `food: u16`, `health: u16` sur `Dwarf` et sur `Hero`, et sur `Dwarf` seulement `ticks_alive: u64`, `jobs_done: u32`. `FOOD_MAX` doit valoir `EAT_EVERY` du serveur (le déclarer dans le core et l'importer côté serveur pour garder une seule valeur).

- [ ] **Step 1: Write the failing tests** `new_dwarf_starts_fed_and_healthy` (faim = `FOOD_MAX`, santé = `HEALTH_MAX`, compteurs à 0), `old_save_loads_with_full_vitals` : charger un `World` sérialisé **avec l'ancien format** de `Dwarf`/`Hero` et vérifier qu'il se charge avec des vitalités pleines. Avant d'écrire ce test, lire comment les sauvegardes sont (dé)sérialisées (postcard n'est pas auto-descriptif : `#[serde(default)]` ne suffit pas pour un champ ajouté en cours de structure). Le mécanisme de migration (version de format, ou structure ancienne dédiée) est au choix du builder ; le test impose seulement le résultat.
- [ ] **Step 2: Run** `cargo test -p moria-core vitals` — attendu : FAIL.
- [ ] **Step 3: Implement** les champs, les constantes et la migration.
- [ ] **Step 4: Run** `cargo test --workspace` — attendu : PASS (corriger les constructions de `Dwarf`/`Hero` des tests existants avec des valeurs pleines, sans changer leurs assertions).
- [ ] **Step 5: Commit** `feat(core): faim, santé et compteurs sur Dwarf et Hero`

### Task 5: Simulation — jauge, dégâts de faim, régénération, compteurs

**Files:**
- Modify: `crates/moria-server/src/sim.rs` (`step`, bloc des repas ~lignes 485–499)
- Test: `crates/moria-server/src/sim.rs` (modèle : `a_hungry_dwarf_stops_working_until_it_has_eaten`, ligne ~1639, pour construire un monde et avancer avec `step`)

**Interfaces:**
- Consumes: `FOOD_MAX`, `HEALTH_MAX`, champs de la Task 4.
- Produces: constantes serveur `HUNGER_DAMAGE_EVERY: u64`, `REGEN_EVERY: u64` (valeurs d'équilibrage au choix du builder ; les tests les référencent par nom) ; règle : `food` baisse de 1 par tick (plancher 0) et revient à `FOOD_MAX` quand le nain mange ; tant que `hungry`, `health` perd 1 tous les `HUNGER_DAMAGE_EVERY` ticks ; sinon elle regagne 1 tous les `REGEN_EVERY` ticks (plafond `HEALTH_MAX`) ; `ticks_alive` +1 par tick ; `jobs_done` +1 à l'achèvement d'une tâche. Même règle de faim/santé pour le héros, mais sa santé ne descend jamais sous 1.

- [ ] **Step 1: Write the failing tests** `food_gauge_drains_and_refills_on_meal`, `hungry_dwarf_loses_health_over_time`, `fed_dwarf_regenerates_health`, `ticks_alive_and_jobs_done_count`, `hero_health_never_below_one`. Chaque test construit un monde, force `hungry`/nourriture comme les tests existants, avance de N ticks avec `step` et compare aux constantes nommées.
- [ ] **Step 2: Run** `cargo test -p moria-server vitals` (préfixer les noms de test par `vitals_`) — attendu : FAIL.
- [ ] **Step 3: Implement** dans la boucle de `step`, sans toucher à la logique `hungry`/repas existante.
- [ ] **Step 4: Run** `cargo test -p moria-server` — attendu : PASS, **dont tous les tests de faim préexistants inchangés**.
- [ ] **Step 5: Commit** `feat(server): jauge de faim, santé et compteurs`

### Task 6: Mort d'un nain

**Files:**
- Modify: `crates/moria-server/src/sim.rs` (`step`, et le suivi des désignations/tâches liées à `Dwarf.task`)
- Test: `crates/moria-server/src/sim.rs`

**Interfaces:**
- Consumes: Task 5.
- Produces: à `health == 0`, le nain est retiré de `Fortress.dwarves` ; sa désignation redevient assignable ; son `DwarfId` n'est jamais réutilisé (`World.next_dwarf` ne recule pas) ; les morts du tick sont mémorisées dans le delta : champ `died: Vec<DwarfId>` ajouté à `TickDelta` (le champ lui-même est ajouté à la Task 7 avec le bump ; ici, utiliser un retour interne et brancher le champ en Task 7).

- [ ] **Step 1: Write the failing tests** `dwarf_dies_at_zero_health`, `dead_dwarf_releases_its_designation` (une autre unité la reprend au tick suivant), `famine_ends_when_last_hungry_dwarf_dies`, `last_dwarf_can_die_without_panic`, `dead_dwarf_id_not_reused`.
- [ ] **Step 2: Run** `cargo test -p moria-server dies` — attendu : FAIL.
- [ ] **Step 3: Implement** le retrait après la mise à jour de santé ; vérifier que `dwarf-ai` (`moria-core/src/ai.rs` : `DwarfView`, `Assignments`) ne garde pas de référence vers l'id mort (`apply_assignments`, `sim.rs` ~ligne 330, doit ignorer un id inconnu).
- [ ] **Step 4: Run** `cargo test --workspace` — attendu : PASS.
- [ ] **Step 5: Commit** `feat(server): la faim tue les nains`

### Task 7: Protocole 11 — `Inspect`, `Inspected`, `died`

**Files:**
- Modify: `crates/moria-core/src/protocol.rs`, `crates/moria-server/src/lib.rs` (dispatch des `ClientMsg`, `self.send(...)`, ~lignes 375–562), `crates/moria-server/src/sim.rs` (`Command::from_client` ~ligne 62 : `Inspect` n'est pas une commande, retourne `None`)
- Test: `crates/moria-core/src/protocol.rs`, `crates/moria-server/src/lib.rs` (tests de serveur existants)

**Interfaces:**
- Consumes: `InspectTarget` (Task 1), vitalité et compteurs (Tasks 4–6).
- Produces:
  - `ClientMsg::Inspect(InspectTarget)` en dernier variant.
  - `pub struct Inspection { pub target: InspectTarget, pub food: u16, pub health: u16, pub ticks_alive: u64, pub jobs_done: u32 }` ; `ServerMsg::Inspected(Option<Inspection>)` en dernier variant (`None` = introuvable).
  - `TickDelta.died: Vec<DwarfId>`.
  - `PROTOCOL_VERSION = 11`.
  - côté serveur : `fn inspect(w: &World, owner: PlayerId, t: InspectTarget) -> Option<Inspection>` dans `sim.rs` (`Hero` : valeurs du héros avec `jobs_done = 0`, `ticks_alive = 0` s'il n'existe pas de compteur ; `Cow` : `None` jusqu'à l'étape 3).

- [ ] **Step 1: Write the failing tests** : `frozen_positions` toujours vert ; `inspect_roundtrip` (encode/decode de `ClientMsg::Inspect` et `ServerMsg::Inspected`) ; côté serveur `inspect_returns_dwarf_vitals`, `inspect_unknown_or_dead_dwarf_is_none`, `inspect_without_hero_is_ignored`, `inspect_cannot_read_another_players_dwarf`.
- [ ] **Step 2: Run** `cargo test -p moria-core protocol && cargo test -p moria-server inspect` — attendu : FAIL.
- [ ] **Step 3: Implement** les types, le bump, `inspect`, et la réponse envoyée uniquement au joueur qui a demandé. Alimenter `died` depuis la Task 6.
- [ ] **Step 4: Run** `cargo test --workspace` — attendu : PASS. Mettre à jour tout test d'intégration qui fixe `PROTOCOL_VERSION`.
- [ ] **Step 5: Commit** `feat(protocol): Inspect/Inspected et morts dans le tick (v11)`

### Task 8: Client — requête périodique, jauges, « Disparu »

**Files:**
- Modify: `crates/moria-client/src/game.rs` (réception de `ServerMsg::Inspected`, de `TickDelta.died`), `crates/moria-client/src/ui.rs`, `crates/moria-client/src/input.rs` (envoi)
- Test: `crates/moria-client/src/game.rs`, `crates/moria-client/src/ui.rs`

**Interfaces:**
- Consumes: `ClientMsg::Inspect`, `ServerMsg::Inspected`, `Game.inspect`.
- Produces: `Game.inspection: Option<Inspection>` ; `Game.inspect_due: bool` ou minuteur d'environ 1 s tant que le panneau est ouvert (une requête à l'ouverture, puis ~1/s) ; `inspect_lines` (Task 3) étendu avec faim (jauge `food/FOOD_MAX`), santé (`health/HEALTH_MAX`), âge (`ticks_alive` en jours de jeu), tâches accomplies ; un `Inspected(None)` pour la cible ouverte met `g.inspect = None` et affiche un message « Disparu » (ou équivalent via le mécanisme de message existant, forme au choix du builder) ; `TickDelta.died` affiche un message de mort et le compteur de nains de la barre baisse naturellement via `dwarves`.

- [ ] **Step 1: Write the failing tests** `inspected_none_closes_panel_with_message`, `inspect_requested_on_open_then_at_most_once_per_second`, `inspect_lines_show_gauges_and_stats`, `died_in_delta_reports_message`, `status_page_with_zero_dwarves_does_not_panic`.
- [ ] **Step 2: Run** `cargo test -p moria-client inspect` — attendu : FAIL.
- [ ] **Step 3: Implement** le minuteur (réutiliser la ressource de temps Bevy déjà présente dans `input.rs`/`game.rs`, pas de nouvelle dépendance), la mise à jour de `Game` à la réception, et l'affichage.
- [ ] **Step 4: Run** `cargo test --workspace` ; jeu lancé à la main : laisser un nain mourir de faim panneau ouvert (`HUNGER_DAMAGE_EVERY` réduit temporairement) et vérifier « Disparu ».
- [ ] **Step 5: Commit** `feat(client): jauges, statistiques et fin de vie dans le panneau (étape 2)`

---

## Étape 3 — Vaches comme entités (après fusion de `nourriture`)

### Task 9: Entité `Cow`, génération et migration

**Files:**
- Modify: `crates/moria-core/src/world.rs` (génération ~ligne 239, `Fortress` ~ligne 80, `World.next_dwarf` à côté duquel ajouter `next_cow`), `crates/moria-core/src/mountain.rs` (~lignes 189 et 584), `crates/moria-core/src/tile.rs` (~lignes 58, 370, 385)
- Test: `crates/moria-core/src/world.rs`

**Interfaces:**
- Produces: `pub struct Cow { pub id: CowId, pub pos: TilePos, pub food: u16, pub health: u16 }` ; `Fortress.cows: Vec<Cow>` ; `World.next_cow: u32`. Le variant `Tile::Cow` **reste dans l'enum** (ne pas renuméroter les variants, sinon sauvegardes et protocole cassent) mais la génération ne le pose plus ; il ne sert que de format de lecture des anciennes sauvegardes.

- [ ] **Step 1: Write the failing tests** `new_mountain_has_cow_entities_and_no_cow_tiles` (même nombre de vaches qu'avant sur la même graine), `old_save_cow_tiles_become_entities_and_tiles_become_grass`, `cow_ids_unique_and_not_reused`.
- [ ] **Step 2: Run** `cargo test -p moria-core cow` — attendu : FAIL.
- [ ] **Step 3: Implement** génération et migration au chargement.
- [ ] **Step 4: Run** `cargo test --workspace` — attendu : certains tests de `sim.rs` qui posent `Tile::Cow` échouent ; les migrer en Task 10, pas ici.
- [ ] **Step 5: Commit** `feat(core): les vaches deviennent des entités`

### Task 10: Simulation des vaches — errance, abattage, troupeau, conteneur de nourriture

**Files:**
- Modify: `crates/moria-server/src/sim.rs` (`graze` ~ligne 447, désignation `Chop` ~lignes 261–274, tests ~lignes 1550–1865), `crates/moria-food` et le pont de nourriture du serveur (après fusion de `nourriture`), `crates/moria-core/src/mountain.rs:189`
- Test: `crates/moria-server/src/sim.rs`

**Interfaces:**
- Consumes: `Fortress.cows`, `Cow` (Task 9).
- Produces: `graze` déplace les entités (`COW_EVERY` inchangé, même règle d'herbe voisine) ; `Chop` sur une case occupée par une vache retire l'entité et donne 4 de nourriture en 20 ticks (valeurs de `tile.rs`, inchangées) ; la limite de troupeau et l'envoi des vaches au conteneur `moria-food` lisent `Fortress.cows`.

- [ ] **Step 1: Write the failing tests** : migrer les tests existants de `sim.rs` qui utilisent `Tile::Cow` (~lignes 1550–1865) vers des entités, en conservant leurs assertions et leurs noms (ils sont les tests RED de cette tâche), plus `chop_on_cow_entity_removes_it_and_gives_four_food`.
- [ ] **Step 2: Run** `cargo test -p moria-server` — attendu : FAIL sur les tests migrés.
- [ ] **Step 3: Implement** la simulation sur les entités, y compris les tests du conteneur de nourriture et les tests e2e.
- [ ] **Step 4: Run** `cargo test --workspace` — attendu : PASS.
- [ ] **Step 5: Commit** `refactor(server): graze et abattage sur des entités vaches`

### Task 11: Vaches dans le protocole, le rendu, la sélection et le panneau

**Files:**
- Modify: `crates/moria-core/src/protocol.rs`, `crates/moria-server/src/sim.rs` (`inspect`, vues), `crates/moria-client/src/{game,render,terrain,ui}.rs`
- Test: les mêmes modules

**Interfaces:**
- Consumes: `Cow`, `InspectTarget::Cow`, `Inspection`.
- Produces: `FortressView.cows: Vec<(CowId, TilePos)>` et `TickDelta.cows: Vec<(CowId, TilePos)>` (liste complète, comme `dwarves`) ; `PROTOCOL_VERSION = 12` ; rendu des vaches comme des sprites à la position des entités (le rendu actuel lit `Tile::Cow` dans `terrain.rs`/`render.rs`) ; `inspect_at` renvoie `Cow(id)` après héros et nain ; `inspect` serveur renvoie `Some(Inspection)` pour une vache (jobs_done = 0, affiché masqué côté client).

- [ ] **Step 1: Write the failing tests** `inspect_at_finds_cow_after_hero_and_dwarf`, `snapshot_and_delta_carry_cows`, `inspect_cow_returns_vitals`, `inspect_unknown_cow_is_none`, `cow_panel_lines_show_food_and_health_but_no_job`.
- [ ] **Step 2: Run** `cargo test --workspace cow` — attendu : FAIL.
- [ ] **Step 3: Implement** protocole (bump 12, variants/champs ajoutés sans casser `frozen_positions`), rendu et sélection.
- [ ] **Step 4: Run** `cargo test --workspace`, `cargo clippy`, `cargo fmt --check` ; jeu lancé à la main : clic du milieu sur une vache, qui bouge et broute pendant que le panneau est ouvert.
- [ ] **Step 5: Commit** `feat: panneau de statut des vaches (étape 3)`

---

## Self-review

- **Couverture de la spec :** clic du milieu et fermeture (T2), panneau minimal (T3), santé/faim/stats (T4–T5), mort (T6), `Inspect` à la demande et rafraîchissement ~1 s (T7–T8), vaches entités, migration, nourriture (T9–T11), priorité héros > nain > vache (T1, T11), version de protocole et `frozen_positions` (T7, T11), choix « le gameplay de la faim ne change pas » (T5).
- **Cohérence des types :** `InspectTarget`, `CowId`, `Inspection`, `FOOD_MAX`, `HEALTH_MAX`, `Game.inspect`, `Game.inspection` ont une seule définition (T1, T4, T7, T8).
- **Hors périmètre volontaire :** héros et vaches ne meurent pas ; aucune autre cause de dégâts ; âge et compétences évolutives non faits (ni demandés).
