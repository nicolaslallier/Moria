//! Keyboard, gamepad and mouse to protocol messages, through the `Game` rules.

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
        (play, mouse)
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

/// What the player asked for this frame, merged from keyboard and every pad,
/// so each action fires at most once whatever the number of devices.
#[derive(Default)]
struct Intent {
    walk: Option<Dir>,
    up: bool,
    down: bool,
    dig: bool,
    toggle: bool,
    mode: Option<DesignationKind>,
    build_menu: bool,
    close_menu: bool,
    status: bool,
}

fn keyboard_intent(keys: &ButtonInput<KeyCode>, logical: &ButtonInput<Key>) -> Intent {
    let ch = |s: &str| logical.just_pressed(Key::Character(s.into()));
    Intent {
        // Held keys keep walking; the cooldown paces them.
        walk: MOVES
            .iter()
            .find(|(a, b, _)| keys.pressed(*a) || keys.pressed(*b))
            .map(|&(_, _, d)| d),
        up: ch("<") || keys.just_pressed(KeyCode::KeyQ),
        down: ch(">") || keys.just_pressed(KeyCode::KeyE),
        dig: keys.just_pressed(KeyCode::Space),
        toggle: keys.just_pressed(KeyCode::KeyX),
        mode: if keys.just_pressed(KeyCode::Digit1) {
            Some(DesignationKind::Dig)
        } else if keys.just_pressed(KeyCode::Digit2) {
            Some(DesignationKind::Stairs)
        } else if keys.just_pressed(KeyCode::Digit4) {
            Some(DesignationKind::Chop)
        } else {
            None
        },
        build_menu: keys.just_pressed(KeyCode::Digit3),
        close_menu: keys.just_pressed(KeyCode::Escape),
        status: keys.just_pressed(KeyCode::KeyC),
    }
}

/// Signed push along `d` and magnitude across it; `None` for Up/Down.
fn along_across(d: Dir, v: Vec2) -> Option<(f32, f32)> {
    match d {
        Dir::N => Some((v.y, v.x.abs())),
        Dir::S => Some((-v.y, v.x.abs())),
        Dir::E => Some((v.x, v.y.abs())),
        Dir::W => Some((-v.x, v.y.abs())),
        Dir::Up | Dir::Down => None,
    }
}

/// Stick to a cardinal direction: dominant axis past `PRESS`, ties go
/// vertical. Hysteresis: the previous direction holds until the push drops
/// under `RELEASE` or the other axis leads by `STICKY`, so a noisy diagonal
/// does not zigzag.
fn stick_dir(v: Vec2, prev: Option<Dir>) -> Option<Dir> {
    const PRESS: f32 = 0.5;
    const RELEASE: f32 = 0.3;
    const STICKY: f32 = 0.2;
    if let Some((along, across)) = prev.and_then(|p| along_across(p, v))
        && along >= RELEASE
        && across < along + STICKY
    {
        return prev;
    }
    if v.x.abs().max(v.y.abs()) < PRESS {
        None
    } else if v.y.abs() >= v.x.abs() {
        Some(if v.y > 0.0 { Dir::N } else { Dir::S })
    } else {
        Some(if v.x > 0.0 { Dir::E } else { Dir::W })
    }
}

/// The D-pad wins over the stick, so a resting stick never cancels it.
fn pad_dir(dpad: Vec2, stick: Vec2, prev: Option<Dir>) -> Option<Dir> {
    if dpad != Vec2::ZERO {
        stick_dir(dpad, None)
    } else {
        stick_dir(stick, prev)
    }
}

/// Buttons by position, not label: South = A/✕, North = Y/△, LB/RB = L1/R1.
fn gamepad_intent(pads: &Query<&Gamepad>, prev: &mut Option<Dir>) -> Intent {
    let any = |b| pads.iter().any(|p| p.just_pressed(b));
    let walk = pads
        .iter()
        .find_map(|p| pad_dir(p.dpad(), p.left_stick(), *prev));
    *prev = walk;
    Intent {
        walk,
        up: any(GamepadButton::LeftTrigger),
        down: any(GamepadButton::RightTrigger),
        dig: any(GamepadButton::South),
        toggle: any(GamepadButton::North),
        build_menu: any(GamepadButton::West),
        close_menu: any(GamepadButton::East),
        status: any(GamepadButton::Select),
        ..default()
    }
}

fn play(
    keys: Res<ButtonInput<KeyCode>>,
    logical: Res<ButtonInput<Key>>,
    pads: Query<&Gamepad>,
    window: Single<&Window, With<PrimaryWindow>>,
    // Last stick direction (hysteresis) and last menu move (one step per press).
    mut prev: Local<(Option<Dir>, Option<Dir>)>,
    time: Res<Time>,
    mut client: ResMut<Client>,
) {
    let now = time.elapsed_secs_f64();
    let Some(g) = &mut client.session.game else {
        return;
    };
    let k = keyboard_intent(&keys, &logical);
    // gilrs keeps reporting pads in the background; only a focused window plays.
    let (stick, menu_dir) = &mut *prev;
    let p = if window.focused {
        gamepad_intent(&pads, stick)
    } else {
        *stick = None;
        Intent::default()
    };
    if k.build_menu || p.build_menu {
        g.pick_action(None);
    } else if k.close_menu || p.close_menu {
        g.build_menu = false;
        g.status = false;
    }
    if k.status || p.status {
        g.status = !g.status;
    }
    // Open menu: moves and the action button drive the menu, not the hero.
    let walk = k.walk.or(p.walk);
    if g.build_menu {
        // Walk is held; step only on a new direction.
        if walk != *menu_dir && matches!(walk, Some(Dir::N | Dir::S)) {
            g.build_step(walk == Some(Dir::S));
        }
        *menu_dir = walk;
        if k.dig || p.dig {
            g.pick_build(g.build_sel);
        }
        if let Some(m) = k.mode {
            g.pick_action(Some(m));
        }
        return;
    }
    *menu_dir = None;
    let mut out = vec![];
    if let Some(d) = walk {
        out.extend(g.try_move(d, now));
    }
    if k.up || p.up {
        out.extend(g.vertical(Dir::Up, now));
    }
    if k.down || p.down {
        out.extend(g.vertical(Dir::Down, now));
    }
    if k.dig || p.dig {
        out.extend(g.dig());
    }
    if let Some(m) = k.mode {
        g.pick_action(Some(m));
    } else if k.toggle || p.toggle {
        g.mode = g.mode.toggled();
    }
    for m in out {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_4_picks_chop() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::Digit4);
        let i = keyboard_intent(&keys, &ButtonInput::default());
        assert_eq!(i.mode, Some(DesignationKind::Chop));
    }

    #[test]
    fn key_c_toggles_status() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyC);
        assert!(keyboard_intent(&keys, &ButtonInput::default()).status);
    }

    #[test]
    fn stick_dir_picks_dominant_axis() {
        assert_eq!(stick_dir(Vec2::new(0.2, 0.3), None), None);
        assert_eq!(stick_dir(Vec2::new(0.0, 1.0), None), Some(Dir::N));
        assert_eq!(stick_dir(Vec2::new(0.4, -0.9), None), Some(Dir::S));
        assert_eq!(stick_dir(Vec2::new(-0.8, 0.6), None), Some(Dir::W));
        assert_eq!(stick_dir(Vec2::new(0.7, 0.0), None), Some(Dir::E));
        // Exact diagonal: ties go vertical.
        assert_eq!(stick_dir(Vec2::new(0.7, 0.7), None), Some(Dir::N));
        assert_eq!(stick_dir(Vec2::new(-0.7, -0.7), None), Some(Dir::S));
    }

    #[test]
    fn stick_dir_hysteresis() {
        // Noise around the diagonal keeps the held direction.
        assert_eq!(stick_dir(Vec2::new(0.71, 0.70), Some(Dir::N)), Some(Dir::N));
        assert_eq!(stick_dir(Vec2::new(0.71, 0.70), None), Some(Dir::E));
        // Clearly turning switches.
        assert_eq!(stick_dir(Vec2::new(1.0, 0.1), Some(Dir::N)), Some(Dir::E));
        // Under PRESS but over RELEASE keeps walking; under RELEASE stops.
        assert_eq!(stick_dir(Vec2::new(0.0, 0.4), Some(Dir::N)), Some(Dir::N));
        assert_eq!(stick_dir(Vec2::new(0.0, 0.4), None), None);
        assert_eq!(stick_dir(Vec2::new(0.0, 0.2), Some(Dir::N)), None);
    }

    #[test]
    fn dpad_overrides_stick() {
        let (r, l, u) = (Vec2::X, Vec2::new(-0.6, 0.0), Vec2::Y);
        assert_eq!(pad_dir(u, r, None), Some(Dir::N));
        assert_eq!(pad_dir(r, l, None), Some(Dir::E));
        assert_eq!(pad_dir(Vec2::ZERO, r, None), Some(Dir::E));
        assert_eq!(pad_dir(Vec2::ZERO, Vec2::ZERO, Some(Dir::N)), None);
    }
}
