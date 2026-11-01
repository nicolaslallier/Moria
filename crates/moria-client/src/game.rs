//! Client-side model, free of Bevy systems: session flow, local fortress copy,
//! prediction and input rules. The Bevy systems in the other modules only call this.

use crate::light::{Explored, SEEN_AT};
use crate::net::NetEvent;
use bevy::prelude::States;
use moria_core::GROUND_Z;
use moria_core::light::{Area, BODY_LIGHT, DayPhase, LightMap, hour, phase};
use moria_core::protocol::{
    ClientMsg, DwarfJob, DwarfState, FortressView, HeroView, Reject, ServerMsg, TickDelta,
};
use moria_core::srd::{Abilities, modifier};
use moria_core::{DesignationKind, Dir, Mountain, Resources, TilePos};
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::path::PathBuf;

/// Two server ticks of 100 ms, plus a little slack for network jitter.
// ponytail: fixed margin; measure real RTT jitter if moves still get dropped server-side.
pub const MOVE_COOLDOWN: f64 = 0.22;
/// Ticks after which an unacknowledged predicted move is considered dropped. Bound: the
/// round trip, plus up to 2 ticks of server cooldown buffering the move, plus margin;
/// 10 ticks (~1 s) keeps good predictions on links up to ~700 ms RTT.
const PENDING_TICKS: u64 = 10;

/// Bounds on an untrusted snapshot, above the real 176×96×12.
fn valid_view(v: &FortressView) -> bool {
    let (w, h, d) = v.mountain.size();
    v.mountain.is_consistent()
        && (1..=256).contains(&w)
        && (1..=256).contains(&h)
        && (1..=64).contains(&d)
        && v.mountain.in_bounds(v.hero.pos)
        && v.dwarves.len() <= 1024
        && v.dwarves.iter().all(|(_, p, _)| v.mountain.in_bounds(*p))
}

#[derive(States, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Screen {
    #[default]
    Login,
    CreateHero,
    Playing,
    Disconnected,
}

pub fn reject_fr(r: &Reject) -> String {
    match r {
        Reject::VersionMismatch { server } => {
            format!("Version différente du serveur (serveur v{server}) — mettez le client à jour")
        }
        Reject::BadPassword => {
            "Pseudo ou mot de passe incorrect — première fois ? utilisez « Créer un compte »".into()
        }
        Reject::ServerFull => "Serveur plein (10 joueurs)".into(),
        Reject::NoFreeMountain => "Plus de montagne libre".into(),
        Reject::InvalidName => "Nom invalide (3–16 caractères : lettres, chiffres, _ ou -)".into(),
        Reject::BadInvite => "Code d'invitation invalide".into(),
        Reject::WeakPassword => "Mot de passe trop court (8 caractères minimum)".into(),
    }
}

/// Rectangle dragged between two tiles of level `z`; `kind: None` cancels.
pub fn drag_msg(
    from: (i16, i16),
    to: (i16, i16),
    z: u8,
    kind: Option<DesignationKind>,
) -> ClientMsg {
    let a = TilePos::new(from.0.min(to.0), from.1.min(to.1), z);
    let b = TilePos::new(from.0.max(to.0), from.1.max(to.1), z);
    match kind {
        Some(kind) => ClientMsg::Designate { a, b, kind },
        None => ClientMsg::CancelDesignation { a, b },
    }
}

/// HUD clock: the game hour and the phase of the day.
pub fn clock_label(tick: u64) -> String {
    let name = match phase(tick) {
        DayPhase::Day => "Jour",
        DayPhase::Dusk => "Crépuscule",
        DayPhase::Night => "Nuit",
        DayPhase::Dawn => "Aube",
    };
    format!("{} h — {name}", hour(tick))
}

/// Level name for the HUD: 0 is the ground, positive above it (z grows downward).
pub fn level_label(z: u8) -> String {
    match z.cmp(&GROUND_Z) {
        Ordering::Equal => "Niveau 0 (sol)".into(),
        Ordering::Less => format!("Niveau +{}", GROUND_Z - z),
        Ordering::Greater => format!("Niveau -{}", z - GROUND_Z),
    }
}

/// "1 pierre", "3 bois".
fn cost_fr(r: Resources) -> String {
    [(r.stone, "pierre"), (r.wood, "bois"), (r.gold, "or")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Build-menu row: "Lit en bois (3 bois)".
pub fn build_text(k: DesignationKind, label: &str) -> String {
    format!("{label} ({})", cost_fr(k.cost()))
}

/// HUD line for the current mode.
pub fn mode_label(k: DesignationKind) -> String {
    match k {
        DesignationKind::Dig => "Mode : Creuser".into(),
        DesignationKind::Stairs => "Mode : Escalier".into(),
        DesignationKind::Chop => "Mode : Couper (arbres)".into(),
        _ => BUILDS
            .iter()
            .find(|b| b.0 == k)
            .map_or_else(String::new, |&(k, l)| {
                format!("Mode : {}", build_text(k, l))
            }),
    }
}

/// Status-page activity of a dwarf; `None` is idle.
pub fn job_label(job: Option<DwarfJob>) -> String {
    use DesignationKind::*;
    let Some(DwarfJob { kind, working }) = job else {
        return "Inactif".into();
    };
    let what = match kind {
        Dig => "Creuse".into(),
        Stairs => "Taille un escalier".into(),
        Chop => "Coupe du bois".into(),
        Wall | Bed | Table | Chair | WoodBed | WoodTable | WoodChair | Candle | Torch | Lamp
        | Campfire | Hearth | Stove => {
            let label = BUILDS.iter().find(|b| b.0 == kind).map_or("?", |b| b.1);
            format!("Construit : {label}")
        }
    };
    if working {
        what
    } else {
        format!("En route : {what}")
    }
}

/// Status-page rows: ability name, score, signed modifier.
pub fn ability_rows(a: Abilities) -> [(&'static str, u8, String); 6] {
    [
        ("Force", a.str),
        ("Dextérité", a.dex),
        ("Constitution", a.con),
        ("Intelligence", a.int),
        ("Sagesse", a.wis),
        ("Charisme", a.cha),
    ]
    .map(|(n, s)| (n, s, format!("{:+}", modifier(s))))
}

pub struct Game {
    pub mountain: Mountain,
    pub hero: HeroView,
    pub dwarves: Vec<DwarfState>,
    pub resources: Resources,
    /// World tick, drives the day/night cycle.
    pub tick: u64,
    /// Tiles the hero has seen lit.
    pub explored: Explored,
    /// Files `explored` was loaded from and is saved to; `None` until loaded.
    pub memory: Option<Vec<PathBuf>>,
    /// Bumped (wrapping) by every tick that changes tiles; a cache key for the light map.
    pub map_rev: u32,
    /// Status page (key C) open.
    pub status: bool,
    /// Last horizontal direction pressed; Space digs that way.
    pub last_dir: Dir,
    pub mode: DesignationKind,
    /// Build menu (key 3) open.
    pub build_menu: bool,
    /// Highlighted row of the build menu, for keyboard and gamepad.
    pub build_sel: usize,
    /// `Some` while the player browses another level than the hero's.
    pub browse_z: Option<u8>,
    last_move: f64,
    /// Last hero position reported by the server.
    pub confirmed: TilePos,
    /// Predicted positions not yet seen in a tick, with the local tick count at send.
    pub pending: VecDeque<(TilePos, u64)>,
    ticks: u64,
}

/// Everything the player can build: mode and label; the cost is `kind.cost()`.
pub const BUILDS: &[(DesignationKind, &str)] = &[
    (DesignationKind::Wall, "Mur"),
    (DesignationKind::Bed, "Lit"),
    (DesignationKind::Table, "Table"),
    (DesignationKind::Chair, "Chaise"),
    (DesignationKind::WoodBed, "Lit en bois"),
    (DesignationKind::WoodTable, "Table en bois"),
    (DesignationKind::WoodChair, "Chaise en bois"),
    (DesignationKind::Candle, "Chandelle"),
    (DesignationKind::Torch, "Torche"),
    (DesignationKind::Lamp, "Lampe"),
    (DesignationKind::Campfire, "Feu de bois"),
    (DesignationKind::Hearth, "Foyer"),
    (DesignationKind::Stove, "Poêle"),
];

/// Bottom-bar buttons with their key; `None` is the build menu.
pub const ACTIONS: &[(&str, Option<DesignationKind>)] = &[
    ("Creuser [1]", Some(DesignationKind::Dig)),
    ("Escalier [2]", Some(DesignationKind::Stairs)),
    ("Construire [3]", None),
    ("Couper [4]", Some(DesignationKind::Chop)),
];

impl Game {
    /// A bar button or its key: picks the mode (closing the menu), or toggles the build menu.
    pub fn pick_action(&mut self, a: Option<DesignationKind>) {
        match a {
            Some(m) => {
                self.mode = m;
                self.build_menu = false;
            }
            None => {
                self.build_menu = !self.build_menu;
                self.build_sel = 0;
            }
        }
    }

    /// Whether the bar highlights `a`: its mode is current, or for "Construire"
    /// the menu is open or a build mode is current.
    pub fn action_active(&self, a: Option<DesignationKind>) -> bool {
        match a {
            Some(m) => !self.build_menu && self.mode == m,
            None => self.build_menu || BUILDS.iter().any(|b| b.0 == self.mode),
        }
    }

    /// Moves the build-menu highlight by one row, wrapping.
    pub fn build_step(&mut self, down: bool) {
        let n = BUILDS.len();
        self.build_sel = (self.build_sel + if down { 1 } else { n - 1 }) % n;
    }

    /// Picks build `i` if the resources cover it: sets the mode and closes the menu.
    pub fn pick_build(&mut self, i: usize) {
        if let Some(&(kind, _)) = BUILDS.get(i)
            && self.resources.covers(&kind.cost())
        {
            self.mode = kind;
            self.build_menu = false;
        }
    }

    pub fn new(v: FortressView) -> Self {
        let (w, h, d) = v.mountain.size();
        Self {
            confirmed: v.hero.pos,
            mountain: v.mountain,
            hero: v.hero,
            dwarves: v.dwarves,
            resources: v.resources,
            tick: v.tick,
            explored: Explored::new(w, h, d),
            memory: None,
            map_rev: 0,
            status: false,
            last_dir: Dir::E,
            mode: DesignationKind::Dig,
            build_menu: false,
            build_sel: 0,
            browse_z: None,
            last_move: f64::NEG_INFINITY,
            pending: VecDeque::new(),
            ticks: 0,
        }
    }

    /// Status-page row per dwarf: name, level, activity.
    pub fn dwarf_rows(&self) -> Vec<(String, String, String)> {
        self.dwarves
            .iter()
            .map(|&(id, p, job)| (format!("Nain n°{}", id.0), level_label(p.z), job_label(job)))
            .collect()
    }

    /// Light sources on level `z` as `(x, y, radius)`: the hero and every dwarf.
    pub fn light_sources(&self, z: u8) -> Vec<(i16, i16, u8)> {
        let hero = std::iter::once(self.hero.pos);
        let dwarves = self.dwarves.iter().map(|d| d.1);
        hero.chain(dwarves)
            .filter(|p| p.z == z)
            .map(|p| (p.x, p.y, BODY_LIGHT))
            .collect()
    }

    /// Marks as explored every tile of `area` on level `z` lit at least `SEEN_AT`.
    pub fn mark_visible(&mut self, z: u8, area: Area, light: &LightMap) {
        for y in area.y0..=area.y1 {
            for x in area.x0..=area.x1 {
                if light.get(x, y) >= SEEN_AT {
                    self.explored.mark(TilePos::new(x, y, z));
                }
            }
        }
    }

    pub fn current_z(&self) -> u8 {
        self.browse_z.unwrap_or(self.hero.pos.z)
    }

    /// Server state wins for the map, dwarves and resources. The hero is reconciled: a
    /// tick that predates our pending moves keeps the prediction, an ack drains it, and a
    /// divergent or long-unacked position snaps to the server's.
    pub fn apply_tick(&mut self, d: &TickDelta) {
        for &(p, t) in &d.tiles {
            self.mountain.set(p, t);
        }
        if !d.tiles.is_empty() {
            self.map_rev = self.map_rev.wrapping_add(1);
        }
        // Out-of-bounds entries are ignored so a hostile server cannot grow the map.
        for p in &d.designations_removed {
            if self.mountain.in_bounds(*p) {
                self.mountain.designations.remove(p);
            }
        }
        for &(p, k) in &d.designations_added {
            if self.mountain.in_bounds(p) {
                self.mountain.designations.insert(p, k);
            }
        }
        self.dwarves.clear();
        let m = &self.mountain;
        self.dwarves
            .extend(d.dwarves.iter().filter(|(_, p, _)| m.in_bounds(*p)));
        self.resources = d.resources;
        self.tick = d.tick;
        self.ticks += 1;
        let s = d.hero_pos;
        if !self.mountain.in_bounds(s) {
            return;
        }
        if s == self.confirmed {
            // Stale: the server has not applied our moves yet. Checked first so pacing
            // A→B→A is not mistaken for an ack of the final A.
            if self
                .pending
                .front()
                .is_some_and(|&(_, t)| self.ticks - t > PENDING_TICKS)
            {
                self.pending.clear();
            }
        } else if let Some(i) = self.pending.iter().position(|&(p, _)| p == s) {
            self.pending.drain(..=i);
        } else {
            self.pending.clear();
        }
        self.confirmed = s;
        self.hero.pos = self.pending.back().map_or(s, |p| p.0);
    }

    /// Predicted move at time `now` (seconds): applied locally and returned for sending
    /// only if the local map allows it and the cooldown has elapsed.
    pub fn try_move(&mut self, dir: Dir, now: f64) -> Option<ClientMsg> {
        if Dir::HORIZONTAL.contains(&dir) {
            self.last_dir = dir;
        }
        if now - self.last_move < MOVE_COOLDOWN {
            return None;
        }
        let to = self.mountain.move_dest(self.hero.pos, dir)?;
        self.last_move = now;
        // Stairs or a slope: the view follows the hero to its new level.
        if to.z != self.hero.pos.z {
            self.browse_z = None;
        }
        self.hero.pos = to;
        self.pending.push_back((self.hero.pos, self.ticks));
        Some(ClientMsg::Move(dir))
    }

    /// `<` (Up) / `>` (Down): take the stairs when the hero can, otherwise browse levels.
    pub fn vertical(&mut self, dir: Dir, now: f64) -> Option<ClientMsg> {
        if self.mountain.can_move(self.hero.pos, dir) {
            return self.try_move(dir, now);
        }
        let depth = self.mountain.size().2;
        let z = self.current_z();
        let z = match dir {
            Dir::Up => z.saturating_sub(1),
            _ => z.saturating_add(1).min(depth.saturating_sub(1)),
        };
        self.browse_z = (z != self.hero.pos.z).then_some(z);
        None
    }

    /// Hero action toward the last move direction: in a build mode build on floor or grass
    /// (given the resources), else dig a solid tile or furniture, or chop a tree.
    pub fn dig(&self) -> Option<ClientMsg> {
        let target = self.hero.pos.step(self.last_dir);
        let m = &self.mountain;
        if self.mode.is_build()
            && m.diggable(target, self.mode)
            && self.resources.covers(&self.mode.cost())
        {
            Some(ClientMsg::Build(self.last_dir, self.mode))
        } else {
            (m.diggable(target, DesignationKind::Dig) || m.diggable(target, DesignationKind::Chop))
                .then_some(ClientMsg::Dig(self.last_dir))
        }
    }
}

/// Designations to outline on level `z`, lit or not: the player plans tunnels through unseen rock.
pub fn outlined(
    designations: &std::collections::BTreeMap<TilePos, DesignationKind>,
    z: u8,
) -> Vec<(TilePos, DesignationKind)> {
    designations
        .iter()
        .filter(|(p, _)| p.z == z)
        .map(|(&p, &k)| (p, k))
        .collect()
}

/// What the client shows, driven by network events.
#[derive(Default)]
pub struct Session {
    pub screen: Screen,
    /// Login error or disconnect reason, in French.
    pub error: Option<String>,
    pub game: Option<Game>,
    /// Bumped on every accepted snapshot so the renderer rebuilds the map.
    pub snapshots: u32,
    /// Games dropped by a disconnect or a new mountain, queued until each is saved to its own files.
    pub left: Vec<Game>,
}

impl Session {
    /// Returns true when the connection is over and the handle should be dropped.
    pub fn handle(&mut self, ev: NetEvent) -> bool {
        match ev {
            NetEvent::Msg(ServerMsg::Welcome { needs_hero, .. }) => {
                self.error = None;
                if needs_hero {
                    self.screen = Screen::CreateHero;
                }
                false
            }
            NetEvent::Msg(ServerMsg::Reject(r)) => {
                self.end(Screen::Login, reject_fr(&r));
                true
            }
            NetEvent::Msg(ServerMsg::Snapshot(v)) if !valid_view(&v) => {
                self.end(
                    Screen::Disconnected,
                    "Données invalides reçues du serveur".into(),
                );
                true
            }
            NetEvent::Msg(ServerMsg::Snapshot(v)) => {
                self.snapshots += 1;
                let mut g = Game::new(*v);
                // A new snapshot of the same mountain keeps what the hero has explored.
                if let Some(old) = self.game.take() {
                    if old.mountain.id == g.mountain.id && old.mountain.size() == g.mountain.size()
                    {
                        g.explored = old.explored;
                        g.memory = old.memory;
                    } else {
                        self.left.push(old);
                    }
                }
                self.game = Some(g);
                self.screen = Screen::Playing;
                false
            }
            NetEvent::Msg(ServerMsg::Tick(d)) => {
                if let Some(g) = &mut self.game {
                    g.apply_tick(&d);
                }
                false
            }
            NetEvent::Disconnected(reason) => {
                // Before login, the reason shows on the login screen itself.
                let to = match self.screen {
                    Screen::Login => Screen::Login,
                    _ => Screen::Disconnected,
                };
                self.end(to, reason);
                true
            }
        }
    }

    fn end(&mut self, screen: Screen, msg: String) {
        self.screen = screen;
        self.error = Some(msg);
        self.left.extend(self.game.take());
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use moria_core::srd::{Abilities, Class, Race, roll_abilities};
    use moria_core::{DwarfId, MountainId, Tile};

    fn p(x: i16, y: i16, z: u8) -> TilePos {
        TilePos::new(x, y, z)
    }

    /// 6×6×3 rock; floor at (1..=3, 1, 0); stairs at (1,1,0)-(1,1,1).
    pub(crate) fn game() -> Game {
        let mut m = Mountain::new(MountainId(0), 6, 6, 3, Tile::Rock);
        for x in 1..=3 {
            m.set(p(x, 1, 0), Tile::Floor);
        }
        m.set(p(1, 1, 0), Tile::Stair);
        m.set(p(1, 1, 1), Tile::Stair);
        game_on(m, p(2, 1, 0))
    }

    #[test]
    fn only_lit_tiles_become_explored() {
        let mut g = game();
        let area = Area {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 5,
        };
        // Night on z = 1 (underground): only the source at (2, 1) lights anything.
        let light = moria_core::light::light_map(&g.mountain, 1, area, &[(2, 1, 4)], 20_000);
        g.mark_visible(1, area, &light);
        assert!(g.explored.seen(p(2, 1, 1)));
        assert!(!g.explored.seen(p(5, 5, 1)));
        assert!(!g.explored.seen(p(2, 1, 0)));
    }

    #[test]
    fn clock_names_the_phase() {
        assert_eq!(clock_label(0), "6 h — Jour");
        assert_eq!(clock_label(16_500), "17 h — Crépuscule");
        assert_eq!(clock_label(27_000), "0 h — Nuit");
        assert_eq!(clock_label(34_500), "5 h — Aube");
    }

    #[test]
    fn hero_lights_only_his_own_level() {
        let mut g = game(); // hero on z = 0
        g.dwarves = vec![(DwarfId(1), p(2, 1, 1), None)];
        assert_eq!(g.light_sources(0), vec![(2, 1, BODY_LIGHT)]);
        assert_eq!(g.light_sources(1), vec![(2, 1, BODY_LIGHT)]); // the dwarf only
        g.dwarves.clear();
        assert!(g.light_sources(1).is_empty());
    }

    fn game_on(mountain: Mountain, pos: TilePos) -> Game {
        Game::new(FortressView {
            mountain,
            hero: HeroView {
                name: "Gimli".into(),
                race: Race::Dwarf,
                class: Class::Fighter,
                abilities: roll_abilities(Race::Dwarf, Class::Fighter),
                pos,
            },
            dwarves: vec![],
            resources: Resources::default(),
            tick: 100,
        })
    }

    fn tick(hero_pos: TilePos) -> TickDelta {
        TickDelta {
            tick: 1,
            hero_pos,
            dwarves: vec![],
            tiles: vec![],
            designations_added: vec![],
            designations_removed: vec![],
            resources: Resources::default(),
        }
    }

    #[test]
    fn reject_messages_in_french() {
        let cases = [
            (
                Reject::VersionMismatch { server: 3 },
                "Version différente du serveur (serveur v3) — mettez le client à jour",
            ),
            (
                Reject::BadPassword,
                "Pseudo ou mot de passe incorrect — première fois ? utilisez « Créer un compte »",
            ),
            (Reject::ServerFull, "Serveur plein (10 joueurs)"),
            (Reject::NoFreeMountain, "Plus de montagne libre"),
            (
                Reject::InvalidName,
                "Nom invalide (3–16 caractères : lettres, chiffres, _ ou -)",
            ),
            (Reject::BadInvite, "Code d'invitation invalide"),
            (
                Reject::WeakPassword,
                "Mot de passe trop court (8 caractères minimum)",
            ),
        ];
        for (r, want) in cases {
            assert_eq!(reject_fr(&r), want);
        }
    }

    #[test]
    fn prediction_applies_valid_move_and_server_wins() {
        let mut g = game();
        assert_eq!(g.try_move(Dir::E, 10.0), Some(ClientMsg::Move(Dir::E)));
        assert_eq!(g.hero.pos, p(3, 1, 0));
        // A position neither confirmed nor predicted: the server wins.
        g.apply_tick(&tick(p(1, 1, 0)));
        assert_eq!(g.hero.pos, p(1, 1, 0));
    }

    #[test]
    fn stale_tick_keeps_prediction() {
        let mut g = game();
        g.try_move(Dir::E, 10.0).unwrap();
        // The server has not seen the move yet: it still reports the old position.
        g.apply_tick(&tick(p(2, 1, 0)));
        assert_eq!(g.hero.pos, p(3, 1, 0));
        // The next prediction starts from the predicted position.
        assert_eq!(g.try_move(Dir::E, 11.0), None); // (4,1) is rock
        assert_eq!(g.try_move(Dir::W, 12.0), Some(ClientMsg::Move(Dir::W)));
        assert_eq!(g.hero.pos, p(2, 1, 0));
    }

    #[test]
    fn ack_tick_drains_pending() {
        let mut g = game();
        g.try_move(Dir::E, 10.0).unwrap();
        g.try_move(Dir::W, 11.0).unwrap();
        g.apply_tick(&tick(p(3, 1, 0)));
        assert_eq!(g.hero.pos, p(2, 1, 0)); // the W move is still pending
        g.apply_tick(&tick(p(2, 1, 0)));
        assert_eq!(g.hero.pos, p(2, 1, 0));
        assert!(g.pending.is_empty());
        assert_eq!(g.confirmed, p(2, 1, 0));
    }

    #[test]
    fn pacing_back_and_forth_does_not_rubber_band() {
        let mut g = game();
        let a = p(2, 1, 0);
        g.try_move(Dir::E, 10.0).unwrap();
        g.try_move(Dir::W, 11.0).unwrap();
        assert_eq!(g.hero.pos, a);
        for s in [a, p(3, 1, 0), a] {
            g.apply_tick(&tick(s));
            assert_eq!(g.hero.pos, a, "after tick at {s:?}");
        }
        assert!(g.pending.is_empty());
    }

    #[test]
    fn divergent_tick_snaps_and_clears() {
        let mut g = game();
        g.try_move(Dir::E, 10.0).unwrap();
        g.apply_tick(&tick(p(1, 1, 0)));
        assert_eq!(g.hero.pos, p(1, 1, 0));
        assert!(g.pending.is_empty());
    }

    #[test]
    fn old_pending_expires() {
        let mut g = game();
        g.try_move(Dir::E, 10.0).unwrap();
        for _ in 0..PENDING_TICKS {
            g.apply_tick(&tick(p(2, 1, 0)));
            assert_eq!(g.hero.pos, p(3, 1, 0));
        }
        // The move was dropped server-side: give up after PENDING_TICKS (~1 s).
        g.apply_tick(&tick(p(2, 1, 0)));
        assert_eq!(g.hero.pos, p(2, 1, 0));
        assert!(g.pending.is_empty());
    }

    #[test]
    fn slow_link_keeps_prediction() {
        let mut g = game();
        g.try_move(Dir::E, 10.0).unwrap();
        // ~800 ms before the server's tick shows the move: the hero never snaps back.
        for _ in 0..7 {
            g.apply_tick(&tick(p(2, 1, 0)));
            assert_eq!(g.hero.pos, p(3, 1, 0));
        }
        g.apply_tick(&tick(p(3, 1, 0)));
        assert_eq!(g.hero.pos, p(3, 1, 0));
        assert!(g.pending.is_empty());
    }

    #[test]
    fn tick_ignores_out_of_bounds_positions() {
        let mut g = game();
        let mut d = tick(p(99, 1, 0));
        d.dwarves = vec![
            (DwarfId(1), p(3, 1, 0), None),
            (DwarfId(2), p(-1, 0, 0), None),
        ];
        d.designations_added = vec![(p(-5, 0, 0), DesignationKind::Dig)];
        g.mountain
            .designations
            .insert(p(100, 0, 0), DesignationKind::Dig);
        d.designations_removed = vec![p(100, 0, 0)];
        g.apply_tick(&d);
        assert_eq!(g.hero.pos, p(2, 1, 0));
        assert_eq!(g.dwarves, vec![(DwarfId(1), p(3, 1, 0), None)]);
        assert!(!g.mountain.designations.contains_key(&p(-5, 0, 0)));
        // Removes are ignored too: nothing out of bounds is touched.
        assert!(g.mountain.designations.contains_key(&p(100, 0, 0)));
    }

    #[test]
    fn invalid_snapshot_disconnects() {
        let view = |m: Mountain| {
            let g = game();
            NetEvent::Msg(ServerMsg::Snapshot(Box::new(FortressView {
                mountain: m,
                hero: g.hero,
                dwarves: vec![],
                resources: Resources::default(),
                tick: 0,
            })))
        };
        // Tile vec not matching the dimensions (only possible via deserialization).
        let mut bytes = moria_core::protocol::encode(&game().mountain);
        assert_eq!(bytes[5], 12); // w = 6, zigzag varint, after the 4-byte length and id
        bytes[5] = 14;
        let bad: Mountain = moria_core::protocol::read_frame(&mut &bytes[..]).unwrap();
        assert!(!bad.is_consistent());
        let mut hero_outside = Mountain::new(MountainId(0), 2, 2, 1, Tile::Floor);
        hero_outside.set(p(0, 0, 0), Tile::Rock);
        for m in [
            bad,
            Mountain::new(MountainId(0), 6, 6, 0, Tile::Rock),
            Mountain::new(MountainId(0), 300, 6, 3, Tile::Rock),
            Mountain::new(MountainId(0), 6, 6, 65, Tile::Rock),
            hero_outside,
        ] {
            let mut s = Session {
                screen: Screen::CreateHero,
                ..Default::default()
            };
            assert!(s.handle(view(m)));
            assert_eq!(s.screen, Screen::Disconnected);
            assert_eq!(
                s.error.as_deref(),
                Some("Données invalides reçues du serveur")
            );
            assert!(s.game.is_none());
        }
        let mut s = Session::default();
        let NetEvent::Msg(ServerMsg::Snapshot(mut v)) = view(game().mountain) else {
            unreachable!()
        };
        v.dwarves = vec![(DwarfId(0), p(2, 1, 0), None); 1025];
        assert!(s.handle(NetEvent::Msg(ServerMsg::Snapshot(v.clone()))));
        assert_eq!(s.screen, Screen::Disconnected);
        let mut s = Session::default();
        v.dwarves = vec![(DwarfId(0), p(6, 1, 0), None)];
        assert!(s.handle(NetEvent::Msg(ServerMsg::Snapshot(v))));
        assert_eq!(s.screen, Screen::Disconnected);
    }

    #[test]
    fn a_pending_left_game_survives_a_later_disconnect() {
        let snap = |id: u16| {
            let g = game();
            let mut m = g.mountain;
            m.id = MountainId(id);
            NetEvent::Msg(ServerMsg::Snapshot(Box::new(FortressView {
                mountain: m,
                hero: g.hero,
                dwarves: vec![],
                resources: Resources::default(),
                tick: 0,
            })))
        };
        let mut s = Session::default();
        s.handle(snap(1));
        s.handle(snap(2)); // old mountain 1 is now pending in `left`
        s.handle(NetEvent::Disconnected("perdu".into()));
        let ids: Vec<_> = s.left.iter().map(|g| g.mountain.id).collect();
        assert_eq!(ids, [MountainId(1), MountainId(2)]); // both queued, none lost
    }

    #[test]
    fn designations_are_outlined_on_their_level_even_in_the_dark() {
        let mut d = std::collections::BTreeMap::new();
        d.insert(p(5, 5, 0), DesignationKind::Dig); // never-seen rock
        d.insert(p(1, 1, 1), DesignationKind::Dig);
        assert_eq!(outlined(&d, 0), vec![(p(5, 5, 0), DesignationKind::Dig)]);
    }

    #[test]
    fn prediction_respects_cooldown() {
        let mut g = game();
        assert!(g.try_move(Dir::E, 10.0).is_some());
        assert_eq!(g.try_move(Dir::W, 10.1), None);
        assert_eq!(g.hero.pos, p(3, 1, 0));
        assert!(g.try_move(Dir::W, 10.0 + MOVE_COOLDOWN).is_some());
        assert_eq!(g.hero.pos, p(2, 1, 0));
    }

    #[test]
    fn prediction_refuses_rock() {
        let mut g = game();
        assert_eq!(g.try_move(Dir::N, 10.0), None);
        assert_eq!(g.hero.pos, p(2, 1, 0));
        // Facing the wall still aims the dig.
        assert_eq!(g.last_dir, Dir::N);
        assert_eq!(g.dig(), Some(ClientMsg::Dig(Dir::N)));
    }

    #[test]
    fn dig_only_toward_solid() {
        let mut g = game();
        g.last_dir = Dir::E;
        assert_eq!(g.dig(), None); // (3,1) is floor
        // In Wall mode the hero builds on floor, given stone.
        g.mode = DesignationKind::Wall;
        assert_eq!(g.dig(), None);
        g.resources.stone = 1;
        assert_eq!(
            g.dig(),
            Some(ClientMsg::Build(Dir::E, DesignationKind::Wall))
        );
        g.last_dir = Dir::N; // rock: Wall mode still digs
        assert_eq!(g.dig(), Some(ClientMsg::Dig(Dir::N)));
        // Furniture is built the same way, and dug back from any mode but a build on floor.
        g.mode = DesignationKind::Bed;
        g.last_dir = Dir::E;
        assert_eq!(
            g.dig(),
            Some(ClientMsg::Build(Dir::E, DesignationKind::Bed))
        );
        g.mountain
            .set(g.hero.pos.step(Dir::E), moria_core::Tile::Bed);
        assert_eq!(g.dig(), Some(ClientMsg::Dig(Dir::E)));
    }

    #[test]
    fn build_menu_pick_needs_stone() {
        let mut g = game();
        g.build_menu = true;
        g.build_step(true);
        g.build_step(false);
        assert_eq!(g.build_sel, 0); // wraps, stays in range
        g.pick_build(g.build_sel);
        assert!(g.build_menu); // no stone: stays open
        g.resources.stone = 1;
        g.pick_build(g.build_sel);
        assert!(!g.build_menu);
        assert_eq!(g.mode, DesignationKind::Wall);
    }

    #[test]
    fn drag_rect_to_designate_msg() {
        assert_eq!(
            drag_msg((2, 3), (5, 1), 4, Some(DesignationKind::Dig)),
            ClientMsg::Designate {
                a: p(2, 1, 4),
                b: p(5, 3, 4),
                kind: DesignationKind::Dig
            }
        );
        assert_eq!(
            drag_msg((5, 1), (2, 3), 4, None),
            ClientMsg::CancelDesignation {
                a: p(2, 1, 4),
                b: p(5, 3, 4)
            }
        );
    }

    #[test]
    fn game_follows_the_world_tick() {
        let mut g = game();
        assert_eq!(g.tick, 100);
        let mut d = tick(g.hero.pos);
        d.tick = 4_242;
        g.apply_tick(&d);
        assert_eq!(g.tick, 4_242);
    }

    #[test]
    fn apply_tick_updates_mountain() {
        let mut g = game();
        g.mountain
            .designations
            .insert(p(0, 0, 0), DesignationKind::Dig);
        let mut d = tick(p(3, 1, 0));
        d.tiles = vec![(p(4, 1, 0), Tile::Floor)];
        d.designations_added = vec![(p(5, 1, 0), DesignationKind::Stairs)];
        d.designations_removed = vec![p(0, 0, 0)];
        d.dwarves = vec![(DwarfId(1), p(3, 1, 0), None)];
        d.resources.stone = 4;
        g.apply_tick(&d);
        assert_eq!(g.map_rev, 1);
        assert_eq!(g.mountain.get(p(4, 1, 0)), Some(Tile::Floor));
        assert_eq!(
            g.mountain.designations.iter().collect::<Vec<_>>(),
            vec![(&p(5, 1, 0), &DesignationKind::Stairs)]
        );
        assert_eq!(g.dwarves, vec![(DwarfId(1), p(3, 1, 0), None)]);
        assert_eq!(g.resources.stone, 4);
        assert_eq!(g.hero.pos, p(3, 1, 0));
    }

    #[test]
    fn level_browsing_and_stairs() {
        let mut g = game();
        // Not on stairs: '>' browses down, clamped to the deepest level.
        assert_eq!(g.vertical(Dir::Down, 10.0), None);
        assert_eq!(g.current_z(), 1);
        g.vertical(Dir::Down, 10.0);
        g.vertical(Dir::Down, 10.0);
        assert_eq!(g.current_z(), 2);
        // Back to the hero's level stops browsing; '<' clamps at the surface.
        g.vertical(Dir::Up, 10.0);
        g.vertical(Dir::Up, 10.0);
        assert_eq!(g.browse_z, None);
        g.vertical(Dir::Up, 10.0);
        assert_eq!(g.current_z(), 0);
        // On stairs: '>' takes them and the view follows the hero.
        g.browse_z = None;
        assert!(g.try_move(Dir::W, 10.0).is_some());
        g.browse_z = Some(2);
        assert_eq!(
            g.vertical(Dir::Down, 11.0),
            Some(ClientMsg::Move(Dir::Down))
        );
        assert_eq!(g.hero.pos, p(1, 1, 1));
        assert_eq!(g.current_z(), 1);
    }

    #[test]
    fn tick_keeps_dwarf_jobs_for_the_status_page() {
        let mut g = game();
        let mut d = tick(p(2, 1, 0));
        let job = |kind, working| Some(DwarfJob { kind, working });
        d.dwarves = vec![
            (DwarfId(1), p(3, 1, 0), None),
            (DwarfId(2), p(1, 1, 0), job(DesignationKind::Chop, true)),
            (DwarfId(3), p(1, 1, 1), job(DesignationKind::Dig, false)),
        ];
        g.apply_tick(&d);
        assert_eq!(
            g.dwarf_rows(),
            vec![
                ("Nain n°1".into(), level_label(0), "Inactif".into()),
                ("Nain n°2".into(), level_label(0), "Coupe du bois".into()),
                (
                    "Nain n°3".into(),
                    level_label(1),
                    "En route : Creuse".into()
                ),
            ]
        );
    }

    #[test]
    fn job_labels() {
        let at = |kind| {
            Some(DwarfJob {
                kind,
                working: true,
            })
        };
        assert_eq!(job_label(None), "Inactif");
        assert_eq!(job_label(at(DesignationKind::Dig)), "Creuse");
        assert_eq!(job_label(at(DesignationKind::Stairs)), "Taille un escalier");
        assert_eq!(
            job_label(at(DesignationKind::WoodBed)),
            "Construit : Lit en bois"
        );
        let walking = Some(DwarfJob {
            kind: DesignationKind::Wall,
            working: false,
        });
        assert_eq!(job_label(walking), "En route : Construit : Mur");
    }

    #[test]
    fn ability_rows_show_modifiers() {
        let a = roll_abilities(Race::Dwarf, Class::Fighter);
        let rows = ability_rows(a);
        assert_eq!(
            rows[0],
            (
                "Force",
                a.str,
                format!("{:+}", moria_core::srd::modifier(a.str))
            )
        );
        assert_eq!(
            rows.map(|r| r.0),
            [
                "Force",
                "Dextérité",
                "Constitution",
                "Intelligence",
                "Sagesse",
                "Charisme"
            ]
        );
        assert_eq!(ability_rows(Abilities { str: 8, ..a })[0].2, "-1");
        assert_eq!(ability_rows(Abilities { str: 15, ..a })[0].2, "+2");
    }

    #[test]
    fn session_flow() {
        let mut s = Session::default();
        let welcome = ServerMsg::Welcome {
            player: moria_core::PlayerId(1),
            needs_hero: true,
        };
        assert!(!s.handle(NetEvent::Msg(welcome)));
        assert_eq!(s.screen, Screen::CreateHero);
        let g = game();
        let view = FortressView {
            mountain: g.mountain,
            hero: g.hero,
            dwarves: vec![],
            resources: Resources::default(),
            tick: 0,
        };
        s.handle(NetEvent::Msg(ServerMsg::Snapshot(Box::new(view))));
        assert_eq!(s.screen, Screen::Playing);
        s.handle(NetEvent::Msg(ServerMsg::Tick(tick(p(3, 1, 0)))));
        assert_eq!(s.game.as_ref().unwrap().hero.pos, p(3, 1, 0));
        assert!(s.handle(NetEvent::Disconnected("perdu".into())));
        assert_eq!(s.screen, Screen::Disconnected);
        assert_eq!(s.error.as_deref(), Some("perdu"));
        assert!(s.game.is_none());

        let mut s = Session::default();
        assert!(s.handle(NetEvent::Msg(ServerMsg::Reject(Reject::BadInvite))));
        assert_eq!(s.screen, Screen::Login);
        assert_eq!(s.error.as_deref(), Some("Code d'invitation invalide"));
    }

    #[test]
    fn level_labels() {
        use moria_core::GROUND_Z;
        assert_eq!(level_label(GROUND_Z), "Niveau 0 (sol)");
        assert_eq!(level_label(GROUND_Z - 2), "Niveau +2");
        assert_eq!(level_label(GROUND_Z + 3), "Niveau -3");
    }

    #[test]
    fn prediction_climbs_slope() {
        // 4×1×3: sky at z 0, rock below; grass (0,0,1), slope (1,0,1), hilltop (2,0,0).
        let mut m = Mountain::new(MountainId(0), 4, 1, 3, Tile::Rock);
        for x in 0..4 {
            m.set(p(x, 0, 0), Tile::Sky);
        }
        m.set(p(0, 0, 1), Tile::Grass);
        m.set(p(1, 0, 1), Tile::Slope);
        m.set(p(2, 0, 0), Tile::Grass);
        let mut g = game_on(m, p(0, 0, 1));
        assert_eq!(g.try_move(Dir::E, 10.0), Some(ClientMsg::Move(Dir::E)));
        assert_eq!(g.hero.pos, p(1, 0, 1));
        // Browsing elsewhere: climbing the hill brings the view back to the hero.
        g.browse_z = Some(2);
        assert_eq!(g.try_move(Dir::E, 11.0), Some(ClientMsg::Move(Dir::E)));
        assert_eq!(g.hero.pos, p(2, 0, 0));
        assert_eq!(g.current_z(), 0);
        // And down again.
        g.browse_z = Some(2);
        assert_eq!(g.try_move(Dir::W, 12.0), Some(ClientMsg::Move(Dir::W)));
        assert_eq!(g.hero.pos, p(1, 0, 1));
        assert_eq!(g.current_z(), 1);
    }

    #[test]
    fn chop_mode_designates_chop() {
        let mut g = game();
        g.mode = g.mode.toggled().toggled();
        assert_eq!(g.mode, DesignationKind::Chop);
        assert_eq!(mode_label(g.mode), "Mode : Couper (arbres)");
        assert_eq!(
            drag_msg((3, 1), (1, 1), g.current_z(), Some(g.mode)),
            ClientMsg::Designate {
                a: p(1, 1, 0),
                b: p(3, 1, 0),
                kind: DesignationKind::Chop
            }
        );
        // Space toward a tree chops it (the server turns a hero Dig on a tree into Chop).
        g.mountain.set(p(3, 1, 0), Tile::Tree);
        g.last_dir = Dir::E;
        assert_eq!(g.dig(), Some(ClientMsg::Dig(Dir::E)));
    }

    #[test]
    fn action_bar_picks_modes() {
        let mut g = game();
        let labels: Vec<&str> = ACTIONS.iter().map(|a| a.0).collect();
        assert_eq!(
            labels,
            [
                "Creuser [1]",
                "Escalier [2]",
                "Construire [3]",
                "Couper [4]"
            ]
        );
        g.pick_action(Some(DesignationKind::Chop));
        assert_eq!(g.mode, DesignationKind::Chop);
        assert!(g.action_active(Some(DesignationKind::Chop)));
        assert!(!g.action_active(Some(DesignationKind::Dig)));
        assert!(!g.action_active(None));
        // Construire toggles the menu; a build mode keeps it highlighted once closed.
        g.pick_action(None);
        assert!(g.build_menu && g.action_active(None));
        g.pick_action(None);
        assert!(!g.build_menu);
        g.mode = DesignationKind::Wall;
        assert!(g.action_active(None));
        // Picking a mode closes the menu.
        g.build_menu = true;
        g.pick_action(Some(DesignationKind::Dig));
        assert!(!g.build_menu && g.mode == DesignationKind::Dig);
    }

    #[test]
    fn lights_are_in_the_build_menu() {
        for (k, l) in [
            (DesignationKind::Torch, "Torche"),
            (DesignationKind::Stove, "Poêle"),
        ] {
            assert!(BUILDS.contains(&(k, l)));
        }
        let mut g = game();
        g.resources.gold = 0;
        g.resources.stone = 5;
        let lamp = BUILDS
            .iter()
            .position(|b| b.0 == DesignationKind::Lamp)
            .unwrap();
        g.pick_build(lamp);
        assert_ne!(g.mode, DesignationKind::Lamp); // no gold, no lamp
    }

    #[test]
    fn build_menu_lists_wood() {
        let rows: Vec<String> = BUILDS.iter().map(|&(k, l)| build_text(k, l)).collect();
        assert_eq!(
            rows,
            [
                "Mur (1 pierre)",
                "Lit (1 pierre)",
                "Table (1 pierre)",
                "Chaise (1 pierre)",
                "Lit en bois (3 bois)",
                "Table en bois (4 bois)",
                "Chaise en bois (2 bois)",
                "Chandelle (1 bois)",
                "Torche (1 bois)",
                "Lampe (1 pierre + 1 or)",
                "Feu de bois (3 bois)",
                "Foyer (4 pierre)",
                "Poêle (3 pierre + 2 bois)",
            ]
        );
        assert_eq!(
            mode_label(DesignationKind::WoodBed),
            "Mode : Lit en bois (3 bois)"
        );
        // Wood builds need wood, not stone.
        let mut g = game();
        g.build_menu = true;
        g.resources.stone = 9;
        g.pick_build(4);
        assert!(g.build_menu);
        g.resources.wood = 3;
        g.pick_build(4);
        assert_eq!(g.mode, DesignationKind::WoodBed);
        g.last_dir = Dir::E; // (3,1) is floor
        assert_eq!(
            g.dig(),
            Some(ClientMsg::Build(Dir::E, DesignationKind::WoodBed))
        );
        g.resources.wood = 2;
        assert_eq!(g.dig(), None);
    }

    #[test]
    fn full_size_snapshot_is_valid() {
        let mut w = moria_core::world::generate(1);
        let mountain = w.mountains.remove(&MountainId(0)).unwrap();
        let mut at = moria_core::world::entrance_tiles();
        let view = FortressView {
            mountain,
            hero: HeroView {
                pos: at.next().unwrap(),
                ..game().hero
            },
            dwarves: at
                .take(3)
                .zip(0..)
                .map(|(p, i)| (DwarfId(i), p, None))
                .collect(),
            resources: Resources::default(),
            tick: 0,
        };
        let mut s = Session::default();
        assert!(!s.handle(NetEvent::Msg(ServerMsg::Snapshot(Box::new(view)))));
        assert_eq!(s.screen, Screen::Playing);
    }

    #[test]
    fn new_snapshot_keeps_explored() {
        let g = game();
        let view = FortressView {
            mountain: g.mountain.clone(),
            hero: g.hero.clone(),
            dwarves: vec![],
            resources: Resources::default(),
            tick: 0,
        };
        let mut s = Session::default();
        s.handle(NetEvent::Msg(ServerMsg::Snapshot(Box::new(view.clone()))));
        s.game.as_mut().unwrap().explored.mark(p(2, 1, 0));
        s.handle(NetEvent::Msg(ServerMsg::Snapshot(Box::new(view))));
        assert!(s.game.unwrap().explored.seen(p(2, 1, 0)));
    }
}
