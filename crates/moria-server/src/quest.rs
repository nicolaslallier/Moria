//! Règles des quêtes du roi : pures, appelées par `sim` (entre `begin_tick` et `end_tick`,
//! pour que la récompense parte dans le lot normal de l'inventaire).

use moria_core::TilePos;
use moria_core::king::{Quest, QuestId, king_in_reach, quest_def};
use moria_core::world::Fortress;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestError {
    /// Une quête est déjà en cours.
    Active,
    /// Quête déjà terminée : chacune ne se fait qu'une fois par montagne.
    Done,
    NotOffered,
}

/// Les quêtes du catalogue pas encore terminées.
pub fn offered(f: &Fortress) -> Vec<QuestId> {
    QuestId::ALL
        .into_iter()
        .filter(|id| !f.done.contains(id))
        .collect()
}

/// Valeur suivie par la quête : production brute, profondeur max ou stock de nourriture.
fn measure(f: &Fortress, id: QuestId) -> u32 {
    match id {
        QuestId::ForgeIngots => f.made.ingots,
        QuestId::ForgeDoors => f.made.doors,
        QuestId::Food => f.resources.food,
        QuestId::Depth => f.max_depth,
    }
}

pub fn accept(f: &mut Fortress, id: QuestId, offered: &[QuestId]) -> Result<(), QuestError> {
    if f.quest.is_some() {
        return Err(QuestError::Active);
    }
    if f.done.contains(&id) {
        return Err(QuestError::Done);
    }
    if !offered.contains(&id) {
        return Err(QuestError::NotOffered);
    }
    // Seules les quêtes de forge soustraient la baseline ; celle de `Depth` est notée pour
    // information, celle de `Food` est inutilisée.
    let baseline = match id {
        QuestId::ForgeIngots | QuestId::ForgeDoors | QuestId::Depth => measure(f, id),
        QuestId::Food => 0,
    };
    f.quest = Some(Quest { id, baseline });
    Ok(())
}

/// `(avancement, cible)` de la quête active, l'avancement plafonné à la cible.
pub fn progress(f: &Fortress) -> Option<(u32, u32)> {
    let q = f.quest?;
    let now = measure(f, q.id);
    let done = match q.id {
        QuestId::ForgeIngots | QuestId::ForgeDoors => now.saturating_sub(q.baseline),
        // Profondeur absolue (« z ≥ Z ») : le monde n'a que quelques niveaux sous le sol.
        QuestId::Food | QuestId::Depth => now,
    };
    let target = quest_def(q.id).target;
    Some((done.min(target), target))
}

/// Verse la récompense, range la quête dans `done` et libère la place.
fn finish(f: &mut Fortress, id: QuestId) {
    f.resources.add(quest_def(id).reward);
    f.done.push(id);
    f.quest = None;
}

/// Validation au tick ; la nourriture ne se valide que par `turn_in`.
pub fn complete_if_ready(f: &mut Fortress) -> bool {
    match (f.quest, progress(f)) {
        (Some(q), Some((n, target))) if q.id != QuestId::Food && n >= target => {
            finish(f, q.id);
            true
        }
        _ => false,
    }
}

/// Remise de la nourriture au roi : retire la cible du stock puis verse la récompense.
pub fn turn_in(f: &mut Fortress, hero_pos: TilePos) -> bool {
    let target = quest_def(QuestId::Food).target;
    let ready = f.quest.is_some_and(|q| q.id == QuestId::Food)
        && king_in_reach(f.king, hero_pos)
        && f.resources.food >= target;
    if ready {
        f.resources.food -= target;
        finish(f, QuestId::Food);
    }
    ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::king::{QuestId, quest_def};
    use moria_core::world::{Fortress, claim_mountain, generate};
    use moria_core::{PlayerId, Resources, TilePos};

    fn fortress() -> Fortress {
        let mut w = generate(1);
        let m = claim_mountain(&mut w, PlayerId(1)).unwrap();
        let mut f = w.fortresses.remove(&m).unwrap();
        f.resources = Resources::default();
        f
    }
    fn far(f: &Fortress) -> TilePos {
        TilePos::new(f.king.x + 2, f.king.y, f.king.z)
    }

    #[test]
    fn accept_refuses_a_second_active_quest() {
        let mut f = fortress();
        let o = offered(&f);
        assert_eq!(accept(&mut f, QuestId::Food, &o), Ok(()));
        assert_eq!(accept(&mut f, QuestId::Depth, &o), Err(QuestError::Active));
        assert_eq!(f.quest.map(|q| q.id), Some(QuestId::Food));
    }

    #[test]
    fn accept_refuses_a_finished_quest() {
        let mut f = fortress();
        f.done.push(QuestId::Depth);
        let all = QuestId::ALL;
        assert_eq!(accept(&mut f, QuestId::Depth, &all), Err(QuestError::Done));
        assert!(f.quest.is_none());
    }

    #[test]
    fn accept_refuses_a_quest_not_offered() {
        let mut f = fortress();
        assert_eq!(
            accept(&mut f, QuestId::Food, &[QuestId::Depth]),
            Err(QuestError::NotOffered)
        );
        assert!(f.quest.is_none());
    }

    #[test]
    fn baseline_is_the_production_counter_at_acceptance() {
        let mut f = fortress();
        f.made.ingots = 7;
        let o = offered(&f);
        accept(&mut f, QuestId::ForgeIngots, &o).unwrap();
        assert_eq!(f.quest.unwrap().baseline, 7);
        assert_eq!(progress(&f), Some((0, 5)));
        f.made.ingots = 9;
        assert_eq!(progress(&f), Some((2, 5)));
        assert!(!complete_if_ready(&mut f));
    }

    #[test]
    fn food_turn_in_needs_reach_and_stock() {
        let mut f = fortress();
        let king = f.king;
        f.resources.food = 100;
        // Pas de quête de nourriture active.
        assert!(!turn_in(&mut f, king));
        let o = offered(&f);
        accept(&mut f, QuestId::Food, &o).unwrap();
        let far = far(&f);
        assert!(!turn_in(&mut f, far), "hors de portée");
        f.resources.food = 19;
        assert!(!turn_in(&mut f, king), "stock insuffisant");
        assert_eq!(f.resources.food, 19);
        // La nourriture ne se valide jamais toute seule au tick.
        f.resources.food = 20;
        assert!(!complete_if_ready(&mut f));
        assert!(turn_in(&mut f, king));
    }

    #[test]
    fn food_turn_in_removes_the_food_and_pays_once() {
        let mut f = fortress();
        let king = f.king;
        let o = offered(&f);
        accept(&mut f, QuestId::Food, &o).unwrap();
        f.resources.food = 25;
        assert!(turn_in(&mut f, king));
        let mut want = Resources {
            food: 5,
            ..Default::default()
        };
        want.add(quest_def(QuestId::Food).reward);
        assert_eq!(f.resources, want);
        assert!(f.quest.is_none());
        assert_eq!(f.done, vec![QuestId::Food]);
        f.resources.food = 25;
        want.food = 25;
        assert!(!turn_in(&mut f, king));
        assert_eq!(f.resources, want);
    }

    #[test]
    fn completed_quest_is_not_offered_again() {
        let mut f = fortress();
        assert_eq!(offered(&f), QuestId::ALL.to_vec());
        let o = offered(&f);
        accept(&mut f, QuestId::ForgeDoors, &o).unwrap();
        f.made.doors = 2;
        assert!(complete_if_ready(&mut f));
        assert!(!offered(&f).contains(&QuestId::ForgeDoors));
        assert_eq!(offered(&f).len(), 3);
        let o = offered(&f);
        assert_eq!(
            accept(&mut f, QuestId::ForgeDoors, &o),
            Err(QuestError::Done)
        );
    }
}
