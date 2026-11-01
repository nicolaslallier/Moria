//! Textured tiles for the current level, hero/dwarf heads, designation outlines.

use crate::Client;
use crate::art::Art;
use crate::game::Screen;
use crate::input::Drag;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use moria_core::{DesignationKind, Tile, TilePos};

pub const TILE: f32 = 48.0;

pub fn to_world(x: i16, y: i16) -> Vec2 {
    Vec2::new(f32::from(x) * TILE, -f32::from(y) * TILE)
}

pub fn to_tile(w: Vec2) -> (i16, i16) {
    ((w.x / TILE).round() as i16, (-w.y / TILE).round() as i16)
}

/// One sprite per on-screen cell: the camera is on the hero, so cell `(dx, dy)` shows the
/// tile at hero + (dx, dy). `shown` caches what is drawn so only changed cells are touched.
#[derive(Component)]
struct TileSprite {
    dx: i16,
    dy: i16,
    shown: Option<(TilePos, Option<Tile>, bool)>,
}

/// Tiles from the hero to the window edge, plus a margin of 2.
// ponytail: assumes the camera is pinned on the hero at scale 1; switch to the camera
// Transform and projection if zoom or panning is ever added.
fn view_radius(width: f32, height: f32) -> (i16, i16) {
    (
        (width / TILE / 2.0) as i16 + 2,
        (height / TILE / 2.0) as i16 + 2,
    )
}

/// Every cell offset of a `rx` x `ry` view radius.
fn cells(rx: i16, ry: i16) -> impl Iterator<Item = (i16, i16)> {
    (-ry..=ry).flat_map(move |dy| (-rx..=rx).map(move |dx| (dx, dy)))
}

#[derive(Component, PartialEq)]
enum Mark {
    Hero,
    Dwarf,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<Built>()
        .add_systems(OnExit(Screen::Playing), |mut b: ResMut<Built>| {
            *b = Built::default()
        })
        .add_systems(
            Update,
            (
                spawn_map,
                (
                    sync_tiles,
                    sync_marks,
                    follow_camera,
                    draw_outlines,
                    draw_relief,
                ),
            )
                .chain()
                .after(crate::input::InputSet)
                .run_if(in_state(Screen::Playing)),
        );
}

/// Heads have a dark square background; drawn smaller so the tile underneath shows.
fn head(image: Handle<Image>) -> impl Bundle {
    (
        Sprite {
            image,
            custom_size: Some(Vec2::splat(TILE * 0.64)),
            ..default()
        },
        DespawnOnExit(Screen::Playing),
        MapPart,
    )
}

/// Everything `spawn_map` must clear before a rebuild.
#[derive(Component)]
struct MapPart;

/// `Session::snapshots` value (0: none) and view radius the sprites were built for.
#[derive(Resource, Default, PartialEq)]
struct Built(u32, (i16, i16));

/// (Re)builds the sprites on entering Playing, on every new snapshot and on window resize.
fn spawn_map(
    mut commands: Commands,
    client: Res<Client>,
    art: Res<Art>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut built: ResMut<Built>,
    old: Query<Entity, With<MapPart>>,
) {
    let Some(g) = &client.session.game else {
        return;
    };
    let radius = view_radius(window.width(), window.height());
    let want = Built(client.session.snapshots, radius);
    if *built == want {
        return;
    }
    *built = want;
    for e in &old {
        commands.entity(e).despawn();
    }
    for (dx, dy) in cells(radius.0, radius.1) {
        commands.spawn((
            Sprite::from_color(Color::BLACK, Vec2::splat(TILE)),
            Transform::default(),
            TileSprite {
                dx,
                dy,
                shown: None,
            },
            DespawnOnExit(Screen::Playing),
            MapPart,
        ));
    }
    commands.spawn((head(art.head(g.hero.race)), Mark::Hero));
}

fn sync_tiles(
    client: Res<Client>,
    art: Res<Art>,
    mut q: Query<(&mut TileSprite, &mut Sprite, &mut Transform)>,
) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    let (hx, hy) = (g.hero.pos.x, g.hero.pos.y);
    for (mut t, mut s, mut tr) in &mut q {
        let at = TilePos::new(hx + t.dx, hy + t.dy, z);
        let tile = g.mountain.get(at);
        let on_grass =
            tile.is_some_and(Tile::is_furniture) && g.mountain.ground_at(at) == Tile::Grass;
        let key = Some((at, tile, on_grass));
        if t.shown == key {
            continue;
        }
        t.shown = key;
        tr.translation = to_world(at.x, at.y).extend(0.0);
        (s.image, s.color) = match tile {
            Some(t) => (art.tile(t, on_grass), Color::WHITE),
            None => (Handle::default(), Color::BLACK),
        };
        // Slopes keep their orientation, stairs are the user's art, sky stays flat.
        if tile.is_some_and(|t| !matches!(t, Tile::Stair | Tile::Slope | Tile::Sky)) {
            let variation = ((i32::from(at.x) * 37 + i32::from(at.y) * 61).rem_euclid(11)) as f32;
            let light = 0.88 + variation * 0.012;
            s.color = Color::srgb(light, light, light);
            s.flip_x = (at.x + at.y) % 2 == 0;
            s.flip_y = at.x % 3 == 0;
        } else {
            s.flip_x = false;
            s.flip_y = false;
        }
    }
}

/// `depth` keeps the hero drawn above a dwarf sharing its tile.
fn place(t: &mut Transform, v: &mut Visibility, p: TilePos, z: u8, depth: f32) {
    t.translation = to_world(p.x, p.y).extend(depth);
    *v = if p.z == z {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
}

fn sync_marks(
    mut commands: Commands,
    client: Res<Client>,
    art: Res<Art>,
    mut marks: Query<(Entity, &Mark, &mut Transform, &mut Visibility)>,
) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    let mut ds = g.dwarves.iter();
    for (e, mark, mut t, mut v) in &mut marks {
        let (pos, depth) = match mark {
            Mark::Hero => (Some(g.hero.pos), 2.0),
            Mark::Dwarf => (ds.next().map(|&(_, p)| p), 1.0),
        };
        match pos {
            Some(p) => place(&mut t, &mut v, p, z, depth),
            None => commands.entity(e).despawn(),
        }
    }
    for &(_, p) in ds {
        let mut t = Transform::default();
        let mut v = Visibility::default();
        place(&mut t, &mut v, p, z, 1.0);
        commands.spawn((head(art.worker_head.clone()), Mark::Dwarf, t, v));
    }
}

fn follow_camera(client: Res<Client>, mut cam: Query<&mut Transform, With<Camera2d>>) {
    let Some(g) = &client.session.game else {
        return;
    };
    for mut t in &mut cam {
        let p = to_world(g.hero.pos.x, g.hero.pos.y);
        t.translation.x = p.x;
        t.translation.y = p.y;
    }
}

/// Recomputed from neighbors so digging and level changes reveal the new edges immediately.
/// Only the tiles on screen (the camera is on the hero), so a 176×96 level stays cheap.
/// Stone faces cast a cliff edge; trees and furniture are solid but not cliffs.
fn draw_relief(
    client: Res<Client>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut gizmos: Gizmos,
) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    let (w, h, _) = g.mountain.size();
    let (rx, ry) = view_radius(window.width(), window.height());
    let (hx, hy) = (g.hero.pos.x, g.hero.pos.y);
    let half = TILE / 2.0;
    for y in (hy - ry).max(0)..(hy + ry).min(h) {
        for x in (hx - rx).max(0)..(hx + rx).min(w) {
            if !g
                .mountain
                .get(TilePos::new(x, y, z))
                .is_some_and(Tile::is_stone)
            {
                continue;
            }
            let center = to_world(x, y);
            for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                if !g.mountain.walkable(TilePos::new(x + dx, y + dy, z)) {
                    continue;
                }
                let normal = Vec2::new(f32::from(dx), -f32::from(dy));
                let tangent = Vec2::new(-normal.y, normal.x) * half;
                let edge = center + normal * half;
                // A dark face on the stone and a soft, stepped shadow on the floor.
                for offset in 0..7 {
                    let at = edge - normal * offset as f32;
                    gizmos.line_2d(at - tangent, at + tangent, Color::srgb(0.10, 0.12, 0.16));
                }
                let rim = edge - normal * 7.0;
                gizmos.line_2d(rim - tangent, rim + tangent, Color::srgb(0.39, 0.43, 0.48));
                for offset in 1..6 {
                    let at = edge + normal * offset as f32;
                    gizmos.line_2d(
                        at - tangent,
                        at + tangent,
                        Color::srgba(0.02, 0.025, 0.04, 0.36 - offset as f32 * 0.045),
                    );
                }
            }
        }
    }
    for &(_, p) in &g.dwarves {
        if p.z == z {
            gizmos.circle_2d(
                to_world(p.x, p.y),
                TILE * 0.47,
                Color::srgba(0.61, 0.69, 0.70, 0.6),
            );
        }
    }
}

fn draw_outlines(client: Res<Client>, drag: Res<Drag>, mut gizmos: Gizmos) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    if g.hero.pos.z == z {
        let at = to_world(g.hero.pos.x, g.hero.pos.y);
        gizmos.circle_2d(at, TILE * 0.47, Color::srgb(0.94, 0.73, 0.35));
        gizmos.circle_2d(at, TILE * 0.49, Color::srgba(0.94, 0.73, 0.35, 0.3));
    }
    // TilePos orders by x first, so a z-range is not contiguous; a scan of all designations.
    for (p, k) in g.mountain.designations.iter().filter(|(p, _)| p.z == z) {
        let color = match k {
            DesignationKind::Dig => Color::srgb(1.0, 0.9, 0.0),
            DesignationKind::Stairs => Color::srgb(0.0, 0.9, 0.9),
            DesignationKind::Wall => Color::srgb(1.0, 0.5, 0.2),
            DesignationKind::Chop => Color::srgb(0.3, 0.9, 0.3),
            DesignationKind::Bed
            | DesignationKind::Table
            | DesignationKind::Chair
            | DesignationKind::WoodBed
            | DesignationKind::WoodTable
            | DesignationKind::WoodChair => Color::srgb(0.7, 0.45, 1.0),
        };
        gizmos.rect_2d(to_world(p.x, p.y), Vec2::splat(TILE - 2.0), color);
    }
    if let (Some((a, button)), Some(b)) = (drag.start, drag.cursor) {
        let color = match button {
            MouseButton::Right => Color::srgb(1.0, 0.3, 0.3),
            _ => Color::WHITE,
        };
        let (wa, wb) = (to_world(a.0, a.1), to_world(b.0, b.1));
        let size = (wa - wb).abs() + Vec2::splat(TILE);
        gizmos.rect_2d((wa + wb) / 2.0, size, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_pool_is_window_sized() {
        let (rx, ry) = view_radius(1920.0, 1080.0);
        assert_eq!((rx, ry), (22, 13));
        let n = cells(rx, ry).count();
        assert_eq!(n, 45 * 27);
        assert!(n < 176 * 96 / 10);
        assert!(cells(rx, ry).any(|c| c == (-rx, -ry)) && cells(rx, ry).any(|c| c == (rx, ry)));
    }
}
