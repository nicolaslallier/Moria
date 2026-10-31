//! Moria client: log in, create a hero, dig the fortress (top-down, one Z-level at a time).

mod art;
mod game;
mod input;
mod net;
mod render;
mod ui;

use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy_egui::EguiPlugin;
use game::{Screen, Session};
use moria_core::protocol::ClientMsg;
use net::NetHandle;

#[derive(Resource, Default)]
pub struct Client {
    pub session: Session,
    /// `SyncCell`: the mpsc receiver is not `Sync`, and only `&mut` access is needed.
    pub net: Option<SyncCell<NetHandle>>,
}

impl Client {
    pub fn send(&mut self, m: ClientMsg) {
        if let Some(n) = &mut self.net {
            // A closed channel shows up as Disconnected on the event side.
            let _ = n.get().tx.send(m);
        }
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Moria".into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(EguiPlugin::default())
        .init_state::<Screen>()
        .init_resource::<Client>()
        .init_resource::<art::Art>()
        .add_systems(Startup, |mut c: Commands| {
            c.spawn(Camera2d);
        })
        .add_systems(PreUpdate, poll_net)
        .add_plugins((ui::plugin, render::plugin, input::plugin))
        .run();
}

/// Drains network events into the session and mirrors its screen into the Bevy state.
fn poll_net(
    mut client: ResMut<Client>,
    state: Res<State<Screen>>,
    mut next: ResMut<NextState<Screen>>,
) {
    let c = &mut *client;
    while let Some(ev) = c.net.as_mut().and_then(|n| n.get().rx.try_recv().ok()) {
        if c.session.handle(ev) {
            c.net = None;
        }
    }
    if c.session.screen != *state.get() {
        next.set(c.session.screen);
    }
}
