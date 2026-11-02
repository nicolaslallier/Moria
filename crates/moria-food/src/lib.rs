//! Service de nourriture : fait tourner `FoodSim` pour chaque montagne, au rythme des `food.tick`
//! du serveur, et lui propose ses evenements (`food.events`). Il ne parle jamais d'une montagne
//! dont il n'a pas recu d'instantane (`food.world`) : un etat vide ne propose rien.

use anyhow::Context;
use futures::{Stream, StreamExt};
use moria_bus::{Bus, Queue, redact_url};
use moria_core::MountainId;
use moria_core::food::{FoodSim, FoodStore};
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
fn seed(m: MountainId) -> u64 {
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

/// Un `Events` par montagne connue qui propose quelque chose.
pub fn handle_tick(store: &mut FoodStore, t: &Tick) -> Vec<Events> {
    store
        .mountains
        .iter_mut()
        .filter_map(|(&mountain, sim)| {
            let list = sim.step(t.tick);
            (!list.is_empty()).then_some(Events {
                tick: t.tick,
                mountain,
                list,
            })
        })
        .collect()
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

pub fn handle_reject(store: &mut FoodStore, r: &Rejects) {
    if let Some(sim) = current(store, r.mountain, r.tick) {
        r.list.iter().for_each(|x| sim.reject(x));
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

/// Decode (borne) et applique un message selon sa cle ; illisible ou inconnu : ignore.
pub fn handle(store: &mut FoodStore, last_tick: &mut u64, key: &str, bytes: &[u8]) -> Vec<Events> {
    match key {
        KEY_TICK => {
            if let Some(t) = read::<Tick>(key, bytes, MAX_FOOD_MSG_BYTES) {
                *last_tick = t.tick;
                return handle_tick(store, &t);
            }
        }
        KEY_WORLD => {
            if let Some(w) = read(key, bytes, MAX_FOOD_WORLD_BYTES) {
                handle_world(store, &w);
            }
        }
        KEY_ACTION => {
            if let Some(a) = read(key, bytes, MAX_FOOD_MSG_BYTES) {
                handle_action(store, &a);
            }
        }
        KEY_REJECT => {
            if let Some(r) = read(key, bytes, MAX_FOOD_MSG_BYTES) {
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

/// Etat cache en memoire, `food.bin` sauvegarde toutes les `SAVE_EVERY` et a l'arret.
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
    let mut last_tick = 0;
    loop {
        let bus = tokio::select! {
            b = Bus::connect_forever(&amqp_url) => b,
            () = &mut shutdown => return save(&store, &data),
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
                        publish(&bus, KEY_STATE, &Status { tick: last_tick }).await;
                    }
                    _ = autosave.tick() => {
                        if let Err(e) = store.save(&data) {
                            tracing::error!("sauvegarde de la nourriture impossible: {e}");
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
            let r = save(&store, &data);
            let _ = tokio::time::timeout(Duration::from_secs(2), bus.close()).await;
            return r;
        }
        tokio::select! {
            () = bus.close() => {}
            () = &mut shutdown => return save(&store, &data),
        }
        tracing::warn!("flux interrompu, reconnexion");
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            () = &mut shutdown => return save(&store, &data),
        }
    }
}

fn save(store: &FoodStore, data: &Path) -> anyhow::Result<()> {
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
        assert!(handle_tick(&mut s, &Tick { tick: 10 }).is_empty(), "vide");
        handle_world(
            &mut s,
            &world(0, vec![(A, vec![cow(5, 5)]), (B, vec![cow(9, 9)])]),
        );
        let evs = handle_tick(&mut s, &Tick { tick: 10 });
        let ms: Vec<_> = evs.iter().map(|e| e.mountain).collect();
        assert_eq!(ms, vec![A, B]);
        assert!(evs.iter().all(|e| e.tick == 10 && !e.list.is_empty()));
        // Pas de pas de vache hors des multiples de COW_EVERY : rien a publier.
        assert!(handle_tick(&mut s, &Tick { tick: 11 }).is_empty());
    }

    #[test]
    fn world_is_a_complete_snapshot() {
        let summer = moria_core::calendar::SEASON_TICKS;
        let fields = vec![field(1, 1), field(4, 1)];
        let mut s = FoodStore::default();
        handle_world(
            &mut s,
            &world(summer, vec![(A, fields.clone()), (B, vec![cow(9, 9)])]),
        );
        // etat cache : une verification de croissance fait avancer les champs
        handle_tick(&mut s, &Tick { tick: summer + 100 });
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
        assert!(handle_tick(&mut s, &Tick { tick: 100 }).is_empty());
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
        handle_reject(&mut s, &born(100));
        assert!(!s.mountains[&A].has_cow(p(1, 1)));
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
            let mut last = 0;
            assert!(
                handle(&mut s, &mut last, key, &[0xff; 7]).is_empty(),
                "{key}"
            );
            assert_eq!(s, seeded(), "{key}");
            assert_eq!(last, 0, "{key}");
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
        let mut last = 0;
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
        assert_eq!(last, 0);
    }

    #[test]
    fn valid_payloads_are_dispatched_by_key() {
        let mut s = FoodStore::default();
        let mut last = 0;
        let w = world(0, vec![(A, vec![cow(5, 5)])]);
        handle(
            &mut s,
            &mut last,
            KEY_WORLD,
            &postcard::to_allocvec(&w).unwrap(),
        );
        let t = postcard::to_allocvec(&Tick { tick: 10 }).unwrap();
        assert_eq!(handle(&mut s, &mut last, KEY_TICK, &t).len(), 1);
        assert_eq!(last, 10);
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
