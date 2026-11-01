# Le dehors et le bois — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every fortress a private outdoor area at the foot of its mountain: forest, river, lake and walkable hills that change level. Add wood as a resource: the hero chops trees and dwarves chop designated ones. Wood builds a bed, a table and a chair alongside the existing stone versions.

**Architecture:** The per-fortress `Mountain` map grows to 176 × 96 × 12. The mountain sits on the east 48 columns. Four sky levels sit above ground level `GROUND_Z = 4`, with eight underground levels below it. Horizontal movement becomes `Mountain::move_dest(from, dir) -> Option<TilePos>`, so a `Slope` tile can carry a walker one level up or down. Pathfinding, the server sim and client prediction all go through that one function. Chopping and wood furniture reuse the existing designation and build pipeline, with costs generalized from "1 stone" to a `Resources` cost per `DesignationKind`. The world is regenerated, with no migration.

**Tech Stack:** Rust 1.98 (edition 2024), Bevy 0.20 + bevy_egui, tokio, async-nats, postcard, rand_chacha. These are the existing workspace dependencies; add none.

**Spec:** `docs/superpowers/specs/2026-11-01-dehors-et-bois-design.md`. Parent spec: `docs/superpowers/specs/2026-10-31-moria-design.md`.

## Global Constraints

- **No migration.** Bump `SAVE_VERSION` from 1 to 2. An old `world.bin` is refused with the existing French error.
- **Protocol.** `PROTOCOL_VERSION` becomes 4. PR #13 sets it to 3. The frozen positions of `Hello`, `Reject` and `VersionMismatch` stay intact.
- **Map size.** `MOUNTAIN_W = 176`, `MOUNTAIN_H = 96`, `MOUNTAIN_D = 12`, `GROUND_Z = 4`. The values below `GROUND_Z` are sky levels and the values above it are underground (`z` grows downward). Keep the constant names, since every crate already validates against them.
- **Layout.** Outdoors is x 0..128. The mountain is x 128..176. The entrance room is x 128..134, y 46..50, z `GROUND_Z`, and it opens west onto the grass at x 127.
- **Lake.** Its centre is about 100 tiles west of the entrance (x ≈ 28, y ≈ 48).
- **Water** is never walkable, diggable or buildable. **Sky** is never walkable, diggable or buildable. Nothing falls.
- **Outdoors works like indoors:** dig the hills and the soil, build on grass, stairs on grass.
- **A tree** takes 20 dig ticks to chop. It yields 3 wood and leaves `Grass`.
- **Costs.** Stone furniture keeps PR #13's 1 stone. Wood table 4, wood bed 3, wood chair 2. Digging a built piece returns its full cost.
- **Security validations** are raised to the new size and never removed: AI view validation, and client snapshot validation (w, h ≤ 256, d ≤ 64).
- **Load.** The 10-bot load check stays at p99 < 150 ms. The sim tick budget test stays at a mean step < 10 ms.
- **Language.** All user-facing text is French.

## Review Focus

1. **A dwarf ordered to chop a tree that is cut off by water** must never be assigned it. The tree stays designated and other work goes on. → Task 6 `tree_across_water_not_assigned`.
2. **Walking off a hill edge with no slope** must not move the walker, so nobody falls or walks into sky. Climbing must not be possible without a slope. → Task 3 `no_fall_off_hill_edge` and `no_climb_without_slope`.
3. **A wood build designated with not enough wood** must wait. It must not take a dwarf in a loop or spend stone. The same holds for stone builds when wood is plentiful. → Task 5 `wood_build_waits_without_wood` and `costs_are_per_resource`.
4. **The bigger map** must keep snapshots and AI views under the 1 MiB frame limit, and must keep the tick budget. → Task 4 `full_map_encodes_under_max_frame` and Task 5 `tick_budget_ten_full_fortresses` (updated).
5. **A deploy over an old volume** must fail with the clear message, and the README must say how to reset the world. → Task 4 `v1_save_is_refused` and Task 8 README step.

---

## File structure (changes only)

```
crates/moria-core/src/tile.rs       new Tile/DesignationKind variants, Resources.wood, cost()
crates/moria-core/src/mountain.rs   constants, GROUND_Z, move_dest, walkable/diggable rules, chop in dig()
crates/moria-core/src/path.rs       BFS via move_dest
crates/moria-core/src/world.rs      outdoor generator, entrance constants, SAVE_VERSION 2
crates/moria-core/src/protocol.rs   PROTOCOL_VERSION 4
crates/moria-server/src/sim.rs      generalized build costs/reservation, chop, move_dest
crates/moria-server/src/lib.rs      AI view: drop unaffordable builds per resource; entrance spawn
crates/moria-dwarf-ai/src/assign.rs pathing via core (no logic change beyond tests)
crates/moria-client/src/{game,input,render,terrain,art,ui}.rs  move_dest prediction, level labels, new tiles/textures, Couper mode, wood in menu/HUD
crates/moria-server/examples/bots.rs, tests/e2e.rs  entrance-relative coordinates
README.md                            reset-the-world instructions
```

---

### Task 1: Land PR #13 (stone furniture)

**Files:** none new. Branch `worktree-furniture` / PR #13.

**Interfaces — Produces (relied on by later tasks):**
- `Tile::{Bed, Table, Chair}`
- `DesignationKind::{Bed, Table, Chair}`
- `DesignationKind::builds(self) -> Option<Tile>`
- `Tile::is_furniture(self) -> bool`
- `ClientMsg::Build(Dir, DesignationKind)`
- the client `BUILDS` menu table
- `PROTOCOL_VERSION = 3`

- [ ] **Step 1:** Check out PR #13 and merge `main` into it. Run `cargo test --workspace` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`. Expected: all green.
- [ ] **Step 2:** Mark the PR ready (`gh pr ready 13`) and wait for CI to pass (`gh pr checks 13 --watch`).
- [ ] **Step 3: STOP and ask the user to approve the merge of PR #13.** Merge it (`gh pr merge 13 --merge`) only after they say yes. Then branch this work from the updated `main`. Do not merge PR #12 as part of this plan.

### Task 2: Core tiles, wood resource, costs

**Files:**
- Modify: `crates/moria-core/src/tile.rs`
- Test: same file, `#[cfg(test)]`

**Interfaces — Produces:**
- `Tile` gains `Grass, Tree, Water, Slope, Soil, Sky, WoodBed, WoodTable, WoodChair`.
- `Resources` gains `wood: u32`, with `add` saturating.
- `Resources::covers(&self, cost: &Resources) -> bool` and `Resources::take(&mut self, cost: &Resources)` (saturating).
- `DesignationKind` gains `Chop, WoodBed, WoodTable, WoodChair`.
- `DesignationKind::cost(self) -> Resources`:
  - Wall/Bed/Table/Chair → stone 1;
  - WoodTable → wood 4, WoodBed → wood 3, WoodChair → wood 2;
  - Dig/Stairs/Chop → zero.
- `DesignationKind::builds(self) -> Option<Tile>` covers the three wood kinds.
- `DesignationKind::is_build(self) -> bool` is true when `builds().is_some()`.
- `toggled()` cycles Dig → Stairs → Chop → Wall → Dig. Furniture kinds go back to Dig, as in PR #13.
- `Tile` rules:
  - `is_solid`: Rock, Gold, Mithril, Wall, Soil, Tree, plus furniture of both materials. NOT Grass, Water, Slope, Sky, Floor, Stair.
  - `is_floor_like` (new): Floor | Grass.
  - `dig_ticks`: Soil 8, Tree 20, Slope 8, wood furniture 10.
  - `yield_`:
    - Tree → wood 3;
    - Soil and Slope → nothing;
    - each furniture piece → its build cost, so wood pieces give wood and stone pieces give stone 1.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn tree_yields_three_wood() { assert_eq!(Tile::Tree.yield_(), Resources { wood: 3, ..Default::default() }); assert_eq!(Tile::Tree.dig_ticks(), 20); }
#[test] fn wood_furniture_costs() {
    assert_eq!(DesignationKind::WoodTable.cost().wood, 4);
    assert_eq!(DesignationKind::WoodBed.cost().wood, 3);
    assert_eq!(DesignationKind::WoodChair.cost().wood, 2);
    assert_eq!(DesignationKind::Bed.cost(), Resources { stone: 1, ..Default::default() });
    assert_eq!(DesignationKind::Chop.cost(), Resources::default());
}
#[test] fn dismantling_returns_cost() { for k in [DesignationKind::WoodBed, DesignationKind::Bed] { assert_eq!(k.builds().unwrap().yield_(), k.cost()); } }
#[test] fn water_and_sky_are_inert() { for t in [Tile::Water, Tile::Sky] { assert!(!t.is_solid() && !t.is_floor_like()); } }
#[test] fn resources_covers_and_take() { /* {stone:1,wood:2} covers wood 2, not wood 3; take saturates */ }
#[test] fn toggle_cycle_includes_chop() { /* Dig→Stairs→Chop→Wall→Dig */ }
```

- [ ] **Step 2:** Run `cargo test -p moria-core tile`. Expected: FAIL (compile errors).
- [ ] **Step 3:** Implement. Fix every non-exhaustive `match` on `Tile` / `DesignationKind` in the workspace, so the workspace compiles. In client `terrain.rs`, give placeholder colours for now; the real textures come in Task 7.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(core): outdoor tiles, wood resource and per-kind build costs`

### Task 3: Movement on slopes, dig and build rules outdoors

**Files:**
- Modify: `crates/moria-core/src/mountain.rs`, `crates/moria-core/src/path.rs`

**Interfaces:**
- **Consumes:** Task 2.
- **Produces:**
  - `pub const GROUND_Z: u8 = 4;` and the new `MOUNTAIN_W/H/D` values (Global Constraints).
  - `Mountain::walkable(p)` = `matches!(t, Floor | Stair | Grass | Slope)`.
  - `Mountain::move_dest(&self, from: TilePos, dir: Dir) -> Option<TilePos>`:
    - **Up/Down:** unchanged stair rule. Stair at both ends.
    - **Horizontal:** `from` must be walkable. Let `q = from.step(dir)`.
      - If `q` is walkable → `Some(q)`.
      - **Climb.** Else if `from` is a `Slope`, `q` is solid, the tile above `from` (`from.z - 1`) is Sky, and `q_up = q.step(Up)` is walkable → `Some(q_up)`.
      - **Descend.** Else if `q` is Sky and `q_down = q.step(Down)` is a `Slope` → `Some(q_down)`.
      - Otherwise `None`.
  - `can_move(from, dir)` becomes `move_dest(from, dir).is_some()`.
  - **Every caller that assumed `from.step(dir)` as the destination must use `move_dest`.** That means the path, the sim, and client prediction.
  - `diggable(p, kind)`:
    - **Dig:** `is_solid` and not `Tree`, plus Slope.
    - **Chop:** `Tree` only.
    - **Stairs:** solid (not Tree) or floor-like, and `p.z + 1 < d`.
    - **Wall and every furniture kind:** floor-like.
    - **Water and Sky:** never.
  - `dig(p, kind)`:
    - **Chop:** Tree becomes Grass and yields 3 wood.
    - **Dig on Slope:** the slope becomes Floor.
    - **Dig on Soil or rock:** becomes Floor, as today.
    - **Build kinds:** set `builds()`.
  - `work_spot_goal`: unchanged. A same-z horizontally adjacent walkable tile.
  - `path::find_path` / `distances_from`: the neighbours are `move_dest(p, d)` for the 6 dirs.

- [ ] **Step 1: Write failing tests** (`mountain.rs` / `path.rs`). Use a small hand-built map: ground at z 2, a 1-high hill at z 1 with a Slope at its foot, and Sky above.

```rust
#[test] fn climb_slope_onto_hill() { /* from Slope(5,5,2) moving E into solid Soil(6,5,2) with Grass(6,5,1) above → move_dest == Some((6,5,1)) */ }
#[test] fn descend_hill_via_slope() { /* from Grass(6,5,1) moving W: (5,5,1) is Sky and (5,5,2) is Slope → Some((5,5,2)) */ }
#[test] fn no_climb_without_slope() { /* from Grass(4,5,2) moving E into the hill side → None */ }
#[test] fn no_fall_off_hill_edge() { /* from hill top moving where Sky has no Slope below → None */ }
#[test] fn water_and_sky_block() { /* move into Water or Sky → None; diggable(Water|Sky, any) == false */ }
#[test] fn chop_tree_gives_wood_and_grass() { /* dig(tree, Chop) == Some(wood 3); tile == Grass; Dig on a tree not diggable */ }
#[test] fn build_on_grass() { /* diggable(grass, WoodBed) && diggable(grass, Wall) && diggable(grass, Stairs) */ }
#[test] fn path_uses_slopes() { /* find_path from ground to hill top goes through the Slope */ }
```

- [ ] **Step 2:** Run `cargo test -p moria-core`. Expected: FAIL.
- [ ] **Step 3:** Implement. Update the server sim, AI and client call sites that compute a destination as `from.step(dir)` after `can_move`, so they use `move_dest` instead (`grep -rn "can_move" crates/`).
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: PASS. Existing tests built on 48 × 48 hand maps keep their own sizes.
- [ ] **Step 5: Commit** `feat(core): slopes change level, outdoor dig/chop/build rules`

### Task 4: Outdoor world generation, save v2

**Files:**
- Modify: `crates/moria-core/src/world.rs`, `crates/moria-core/src/protocol.rs` (only `PROTOCOL_VERSION = 4`)

**Interfaces — Produces:**
- `pub const ENTRANCE_X: std::ops::Range<i16> = 128..134;`
- `pub const ENTRANCE_Y: std::ops::Range<i16> = 46..50;`
- `pub fn entrance_tiles() -> impl Iterator<Item = TilePos>` (at `GROUND_Z`).
- `SAVE_VERSION = 2`.
- `generate(seed)` builds every mountain at the new size. `claim_mountain` spawns the 3 dwarves on the first 3 `entrance_tiles()`. The server's `entrance()` returns the first entrance tile.

**Generator requirements:** use `rand_chacha`, and the result must be deterministic. Details are left to the implementer, except for the following.
- Sky (z < `GROUND_Z`) everywhere except hills and the mountain mass.
- Mountain mass:
  - x ≥ 128 is Rock from z `GROUND_Z` down. Its existing ore veins move to underground z (gold anywhere underground in the mountain; mithril only at z ≥ `GROUND_Z` + 5).
  - Above ground, the mountain is a rock mass whose height grows toward the east, up to z 0.
- Outdoor ground at z `GROUND_Z`: Grass with Trees.
  - The tree density rises with distance from the entrance: sparse within about 20 tiles, dense near the lake.
- Lake: Water centred near (28, 48), radius about 10–14.
- River: Water, 1–2 wide, from the mountain's foot (x = 127) at y ≈ 70 down to the lake. It must not cut the entrance off from the forest north of it.
- Hills: 1–2 hills of Soil, rising 1–2 levels. Their walkable tops are Grass, with at least one Slope on the ground at the foot of each.
- Underground outdoors: Soil for 2 levels below ground, then Rock with about half the mountain's gold veins.
- Entrance room: Floor, at `ENTRANCE_X × ENTRANCE_Y × GROUND_Z`, open west to Grass at x = 127.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn new_world_dimensions() { /* every mountain size() == (176, 96, 12) */ }
#[test] fn entrance_opens_on_grass() { /* every entrance tile walkable; (127, 47, GROUND_Z) is Grass */ }
#[test] fn forest_reachable_from_entrance() { /* distances_from(entrance) reaches the work spot of ≥ 50 Tree tiles */ }
#[test] fn lake_about_100_tiles_away() { /* nearest Water with x < 60 has x ≤ 40; Water exists near (28,48) */ }
#[test] fn hill_reachable_by_slope() { /* some Grass at z < GROUND_Z is reachable from the entrance */ }
#[test] fn sky_above_ground_outdoors() { /* (60, 10, 0) is Sky */ }
#[test] fn mithril_only_deep() { /* every Mithril has z >= GROUND_Z + 5 */ }
#[test] fn full_map_encodes_under_max_frame() { /* protocol::encode(&FortressView{ mountain, .. }).len() < MAX_FRAME / 2 */ }
#[test] fn v1_save_is_refused() { /* a file starting with 1u32 LE → load() Err whose message contains "non supporté" */ }
```

  Keep `generate_is_deterministic`, and update `claim_gives_three_dwarves_on_floor_and_caps_at_ten`.
- [ ] **Step 2:** Run `cargo test -p moria-core world`. Expected: FAIL.
- [ ] **Step 3:** Implement. Update the server `entrance()`, and any test in other crates that hard-codes `(0..6, 22..26, 0)`, so they use `entrance_tiles()` or `GROUND_Z`. That includes `crates/moria-server/tests/*.rs`, `examples/bots.rs` (its designation targets move outdoors west of the entrance: Chop rectangles over the nearest trees, with ≥ 40 valid targets per fortress) and `moria-dwarf-ai` tests.
- [ ] **Step 4:** Run `cargo test --workspace`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(world): outdoor forest, river, lake and hills; save format v2`

### Task 5: Server — chop, wood builds, per-resource reservation

**Files:**
- Modify: `crates/moria-server/src/sim.rs`, `crates/moria-server/src/lib.rs`

**Interfaces:**
- **Consumes:** Tasks 2–4.
- **Produces:**
  - Every place that reserves or spends "1 stone for a Wall/furniture build" now uses `kind.cost()` with `Resources::covers` / `take`, for the hero and for dwarves:
    - `spare_stone` becomes `pub fn spare(f: &Fortress) -> Resources` (resources minus the costs reserved by in-progress dwarf and hero builds);
    - `hero_stone` is generalized the same way.
  - `apply_assignments` refuses a build whose cost `spare(f)` doesn't cover.
  - A job pauses when its cost is not covered at completion, as it does today when stone runs out.
  - The hero's `Dig(dir)` on a Tree chops it, which is the same as starting a `DigJob` with kind `Chop`. `Build(dir, kind)` accepts the wood kinds.
  - Chop designations go through the same assign → validate → execute → resources → delta path as Dig.
  - **AI view** (`lib.rs` `send_view`): instead of the stone-only Wall filter, drop every build designation whose cost `spare(f)` doesn't cover. Keep the `ponytail:` note.
  - `tick_budget_ten_full_fortresses`: update it for the new map, with designations spread across outdoors and underground. The budget stays at mean step+assign < 10 ms in release.

- [ ] **Step 1: Write failing tests** (`sim.rs`)

```rust
#[test] fn dwarf_chops_designated_tree() { /* designate Chop on a tree; assignment; steps → tile Grass, wood += 3, designation gone, delta reports both */ }
#[test] fn hero_chops_tree() { /* Dig(dir) at a Tree → after 20±STR ticks: Grass, wood 3 */ }
#[test] fn wood_build_waits_without_wood() { /* WoodTable designated, wood 3 → not assigned, not in AI view; wood 4 → assigned and built, wood 0 */ }
#[test] fn costs_are_per_resource() { /* stone 0, wood 9: WoodBed builds, stone Bed waits; stone never spent on a wood build */ }
#[test] fn hero_builds_wood_chair() { /* Build(dir, WoodChair) with wood 2 → WoodChair, wood 0 */ }
#[test] fn dismantle_wood_furniture_returns_wood() { /* Dig a WoodBed → wood +3 */ }
```

- [ ] **Step 2:** Run `cargo test -p moria-server sim`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and `cargo test --release -p moria-server -- --ignored tick_budget`. Expected: PASS, and the mean is reported under 10 ms.
- [ ] **Step 5: Commit** `feat(server): chopping and wood builds with per-resource reservation`

### Task 6: Dwarf AI on the new map

**Files:**
- Modify: `crates/moria-dwarf-ai/src/assign.rs` (tests, plus anything the new pathing needs)

**Interfaces:**
- **Consumes:** `path::distances_from` (now slope-aware) and `Mountain::diggable` (Chop).
- `validate` keeps using `MOUNTAIN_W/H/D`, which are now raised.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn tree_across_water_not_assigned() { /* tree whose only work spots are cut off by Water → not assigned; a reachable Dig gets the dwarf */ }
#[test] fn nearest_tree_wins_via_slope() { /* two trees: one on a hill reached via a Slope (path 6), one on flat ground (path 12) → the hill tree */ }
#[test] fn full_size_view_validates() { /* a generate()d mountain in an AiView passes validate */ }
```

- [ ] **Step 2:** Run `cargo test -p moria-dwarf-ai`. Expected: FAIL, unless the core changes already make them pass. If they pass, write that down and keep the tests.
- [ ] **Step 3:** Fix whatever fails.
- [ ] **Step 4:** Run `cargo test --workspace`, then the NATS e2e suite: `docker run -d --rm -p 4222:4222 --name nats-test nats:2-alpine && NATS_TEST_URL=nats://127.0.0.1:4222 cargo test -p moria-server --test e2e -- --ignored; docker stop nats-test`. Expected: PASS.
- [ ] **Step 5: Commit** `test(ai): chopping, slopes and full-size views`

### Task 7: Client — outdoor tiles, levels, chop mode, wood

**Files:**
- Modify: `crates/moria-client/src/{game.rs,input.rs,render.rs,terrain.rs,art.rs,ui.rs}`

**Interfaces:**
- **Consumes:** Tasks 2–4.
- **Produces:**
  - Prediction uses `Mountain::move_dest`, so walking onto a slope predicts the level change.
  - `pub fn level_label(z: u8) -> String`, in `game.rs`, displays as follows (z grows downward):
    - z == GROUND_Z → "Niveau 0 (sol)";
    - z < GROUND_Z → "Niveau +{GROUND_Z - z}";
    - z > GROUND_Z → "Niveau -{z - GROUND_Z}".
  - The HUD uses `level_label` and shows "Bois" next to Pierre/Or/Mithril.
  - The view follows the hero's z when it changes through a slope, exactly as with stairs.
  - The mode cycle (X / gamepad north) includes "Couper", with the HUD text « Mode : Couper (arbres) ». Drag-designating in Couper sends `Designate{kind: Chop}`.
  - The build menu (key 3, `BUILDS`) lists stone and wood variants with their cost and resource:
    - Mur (1 pierre)
    - Lit (1 pierre)
    - Table (1 pierre)
    - Chaise (1 pierre)
    - Lit en bois (3 bois)
    - Table en bois (4 bois)
    - Chaise en bois (2 bois)
    - The `BUILDS` table gains the resource, e.g. `(kind, label, cost: Resources)`.
  - Textures: Grass, Tree (on grass), Water, Slope (readable as a slope), Soil, Sky (dark blue, empty) and wood furniture. Wood furniture must look different from stone furniture in colour.
  - Rendering stays efficient at 176 × 96 sprites per level: the map is spawned once per snapshot and only changed tiles are recoloured, as today. Spawn the camera on the hero.

- [ ] **Step 1: Write failing tests** (`game.rs`)

```rust
#[test] fn level_labels() { assert_eq!(level_label(GROUND_Z), "Niveau 0 (sol)"); assert_eq!(level_label(GROUND_Z - 2), "Niveau +2"); assert_eq!(level_label(GROUND_Z + 3), "Niveau -3"); }
#[test] fn prediction_climbs_slope() { /* hand map with a Slope: try_move toward the hill → predicted pos is one level up */ }
#[test] fn chop_mode_designates_chop() { /* mode Chop + drag → Designate{kind: Chop} */ }
#[test] fn build_menu_lists_wood() { /* BUILDS contains WoodBed with wood 3, WoodTable wood 4, WoodChair wood 2 */ }
#[test] fn full_size_snapshot_is_valid() { /* a generate()d FortressView passes the snapshot validation */ }
```

  Also extend the existing `terrain.rs` test so every new tile has distinct pixels from Grass/Floor.
- [ ] **Step 2:** Run `cargo test -p moria-client`. Expected: FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --workspace` and `cargo build -p moria-client --release`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(client): outdoors, slopes, chop mode and wood furniture`

### Task 8: Docs, load check, reset instructions

**Files:**
- Modify: `README.md`, `crates/moria-server/examples/bots.rs` (if Task 4 didn't finish it), `Makefile` (only if its `reset-world` target needs changes)

- [ ] **Step 1: README (in French).** Add a section « Mise à jour vers le dehors et le bois », covering:
  - the world is regenerated, and old saves are refused with « format de sauvegarde v1 non supporté »;
  - how to reset: locally `make reset-world`, or `docker compose down -v`; on Portainer, stop the stack, delete the `moria-data` volume, then start the stack again;
  - every player recreates their account.
  - Also add one line on the new controls: Couper (X), the slopes, the wood furniture in the build menu.
- [ ] **Step 2:** Run the 10-bot load check against a fresh local stack. The rules are the same as before:
  - `docker volume ls` must show no pre-existing `moria-data`, otherwise STOP;
  - run `MORIA_INVITE=test MORIA_MAX_PER_IP=16 docker compose -p moria-test up -d --build` (with the local-build override if the compose file pins GHCR images);
  - run `cargo run --release -p moria-server --example bots -- 127.0.0.1:7777 10 120`;
  - finish with `docker compose -p moria-test down -v --rmi local`.
  - Expected: p99 < 150 ms and dug > 0 for every bot.
- [ ] **Step 3:** Run `cargo test --workspace`, `cargo clippy --workspace --all-targets --all-features -- -D warnings` and `cargo fmt --check`. Expected: clean.
- [ ] **Step 4: Commit** `docs: reset the world for the outdoor update; load check on the new map`

---

## Spec coverage

| Spec item | Task |
|---|---|
| One continuous map, private outdoors | 3, 4 |
| Relief A: ground at the entrance level, hills and mountain rise, slopes change level | 3, 4 |
| Dig under the forest; outdoors works like indoors; water untouchable | 2, 3 |
| Lake about 100 tiles from the entrance | 4 |
| Wood resource; hero chops; dwarves chop designated trees | 2, 3, 5, 6, 7 |
| Bed/table/chair in wood and stone | 1, 2, 5, 7 |
| No regrowth, no crossing water, no falling, sky is void | 2, 3 |
| PR #13 first; protocol 4; save v2; world regenerated | 1, 4, 8 |
| Size validations raised, not removed; frame limit; load | 4, 5, 6, 7, 8 |
| Spawn in the entrance room; no door | 4 |
| Anti-goal: stupid dwarves (unreachable trees, slopes, nearest) | 3, 6 |
