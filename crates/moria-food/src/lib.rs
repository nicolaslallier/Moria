//! Service de nourriture : fait tourner `FoodSim` pour chaque montagne, au rythme des `food.tick`
//! du serveur, et lui propose ses evenements (`food.events`). Il ne parle jamais d'une montagne
//! dont il n'a pas recu d'instantane (`food.world`) : un etat vide ne propose rien.

use anyhow::Context;
use futures::{Stream, StreamExt};
use moria_bus::{Bus, Queue, redact_url};
use moria_core::MountainId;
use moria_core::food::{COW_EVERY, FoodSim, FoodStore};
use moria_core::food_msg::{
    Actions, Events, KEY_ACTION, KEY_EVENTS, KEY_REJECT, KEY_STATE, KEY_TICK, KEY_WORLD,
    MAX_FOOD_MSG_BYTES, MAX_FOOD_WORLD_BYTES, Rejects, Status, Tick, World, decode,
};
use serde::{Serialize, de::DeserializeOwned};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Battement de coeur `food.state`.
pub const STATE_EVERY: Duration = Duration::from_secs(1);
/// Sauvegarde periodique de `food.bin`.
pub const SAVE_EVERY: Duration = Duration::from_secs(60);

/// `World` ne porte pas la graine du monde : graine stable tiree du seul id de la montagne.
pub fn seed(m: MountainId) -> u64 {
    (m.0 as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// Instantane complet : adopte les montagnes listees, oublie les autres (le conteneur n'a aucun
/// moyen de supprimer quoi que ce soit de la carte).
pub fn handle_world(store: &mut FoodStore, w: &World) {
    store
        .mountains
        .retain(|m, _| w.mountains.iter().any(|(l, _)| l == m));
    for (m, cells) in &w.mountains {
        store
            .mountains
            .entry(*m)
            .or_insert_with(|| FoodSim::new(seed(*m)))
            .adopt(cells, w.tick);
    }
}

/// Evenements par message : un `FoodEvent` s'encode en 15 octets au plus, l'en-tete en 16,
/// donc sous `MAX_FOOD_MSG_BYTES` (le serveur jette un message trop gros en entier).
const EVENTS_PER_MSG: usize = 1000;

/// Des `Events` (decoupes) par montagne connue et par tick qui propose quelque chose.
/// `food.tick` ne garde que le dernier message : un saut depuis `last` rattrape les ticks des
/// `COW_EVERY` derniers seulement, soit au plus un pas de vaches et une verification de
/// croissance (les plus recents). Enchainer plusieurs pas a l'aveugle avant le retour des
/// refus ferait oublier au conteneur une vache bloquee ; un pas manque de temps en temps est
/// sans consequence. Un tick qui recule (serveur redemarre) n'avance que ce tick-la. Jamais le
/// tick d'un instantane adopte ni avant : il le contient deja, et un refus portant ce tick
/// vient d'une proposition anterieure (voir `handle_reject`).
pub fn handle_tick(store: &mut FoodStore, last: u64, t: &Tick) -> Vec<Events> {
    let from = if t.tick > last {
        last.max(t.tick.saturating_sub(COW_EVERY))
    } else {
        t.tick.saturating_sub(1)
    };
    let mut out = vec![];
    for (&mountain, sim) in store.mountains.iter_mut() {
        let start = from.max(sim.adopted_at()).saturating_add(1);
        for k in start..=t.tick {
            out.extend(sim.step(k).chunks(EVENTS_PER_MSG).map(|c| Events {
                tick: k,
                mountain,
                list: c.to_vec(),
            }));
        }
    }
    out
}

/// Montagne connue et message pas plus ancien que son dernier instantane (qui le contient deja).
fn current(store: &mut FoodStore, m: MountainId, tick: u64) -> Option<&mut FoodSim> {
    store
        .mountains
        .get_mut(&m)
        .filter(|sim| tick >= sim.adopted_at())
}

pub fn handle_action(store: &mut FoodStore, a: &Actions) {
    if let Some(sim) = current(store, a.mountain, a.tick) {
        a.list.iter().for_each(|x| sim.apply_action(x));
    }
}

/// Strictement apres l'instantane : un refus portant son tick vise une proposition faite avant
/// son adoption (aucun pas n'est fait a ce tick ensuite), deja effacee par l'adoption.
pub fn handle_reject(store: &mut FoodStore, r: &Rejects) {
    if let Some(sim) = current(store, r.mountain, r.tick).filter(|s| r.tick > s.adopted_at()) {
        sim.reject(&r.list);
    }
}

fn read<T: DeserializeOwned>(key: &str, bytes: &[u8], max: usize) -> Option<T> {
    let v = decode(bytes, max);
    if v.is_none() {
        tracing::warn!(
            "message {key} illisible ou trop gros, ignore ({} octets)",
            bytes.len()
        );
    }
    v
}

/// Battement : le dernier tick vu, 0 tant qu'aucun `World` n'a ete adopte depuis le demarrage.
pub fn status(last_tick: Option<u64>) -> Status {
    Status {
        tick: last_tick.unwrap_or(0),
    }
}

/// Decode (borne) et applique un message selon sa cle ; illisible ou inconnu : ignore.
/// `last_tick` vaut `None` tant qu'aucun `World` n'a ete adopte depuis le demarrage.
pub fn handle(
    store: &mut FoodStore,
    last_tick: &mut Option<u64>,
    key: &str,
    bytes: &[u8],
) -> Vec<Events> {
    match key {
        KEY_TICK => {
            // Avant le premier `World` : `food.bin` peut dater d'une minute, ne rien proposer.
            if let (Some(t), Some(last)) = (read::<Tick>(key, bytes, MAX_FOOD_MSG_BYTES), last_tick)
            {
                let prev = std::mem::replace(last, t.tick);
                return handle_tick(store, prev, &t);
            }
        }
        KEY_WORLD => {
            if let Some(w) = read::<World>(key, bytes, MAX_FOOD_WORLD_BYTES) {
                handle_world(store, &w);
                last_tick.get_or_insert(w.tick);
            }
        }
        KEY_ACTION => {
            if let Some(a) = read(key, bytes, MAX_FOOD_MSG_BYTES) {
                handle_action(store, &a);
            }
        }
        KEY_REJECT => {
            // Jamais de proposition apres le dernier tick : un refus plus recent vient d'un
            // serveur reparti d'une sauvegarde plus ancienne. (Les actions, elles, portent le
            // tick du serveur et peuvent devancer son `food.tick` sur une autre file.)
            if let Some(r) = read::<Rejects>(key, bytes, MAX_FOOD_MSG_BYTES)
                && last_tick.is_none_or(|l| r.tick <= l)
            {
                handle_reject(store, &r);
            }
        }
        _ => tracing::warn!("cle inconnue ignoree: {key}"),
    }
    vec![]
}

async fn publish<T: Serialize>(bus: &Bus, key: &str, msg: &T) {
    match postcard::to_allocvec(msg) {
        Ok(b) => {
            if let Err(e) = bus.publish(key, b, false).await {
                tracing::warn!("publication {key} impossible: {e:#}");
            }
        }
        Err(e) => tracing::error!("message {key} non serialisable: {e}"),
    }
}

/// Les quatre abonnements fusionnes ; `None` signale la fin d'un flux (connexion perdue).
async fn subscribe_all(
    bus: &Bus,
) -> anyhow::Result<impl Stream<Item = (&'static str, Option<Vec<u8>>)> + Unpin + use<>> {
    let mut subs = vec![];
    for (key, name, latest_only) in [
        (KEY_TICK, "food-tick", true),
        (KEY_WORLD, "food-world", true),
        (KEY_ACTION, "food-action", false),
        (KEY_REJECT, "food-reject", false),
    ] {
        let s = bus
            .subscribe(key, Queue::Shared { name, latest_only })
            .await?;
        subs.push(
            s.map(move |b| (key, Some(b)))
                .chain(futures::stream::once(std::future::ready((key, None)))),
        );
    }
    Ok(futures::stream::select_all(subs))
}

/// Etat cache en memoire, `food.bin` sauvegarde toutes les `SAVE_EVERY` (a cote de la boucle)
/// et a l'arret (avant la fermeture du broker).
pub async fn run(
    amqp_url: String,
    data: PathBuf,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    let mut store = FoodStore::load_or_default(&data).with_context(|| {
        format!(
            "nourriture illisible ({}), refus de demarrer",
            data.display()
        )
    })?;
    // Sonde d'ecriture : sans elle, l'etat cache serait perdu en silence a l'arret.
    store.save(&data).with_context(|| {
        format!(
            "ecriture impossible dans {}, refus de demarrer",
            data.display()
        )
    })?;
    tracing::info!(
        "nourriture chargee ({} montagnes), broker {}",
        store.mountains.len(),
        redact_url(&amqp_url)
    );
    tokio::pin!(shutdown);
    let mut beat = tokio::time::interval(STATE_EVERY);
    let mut autosave = tokio::time::interval(SAVE_EVERY);
    // Apres une longue attente (reconnexion), pas de rafale de battements ni de sauvegardes.
    beat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    autosave.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tick = None;
    // Sauvegarde periodique hors de la boucle (fsync) : un `food.tick` n'attend pas le disque.
    let mut saving: Option<tokio::task::JoinHandle<()>> = None;
    loop {
        let bus = tokio::select! {
            b = Bus::connect_forever(&amqp_url) => b,
            () = &mut shutdown => return save(&store, &data, &mut saving).await,
        };
        let mut stop = false;
        match subscribe_all(&bus).await {
            Err(e) => tracing::warn!("abonnement impossible: {e:#}"),
            Ok(mut inbox) => loop {
                tokio::select! {
                    () = &mut shutdown => {
                        stop = true;
                        break;
                    }
                    _ = beat.tick() => {
                        if !bus.is_connected() { break; }
                        publish(&bus, KEY_STATE, &status(last_tick)).await;
                    }
                    _ = autosave.tick() => {
                        // La precedente encore en cours : celle-ci attend le tour suivant.
                        if saving.as_ref().is_none_or(|h| h.is_finished()) {
                            let (st, path) = (store.clone(), data.clone());
                            saving = Some(tokio::task::spawn_blocking(move || {
                                if let Err(e) = st.save(&path) {
                                    tracing::error!("sauvegarde de la nourriture impossible: {e}");
                                }
                            }));
                        }
                    }
                    m = inbox.next() => {
                        let Some((key, Some(body))) = m else { break };
                        for ev in handle(&mut store, &mut last_tick, key, &body) {
                            publish(&bus, KEY_EVENTS, &ev).await;
                        }
                    }
                }
            },
        }
        if stop {
            // Sauvegarde d'abord : la fermeture du broker ne doit pas pouvoir la retarder.
            let r = save(&store, &data, &mut saving).await;
            let _ = tokio::time::timeout(Duration::from_secs(2), bus.close()).await;
            return r;
        }
        tokio::select! {
            () = bus.close() => {}
            () = &mut shutdown => return save(&store, &data, &mut saving).await,
        }
        tracing::warn!("flux interrompu, reconnexion");
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            () = &mut shutdown => return save(&store, &data, &mut saving).await,
        }
    }
}

/// Sauvegarde finale, synchrone, apres la periodique en cours (meme fichier temporaire).
async fn save(
    store: &FoodStore,
    data: &Path,
    saving: &mut Option<tokio::task::JoinHandle<()>>,
) -> anyhow::Result<()> {
    if let Some(h) = saving.take() {
        let _ = h.await;
    }
    store.save(data).context("sauvegarde finale impossible")
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::food::{Cell, CellKind, FoodAction, FoodEvent};
    use moria_core::food_msg::{
        KEY_ACTION, KEY_REJECT, KEY_TICK, KEY_WORLD, MAX_FOOD_MSG_BYTES, MAX_FOOD_WORLD_BYTES,
    };
    use moria_core::{MountainId, TilePos};
    use std::time::Duration;

    const A: MountainId = MountainId(1);
    const B: MountainId = MountainId(2);

    fn p(x: i16, y: i16) -> TilePos {
        TilePos { x, y, z: 0 }
    }
    fn cow(x: i16, y: i16) -> Cell {
        Cell {
            pos: p(x, y),
            kind: CellKind::Cow,
        }
    }
    fn field(x: i16, y: i16) -> Cell {
        Cell {
            pos: p(x, y),
            kind: CellKind::Field,
        }
    }
    fn world(tick: u64, mountains: Vec<(MountainId, Vec<Cell>)>) -> World {
        World { tick, mountains }
    }

    #[test]
    fn tick_steps_every_known_mountain() {
        let mut s = FoodStore::default();
        assert!(
            handle_tick(&mut s, 9, &Tick { tick: 10 }).is_empty(),
            "vide"
        );
        handle_world(
            &mut s,
            &world(0, vec![(A, vec![cow(5, 5)]), (B, vec![cow(9, 9)])]),
        );
        let evs = handle_tick(&mut s, 9, &Tick { tick: 10 });
        let ms: Vec<_> = evs.iter().map(|e| e.mountain).collect();
        assert_eq!(ms, vec![A, B]);
        assert!(evs.iter().all(|e| e.tick == 10 && !e.list.is_empty()));
        // Pas de pas de vache hors des multiples de COW_EVERY : rien a publier.
        assert!(handle_tick(&mut s, 10, &Tick { tick: 11 }).is_empty());
    }

    #[test]
    fn world_is_a_complete_snapshot() {
        let summer = moria_core::calendar::SEASON_TICKS;
        let fields = vec![field(1, 1), field(4, 1), cow(7, 7)];
        let mut s = FoodStore::default();
        // etat cache : l'age de la vache date de sa premiere adoption
        handle_world(
            &mut s,
            &world(summer, vec![(A, fields.clone()), (B, vec![cow(9, 9)])]),
        );
        let mut kept = s.mountains[&A].clone();
        kept.adopt(&fields, summer + 200);
        let mut fresh = FoodSim::new(seed(A));
        fresh.adopt(&fields, summer + 200);
        assert_ne!(kept, fresh, "aucun etat cache a garder");

        handle_world(&mut s, &world(summer + 200, vec![(A, fields)]));
        assert!(!s.mountains.contains_key(&B), "montagne absente gardee");
        assert_eq!(s.mountains[&A], kept);
    }

    #[test]
    fn unknown_mountain_in_an_action_is_ignored() {
        let mut s = FoodStore::default();
        handle_action(
            &mut s,
            &Actions {
                tick: 5,
                mountain: A,
                list: vec![FoodAction::FieldSown(p(1, 1))],
            },
        );
        handle_reject(
            &mut s,
            &Rejects {
                tick: 5,
                mountain: A,
                list: vec![FoodEvent::CowBorn { at: p(1, 1) }],
            },
        );
        assert_eq!(s, FoodStore::default());
        assert!(handle_tick(&mut s, 99, &Tick { tick: 100 }).is_empty());
    }

    #[test]
    fn stale_action_older_than_the_last_snapshot_is_ignored() {
        let mut s = FoodStore::default();
        handle_world(&mut s, &world(100, vec![(A, vec![field(1, 1)])]));
        let removed = |tick| Actions {
            tick,
            mountain: A,
            list: vec![FoodAction::FieldRemoved(p(1, 1))],
        };
        handle_action(&mut s, &removed(99));
        assert_eq!(s.mountains[&A].field_count(), 1, "action perimee appliquee");
        handle_action(&mut s, &removed(100));
        assert_eq!(s.mountains[&A].field_count(), 0);
    }

    #[test]
    fn stale_reject_older_than_the_last_snapshot_is_ignored() {
        let mut s = FoodStore::default();
        handle_world(&mut s, &world(100, vec![(A, vec![cow(1, 1)])]));
        let born = |tick| Rejects {
            tick,
            mountain: A,
            list: vec![FoodEvent::CowBorn { at: p(1, 1) }],
        };
        handle_reject(&mut s, &born(99));
        assert!(s.mountains[&A].has_cow(p(1, 1)), "refus perime applique");
        // Stamped with the snapshot's own tick: proposed before it was adopted (the
        // container never steps that tick once it holds the snapshot).
        handle_reject(&mut s, &born(100));
        assert!(
            s.mountains[&A].has_cow(p(1, 1)),
            "refus d'avant l'instantane"
        );
        handle_reject(&mut s, &born(101));
        assert!(!s.mountains[&A].has_cow(p(1, 1)));
    }

    fn crop(x: i16, y: i16) -> Cell {
        Cell {
            pos: p(x, y),
            kind: CellKind::Crop,
        }
    }
    fn enc_tick(t: u64) -> Vec<u8> {
        postcard::to_allocvec(&Tick { tick: t }).unwrap()
    }
    fn enc_world(w: &World) -> Vec<u8> {
        postcard::to_allocvec(w).unwrap()
    }
    fn stamps(evs: &[Events]) -> Vec<u64> {
        let mut v: Vec<u64> = evs.iter().map(|e| e.tick).collect();
        v.dedup();
        v
    }

    #[test]
    fn the_snapshot_tick_is_never_stepped() {
        let mut s = FoodStore::default();
        let mut last = None;
        let w = world(100, vec![(A, vec![cow(5, 5), crop(1, 1)])]);
        handle(&mut s, &mut last, KEY_WORLD, &enc_world(&w));
        assert!(handle(&mut s, &mut last, KEY_TICK, &enc_tick(100)).is_empty());
        assert_eq!(
            stamps(&handle(&mut s, &mut last, KEY_TICK, &enc_tick(110))),
            [110]
        );
        // a later snapshot of a tick already stepped: nothing is stepped up to it again
        handle(
            &mut s,
            &mut last,
            KEY_WORLD,
            &enc_world(&world(110, w.mountains)),
        );
        assert!(handle(&mut s, &mut last, KEY_TICK, &enc_tick(110)).is_empty());
    }

    #[test]
    fn coalesced_ticks_are_caught_up() {
        let mut s = FoodStore::default();
        let mut last = None;
        let w = world(0, vec![(A, vec![cow(5, 5), crop(1, 1)])]);
        handle(&mut s, &mut last, KEY_WORLD, &enc_world(&w));
        handle(&mut s, &mut last, KEY_TICK, &enc_tick(95));
        // 100 (cows and growth) was skipped by the latest-only queue
        let evs = handle(&mut s, &mut last, KEY_TICK, &enc_tick(105));
        assert_eq!(stamps(&evs), [100]);
        assert!(evs[0].list.contains(&FoodEvent::CropRipe { at: p(1, 1) }));
        assert!(
            evs[0]
                .list
                .iter()
                .any(|e| !matches!(e, FoodEvent::CropRipe { .. }))
        );
    }

    #[test]
    fn a_reject_past_the_last_tick_is_ignored() {
        // The container never proposes past its last tick: such a reject comes from a server
        // restarted from an older save, about a proposal it never saw.
        let mut s = FoodStore::default();
        let mut last = None;
        let w = world(100, vec![(A, vec![cow(1, 1), cow(3, 3), field(5, 5)])]);
        handle(&mut s, &mut last, KEY_WORLD, &enc_world(&w));
        handle(&mut s, &mut last, KEY_TICK, &enc_tick(105));
        let born = |tick, x| {
            postcard::to_allocvec(&Rejects {
                tick,
                mountain: A,
                list: vec![FoodEvent::CowBorn { at: p(x, x) }],
            })
            .unwrap()
        };
        handle(&mut s, &mut last, KEY_REJECT, &born(106, 1));
        assert!(
            s.mountains[&A].has_cow(p(1, 1)),
            "reject from the future applied"
        );
        handle(&mut s, &mut last, KEY_REJECT, &born(105, 3));
        assert!(!s.mountains[&A].has_cow(p(3, 3)));
        // Actions are stamped by the server and may outrun its Tick (another queue): kept.
        let a = Actions {
            tick: 106,
            mountain: A,
            list: vec![FoodAction::FieldRemoved(p(5, 5))],
        };
        handle(
            &mut s,
            &mut last,
            KEY_ACTION,
            &postcard::to_allocvec(&a).unwrap(),
        );
        assert_eq!(s.mountains[&A].field_count(), 0);
    }

    #[test]
    fn a_big_gap_catches_up_one_cow_step_only() {
        // Chained blind steps before any reject returns would make the container forget a
        // penned cow: one step per mountain, the newest, never older than COW_EVERY.
        use moria_core::food::COW_EVERY;
        let mut s = FoodStore::default();
        let mut last = None;
        let w = world(
            0,
            vec![(A, vec![cow(5, 5), crop(1, 1)]), (B, vec![cow(9, 9)])],
        );
        handle(&mut s, &mut last, KEY_WORLD, &enc_world(&w));
        handle(&mut s, &mut last, KEY_TICK, &enc_tick(95));
        let evs = handle(&mut s, &mut last, KEY_TICK, &enc_tick(160));
        let per: Vec<_> = evs.iter().map(|e| (e.mountain, e.tick)).collect();
        assert_eq!(per, [(A, 160), (B, 160)]);
        assert!(evs.iter().all(|e| e.tick + COW_EVERY > 160));
        // far gap: the newest step, nothing older
        let evs = handle(&mut s, &mut last, KEY_TICK, &enc_tick(100_005));
        assert_eq!(stamps(&evs), [100_000]);
        // going back (the server restarted from an older save): that tick only
        let evs = handle(&mut s, &mut last, KEY_TICK, &enc_tick(50_000));
        assert_eq!(stamps(&evs), [50_000]);
    }

    fn seeded() -> FoodStore {
        let mut s = FoodStore::default();
        handle_world(&mut s, &world(0, vec![(A, vec![cow(5, 5), field(1, 1)])]));
        s
    }

    #[test]
    fn garbage_payload_is_ignored() {
        for key in [KEY_TICK, KEY_WORLD, KEY_ACTION, KEY_REJECT, "food.inconnu"] {
            let mut s = seeded();
            let mut last = Some(0);
            assert!(
                handle(&mut s, &mut last, key, &[0xff; 7]).is_empty(),
                "{key}"
            );
            assert_eq!(s, seeded(), "{key}");
            assert_eq!(last, Some(0), "{key}");
        }
    }

    #[test]
    fn oversized_payload_is_ignored() {
        let pad = |v: Vec<u8>, max: usize| {
            let mut v = v;
            // des octets en trop se decodent quand meme : seule la borne refuse
            v.resize(max + 1, 0);
            v
        };
        let w = world(0, vec![(A, vec![cow(5, 5)])]);
        let big = pad(postcard::to_allocvec(&w).unwrap(), MAX_FOOD_WORLD_BYTES);
        assert!(postcard::from_bytes::<World>(&big).is_ok());
        let mut s = FoodStore::default();
        let mut last = None;
        handle(&mut s, &mut last, KEY_WORLD, &big);
        assert_eq!(s, FoodStore::default());

        let mut s = seeded();
        let a = Actions {
            tick: 0,
            mountain: A,
            list: vec![FoodAction::FieldRemoved(p(1, 1))],
        };
        let big = pad(postcard::to_allocvec(&a).unwrap(), MAX_FOOD_MSG_BYTES);
        handle(&mut s, &mut last, KEY_ACTION, &big);
        assert_eq!(s, seeded());

        let big = pad(
            postcard::to_allocvec(&Tick { tick: 10 }).unwrap(),
            MAX_FOOD_MSG_BYTES,
        );
        assert!(handle(&mut s, &mut last, KEY_TICK, &big).is_empty());
        assert_eq!(last, None);
    }

    #[test]
    fn valid_payloads_are_dispatched_by_key() {
        let mut s = FoodStore::default();
        let mut last = None;
        let w = world(0, vec![(A, vec![cow(5, 5)])]);
        handle(
            &mut s,
            &mut last,
            KEY_WORLD,
            &postcard::to_allocvec(&w).unwrap(),
        );
        let t = postcard::to_allocvec(&Tick { tick: 10 }).unwrap();
        assert_eq!(handle(&mut s, &mut last, KEY_TICK, &t).len(), 1);
        assert_eq!(last, Some(10));
    }

    #[test]
    fn events_are_split_under_the_message_limit() {
        // Pire cas : des milliers de recoltes mures loin de l'origine, redemandees a chaque
        // verification, plus des vaches qui se deplacent (World forge).
        let far = |i: i16| TilePos::new(i16::MIN + i, i16::MAX - i, u8::MAX);
        let mut cells: Vec<Cell> = (0..6000)
            .map(|i| Cell {
                pos: far(i),
                kind: CellKind::Crop,
            })
            .collect();
        cells.extend((0..3000).map(|i| Cell {
            pos: TilePos::new(i16::MAX - 1 - 2 * i, i16::MIN + 1 + 2 * i, u8::MAX),
            kind: CellKind::Cow,
        }));
        let m = MountainId(u16::MAX);
        let mut s = FoodStore::default();
        handle_world(&mut s, &world(0, vec![(m, cells)]));
        let tick = 100 * u64::from(u32::MAX); // multiple de COW_EVERY et GROW_EVERY
        let expected = s.mountains[&m].clone().step(tick);
        assert!(expected.len() > 6000);
        let msgs = handle_tick(&mut s, tick - 1, &Tick { tick });
        assert!(msgs.len() > 1);
        for e in &msgs {
            let b = postcard::to_allocvec(e).unwrap();
            assert!(b.len() <= MAX_FOOD_MSG_BYTES, "{} octets", b.len());
            assert_eq!(decode::<Events>(&b, MAX_FOOD_MSG_BYTES).as_ref(), Some(e));
            assert_eq!((e.tick, e.mountain), (tick, m));
        }
        let all: Vec<_> = msgs.into_iter().flat_map(|e| e.list).collect();
        assert_eq!(all, expected, "evenement perdu");
    }

    #[test]
    fn no_step_before_the_first_world_since_start() {
        // `food.bin` charge (montagne connue) mais aucun `World` recu depuis le demarrage.
        let mut s = seeded();
        let mut last = None;
        let enc = |t| postcard::to_allocvec(&Tick { tick: t }).unwrap();
        assert!(handle(&mut s, &mut last, KEY_TICK, &enc(10)).is_empty());
        assert_eq!(s, seeded(), "etat perime avance");
        assert_eq!(
            status(last),
            Status { tick: 0 },
            "le serveur doit renvoyer un World"
        );
        let w = world(20, vec![(A, vec![cow(5, 5)])]);
        handle(
            &mut s,
            &mut last,
            KEY_WORLD,
            &postcard::to_allocvec(&w).unwrap(),
        );
        assert_eq!(status(last), Status { tick: 20 });
        assert_eq!(handle(&mut s, &mut last, KEY_TICK, &enc(30)).len(), 1);
        assert_eq!(status(last), Status { tick: 30 });
    }

    fn bad_path() -> PathBuf {
        std::env::temp_dir()
            .join("moria-food-absent-dir")
            .join("food.bin")
    }

    #[tokio::test]
    async fn refuses_to_start_when_data_dir_is_missing() {
        // Echoue avant toute connexion : l'URL n'est jamais jointe.
        let r = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                "amqp://x:y@127.0.0.1:1/".into(),
                bad_path(),
                std::future::pending(),
            ),
        )
        .await
        .expect("run aurait du refuser de demarrer");
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn refuses_to_start_on_corrupt_file() {
        let p = std::env::temp_dir().join(format!(
            "moria-food-unit-{}-corrupt.bin",
            std::process::id()
        ));
        std::fs::write(&p, [0xffu8; 9]).unwrap();
        let r = tokio::time::timeout(
            Duration::from_secs(3),
            run(
                "amqp://x:y@127.0.0.1:1/".into(),
                p.clone(),
                std::future::pending(),
            ),
        )
        .await
        .expect("run aurait du refuser de demarrer");
        assert!(r.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), [0xffu8; 9], "fichier ecrase");
    }
}
