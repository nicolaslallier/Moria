//! egui screens: login, hero creation, HUD, disconnection.

use crate::Client;
use crate::art::Art;
use crate::game::{BUILDS, Screen, Session};
use crate::net::connect;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, EguiTextureHandle, egui};
use moria_core::DesignationKind;
use moria_core::protocol::{ClientMsg, PROTOCOL_VERSION};
use moria_core::srd::{Class, Race, roll_abilities};
use std::path::PathBuf;

const ATTRIBUTION: &str = include_str!("../../../ATTRIBUTION.md");

/// Password and invite live only in memory; address and name go to `prefs_path`.
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
                (hud, build_menu).run_if(in_state(Screen::Playing)),
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

/// Used when the binary's folder is not writable.
fn fallback_prefs_path() -> Option<PathBuf> {
    let dir = if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA")?)
    } else {
        PathBuf::from(std::env::var_os("HOME")?).join(".config")
    };
    Some(dir.join("moria").join("prefs"))
}

fn load_prefs() -> LoginForm {
    let text = [prefs_path(), fallback_prefs_path()]
        .into_iter()
        .flatten()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let mut l = text.lines().map(str::to_owned);
    LoginForm {
        addr: l
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1:7777".into()),
        name: l.next().unwrap_or_default(),
        password: String::new(),
        invite: String::new(), // an older prefs file's 3rd line (the invite) is ignored
    }
}

/// Never contains the password or the invite code.
fn prefs_text(f: &LoginForm) -> String {
    format!("{}\n{}\n", f.addr, f.name)
}

fn save_prefs(f: &LoginForm) {
    let text = prefs_text(f);
    if prefs_path().is_some_and(|p| std::fs::write(p, &text).is_ok()) {
        return;
    }
    if let Some(p) = fallback_prefs_path() {
        // Best effort: losing the prefs only costs some typing.
        let _ = p
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::write(&p, &text));
    }
}

fn centered(title: &str) -> egui::Window<'_> {
    egui::Window::new(title)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .collapsible(false)
        .resizable(false)
}

/// Paints `image` behind every window, cropped to fill the screen without stretching.
/// Nothing is drawn until the image has loaded.
fn backdrop(ctx: &mut EguiContexts, images: &Assets<Image>, image: &Handle<Image>) -> Result {
    let Some(size) = images.get(image).map(Image::size_f32) else {
        return Ok(());
    };
    let aspect = size.x / size.y.max(1.0);
    let id = ctx.add_image(EguiTextureHandle::Weak(image.id()));
    let c = ctx.ctx_mut()?;
    let screen = c.content_rect();
    let (sw, sh) = (screen.width(), screen.height().max(1.0));
    // Fraction of the image kept on each axis.
    let (u, v) = if sw / sh > aspect {
        (1.0, aspect * sh / sw)
    } else {
        (sw / sh / aspect, 1.0)
    };
    let uv = egui::Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(u, v));
    c.layer_painter(egui::LayerId::background())
        .image(id, screen, uv, egui::Color32::WHITE);
    Ok(())
}

fn error_label(ui: &mut egui::Ui, s: &Session) {
    if let Some(e) = &s.error {
        ui.colored_label(egui::Color32::LIGHT_RED, e);
    }
}

fn login(
    mut ctx: EguiContexts,
    art: Res<Art>,
    images: Res<Assets<Image>>,
    mut form: ResMut<LoginForm>,
    mut client: ResMut<Client>,
) -> Result {
    backdrop(&mut ctx, &images, &art.castle)?;
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
            ui.add(
                egui::TextEdit::singleline(&mut form.invite)
                    .hint_text("seulement pour créer un compte"),
            );
            ui.end_row();
        });
        error_label(ui, &client.session);
        if connecting {
            ui.label("Connexion en cours…");
            return;
        }
        let create = ui.horizontal(|ui| {
            if ui.button("Se connecter").clicked() {
                Some(false)
            } else if ui.button("Créer un compte").clicked() {
                Some(true)
            } else {
                None
            }
        });
        if let Some(create) = create.inner {
            save_prefs(&form);
            let invite = form.invite.trim();
            client.session.error = None;
            client.net = Some(SyncCell::new(connect(form.addr.trim().to_owned())));
            client.send(ClientMsg::Hello {
                version: PROTOCOL_VERSION,
                name: form.name.trim().to_owned(),
                password: form.password.clone(),
                invite: (create && !invite.is_empty()).then(|| invite.to_owned()),
                create,
            });
        }
    });
    Ok(())
}

fn create_hero(
    mut ctx: EguiContexts,
    art: Res<Art>,
    images: Res<Assets<Image>>,
    mut form: ResMut<HeroForm>,
    mut client: ResMut<Client>,
) -> Result {
    backdrop(&mut ctx, &images, &art.castle)?;
    let portrait = ctx.add_image(EguiTextureHandle::Weak(art.portrait(form.race).id()));
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
        ui.image(egui::load::SizedTexture::new(portrait, [192.0, 192.0]));
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

fn build_menu(mut ctx: EguiContexts, mut client: ResMut<Client>) -> Result {
    let Some(g) = &mut client.session.game else {
        return Ok(());
    };
    if !g.build_menu {
        return Ok(());
    }
    let stone = g.resources.stone;
    centered("Construire").show(ctx.ctx_mut()?, |ui| {
        ui.label(format!("Pierre disponible : {stone}"));
        ui.separator();
        let mut picked = None;
        for (i, &(_, label, cost)) in BUILDS.iter().enumerate() {
            let text = format!("{label} — {cost} pierre");
            let b = egui::Button::new(text).selected(i == g.build_sel);
            if ui.add_enabled(stone >= cost, b).clicked() {
                picked = Some(i);
            }
        }
        if let Some(i) = picked {
            g.pick_build(i);
        }
        ui.separator();
        ui.small("Clavier : flèches + Espace · 3 ou Échap : fermer");
        ui.small("Manette : croix/stick + A/✕ · bouton gauche (X/□) ou B/○ : fermer");
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
                DesignationKind::Wall => "Mode : Mur (1 pierre)",
            });
            ui.small(
                "WASD (ZQSD en AZERTY)/flèches : marcher · Espace : creuser (en mode Mur : construire) · < > ou Q/E (A/E en AZERTY) : escalier ou niveau \
                 · X, 1, 2 : mode · 3 : menu construire · glisser clic gauche : désigner · clic droit : annuler",
            );
            ui.small("Manette : stick/croix : marcher · bouton du bas (A/✕) : creuser/construire \
                 · LB/RB (L1/R1) : monter/descendre · bouton du haut (Y/△) : mode \
                 · bouton gauche (X/□) : menu construire");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_never_store_secrets() {
        let f = LoginForm {
            addr: "1.2.3.4:7777".into(),
            name: "Gimli".into(),
            password: "motdepasse".into(),
            invite: "mellon".into(),
        };
        let t = prefs_text(&f);
        assert_eq!(t, "1.2.3.4:7777\nGimli\n");
    }
}
