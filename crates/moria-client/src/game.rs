//! Client-side model, free of Bevy systems: session flow, local fortress copy,
//! prediction and input rules. The Bevy systems in the other modules only call this.

use crate::net::NetEvent;
use bevy::prelude::States;
use moria_core::protocol::{ClientMsg, FortressView, HeroView, Reject, ServerMsg, TickDelta};
use moria_core::{DesignationKind, Dir, DwarfId, Mountain, Resources, TilePos};

/// Two server ticks of 100 ms, plus a little slack for network jitter.
// ponytail: fixed margin; measure real RTT jitter if moves still get dropped server-side.
pub const MOVE_COOLDOWN: f64 = 0.22;

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
}

impl Game {
    pub fn new(v: FortressView) -> Self {
        Self {
            mountain: v.mountain,
            hero: v.hero,
            dwarves: v.dwarves,
            resources: v.resources,
            last_dir: Dir::E,
            mode: DesignationKind::Dig,
            browse_z: None,
            last_move: f64::NEG_INFINITY,
        }
    }

    pub fn current_z(&self) -> u8 {
        self.browse_z.unwrap_or(self.hero.pos.z)
    }

    /// Server state wins: tiles, designations, dwarves, resources, and the hero snaps to
    /// the authoritative position.
    pub fn apply_tick(&mut self, d: &TickDelta) {
        for &(p, t) in &d.tiles {
            self.mountain.set(p, t);
        }
        for p in &d.designations_removed {
            self.mountain.designations.remove(p);
        }
        for &(p, k) in &d.designations_added {
            self.mountain.designations.insert(p, k);
        }
        self.dwarves.clone_from(&d.dwarves);
        self.resources = d.resources;
        self.hero.pos = d.hero_pos;
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
            _ => (z + 1).min(depth - 1),
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
            NetEvent::Msg(ServerMsg::Snapshot(v)) => {
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
        g.apply_tick(&tick(p(2, 1, 0)));
        assert_eq!(g.hero.pos, p(2, 1, 0));
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
