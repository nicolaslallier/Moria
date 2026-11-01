# Inventory container Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move every fortress resource into a `moria-inventory` container on the Infra RabbitMQ; the server keeps a cache and keeps playing when the inventory is down.

**Architecture:** All inventory rules (apply, dedup, clamp, cache reconcile) live in Bevy-free, broker-free modules of `moria-core`, unit-tested without RabbitMQ. A small `moria-bus` crate wraps `lapin` for the three binaries (server, dwarf-ai, inventory). The server leaves `sim.rs` untouched: `Fortress.resources` stays the live cache, and the server diffs it around each `sim::step` to build one batch per mountain per tick.

**Tech Stack:** Rust 2024, tokio, `lapin` (AMQP 0-9-1), postcard, RabbitMQ 4 (Infra stack), Portainer/GHCR deploy.

**Spec:** `docs/superpowers/specs/2026-11-01-inventaire-design.md`

## Global Constraints

- TDD is mandatory: every task writes its failing test first, runs it RED, then GREEN.
- Exchange `moria` (topic, durable) in vhost `moria`. Routing keys exactly: `ai.view`, `ai.assign`, `inventory.apply`, `inventory.state`.
- Inventory publishes `inventory.state` every **1 s** and right after each applied batch; the server calls the inventory stopped after **5 s** without one. The stopped flag only drives the metric `moria_nats_connected` (name kept so the Infra Grafana panel keeps working).
- Resource order inside a batch: **adds first, then takes**; a take below zero is clamped to zero and the build stays.
- A batch is `{sender, epoch, seq, mountain, add, take, seed}`; `epoch` = Unix ms at emitter start; dedup is per sender on `(epoch, seq)`; a newer epoch resets numbering.
- `inventory.bin` holds totals, the per-sender `(epoch, seq)` table and the per-mountain `seeded` flags **in one atomic write** (temp file + rename, 4-byte LE version header), written after every applied batch and on SIGTERM.
- The simulation never awaits the bus: batches go through a bounded channel to a bridge task.
- `PROTOCOL_VERSION` and `SAVE_VERSION` do **not** change (see Plan notes).
- User-visible strings and log messages are in French (like the existing code).
- Checks: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`. Share the target dir: `CARGO_TARGET_DIR=/Users/nicolaslallier/OpenCode/Moria/target`.
- Broker tests are `#[ignore]` and read `AMQP_TEST_URL`; everything else runs without RabbitMQ.

## Plan notes (deliberate differences from the spec — amend the spec in Task 11)

1. **`world.bin` keeps its shape.** `Fortress.resources` stays serialized as the cache's last value. No `SAVE_VERSION` bump and no v5 reader. Migration: at every start the server sends a `seed` batch from the loaded resources for each mountain until `inventory.state` lists that mountain as `seeded`; the inventory applies a seed once per mountain.
2. **Replay = bridge retry.** Batches are published durably, so the broker queue already holds those sent during an inventory outage. The bridge only re-sends batches whose publish or confirm failed (broker down). The unsent list is capped at `MAX_UNSENT = 100_000`; beyond it the oldest are dropped with an error log (spec said merge; merging republished batches under a new `seq` can double count).

## Review Focus

1. **Server restarts and numbers from 1 again while the inventory keeps its table**: batches must not be dropped. Pinned by `newer_epoch_resets_numbering` (Task 1).
2. **Inventory killed between applying a batch and the broker redelivering it**: no double apply. Pinned by `redelivered_batch_not_applied_twice_after_reload` (Task 2).
3. **`inventory.bin` truncated or garbage**: the inventory refuses to start; it never silently starts at zero stock. Pinned by `load_rejects_garbage_and_truncated` (Task 2).
4. **Inventory down for a long time**: cache stays correct, memory stays bounded. Pinned by `unsent_list_is_capped_oldest_first` (Task 9).
5. **Garbage or forged bytes on any queue**: ignored with a warning, never a panic. Pinned by `garbage_payload_is_ignored` (Task 7).

---

### Task 1: Inventory types and batch application

**Files:**
- Create: `crates/moria-core/src/inventory.rs`
- Modify: `crates/moria-core/src/lib.rs` (add `pub mod inventory;`), `crates/moria-core/src/tile.rs` (add `Resources::delta`)
- Test: in-file `#[cfg(test)]` modules

**Interfaces:**
- Produces (all `pub`, derive `Serialize, Deserialize, Clone, Debug, PartialEq`):
  - `const EXCHANGE: &str = "moria"`, `KEY_APPLY: &str = "inventory.apply"`, `KEY_STATE: &str = "inventory.state"`, `STATE_EVERY: Duration = 1 s`, `STOPPED_AFTER: Duration = 5 s`
  - `struct Batch { sender: String, epoch: u64, seq: u64, mountain: MountainId, add: Resources, take: Resources, seed: bool }`
  - `struct State { totals: BTreeMap<MountainId, Resources>, seeded: BTreeSet<MountainId>, acks: BTreeMap<String, (u64, u64)> }` with `Default`
  - `State::apply(&mut self, b: &Batch) -> bool` — `true` if the ack advanced (applied or seed-ignored), `false` for a stale or duplicate batch
  - `Resources::delta(before: &Resources, after: &Resources) -> (Resources, Resources)` — `(add, take)`, per field the positive and negative difference

- [ ] **Step 1: Write failing tests** (in `inventory.rs`): `apply_adds_then_takes`, `take_below_zero_clamps_to_zero` (stone 1, take 5 → 0), `duplicate_batch_is_ignored`, `older_seq_is_ignored`, `newer_epoch_resets_numbering` (epoch 2 seq 1 after epoch 1 seq 9 is applied; epoch 1 seq 10 afterwards is ignored), `seed_applies_once_per_mountain` (second seed returns `true` for the ack but totals unchanged), `ordinary_batches_before_seed_do_not_block_it`, `senders_are_independent`. In `tile.rs`: `delta_splits_gains_and_losses` (before stone 5 wood 1, after stone 2 wood 4 → add wood 3, take stone 3).
- [ ] **Step 2: Run** `cargo test -p moria-core inventory delta` — expect compile failure / FAIL.
- [ ] **Step 3: Implement** the items above. `apply` uses `Resources::add` then `Resources::take` (already saturating) and records `acks[sender] = (epoch, seq)`.
- [ ] **Step 4: Run** the same command — expect PASS.
- [ ] **Step 5: Commit** `feat(core): inventaire — lots, état et application`

### Task 2: Inventory persistence

**Files:**
- Modify: `crates/moria-core/src/inventory.rs`

**Interfaces:**
- Consumes: `State` (Task 1)
- Produces: `State::save(&self, path: &Path) -> io::Result<()>`, `State::load_or_default(path: &Path) -> io::Result<State>` (missing file → `State::default()`, any other problem → `Err`), `const INVENTORY_VERSION: u32 = 1`

- [ ] **Step 1: Write failing tests** (use `std::env::temp_dir()` + unique name): `save_then_load_roundtrips`, `load_missing_file_is_default`, `load_rejects_garbage_and_truncated` (3 random bytes, and a valid file cut in half → `Err`), `load_rejects_newer_version`, `redelivered_batch_not_applied_twice_after_reload` (apply batch, save, load, apply the same batch → `false`, totals unchanged).
- [ ] **Step 2: Run** `cargo test -p moria-core inventory` — expect FAIL.
- [ ] **Step 3: Implement** `save`/`load_or_default` copying the temp-file-then-rename pattern of `world::save` (`crates/moria-core/src/world.rs`, `save`), with the 4-byte LE version header.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): sauvegarde atomique de l'inventaire`

### Task 3: Server-side cache

**Files:**
- Create: `crates/moria-core/src/inventory_cache.rs`
- Modify: `crates/moria-core/src/lib.rs` (`pub mod inventory_cache;`)

**Interfaces:**
- Consumes: `Batch`, `State`, `Resources`
- Produces:
  - `struct Cache` with `Cache::new(sender: &str, epoch: u64) -> Cache`
  - `Cache::push(&mut self, mountain: MountainId, add: Resources, take: Resources, seed: bool) -> Batch` — assigns the next `seq` (starts at 1), stores a clone in `pending`
  - `Cache::reconcile(&mut self, st: &State)` — drops pending batches of this sender/epoch with `seq <=` the acked seq
  - `Cache::view(&self, st: &State, m: MountainId) -> Resources` — `st.totals[m]` (zero if absent) with this mountain's pending batches replayed (add then take)
  - `Cache::pending_len(&self) -> usize`

- [ ] **Step 1: Write failing tests:** `push_numbers_from_one`, `view_is_totals_plus_pending`, `acked_batches_are_not_counted_twice` (push seq 1 add stone 3; state totals stone 3 with ack (epoch, 1); `reconcile`; `view` = 3, not 6), `ack_of_other_epoch_keeps_pending`, `unacked_batch_stays_in_view` (state acks seq 1 only, seq 2 pending → view counts seq 2), `view_clamps_at_zero`.
- [ ] **Step 2: Run** `cargo test -p moria-core inventory_cache` — expect FAIL.
- [ ] **Step 3: Implement** with a `Vec<Batch>` pending list.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(core): cache de l'inventaire côté serveur`

### Task 4: `moria-bus` crate

**Files:**
- Create: `crates/moria-bus/Cargo.toml`, `crates/moria-bus/src/lib.rs`
- Modify: `Cargo.toml` (workspace dep `lapin`, member is picked up by `crates/*`)

**Interfaces:**
- Consumes: `moria_core::inventory::EXCHANGE`
- Produces:
  - `struct Bus` ; `Bus::connect(url: &str) -> anyhow::Result<Bus>` (one attempt; declares the durable topic exchange `moria`; channel in confirm mode)
  - `Bus::connect_forever(url: &str) -> Bus` (retry every 2 s, logs in French)
  - `Bus::publish(&self, key: &str, body: Vec<u8>, durable: bool) -> anyhow::Result<()>` — `durable` ⇒ persistent message and waits for the publisher confirm; otherwise fire-and-forget
  - `enum Queue<'a> { Shared { name: &'a str, latest_only: bool }, Exclusive, Durable(&'a str) }`
  - `Bus::subscribe(&self, key: &str, q: Queue) -> anyhow::Result<impl Stream<Item = Vec<u8>> + Unpin>` — declares and binds the queue; `latest_only` sets `x-max-length: 1` with `drop-head`; `Durable` is a durable named queue; the stream ends when the connection dies (callers reconnect); each delivery is acked automatically
  - `Bus::is_connected(&self) -> bool`

- [ ] **Step 1: Write failing test** `crates/moria-bus/tests/broker.rs`, `#[ignore]`, reads `AMQP_TEST_URL`: `publish_reaches_subscriber` (subscribe `Queue::Exclusive` on `test.a`, publish `b"x"`, receive it within 2 s), `latest_only_keeps_one` (publish 3 to a `latest_only` shared queue with no consumer, then subscribe: only the last arrives), `durable_publish_survives_late_subscriber` (publish to a `Durable` queue before subscribing, then receive).
- [ ] **Step 2: Run** `AMQP_TEST_URL=... cargo test -p moria-bus -- --ignored` — expect FAIL (compile). Start a throwaway broker with `docker run -d --rm --name moria-amqp-test -p 5672:5672 rabbitmq:4-alpine`.
- [ ] **Step 3: Implement.** Check the current `lapin` API and executor features on docs.rs before writing (tokio runtime); follow `bridge.rs`'s retry style (2 s, French warnings).
- [ ] **Step 4: Run** the same command — expect PASS.
- [ ] **Step 5: Commit** `feat(bus): crate moria-bus autour de lapin`

### Task 5: `dwarf-ai` on the bus

**Files:**
- Modify: `crates/moria-core/src/ai.rs` (replace `SUBJECT_VIEW`/`SUBJECT_ASSIGN` with `KEY_VIEW = "ai.view"`, `KEY_ASSIGN = "ai.assign"`), `crates/moria-dwarf-ai/src/lib.rs`, `crates/moria-dwarf-ai/src/main.rs`, `crates/moria-dwarf-ai/Cargo.toml` (drop `async-nats`, add `moria-bus`)
- Test: the existing `assign.rs` tests must stay green; the broker behaviour is covered by Task 6's e2e

**Interfaces:**
- Consumes: `Bus::connect_forever`, `Bus::subscribe(KEY_VIEW, Queue::Shared { name: "dwarf-ai", latest_only: true })`, `Bus::publish(KEY_ASSIGN, _, false)`
- Produces: `pub async fn run(amqp_url: String, shutdown: impl Future<Output = ()>) -> anyhow::Result<()>` (same shape, renamed parameter); env var `AMQP_URL` (default `amqp://moria:moria@rabbitmq:5672/moria`)

- [ ] **Step 1: Write failing test** in `crates/moria-dwarf-ai/src/lib.rs`: `keys_are_the_documented_ones` asserting `KEY_VIEW == "ai.view"` and `KEY_ASSIGN == "ai.assign"` (fails to compile before the rename).
- [ ] **Step 2: Run** `cargo test -p moria-dwarf-ai` — expect FAIL.
- [ ] **Step 3: Implement** the swap in `run`: same loop as today, decode with postcard, on stream end reconnect via `connect_forever` instead of bailing.
- [ ] **Step 4: Run** `cargo test -p moria-dwarf-ai && cargo clippy -p moria-dwarf-ai --all-targets -- -D warnings` — expect PASS.
- [ ] **Step 5: Commit** `refactor(dwarf-ai): passe de NATS à RabbitMQ`

### Task 6: Server bridge on the bus (AI traffic only)

**Files:**
- Modify: `crates/moria-server/src/bridge.rs`, `crates/moria-server/src/lib.rs` (`Config.nats_url` → `amqp_url`), `crates/moria-server/src/main.rs` (`AMQP_URL`), `crates/moria-server/Cargo.toml`, `crates/moria-server/tests/e2e.rs`, `crates/moria-server/tests/net.rs` (field rename), `Makefile` (`e2e` target)

**Interfaces:**
- Consumes: Task 4 `Bus`, Task 5 keys
- Produces: `bridge::run(url: String, known: BTreeSet<MountainId>, views: mpsc::Receiver<AiView>, assign: mpsc::Sender<Assignments>)` — unchanged signature; `bridge::accept` unchanged; `Config.amqp_url: Option<String>`

- [ ] **Step 1: Write failing test:** `filter_drops_oversized_and_unknown` already exists and stays; add `bridge::tests::garbage_assignment_bytes_are_rejected` (calls a new `fn decode_assignments(bytes: &[u8], known: &BTreeSet<MountainId>) -> Option<Assignments>` with `[0xff; 5]` → `None`, and a valid encoding of an oversized list → `None`). Update `e2e.rs` to use `AMQP_TEST_URL` and `amqp_url`.
- [ ] **Step 2: Run** `cargo test -p moria-server bridge` — expect FAIL.
- [ ] **Step 3: Implement** `decode_assignments` and rewrite `run`: connect with `connect_forever`, subscribe `KEY_ASSIGN` with `Queue::Exclusive`, publish views non-durably, `M.nats_connected` follows `Bus::is_connected`; on stream end loop back to reconnect.
- [ ] **Step 4: Run** `cargo test -p moria-server` and, with a broker up, `AMQP_TEST_URL=amqp://guest:guest@127.0.0.1:5672 cargo test -p moria-server --test e2e -- --ignored --test-threads=1` — expect PASS. Replace the `e2e` Makefile target's NATS container by `rabbitmq:4-alpine` and `AMQP_TEST_URL`.
- [ ] **Step 5: Commit** `refactor(server): le pont IA passe sur RabbitMQ`

### Task 7: `moria-inventory` service

**Files:**
- Create: `crates/moria-inventory/Cargo.toml`, `src/lib.rs`, `src/main.rs`
- Test: `crates/moria-inventory/src/lib.rs` tests; `crates/moria-inventory/tests/broker.rs` (`#[ignore]`)

**Interfaces:**
- Consumes: `State`, `Batch`, `KEY_APPLY`, `KEY_STATE`, `STATE_EVERY`, `Bus`
- Produces:
  - `fn handle(state: &mut State, payload: &[u8]) -> bool` — decodes a `Batch` with postcard; garbage → warn, `false`; otherwise `State::apply`
  - `pub async fn run(amqp_url: String, data: PathBuf, shutdown: impl Future<Output = ()>) -> anyhow::Result<()>` — loads `data` with `State::load_or_default` (error ⇒ return `Err`, never start empty), single task: for each delivery from `Queue::Durable("inventory")` bound to `KEY_APPLY` → `handle`; if it returned `true`, `State::save` **then** publish the state (non-durable, `KEY_STATE`); a `STATE_EVERY` timer also publishes; on `shutdown` save and return
  - env: `AMQP_URL`, `MORIA_INVENTORY_FILE` (default `/data/inventory.bin`)

- [ ] **Step 1: Write failing tests:** `garbage_payload_is_ignored` (`handle(&mut s, &[0xff; 7])` → `false`, state unchanged), `handle_applies_a_valid_batch`; broker test `batch_in_state_out` (publish a batch, receive an `inventory.state` containing its totals and ack, file on disk reloads to the same state).
- [ ] **Step 2: Run** `cargo test -p moria-inventory` — expect FAIL.
- [ ] **Step 3: Implement** `handle`, `run`, `main` (same SIGTERM handling as `moria-dwarf-ai/src/main.rs`). Order inside the loop is save-then-publish so a published state never runs ahead of disk.
- [ ] **Step 4: Run** the unit tests, then the ignored broker test — expect PASS.
- [ ] **Step 5: Commit** `feat(inventory): conteneur moria-inventory`

### Task 8: Server batch builder (pure)

**Files:**
- Create: `crates/moria-server/src/inventory.rs`
- Modify: `crates/moria-server/src/lib.rs` (`mod inventory;`)

**Interfaces:**
- Consumes: `Cache`, `State`, `Resources::delta`, `World`
- Produces: `struct InvSync` with
  - `InvSync::new(sender: &str, epoch: u64) -> InvSync`
  - `InvSync::begin_tick(&mut self, w: &mut World) -> ()` — for each fortress sets `f.resources = cache.view(&state, m)` **only for mountains the state has seen or that have no pending seed**; then snapshots `before`
  - `InvSync::end_tick(&mut self, w: &World) -> Vec<Batch>` — for each fortress, `Resources::delta(before, f.resources)`; skips an all-zero delta; sends pending seeds (mountains not in `state.seeded`) as `seed: true` batches built from the world's loaded resources, once per start until seeded
  - `InvSync::on_state(&mut self, st: State)` — stores it and calls `cache.reconcile`
  - `InvSync::inventory_up(&self, now: Instant) -> bool` — a state received within `STOPPED_AFTER`

- [ ] **Step 1: Write failing tests** (build a `World` via `moria_core::world::generate` + `claim_mountain`): `digging_yield_becomes_an_add_batch`, `build_cost_becomes_a_take_batch`, `quiet_tick_makes_no_batch`, `state_replaces_stale_world_bin_resources` (world.bin food 12, state food 7 → after `begin_tick` fortress food 7), `unacked_batch_survives_a_new_state` (no double count), `seed_is_sent_until_state_lists_it` (two `end_tick` calls without state → seed batch both times, none after `on_state` with `seeded`), `inventory_up_expires_after_five_seconds`.
- [ ] **Step 2: Run** `cargo test -p moria-server inventory::` — expect FAIL.
- [ ] **Step 3: Implement** `InvSync`. Before the first state arrives `begin_tick` leaves `f.resources` alone (the game plays on the loaded value). Mark with `// ponytail: net diff per tick, so add-then-take inside one tick clamps on the net; fine unless a tick both exhausts and refills a resource`.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `feat(server): synchro de l'inventaire par différence de tick`

### Task 9: Wire the inventory into the server

**Files:**
- Modify: `crates/moria-server/src/bridge.rs`, `crates/moria-server/src/lib.rs` (State struct, `step`, `run_with_listener`), `crates/moria-server/src/metrics.rs`

**Interfaces:**
- Consumes: `InvSync` (Task 8), `Bus`, `KEY_APPLY`, `KEY_STATE`
- Produces:
  - `bridge::run` gains `batches: mpsc::Receiver<Batch>` and `states: mpsc::Sender<State>`; it publishes each batch durably, keeps failed ones in a `VecDeque<Batch>` capped by `const MAX_UNSENT: usize = 100_000` (drop oldest, `tracing::error!`), re-sends them in order after every reconnect, and forwards decoded `inventory.state` messages (non-durable queue, `Queue::Exclusive`)
  - `fn push_unsent(q: &mut VecDeque<Batch>, b: Batch)` — the cap logic, unit-testable
  - `State.inv: InvSync`; `epoch` = `SystemTime` Unix ms at start; sender `"server"`
  - in `State::step`: drain `states_rx` (bounded, like `MAX_ASSIGN_PER_TICK`) → `inv.on_state`; `inv.begin_tick(&mut w)` right before `sim::step`; `inv.end_tick(&w)` right after, each batch `try_send` to the bridge (full channel = log + count, the unsent logic covers the broker side); metric `moria_nats_connected` also requires `inv.inventory_up` (keep one gauge)
  - a mountain claimed after start is seeded by the normal `end_tick` seed rule (no extra code path)

- [ ] **Step 1: Write failing tests:** `unsent_list_is_capped_oldest_first` (push `MAX_UNSENT + 3` → len is `MAX_UNSENT`, first element is the 4th pushed); in `crates/moria-server/tests/net.rs` style, `game_keeps_running_without_inventory` (server with `amqp_url: None`: dig a tile, tick advances, hero's `resources` in the next `TickDelta` include the yield — the cache alone carries the game).
- [ ] **Step 2: Run** `cargo test -p moria-server` — expect FAIL.
- [ ] **Step 3: Implement** the wiring above. `amqp_url: None` must keep working: `InvSync` then runs cache-only and `end_tick` batches are dropped.
- [ ] **Step 4: Run** `cargo test -p moria-server && cargo clippy --workspace --all-targets -- -D warnings` — expect PASS.
- [ ] **Step 5: Commit** `feat(server): cache d'inventaire branché sur le bus`

### Task 10: End-to-end with a real broker, including an outage

**Files:**
- Modify: `crates/moria-server/tests/e2e.rs`, `crates/moria-server/Cargo.toml` (dev-dep `moria-inventory`)

**Interfaces:**
- Consumes: `moria_inventory::run`, `moria_dwarf_ai::run`, server `run_with_listener`

- [ ] **Step 1: Write failing `#[ignore]` tests:** `resources_flow_through_the_inventory` (server + inventory; a dig yield shows up in `inventory.state` totals and in the file), `inventory_outage_keeps_the_game_running` (stop the inventory task, dig and build for a few ticks, client resources keep updating; restart it on the same file: its totals equal the cache and no stone was counted twice), `server_restart_does_not_drop_batches` (restart the server mid-run, keep the inventory: new batches are applied).
- [ ] **Step 2: Run** with the throwaway broker: `AMQP_TEST_URL=... cargo test -p moria-server --test e2e -- --ignored --test-threads=1` — expect FAIL before wiring is complete, PASS after Task 9.
- [ ] **Step 3: Fix** whatever the outage tests expose (expected spots: reconnect loop in `bridge::run`, ack handling in `Cache::reconcile`).
- [ ] **Step 4: Run** the full suite plus `cargo fmt --check` — expect PASS.
- [ ] **Step 5: Commit** `test(e2e): inventaire, panne et redémarrage du serveur`

### Task 11: Deploy files, Infra change, spec amendment

**Files:**
- Modify: `.github/workflows/server.yml` (matrix `bin` gains `moria-inventory`), `docker-compose.yml` (remove `nats`; add `inventory` service: image `ghcr.io/nicolaslallier/moria-inventory:sha-…`, volume `moria-inventory:/data`, `infra-net`, `AMQP_URL`; `server` and `dwarf-ai` get `AMQP_URL` and `infra-net`, `depends_on` on `nats` removed), `docker-compose.override.yml` (build entry for `inventory`), `Makefile`, `README.md` (architecture line, NATS mentions, `AMQP_URL`), `docs/superpowers/specs/2026-11-01-inventaire-design.md` (§4/§6/§7 amended with Plan notes 1 and 2)
- Other repo: `/Users/nicolaslallier/OpenCode/Infra` — vhost `moria` and user `moria` with configure/write/read on that vhost only; password supplied to the Moria stack as the Portainer variable `MORIA_AMQP_PASSWORD`, used in `AMQP_URL=amqp://moria:${MORIA_AMQP_PASSWORD:?…}@rabbitmq:5672/moria`

- [ ] **Step 1: Write the check:** `docker compose -f docker-compose.yml config` must succeed with `MORIA_INVITE` and `MORIA_AMQP_PASSWORD` set, and `grep -n nats docker-compose.yml` must print nothing. Run it first: it fails (nats still there).
- [ ] **Step 2: Edit** the compose, CI matrix, override, Makefile and README as listed.
- [ ] **Step 3: Infra PR:** in the Infra repo (follow its `CLAUDE.md`/`AGENTS.md`), add the vhost and user creation (RabbitMQ definitions file or bootstrap script — builder's choice, spec §7), open a PR there. **Merge it first**; this plan does not merge anything.
- [ ] **Step 4: Run** the Step 1 check again — expect success and empty grep; `cargo test --workspace` — PASS.
- [ ] **Step 5: Commit** `deploy: conteneur d'inventaire, RabbitMQ à la place de NATS`
- [ ] **Step 6 (after the user approves merging):** deploy order is Infra PR → Moria PR; then verify `moria-inventory` logs a state every second and that the Grafana "Pont NATS" panel is 1. Update `moria-portainer-redeploy.md` in memory.
