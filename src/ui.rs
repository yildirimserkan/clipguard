//! Terminal-style picker window.
//!
//!   ↑/↓ select · Enter paste · Shift+Enter copy only · Ctrl+P pin · Shift+Del delete · Esc hide
//!   Hotkey toggles the window: Ctrl+` (Windows/Linux), Cmd+Shift+C (macOS)

use crate::engine::{Core, ItemView, SECRET_TTL_SECS};
use crate::paste;
use crate::tray::{self, Action, Tray};
use crate::autostart;
use crate::storage::now_ms;
use eframe::egui::{self, Color32, Key, Modifiers, RichText};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use global_hotkey::hotkey::{Code, HotKey, Modifiers as HkMods};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

// ---- palette (terminal)
const BG: Color32 = Color32::from_rgb(0x0b, 0x0e, 0x14);
const BG_SEL: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
const FG: Color32 = Color32::from_rgb(0xc9, 0xd1, 0xd9);
const DIM: Color32 = Color32::from_rgb(0x6e, 0x76, 0x81);
const GREEN: Color32 = Color32::from_rgb(0x7e, 0xe7, 0x87);
const AMBER: Color32 = Color32::from_rgb(0xf0, 0xb4, 0x29);
const BLUE: Color32 = Color32::from_rgb(0x79, 0xc0, 0xff);

const PREVIEW_CHARS: usize = 72;
const MAX_ROWS: usize = 500; // render cap; search narrows further

/// Hotkeys tried in order; the first one the OS lets us register wins.
/// (Ctrl+` is Ditto's default: if Ditto is still running it holds that key.)
fn hotkey_candidates() -> Vec<(HotKey, &'static str)> {
    #[cfg(target_os = "macos")]
    return vec![
        (HotKey::new(Some(HkMods::META | HkMods::SHIFT), Code::KeyC), "Cmd+Shift+C"),
        (HotKey::new(Some(HkMods::CONTROL | HkMods::SHIFT), Code::KeyV), "Ctrl+Shift+V"),
    ];
    #[cfg(not(target_os = "macos"))]
    return vec![
        (HotKey::new(Some(HkMods::CONTROL), Code::Backquote), "Ctrl+`"),
        (HotKey::new(Some(HkMods::CONTROL | HkMods::SHIFT), Code::Backquote), "Ctrl+Shift+`"),
        (HotKey::new(Some(HkMods::META | HkMods::SHIFT), Code::KeyV), "Win+Shift+V"),
    ];
}

/// Registers the first free candidate. Returns its label, or None.
fn register_hotkey(manager: &GlobalHotKeyManager) -> Option<&'static str> {
    for (hk, label) in hotkey_candidates() {
        match manager.register(hk) {
            Ok(()) => return Some(label),
            Err(e) => eprintln!("[ui] hotkey {label} unavailable: {e}"),
        }
    }
    None
}

pub struct ClipApp {
    core: Arc<Mutex<Core>>,
    query: String,
    selected: usize,
    filtered: Vec<usize>,
    cache_key: Option<(String, u64)>,
    matcher: SkimMatcherV2,
    toggle_request: Arc<AtomicBool>,
    visible: bool,
    hotkey: Option<&'static str>,
    _hotkeys: Option<GlobalHotKeyManager>,
    tray: Option<Tray>,
    tray_queue: Arc<Mutex<Vec<Action>>>,
}

impl ClipApp {
    pub fn new(cc: &eframe::CreationContext<'_>, core: Arc<Mutex<Core>>, ctx_slot: Arc<OnceLock<egui::Context>>) -> Self {
        apply_style(&cc.egui_ctx);
        let _ = ctx_slot.set(cc.egui_ctx.clone());

        // Hotkey manager must live on the main (event-loop) thread.
        let toggle_request = Arc::new(AtomicBool::new(false));
        let manager = GlobalHotKeyManager::new().ok();
        let hotkey = manager.as_ref().and_then(register_hotkey);
        match hotkey {
            Some(label) => println!("[clipguard] {label} toggles the window"),
            None => eprintln!("[ui] no global hotkey available (Wayland, or all taken); use the tray icon"),
        }
        {
            let flag = toggle_request.clone();
            let ctx = cc.egui_ctx.clone();
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state == HotKeyState::Pressed {
                    flag.store(true, Ordering::SeqCst);
                    ctx.request_repaint(); // wakes `logic` even while hidden
                }
            }));
        }

        let tray_queue = Arc::new(Mutex::new(Vec::new()));
        let tray = tray::build(&cc.egui_ctx, tray_queue.clone(), hotkey.unwrap_or("tray only"));

        Self {
            core,
            query: String::new(),
            selected: 0,
            filtered: Vec::new(),
            cache_key: None,
            matcher: SkimMatcherV2::default().ignore_case(),
            toggle_request,
            visible: true,
            hotkey,
            _hotkeys: manager,
            tray,
            tray_queue,
        }
    }

    pub fn mark_hidden(&mut self) {
        self.visible = false;
    }

    fn show(&mut self, ctx: &egui::Context) {
        paste::show_app();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        self.visible = true;
        self.query.clear();
        self.selected = 0;
    }

    fn hide(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        paste::hide_app();
        self.visible = false;
    }

    /// Recompute filtered indices when the query or the data changed.
    fn refilter(&mut self, items: &[ItemView], version: u64) {
        let key = (self.query.clone(), version);
        if self.cache_key.as_ref() == Some(&key) {
            return;
        }
        self.cache_key = Some(key);
        let q = self.query.trim();
        if q.is_empty() {
            self.filtered = (0..items.len().min(MAX_ROWS)).collect();
        } else {
            // Secrets are searchable by their kind ("github", "jwt"…), never by content.
            let mut scored: Vec<(i64, usize)> = items
                .iter()
                .enumerate()
                .filter_map(|(i, it)| {
                    let hay: &str = match it.secret { Some(kind) => kind, None => &it.text };
                    self.matcher.fuzzy_match(hay, q).map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.filtered = scored.into_iter().take(MAX_ROWS).map(|(_, i)| i).collect();
        }
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

impl eframe::App for ClipApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut toggle = self.toggle_request.swap(false, Ordering::SeqCst);
        let actions: Vec<Action> = std::mem::take(&mut *self.tray_queue.lock().unwrap_or_else(|p| p.into_inner()));
        for a in actions {
            match a {
                Action::Toggle => toggle = !toggle,
                Action::PauseClicked => {
                    let on = self.tray.as_ref().map(|t| t.pause_checked()).unwrap_or(false);
                    self.core.lock().unwrap_or_else(|p| p.into_inner()).paused = on;
                    ctx.request_repaint();
                }
                Action::AutostartClicked => {
                    if let Some(t) = &self.tray {
                        let actual = autostart::set(t.autostart_checked());
                        t.set_autostart_checked(actual);
                    }
                }
                Action::Quit => std::process::exit(0),
            }
        }
        if toggle {
            if self.visible { self.hide(ctx) } else { self.show(ctx) }
        }
        // Closing the window hides it; the app keeps guarding the clipboard.
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide(ctx);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // ---- keys (consumed before the text field sees them)
        let (up, down, enter, shift_enter, pin, del, esc, quit) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::SHIFT, Key::Enter),
                i.consume_key(Modifiers::COMMAND, Key::P),
                i.consume_key(Modifiers::SHIFT, Key::Delete),
                i.consume_key(Modifiers::NONE, Key::Escape),
                i.consume_key(Modifiers::COMMAND, Key::Q),
            )
        });
        if quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            std::process::exit(0);
        }
        if esc {
            self.hide(&ctx);
            return;
        }

        let core = self.core.clone();
        let mut core = core.lock().unwrap_or_else(|p| p.into_inner());
        let version = core.version;
        self.refilter(&core.items, version);

        let n = self.filtered.len();
        if down && n > 0 { self.selected = (self.selected + 1).min(n - 1); }
        if up { self.selected = self.selected.saturating_sub(1); }
        let sel_id = self.filtered.get(self.selected).map(|&i| core.items[i].id);

        if let Some(id) = sel_id {
            if (enter || shift_enter) && core.copy(id) {
                drop(core);
                self.hide(&ctx);
                if enter { paste::paste_into_previous_app(); }
                return;
            }
            if pin { core.toggle_pin(id); }
            if del { core.delete(id); }
        }
        let total = core.items.len();
        let paused = core.paused;

        egui::Frame::new().fill(BG).inner_margin(12.0).show(ui, |ui| {
            // ---- prompt
            ui.horizontal(|ui| {
                // The "clipguard ❯" label is the window's drag handle (no title bar).
                let handle = ui
                    .add(egui::Label::new(RichText::new("clipguard ❯").color(GREEN).strong()).sense(egui::Sense::drag()))
                    .on_hover_cursor(egui::CursorIcon::Grab);
                if handle.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .hint_text(RichText::new("search…").color(DIM))
                    .frame(egui::Frame::NONE)
                    .desired_width(f32::INFINITY)
                    .text_color(FG);
                let r = ui.add(edit);
                // Keep typing going to the search box, but only while our window is
                // focused: requesting focus on an unfocused window repaints every frame.
                let window_focused = ui.ctx().input(|i| i.viewport().focused.unwrap_or(false));
                if window_focused && !r.has_focus() { r.request_focus(); }
                if r.changed() { self.selected = 0; }
            });
            ui.add_space(4.0);
            ui.separator();

            // ---- list
            let footer_h = 30.0;
            let list_h = ui.available_height() - footer_h;
            egui::ScrollArea::vertical()
                .max_height(list_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.filtered.is_empty() {
                        let msg = if total == 0 { "history is empty — copy something" } else { "no match" };
                        ui.label(RichText::new(msg).color(DIM));
                    }
                    let now = now_ms();
                    let mut clicked: Option<(usize, bool)> = None;
                    for (row, &idx) in self.filtered.iter().enumerate() {
                        let it = &core.items[idx];
                        let is_sel = row == self.selected;
                        let resp = egui::Frame::new()
                            .fill(if is_sel { BG_SEL } else { BG })
                            .inner_margin(egui::Margin::symmetric(6, 3))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| row_ui(ui, it, is_sel, now));
                            })
                            .response
                            .interact(egui::Sense::click());
                        if is_sel && (up || down) { resp.scroll_to_me(None); }
                        if resp.double_clicked() { clicked = Some((row, true)); }
                        else if resp.clicked() { clicked = Some((row, false)); }
                    }
                    if let Some((row, paste_now)) = clicked {
                        self.selected = row;
                        if paste_now {
                            let id = core.items[self.filtered[row]].id;
                            if core.copy(id) {
                                self.toggle_request.store(true, Ordering::SeqCst);
                                paste::paste_into_previous_app();
                                ctx.request_repaint();
                            }
                        }
                    }
                });

            // ---- footer (also a drag handle)
            ui.separator();
            let footer = ui.horizontal(|ui| {
                let keys = "↑↓ select · enter paste · shift+enter copy · ^P pin · shift+del delete · esc hide";
                ui.label(RichText::new(keys).color(DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match self.hotkey {
                        Some(hk) => ui.label(RichText::new(hk).color(GREEN)),
                        None => ui.label(RichText::new("no hotkey: use tray").color(AMBER)),
                    };
                    ui.label(RichText::new(format!("{total} items · aes-256-gcm ·")).color(DIM));
                    if paused {
                        ui.label(RichText::new("capture paused ·").color(AMBER));
                    }
                });
            });
            let drag = ui.interact(footer.response.rect, ui.scope_id().with("footer_drag"), egui::Sense::drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        });
    }
}

fn row_ui(ui: &mut egui::Ui, it: &ItemView, is_sel: bool, now: i64) {
    let cursor = if is_sel { "▸" } else { " " };
    ui.label(RichText::new(cursor).color(GREEN));

    if let Some(kind) = it.secret {
        // Countdown needs a redraw once per second, only while a secret is on screen.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        // Secrets are never rendered; only their kind and remaining lifetime.
        let left = (SECRET_TTL_SECS * 1000 - (now - it.last_used_at)).max(0) / 1000;
        ui.label(RichText::new("●●●●●●●●").color(AMBER));
        ui.label(RichText::new(kind).color(AMBER));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(format!("{}:{:02}", left / 60, left % 60)).color(AMBER));
        });
        return;
    }

    if it.pinned {
        ui.label(RichText::new("◆").color(BLUE));
    }
    let (preview, extra_lines) = preview(&it.text);
    ui.add(egui::Label::new(RichText::new(preview).color(if is_sel { FG } else { FG.gamma_multiply(0.85) })).truncate());
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.label(RichText::new(age(now - it.last_used_at)).color(DIM));
        if extra_lines > 0 {
            ui.label(RichText::new(format!("+{extra_lines}")).color(DIM));
        }
    });
}

/// First non-empty line, whitespace collapsed, truncated.
fn preview(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let first = lines.first().copied().unwrap_or("");
    let collapsed: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s: String = collapsed.chars().take(PREVIEW_CHARS).collect();
    if collapsed.chars().count() > PREVIEW_CHARS { s.push('…'); }
    (s, lines.len().saturating_sub(1))
}

fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn apply_style(ctx: &egui::Context) {
    // Monospace everywhere.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(mono) = fonts.families.get(&egui::FontFamily::Monospace).cloned() {
        fonts.families.insert(egui::FontFamily::Proportional, mono);
    }
    ctx.set_fonts(fonts);

    // Labels are not text-selectable: dragging the window must not highlight text.
    ctx.all_styles_mut(|style| style.interaction.selectable_labels = false);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = BG;
    v.override_text_color = Some(FG);
    v.selection.bg_fill = Color32::from_rgb(0x23, 0x4f, 0x2b);
    v.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(0x21, 0x26, 0x2d);
    ctx.set_visuals(v);
}

#[cfg(test)]
mod tests {
    use super::{age, preview};

    #[test]
    fn preview_collapses_and_counts_lines() {
        let (p, extra) = preview("\n  SELECT  *\n FROM users\n\nWHERE 1=1");
        assert_eq!(p, "SELECT *");
        assert_eq!(extra, 2);
        let long = "x".repeat(100);
        assert!(preview(&long).0.ends_with('…'));
    }

    #[test]
    fn age_units() {
        assert_eq!(age(5_000), "5s");
        assert_eq!(age(120_000), "2m");
        assert_eq!(age(7_200_000), "2h");
        assert_eq!(age(172_800_000), "2d");
    }
}
