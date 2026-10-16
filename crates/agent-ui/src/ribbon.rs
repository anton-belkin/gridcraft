//! The Agent tab on the ribbon: what isn't about one cell. The pane, client data, how the agent is
//! connected, the heat map, sharing. Groups are named the same in WordCraft and DeckCraft:
//! Agent, Client data, Connection, View (the heat map is GridCraft's), Share.

use egui::{Align2, Color32, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use gridcraft_agent::layer::{self, QuestionStatus};
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::icons::{self, Icon};
use gridcraft_ui_egui::theme::{self, Tokens};
use serde_json::json;

use crate::{AgentUi, Heat, Tab, palette};

/// The tab's name.
pub const TAB: &str = "Agent";
/// Its keytip (Alt, G; F10, G on macOS). WordCraft and DeckCraft use A, which is Data here, as in
/// Excel.
pub const KEYTIP: &str = "G";
/// Height of a group's controls (its caption goes under them).
const BODY: f32 = 58.0;

/// A group: its controls in a row, its name under them, a separator after.
fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
        let top = ui.cursor().top();
        let row = ui.horizontal(|ui| {
            ui.set_min_height(BODY);
            add(ui);
        });
        let w = row.response.rect.width().max(40.0);
        // Every group's name on the same line, under its controls.
        let cap = Rect::from_min_size(pos2(row.response.rect.left(), top + BODY + 2.0), vec2(w, 14.0));
        ui.allocate_rect(cap, Sense::hover());
        ui.painter().text(cap.center(), Align2::CENTER_CENTER, title, theme::ui_font(10.5), t.text_dim);
    });
    gridcraft_ui_egui::ext::ribbon_separator(ui);
}

/// A column of small controls.
fn column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
        add(ui);
    });
}

/// A large button that fits a group: a drawn icon over one or two label lines and a sub-label in
/// its colour.
fn tall_button(
    ui: &mut Ui,
    label: &str,
    sub: Option<(&str, Color32)>,
    on: bool,
    enabled: bool,
    tip: &str,
    icon: impl FnOnce(&egui::Painter, Rect, Color32),
) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(11.0);
    let w_of = |s: &str, f: egui::FontId| ui.painter().layout_no_wrap(s.to_string(), f, t.text).size().x;
    let w = label.split('\n').map(|l| w_of(l, font.clone())).fold(sub.map_or(0.0, |(s, _)| w_of(s, theme::ui_bold(10.0))), f32::max);
    let (rect, resp) = ui.allocate_exact_size(vec2(w.max(40.0) + 14.0, BODY), if enabled { Sense::click() } else { Sense::hover() });
    if on {
        ui.painter().rect_filled(rect, 5.0, t.pressed);
    } else if resp.hovered() && enabled {
        ui.painter().rect_filled(rect, 5.0, t.hover);
    }
    let lines: Vec<&str> = label.split('\n').collect();
    let rows = lines.len() + usize::from(sub.is_some());
    let icon_h = if rows >= 3 { 20.0 } else { 26.0 };
    icon(
        ui.painter(),
        Rect::from_center_size(pos2(rect.center().x, rect.top() + 2.0 + icon_h / 2.0), vec2(icon_h, icon_h)),
        if enabled { t.text } else { t.text_disabled },
    );
    let mut y = rect.top() + icon_h + 9.0;
    for l in lines {
        ui.painter().text(pos2(rect.center().x, y), Align2::CENTER_CENTER, l, font.clone(), if enabled { t.text } else { t.text_disabled });
        y += 11.5;
    }
    if let Some((s, c)) = sub {
        ui.painter().text(pos2(rect.center().x, y), Align2::CENTER_CENTER, s, theme::ui_bold(10.0), c);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label.replace('\n', " ")));
    resp.on_hover_text(tip)
}

/// The agent's mark, drawn (the interface fonts have no such glyph): a four-pointed star and a
/// small one, as in WordCraft and DeckCraft.
fn sparkle(p: &egui::Painter, r: Rect, color: Color32) {
    let star = |c: egui::Pos2, o: f32, i: f32| {
        let pts = vec![
            c + vec2(0.0, -o),
            c + vec2(i, -i),
            c + vec2(o, 0.0),
            c + vec2(i, i),
            c + vec2(0.0, o),
            c + vec2(-i, i),
            c + vec2(-o, 0.0),
            c + vec2(-i, -i),
        ];
        p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    };
    let c = r.center();
    let o = r.width() * 0.42;
    star(c, o, r.width() * 0.11);
    star(c + vec2(o * 0.95, -o * 0.75), r.width() * 0.16, r.width() * 0.05);
}

/// A compact ribbon row: 14 px icon and a label (three fit over a group's caption).
fn row_button(ui: &mut Ui, icon: Icon, label: &str, tip: &str, dropdown: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(12.0);
    let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 26.0 + if dropdown { 12.0 } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), Sense::click());
    if resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 4.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.left() + 10.0, rect.center().y), vec2(14.0, 14.0)), icon, t.text);
    ui.painter().text(pos2(rect.left() + 22.0, rect.center().y), Align2::LEFT_CENTER, label, font, t.text);
    if dropdown {
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(rect.right() - 7.0, rect.center().y + 1.0), vec2(9.0, 9.0)),
            Icon::Chevron,
            t.text_dim,
        );
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    resp.on_hover_text(tip)
}

/// One choice of a few (a radio as a small row with a tick).
fn choice(ui: &mut Ui, label: &str, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(12.0);
    let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 26.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let c = pos2(rect.left() + 9.0, rect.center().y);
    ui.painter().circle_stroke(c, 5.0, Stroke::new(1.0, t.text_dim));
    if on {
        ui.painter().circle_filled(c, 3.0, t.accent);
    }
    ui.painter().text(pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, label, font, if on { t.text } else { t.text_dim });
    resp.on_hover_text(tip)
}

/// A status line: a dot and a short text, details on hover.
fn status_line(ui: &mut Ui, text: &str, color: Color32, tip: &str) {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(11.5);
    let w = ui.painter().layout_no_wrap(text.to_string(), font.clone(), t.text).size().x + 22.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), Sense::hover());
    ui.painter().circle_filled(pos2(rect.left() + 8.0, rect.center().y), 4.0, color);
    ui.painter().text(pos2(rect.left() + 18.0, rect.center().y), Align2::LEFT_CENTER, text, font, t.text);
    if !tip.is_empty() {
        resp.on_hover_text(tip);
    }
}

/// What waits for the reader: (answers ready, questions with the agent).
pub(crate) fn counts(app: &SheetApp) -> (usize, usize) {
    let Some(d) = app.session.active() else { return (0, 0) };
    let l = layer::get(d);
    let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen && !q.hidden).count();
    let waiting =
        l.questions.iter().filter(|q| matches!(q.status, QuestionStatus::Pending | QuestionStatus::Working) && !q.is_ping() && !q.hidden).count();
    (ready, waiting)
}

/// What the Agent button says under its label: readiness for sign-off ("3 of 5 agreed"), or what
/// waits when there are no topics to decide yet; the details on hover.
fn status(app: &SheetApp, t: &Tokens, p: &crate::Palette) -> (String, Color32, String) {
    let Some(d) = app.session.active() else { return ("idle".into(), t.text_disabled, String::new()) };
    let l = layer::get(d);
    let live = |q: &&layer::Question| !q.is_ping() && !q.hidden;
    let ready = l.questions.iter().filter(live).filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
    let working = l.questions.iter().filter(live).filter(|q| q.status == QuestionStatus::Working).count();
    let waiting = l.questions.iter().filter(live).filter(|q| q.status == QuestionStatus::Pending).count();
    let mut tip = crate::pane::signoff_line(&l);
    for (k, what) in [(ready, "answers ready"), (working, "in work with the agent"), (waiting, "waiting for the agent")] {
        if k > 0 {
            tip.push_str(&format!("\n{k} {what}"));
        }
    }
    let (agreed, decide, revise, open, _) = crate::pane::signoff(&l);
    if decide > 0 {
        let color = if revise > 0 {
            p.revise
        } else if agreed == decide {
            p.agreed
        } else if open > 0 {
            p.open
        } else {
            t.text_dim
        };
        return (format!("{agreed} of {decide} agreed"), color, tip);
    }
    if ready > 0 {
        (format!("{ready} ready"), p.answered, tip)
    } else if working > 0 {
        (format!("{working} in work"), p.working, tip)
    } else if waiting > 0 {
        (format!("{waiting} waiting"), p.pending, tip)
    } else {
        ("idle".into(), t.text_disabled, tip)
    }
}

/// Something the user clicked, done once drawing is finished.
enum Act {
    Run(&'static str, serde_json::Value),
    Pane(bool, Tab),
    Heat(Heat),
    CopyLink,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Copy(String),
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Cowork(serde_json::Value),
    Share(String),
    #[cfg(not(target_arch = "wasm32"))]
    VoiceModel(Option<craft_voice::Model>),
    #[cfg(not(target_arch = "wasm32"))]
    VoiceToggle,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Message(String, String),
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    OpenClaude,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    InstallHub,
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SheetApp, ui: &mut Ui) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(msg) = gridcraft_agent::hub::take_result() {
        app.message = Some(("AItoCraft".into(), msg));
    }
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let read_only = gridcraft_agent::read_only();
    let mut acts: Vec<Act> = vec![];
    let (ready, waiting) = counts(app);
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);

    // Agent: the pane and what waits.
    group(ui, "Agent", |ui| {
        let (sub, color, detail) = status(app, &t, &p);
        let tip = format!("Show or hide the Agent pane: topics on the selection, their status, notes and discussion\n\n{detail}");
        if tall_button(ui, "Agent", Some((&sub, color)), st.open, true, tip.trim_end(), |pa, r, _| sparkle(pa, r, p.agent)).clicked() {
            acts.push(Act::Pane(!st.open, if ready > 0 || waiting > 0 { Tab::Overview } else { Tab::Selection }));
        }
    });

    // Client data: what the agent sees as stand-ins, and what a saved file holds.
    if !read_only {
        let private_cells = gridcraft_agent::analysis::private_cells(&d.wb, &l);
        let sel = d.selection.current();
        let si = d.wb.active_sheet;
        let is_private = sel.iter().take(10_000).any(|c| private_cells.contains(&(si, c)));
        let safe = l.safe.as_ref();
        let locked = safe.is_some_and(|s| s.sealed.is_some());
        group(ui, "Client data", |ui| {
            column(ui, |ui| {
                let (label, tip) = if is_private {
                    ("Unmark Selection", "The selected cells are no longer client data: your agent will see their real values")
                } else {
                    (
                        "Mark Selection",
                        "Mark the selected cells as client data: your agent sees stand-in values, with the model recalculated from them",
                    )
                };
                if row_button(ui, Icon::Lock, label, tip, false).clicked() {
                    acts.push(Act::Run("agent.private", json!({"on": !is_private})));
                }
                let (label, tip) = match safe {
                    _ if locked => {
                        ("Unlock Client Data", "This safe file's real values are sealed under a key that isn't on this computer: try again")
                    }
                    Some(_) => (
                        "Safe File: On",
                        "Saving writes client data as stand-ins; the real values are sealed under a key kept on this computer. Click to turn off",
                    ),
                    None => ("Safe File: Off", "Save client data as stand-ins, with the real values sealed under a key kept on this computer"),
                };
                if row_button(ui, Icon::Save, label, tip, false).clicked() {
                    acts.push(if locked { Act::Run("agent.unlock", json!({})) } else { Act::Run("agent.safeMode", json!({"on": safe.is_none()})) });
                }
                if row_button(
                    ui,
                    Icon::Share,
                    "Export for Excel",
                    "A copy beside the workbook with the real values and nothing sealed, for standard Excel",
                    false,
                )
                .clicked()
                {
                    acts.push(Act::Run("agent.exportExcel", json!({})));
                }
            });
        });
    }

    // Connection: how the agent reaches this workbook.
    group(ui, "Connection", |ui| connection(app, ui, &t, &p, read_only, &mut acts));

    // Voice: ask by speaking and pointing, and the speech model (its own group, after Connection).
    #[cfg(not(target_arch = "wasm32"))]
    if !read_only {
        group(ui, "Voice", |ui| voice_group(st, ui, &t, &mut acts));
    }

    // View: the heat maps.
    group(ui, "View", |ui| {
        column(ui, |ui| {
            for (h, label, tip) in [
                (Heat::Off, "No Heat Map", "Cells as they are"),
                (Heat::Confidence, "Confidence", "How sure your agent is about each range: red guess, green verified"),
                (Heat::Impact, "Impact", "How much each input moves the selected result"),
            ] {
                if choice(ui, label, st.heat == h, tip).clicked() {
                    acts.push(Act::Heat(h));
                }
            }
        });
    });

    // Share: a slide, a web page.
    if !read_only {
        group(ui, "Share", |ui| {
            column(ui, |ui| {
                if row_button(
                    ui,
                    Icon::Link,
                    "Copy Link for Slide",
                    "⌘⌥C · paste it on a DeckCraft slide: the slide stays linked to these cells",
                    false,
                )
                .clicked()
                {
                    acts.push(Act::CopyLink);
                }
                let path = d.path.as_deref().map(|x| std::path::Path::new(x).with_extension("html").display().to_string());
                let web = !cfg!(target_arch = "wasm32");
                if web {
                    let tip = path.clone().unwrap_or_else(|| "Save the workbook first: the page is written beside it".into());
                    if row_button(
                        ui,
                        Icon::Picture,
                        "Export Web Page",
                        &format!("One HTML file with the topics, their status and notes, read-only · {tip}"),
                        false,
                    )
                    .clicked()
                        && let Some(path) = path
                    {
                        acts.push(Act::Share(path));
                    }
                    if choice(
                        ui,
                        "Include Discussion",
                        st.share_discussion,
                        "The web page also carries the questions and answers. Off: only the status and notes of each topic go out",
                    )
                    .clicked()
                    {
                        st.share_discussion = !st.share_discussion;
                    }
                }
            });
        });
    }

    let ctx = ui.ctx().clone();
    for a in acts {
        match a {
            Act::Run(id, params) => {
                let out = app.run(id, params);
                match out {
                    Ok(v) => {
                        if let Some(path) = v.get("path").and_then(|x| x.as_str())
                            && id == "agent.exportExcel"
                        {
                            app.toast = Some((format!("Saved {path}"), gridcraft_ui_egui::now_ms()));
                        }
                    }
                    Err(e) => app.message = Some(("Agent".into(), e)),
                }
            }
            Act::Pane(open, tab) => {
                st.open = open;
                st.tab = tab;
                if open && tab == Tab::Selection {
                    st.focus_draft = true;
                }
            }
            Act::Heat(h) => st.heat = h,
            Act::CopyLink => app.copy_to_clipboard(&ctx, "link.copy"),
            Act::Copy(text) => {
                ctx.copy_text(text);
                app.toast = Some(("Copied".into(), gridcraft_ui_egui::now_ms()));
            }
            Act::Cowork(entry) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    gridcraft_agent::cowork::connect(entry);
                    let st = gridcraft_agent::cowork::state_json(&gridcraft_agent::cowork::state());
                    let msg = st.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                    if !msg.is_empty() {
                        app.message = Some(("Connect Cowork".into(), msg));
                    }
                }
                #[cfg(target_arch = "wasm32")]
                let _ = entry;
            }
            Act::Message(title, text) => app.message = Some((title, text)),
            Act::InstallHub =>
            {
                #[cfg(not(target_arch = "wasm32"))]
                match gridcraft_agent::hub::install_desktop() {
                    Ok(()) => app.toast = Some(("Installing AItoCraft in Claude Desktop…".into(), gridcraft_ui_egui::now_ms())),
                    Err(e) => app.message = Some(("AItoCraft".into(), e)),
                }
            }
            Act::OpenClaude => {
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("/usr/bin/open").args(["-a", "Claude"]).spawn();
                }
                app.toast =
                    Some(("Claude Desktop: the session is in its Code tab, under this workbook's folder".into(), gridcraft_ui_egui::now_ms()));
            }
            Act::Share(path) => match app.run("agent.exportHtml", json!({"path": path, "includeDiscussion": st.share_discussion})) {
                Ok(_) => app.toast = Some((format!("Saved {path}"), gridcraft_ui_egui::now_ms())),
                Err(e) => app.message = Some(("Export Web Page".into(), e)),
            },
            #[cfg(not(target_arch = "wasm32"))]
            Act::VoiceModel(m) => st.voice.choose(app, &ctx, m),
            #[cfg(not(target_arch = "wasm32"))]
            Act::VoiceToggle => st.voice.toggle(app, &ctx),
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn connection(_app: &SheetApp, ui: &mut Ui, t: &Tokens, _p: &crate::Palette, _read_only: bool, _acts: &mut Vec<Act>) {
    column(ui, |ui| {
        status_line(
            ui,
            "Read-only copy",
            t.text_dim,
            "Exported from GridCraft with the answers and notes the author's agent prepared. No agent is connected here.",
        );
    });
}

/// The connection's state as a drawn mark: filled (answering), half (a listener), ring (nobody),
/// cross (no answer).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
#[derive(Clone, Copy, PartialEq)]
enum Mark {
    Full,
    Half,
    Ring,
    Cross,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn mark(p: &egui::Painter, c: egui::Pos2, r: f32, m: Mark, color: Color32) {
    match m {
        Mark::Full => {
            p.circle_filled(c, r, color);
        }
        Mark::Half => {
            p.circle_stroke(c, r - 0.6, Stroke::new(1.4, color));
            let pts: Vec<egui::Pos2> = (0..=16)
                .map(|i| {
                    let a = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / 16.0;
                    c + vec2(a.cos(), a.sin()) * r
                })
                .collect();
            p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        Mark::Ring => {
            p.circle_stroke(c, r - 0.6, Stroke::new(1.4, color));
        }
        Mark::Cross => {
            let d = r * 0.8;
            p.line_segment([c - vec2(d, d), c + vec2(d, d)], Stroke::new(2.0, color));
            p.line_segment([c + vec2(-d, d), c + vec2(d, -d)], Stroke::new(2.0, color));
        }
    }
}

/// "12 s", "3 min", "2 h".
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn span(ms: u64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h", s / 3600)
    }
}

/// A menu item that copies one thing (its text on hover).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn copy_item(ui: &mut Ui, acts: &mut Vec<Act>, label: &str, tip: &str, text: String) {
    if ui.button(label).on_hover_text(format!("{tip}\n\n{text}")).clicked() {
        acts.push(Act::Copy(text));
        ui.close();
    }
}

/// The status: a mark, one line and a sub-line; the details on hover.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn status_block(ui: &mut Ui, m: Mark, color: Color32, line: &str, sub: &str, tip: &str) {
    let t = Tokens::get(ui.ctx());
    let big = theme::ui_bold(12.5);
    let small = theme::ui_font(10.5);
    let w = ui
        .painter()
        .layout_no_wrap(line.to_string(), big.clone(), t.text)
        .size()
        .x
        .max(ui.painter().layout_no_wrap(sub.to_string(), small.clone(), t.text).size().x)
        + 24.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
    mark(ui.painter(), pos2(rect.left() + 8.0, rect.top() + 9.0), 5.5, m, color);
    ui.painter().text(pos2(rect.left() + 20.0, rect.top() + 9.0), Align2::LEFT_CENTER, line, big, t.text);
    ui.painter().text(pos2(rect.left() + 20.0, rect.top() + 25.0), Align2::LEFT_CENTER, sub, small, t.text_dim);
    resp.on_hover_text(tip);
}

#[cfg(not(target_arch = "wasm32"))]
fn connection(app: &SheetApp, ui: &mut Ui, t: &Tokens, p: &crate::Palette, read_only: bool, acts: &mut Vec<Act>) {
    use gridcraft_agent::cowork::{self, Configured, State};
    use gridcraft_agent::runner::{self, Mode, Presence};
    if read_only {
        column(ui, |ui| {
            status_line(
                ui,
                "Read-only copy",
                t.text_dim,
                "Exported from GridCraft with the answers and notes the author's agent prepared. No agent is connected here.",
            );
        });
        return;
    }
    let Some(d) = app.session.active() else { return };
    let mode = runner::mode();
    let url = crate::server_url().unwrap_or("http://127.0.0.1:7980/mcp").to_string();
    let base = url.trim_end_matches("/mcp").to_string();
    let bg = runner::status(d.uid).unwrap_or_default();
    let session = if bg.session_id.is_empty() { layer::get(d).agent_session.clone() } else { bg.session_id.clone() };
    let entry = crate::stdio_entry();
    let configured = match (&entry, cowork::config_path()) {
        (Some(e), Some(path)) => cowork::configured(&path, e),
        _ => Configured::NoClaude,
    };
    let cowork_state = cowork::state();
    let cowork_text = match (&cowork_state, &configured) {
        (State::Waiting, _) => "waiting: quit Claude (⌘Q)",
        (State::Done { .. }, _) => "added: open Claude",
        (State::Failed(_), _) => "failed",
        (_, Configured::Yes) => "connected",
        (_, Configured::Different) => "another build is connected",
        _ => "not set up",
    };
    // Everything that isn't the status goes into the details.
    let signals = runner::signals(d.uid);
    let mut details = format!("MCP server: {url}\nEvent stream: {base}/events\nCowork (Claude Desktop): {cowork_text}");
    if let State::Failed(e) = &cowork_state {
        details.push_str(&format!(" ({e})"));
    }
    details.push_str(&format!(
        "\nListening now: {} event stream(s), {} channel stream(s){}",
        signals.event_streams,
        signals.channel_streams,
        if signals.background { ", background agent" } else { "" }
    ));
    if signals.last_call_ms > 0 {
        details.push_str(&format!("\nLast agent call: {} ago", span(gridcraft_agent::now_ms().saturating_sub(signals.last_call_ms))));
    }
    if !session.is_empty() {
        details.push_str(&format!("\nBackground session: {session}"));
        if !bg.cwd.is_empty() {
            details.push_str(&format!(" in {}", bg.cwd));
        }
    }
    if bg.turns > 0 {
        details.push_str(&format!("\nBackground agent: {} turns, ${:.2}", bg.turns, bg.cost_usd));
    }
    if !bg.last_error.is_empty() {
        details.push_str(&format!("\nLast error: {}", bg.last_error));
    }
    let presence = runner::presence(&app.session);
    let red = Color32::from_rgb(0xC4, 0x2B, 0x1C);
    let answered_by = layer::get(d).ping_answered_by();
    let (m, color, line, sub) = match &presence {
        _ if mode == Mode::Off => (Mark::Ring, t.text_disabled, "Agent off".to_string(), "choose a mode to let an agent in".to_string()),
        Presence::Answering { checked_ms, latency_ms } => (
            Mark::Full,
            p.answered,
            match &answered_by {
                Some(by) => format!("Agent answering: {by}"),
                None => "Agent answering".to_string(),
            },
            format!("checked {} ago · answered in {}", span(*checked_ms), span(*latency_ms)),
        ),
        Presence::Checking { since_ms } => (Mark::Half, p.pending, "Checking…".to_string(), format!("asked {} ago", span(*since_ms))),
        Presence::NoAnswer { asked_ms } => (Mark::Cross, red, "No answer to check".to_string(), format!("asked {} ago", span(*asked_ms))),
        Presence::Listening { label, seen_ms, more } => (
            Mark::Full,
            p.answered,
            format!("Listening: {label}"),
            if *more > 0 { format!("{} ago · and {more} more", span(*seen_ms)) } else { format!("{} ago", span(*seen_ms)) },
        ),
        Presence::Listener(what) => (Mark::Half, p.pending, "Listener connected".to_string(), format!("not checked · {}", what.join(", "))),
        Presence::Nobody => {
            (Mark::Ring, t.text_dim, "Nobody is listening for this document".to_string(), "questions still queue here · press Check".to_string())
        }
    };
    if !matches!(presence, Presence::Nobody) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
    let title = d.display_title();
    column(ui, |ui| {
        status_block(ui, m, color, &line, &sub, &details);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if mode != Mode::Off
                && row_button(ui, Icon::Search, "Check", "Ask \"is anyone there?\": a connection check this document's listeners answer with \"here\"", false).clicked()
            {
                acts.push(Act::Run("agent.ping", json!({})));
            }
            // Mode: a simple list.
            let r = row_button(ui, Icon::Settings, &format!("Mode: {}", mode.label().0), "How your agent gets the questions", true);
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(330.0);
                for m in Mode::ALL {
                    let (name, what) = m.label();
                    let on = mode == m;
                    let resp = ui.add(egui::Button::selectable(on, egui::RichText::new(name).strong()).min_size(vec2(320.0, 18.0)));
                    ui.label(egui::RichText::new(what).small().color(t.text_dim));
                    if resp.clicked() {
                        if !on {
                            acts.push(Act::Run("agent.agentMode", json!({"mode": m.id()})));
                        }
                        ui.close();
                    }
                }
            });
            // Copy: what this mode needs, one thing per item.
            let menu_title = if mode == Mode::Background { "Session" } else { "Copy" };
            let r = row_button(ui, Icon::Copy, menu_title, "What to paste where, for this mode", true);
            egui::Popup::menu(&r).show(|ui| {
                ui.set_min_width(280.0);
                match mode {
                    Mode::Manual | Mode::Channel => {
                        copy_item(ui, acts, "Copy phrase for Claude", "Paste it where the work belongs: a Cowork project, a chat, a Claude Code session", gridcraft_agent::hub::phrase(&title));
                        if mode == Mode::Manual
                            && ui.button("Install AItoCraft in Claude Desktop").on_hover_text("One connection for every Craft app in Claude Desktop and Cowork").clicked()
                        {
                            acts.push(Act::InstallHub);
                            ui.close();
                        }
                        copy_item(ui, acts, "Claude Code: add AItoCraft", "Once, in a terminal", gridcraft_agent::hub::claude_code_command());
                        if mode == Mode::Channel {
                            copy_item(ui, acts, "Start Claude Code with the channel", "Questions arrive in that session as they're asked", gridcraft_agent::hub::CHANNEL_COMMAND.to_string());
                        }
                    }
                    Mode::Background => {
                        if ui.button("Open session in Claude Desktop").on_hover_text("Background sessions are in Claude's Code tab").clicked() {
                            acts.push(Act::OpenClaude);
                            ui.close();
                        }
                        if ui.button("New session").on_hover_text("Start over from a summary of this workbook's history").clicked() {
                            acts.push(Act::Run("agent.agentReset", json!({})));
                            ui.close();
                        }
                        let mut own = runner::config().project_folder;
                        if ui
                            .checkbox(&mut own, "Use the document's folder as its project")
                            .on_hover_text("Its CLAUDE.md, memory and settings then join the agent's context. Off: a neutral, empty folder.")
                            .changed()
                        {
                            acts.push(Act::Run("agent.agentMode", json!({"projectFolder": own})));
                        }
                    }
                    Mode::Off => {}
                }
                ui.separator();
                ui.menu_button("Details", |ui| {
                    if ui.button("Connection details…").clicked() {
                        acts.push(Act::Message("Connection".into(), details.clone()));
                        ui.close();
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Without AItoCraft (this app only)").small().color(t.text_dim));
                    let can = entry.is_some() && cowork_state != State::Waiting;
                    if ui
                        .add_enabled(can, egui::Button::new(format!("Connect GridCraft to Claude Desktop… ({cowork_text})")))
                        .on_hover_text("gridcraft --agent-stdio in Claude Desktop's settings. Claude rewrites its settings when it quits, so while it runs GridCraft waits for you to quit it (⌘Q), then writes.")
                        .clicked()
                        && let Some(e) = entry.clone()
                    {
                        acts.push(Act::Cowork(e));
                        ui.close();
                    }
                    let mut copy = |ui: &mut Ui, label: &str, text: String| {
                        if ui.button(label).on_hover_text(&text).clicked() {
                            acts.push(Act::Copy(text));
                            ui.close();
                        }
                    };
                    copy(ui, "Claude Code: add GridCraft's MCP server", format!("claude mcp add --transport http gridcraft {url}"));
                    copy(ui, "Monitor command", format!("curl -sN {base}/events"));
                });
            });
        });
    });
}

/// The Voice group: the microphone (status under it) and the model, off until the reader picks one.
#[cfg(not(target_arch = "wasm32"))]
fn voice_group(st: &AgentUi, ui: &mut Ui, t: &Tokens, acts: &mut Vec<Act>) {
    use crate::voice::{KEY, Status};
    use craft_voice::Model;
    let red = Color32::from_rgb(0xD9, 0x30, 0x25);
    let p = palette(t);
    let v = &st.voice;
    let (status, color, tip): (String, Color32, String) = match (v.meter(), v.status()) {
        (Some((secs, _)), _) => (format!("● {secs:.0}s"), red, "Listening. Click the cells you mean; click again to send".into()),
        (None, Status::Off) => ("Off".into(), t.text_dim, "Choose a speech model (right) to ask by voice".into()),
        (None, Status::Downloading(done, total)) => {
            let pct = if *total > 0 { *done as f64 * 100.0 / *total as f64 } else { 0.0 };
            (format!("Downloading {pct:.0}%"), p.working, format!("Downloading the speech model ({} of {} MB)", done / 1_000_000, total / 1_000_000))
        }
        (None, Status::Loading) => ("Loading…".into(), p.working, "Loading the speech model".into()),
        (None, Status::Failed(e)) => ("Model error".into(), red, format!("{e}. Choose the model again to retry")),
        (None, Status::Idle | Status::Ready) => {
            // Words, not "⌥": the bold ribbon font has no such glyph.
            (
                if cfg!(target_os = "macos") { "Hold Option".to_string() } else { "Hold Alt".to_string() },
                t.text_dim,
                format!("Hold {KEY}, speak, click the cells you mean; release to send. Or click here to start and again to send"),
            )
        }
    };
    let on = v.recording();
    let resp = tall_button(ui, "Ask by Voice", Some((&status, color)), on, true, &tip, |pa, r, ink| mic(pa, r, if on { red } else { ink }, on));
    // Download progress as a bar under the status.
    if let Status::Downloading(done, total) = v.status()
        && *total > 0
    {
        let r = resp.rect;
        let bar = Rect::from_min_size(pos2(r.left() + 6.0, r.bottom() - 3.0), vec2(r.width() - 12.0, 2.5));
        ui.painter().rect_filled(bar, 1.0, t.separator);
        let k = (*done as f32 / *total as f32).clamp(0.0, 1.0);
        ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * k, bar.height())), 1.0, p.working);
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    if resp.clicked() {
        acts.push(if v.choice().is_some() || v.recording() { Act::VoiceToggle } else { Act::VoiceModel(Some(Model::Small)) });
    }
    column(ui, |ui| {
        let here = |m: Model| craft_voice::default_dir().is_some_and(|d| m.is_ready(&d));
        let size = |m: Model| if here(m) { String::new() } else { format!(" · {} MB", m.download_size() / 1_000_000) };
        if choice(ui, "Voice Off", v.choice().is_none(), "No voice questions (the model is unloaded)").clicked() {
            acts.push(Act::VoiceModel(None));
        }
        if choice(
            ui,
            &format!("Small{}", size(Model::Small)),
            v.choice() == Some(Model::Small),
            "Whisper small, 5-bit: fast (about 1–1.5 s for a 5-second question on an M1 Pro). Downloaded once, on this computer; audio never leaves it",
        )
        .clicked()
        {
            acts.push(Act::VoiceModel(Some(Model::Small)));
        }
        if choice(
            ui,
            &format!("Turbo, slower{}", size(Model::Turbo)),
            v.choice() == Some(Model::Turbo),
            "Whisper large-v3-turbo, 5-bit: more accurate with numbers and names, slower (about 2 s for a 5-second question on an M1 Pro). Downloaded once, on this computer; audio never leaves it",
        )
        .clicked()
        {
            acts.push(Act::VoiceModel(Some(Model::Turbo)));
        }
    });
}

/// A microphone, drawn (the icon set has none): capsule, cradle, stem and base; filled while
/// recording.
#[cfg(not(target_arch = "wasm32"))]
fn mic(p: &egui::Painter, r: Rect, color: Color32, on: bool) {
    let k = r.width() / 26.0;
    let c = r.center();
    let stroke = Stroke::new(1.6, color);
    let body = Rect::from_center_size(c + vec2(0.0, -4.0 * k), vec2(8.0 * k, 13.0 * k));
    if on {
        p.rect_filled(body, 4.0 * k, color);
    } else {
        p.rect_stroke(body, 4.0 * k, stroke, egui::StrokeKind::Middle);
    }
    let pts: Vec<egui::Pos2> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 16.0);
            c + vec2(7.0 * k * a.cos(), k * (6.0 * a.sin()))
        })
        .collect();
    p.add(egui::Shape::line(pts, stroke));
    p.line_segment([c + vec2(0.0, 6.0 * k), c + vec2(0.0, 9.5 * k)], stroke);
    p.line_segment([c + vec2(-4.0 * k, 9.5 * k), c + vec2(4.0 * k, 9.5 * k)], stroke);
}
