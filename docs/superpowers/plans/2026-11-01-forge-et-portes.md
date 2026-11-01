# Forge et portes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Iron ore and coal veins, a forge that smelts ingots and an anvil that forges iron doors on the player's orders (worked by dwarves), and wooden, stone and iron doors the hero locks to keep dwarves out of a room.

**Architecture:** All rules live in `moria-core` (tiles, costs, recipes, door movement, door light), so they are unit-tested there and shared by server, AI service and client. A workshop order is a count in a new `Mountain::orders` map. The server shows it to the AI as a designation of a new *work* kind (`Smelt`, `Smith`) on the workshop tile, so `moria-dwarf-ai` needs no code change. A locked door is its own `Tile` variant: toggling it is an ordinary tile change in `TickDelta::tiles`.

**Tech Stack:** Rust 2024, Bevy + bevy_egui (client), postcard protocol, tokio server, NATS (AI service).

**Spec:** `docs/superpowers/specs/2026-11-01-forge-et-portes-design.md`

## Global Constraints

- TDD is mandatory: every task writes its failing test first, runs it RED, then GREEN.
- New `Tile` and `DesignationKind` variants are appended **at the end**, in the order given in Task 1 (postcard encoding). Extend the `wire_encoding_is_unchanged` test with them.
- `PROTOCOL_VERSION` = **8** (Task 4). `SAVE_VERSION` = **3** (Task 3). No v2 loader.
- Costs: Forge 6 pierre · Enclume 3 lingots · Porte en bois 3 bois · Porte en pierre 3 pierre · Porte en fer 1 porte en fer forgée.
- Recipes: Lingot = 2 minerai + 1 charbon, 30 ticks, at a forge · Porte en fer = 2 lingots, 40 ticks, at an anvil.
- Order count per workshop: 1..=99; `+1`, `+5`, cancel. Orders wait (never cancel) when stock is short.
- Dig ticks: IronOre 15, Coal 12, Forge/Anvil/doors 10 (like furniture). Yields: IronOre → 1 `iron_ore`, Coal → 1 `coal`; built tiles return their build cost.
- Veins per level `z` in `GROUND_Z..MOUNTAIN_D`: mountain side 8 iron + 10 coal; outdoor side (only `z > GROUND_Z + 2`) 4 iron + 5 coal. Drawn with a **separate RNG** after the rest of the generation.
- Forge light radius 4. Anvil none.
- Every user-visible string is in French.
- Checks: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. Share one target dir: `CARGO_TARGET_DIR=/Users/nicolaslallier/OpenCode/Moria/target`.

## Review Focus

1. **A dwarf mid-path when the hero locks the door in front of it**: it drops the task (existing re-path → `None` → drop), never stands still forever. Pinned by `locking_a_door_reroutes_or_drops_the_dwarf` (Task 5).
2. **Locking a door someone stands in** would strand them on a tile `walkable` refuses: the toggle is refused while anyone occupies the door. Pinned by `cannot_lock_an_occupied_door` (Task 5).
3. **Stock spent elsewhere while a dwarf works the forge**: no ingot, nothing taken, order count unchanged. Pinned by `smelting_without_stock_produces_nothing` (Task 6).
4. **Dismantling a workshop that has an order**: the order disappears and the client is told. Pinned by `digging_a_forge_drops_its_order` (Task 6).
5. **A forged `Designate`/`Order` from a client** (work kind designated by hand, order on a non-workshop, far from the hero, count overflow): ignored or clamped. Pinned by `work_kinds_cannot_be_designated` (Task 4) and `order_needs_an_adjacent_workshop_and_clamps` (Task 6).

---

## Part 1 — Le cœur : cases, ressources, portes, filons, protocole

### Task 1: Tiles, resources, costs and recipes

**Files:**
- Modify: `crates/moria-core/src/tile.rs`
- Modify: `crates/moria-core/src/mountain.rs` (`orders`, `work_at`, `diggable`, `dig`)
- Fix compile fallout wherever a `Resources { .. }` literal or an exhaustive `match` on `Tile`/`DesignationKind` breaks (server, client `job_label`, tests).

**Interfaces:**
- Produces:
  - `Tile` appended: `IronOre, Coal, Forge, Anvil, WoodDoor, StoneDoor, IronDoor, WoodDoorLocked, StoneDoorLocked, IronDoorLocked`.
  - `DesignationKind` appended: `Forge, Anvil, WoodDoor, StoneDoor, IronDoor, Smelt, Smith`. `builds()`: the five builds map to `Tile::Forge, Anvil, WoodDoor, StoneDoor, IronDoor`; `Smelt`/`Smith` → `None`.
  - `Resources` gains `pub iron_ore: u32, pub coal: u32, pub iron: u32, pub iron_door: u32`, covered by `add`/`covers`/`take`.
  - `DesignationKind::is_work(self) -> bool` (`Smelt | Smith`).
  - `DesignationKind::product(self) -> Resources`: `Smelt` → 1 `iron`, `Smith` → 1 `iron_door`, others empty.
  - `DesignationKind::work_ticks(self) -> u16`: `Smelt` 30, `Smith` 40, others 0.
  - `Tile::recipe(self) -> Option<DesignationKind>`: `Forge` → `Smelt`, `Anvil` → `Smith`.
  - `Tile::is_door(self)`, `Tile::is_locked_door(self)`, `Tile::toggled_door(self) -> Option<Tile>` (open ↔ locked of the same material).
  - `is_furniture()` includes Forge, Anvil and **all six** door tiles (so they dig back and are solid-but-not-stone).
  - `Mountain` gains `pub orders: BTreeMap<TilePos, u8>` (empty in `new`).
  - `Mountain::work_at(&self, p) -> Option<DesignationKind>`: the designation at `p` if any, else `get(p)?.recipe()` when `orders[p] > 0`.
  - `diggable(p, Smelt)` ⇔ tile is `Forge`; `diggable(p, Smith)` ⇔ tile is `Anvil`. `dig(p, Smelt|Smith)` returns `None`. `dig` of any tile also removes `orders[p]`.

- [ ] **Step 1: Write the failing tests** in `tile.rs` `mod tests`:

```rust
#[test]
fn iron_chain_costs_and_recipes() {
    use DesignationKind as K;
    let r = |f: fn(&mut Resources)| { let mut r = Resources::default(); f(&mut r); r };
    assert_eq!(K::Forge.cost(), r(|r| r.stone = 6));
    assert_eq!(K::Anvil.cost(), r(|r| r.iron = 3));
    assert_eq!(K::WoodDoor.cost(), r(|r| r.wood = 3));
    assert_eq!(K::StoneDoor.cost(), r(|r| r.stone = 3));
    assert_eq!(K::IronDoor.cost(), r(|r| r.iron_door = 1));
    assert_eq!(K::Smelt.cost(), r(|r| { r.iron_ore = 2; r.coal = 1 }));
    assert_eq!(K::Smelt.product(), r(|r| r.iron = 1));
    assert_eq!(K::Smith.cost(), r(|r| r.iron = 2));
    assert_eq!(K::Smith.product(), r(|r| r.iron_door = 1));
    assert_eq!((K::Smelt.work_ticks(), K::Smith.work_ticks()), (30, 40));
    assert_eq!(Tile::Forge.recipe(), Some(K::Smelt));
    assert_eq!(Tile::Anvil.recipe(), Some(K::Smith));
    assert_eq!(Tile::Rock.recipe(), None);
    assert!(K::Smelt.builds().is_none() && K::Smelt.is_work());
}

#[test]
fn ores_and_doors_dig_back() {
    assert_eq!(Tile::IronOre.yield_().iron_ore, 1);
    assert_eq!(Tile::Coal.yield_().coal, 1);
    assert_eq!((Tile::IronOre.dig_ticks(), Tile::Coal.dig_ticks()), (15, 12));
    assert_eq!(Tile::IronDoorLocked.yield_().iron_door, 1);
    assert_eq!(Tile::StoneDoorLocked.yield_(), DesignationKind::StoneDoor.cost());
    assert_eq!(Tile::WoodDoor.toggled_door(), Some(Tile::WoodDoorLocked));
    assert_eq!(Tile::IronDoorLocked.toggled_door(), Some(Tile::IronDoor));
    assert_eq!(Tile::Forge.toggled_door(), None);
    for t in [Tile::Forge, Tile::Anvil, Tile::WoodDoor, Tile::IronDoorLocked] {
        assert!(t.is_furniture() && !t.is_stone(), "{t:?}");
    }
    assert!(Tile::IronOre.is_stone() && Tile::Coal.is_stone());
}
```

  In `mountain.rs` `mod tests`:

```rust
#[test]
fn orders_make_work_and_die_with_the_workshop() {
    let mut m = Mountain::new(MountainId(0), 4, 4, 1, Tile::Floor);
    let p = TilePos::new(1, 1, 0);
    m.set(p, Tile::Forge);
    assert_eq!(m.work_at(p), None);
    m.orders.insert(p, 5);
    assert_eq!(m.work_at(p), Some(DesignationKind::Smelt));
    assert!(m.diggable(p, DesignationKind::Smelt) && !m.diggable(p, DesignationKind::Smith));
    m.designate(p, p, DesignationKind::Dig);
    assert_eq!(m.work_at(p), Some(DesignationKind::Dig)); // dismantling wins
    assert_eq!(m.dig(p, DesignationKind::Dig).unwrap().stone, 6);
    assert!(m.orders.is_empty());
}
```

- [ ] **Step 2: Run** `cargo test -p moria-core` → FAIL (unknown variants/methods).
- [ ] **Step 3: Implement** the Interfaces above. Extend the `wire_encoding_is_unchanged` expected list with the ten new tiles, in order.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS. (Client `job_label`: `Smelt` → "Fond du fer", `Smith` → "Forge une porte"; the five builds go in the "Construit" arm.)
- [ ] **Step 5: Commit** `feat(core): minerai, charbon, forge, enclume et portes`

### Task 2: Doors stop dwarves and light, not the hero

**Files:**
- Modify: `crates/moria-core/src/mountain.rs` (`walkable`, new `hero_move_dest`)
- Modify: `crates/moria-core/src/light.rs` (`blocks_light`, `blocked`, `light_radius`)

**Interfaces:**
- Consumes: Task 1 door helpers.
- Produces:
  - `walkable(p)` is false for locked doors (open doors stay walkable as furniture). So `move_dest`, `find_path`, `distances_from` and the AI all treat a locked door as a wall.
  - `Mountain::hero_move_dest(&self, from, dir) -> Option<TilePos>`: like `move_dest`, but for a horizontal `dir` a locked door counts as walkable, both as `from` and as the destination.
  - `blocks_light(t)` = `t.is_stone() || t.is_door()`.
  - In `light_map`, a door tile **at a source position** (someone stands in it: every body is a `BODY_LIGHT` source) does not block. Pass the set of source positions into `blocked`.
  - `light_radius(Tile::Forge) == Some(4)`.

- [ ] **Step 1: Write the failing tests:**

```rust
// mountain.rs
#[test]
fn locked_doors_stop_dwarves_not_the_hero() {
    let mut m = Mountain::new(MountainId(0), 5, 1, 1, Tile::Floor);
    let (a, door, b) = (TilePos::new(1, 0, 0), TilePos::new(2, 0, 0), TilePos::new(3, 0, 0));
    m.set(door, Tile::WoodDoor);
    assert_eq!(m.move_dest(a, Dir::E), Some(door));
    m.set(door, Tile::WoodDoorLocked);
    assert!(!m.walkable(door));
    assert_eq!(m.move_dest(a, Dir::E), None);
    assert!(crate::path::find_path(&m, a, |p| p == b, &Default::default()).is_none());
    assert_eq!(m.hero_move_dest(a, Dir::E), Some(door));
    assert_eq!(m.hero_move_dest(door, Dir::E), Some(b));
}

// light.rs
#[test]
fn a_door_hides_the_room_unless_someone_stands_in_it() {
    let mut m = Mountain::new(MountainId(0), 9, 1, 2, Tile::Floor); // z 1: mine, always dark
    m.set(TilePos::new(4, 0, 1), Tile::StoneDoor);
    let area = Area { x0: 0, y0: 0, x1: 8, y1: 0 };
    let shut = light_map(&m, 1, area, &[(2, 0, 4)], 20_000);
    assert!(shut.get(4, 0) > 0.0);   // the door itself is lit
    assert_eq!(shut.get(5, 0), 0.0); // the room behind is not
    let open = light_map(&m, 1, area, &[(2, 0, 4), (4, 0, 4)], 20_000);
    assert!(open.get(6, 0) > 0.0);
    assert_eq!(light_radius(Tile::Forge), Some(4));
}
```

- [ ] **Step 2: Run** `cargo test -p moria-core` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS.
- [ ] **Step 5: Commit** `feat(core): les portes verrouillées arrêtent les nains et la lumière`

### Task 3: Veins, save v3 and the automatic world reset

**Files:**
- Modify: `crates/moria-core/src/world.rs` (`generate`, `SAVE_VERSION`)
- Modify: `crates/moria-worldgen/src/main.rs`

**Interfaces:**
- Produces:
  - After the existing `for` loop in `generate`, a second pass with its own `ChaCha8Rng::seed_from_u64(seed ^ 0x1f0e_c0a1)` adds, for each mountain and each `z` in `GROUND_Z..MOUNTAIN_D`, 8 `IronOre` + 10 `Coal` veins on `foot..MOUNTAIN_W`, and for `z > GROUND_Z + 2` 4 + 5 on `0..foot`. Reuse `vein` (it only overwrites `Rock`). The main RNG sequence is untouched.
  - `SAVE_VERSION = 3`.
  - `pub fn save_version(path: &Path) -> io::Result<Option<u32>>` in `world.rs`: the 4-byte header, `None` if the file is missing.
  - `moria-worldgen`: if `--out` exists with a version **other than** `SAVE_VERSION`, rename it to `<out>.v<n>.bak` and generate a new world (print `ancien monde v<n> mis de côté : <bak>`). The same version without `--force` keeps today's "monde déjà présent, rien à faire". This is the world reset decided in the spec: compose already runs worldgen before the server on every deploy.

- [ ] **Step 1: Write the failing tests** in `world.rs`. Factor the new pass into `fn add_iron_and_coal(w: &mut World)`, called at the end of `generate`, and keep the old body as a private `fn generate_base(seed) -> World`:

```rust
#[test]
fn iron_and_coal_veins_only_replace_rock() {
    let (base, w) = (generate_base(1), generate(1));
    for (id, m) in &w.mountains {
        let mut n = (0, 0);
        for ((p, t), (_, b)) in tiles(m).zip(tiles(&base.mountains[id])) {
            match t {
                Tile::IronOre => n.0 += 1,
                Tile::Coal => n.1 += 1,
                _ => assert_eq!(t, b, "{p:?}"),
            }
            if matches!(t, Tile::IronOre | Tile::Coal) {
                assert_eq!(b, Tile::Rock, "{p:?}");
            }
        }
        assert!(n.0 > 0 && n.1 > 0, "{id:?}");
    }
}

#[test]
fn save_version_reads_the_header() {
    let p = tmpdir("save-version").join("w.bin");
    assert_eq!(save_version(&p).unwrap(), None);
    std::fs::write(&p, 2u32.to_le_bytes()).unwrap();
    assert_eq!(save_version(&p).unwrap(), Some(2));
}
```

  (`tiles` and `tmpdir` are the existing test helpers in `world.rs`; adapt `tiles` to yield `(TilePos, Tile)` in a fixed order if it doesn't already.)

- [ ] **Step 2: Run** `cargo test -p moria-core` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above, then the worldgen change (keep it in `main`; no new crate dependency).
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS. Then, by hand: write a v2-headed file, run `cargo run -p moria-worldgen -- --out <it>`, and check that the `.v2.bak` appears and the new file loads.
- [ ] **Step 5: Commit** `feat(worldgen): filons de fer et de charbon, sauvegarde v3`

### Task 4: Protocol v8

**Files:**
- Modify: `crates/moria-core/src/protocol.rs`
- Modify: `crates/moria-server/src/sim.rs` (`Command`, `Command::from_client`)

**Interfaces:**
- Produces:
  - `ClientMsg::Use(Dir)`: the hero acts on the tile toward `Dir` (door: lock/unlock).
  - `ClientMsg::Order { dir: Dir, add: u8 }`: order `add` more units at the workshop toward `dir`; `add == 0` cancels.
  - `TickDelta` gains `pub orders: Vec<(TilePos, u8)>`: changed order counts, `0` = order gone.
  - `Command::Use(Dir)`, `Command::Order { dir: Dir, add: u8 }`.
  - `from_client` drops `Designate` whose `kind.is_work()`.
  - `PROTOCOL_VERSION = 8`.

- [ ] **Step 1: Write the failing test** in `sim.rs`:

```rust
#[test]
fn work_kinds_cannot_be_designated() {
    let q = TilePos::new(0, 0, 0);
    for kind in [DesignationKind::Smelt, DesignationKind::Smith] {
        assert_eq!(Command::from_client(ClientMsg::Designate { a: q, b: q, kind }), None);
    }
    assert_eq!(Command::from_client(ClientMsg::Use(Dir::E)), Some(Command::Use(Dir::E)));
    assert_eq!(
        Command::from_client(ClientMsg::Order { dir: Dir::N, add: 5 }),
        Some(Command::Order { dir: Dir::N, add: 5 })
    );
}
```

  Also bump the protocol layout test if it pins `PROTOCOL_VERSION`.
- [ ] **Step 2: Run** `cargo test -p moria-server work_kinds` → FAIL.
- [ ] **Step 3: Implement.** `apply_command` gets empty arms for `Use`/`Order` here (filled in Tasks 5–6).
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS.
- [ ] **Step 5: Commit** `feat(protocol): v8, utiliser, commandes d'atelier`

---

## Part 2 — Le serveur

### Task 5: The hero locks doors and walks through them

**Files:**
- Modify: `crates/moria-server/src/sim.rs`

**Interfaces:**
- Consumes: `Tile::toggled_door`, `Mountain::hero_move_dest` (Tasks 1–2), `Command::Use` (Task 4).
- Produces:
  - `Changes` becomes a named struct `{ designations: BTreeMap<TilePos, Option<DesignationKind>>, tiles: Vec<(TilePos, Tile)>, orders: BTreeMap<TilePos, u8> }`. `then` folds all three (last write wins). `merge_into` puts command tiles **before** the step's tiles in `d.tiles` (the step's later writes win on the client), and adds orders the step didn't already report.
  - `Command::Use(d)`, horizontal `d` only: if the target is a door and neither the hero nor any dwarf stands on it, set it to `toggled_door()` and record the tile.
  - `hero_move` uses `hero_move_dest`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn hero_locks_and_unlocks_a_door() {
    let mut w = setup();
    let door = p(3, 1, 0);
    w.mountains.get_mut(&M).unwrap().set(door, Tile::WoodDoor);
    let ch = apply_command(&mut w, M, Command::Use(Dir::E));
    assert_eq!(tile(&w, door), Tile::WoodDoorLocked);
    assert_eq!(ch.tiles, vec![(door, Tile::WoodDoorLocked)]);
    apply_command(&mut w, M, Command::Move(Dir::E)); // the hero has the key
    assert_eq!(hero(&w).pos, door);
}

#[test]
fn cannot_lock_an_occupied_door() {
    let mut w = setup();
    let door = p(3, 1, 0);
    w.mountains.get_mut(&M).unwrap().set(door, Tile::StoneDoor);
    w.fortresses.get_mut(&M).unwrap().dwarves[0].pos = door;
    apply_command(&mut w, M, Command::Use(Dir::E));
    assert_eq!(tile(&w, door), Tile::StoneDoor);
}

#[test]
fn locking_a_door_reroutes_or_drops_the_dwarf() {
    // A dwarf with a Dig task whose only path crosses `door`; lock it mid-path, step a few
    // ticks: the dwarf's task is None (or its path avoids `door`), and it never stands on it.
}
```

  Write the third test fully with the room layout the existing path tests use (a dug pocket reached through a single tile).
- [ ] **Step 2: Run** `cargo test -p moria-server door` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS.
- [ ] **Step 5: Commit** `feat(server): le héros verrouille les portes`

### Task 6: Workshop orders worked by dwarves

**Files:**
- Modify: `crates/moria-server/src/sim.rs`

**Interfaces:**
- Consumes: `Mountain::orders`, `work_at`, `DesignationKind::{is_work, product, work_ticks}`, `Tile::recipe` (Task 1); `Changes.orders` (Task 5); `Command::Order` (Task 4).
- Produces:
  - `Command::Order { dir, add }`, horizontal `dir`: target `t = hero.pos.step(dir)` must have `recipe().is_some()`. `add == 0` removes the order; else `orders[t] = min(old + add, 99)`. Record the new count (0 if removed) in `Changes.orders`.
  - `view_mountain`: for each order `(p, n > 0)` with no designation at `p`, insert `(p, recipe)` into the view's designations while `len < MAX_DESIGNATIONS`, before the existing `retain(spare.covers(cost))`. That hides orders the stock can't pay, and they wait.
  - `target_valid` and `apply_assignments` use `m.work_at(t)` instead of `designations.get(&t)`.
  - The idle-dwarf `busy` check also counts `work_at(q).is_some()`.
  - In the task loop, a work kind's `needed` is `kind.work_ticks()`. On completion, if not blocked and `f.resources.covers(&kind.cost())`: take the cost, add `kind.product()`, decrement `orders[target]` (remove at 0), record it in `ch.orders`. Not `dig_at`.
  - `dig_at` records an order removed by `Mountain::dig` (0 in `ch.orders`).
  - `step`'s `TickDelta` carries `orders`.

- [ ] **Step 1: Write the failing tests:**

```rust
#[test]
fn order_needs_an_adjacent_workshop_and_clamps() {
    let mut w = setup();
    let forge = p(3, 1, 0);
    w.mountains.get_mut(&M).unwrap().set(forge, Tile::Forge);
    apply_command(&mut w, M, Command::Order { dir: Dir::W, add: 5 }); // not a workshop
    assert!(w.mountains[&M].orders.is_empty());
    for _ in 0..30 {
        apply_command(&mut w, M, Command::Order { dir: Dir::E, add: 5 });
    }
    assert_eq!(w.mountains[&M].orders[&forge], 99);
    let ch = apply_command(&mut w, M, Command::Order { dir: Dir::E, add: 0 });
    assert!(w.mountains[&M].orders.is_empty());
    assert_eq!(ch.orders.get(&forge), Some(&0));
}

#[test]
fn a_dwarf_smelts_the_ordered_ingots() {
    // Forge at p(3, 1, 0), orders = 2, resources: iron_ore 4, coal 2.
    // Loop: view_mountain + assign (moria_dwarf_ai is not a dependency: hand the dwarf the
    // assignment directly with apply_assignments(&mut w, &Assignments { .. }) the way the
    // existing assignment tests do), then step until orders is empty (bounded loop).
    // Assert: resources.iron == 2, iron_ore == 0, coal == 0, orders empty, and the last
    // delta's orders contains (forge, 0).
}

#[test]
fn smelting_without_stock_produces_nothing() {
    // Forge with order 1, iron_ore 2, coal 1; a dwarf working it. One tick before it
    // finishes, set coal = 0. After it finishes: iron == 0, iron_ore == 2, orders[forge] == 1.
}

#[test]
fn short_stock_hides_the_order_from_the_ai() {
    // Forge with order 3, iron_ore 1: view_mountain(..).designations has no entry at the forge.
    // Add iron_ore 1 + coal 1: it shows (forge, Smelt).
}

#[test]
fn digging_a_forge_drops_its_order() {
    // Forge with order 3; the hero digs it (Command::Dig toward it, step until done).
    // orders empty, stone += 6, and the delta's orders contains (forge, 0).
}
```

  Write the bodies in full, with the helpers already in `sim.rs` tests (`setup`, `p`, `tile`, `designate`).
- [ ] **Step 2: Run** `cargo test -p moria-server` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS. The AI crate's tests must still pass unchanged.
- [ ] **Step 5: Commit** `feat(server): commandes d'atelier travaillées par les nains`

---

## Part 3 — Le client

### Task 7: Game state, build menu and HUD

**Files:**
- Modify: `crates/moria-client/src/game.rs`
- Modify: `crates/moria-client/src/ui.rs` (HUD line, build-menu stock line)

**Interfaces:**
- Consumes: Tasks 1, 2, 4.
- Produces:
  - `BUILDS` appended: `(Forge, "Forge")`, `(Anvil, "Enclume")`, `(WoodDoor, "Porte en bois")`, `(StoneDoor, "Porte en pierre")`, `(IronDoor, "Porte en fer")`.
  - `cost_fr` also names `iron_ore` "minerai de fer", `coal` "charbon", `iron` "lingot(s)" (write "lingots", not pluralized by count) and `iron_door` "porte en fer forgée".
  - HUD resources line: `Pierre {} / Bois {} / Or {} / Mithril {} / Minerai de fer {} / Charbon {} / Lingots {} / Portes en fer {}`.
  - `apply_tick` applies `d.orders` to `mountain.orders` (in-bounds only; `0` removes).
  - `try_move` uses `mountain.hero_move_dest`.
  - `Game` gains `pub order_menu: Option<Dir>` and `pub order_sel: usize`.
  - `Game::use_(&mut self) -> Option<ClientMsg>`: target toward `last_dir`. A workshop (`recipe().is_some()`) sets `order_menu = Some(last_dir)`, `order_sel = 0`, and returns `None`. A door returns `Some(ClientMsg::Use(last_dir))`. Anything else returns `None`.
  - `pub const ORDER_ROWS: [(&str, u8); 3] = [("+1", 1), ("+5", 5), ("Annuler la commande", 0)];` and `Game::pick_order(&mut self, i) -> Option<ClientMsg>`: `ClientMsg::Order { dir, add }`. Picking closes the menu only for the cancel row, so +1 can be pressed repeatedly.
  - `recipe_label(k) -> &str`: `Smelt` → "Lingot de fer", `Smith` → "Porte en fer".

- [ ] **Step 1: Write the failing tests** in `game.rs`:

```rust
#[test]
fn use_opens_the_forge_menu_and_toggles_doors() {
    let mut g = game(); // existing test helper (game.rs tests)
    g.last_dir = Dir::E;
    let e = g.hero.pos.step(Dir::E);
    g.mountain.set(e, Tile::Forge);
    assert_eq!(g.use_(), None);
    assert_eq!(g.order_menu, Some(Dir::E));
    assert_eq!(g.pick_order(1), Some(ClientMsg::Order { dir: Dir::E, add: 5 }));
    assert_eq!(g.order_menu, Some(Dir::E));
    assert_eq!(g.pick_order(2), Some(ClientMsg::Order { dir: Dir::E, add: 0 }));
    assert_eq!(g.order_menu, None);
    g.mountain.set(e, Tile::WoodDoorLocked);
    assert_eq!(g.use_(), Some(ClientMsg::Use(Dir::E)));
}

#[test]
fn ticks_carry_orders_and_the_hero_crosses_locked_doors() {
    // apply_tick with orders [(p, 4)] → mountain.orders[p] == 4; then [(p, 0)] → removed.
    // A locked door east of the hero: try_move(E) moves onto it.
}

#[test]
fn new_builds_are_in_the_menu() {
    for (k, l) in [(DesignationKind::Forge, "Forge"), (DesignationKind::IronDoor, "Porte en fer")] {
        assert!(BUILDS.contains(&(k, l)));
    }
    assert_eq!(build_text(DesignationKind::Anvil, "Enclume"), "Enclume (3 lingots)");
}
```

- [ ] **Step 2: Run** `cargo test -p moria-client` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS.
- [ ] **Step 5: Commit** `feat(client): état des commandes, portes et menu Construire`

### Task 8: The "use" button and the workshop menu

**Files:**
- Modify: `crates/moria-client/src/input.rs`
- Modify: `crates/moria-client/src/ui.rs`

**Interfaces:**
- Consumes: `Game::{use_, pick_order, order_menu, order_sel}`, `ORDER_ROWS`, `recipe_label` (Task 7).
- Produces:
  - `Intent` gains `use_: bool`: keyboard **F**, gamepad **RightTrigger2** (RT/R2), both free today.
  - In `play`: Escape/East also closes `order_menu`. While `order_menu` is open, N/S step `order_sel` (wrap over 3) and dig/South calls `pick_order`, like the build menu. Otherwise `use_` sends `g.use_()`. Moving the hero closes the order menu.
  - `ui.rs` `order_menu` window titled after the workshop ("Forge" or "Enclume"). It shows `recipe_label` + `build_text`-style cost, the current stock of each input, the current order count (`mountain.orders`, 0 if none) and the three `ORDER_ROWS` buttons, the selected one highlighted. When the stock doesn't cover one unit, it adds the line "En attente : il manque de la matière" (spec anti-goal: never stall silently).
  - HUD help lines add `F : utiliser (porte, atelier)` and `RT (R2) : utiliser`.

- [ ] **Step 1: Write the failing test** in `input.rs`:

```rust
#[test]
fn key_f_is_use() {
    let mut keys = ButtonInput::<KeyCode>::default();
    keys.press(KeyCode::KeyF);
    assert!(keyboard_intent(&keys, &ButtonInput::default()).use_);
}
```

  And in `ui.rs` tests, following `space_does_not_press_bar_buttons`: the order window shows "En attente" when `resources` are empty and hides it with `iron_ore 2, coal 1`.
- [ ] **Step 2: Run** `cargo test -p moria-client` → FAIL.
- [ ] **Step 3: Implement** the Interfaces above.
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS.
- [ ] **Step 5: Commit** `feat(client): bouton utiliser et menu d'atelier`

### Task 9: Drawings and the order count on the map

**Files:**
- Modify: `crates/moria-client/src/terrain.rs`, `crates/moria-client/src/art.rs` (if a furniture list needs the new tiles), `crates/moria-client/src/render.rs`

**Interfaces:**
- Consumes: Task 1 tiles; `mountain.orders`.
- Produces:
  - Procedural looks: `IronOre` (rock with rust-red/brown flecks) and `Coal` (rock with black flecks), drawn like the gold vein. `Forge` (stone body with an orange mouth), `Anvil` (dark iron block on a stump), three doors (wood planks / grey slab / riveted iron). Each locked variant is the same door with a dark bar across. All of them have an outdoor look on grass, like the other furniture.
  - A small white number (the order count) drawn on each workshop with an order, on the viewed level, darkened by the light like the tile.

- [ ] **Step 1: Write the failing test:** extend the existing per-tile look tests in `terrain.rs` (`outdoor_and_wood_tiles_have_their_own_look`, `furniture_outdoors_sits_on_grass`) to the new tiles, and assert that `WoodDoor` and `WoodDoorLocked` bake to different pixels.
- [ ] **Step 2: Run** `cargo test -p moria-client terrain` → FAIL.
- [ ] **Step 3: Implement** the looks and the count (a `Text2d` per ordered workshop, synced like the dwarf heads).
- [ ] **Step 4: Run** `cargo test --workspace`, clippy, fmt → PASS. Then play locally (`cargo run -p moria-worldgen -- --out <tmp>/world.bin`, server with `MORIA_DATA` on it, then the client) and check by eye: find ore and coal, build a forge, order 5 ingots and watch a dwarf make them, build an anvil, forge and place an iron door, lock it and see dwarves route around it while the hero walks through, and see the room behind it go dark.
- [ ] **Step 5: Commit** `feat(client): dessins du fer, de la forge, de l'enclume et des portes`

---

## Livraison

- Merge the branch through a PR. The server CI pins the new images, and the deploy (manual Portainer redeploy if AutoUpdate misses again) runs worldgen: it sets the v2 world aside as `world.bin.v2.bak` and generates a v3 one. **Every account is reset**, as decided. Probe the server for protocol **8**.
- Release a client tagged for protocol 8 (`v0.4.0`), since v0.3.0 clients will get `VersionMismatch`.
