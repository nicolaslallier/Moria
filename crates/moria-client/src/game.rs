//! Client-side model, free of Bevy systems: session flow, local fortress copy,
//! prediction and input rules. The Bevy systems in the other modules only call this.

use crate::net::NetEvent;
use bevy::prelude::States;
use moria_core::protocol::{ClientMsg, FortressView, HeroView, Reject, ServerMsg, TickDelta};
use moria_core::{DesignationKind, Dir, DwarfId, Mountain, Resources, TilePos};
use std::collections::VecDeque;

/// Two server ticks of 100 ms, plus a little slack for network jitter.
// ponytail: fixed margin; measure real RTT jitter if moves still get dropped server-side.
pub const MOVE_COOLDOWN: f64 = 0.22;
/// Ticks after which an unacknowledged predicted move is considered dropped.
const PENDING_TICKS: u64 = 5;

/// Bounds on an untrusted snapshot, well above the real 48×48×8.
fn valid_view(v: &FortressView) -> bool {
    let (w, h, d) = v.mountain.size();
    v.mountain.is_consistent()
        && (1..=256).contains(&w)
        && (1..=256).contains(&h)
        && (1..=64).contains(&d)
        && v.mountain.in_bounds(v.hero.pos)
        && v.dwarves.len() <= 1024
        && v.dwarves.iter().all(|(_, p)| v.mountain.in_bounds(*p))
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
        Reject::BadPassword => "Mot de passe incorrect".into(),
        Reject::ServerFull => "Serveur plein (10 joueurs)".into(),
        Reject::NoFreeMountain => "Plus de montagne libre".into(),
        Reject::InvalidName => "Nom invalide (3–16 caractères : lettres, chiffres, _ ou -)".into(),
        Reject::BadInvite => "Code d'invitation invalide".into(),
        Reject::UnknownAccount => "Compte inconnu — utilisez « Créer un compte »".into(),
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

pub struct Game {
    pub mountain: Mountain,
    pub hero: HeroView,
    pub dwarves: Vec<(DwarfId, TilePos)>,
    pub resources: Resources,
    /// Last horizontal direction pressed; Space digs that way.
    pub last_dir: Dir,
    pub mode: DesignationKind,
    /// `Some` while the player browses another level than the hero's.
    pub browse_z: Option<u8>,
    last_move: f64,
    /// Last hero position reported by the server.
    pub confirmed: TilePos,
    /// Predicted positions not yet seen in a tick, with the local tick count at send.
    pub pending: VecDeque<(TilePos, u64)>,
    ticks: u64,
}

impl Game {
    pub fn new(v: FortressView) -> Self {
        Self {
            confirmed: v.hero.pos,
            mountain: v.mountain,
            hero: v.hero,
            dwarves: v.dwarves,
            resources: v.resources,
            last_dir: Dir::E,
            mode: DesignationKind::Dig,
            browse_z: None,
            last_move: f64::NEG_INFINITY,
            pending: VecDeque::new(),
            ticks: 0,
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
            .extend(d.dwarves.iter().filter(|(_, p)| m.in_bounds(*p)));
        self.resources = d.resources;
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
        if now - self.last_move < MOVE_COOLDOWN || !self.mountain.can_move(self.hero.pos, dir) {
            return None;
        }
        self.last_move = now;
        self.hero.pos = self.hero.pos.step(dir);
        self.pending.push_back((self.hero.pos, self.ticks));
        if matches!(dir, Dir::Up | Dir::Down) {
            self.browse_z = None;
        }
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

    /// Hero dig toward the last move direction, only at a solid tile.
    pub fn dig(&self) -> Option<ClientMsg> {
        let target = self.hero.pos.step(self.last_dir);
        self.mountain
            .get(target)
            .is_some_and(|t| t.is_solid())
            .then_some(ClientMsg::Dig(self.last_dir))
    }
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
                self.game = Some(Game::new(*v));
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
        self.game = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::srd::{Class, Race, roll_abilities};
    use moria_core::{MountainId, Tile};

    fn p(x: i16, y: i16, z: u8) -> TilePos {
        TilePos::new(x, y, z)
    }

    /// 6×6×3 rock; floor at (1..=3, 1, 0); stairs at (1,1,0)-(1,1,1).
    fn game() -> Game {
        let mut m = Mountain::new(MountainId(0), 6, 6, 3, Tile::Rock);
        for x in 1..=3 {
            m.set(p(x, 1, 0), Tile::Floor);
        }
        m.set(p(1, 1, 0), Tile::Stair);
        m.set(p(1, 1, 1), Tile::Stair);
        Game::new(FortressView {
            mountain: m,
            hero: HeroView {
                name: "Gimli".into(),
                race: Race::Dwarf,
                class: Class::Fighter,
                abilities: roll_abilities(Race::Dwarf, Class::Fighter),
                pos: p(2, 1, 0),
            },
            dwarves: vec![],
            resources: Resources::default(),
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
            (Reject::BadPassword, "Mot de passe incorrect"),
            (Reject::ServerFull, "Serveur plein (10 joueurs)"),
            (Reject::NoFreeMountain, "Plus de montagne libre"),
            (
                Reject::InvalidName,
                "Nom invalide (3–16 caractères : lettres, chiffres, _ ou -)",
            ),
            (Reject::BadInvite, "Code d'invitation invalide"),
            (
                Reject::UnknownAccount,
                "Compte inconnu — utilisez « Créer un compte »",
            ),
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
        for _ in 0..5 {
            g.apply_tick(&tick(p(2, 1, 0)));
            assert_eq!(g.hero.pos, p(3, 1, 0));
        }
        // The move was dropped server-side: give up after ~5 ticks.
        g.apply_tick(&tick(p(2, 1, 0)));
        assert_eq!(g.hero.pos, p(2, 1, 0));
        assert!(g.pending.is_empty());
    }

    #[test]
    fn tick_ignores_out_of_bounds_positions() {
        let mut g = game();
        let mut d = tick(p(99, 1, 0));
        d.dwarves = vec![(DwarfId(1), p(3, 1, 0)), (DwarfId(2), p(-1, 0, 0))];
        d.designations_added = vec![(p(-5, 0, 0), DesignationKind::Dig)];
        g.mountain
            .designations
            .insert(p(100, 0, 0), DesignationKind::Dig);
        d.designations_removed = vec![p(100, 0, 0)];
        g.apply_tick(&d);
        assert_eq!(g.hero.pos, p(2, 1, 0));
        assert_eq!(g.dwarves, vec![(DwarfId(1), p(3, 1, 0))]);
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
        v.dwarves = vec![(DwarfId(0), p(2, 1, 0)); 1025];
        assert!(s.handle(NetEvent::Msg(ServerMsg::Snapshot(v.clone()))));
        assert_eq!(s.screen, Screen::Disconnected);
        let mut s = Session::default();
        v.dwarves = vec![(DwarfId(0), p(6, 1, 0))];
        assert!(s.handle(NetEvent::Msg(ServerMsg::Snapshot(v))));
        assert_eq!(s.screen, Screen::Disconnected);
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
    fn apply_tick_updates_mountain() {
        let mut g = game();
        g.mountain
            .designations
            .insert(p(0, 0, 0), DesignationKind::Dig);
        let mut d = tick(p(3, 1, 0));
        d.tiles = vec![(p(4, 1, 0), Tile::Floor)];
        d.designations_added = vec![(p(5, 1, 0), DesignationKind::Stairs)];
        d.designations_removed = vec![p(0, 0, 0)];
        d.dwarves = vec![(DwarfId(1), p(3, 1, 0))];
        d.resources.stone = 4;
        g.apply_tick(&d);
        assert_eq!(g.mountain.get(p(4, 1, 0)), Some(Tile::Floor));
        assert_eq!(
            g.mountain.designations.iter().collect::<Vec<_>>(),
            vec![(&p(5, 1, 0), &DesignationKind::Stairs)]
        );
        assert_eq!(g.dwarves, vec![(DwarfId(1), p(3, 1, 0))]);
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
}
