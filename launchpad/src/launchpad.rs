//! The small window, laid out like Stand's launchpad: every rect, colour and the font measured from
//! the player's screenshot of it. Buttons left; Advanced or the changelog on the right.

use eframe::egui::{self, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::platform::{self, Platform};
use crate::{App, CHANGELOG, Pane, install, settings};
use aurora_core::paths::DLL_FILE;

pub const SMALL: [f32; 2] = [232.0, 175.0];
/// With the changelog open.
pub const WIDE: [f32; 2] = [694.0, 175.0];
/// With Advanced open: the DLL list, then the game folder and where to hook under it.
pub const ADVANCED: [f32; 2] = [694.0, 215.0];

const BG: Color32 = Color32::from_rgb(17, 17, 17);
const BTN: Color32 = Color32::from_rgb(37, 40, 43);
const BTN_HOVER: Color32 = Color32::from_rgb(52, 56, 60);
const BTN_DOWN: Color32 = Color32::from_rgb(28, 31, 34);
const BTN_EDGE: Color32 = Color32::from_rgb(22, 25, 29);
const LIST_EDGE: Color32 = Color32::from_rgb(100, 100, 100);
const TEXT: Color32 = Color32::WHITE;
const DIM: Color32 = Color32::from_rgb(109, 109, 109);
const BOX: Color32 = Color32::from_rgb(136, 136, 136);
const TICK: Color32 = Color32::from_rgb(0, 95, 184);
const PICK: Color32 = Color32::from_rgb(0, 84, 153);
const FIELD_EDGE: Color32 = Color32::from_rgb(171, 173, 179);
const ARROW_BOX: Color32 = Color32::from_rgb(240, 240, 240);
/// The grey of the line beside Where to hook in Advanced.
const NOTE: Color32 = Color32::from_rgb(160, 160, 160);
/// The line between the DLL list and the game and hook rows in Advanced.
const SEP: Color32 = Color32::from_rgb(46, 46, 46);
/// Where to hook's drop-down box in Advanced (x, y, width, height in window pixels).
const HOOK_BOX: [f32; 4] = [322.0, 165.0, 110.0, 21.0];

/// A hook choice as the drop-down shows it.
fn hook_name(h: settings::Hook) -> &'static str {
    match h {
        settings::Hook::Auto => "Auto",
        settings::Hook::Driver => "GPU driver",
        settings::Hook::Off => "Off",
    }
}

/// The grey line beside the drop-down: what the pick does, short enough for the pane.
fn hook_line(h: settings::Hook, dx11: bool) -> &'static str {
    match h {
        settings::Hook::Auto => "Steam overlay if on, else the GPU driver.",
        settings::Hook::Driver if dx11 => "Vulkan only; on DirectX 11 it works as Auto.",
        settings::Hook::Driver => "Skips overlays (Steam, OBS, RTSS).",
        settings::Hook::Off => "No in-game menu or ESP. Hotkeys still work.",
    }
}

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

/// Another DLL to load after the mod's own, like Stand's list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtraDll {
    pub path: String,
    pub on: bool,
}

/// Segoe UI, Windows' own font (what Stand's WinForms window draws with), as its own family so the
/// in-game menu keeps its look.
pub fn load_font(ctx: &egui::Context) {
    let Ok(bytes) = std::fs::read(r"C:\Windows\Fonts\segoeui.ttf") else { return };
    ctx.add_font(egui::epaint::text::FontInsert::new(
        "segoe",
        egui::FontData::from_owned(bytes),
        vec![egui::epaint::text::InsertFontFamily {
            family: FontFamily::Name("segoe".into()),
            priority: egui::epaint::text::FontPriority::Highest,
        }],
    ));
}

fn font(ctx: &egui::Context) -> FontId {
    let fam = FontFamily::Name("segoe".into());
    // Falls back to egui's own font if Segoe UI was not found.
    let has = ctx.fonts(|f| f.families().contains(&fam));
    // Egui draws Segoe UI about 12 % wider than Windows does at the same size, so a little smaller
    // matches.
    FontId::new(10.7, if has { fam } else { FontFamily::Proportional })
}

/// Is the mouse really over `rect`? Egui keeps its last mouse spot when the mouse leaves the window
/// fast, and counts a few pixels around a widget as on it, which left buttons lit with the mouse
/// elsewhere, and clickable beside them. This asks Windows: the window under the mouse must be this
/// Launchpad's own (not one in front of it), and the spot is measured in that window's client area
/// by Windows itself (`ScreenToClient`), not from egui's idea of where the window is. Every widget
/// here asks this for its colour AND its click.
pub(crate) fn hot(ui: &egui::Ui, rect: Rect) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetCursorPos, GetWindowThreadProcessId, WindowFromPoint};
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: plain Win32 calls on one POINT we own and window handles Windows gave us; a
    // Null handle is checked before use.
    unsafe {
        if GetCursorPos(&mut p) == 0 {
            return false;
        }
        let under = WindowFromPoint(p);
        if under.is_null() {
            return false;
        }
        let root = GetAncestor(under, GA_ROOT);
        if root.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(root, &mut pid);
        // Another program's window is over ours (or the mouse is outside it): not hot. While a file
        // dialog of ours is open egui gets no frames, so our only window here is ours.
        if pid != GetCurrentProcessId() || ScreenToClient(root, &mut p) == 0 {
            return false;
        }
    }
    let ppp = ui.ctx().pixels_per_point();
    rect.contains(pos2(p.x as f32 / ppp, p.y as f32 / ppp))
}

/// A WinForms-style flat button. `left`: text starts at the left, else centred.
fn button(ui: &mut egui::Ui, rect: Rect, text: &str, enabled: bool, left: bool) -> bool {
    let resp = ui.interact(rect, ui.id().with(("lp", text, rect.min.x as i32, rect.min.y as i32)), if enabled { Sense::click() } else { Sense::hover() });
    let fill = if !enabled {
        BTN
    } else if resp.is_pointer_button_down_on() {
        BTN_DOWN
    } else if resp.hovered() && hot(ui, rect) {
        BTN_HOVER
    } else {
        BTN
    };
    let p = ui.painter();
    p.rect(rect, CornerRadius::ZERO, fill, Stroke::new(1.0, BTN_EDGE), StrokeKind::Inside);
    let f = font(ui.ctx());
    let color = if enabled { TEXT } else { DIM };
    // Centred on the ink, not the layout box, snapped to whole pixels so it never blurs.
    let galley = ui.ctx().fonts_mut(|fs| fs.layout_no_wrap(text.to_string(), f, color));
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let x = if left { rect.min.x + 13.0 } else { rect.center().x - galley.mesh_bounds.center().x };
    let y = rect.center().y - galley.size().y / 2.0 - 1.0;
    p.galley(pos2(snap(x), snap(y)), galley, color);
    enabled && resp.clicked() && hot(ui, rect)
}

fn label(ui: &egui::Ui, at: Pos2, text: &str, color: Color32) {
    ui.painter().text(at, Align2::LEFT_CENTER, text, font(ui.ctx()), color);
}

/// Stand's grey square checkbox; returns true when clicked.
fn checkbox(ui: &mut egui::Ui, at: Pos2, on: bool, text: &str) -> bool {
    let bx = r(at.x, at.y, 11.0, 11.0);
    let f = font(ui.ctx());
    let width = ui.ctx().fonts_mut(|fs| fs.layout_no_wrap(text.to_string(), f, TEXT).size().x);
    let hit = bx.union(r(at.x, at.y - 2.0, 19.0 + width, 15.0));
    let resp = ui.interact(hit, ui.id().with(("cb", text)), Sense::click());
    let p = ui.painter();
    p.rect_filled(bx, CornerRadius::ZERO, if resp.hovered() && hot(ui, hit) { Color32::from_rgb(160, 160, 160) } else { BOX });
    if on {
        let c = bx.center();
        p.line_segment([pos2(c.x - 3.5, c.y), pos2(c.x - 1.0, c.y + 2.5)], Stroke::new(1.6, BG));
        p.line_segment([pos2(c.x - 1.0, c.y + 2.5), pos2(c.x + 3.5, c.y - 3.0)], Stroke::new(1.6, BG));
    }
    label(ui, pos2(at.x + 18.0, at.y + 5.5), text, TEXT);
    resp.clicked() && hot(ui, hit)
}

/// The blue rounded tick of a list row.
fn list_tick(ui: &egui::Ui, at: Pos2, on: bool) {
    let bx = r(at.x, at.y, 13.0, 13.0);
    let p = ui.painter();
    if on {
        p.rect_filled(bx, CornerRadius::same(2), TICK);
        let c = bx.center();
        p.line_segment([pos2(c.x - 3.5, c.y), pos2(c.x - 1.0, c.y + 2.5)], Stroke::new(1.3, TEXT));
        p.line_segment([pos2(c.x - 1.0, c.y + 2.5), pos2(c.x + 3.5, c.y - 3.0)], Stroke::new(1.3, TEXT));
    } else {
        p.rect_stroke(bx, CornerRadius::same(2), Stroke::new(1.0, BOX), StrokeKind::Inside);
    }
}

fn triangle(ui: &egui::Ui, c: Pos2, up: bool) {
    let (a, b) = if up { (-2.0, 1.5) } else { (2.0, -1.5) };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![pos2(c.x, c.y + a), pos2(c.x - 3.0, c.y + b), pos2(c.x + 3.0, c.y + b)],
        Color32::BLACK,
        Stroke::NONE,
    ));
}

/// How old Bin's DLL is (the one inject loads; the exe carries none).
fn built_text() -> String {
    match install::check_dll() {
        install::Dll::Missing => "not in Bin".to_string(),
        install::Dll::There(None) => "in Bin".to_string(),
        install::Dll::There(Some(e)) => match e.as_secs() {
            s if s < 120 => "built just now".to_string(),
            s if s < 7200 => format!("built {} min ago", s / 60),
            s if s < 172_800 => format!("built {} h ago", s / 3600),
            s => format!("built {} days ago", s / 86_400),
        },
    }
}

/// Windows' own Open dialog for a.dll; None if the player cancels.
fn pick_dll() -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW};
    let filter: Vec<u16> = "DLL files (*.dll)\0*.dll\0All files\0*.*\0\0".encode_utf16().collect();
    let mut file = vec![0u16; 1024];
    // SAFETY: plain struct of numbers and pointers; zero is its documented empty state.
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = file.len() as u32;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    // SAFETY: every pointer in `ofn` points into buffers alive for the call.
    if unsafe { GetOpenFileNameW(&mut ofn) } == 0 {
        return None;
    }
    let end = file.iter().position(|&c| c == 0).unwrap_or(file.len());
    Some(String::from_utf16_lossy(&file[..end]))
}

impl App {
    pub(crate) fn launcher(&mut self, ui: &mut egui::Ui) {
        let want = match self.pane {
            Pane::None => SMALL,
            Pane::Advanced => ADVANCED,
            Pane::Changelog => WIDE,
        };
        if (ui.ctx().content_rect().width() - want[0]).abs() > 2.0 || (ui.ctx().content_rect().height() - want[1]).abs() > 2.0 {
            ui.ctx().send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::InnerSize(want.into()));
        }
        let o = ui.ctx().content_rect().min - Pos2::ZERO;
        ui.painter().rect_filled(ui.ctx().content_rect(), CornerRadius::ZERO, BG);
        // Everything below is placed in window pixels, measured from Stand's window.
        let at = |x: f32, y: f32, w: f32, h: f32| r(x, y, w, h).translate(o);
        self.launch_button(ui, at(12.0, 12.0, 74.0, 23.0));
        self.store_box(ui, at(92.0, 14.0, 129.0, 21.0));
        if checkbox(ui, at(12.0, 45.0, 0.0, 0.0).min, self.cfg.auto_inject, "Automatically inject when game starts.") {
            self.cfg.auto_inject = !self.cfg.auto_inject;
            self.auto_inject = self.cfg.auto_inject && self.launched && !self.mod_in;
        }
        // The row under the checkbox, as Stand's: Check for updates where Changelog was, Changelog
        // where Options was; Options went into Advanced. Check for updates is greyed: there is
        // nowhere to check yet. Changelog grayed out for now. Text centred, as Stand's.
        button(ui, at(12.0, 64.0, 101.0, 23.0), "Check for updates", false, false);
        for (rect, name, pane, on) in [(at(119.0, 64.0, 102.0, 23.0), "Changelog", Pane::Changelog, false), (at(12.0, 122.0, 209.0, 23.0), "Advanced", Pane::Advanced, true)] {
            if button(ui, rect, name, on, false) {
                self.pane = if self.pane == pane { Pane::None } else { pane };
            }
        }
        if button(ui, at(12.0, 93.0, 209.0, 23.0), "Open Aurora folder", true, false) {
            // Through ensure_folders, never a bare create: an old grbmod folder moves first instead
            // of an empty Aurora appearing beside it.
            match crate::install::ensure_folders() {
                Err(e) => {
                    self.say(e);
                    self.launch_error = Some(crate::FOLDER_BUSY.into());
                }
                Ok(done) => {
                    done.into_iter().for_each(|l| self.say(l));
                    // The player's default folder app (Glint), as a double-click: see folder.rs.
                    crate::folder::open(&aurora_core::paths::root());
                }
            }
        }
        let (status, color) = self.status_line();
        let spot = at(13.0, 154.0, 0.0, 12.0).left_center();
        label(ui, spot, &status, color);
        // A refusal's whole message, with what to copy where, as the line's tooltip.
        let line = Rect::from_min_size(pos2(spot.x, spot.y - 7.0), vec2(207.0, 14.0));
        // A refusal shows in its window only: nothing on the status line, no hover. Inject tries
        // again and opens it again if it still fails.
        if let Some((short, full)) = &self.refusal
            && self.launch_error.as_deref() == Some(short.as_str())
        {
            if self.refusal_open {
                let full = full.clone();
                self.refusal_window(ui, &full);
            }
        } else if self.launch_error.is_none() && self.game.is_some() && !self.dlls_in.is_empty() {
            ui.interact(line, ui.id().with("status tip"), Sense::hover()).on_hover_text(self.status_tip());
        }
        match self.pane {
            Pane::Advanced => self.advanced_pane(ui, o),
            Pane::Changelog => self.changelog_pane(ui, o),
            Pane::None => {}
        }
        self.store_list(ui, at(92.0, 14.0, 129.0, 21.0));
        // Drawn last, over the DLL list it opens onto. Closed with Advanced, so it never comes back
        // open by itself.
        if self.pane == Pane::Advanced {
            self.hook_list(ui, at(HOOK_BOX[0], HOOK_BOX[1], HOOK_BOX[2], HOOK_BOX[3]));
        } else {
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("hook open"), false));
        }
    }

    /// One button that follows the game: Launch while the game is not running, Inject while it
    /// runs. Once the mod is in, Inject still works: it loads the ticked DLLs that are not in yet
    /// (a debugger DLL, say); Aurora already in is left alone, never a second one. There is no
    /// Unload here: the mod unloads from its own menu.
    fn launch_button(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let (text, enabled) = match (&self.game, self.mod_in) {
            (Some(_), true) => ("Inject", self.proc.is_some()),
            (None, true) => ("Inject", false),
            (None, _) => ("Launch", !self.launched && self.store_ready()),
            // Waiting for the title screen: Inject still works at once.
            (Some(_), false) => ("Inject", !(self.auto_inject && (self.inject_now || self.title_up()))),
        };
        if !button(ui, rect, text, enabled, false) {
            return;
        }
        match (&self.game, self.mod_in) {
            (_, true) => self.inject_more(),
            (None, _) => self.launch(),
            (Some(_), false) => {
                self.auto_inject = true;
                self.inject_now = true;

                self.last_inject_try = crate::ago(Duration::from_secs(60));
                self.game_seen = Some(crate::ago(Duration::from_secs(3600)));
            }
        }
    }

    /// Stand's store drop-down. The closed box; a click opens the list, drawn last over the rest.
    fn store_box(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let resp = ui.interact(rect, ui.id().with("store box"), Sense::click());
        let lit = resp.hovered() && hot(ui, rect);
        let p = ui.painter();
        p.rect(rect, CornerRadius::ZERO, BTN, Stroke::new(1.0, TEXT), StrokeKind::Inside);
        label(ui, pos2(rect.min.x + 4.0, rect.center().y), self.cfg.platform.name(), TEXT);
        let arrow = Rect::from_min_max(pos2(rect.max.x - 17.0, rect.min.y + 1.0), pos2(rect.max.x - 1.0, rect.max.y - 1.0));
        ui.painter().rect_filled(arrow, CornerRadius::ZERO, if lit { Color32::WHITE } else { ARROW_BOX });
        triangle(ui, arrow.center(), false);
        if resp.clicked() && hot(ui, rect) {
            let id = egui::Id::new("store open");
            let open = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            ui.ctx().data_mut(|d| d.insert_temp(id, !open));
        }
    }

    /// The open store list under the box, one row per store.
    fn store_list(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let id = egui::Id::new("store open");
        if !ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false)) {
            return;
        }
        let mut close = false;
        for (i, store) in Platform::ALL.into_iter().enumerate() {
            let row = Rect::from_min_size(pos2(rect.min.x, rect.max.y + i as f32 * rect.height()), rect.size());
            let resp = ui.interact(row, ui.id().with(("store row", i)), Sense::click());
            let lit = resp.hovered() && hot(ui, row);
            ui.painter().rect(row, CornerRadius::ZERO, if lit { PICK } else { BTN }, Stroke::new(1.0, TEXT), StrokeKind::Inside);
            label(ui, pos2(row.min.x + 4.0, row.center().y), store.name(), TEXT);
            if resp.clicked() && hot(ui, row) {
                if self.cfg.platform != store {
                    self.cfg.platform = store;
                    self.launch_error = None;
                    self.find_install();
                    let found = self.install.as_ref().map_or("game not found".to_string(), |i| i.dir.display().to_string());
                    self.say(format!("{}: {found}", store.name()));
                }
                close = true;
            }
        }
        // A click anywhere else closes it too.
        let area = Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.height() * (1 + Platform::ALL.len()) as f32));
        if close || (ui.input(|i| i.pointer.any_click()) && !ui.rect_contains_pointer(area)) {
            ui.ctx().data_mut(|d| d.insert_temp(id, false));
        }
    }

    /// Launch can start: for Ubisoft Connect the game must be found (Steam finds its own).
    fn store_ready(&self) -> bool {
        match self.cfg.platform {
            Platform::Steam => true,
            Platform::Ubisoft => self.install.is_some(),
        }
    }

    /// The whole refusal in a window of its own, in full, with the text to copy and the Bin folder
    /// a click away. Opens with each refusal; Close keeps it away until the next one (the next
    /// Inject that fails).
    fn refusal_window(&mut self, ui: &mut egui::Ui, full: &str) {
        let mut open = true;
        egui::Window::new("Not injected")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .fixed_size(vec2(SMALL[0] - 28.0, 0.0))
            .frame(egui::Frame::window(ui.style()).fill(BG).stroke(Stroke::new(1.0, LIST_EDGE)))
            .show(ui.ctx(), |ui| {
                ui.style_mut().visuals.override_text_color = Some(TEXT);
                ui.add(egui::Label::new(egui::RichText::new(full).font(font(ui.ctx()))).wrap());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy").clicked() {
                        ui.ctx().copy_text(full.to_string());
                    }
                    if ui.button("Open Bin folder").clicked() {
                        crate::folder::open(aurora_core::paths::dll().parent().unwrap_or(&aurora_core::paths::root()));
                    }
                    if ui.button("Close").clicked() {
                        open = false;
                    }
                });
            });
        if !open {
            self.close_refusal();
        }
    }

    fn status_line(&self) -> (String, Color32) {
        // A refusal is never on the line: its window is the only place it shows; the line says
        // nothing, before and after Close.
        if self.refusal.as_ref().is_some_and(|(short, _)| self.launch_error.as_deref() == Some(short.as_str())) {
            return (String::new(), TEXT);
        }
        if let Some(e) = &self.launch_error {
            return (e.clone(), Color32::from_rgb(235, 120, 100));
        }
        let text = match (&self.game, self.mod_in) {
            (None, _) if self.launched => format!("{} is starting the game.", self.cfg.platform.name()),
            (None, _) if !self.store_ready() => format!("Game not found in {}: see Advanced.", self.cfg.platform.name()),
            (None, _) => "Ready to inject; just start the game.".to_string(),
            // Only extras in (Bin's unticked, or another-named Aurora): still what is in.
            (Some(_), false) if !self.dlls_in.is_empty() && !self.auto_inject => return (self.status(), Color32::from_rgb(90, 200, 120)),
            (Some(_), false) if self.auto_inject && !self.inject_now && !self.title_up() => "Waiting for the game to start...".to_string(),
            (Some(g), false) if self.auto_inject => format!("Game found ({}), injecting...", g.api()),
            (Some(g), false) => format!("Game found ({}). Not injected.", g.api()),
            (Some(_), true) => return (self.status(), Color32::from_rgb(90, 200, 120)),
        };
        (text, TEXT)
    }

    /// Stand's Advanced side: the delay, the ↑ ↓ Add Remove row, the DLL list; then, under a line,
    /// what Options held: the game folder and where to hook (`game_and_hook`).
    fn advanced_pane(&mut self, ui: &mut egui::Ui, o: egui::Vec2) {
        let at = |x: f32, y: f32, w: f32, h: f32| r(x, y, w, h).translate(o);
        label(ui, at(235.0, 24.5, 0.0, 0.0).min, "Automatic Injection Delay (Seconds):", TEXT);
        // The number box with its two little arrow buttons.
        let field = at(415.0, 15.0, 47.0, 20.0);
        ui.painter().rect(field, CornerRadius::ZERO, BG, Stroke::new(1.0, FIELD_EDGE), StrokeKind::Inside);
        label(ui, pos2(field.min.x + 3.0, field.center().y), &self.cfg.inject_delay.to_string(), TEXT);
        let up = Rect::from_min_max(pos2(field.max.x - 17.0, field.min.y + 1.0), pos2(field.max.x - 1.0, field.center().y));
        let down = Rect::from_min_max(pos2(field.max.x - 17.0, field.center().y), pos2(field.max.x - 1.0, field.max.y - 1.0));
        for (rect, step, is_up) in [(up, 1i32, true), (down, -1i32, false)] {
            let resp = ui.interact(rect, ui.id().with(("spin", is_up)), Sense::click());
            ui.painter().rect(rect, CornerRadius::ZERO, ARROW_BOX, Stroke::new(1.0, FIELD_EDGE), StrokeKind::Inside);
            triangle(ui, rect.center(), is_up);
            if resp.clicked() && hot(ui, rect) {
                self.cfg.inject_delay = (self.cfg.inject_delay as i32 + step).clamp(0, 120) as u32;
            }
        }
        let wheel = ui.interact(field, ui.id().with("spin field"), Sense::hover());
        if wheel.hovered() && hot(ui, field) {
            // One step per wheel notch.
            let dy: f32 = ui.input(|i| {
                i.events.iter().map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y.signum() } else { 0.0 }).sum()
            });
            if dy != 0.0 {
                self.cfg.inject_delay = (self.cfg.inject_delay as i32 + dy.signum() as i32).clamp(0, 120) as u32;
            }
        }
        // ↑ and ↓ move the picked DLL; the mod's own stays first. Rows count the mod's as 0.
        let n = self.cfg.extra_dlls.len();
        // A move or an Add scrolls the list to the picked row; the wheel is free otherwise.
        let mut reveal = false;
        for (x, glyph, delta) in [(533.0, "↑", -1i32), (551.0, "↓", 1i32)] {
            let rect = at(x, 15.0, 12.0, 18.0);
            let resp = ui.interact(rect, ui.id().with(("move", glyph)), Sense::click());
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, glyph, font(ui.ctx()), if resp.hovered() && hot(ui, rect) { DIM } else { TEXT });
            if resp.clicked()
                && hot(ui, rect)
                && let Some(i) = self.dll_pick.filter(|&i| i > 0)
            {
                let j = i as i32 + delta;
                if (1..=n as i32).contains(&j) {
                    self.cfg.extra_dlls.swap(i - 1, j as usize - 1);
                    self.dll_pick = Some(j as usize);
                    reveal = true;
                }
            }
        }
        if button(ui, at(567.0, 12.0, 47.0, 23.0), "Add", true, false)
            && let Some(path) = pick_dll()
            && !self.cfg.extra_dlls.iter().any(|d| d.path.eq_ignore_ascii_case(&path))
        {
            self.cfg.extra_dlls.push(ExtraDll { path, on: true });
            self.dll_pick = Some(self.cfg.extra_dlls.len());
            reveal = true;
        }
        // The mod's own row can't be removed, only unticked.
        let removable = self.dll_pick.is_some_and(|i| (1..=n).contains(&i));
        if button(ui, at(620.0, 12.0, 65.0, 23.0), "Remove", removable, false)
            && let Some(i) = self.dll_pick.take()
        {
            self.cfg.extra_dlls.remove(i - 1);
        }
        let list = at(235.0, 41.0, 450.0, 80.0);
        ui.painter().rect(list, CornerRadius::ZERO, BG, Stroke::new(1.0, LIST_EDGE), StrokeKind::Inside);
        let row_h = 17.0;
        let rows = std::iter::once((format!("{DLL_FILE}   ({})", built_text()), self.cfg.aurora_on))
            .chain(self.cfg.extra_dlls.iter().map(|d| {
                let name = std::path::Path::new(&d.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| d.path.clone());
                (name, d.on)
            }))
            .collect::<Vec<_>>();
        // As many rows as fit (4); more scroll with the mouse wheel over the list, and a thin bar
        // on the right shows where. After Add or a move the picked row is scrolled into view.
        let fit = ((list.height() - 2.0) / row_h).floor() as usize;
        let total = rows.len();
        let last_top = total.saturating_sub(fit);
        if reveal && let Some(p) = self.dll_pick {
            if p < self.dll_top {
                self.dll_top = p;
            } else if p >= self.dll_top + fit {
                self.dll_top = p + 1 - fit;
            }
        }
        // Asked of Windows, not egui's hover: the rows sense clicks and sit on top of the list, so
        // egui never counts the list as hovered over a row. Not while the hook drop-down is open
        // over it.
        let hook_open = ui.ctx().data(|d| d.get_temp::<bool>(egui::Id::new("hook open")).unwrap_or(false));
        if hot(ui, list) && !hook_open {
            let dy: f32 = ui.input(|i| i.events.iter().map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y.signum() } else { 0.0 }).sum());
            if dy > 0.0 {
                self.dll_top = self.dll_top.saturating_sub(1);
            } else if dy < 0.0 {
                self.dll_top += 1;
            }
        }
        self.dll_top = self.dll_top.min(last_top);
        let rows_w = if total > fit { list.width() - 8.0 } else { list.width() - 2.0 };
        if total > fit {
            let track = Rect::from_min_max(pos2(list.max.x - 6.0, list.min.y + 2.0), pos2(list.max.x - 2.0, list.max.y - 2.0));
            let h = track.height() * fit as f32 / total as f32;
            let y = track.min.y + (track.height() - h) * self.dll_top as f32 / last_top.max(1) as f32;
            ui.painter().rect_filled(Rect::from_min_size(pos2(track.min.x, y), vec2(track.width(), h)), CornerRadius::same(2), BOX);
        }
        let top = self.dll_top;
        for (i, (name, on)) in rows.into_iter().enumerate().skip(top).take(fit) {
            let row = Rect::from_min_size(list.min + vec2(1.0, 1.0 + (i - top) as f32 * row_h), vec2(rows_w, row_h));
            if self.dll_pick == Some(i) {
                ui.painter().rect_filled(row, CornerRadius::ZERO, PICK);
            }
            let tick = r(row.min.x, row.min.y + 1.0, 13.0, 13.0);
            list_tick(ui, tick.min, on);
            label(ui, pos2(row.min.x + 18.0, row.center().y), &name, TEXT);
            let resp = ui.interact(row, ui.id().with(("dll row", i)), Sense::click());
            // The tick is its own widget on top of the row, so a click on it always toggles.
            let tick_hit = tick.expand(2.0);
            let tick_resp = ui.interact(tick_hit, ui.id().with(("dll tick", i)), Sense::click());
            let tick_clicked = tick_resp.clicked() && hot(ui, tick_hit);
            let row_clicked = resp.clicked() && hot(ui, row);
            if tick_clicked {
                match i {
                    0 => self.cfg.aurora_on = !self.cfg.aurora_on,
                    k => self.cfg.extra_dlls[k - 1].on = !self.cfg.extra_dlls[k - 1].on,
                }
            }
            if row_clicked || tick_clicked {
                self.dll_pick = Some(i);
            }
            if i > 0 {
                resp.on_hover_text(&self.cfg.extra_dlls[i - 1].path);
            }
        }
        ui.painter().line_segment([at(235.0, 130.0, 0.0, 0.0).min, at(685.0, 130.0, 0.0, 0.0).min], Stroke::new(1.0, SEP));
        self.game_and_hook(ui, o);
    }

    /// The changelog in the list's place.
    fn changelog_pane(&mut self, ui: &mut egui::Ui, o: egui::Vec2) {
        let list = r(235.0, 12.0, 450.0, 151.0).translate(o);
        ui.painter().rect(list, CornerRadius::ZERO, BG, Stroke::new(1.0, LIST_EDGE), StrokeKind::Inside);
        let inner = list.shrink(5.0);
        let f = font(ui.ctx());
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            egui::ScrollArea::vertical().id_salt("changelog").auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                for line in CHANGELOG.lines() {
                    match line.strip_prefix("## ") {
                        Some(h) => {
                            ui.add_space(3.0);
                            ui.label(egui::RichText::new(h).font(f.clone()).color(TEXT).strong());
                        }
                        None if !line.trim().is_empty() => {
                            ui.label(egui::RichText::new(line.replacen("- ", "• ", 1)).font(f.clone()).color(TEXT));
                        }
                        None => {}
                    }
                }
            });
        });
    }

    /// Under Advanced's DLL list: the picked store's game folder with Locate, where the in-game
    /// drawing hooks as a drop-down with one grey line about the pick, and where the mod in the
    /// game hooked. Saved at once; the DLL reads the hook at inject.
    fn game_and_hook(&mut self, ui: &mut egui::Ui, o: egui::Vec2) {
        let at = |x: f32, y: f32, w: f32, h: f32| r(x, y, w, h).translate(o);
        // The picked store's game folder: found by itself, or picked here.
        let game = match &self.install {
            Some(i) => format!("Game: {}", i.dir.display()),
            None => format!("Game: not found in {}", self.cfg.platform.name()),
        };
        let game = if game.chars().count() > 58 { format!("{}...", game.chars().take(55).collect::<String>()) } else { game };
        label(ui, at(240.0, 149.0, 0.0, 0.0).min, &game, if self.install.is_some() { TEXT } else { Color32::from_rgb(235, 120, 100) });
        if button(ui, at(607.0, 140.0, 68.0, 19.0), "Locate...", true, false)
            && let Some(dir) = platform::pick_folder()
        {
            if platform::is_game_dir(&dir) {
                let p = self.cfg.platform;
                self.cfg.set_game_dir(p, Some(dir));
                self.find_install();
                self.launch_error = None;
            } else {
                self.say(format!("{} has no GRB.exe or GRB_vulkan.exe", dir.display()));
                self.launch_error = Some("That folder has no Breakpoint exe in it.".into());
            }
        }
        label(ui, at(240.0, 175.0, 0.0, 0.0).min, "Where to hook:", TEXT);
        let rect = at(HOOK_BOX[0], HOOK_BOX[1], HOOK_BOX[2], HOOK_BOX[3]);
        let resp = ui.interact(rect, ui.id().with("hook box"), Sense::click());
        let lit = resp.hovered() && hot(ui, rect);
        let p = ui.painter();
        p.rect(rect, CornerRadius::ZERO, BTN, Stroke::new(1.0, TEXT), StrokeKind::Inside);
        label(ui, pos2(rect.min.x + 4.0, rect.center().y), hook_name(self.cfg.hook), TEXT);
        let arrow = Rect::from_min_max(pos2(rect.max.x - 17.0, rect.min.y + 1.0), pos2(rect.max.x - 1.0, rect.max.y - 1.0));
        ui.painter().rect_filled(arrow, CornerRadius::ZERO, if lit { Color32::WHITE } else { ARROW_BOX });
        triangle(ui, arrow.center(), false);
        if resp.clicked() && hot(ui, rect) {
            let id = egui::Id::new("hook open");
            let open = ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            ui.ctx().data_mut(|d| d.insert_temp(id, !open));
        }
        // One grey line beside it: what the pick does, or that it waits for the next inject.
        let note = if self.mod_in && self.hook_differs() { "Takes effect at the next inject." } else { hook_line(self.cfg.hook, self.dx11()) };
        label(ui, pos2(rect.max.x + 10.0, rect.center().y), note, NOTE);
        let line = match (self.mod_in, &self.hook_line) {
            (false, _) => "Not injected yet".to_string(),
            // A long Failed line is cut to fit the pane.
            (true, Some(l)) if l.chars().count() > 72 => format!("Hooked at: {}...", l.chars().take(69).collect::<String>()),
            (true, Some(l)) => format!("Hooked at: {l}"),
            (true, None) => "Hooked at: not known".to_string(),
        };
        label(ui, at(240.0, 201.0, 0.0, 0.0).min, &line, TEXT);
    }

    /// The open hook list, one row per choice, opening upward over the DLL list (the window ends
    /// just under the box). Drawn last, like the store list; a click elsewhere closes it.
    fn hook_list(&mut self, ui: &mut egui::Ui, rect: Rect) {
        use settings::Hook;
        let id = egui::Id::new("hook open");
        if !ui.ctx().data(|d| d.get_temp::<bool>(id).unwrap_or(false)) {
            return;
        }
        let dx11 = self.dx11();
        let all = [Hook::Auto, Hook::Driver, Hook::Off];
        let mut close = false;
        for (i, hook) in all.into_iter().enumerate() {
            let row = Rect::from_min_size(pos2(rect.min.x, rect.min.y - (all.len() - i) as f32 * rect.height()), rect.size());
            let resp = ui.interact(row, ui.id().with(("hook row", i)), Sense::click());
            let lit = resp.hovered() && hot(ui, row);
            ui.painter().rect(row, CornerRadius::ZERO, if lit { PICK } else { BTN }, Stroke::new(1.0, TEXT), StrokeKind::Inside);
            // GPU driver skips Vulkan layers: on a DirectX 11 game the mod hooks as Auto.
            label(ui, pos2(row.min.x + 4.0, row.center().y), hook_name(hook), if dx11 && hook == Hook::Driver { DIM } else { TEXT });
            if resp.clicked() && hot(ui, row) {
                if self.cfg.hook != hook {
                    self.cfg.hook = hook;
                    self.keep_settings();
                }
                close = true;
            }
        }
        let area = Rect::from_min_max(pos2(rect.min.x, rect.min.y - all.len() as f32 * rect.height()), rect.max);
        if close || (ui.input(|i| i.pointer.any_click()) && !ui.rect_contains_pointer(area)) {
            ui.ctx().data_mut(|d| d.insert_temp(id, false));
        }
    }

    /// The running game is the DirectX 11 build.
    fn dx11(&self) -> bool {
        self.game.as_ref().is_some_and(|g| !g.vulkan())
    }

    /// The pick is not what the DLL in the game runs with. Known when we injected it; else guessed
    /// from its hook.txt line (a GPU driver or DXGI line fits both Auto and GPU driver).
    fn hook_differs(&self) -> bool {
        hook_differs(self.cfg.hook, self.hook_injected, self.hook_line.as_deref(), self.dx11())
    }

    /// After the mod's DLL: load the ticked extra DLLs, in list order, once per game. The Add rule:
    /// one that is Aurora (`sign::is_aurora`: named aurora.dll, or a card or stamp inside) goes in
    /// the gatekeeper's way, checked, matched and asked like Bin's (`load_aurora`), and never
    /// beside another Aurora: with Bin's DLL in, or once one Add Aurora is in, the next is skipped
    /// and the log says so. Other DLLs are loaded unchecked, as before. Inject while the mod is in:
    /// only the ticked DLLs not in the game yet. Bin's aurora.dll is not touched, and an Aurora
    /// copy in the list is skipped (`inject_extras`, never two).
    fn inject_more(&mut self) {
        self.clear_refusal();
        let before = self.dlls_in.len();
        self.inject_extras();
        self.count_dlls();
        if self.dlls_in.len() == before {
            self.say("nothing new to inject: the ticked DLLs are in the game already, or were not loaded (see the lines above)");
        }
    }

    pub(crate) fn inject_extras(&mut self) {
        let Some(p) = self.proc.clone() else { return };
        // Any Aurora in the game, under any name, not only Bin's.
        let mut aurora_in = self.aurora_running(&p).is_some();
        for d in self.cfg.extra_dlls.clone().into_iter().filter(|d| d.on) {
            let path = std::path::PathBuf::from(&d.path);
            let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            if name.is_empty() || p.has_module(&name) {
                continue;
            }
            if crate::sign::is_aurora(&path) {
                if aurora_in {
                    self.say(format!("{name} not loaded: it is Aurora, and an Aurora is already in the game (never two)"));
                    continue;
                }
                aurora_in = self.load_aurora(&p, &path, false);
                continue;
            }
            match p.inject(&path) {
                Ok(()) => {
                    self.say(format!("loaded {name}"));
                    self.note_loaded(&name, false);
                }
                Err(e) => self.say(format!("{name}: {e}")),
            }
        }
    }
}

/// Does the pick (`want`) differ from what the DLL in the game runs with? `injected`: the pick at
/// our inject, if we did it; else `line` (its hook.txt) is the guide. On DirectX 11 GPU driver
/// hooks as Auto, so those two count as the same there.
fn hook_differs(want: settings::Hook, injected: Option<settings::Hook>, line: Option<&str>, dx11: bool) -> bool {
    use settings::Hook;
    let same = |h: Hook| if dx11 && h == Hook::Driver { Hook::Auto } else { h };
    let want = same(want);
    if let Some(h) = injected {
        return same(h) != want;
    }
    match line {
        Some("Off") => want != Hook::Off,
        Some(l) if l.starts_with("Steam overlay") || l.starts_with("Other layer") => want != Hook::Auto,
        // "GPU driver (...)", "DXGI (...)" or "Not yet": fits Auto and GPU driver.
        Some(l) if !l.starts_with("Failed") => want == Hook::Off,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::hook_differs;
    use crate::settings::Hook;

    #[test]
    fn hook_differs_vulkan_as_before() {
        assert!(hook_differs(Hook::Driver, Some(Hook::Auto), None, false));
        assert!(!hook_differs(Hook::Auto, Some(Hook::Auto), None, false));
        assert!(hook_differs(Hook::Driver, None, Some("Steam overlay (SteamOverlayVulkanLayer64.dll)"), false));
        assert!(!hook_differs(Hook::Driver, None, Some("GPU driver (amdvlk64.dll)"), false));
        assert!(hook_differs(Hook::Off, None, Some("GPU driver (amdvlk64.dll)"), false));
        assert!(hook_differs(Hook::Auto, None, Some("Off"), false));
        assert!(!hook_differs(Hook::Auto, None, Some("Failed: no GPU"), false));
    }

    #[test]
    fn hook_differs_dx11() {
        // GPU driver is Auto on DirectX 11.
        assert!(!hook_differs(Hook::Driver, Some(Hook::Auto), None, true));
        assert!(!hook_differs(Hook::Driver, None, Some("Steam overlay (GameOverlayRenderer64.dll)"), true));
        assert!(!hook_differs(Hook::Auto, None, Some("DXGI (dxgi.dll)"), true));
        assert!(!hook_differs(Hook::Driver, None, Some("DXGI (dxgi.dll)"), true));
        assert!(hook_differs(Hook::Off, None, Some("DXGI (dxgi.dll)"), true));
        assert!(hook_differs(Hook::Off, Some(Hook::Driver), None, true));
    }
}
