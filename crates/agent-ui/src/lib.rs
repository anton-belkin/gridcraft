//! The agent layer's UI (a GridCraft fork feature), plugged in through `gridcraft_ui_egui::ext`:
//!
//! - topic markers on cells (numbered circles in the status colour), client data, marked inputs;
//!   heat maps of the agent's confidence and of each input's impact;
//! - hovering a number shows its sensitivity to its inputs (a small tornado chart);
//! - the Agent pane: the selection's topics (status, places, notes, discussion) and, folded, where
//!   the number comes from, sensitivity, alternatives and risk; an overview with sign-off; history;
//! - the Agent tab on the ribbon: the pane and what waits, client data, the context (what the agent
//!   knows: only the document, your Cowork chat, or Claude Code), the heat maps, sharing;
//! - context-menu items: Ask the Agent, Where Does This Come From, Mark as Client Data;
//! - voice questions (native): hold ⌥ or click the microphone, speak, click the cells you mean.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod context;
mod overlay;
mod pane;
mod ribbon;
#[cfg(not(target_arch = "wasm32"))]
pub mod voice;

use std::sync::OnceLock;

use egui::{Color32, Ui, vec2};
use gridcraft_agent::anchor;
use gridcraft_agent::layer::{self, QuestionStatus};
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::ext::{CellGeometry, UiExtension};
use gridcraft_ui_egui::theme::Tokens;
use serde_json::json;

static SERVER: OnceLock<String> = OnceLock::new();
static STDIO_ENTRY: OnceLock<serde_json::Value> = OnceLock::new();

/// The `claude_desktop_config.json` entry that starts this app in stdio mode (kept for the hosts
/// that set it; `agent.connectCowork` builds its own).
pub fn set_stdio_entry(entry: serde_json::Value) {
    let _ = STDIO_ENTRY.set(entry);
}

/// Registers the agent UI (call once, on the UI thread, before the first frame).
pub fn install() {
    gridcraft_agent::install();
    // Once: a second registration would run every hook twice.
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        #[cfg(not(target_arch = "wasm32"))]
        gridcraft_engine::ext::register_commands(voice::SPECS);
        gridcraft_ui_egui::ext::register(Box::new(AgentUi::default()));
    });
}

/// Tells the UI where the agent server listens (shown on the Agent tab).
pub fn set_server_url(url: String) {
    let _ = SERVER.set(url);
}

/// The app's folder picker (desktop: a native dialog).
type PickFolder = Box<dyn Fn() -> Option<String> + Send + Sync>;
static PICK_FOLDER: OnceLock<PickFolder> = OnceLock::new();

/// Context → Claude Code → Project folder → Choose folder… asks this.
pub fn set_pick_folder(pick: PickFolder) {
    let _ = PICK_FOLDER.set(pick);
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn pick_folder() -> Option<String> {
    PICK_FOLDER.get().and_then(|f| f())
}

static REQUEST: std::sync::Mutex<Option<(bool, String, String)>> = std::sync::Mutex::new(None);
static GOTO: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);

/// Selects a defined name or a place (`Sheet1!B5`) once a workbook is open (`--goto`).
pub fn set_goto(target: String) {
    if let Ok(mut g) = GOTO.lock() {
        *g = Some((target, 0));
    }
}

/// Opens (or closes) the Agent pane on a tab (`selection`, `overview`, `history`, `file`; `variants` and
/// `risk` open the selection's analysis)
/// with a heat map (`off`, `confidence`, `impact`), from the next frame.
pub fn request_pane(open: bool, tab: &str, heat: &str) {
    if let Ok(mut r) = REQUEST.lock() {
        *r = Some((open, tab.to_string(), heat.to_string()));
    }
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn server_url() -> Option<&'static str> {
    SERVER.get().map(String::as_str)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Selection,
    Overview,
    History,
    /// The file's own context (the button beside the tabs).
    FileContext,
}

/// What the Overview lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Filter {
    #[default]
    All,
    Status(layer::Status),
    Unread,
    Working,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Heat {
    #[default]
    Off,
    Confidence,
    Impact,
}

#[derive(Default)]
pub(crate) struct AgentUi {
    pub open: bool,
    pub tab: Tab,
    pub heat: Heat,
    /// Draft question for the selection, and whether its box should take focus.
    pub draft: String,
    pub focus_draft: bool,
    /// Where the draft goes: a topic id, or a new topic (`None`: the selection's first topic).
    pub ask_into: Option<String>,
    pub ask_new: bool,
    /// A topic opened from a marker, the overview or a link, shown first and scrolled to.
    pub focus_topic: Option<String>,
    /// A note to open in its topic.
    pub highlight_note: Option<String>,
    pub filter: Filter,
    /// The web export takes the discussion along.
    pub share_discussion: bool,
    /// The proposal option previewed on the sheet: (topic, option).
    pub preview: Option<(String, String)>,
    /// The link under the pointer in the pane: its target is highlighted on the sheet.
    pub hover_link: Option<String>,
    pub overlay: overlay::State,
    pub pane: pane::State,
    #[cfg(not(target_arch = "wasm32"))]
    pub voice: voice::Voice,
    #[cfg(not(target_arch = "wasm32"))]
    pub context: context::State,
}

/// The agent at work, and an answer not yet read.
pub(crate) const BLUE: Color32 = Color32::from_rgb(0x1A, 0x73, 0xE8);

/// The agent layer's colours (not in the theme: they mean the same in light and dark).
pub(crate) struct Palette {
    /// Topic statuses: Open, Revise, Agreed, For information.
    pub open: Color32,
    pub revise: Color32,
    pub agreed: Color32,
    pub info: Color32,
    pub pending: Color32,
    pub working: Color32,
    /// A spoken question turning into text; one whose transcription failed.
    pub transcribing: Color32,
    pub failed: Color32,
    pub answered: Color32,
    pub seen: Color32,
    pub note: Color32,
    pub private: Color32,
    pub input: Color32,
    pub agent: Color32,
    pub person: Color32,
    /// Alternatives and calculation methods.
    pub variant: Color32,
    /// Ranges linked to slides.
    pub slide: Color32,
    /// A choice that changes the result materially.
    pub material: Color32,
}

impl Palette {
    pub(crate) fn status(&self, s: layer::Status) -> Color32 {
        match s {
            layer::Status::Open => self.open,
            layer::Status::Revise => self.revise,
            layer::Status::Agreed => self.agreed,
            layer::Status::Info => self.info,
        }
    }
}

pub(crate) fn palette(t: &Tokens) -> Palette {
    Palette {
        open: Color32::from_rgb(0xE0, 0x8A, 0x00),
        revise: Color32::from_rgb(0xD9, 0x30, 0x25),
        agreed: Color32::from_rgb(0x1E, 0x9E, 0x5A),
        info: if t.dark { Color32::from_rgb(0x8C, 0x93, 0x9D) } else { Color32::from_rgb(0x7A, 0x80, 0x89) },
        pending: Color32::from_rgb(0xE0, 0x8A, 0x00),
        working: BLUE,
        transcribing: Color32::from_rgb(0x6B, 0x7C, 0x93),
        failed: Color32::from_rgb(0xD9, 0x30, 0x25),
        answered: Color32::from_rgb(0x1E, 0x9E, 0x5A),
        seen: t.text_disabled,
        note: Color32::from_rgb(0x7B, 0x4F, 0xD6),
        private: if t.dark { Color32::from_rgb(0x8F, 0xA6, 0xC8) } else { Color32::from_rgb(0x4A, 0x62, 0x8A) },
        input: Color32::from_rgb(0xD9, 0x5D, 0x1E),
        agent: Color32::from_rgb(0x7B, 0x4F, 0xD6),
        person: t.accent,
        variant: Color32::from_rgb(0x0E, 0x8C, 0x8C),
        slide: Color32::from_rgb(0xC2, 0x41, 0x0C),
        material: Color32::from_rgb(0xD9, 0x6A, 0x00),
    }
}

/// A number in a filled circle (a cell pointed at while speaking), its top-left corner at `at`.
pub(crate) fn voice_bubble(painter: &egui::Painter, at: egui::Pos2, n: u32, color: Color32) -> egui::Rect {
    let galley = painter.layout_no_wrap(n.to_string(), gridcraft_ui_egui::theme::ui_bold(9.5), Color32::WHITE);
    let w = (galley.size().x + 6.0).max(14.0);
    let r = egui::Rect::from_min_size(at, vec2(w, 14.0));
    painter.rect_filled(r, 7.0, color);
    painter.galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
    r
}

/// A short, readable number: 1,234,567 · 12.5k · 0.073.
pub(crate) fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "—".into();
    }
    let a = v.abs();
    let s = if a >= 1e9 {
        format!("{:.2}bn", v / 1e9)
    } else if a >= 1e6 {
        format!("{:.2}M", v / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 100.0 || v.fract() == 0.0 {
        format!("{v:.0}")
    } else if a >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    };
    s.replace(".00bn", "bn").replace(".00M", "M").replace(".0k", "k")
}

/// A value as the cell at `k` would show it (its number format), else [`fmt_num`].
pub(crate) fn fmt_at(wb: &gridcraft_engine::model::Workbook, k: (usize, gridcraft_engine::core::CellRef), v: f64) -> String {
    let code = wb.sheet(k.0).map(|sh| wb.styles.get(sh.style_id(k.1)).num_fmt.as_str().to_string()).unwrap_or_default();
    if code.is_empty() || code.eq_ignore_ascii_case("general") || !v.is_finite() {
        return fmt_num(v);
    }
    gridcraft_engine::display::format(&gridcraft_engine::core::Value::Number(v), &code, wb).text
}

/// Follows a link: `cell:Sheet1!B5` selects it, `note:n3` shows the note, a URL opens.
pub(crate) fn follow(ui_state: &mut AgentUi, app: &mut SheetApp, ctx: &egui::Context, href: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(id) = href.strip_prefix("voice-retry:") {
        ui_state.voice.retry(app, ctx, id);
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(id) = href.strip_prefix("voice-dismiss:") {
        ui_state.voice.dismiss(app, id);
        return;
    }
    if let Some(target) = href.strip_prefix("cell:") {
        go_to(app, target);
    } else if let Some(r) = href.strip_prefix("note:") {
        // `note:n3` or `note:3/n3`.
        let id = r.rsplit('/').next().unwrap_or(r).to_string();
        let topic = app.session.active().and_then(|d| layer::get(d).notes.iter().find(|n| n.id == id).map(|n| n.topic.clone()));
        if let Some(t) = topic {
            open_topic(ui_state, app, &t, None);
        }
        ui_state.highlight_note = Some(id);
    } else if let Some(r) = href.strip_prefix("topic:") {
        let t = app.session.active().and_then(|d| gridcraft_agent::topics::find_topic(&d.wb, &layer::get(d), r));
        if let Some(t) = t {
            open_topic(ui_state, app, &t, None);
        }
    } else if let Some(r) = href.strip_prefix("place:") {
        let found = app.session.active().and_then(|d| gridcraft_agent::topics::find_place(&d.wb, &layer::get(d), r));
        if let Some((t, a)) = found {
            open_topic(ui_state, app, &t, Some(&a));
        }
    } else if href.starts_with("https://") || href.starts_with("http://") {
        match &app.services.open_url {
            Some(open) => open(href),
            None => ctx.open_url(egui::OpenUrl::new_tab(href)),
        }
    }
}

/// Opens a topic in the pane and selects one of its places (`anchor`, else the first that's
/// still there).
pub(crate) fn open_topic(st: &mut AgentUi, app: &mut SheetApp, topic: &str, anchor_id: Option<&str>) {
    st.open = true;
    st.tab = Tab::Selection;
    st.focus_topic = Some(topic.to_string());
    let target = app.session.active().and_then(|d| {
        let l = layer::get(d);
        let t = l.topic(topic)?;
        let live = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a));
        match anchor_id {
            Some(a) => live(a),
            None => {
                let n = gridcraft_agent::topics::numbering(&d.wb, &l);
                let mut ps: Vec<_> =
                    t.places.iter().filter_map(|p| Some((n.place.get(&(t.id.clone(), p.anchor.clone()))?.clone(), live(&p.anchor)?))).collect();
                ps.sort_by_key(|a| pane::natural(&a.0));
                ps.into_iter().next().map(|x| x.1)
            }
        }
    });
    if let Some(p) = target {
        go_to(app, &p.a1());
    }
}

/// Selects a place (`Sheet1!B5`, `B5`) and scrolls it into view.
pub(crate) fn go_to(app: &mut SheetApp, target: &str) {
    let Some(d) = app.session.active() else { return };
    let Some(place) = anchor::parse_place(&d.wb, target, d.wb.active_sheet) else { return };
    if place.sheet != d.wb.active_sheet {
        let _ = app.run("sheet.activate", json!({"sheet": place.sheet}));
    }
    let _ = app.run("selection.set", json!({"range": place.range.a1()}));
    app.grid.ensure_visible = true;
}

impl UiExtension for AgentUi {
    fn frame(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        if let Some((open, tab, heat)) = REQUEST.lock().ok().and_then(|mut r| r.take()) {
            self.open = open;
            self.tab = match tab.as_str() {
                "overview" | "questions" | "notes" => Tab::Overview,
                "history" => Tab::History,
                "file" | "filecontext" | "file-context" => Tab::FileContext,
                _ => Tab::Selection,
            };
            self.pane.analysis_open = matches!(tab.as_str(), "variants" | "risk" | "analysis");
            self.heat = match heat.as_str() {
                "confidence" => Heat::Confidence,
                "impact" => Heat::Impact,
                _ => Heat::Off,
            };
            self.pane.trace_open = true;
        }
        self.launcher(app, ctx);
        self.marker_click(app, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.voice.frame(app, ctx);
            voice::indicator(&self.voice, app, ctx);
        }
        // Copy Link to Cells: Cmd+Alt+C (Ctrl+Alt+C), not while typing in a cell.
        if app.editor.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::ALT, egui::Key::C))
        {
            app.copy_to_clipboard(ctx, "link.copy");
        }
        // `--goto` once the workbook is open (a few frames at most).
        if let Ok(mut g) = GOTO.lock()
            && let Some((target, tries)) = g.as_mut()
        {
            *tries += 1;
            let done = app.session.active().is_some() && gridcraft_agent::links::goto(&mut app.session, target).is_ok();
            if done || *tries > 300 {
                *g = None;
            }
        }
        // AItoCraft: keep this app's registry entry current; it goes when the window closes.
        #[cfg(not(target_arch = "wasm32"))]
        {
            if ctx.input(|i| i.viewport().close_requested()) {
                gridcraft_agent::registry::remove();
            } else {
                let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
                gridcraft_agent::docs::note_focus(&app.session, focused, gridcraft_agent::now_ms());
                gridcraft_agent::registry::tick(&app.session);
                ctx.request_repaint_after(std::time::Duration::from_secs(5));
            }
        }
        // The runner follows the active document's context; readiness is checked quietly.
        #[cfg(not(target_arch = "wasm32"))]
        context::frame(self, app, ctx);
        // The background agent: start it on a new question, feed it the questions.
        #[cfg(not(target_arch = "wasm32"))]
        if gridcraft_agent::runner::mode() == gridcraft_agent::runner::Mode::Background {
            gridcraft_agent::runner::tick(&mut app.session);
            ctx.request_repaint_after(std::time::Duration::from_millis(700));
        }
        // A slide asked to see its cells: scroll there and come to the front.
        if gridcraft_agent::links::take_revealed() {
            app.grid.ensure_visible = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    fn side_panel(&mut self, app: &mut SheetApp, ui: &mut Ui) {
        // The pane says which proposal to preview and which link is hovered, while it shows them.
        self.preview = None;
        self.hover_link = None;
        if self.open {
            pane::show(self, app, ui);
        }
    }

    fn paint_cells(&mut self, app: &SheetApp, ui: &Ui, painter: &egui::Painter, cells: &CellGeometry) {
        overlay::paint(self, app, ui, painter, cells);
        #[cfg(not(target_arch = "wasm32"))]
        voice::bubbles(&self.voice, app, painter, cells);
    }

    fn ribbon_tabs(&self) -> Vec<&'static str> {
        vec![ribbon::TAB]
    }

    fn ribbon_keytips(&self) -> Vec<(&'static str, &'static str)> {
        vec![(ribbon::TAB, ribbon::KEYTIP)]
    }

    fn ribbon_tab(&mut self, app: &mut SheetApp, ui: &mut Ui, tab: &str) {
        if tab == ribbon::TAB {
            ribbon::show(self, app, ui);
        }
    }

    fn context_menu(&mut self, app: &mut SheetApp, ui: &mut Ui) -> bool {
        ui.separator();
        let item = |ui: &mut Ui, label: &str| ui.add(egui::Button::new(label).frame(false).min_size(vec2(190.0, 20.0))).clicked();
        let read_only = gridcraft_agent::read_only();
        if !read_only && item(ui, "Ask the Agent…") {
            self.open = true;
            self.tab = Tab::Selection;
            self.focus_draft = true;
            return true;
        }
        if item(ui, "Where Does This Come From?") {
            self.open = true;
            self.tab = Tab::Selection;
            self.pane.trace_open = true;
            return true;
        }
        if item(ui, "Copy Link to Cells") {
            let ctx = ui.ctx().clone();
            app.copy_to_clipboard(&ctx, "link.copy");
            return true;
        }
        let private = app.session.active().is_some_and(|d| {
            let l = layer::get(d);
            let cells = gridcraft_agent::analysis::private_cells(&d.wb, &l);
            cells.contains(&(d.wb.active_sheet, d.selection.active))
        });
        if !read_only && item(ui, if private { "Unmark Client Data" } else { "Mark as Client Data" }) {
            app.run_or_alert("agent.private", json!({"on": !private}));
            return true;
        }
        if item(ui, if self.open { "Hide Agent Pane" } else { "Agent Pane" }) {
            self.open = !self.open;
            return true;
        }
        false
    }
}

impl AgentUi {
    /// A click on a topic marker (painted last frame) opens the topic.
    fn marker_click(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        let Some(pos) = ctx.input(|i| if i.pointer.primary_clicked() { i.pointer.interact_pos() } else { None }) else { return };
        // Not through a menu or a card above the sheet.
        if !app.grid.cells_rect.is_some_and(|r| r.contains(pos)) || ctx.layer_id_at(pos).is_some_and(|l| l.order != egui::Order::Background) {
            return;
        }
        let hit = self.overlay.markers.iter().find(|m| m.0.expand(1.5).contains(pos)).map(|m| (m.1.clone(), m.2.clone()));
        if let Some((topic, anchor_id)) = hit {
            open_topic(self, app, &topic, Some(&anchor_id));
        }
    }

    /// The always-there Agent button at the bottom right of the sheet, with what is waiting.
    fn launcher(&mut self, app: &mut SheetApp, ctx: &egui::Context) {
        let Some(cells) = app.grid.cells_rect else { return };
        let Some(d) = app.session.active() else { return };
        let l = layer::get(d);
        let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen && !q.hidden).count();
        let waiting =
            l.questions.iter().filter(|q| matches!(q.status, QuestionStatus::Pending | QuestionStatus::Working) && !q.is_ping() && !q.hidden).count();
        let t = Tokens::get(ctx);
        let p = palette(&t);
        let (text, color) = if ready > 0 {
            (format!("{ready} answer{} ready", if ready == 1 { "" } else { "s" }), p.answered)
        } else if waiting > 0 {
            (format!("{waiting} question{} with the agent", if waiting == 1 { "" } else { "s" }), p.pending)
        } else {
            ("Agent".to_string(), t.accent)
        };
        // The Agent tab shows the status; the button only while the ribbon is collapsed.
        if !app.ui.ribbon_collapsed || (self.open && ready == 0 && waiting == 0) {
            return;
        }
        let pos = egui::pos2(cells.right() - 14.0, cells.bottom() - 14.0);
        egui::Area::new(egui::Id::new("agent_launcher")).pivot(egui::Align2::RIGHT_BOTTOM).fixed_pos(pos).order(egui::Order::Foreground).show(
            ctx,
            |ui| {
                let b = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong()).fill(color).corner_radius(12.0);
                if ui.add(b).on_hover_text("Questions, answers and notes of your agent").clicked() {
                    self.open = true;
                    self.tab = if ready > 0 || waiting > 0 { Tab::Overview } else { Tab::Selection };
                }
            },
        );
    }
}
