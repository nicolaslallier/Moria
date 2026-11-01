//! egui screens: login, hero creation, HUD, disconnection.

use crate::Client;
use crate::art::Art;
use crate::game::{
    ACTIONS, BUILDS, Game, ORDER_ROWS, Screen, Session, ability_rows, build_text, clock_label,
    level_label, mode_label, recipe_label, resource_bar, stock_text, workshop_label,
};
use crate::lang::{en, set_en, tr, word};
use crate::light::{MemorySync, memory_files};
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
    /// English instead of French.
    en: bool,
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
        .add_systems(Last, memory_sync)
        .add_systems(
            EguiPrimaryContextPass,
            (
                login.run_if(in_state(Screen::Login)),
                create_hero.run_if(in_state(Screen::CreateHero)),
                (
                    top_bar, hud, action_bar, build_menu, order_menu, status, pause_menu,
                )
                    .chain()
                    .run_if(in_state(Screen::Playing)),
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

/// Loads the explored memory when a game starts, saves it every 30 s, when the game ends and on app exit.
fn memory_sync(
    mut client: ResMut<Client>,
    form: Res<LoginForm>,
    time: Res<Time>,
    mut sync: Local<MemorySync>,
    mut dirs: Local<Option<Vec<PathBuf>>>,
    mut exit: MessageReader<AppExit>,
) {
    // Closing the window ends the game too: save what is running (this system runs in `Last`).
    let exit = exit.read().count() > 0;
    // The one or two candidate folders, resolved once.
    let dirs = dirs.get_or_insert_with(|| {
        [prefs_path(), fallback_prefs_path()]
            .into_iter()
            .flatten()
            .filter_map(|p| p.parent().map(PathBuf::from))
            .collect()
    });
    sync.step(time.delta_secs(), exit, &mut client.session, |m| {
        memory_files(dirs, &form.addr, &form.name, m)
    });
}

fn load_prefs() -> LoginForm {
    let text = [prefs_path(), fallback_prefs_path()]
        .into_iter()
        .flatten()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    let f = parse_prefs(&text);
    set_en(f.en);
    f
}

/// Lines: address, name, language (`en` or `fr`).
fn parse_prefs(text: &str) -> LoginForm {
    let mut l = text.lines().map(str::to_owned);
    LoginForm {
        addr: l
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "127.0.0.1:7777".into()),
        name: l.next().unwrap_or_default(),
        password: String::new(),
        invite: String::new(),
        // An older file's 3rd line is the invite: anything but `en` means French.
        en: l.next().is_some_and(|s| s == "en"),
    }
}

/// Never contains the password or the invite code.
fn prefs_text(f: &LoginForm) -> String {
    let lang = if f.en { "en" } else { "fr" };
    format!("{}\n{}\n{lang}\n", f.addr, f.name)
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
    centered(tr("Moria — Connexion", "Moria — Login")).show(ctx.ctx_mut()?, |ui| {
        egui::Grid::new("login").num_columns(2).show(ui, |ui| {
            ui.label(tr("Serveur", "Server"));
            ui.text_edit_singleline(&mut form.addr);
            ui.end_row();
            ui.label(tr("Nom", "Name"));
            ui.text_edit_singleline(&mut form.name);
            ui.end_row();
            ui.label(tr("Mot de passe", "Password"));
            ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
            ui.end_row();
            ui.label(tr("Code d'invitation", "Invite code"));
            ui.add(egui::TextEdit::singleline(&mut form.invite).hint_text(tr(
                "seulement pour créer un compte",
                "only to create an account",
            )));
            ui.end_row();
        });
        lang_picker(ui, &mut form);
        error_label(ui, &client.session);
        if connecting {
            ui.label(tr("Connexion en cours…", "Connecting…"));
            return;
        }
        let create = ui.horizontal(|ui| {
            if ui.button(tr("Se connecter", "Log in")).clicked() {
                Some(false)
            } else if ui
                .button(tr("Créer un compte", "Create an account"))
                .clicked()
            {
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
    centered(tr("Création du héros", "Create your hero")).show(ctx.ctx_mut()?, |ui| {
        egui::ComboBox::from_label("Race")
            .selected_text(word(form.race.label_fr()))
            .show_ui(ui, |ui| {
                for r in Race::ALL {
                    ui.selectable_value(&mut form.race, r, word(r.label_fr()));
                }
            });
        egui::ComboBox::from_label(tr("Classe", "Class"))
            .selected_text(word(form.class.label_fr()))
            .show_ui(ui, |ui| {
                for c in Class::ALL {
                    ui.selectable_value(&mut form.class, c, word(c.label_fr()));
                }
            });
        ui.image(egui::load::SizedTexture::new(portrait, [192.0, 192.0]));
        let a = roll_abilities(form.race, form.class);
        let (s, w) = if en() { ("STR", "WIS") } else { ("FOR", "SAG") };
        ui.label(format!(
            "{s} {}  DEX {}  CON {}  INT {}  {w} {}  CHA {}",
            a.str, a.dex, a.con, a.int, a.wis, a.cha
        ));
        if form.sent {
            ui.label(tr("Création en cours…", "Creating…"));
        } else if ui.button(tr("Créer le héros", "Create the hero")).clicked() {
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
    centered(tr("Construire", "Build")).show(ctx.ctx_mut()?, |ui| {
        ui.label(if en() {
            format!(
                "Available: {} stone · {} wood · {} gold · {} ingots · {} iron doors",
                r.stone, r.wood, r.gold, r.iron, r.iron_door
            )
        } else {
            format!(
                "Disponible : {} pierre · {} bois · {} or · {} lingots · {} portes en fer",
                r.stone, r.wood, r.gold, r.iron, r.iron_door
            )
        });
        ui.separator();
        let mut picked = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, &(kind, label)) in BUILDS.iter().enumerate() {
                let b = egui::Button::new(build_text(kind, label)).selected(i == g.build_sel);
                if ui.add_enabled(r.covers(&kind.cost()), b).clicked() {
                    picked = Some(i);
                }
            }
        });
        if let Some(i) = picked {
            g.pick_build(i);
        }
        ui.separator();
        ui.small(tr(
            "Clavier : flèches + Espace · 3 ou Échap : fermer",
            "Keyboard: arrows + Space · 3 or Esc: close",
        ));
        ui.small(tr(
            "Manette : croix/stick + A/✕ · bouton gauche (X/□) ou B/○ : fermer",
            "Gamepad: D-pad/stick + A/✕ · left button (X/□) or B/○: close",
        ));
    });
    Ok(())
}

fn order_menu(mut ctx: EguiContexts, mut client: ResMut<Client>) -> Result {
    let Some(g) = &mut client.session.game else {
        return Ok(());
    };
    let msg = order_window(ctx.ctx_mut()?, g);
    if let Some(m) = msg {
        client.send(m);
    }
    Ok(())
}

/// Workshop window: recipe, stock, standing order and the order buttons.
fn order_window(ctx: &egui::Context, g: &mut Game) -> Option<ClientMsg> {
    let dir = g.order_menu?;
    let at = g.hero.pos.step(dir);
    let Some((tile, kind)) = g.mountain.get(at).and_then(|t| Some((t, t.recipe()?))) else {
        g.order_menu = None;
        return None;
    };
    // The server only starts work the spare stock can pay.
    let r = g.spare();
    let queued = g.mountain.orders.get(&at).copied().unwrap_or(0);
    let mut picked = None;
    centered(word(workshop_label(tile))).show(ctx, |ui| {
        ui.label(build_text(kind, recipe_label(kind)));
        ui.label(stock_text(kind, r));
        ui.label(format!(
            "{}{queued}",
            tr("Commande en cours : ", "Current order: ")
        ));
        if !r.covers(&kind.cost()) {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                tr(
                    "En attente : il manque de la matière",
                    "Waiting: not enough materials",
                ),
            );
        }
        ui.separator();
        for (i, &(label, _)) in ORDER_ROWS.iter().enumerate() {
            let b = egui::Button::new(word(label)).selected(i == g.order_sel);
            let r = ui.add(b);
            // Space picks the row (see `play`): a focused button would click too.
            r.surrender_focus();
            if r.clicked() {
                picked = Some(i);
            }
        }
        ui.small(tr(
            "Clavier : N/S + Espace · Échap : fermer",
            "Keyboard: N/S + Space · Esc: close",
        ));
        ui.small(tr(
            "Manette : croix/stick + A/✕ · B/○ : fermer",
            "Gamepad: D-pad/stick + A/✕ · B/○: close",
        ));
    });
    let i = picked?;
    g.order_sel = i;
    g.pick_order(i)
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
        let b = egui::Button::new(egui::RichText::new(word(label)).heading())
            .selected(g.action_active(a));
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
    status_window(ctx.ctx_mut()?, g, portrait);
    Ok(())
}

/// No title bar: its collapse and close buttons could hold focus, and Space digs.
fn status_window(ctx: &egui::Context, g: &mut Game, portrait: egui::TextureId) {
    let rows = g.dwarf_rows();
    let h = &g.hero;
    egui::Window::new("Statut")
        .title_bar(false)
        .anchor(egui::Align2::RIGHT_TOP, [-8.0, below_bar(ctx)])
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading(format!("{}{}", tr("Statut — ", "Status — "), h.name));
            ui.horizontal(|ui| {
                ui.image(egui::load::SizedTexture::new(portrait, [96.0, 96.0]));
                ui.vertical(|ui| {
                    ui.label(format!(
                        "{} {}",
                        word(h.race.label_fr()),
                        word(h.class.label_fr())
                    ));
                    ui.label(level_label(h.pos.z));
                    egui::Grid::new("abilities").show(ui, |ui| {
                        for (name, score, m) in ability_rows(h.abilities) {
                            ui.label(word(name));
                            ui.label(score.to_string());
                            ui.label(m);
                            ui.end_row();
                        }
                    });
                });
            });
            ui.separator();
            ui.strong(format!("{} ({})", tr("Nains", "Dwarves"), rows.len()));
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
            ui.small(tr(
                "C ou Échap : fermer · manette : Select ou B/○",
                "C or Esc: close · gamepad: Select or B/○",
            ));
        });
}

/// Where `bar_window` leaves the bar's bottom edge, in egui's temp memory.
const BAR_BOTTOM: &str = "barre_bas";

/// Top margin of the corner windows: 8 pt under the bar as last drawn.
fn below_bar(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new(BAR_BOTTOM)))
        .unwrap_or(0.0)
        + 8.0
}

fn top_bar(mut ctx: EguiContexts, client: Res<Client>) -> Result {
    if let Some(g) = &client.session.game {
        bar_window(ctx.ctx_mut()?, g);
    }
    Ok(())
}

/// Full-width bar on top, StarCraft style: resources on the left, dwarves and clock on the right.
/// Resources wrap on narrow screens; the bar's rect is returned and its bottom remembered.
fn bar_window(ctx: &egui::Context, g: &Game) -> egui::Rect {
    let width = (ctx.content_rect().width() - 16.0).max(0.0); // the frame's 8 pt side margins
    let rect = egui::Area::new("ressources".into())
        .order(egui::Order::Foreground) // stays over the menus on very short screens
        .anchor(egui::Align2::LEFT_TOP, [0.0, 0.0])
        .show(ctx, |ui| {
            egui::Frame::side_top_panel(&ctx.global_style()).show(ui, |ui| {
                ui.set_width(width);
                // Right group first, so the resources only get the space left of it.
                // One row tall to start, like `ui.horizontal`: `with_layout` would take the whole screen height.
                let row = egui::vec2(width, ui.spacing().interact_size.y);
                let rtl = egui::Layout::right_to_left(egui::Align::Min);
                ui.allocate_ui_with_layout(row, rtl, |ui| {
                    ui.label(clock_label(g.tick));
                    ui.add_space(12.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {}",
                            tr("Nains", "Dwarves"),
                            g.dwarves.len()
                        ))
                        .strong(),
                    );
                    ui.add_space(24.0);
                    let wrap = egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true);
                    ui.with_layout(wrap, |ui| {
                        for (n, what, [r, gr, b]) in resource_bar(g.resources) {
                            // One label that never wraps: a swatch and its name stay on one row.
                            let mut job = egui::text::LayoutJob::default();
                            let fmt = |color| egui::TextFormat {
                                color,
                                ..Default::default()
                            };
                            job.append("■ ", 0.0, fmt(egui::Color32::from_rgb(r, gr, b)));
                            let strong = ui.visuals().strong_text_color();
                            job.append(&format!("{} {n}", word(what)), 0.0, fmt(strong));
                            ui.add(egui::Label::new(job).extend());
                            ui.add_space(12.0);
                        }
                    });
                });
            });
        })
        .response
        .rect;
    ctx.data_mut(|d| d.insert_temp(egui::Id::new(BAR_BOTTOM), rect.bottom()));
    rect
}

fn hud(mut ctx: EguiContexts, client: Res<Client>) -> Result {
    let Some(g) = &client.session.game else {
        return Ok(());
    };
    let ctx = ctx.ctx_mut()?;
    egui::Window::new("hud")
        .title_bar(false)
        .resizable(false)
        .anchor(egui::Align2::LEFT_TOP, [8.0, below_bar(ctx)])
        .show(ctx, |ui| {
            let level = level_label(g.current_z());
            if g.browse_z.is_some() {
                ui.label(format!(
                    "{level} ({}{})",
                    tr("héros : ", "hero: "),
                    level_label(g.hero.pos.z)
                ));
            } else {
                ui.label(level);
            }
            ui.label(format!(
                "{} — {} {}",
                g.hero.name,
                word(g.hero.race.label_fr()),
                word(g.hero.class.label_fr())
            ));
            ui.label(mode_label(g.mode));
            ui.small(tr(
                "WASD (ZQSD en AZERTY)/flèches : marcher · Espace : creuser ou couper (en mode construction : construire) · < > ou Q/E (A/E en AZERTY) : escalier ou niveau \
                 · X : mode suivant · 1, 2, 3, 4 ou barre du bas : creuser, escalier, construire, couper · C : statut · F : utiliser (porte, atelier) · glisser clic gauche : désigner · clic droit : annuler · Échap : menu",
                "WASD (ZQSD on AZERTY)/arrows: walk · Space: dig or chop (in build mode: build) · < > or Q/E (A/E on AZERTY): stairs or level \
                 · X: next mode · 1, 2, 3, 4 or bottom bar: dig, stairs, build, chop · C: status · F: use (door, workshop) · left-drag: designate · right-click: cancel · Esc: menu",
            ));
            ui.small(tr(
                "Manette : stick/croix : marcher · bouton du bas (A/✕) : creuser/construire \
                 · LB/RB (L1/R1) : monter/descendre · bouton du haut (Y/△) : mode \
                 · bouton gauche (X/□) : menu construire · RT (R2) : utiliser · Select : statut · Start : menu",
                "Gamepad: stick/D-pad: walk · bottom button (A/✕): dig/build \
                 · LB/RB (L1/R1): up/down · top button (Y/△): mode \
                 · left button (X/□): build menu · RT (R2): use · Select: status · Start: menu",
            ));
        });
    Ok(())
}

fn disconnected(mut ctx: EguiContexts, mut client: ResMut<Client>) -> Result {
    centered(tr("Déconnecté", "Disconnected")).show(ctx.ctx_mut()?, |ui| {
        error_label(ui, &client.session);
        if ui.button(tr("Reconnecter", "Reconnect")).clicked() {
            client.session = Session::default();
            client.net = None;
        }
    });
    Ok(())
}

/// French / English; the choice is kept in the prefs file.
fn lang_picker(ui: &mut egui::Ui, form: &mut LoginForm) {
    ui.horizontal(|ui| {
        ui.label(tr("Langue", "Language"));
        let mut en = form.en;
        ui.radio_value(&mut en, false, "Français");
        ui.radio_value(&mut en, true, "English");
        if en != form.en {
            form.en = en;
            set_en(en);
            save_prefs(form);
        }
    });
}

/// Escape menu: back to the login screen, save, language, quit. The world keeps running.
fn pause_menu(
    mut ctx: EguiContexts,
    mut client: ResMut<Client>,
    mut form: ResMut<LoginForm>,
    mut exit: MessageWriter<AppExit>,
    mut saved: Local<bool>,
) -> Result {
    let Some(g) = &mut client.session.game else {
        return Ok(());
    };
    if !g.pause {
        *saved = false;
        return Ok(());
    }
    let (mut leave, mut save) = (false, false);
    centered(tr("Menu", "Menu")).show(ctx.ctx_mut()?, |ui| {
        ui.vertical_centered_justified(|ui| {
            if ui.button(tr("Reprendre (Échap)", "Resume (Esc)")).clicked() {
                g.pause = false;
            }
            leave = ui
                .button(tr("Retour au menu principal", "Back to main menu"))
                .clicked();
            save = ui.button(tr("Sauvegarder", "Save")).clicked();
            if *saved {
                ui.label(tr(
                    "Carte sauvegardée. Le serveur sauvegarde le monde toutes les 30 s.",
                    "Map saved. The server saves the world every 30 s.",
                ));
            }
            ui.separator();
            lang_picker(ui, &mut form);
            ui.separator();
            if ui.button(tr("Quitter le jeu", "Quit the game")).clicked() {
                // `memory_sync` saves the map on exit.
                exit.write(AppExit::Success);
            }
        });
    });
    if save {
        client.session.save_now = true;
        *saved = true;
    }
    if leave {
        // Dropping the handle closes the connection; `memory_sync` saves the map.
        client.session.leave();
        client.net = None;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use moria_core::{DesignationKind, Resources};

    #[test]
    fn stock_text_lists_only_the_recipe_inputs() {
        let r = Resources::default();
        let anvil = stock_text(DesignationKind::Smith, r);
        assert!(anvil.contains("lingots") && !anvil.contains("charbon"));
        let forge = stock_text(DesignationKind::Smelt, r);
        assert!(forge.contains("charbon") && !forge.contains("lingots"));
        assert_eq!(
            stock_text(DesignationKind::Smelt, r),
            "En réserve : 0 minerai de fer · 0 charbon"
        );
    }

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

    /// Space picks the order row itself: no order button may hold focus.
    #[test]
    fn order_buttons_never_take_focus() {
        use moria_core::{Dir, Tile};
        let mut g = crate::game::tests::game();
        let e = g.hero.pos.step(Dir::E);
        g.mountain.set(e, Tile::Forge);
        g.order_menu = Some(Dir::E);
        let ctx = egui::Context::default();
        let key = |k| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        for k in [
            egui::Key::Tab,
            egui::Key::Space,
            egui::Key::Tab,
            egui::Key::Space,
        ] {
            let input = egui::RawInput {
                events: vec![key(k)],
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| {
                order_window(ui.ctx(), &mut g);
            });
            out.textures_delta.clear();
            assert_eq!(ctx.memory(|m| m.focused()), None);
        }
    }

    /// Space digs: no status-window control may hold focus and take it as a click.
    #[test]
    fn status_window_never_takes_focus() {
        let mut g = crate::game::tests::game();
        g.status = true;
        let ctx = egui::Context::default();
        let key = |k| egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        for k in [
            egui::Key::Tab,
            egui::Key::Space,
            egui::Key::Tab,
            egui::Key::Space,
        ] {
            let input = egui::RawInput {
                events: vec![key(k)],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                status_window(ui.ctx(), &mut g, egui::TextureId::default())
            })
            .textures_delta
            .clear();
            assert_eq!(ctx.memory(|m| m.focused()), None);
        }
        assert!(g.status);
    }

    /// A painted text: its string, screen rect and number of rows.
    type Text = (String, egui::Rect, usize);

    /// Draws the bar twice on a `w`-pt-wide screen, the pointer over it the second time.
    /// Returns the bar's rect and the texts of the last frame, in paint order.
    fn draw_bar(g: &Game, w: f32) -> (egui::Context, egui::Rect, Vec<Text>) {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, 400.0));
        let mut bar = egui::Rect::NOTHING;
        let mut texts = vec![];
        for pointer in [None, Some(egui::pos2(w / 2.0, 10.0))] {
            let input = egui::RawInput {
                screen_rect: Some(screen),
                events: pointer.map(egui::Event::PointerMoved).into_iter().collect(),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| bar = bar_window(ui.ctx(), g));
            out.textures_delta.clear();
            texts = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) => Some((
                        t.galley.text().to_owned(),
                        s.shape.visual_bounding_rect(),
                        t.galley.rows.len(),
                    )),
                    _ => None,
                })
                .collect();
        }
        (ctx, bar, texts)
    }

    /// The painted text starting with `prefix`.
    fn text<'a>(texts: &'a [Text], prefix: &str) -> &'a Text {
        texts
            .iter()
            .find(|t| t.0.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix:?} in {texts:?}"))
    }

    #[test]
    fn top_bar_is_one_row_on_wide_screens() {
        let (_, bar, texts) = draw_bar(&crate::game::tests::game(), 1600.0);
        assert!(bar.height() < 30.0, "{bar:?}");
        assert_eq!(bar.width(), 1600.0);
        // The resources stop well short of the dwarves and clock.
        let last = text(&texts, "■ Portes en fer").1;
        let dwarves = text(&texts, "Nains").1;
        assert!(last.right() < dwarves.left(), "{last:?} {dwarves:?}");
    }

    #[test]
    fn top_bar_wraps_whole_resources_on_narrow_screens() {
        let mut g = crate::game::tests::game();
        g.resources.iron_ore = 12345;
        let (_, _, texts) = draw_bar(&g, 600.0);
        for t in &texts {
            assert_eq!(t.2, 1, "{:?} is cut across rows", t.0);
        }
        // Each name carries its swatch, so the two cannot end up on different rows.
        for (n, what, _) in crate::game::resource_bar(g.resources) {
            text(&texts, &format!("■ {what} {n}"));
        }
    }

    #[test]
    fn top_bar_fits_narrow_screens_and_swallows_clicks() {
        let mut g = crate::game::tests::game();
        g.resources.iron_ore = 12345;
        let (ctx, bar, _) = draw_bar(&g, 600.0);
        // Wraps instead of running off the screen.
        assert!(bar.right() <= 600.0, "{bar:?}");
        assert!(
            bar.height() > 30.0 && bar.height() < 100.0,
            "a few rows: {bar:?}"
        );
        // The corner windows start under the bar, however tall it gets.
        assert_eq!(below_bar(&ctx), bar.bottom() + 8.0);
        // A click on the bar is egui's, not the map's.
        assert!(ctx.is_pointer_over_egui());
    }

    /// All text painted by `order_window` for a forge east of the hero.
    fn order_text(res: moria_core::Resources) -> String {
        let mut g = crate::game::tests::game();
        g.resources = res;
        order_text_at(g, moria_core::Tile::Forge)
    }

    /// All text painted by `order_window` for `workshop` east of the hero in `g`.
    fn order_text_at(mut g: Game, workshop: moria_core::Tile) -> String {
        use moria_core::Dir;
        let e = g.hero.pos.step(Dir::E);
        g.mountain.set(e, workshop);
        g.order_menu = Some(Dir::E);
        let ctx = egui::Context::default();
        // Two frames: a new window is laid out hidden on its first.
        let mut text = String::new();
        for _ in 0..2 {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                order_window(ui.ctx(), &mut g);
            });
            out.textures_delta.clear();
            text = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::epaint::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        text
    }

    #[test]
    fn order_window_warns_when_short_of_matter() {
        let none = moria_core::Resources::default();
        assert!(order_text(none).contains("En attente"));
        let ok = moria_core::Resources {
            iron_ore: 2,
            coal: 1,
            ..none
        };
        let t = order_text(ok);
        assert!(!t.contains("En attente"));
        assert!(t.contains("Forge"));
    }

    #[test]
    fn order_window_counts_only_the_spare_stock() {
        use moria_core::protocol::DwarfJob;
        use moria_core::{DwarfId, Tile};
        let mut g = crate::game::tests::game();
        g.resources = moria_core::Resources {
            iron: 4,
            ..Default::default()
        };
        // Building an anvil promises 3 of the 4 ingots; a door needs 2.
        let job = DwarfJob {
            kind: DesignationKind::Anvil,
            working: true,
        };
        g.dwarves = vec![(DwarfId(1), g.hero.pos, Some(job))];
        let t = order_text_at(g, Tile::Anvil);
        assert!(t.contains("En attente"), "{t}");
        assert!(t.contains("Enclume"), "{t}");
    }

    #[test]
    fn prefs_never_store_secrets() {
        let f = LoginForm {
            addr: "1.2.3.4:7777".into(),
            name: "Gimli".into(),
            password: "motdepasse".into(),
            invite: "mellon".into(),
            en: true,
        };
        let t = prefs_text(&f);
        assert_eq!(t, "1.2.3.4:7777\nGimli\nen\n");
    }

    #[test]
    fn prefs_keep_the_language_and_ignore_an_old_invite_line() {
        assert!(parse_prefs("a\nb\nen\n").en);
        assert!(!parse_prefs("a\nb\nfr\n").en);
        assert!(!parse_prefs("a\nb\nmellon\n").en); // older file: 3rd line was the invite
        assert!(!parse_prefs("").en);
    }
}
