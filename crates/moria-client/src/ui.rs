//! egui screens: login, hero creation, HUD, disconnection.

use crate::Client;
use crate::art::Art;
use crate::game::{
    ACTIONS, BUILDS, Game, Screen, Session, ability_rows, build_text, level_label, mode_label,
};
use crate::net::connect;
use bevy::platform::cell::SyncCell;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, EguiTextureHandle, egui};
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
                (hud, action_bar, build_menu, status).run_if(in_state(Screen::Playing)),
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
    let r = g.resources;
    centered("Construire").show(ctx.ctx_mut()?, |ui| {
        ui.label(format!(
            "Pierre disponible : {} · Bois disponible : {}",
            r.stone, r.wood
        ));
        ui.separator();
        let mut picked = None;
        for (i, &(kind, label)) in BUILDS.iter().enumerate() {
            let b = egui::Button::new(build_text(kind, label)).selected(i == g.build_sel);
            if ui.add_enabled(r.covers(&kind.cost()), b).clicked() {
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

/// Bottom bar: one button per action, the current one highlighted.
fn action_bar(mut ctx: EguiContexts, mut client: ResMut<Client>) -> Result {
    let Some(g) = &mut client.session.game else {
        return Ok(());
    };
    egui::Window::new("actions")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -8.0])
        .show(ctx.ctx_mut()?, |ui| {
            ui.horizontal(|ui| action_buttons(ui, g));
        });
    Ok(())
}

fn action_buttons(ui: &mut egui::Ui, g: &mut Game) {
    for &(label, a) in ACTIONS {
        let b =
            egui::Button::new(egui::RichText::new(label).heading()).selected(g.action_active(a));
        let r = ui.add(b);
        // Space digs: a focused button would also take it as a click.
        r.surrender_focus();
        if r.clicked() {
            g.pick_action(a);
        }
    }
}

/// Status page (key C): the hero's sheet and what every dwarf is doing.
fn status(mut ctx: EguiContexts, art: Res<Art>, mut client: ResMut<Client>) -> Result {
    let Some(g) = &mut client.session.game else {
        return Ok(());
    };
    if !g.status {
        return Ok(());
    }
    let portrait = ctx.add_image(EguiTextureHandle::Weak(art.portrait(g.hero.race).id()));
    let rows = g.dwarf_rows();
    let h = &g.hero;
    egui::Window::new("Statut")
        .open(&mut g.status)
        .anchor(egui::Align2::RIGHT_TOP, [-8.0, 8.0])
        .resizable(false)
        .show(ctx.ctx_mut()?, |ui| {
            ui.heading(&h.name);
            ui.horizontal(|ui| {
                ui.image(egui::load::SizedTexture::new(portrait, [96.0, 96.0]));
                ui.vertical(|ui| {
                    ui.label(format!("{} {}", h.race.label_fr(), h.class.label_fr()));
                    ui.label(level_label(h.pos.z));
                    egui::Grid::new("abilities").show(ui, |ui| {
                        for (name, score, m) in ability_rows(h.abilities) {
                            ui.label(name);
                            ui.label(score.to_string());
                            ui.label(m);
                            ui.end_row();
                        }
                    });
                });
            });
            ui.separator();
            ui.strong(format!("Nains ({})", rows.len()));
            egui::ScrollArea::vertical()
                .max_height(240.0)
                .show(ui, |ui| {
                    egui::Grid::new("dwarves").striped(true).show(ui, |ui| {
                        for (name, level, job) in rows {
                            ui.label(name);
                            ui.label(level);
                            ui.label(job);
                            ui.end_row();
                        }
                    });
                });
            ui.small("C ou Échap : fermer · manette : Select");
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
            let level = level_label(g.current_z());
            if g.browse_z.is_some() {
                ui.label(format!("{level} (héros : {})", level_label(g.hero.pos.z)));
            } else {
                ui.label(level);
            }
            let r = g.resources;
            ui.label(format!(
                "Pierre {} / Bois {} / Or {} / Mithril {}",
                r.stone, r.wood, r.gold, r.mithril
            ));
            ui.label(format!(
                "{} — {} {}",
                g.hero.name,
                g.hero.race.label_fr(),
                g.hero.class.label_fr()
            ));
            ui.label(mode_label(g.mode));
            ui.small(
                "WASD (ZQSD en AZERTY)/flèches : marcher · Espace : creuser ou couper (en mode construction : construire) · < > ou Q/E (A/E en AZERTY) : escalier ou niveau \
                 · X : mode suivant · 1, 2, 3, 4 ou barre du bas : creuser, escalier, construire, couper · C : statut · glisser clic gauche : désigner · clic droit : annuler",
            );
            ui.small("Manette : stick/croix : marcher · bouton du bas (A/✕) : creuser/construire \
                 · LB/RB (L1/R1) : monter/descendre · bouton du haut (Y/△) : mode \
                 · bouton gauche (X/□) : menu construire · Select : statut");
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

    /// Space digs; it must never also press a bar button that holds focus.
    #[test]
    fn space_does_not_press_bar_buttons() {
        let mut g = crate::game::tests::game();
        g.mode = moria_core::DesignationKind::Chop;
        let ctx = egui::Context::default();
        let key = |k| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        for k in [egui::Key::Tab, egui::Key::Space, egui::Key::Space] {
            let input = egui::RawInput {
                events: vec![key(k)],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| action_buttons(ui, &mut g))
                .textures_delta
                .clear();
        }
        assert_eq!(g.mode, moria_core::DesignationKind::Chop);
        assert!(!g.build_menu);
    }

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
