# Seasons calendar and food container Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a shared seasons calendar to `moria-core`, then move the cows' and crops' simulation (grazing, breeding, growth by season) into a `moria-food` container on the Infra RabbitMQ, with the server staying the authority for actions.

**Architecture:** The rules are pure code in `moria_core::food` (`FoodSim`): they only *propose* events (a cow moved, a birth, a crop ripe) without knowing the map. The server validates each proposal against its tile grid (the truth for positions) and answers with rejects; a full snapshot (`food.world`) re-syncs the container. The container and the server's no-bus mode run the same `FoodSim`. Copy the conventions of the inventory feature (`moria-bus`, queues, heartbeat, atomic save).

**Tech Stack:** Rust 2024, tokio, `lapin` via `moria-bus`, postcard, RabbitMQ 4 (Infra stack).

**Spec:** `docs/superpowers/specs/2026-11-02-nourriture-et-saisons-design.md` (read §8 bis too). Branch is stacked on `worktree-inventory-design` (PR #24): rebase onto `main` once it is merged.

## Global Constraints

- TDD is mandatory: failing test first (RED shown), then GREEN. Where a stub is needed, stub with `todo!()` and show the behaviour tests failing.
- Calendar: 4 seasons of 7 game days; `DAY_TICKS` = 36 000 (from `moria_core::light`); `SEASON_TICKS` = 252 000; `YEAR_TICKS` = 1 008 000; tick 0 = first day of spring.
- Cows step every `COW_EVERY` = 10 ticks, growth every `GROW_EVERY` = 100 ticks (both move from `sim.rs` to `moria_core::food`); in summer a sown field ripens with the current odds (`RIPEN_ODDS` = 60 per growth check) so one field still feeds about two dwarves.
- Seasons: cows breed only in spring and summer; crops grow in spring, summer and autumn, never in winter (slower in spring and autumn, e.g. odds 90); cows step half as often in winter; a ripe crop stays ripe until harvested.
- Herd cap per mountain `COW_CAP` = 24; a cow is adult after `DAY_TICKS` ticks; birth cooldown `2 * DAY_TICKS`. Numbers are defaults: keep them as named constants.
- Bus keys: `food.tick`, `food.world`, `food.action`, `food.reject` (server → food), `food.events`, `food.state` (food → server). All non-durable, fire-and-forget. Heartbeat `food.state` every 1 s; the server calls food stopped after 5 s (`STOPPED_AFTER`).
- Every message carries the server tick it refers to. Bound every payload before decoding (`MAX_FOOD_WORLD_BYTES` = 256 KiB, other messages 16 KiB).
- The server stays authoritative: a food event is applied only if valid on the grid; the food container never deletes anything from the map.
- `PROTOCOL_VERSION` and `SAVE_VERSION` do not change. `World` gains only a `#[serde(skip)]` field. `Fortress.resources`, inventory and `world.bin` are untouched.
- Log messages are in French; no secrets in logs (`moria_bus::redact_url`). Checks: `cargo test --workspace --exclude moria-client`, `cargo check -p moria-client --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. ALWAYS `CARGO_TARGET_DIR=/Users/nicolaslallier/OpenCode/Moria/target` (never a `target/` in the worktree; the disk filled up once). Broker tests are `#[ignore]`, read `AMQP_TEST_URL`, run with `--test-threads=1`.

## Plan notes (differences from the spec)

1. **No client banner.** Spec §6 proposes a "nourriture indisponible" banner; it needs a protocol change and client UI. Out of this plan (YAGNI); follow-up with the other season effects.
2. **Blind proposals.** The container has no map, so cow moves and births are proposals in a random direction; the server refuses invalid ones (not grass, occupied, pinned) and the container reverts on `food.reject`. This is the spec's refusal mechanism, used on purpose instead of shipping the grass layer.

## Review Focus

1. **A food container that starts empty or restarts must never remove cows or fields from the map.** Pinned by `empty_food_state_proposes_nothing` (Task 2) and `snapshot_adoption_never_emits_events` (Task 2).
2. **A cow butchered right before a stale move proposal arrives** must not reappear or move: the server refuses, the container forgets it. Pinned by `move_of_a_butchered_cow_is_refused` (Task 6) and `reject_of_a_move_forgets_the_cow_if_its_origin_is_gone` (Task 3).
3. **A herd that never explodes** over many seasons even when births are refused. Pinned by `herd_never_exceeds_cap_over_two_years` (Task 3).
4. **Winter:** no crop growth, no breeding. Pinned by `no_crop_grows_in_winter`, `no_birth_in_winter` (Task 3).
5. **Famine deadlock:** ripe crops must stay designated for harvest even when the food container is down. Pinned by `ripe_crops_stay_designated_without_food_events` (Task 6).

---

### Task 1: Calendar in `moria-core`

**Files:**
- Create: `crates/moria-core/src/calendar.rs`
- Modify: `crates/moria-core/src/lib.rs` (`pub mod calendar;`)

**Interfaces:**
- Produces: `pub const SEASON_DAYS: u64 = 7`, `SEASON_TICKS: u64`, `YEAR_TICKS: u64`; `#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] pub enum Season { Spring, Summer, Autumn, Winter }`; `pub fn season(tick: u64) -> Season`; `pub fn day_of_season(tick: u64) -> u8` (0..=6); `pub fn year(tick: u64) -> u64`.

- [ ] **Step 1: Write failing tests** (stub the three functions with `todo!()`): `tick_zero_is_spring_day_zero`, `season_boundaries` (tick `SEASON_TICKS - 1` is Spring, `SEASON_TICKS` is Summer, `3 * SEASON_TICKS` is Winter, `YEAR_TICKS` is Spring again with `year == 1`), `day_of_season_cycles` (`day_of_season(6 * DAY_TICKS) == 6`, `day_of_season(7 * DAY_TICKS) == 0`), `year_length_is_28_days`.
- [ ] **Step 2: Run** `cargo test -p moria-core calendar` — expect FAIL (panics at `todo!()`).
- [ ] **Step 3: Implement** the three functions from `light::DAY_TICKS` with integer division and modulo only.
- [ ] **Step 4: Run** the same command — expect PASS.
- [ ] **Step 5: Commit** `feat(core): calendrier des saisons`

### Task 2: `FoodSim` model, adoption and actions (pure)

**Files:**
- Create: `crates/moria-core/src/food.rs`
- Modify: `crates/moria-core/src/lib.rs` (`pub mod food;`)

**Interfaces:**
- Consumes: `TilePos`, `MountainId`.
- Produces (derive `Serialize, Deserialize, Clone, Debug, PartialEq` on all): `pub const COW_EVERY: u64 = 10`, `GROW_EVERY: u64 = 100`, `RIPEN_ODDS: u64 = 60`, `COW_CAP: usize = 24`; `pub enum CellKind { Cow, Field, Crop }`; `pub struct Cell { pub pos: TilePos, pub kind: CellKind }`; `pub enum FoodEvent { CowMoved { from: TilePos, to: TilePos }, CowBorn { at: TilePos }, CropRipe { at: TilePos } }`; `pub enum FoodAction { CowRemoved(TilePos), FieldSown(TilePos), FieldHarvested(TilePos), FieldRemoved(TilePos) }`; `pub struct FoodSim { .. }` with `FoodSim::new(seed: u64)`, `adopt(&mut self, cells: &[Cell], tick: u64)`, `apply_action(&mut self, a: &FoodAction)`, `cow_count(&self) -> usize`, `field_count(&self) -> usize`, `is_ripe(&self, p: TilePos) -> bool`, `has_cow(&self, p: TilePos) -> bool`.

- [ ] **Step 1: Write failing tests** (stubs with `todo!()`): `adopt_creates_default_state_for_unknown_cells` (a cow cell becomes an adult cow, a Field cell a growing field, a Crop cell a ripe field), `adopt_keeps_hidden_state_of_known_entities` (a cow with a custom fertile_at keeps it after a second `adopt` that still lists it), `adopt_forgets_entities_not_on_the_map`, `empty_food_state_proposes_nothing` (`FoodSim::new(1)` has no cows; the API has no way to delete from the map), `snapshot_adoption_never_emits_events`, `actions_are_idempotent` (applying `CowRemoved` twice, or on an unknown cell, changes nothing), `field_sown_adds_a_growing_field`, `field_harvested_resets_to_growing`, `field_removed_forgets_it`.
- [ ] **Step 2: Run** `cargo test -p moria-core food` — expect FAIL.
- [ ] **Step 3: Implement** the model: cows keyed by position with `born: u64` and `fertile_at: u64`, fields keyed by position with `progress: u32` and `ripe: bool`, plus `seed`. `adopt` rebuilds the maps from the snapshot as described in the spec §5.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): modèle de la simulation de nourriture`

### Task 3: Stepping rules and rejects (pure)

**Files:**
- Modify: `crates/moria-core/src/food.rs`

**Interfaces:**
- Consumes: Task 1 `Season`, `season()`; Task 2 types.
- Produces: `FoodSim::step(&mut self, tick: u64) -> Vec<FoodEvent>`; `FoodSim::reject(&mut self, ev: &FoodEvent)`; consts `ADULT_TICKS: u64 = DAY_TICKS`, `BIRTH_COOLDOWN: u64 = 2 * DAY_TICKS`.

- [ ] **Step 1: Write failing tests:** `cows_step_only_on_cow_ticks` (no event at ticks not multiple of 10), `a_cow_proposes_a_horizontal_neighbour` (the proposed `to` is one of the 4 horizontal neighbours, same z), `winter_cows_step_half_as_often`, `no_birth_in_winter`, `births_need_an_adult_and_a_cooldown`, `birth_proposal_is_next_to_a_parent`, `herd_never_exceeds_cap_over_two_years` (simulate `2 * YEAR_TICKS` ticks with every birth accepted, then another run with half refused; `cow_count() <= COW_CAP` always), `no_crop_grows_in_winter` (a sown field never ripens in a winter-only run), `summer_ripening_matches_the_old_odds` (over many fields in summer, the ripe count after a few growth checks is within the range the old `fields_ripen_and_call_for_harvest` test asserted: ≤ 10 after one check, ≥ 60 of 100 after enough), `crops_grow_slower_in_spring_and_autumn`, `step_is_deterministic` (same seed and same ticks give the same events), `reject_of_a_move_puts_the_cow_back`, `reject_of_a_move_forgets_the_cow_if_its_origin_is_gone`, `reject_of_a_birth_removes_it`, `reject_of_ripe_marks_the_field_growing_again_with_zero_progress`.
- [ ] **Step 2: Run** `cargo test -p moria-core food` — expect FAIL.
- [ ] **Step 3: Implement** `step` and `reject` using the stateless `mix(pos, n)` hash moved from `sim.rs` (copy it; keep the splitmix finalizer) combined with `seed`; `step` also updates the container's own state optimistically (a proposed move already moves the cow in the sim).
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): règles des troupeaux, des cultures et des saisons`

### Task 4: `food.bin` persistence (pure)

**Files:**
- Modify: `crates/moria-core/src/food.rs`

**Interfaces:**
- Produces: `pub struct FoodStore { pub mountains: BTreeMap<MountainId, FoodSim> }` (derive `Default`, serde); `FoodStore::save(&self, path: &Path) -> io::Result<()>`; `FoodStore::load_or_default(path: &Path) -> io::Result<FoodStore>`; `const FOOD_VERSION: u32 = 1`.

- [ ] **Step 1: Write failing tests** (copy the inventory ones): `save_then_load_roundtrips`, `load_missing_file_is_default`, `load_rejects_garbage_and_truncated`, `load_rejects_newer_version`.
- [ ] **Step 2: Run** `cargo test -p moria-core food` — expect FAIL.
- [ ] **Step 3: Implement** exactly like `State::save`/`load_or_default` in `crates/moria-core/src/inventory.rs` (temp file, `sync_all`, rename, parent-directory fsync, 4-byte LE version header, any problem other than NotFound is `Err`).
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): sauvegarde atomique de la nourriture`

### Task 5: Bus messages (pure)

**Files:**
- Create: `crates/moria-core/src/food_msg.rs`
- Modify: `crates/moria-core/src/lib.rs` (`pub mod food_msg;`)

**Interfaces:**
- Produces: consts `KEY_TICK = "food.tick"`, `KEY_WORLD = "food.world"`, `KEY_ACTION = "food.action"`, `KEY_REJECT = "food.reject"`, `KEY_EVENTS = "food.events"`, `KEY_STATE = "food.state"`, `MAX_FOOD_WORLD_BYTES: usize = 262_144`, `MAX_FOOD_MSG_BYTES: usize = 16_384`; messages (serde): `Tick { tick: u64 }`, `World { tick: u64, mountains: Vec<(MountainId, Vec<Cell>)> }`, `Actions { tick: u64, mountain: MountainId, list: Vec<FoodAction> }`, `Rejects { tick: u64, mountain: MountainId, list: Vec<FoodEvent> }`, `Events { tick: u64, mountain: MountainId, list: Vec<FoodEvent> }`, `Status { tick: u64 }`; `pub fn decode<T: DeserializeOwned>(bytes: &[u8], max: usize) -> Option<T>` (rejects over `max` before postcard, garbage returns `None`).

- [ ] **Step 1: Write failing tests:** `roundtrip_every_message`, `decode_rejects_oversized_before_decoding`, `decode_rejects_garbage`, `max_world_fits_in_the_limit` (a world with `COW_CAP` cows and 4096 fields on 10 mountains encodes under `MAX_FOOD_WORLD_BYTES`).
- [ ] **Step 2: Run** `cargo test -p moria-core food_msg` — expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): messages du bus nourriture`

### Task 6: Server validation, snapshot and in-process mode

**Files:**
- Create: `crates/moria-server/src/food.rs`
- Modify: `crates/moria-server/src/sim.rs` (remove `graze`, `grow`, `COW_EVERY`, `GROW_EVERY`, `RIPEN_ODDS`, `mix` if unused; keep `on_ground`; add `Changes.food: Vec<FoodAction>`; emit actions where cows and fields are created or removed by actions), `crates/moria-server/src/lib.rs` (`mod food;`, drive the in-process `FoodSim`s from `State::step`)

**Interfaces:**
- Consumes: Tasks 2, 3, 5.
- Produces: `pub fn validate(m: &Mountain, f: &Fortress, ev: &FoodEvent) -> bool` (the `pinned` and `occupied` rules of today's `graze`, destination must be `Tile::Grass`, origin must still be `Tile::Cow`; a birth needs a free `Grass`; a ripe needs a `Field`); `pub fn apply_events(m: &mut Mountain, f: &Fortress, evs: &[FoodEvent], tiles: &mut Vec<(TilePos, Tile)>) -> Vec<FoodEvent>` (applies the valid ones to the grid, returns the rejected); `pub fn snapshot(m: &Mountain) -> Vec<Cell>`; `pub fn keep_harvest_designated(m: &mut Mountain, ch: &mut Changes)` (the Chop re-add loop that today lives at the end of `grow`)

- [ ] **Step 1: Write failing tests** (move the existing `graze`/`grow` tests from `sim.rs` to the new functions and keep their assertions): `move_of_a_butchered_cow_is_refused`, `pinned_cow_move_is_refused` (designated cow, hero target, target tile occupied by a dwarf), `valid_move_updates_the_grid_and_emits_tiles`, `birth_needs_free_grass`, `ripe_turns_a_field_into_a_crop`, `ripe_crops_stay_designated_without_food_events` (with no `FoodSim` running at all, `keep_harvest_designated` re-adds the Chop designation even past the designation cap, as `grow` did), `snapshot_lists_cows_fields_and_crops_of_the_ground_level`, `butchering_a_cow_emits_cow_removed`, `building_a_farm_emits_field_sown`, `harvest_emits_field_harvested`, `digging_a_field_emits_field_removed`, `in_process_mode_moves_cows_like_before` (a world with cows: after 10 ticks of `State::step` without bus, at least one cow moved or none stayed illegally; tiles broadcast to the owner's `TickDelta`).
- [ ] **Step 2: Run** `cargo test -p moria-server food` and `cargo test -p moria-server sim` — expect FAIL.
- [ ] **Step 3: Implement.** In `State::step`, keep `COW_EVERY`/`GROW_EVERY` out of `sim.rs`: after `sim::step`, for each fortress run its in-process `FoodSim` (lazily created per mountain from `snapshot(m)` with `adopt` at first use, then every 5 minutes re-`adopt` for anti-entropy), apply the proposals with `apply_events`, feed the rejects back with `FoodSim::reject`, feed `Changes.food` actions to `apply_action`, call `keep_harvest_designated` every `GROW_EVERY` ticks, and fold the tile changes into the per-mountain `Changes` that is merged into the `TickDelta` (the existing `merge_into`). Find every site where a `Tile::Cow`/`Field`/`Crop` appears or disappears through an action with `grep -n "Tile::Field\|Tile::Crop\|Tile::Cow" crates/moria-server/src/sim.rs` and funnel them through one helper that pushes the `FoodAction`.
- [ ] **Step 4: Run** `cargo test --workspace --exclude moria-client` — expect PASS, with every pre-existing sim test (butcher, harvest, famine, fields) still green.
- [ ] **Step 5: Commit** `feat(server): règles de nourriture validées par la grille, mode sans bus`

### Task 7: `moria-food` service

**Files:**
- Create: `crates/moria-food/Cargo.toml`, `crates/moria-food/src/lib.rs`, `crates/moria-food/src/main.rs`
- Test: in-file unit tests and `crates/moria-food/tests/broker.rs`

**Interfaces:**
- Consumes: `FoodStore`, `FoodSim`, messages (Task 5), `Bus`.
- Produces: `pub fn handle_world(store: &mut FoodStore, w: &World)`, `pub fn handle_tick(store: &mut FoodStore, t: &Tick) -> Vec<Events>` (one `Events` per mountain that proposed something), `pub fn handle_action(store: &mut FoodStore, a: &Actions)`, `pub fn handle_reject(store: &mut FoodStore, r: &Rejects)`; `pub async fn run(amqp_url: String, data: PathBuf, shutdown: impl Future<Output = ()>) -> anyhow::Result<()>`; env `AMQP_URL`, `MORIA_FOOD_FILE` (default `/data/food.bin`).

- [ ] **Step 1: Write failing tests:** `tick_steps_every_known_mountain`, `unknown_mountain_in_an_action_is_ignored`, `stale_action_older_than_the_last_snapshot_is_ignored`, `garbage_payload_is_ignored` for each key, `oversized_payload_is_ignored`, `refuses_to_start_on_corrupt_file`, `refuses_to_start_when_data_dir_is_missing`; ignored broker tests: `world_in_events_out` (publish a `food.world` with two cows and a `food.tick` train, receive `food.events`), `heartbeat_is_published_every_second`, `restart_keeps_hidden_state` (state survives via `food.bin`).
- [ ] **Step 2: Run** `cargo test -p moria-food` — expect FAIL.
- [ ] **Step 3: Implement** `run` with the inventory service's structure (`crates/moria-inventory/src/lib.rs`): load `FoodStore` (refuse to start on a corrupt file or an unwritable data path), `connect_forever`, subscribe with `Queue::Shared` names `food-tick` and `food-world` (both `latest_only: true`) and `food-action`, `food-reject` (`latest_only: false`), a single `select!` loop, reconnect on stream end without losing the store, publish `food.state` every `STATE_EVERY`, save `food.bin` every 60 s and on shutdown (save before closing the bus). Never publish anything about a mountain it has no snapshot for.
- [ ] **Step 4: Run** unit tests, then the ignored broker tests against a throwaway broker (`docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine`, wait for `rabbitmq-diagnostics -q ping`, `AMQP_TEST_URL=amqp://guest:guest@127.0.0.1:5672 ... -- --ignored --test-threads=1`, always `docker stop` it) — expect PASS.
- [ ] **Step 5: Commit** `feat(food): conteneur moria-food`

### Task 8: Server bus link for food

**Files:**
- Create: `crates/moria-server/src/food_bridge.rs`
- Modify: `crates/moria-server/src/lib.rs`, `crates/moria-server/src/metrics.rs`, `crates/moria-server/src/food.rs`, `crates/moria-server/src/sim.rs` (skip the meal while paused), `crates/moria-core/src/world.rs` (`World.food_paused: bool` with `#[serde(skip)]`, so the save format is unchanged)

**Interfaces:**
- Consumes: Task 6 `validate`/`apply_events`/`snapshot`, Task 5 messages, `Bus`.
- Produces: `food_bridge::run(url: String, out: mpsc::Receiver<Outgoing>, inc: mpsc::Sender<Incoming>)` where `enum Outgoing { Tick(Tick), World(World), Actions(Actions), Rejects(Rejects) }` and `enum Incoming { Events(Events), Status(Status) }`; `pub fn decode_incoming(key: &str, bytes: &[u8]) -> Option<Incoming>`; `State` field `food_up_at: Option<std::time::Instant>`; metric gauge `moria_food_up` (1 when a `food.state` was seen within `STOPPED_AFTER`); `World.food_paused: bool` (not saved); in `sim::step` the meal tick is skipped while `w.food_paused` is true.

- [ ] **Step 1: Write failing tests:** `meal_skipped_while_food_paused` (a meal tick with `food_paused = true` sets nobody hungry and takes no food; the same tick with `false` still does; a hungry dwarf stays hungry), `garbage_and_oversized_incoming_are_ignored`, `food_up_expires_after_five_seconds` (pass an explicit `now`), `world_is_resent_when_heartbeat_returns` (state machine: heartbeat missing then back ⇒ one `World` is queued), `world_is_resent_every_five_minutes`, `events_are_validated_and_rejects_sent_back`, `food_paused_follows_staleness_when_a_bus_is_configured` (and is always false without bus).
- [ ] **Step 2: Run** `cargo test -p moria-server food` — expect FAIL.
- [ ] **Step 3: Implement.** The bridge uses its own `Bus::connect_forever` connection (reconnect on stream end with the 2 s pause raced against shutdown, as `bridge.rs` does), subscribes to `food.events` and `food.state` with `Queue::Exclusive`, and publishes everything non-durably. The world task sends one `food.tick` per tick, and a `World` snapshot when the heartbeat first appears, when it reappears after a gap, and every 5 minutes; it never awaits (bounded channels, `try_send`, a full channel drops the message). Incoming events go through `validate`/`apply_events`; rejects are sent back in one message per mountain. With a bus configured, `State` no longer runs the in-process `FoodSim` (it is replaced by the container) and sets `world.food_paused` from the heartbeat (always false without a bus); the harvest-designation loop (`keep_harvest_designated`) still runs in the server.
- [ ] **Step 4: Run** `cargo test --workspace --exclude moria-client` and clippy — expect PASS.
- [ ] **Step 5: Commit** `feat(server): lien avec le conteneur nourriture`

### Task 9: End-to-end with a real broker

**Files:**
- Modify: `crates/moria-server/tests/e2e.rs`, `crates/moria-server/Cargo.toml` (dev-dependency `moria-food`)

- [ ] **Step 1: Write failing `#[ignore]` tests** (shared helpers from the inventory e2e tests; fresh temp dirs; unique queue names or purges): `cows_graze_through_the_container` (server + food; after some seconds at least one cow tile moved in the client's view, and the grid has the same number of cows as at the start), `butchered_cow_does_not_come_back` (butcher a cow, wait several steps and a snapshot: the cell stays grass), `food_outage_freezes_food_and_pauses_hunger` (stop the container: cows stop moving, no dwarf becomes hungry across a meal tick; the dig and harvest still work; restart: cows move again and hunger resumes), `food_started_empty_deletes_nothing` (start the container on a fresh `food.bin`: the cow and field counts in the client's view are unchanged), `server_restart_keeps_the_herd` (restart the server with the container running: the grid is the same, no duplicated cow), `crop_ripens_and_stays_designated` (a sown field ripens in summer and its Chop designation exists even with the container stopped afterwards).
- [ ] **Step 2: Run** them against a throwaway broker (`--ignored --test-threads=1`, container stopped after) — expect FAIL before the wiring is complete, PASS after Task 8. Report any flakiness; the known unrelated failure `ai_restart_dwarves_pause_then_resume` stays.
- [ ] **Step 3: Fix** what they expose, in the owning crate, with a separate commit and a note in the report.
- [ ] **Step 4: Run** the whole suite plus clippy and fmt — expect PASS.
- [ ] **Step 5: Commit** `test(e2e): nourriture, panne du conteneur et redémarrages`

### Task 10: Deploy files

**Files:**
- Modify: `.github/workflows/server.yml` (matrix gains `moria-food`), `docker-compose.yml` (service `food`: image `ghcr.io/nicolaslallier/moria-food:sha-<same pinned sha>` with a comment that the CI bot rewrites it, volume `moria-food:/data`, `stop_grace_period: 30s`, `restart: unless-stopped`, network `infra-net`, `AMQP_URL` through the existing `*amqp` anchor; volume `moria-food` with `name: moria-food`), `docker-compose.override.yml` (local build entry), `docker/Dockerfile` (bin list in the error message), `README.md` (French: the food container, what it owns, `food.bin`, what happens when it is down, seasons are a function of the tick, the reset procedure also removes `food.bin`)

- [ ] **Step 1: Write the check:** `MORIA_INVITE=x MORIA_AMQP_PASSWORD=Abc123 docker compose -f docker-compose.yml config` must show a `food` service on `infra-net` with the volume, and `grep -n "moria-food" .github/workflows/server.yml docker-compose.yml` must find the matrix entry and the image. Run it first — it fails.
- [ ] **Step 2: Edit** the files; the pin sed regex `moria-[a-z-]+` already covers the new image.
- [ ] **Step 3: Run** the check again plus `cargo test --workspace --exclude moria-client` — expect PASS.
- [ ] **Step 4: Commit** `deploy: conteneur nourriture`
- [ ] **Step 5 (after the user approves merging, not before):** deploy order Moria stack after the inventory PRs are merged; check the new GHCR package is pullable; update the project memory note.
