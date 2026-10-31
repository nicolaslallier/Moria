//! Textured tiles for the current level, hero/dwarf heads, designation outlines.

use crate::Client;
use crate::art::Art;
use crate::game::Screen;
use crate::input::Drag;
use bevy::prelude::*;
use moria_core::{DesignationKind, Tile, TilePos};

pub const TILE: f32 = 48.0;

/// Image and tint per tile: ore veins are the rock texture tinted (no ore art yet).
fn tile_look(t: Tile, art: &Art) -> (Handle<Image>, Color) {
    match t {
        Tile::Rock => (art.rock.clone(), Color::WHITE),
        Tile::Floor => (art.floor.clone(), Color::srgb(0.55, 0.45, 0.35)),
        Tile::Gold => (art.rock.clone(), Color::srgb_u8(0xff, 0xc8, 0x30)),
        Tile::Mithril => (art.rock.clone(), Color::srgb_u8(0x60, 0xc0, 0xff)),
        Tile::Stair => (art.stair.clone(), Color::WHITE),
    }
}

pub fn to_world(x: i16, y: i16) -> Vec2 {
    Vec2::new(f32::from(x) * TILE, -f32::from(y) * TILE)
}

pub fn to_tile(w: Vec2) -> (i16, i16) {
    ((w.x / TILE).round() as i16, (-w.y / TILE).round() as i16)
}

/// `shown` caches the drawn tile so only changed sprites get a new color.
#[derive(Component)]
struct TileSprite {
    x: i16,
    y: i16,
    shown: Option<Tile>,
}

#[derive(Component, PartialEq)]
enum Mark {
    Hero,
    Dwarf,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<Built>()
        .add_systems(OnExit(Screen::Playing), |mut b: ResMut<Built>| b.0 = 0)
        .add_systems(
            Update,
            (
                spawn_map,
                (sync_tiles, sync_marks, follow_camera, draw_outlines),
            )
                .chain()
                .after(crate::input::InputSet)
                .run_if(in_state(Screen::Playing)),
        );
}

fn head(image: Handle<Image>) -> impl Bundle {
    (
        Sprite {
            image,
            custom_size: Some(Vec2::splat(TILE)),
            ..default()
        },
        DespawnOnExit(Screen::Playing),
        MapPart,
    )
}

/// Everything `spawn_map` must clear before a rebuild.
#[derive(Component)]
struct MapPart;

/// `Session::snapshots` value the map sprites were built for (0: none).
#[derive(Resource, Default)]
struct Built(u32);

/// (Re)builds the sprites on entering Playing and on every new snapshot.
fn spawn_map(
    mut commands: Commands,
    client: Res<Client>,
    art: Res<Art>,
    mut built: ResMut<Built>,
    old: Query<Entity, With<MapPart>>,
) {
    let Some(g) = &client.session.game else {
        return;
    };
    if built.0 == client.session.snapshots {
        return;
    }
    built.0 = client.session.snapshots;
    for e in &old {
        commands.entity(e).despawn();
    }
    let (w, h, _) = g.mountain.size();
    for y in 0..h {
        for x in 0..w {
            commands.spawn((
                Sprite::from_color(Color::BLACK, Vec2::splat(TILE)),
                Transform::from_translation(to_world(x, y).extend(0.0)),
                TileSprite { x, y, shown: None },
                DespawnOnExit(Screen::Playing),
                MapPart,
            ));
        }
    }
    commands.spawn((head(art.head(g.hero.race)), Mark::Hero));
}

fn sync_tiles(client: Res<Client>, art: Res<Art>, mut q: Query<(&mut TileSprite, &mut Sprite)>) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    for (mut t, mut s) in &mut q {
        let tile = g.mountain.get(TilePos::new(t.x, t.y, z));
        if tile != t.shown {
            t.shown = tile;
            (s.image, s.color) = match tile {
                Some(t) => tile_look(t, &art),
                None => (Handle::default(), Color::BLACK),
            };
        }
    }
}

fn place(t: &mut Transform, v: &mut Visibility, p: TilePos, z: u8) {
    t.translation = to_world(p.x, p.y).extend(1.0);
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
        let pos = match mark {
            Mark::Hero => Some(g.hero.pos),
            Mark::Dwarf => ds.next().map(|&(_, p)| p),
        };
        match pos {
            Some(p) => place(&mut t, &mut v, p, z),
            None => commands.entity(e).despawn(),
        }
    }
    for &(_, p) in ds {
        let mut t = Transform::default();
        let mut v = Visibility::default();
        place(&mut t, &mut v, p, z);
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

fn draw_outlines(client: Res<Client>, drag: Res<Drag>, mut gizmos: Gizmos) {
    let Some(g) = &client.session.game else {
        return;
    };
    let z = g.current_z();
    if g.hero.pos.z == z {
        let at = to_world(g.hero.pos.x, g.hero.pos.y);
        gizmos.rect_2d(at, Vec2::splat(TILE), Color::srgb(1.0, 0.15, 0.15));
    }
    // TilePos orders by x first, so a z-range is not contiguous; the map is small.
    for (p, k) in g.mountain.designations.iter().filter(|(p, _)| p.z == z) {
        let color = match k {
            DesignationKind::Dig => Color::srgb(1.0, 0.9, 0.0),
            DesignationKind::Stairs => Color::srgb(0.0, 0.9, 0.9),
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
