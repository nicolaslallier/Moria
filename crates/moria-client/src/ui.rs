//! egui screens: login, hero creation, HUD, disconnection.

use crate::Client;
use crate::game::{Screen, Session};
use crate::net::connect;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use moria_core::DesignationKind;
use moria_core::protocol::{ClientMsg, PROTOCOL_VERSION};
use moria_core::srd::{Class, Race, roll_abilities};
use std::path::PathBuf;

const ATTRIBUTION: &str = include_str!("../../../ATTRIBUTION.md");

/// The password lives only in memory; address, name and invite go to `prefs_path`.
#[derive(Resource)]
struct LoginForm {
    addr: String,
    name: String,
    password: String,
    invite: String,
}

#[derive(Resource)]
struct HeroForm {
    race: Race,
    class: Class,
    sent: bool,
}

pub fn plugin(app: &mut App) {
    app.insert_resource(load_prefs())
        .insert_resource(HeroForm {
            race: Race::Dwarf,
            class: Class::Fighter,
            sent: false,
        })
        .add_systems(OnEnter(Screen::CreateHero), |mut f: ResMut<HeroForm>| {
            f.sent = false;
        })
        .add_systems(
            EguiPrimaryContextPass,
            (
                login.run_if(in_state(Screen::Login)),
                create_hero.run_if(in_state(Screen::CreateHero)),
                hud.run_if(in_state(Screen::Playing)),
                disconnected.run_if(in_state(Screen::Disconnected)),
            ),
        );
}

/// Next to the binary, so each copy of the game keeps its own settings.
fn prefs_path() -> Option<PathBuf> {
    Some(
        std::env::current_exe()
            .ok()?
            .with_file_name("moria-client.txt"),
    )
}

fn load_prefs() -> LoginForm {
    let text = prefs_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let mut l = text.lines().map(str::to_owned);
    LoginForm {
        addr: l
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1:7777".into()),
        name: l.next().unwrap_or_default(),
        password: String::new(),
        invite: l.next().unwrap_or_default(),
    }
}

fn save_prefs(f: &LoginForm) {
    if let Some(p) = prefs_path() {
        // Best effort: a read-only install folder only loses the convenience.
        let _ = std::fs::write(p, format!("{}\n{}\n{}\n", f.addr, f.name, f.invite));
    }
}

fn centered(title: &str) -> egui::Window<'_> {
    egui::Window::new(title)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
}

fn error_label(ui: &mut egui::Ui, s: &Session) {
    if let Some(e) = &s.error {
        ui.colored_label(egui::Color32::LIGHT_RED, e);
    }
}

fn login(mut ctx: EguiContexts, mut form: ResMut<LoginForm>, mut client: ResMut<Client>) -> Result {
    let connecting = client.net.is_some();
    centered("Moria — Connexion").show(ctx.ctx_mut()?, |ui| {
        egui::Grid::new("login").num_columns(2).show(ui, |ui| {
            ui.label("Serveur");
            ui.text_edit_singleline(&mut form.addr);
            ui.end_row();
            ui.label("Nom");
            ui.text_edit_singleline(&mut form.name);
            ui.end_row();
            ui.label("Mot de passe");
            ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
            ui.end_row();
            ui.label("Code d'invitation");
            ui.add(egui::TextEdit::singleline(&mut form.invite).hint_text("facultatif"));
            ui.end_row();
        });
        error_label(ui, &client.session);
        if connecting {
            ui.label("Connexion en cours…");
        } else if ui.button("Se connecter").clicked() {
            save_prefs(&form);
            let invite = form.invite.trim();
            client.session.error = None;
            client.net = Some(SyncCell::new(connect(form.addr.trim().to_owned())));
            client.send(ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                name: form.name.trim().to_owned(),
                password: form.password.clone(),
                invite: (!invite.is_empty()).then(|| invite.to_owned()),
            });
        }
    });
    Ok(())
}

fn create_hero(
    mut ctx: EguiContexts,
    mut form: ResMut<HeroForm>,
    mut client: ResMut<Client>,
) -> Result {
    centered("Création du héros").show(ctx.ctx_mut()?, |ui| {
        egui::ComboBox::from_label("Race")
            .selected_text(form.race.label_fr())
            .show_ui(ui, |ui| {
                for r in Race::ALL {
                    ui.selectable_value(&mut form.race, r, r.label_fr());
                }
            });
        egui::ComboBox::from_label("Classe")
            .selected_text(form.class.label_fr())
            .show_ui(ui, |ui| {
                for c in Class::ALL {
                    ui.selectable_value(&mut form.class, c, c.label_fr());
                }
            });
        let a = roll_abilities(form.race, form.class);
        ui.label(format!(
            "FOR {}  DEX {}  CON {}  INT {}  SAG {}  CHA {}",
            a.str, a.dex, a.con, a.int, a.wis, a.cha
        ));
        if form.sent {
            ui.label("Création en cours…");
        } else if ui.button("Créer le héros").clicked() {
            client.send(ClientMsg::CreateHero {
                race: form.race,
                class: form.class,
            });
            form.sent = true;
        }
        ui.separator();
        let text = ATTRIBUTION.trim_start_matches("# Attribution").trim();
        ui.add(egui::Label::new(egui::RichText::new(text).small()).wrap());
    });
    Ok(())
}

fn hud(mut ctx: EguiContexts, client: Res<Client>) -> Result {
    let Some(g) = &client.session.game else {
        return Ok(());
    };
    egui::Window::new("hud")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::LEFT_TOP, [8.0, 8.0])
        .show(ctx.ctx_mut()?, |ui| {
            let z = g.current_z();
            if g.browse_z.is_some() {
                ui.label(format!("Niveau Z {z} (héros en Z {})", g.hero.pos.z));
            } else {
                ui.label(format!("Niveau Z {z}"));
            }
            let r = g.resources;
            ui.label(format!(
                "Pierre {} / Or {} / Mithril {}",
                r.stone, r.gold, r.mithril
            ));
            ui.label(format!(
                "{} — {} {}",
                g.hero.name,
                g.hero.race.label_fr(),
                g.hero.class.label_fr()
            ));
            ui.label(match g.mode {
                DesignationKind::Dig => "Mode : Creuser",
                DesignationKind::Stairs => "Mode : Escalier",
            });
            ui.small(
                "WASD (ZQSD en AZERTY)/flèches : marcher · Espace : creuser · < > ou Q/E (A/E en AZERTY) : escalier ou niveau \
                 · X, 1, 2 : mode · glisser clic gauche : désigner · clic droit : annuler",
            );
        });
    Ok(())
}

fn disconnected(mut ctx: EguiContexts, mut client: ResMut<Client>) -> Result {
    centered("Déconnecté").show(ctx.ctx_mut()?, |ui| {
        error_label(ui, &client.session);
        if ui.button("Reconnecter").clicked() {
            client.session = Session::default();
            client.net = None;
        }
    });
    Ok(())
}
