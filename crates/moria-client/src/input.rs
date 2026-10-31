//! Keyboard and mouse to protocol messages, through the `Game` rules.

use crate::Client;
use crate::game::{Screen, drag_msg};
use crate::render::to_tile;
use bevy::input::keyboard::Key;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_egui::input::EguiWantsInput;
use moria_core::{DesignationKind, Dir};

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct InputSet;

/// Mouse drag in progress: start tile and button, and the tile under the cursor.
#[derive(Resource, Default)]
pub struct Drag {
    pub start: Option<((i16, i16), MouseButton)>,
    pub cursor: Option<(i16, i16)>,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<Drag>().add_systems(
        Update,
        (keyboard, mouse)
            .in_set(InputSet)
            .run_if(in_state(Screen::Playing)),
    );
}

const MOVES: [(KeyCode, KeyCode, Dir); 4] = [
    (KeyCode::KeyW, KeyCode::ArrowUp, Dir::N),
    (KeyCode::KeyS, KeyCode::ArrowDown, Dir::S),
    (KeyCode::KeyA, KeyCode::ArrowLeft, Dir::W),
    (KeyCode::KeyD, KeyCode::ArrowRight, Dir::E),
];

fn keyboard(
    keys: Res<ButtonInput<KeyCode>>,
    logical: Res<ButtonInput<Key>>,
    time: Res<Time>,
    mut client: ResMut<Client>,
) {
    let now = time.elapsed_secs_f64();
    let Some(g) = &mut client.session.game else {
        return;
    };
    let mut out = None;
    // Held keys keep walking; the cooldown paces them.
    if let Some(&(_, _, d)) = MOVES
        .iter()
        .find(|(a, b, _)| keys.pressed(*a) || keys.pressed(*b))
    {
        out = g.try_move(d, now);
    }
    let ch = |s: &str| logical.just_pressed(Key::Character(s.into()));
    if ch("<") || keys.just_pressed(KeyCode::KeyQ) {
        out = g.vertical(Dir::Up, now);
    }
    if ch(">") || keys.just_pressed(KeyCode::KeyE) {
        out = g.vertical(Dir::Down, now);
    }
    if keys.just_pressed(KeyCode::Space) {
        out = g.dig();
    }
    if keys.just_pressed(KeyCode::Digit1) {
        g.mode = DesignationKind::Dig;
    }
    if keys.just_pressed(KeyCode::Digit2) {
        g.mode = DesignationKind::Stairs;
    }
    if keys.just_pressed(KeyCode::KeyX) {
        g.mode = match g.mode {
            DesignationKind::Dig => DesignationKind::Stairs,
            DesignationKind::Stairs => DesignationKind::Dig,
        };
    }
    if let Some(m) = out {
        client.send(m);
    }
}

fn mouse(
    buttons: Res<ButtonInput<MouseButton>>,
    window: Single<&Window, With<PrimaryWindow>>,
    camera: Single<(&Camera, &GlobalTransform)>,
    egui: Res<EguiWantsInput>,
    mut drag: ResMut<Drag>,
    mut client: ResMut<Client>,
) {
    let (cam, cam_t) = *camera;
    drag.cursor = window
        .cursor_position()
        .and_then(|p| cam.viewport_to_world_2d(cam_t, p).ok())
        .map(to_tile);
    let Some(g) = &client.session.game else {
        return;
    };
    let (mode, z) = (g.mode, g.current_z());
    if drag.start.is_none() && !egui.wants_pointer_input() {
        for b in [MouseButton::Left, MouseButton::Right] {
            if buttons.just_pressed(b) {
                drag.start = drag.cursor.map(|t| (t, b));
            }
        }
    }
    if let (Some((a, b)), Some(cur)) = (drag.start, drag.cursor)
        && buttons.just_released(b)
    {
        let kind = (b == MouseButton::Left).then_some(mode);
        client.send(drag_msg(a, cur, z, kind));
        drag.start = None;
    }
    if drag
        .start
        .is_some_and(|(_, b)| !buttons.pressed(b) && !buttons.just_released(b))
    {
        drag.start = None;
    }
}
