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

/// Claude Desktop's plug: a box with a plus.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn plug(p: &egui::Painter, r: Rect, color: Color32) {
    let b = r.shrink(r.width() * 0.18);
    p.rect_stroke(b, 3.0, Stroke::new(1.6, color), egui::StrokeKind::Inside);
    let c = b.center();
    let s = b.width() * 0.25;
    p.line_segment([c - vec2(s, 0.0), c + vec2(s, 0.0)], Stroke::new(1.8, color));
    p.line_segment([c - vec2(0.0, s), c + vec2(0.0, s)], Stroke::new(1.8, color));
}

/// "just now", "5 min ago", "2 h ago".
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn ago(ms: u64) -> String {
    let s = gridcraft_agent::now_ms().saturating_sub(ms) / 1000;
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} min ago", s / 60)
    } else {
        format!("{} h ago", s / 3600)
    }
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
    let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
    let waiting = l.questions.iter().filter(|q| matches!(q.status, QuestionStatus::Pending | QuestionStatus::Working)).count();
    (ready, waiting)
}

/// What the Agent button says under its label: "1 ready", "1 in work", "2 waiting", "idle".
fn status(app: &SheetApp, t: &Tokens, p: &crate::Palette) -> (String, Color32) {
    let Some(d) = app.session.active() else { return ("idle".into(), t.text_disabled) };
    let l = layer::get(d);
    let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
    let working = l.questions.iter().filter(|q| q.status == QuestionStatus::Working).count();
    let waiting = l.questions.iter().filter(|q| q.status == QuestionStatus::Pending).count();
    if ready > 0 {
        (format!("{ready} ready"), p.answered)
    } else if working > 0 {
        (format!("{working} in work"), p.working)
    } else if waiting > 0 {
        (format!("{waiting} waiting"), p.pending)
    } else {
        ("idle".into(), t.text_disabled)
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
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SheetApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let read_only = gridcraft_agent::read_only();
    let mut acts: Vec<Act> = vec![];
    let (ready, waiting) = counts(app);
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);

    // Agent: the pane and what waits.
    group(ui, "Agent", |ui| {
        let (sub, color) = status(app, &t, &p);
        if tall_button(
            ui,
            "Agent",
            Some((&sub, color)),
            st.open,
            true,
            "Show or hide the Agent pane: your questions, the agent's answers and notes",
            |pa, r, _| sparkle(pa, r, p.agent),
        )
        .clicked()
        {
            acts.push(Act::Pane(!st.open, if ready > 0 || waiting > 0 { Tab::Questions } else { Tab::Cell }));
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
                        &format!("One HTML file with the prepared answers and notes, read-only · {tip}"),
                        false,
                    )
                    .clicked()
                        && let Some(path) = path
                    {
                        acts.push(Act::Share(path));
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
                if open && tab == Tab::Cell {
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
                gridcraft_agent::cowork::connect(entry);
                #[cfg(target_arch = "wasm32")]
                let _ = entry;
            }
            Act::Share(path) => match app.run("agent.exportHtml", json!({"path": path})) {
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

#[cfg(not(target_arch = "wasm32"))]
fn connection(app: &SheetApp, ui: &mut Ui, t: &Tokens, p: &crate::Palette, read_only: bool, acts: &mut Vec<Act>) {
    use gridcraft_agent::cowork::{self, Configured, State};
    use gridcraft_agent::runner::{self, Mode};
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
    // Connect Cowork: GridCraft in Claude Desktop's MCP servers.
    let entry = crate::stdio_entry();
    let configured = match (&entry, cowork::config_path()) {
        (Some(e), Some(path)) => cowork::configured(&path, e),
        _ => Configured::NoClaude,
    };
    let state = cowork::state();
    let (sub, color) = match (&state, &configured) {
        (State::Waiting, _) => ("quit Claude (⌘Q)".to_string(), p.pending),
        (State::Done { .. }, _) => ("open Claude".to_string(), p.answered),
        (State::Failed(_), _) => ("failed".to_string(), p.material),
        (_, Configured::Yes) => ("connected".to_string(), p.answered),
        (_, Configured::Different) => ("other build".to_string(), p.pending),
        _ => ("not set up".to_string(), t.text_disabled),
    };
    let tip = match &state {
        State::Waiting => "Claude Desktop rewrites its settings when it quits, so GridCraft waits: quit Claude with ⌘Q, GridCraft adds itself, then open Claude again.".to_string(),
        State::Done { backup } => format!(
            "Added to Claude Desktop{}. Open Claude (or quit it with ⌘Q and open it again): the gridcraft tools appear in Cowork and chats.",
            backup.as_ref().map(|b| format!(" (old settings saved as {})", b.display())).unwrap_or_default()
        ),
        State::Failed(e) => e.clone(),
        State::Idle => "Add GridCraft to Claude Desktop, so Cowork and chats there can answer your questions. Other servers in its settings stay as they are.".to_string(),
    };
    let can = entry.is_some() && state != State::Waiting;
    if tall_button(ui, "Connect\nCowork", Some((&sub, color)), configured == Configured::Yes, can, &tip, plug).clicked()
        && let Some(e) = entry
    {
        acts.push(Act::Cowork(e));
    }
    column(ui, |ui| {
        match crate::server_url() {
            Some(u) => status_line(
                ui,
                &format!("Listening · {}", u.trim_start_matches("http://").trim_end_matches("/mcp")),
                p.answered,
                "The agent server (MCP over HTTP, this computer only)",
            ),
            None => status_line(ui, "Server off", t.text_disabled, "Start GridCraft without --no-agent"),
        }
        let last = gridcraft_agent::last_agent_call_ms();
        let active = last > 0 && gridcraft_agent::now_ms().saturating_sub(last) < 120_000;
        if active {
            status_line(ui, &format!("Agent active · {}", ago(last)), p.agent, "An agent called the layer in the last two minutes");
        } else if last > 0 {
            status_line(ui, &format!("Agent last seen {}", ago(last)), t.text_disabled, "");
        } else {
            status_line(ui, "No agent connected yet", t.text_disabled, "Connect Claude Code (Copy command) or Cowork");
        }
        let url = crate::server_url().unwrap_or("http://127.0.0.1:7980/mcp");
        let cmd = format!("claude mcp add --transport http gridcraft {url}");
        if row_button(ui, Icon::Copy, "Copy Claude Code command", &cmd, false).clicked() {
            acts.push(Act::Copy(cmd));
        }
    });
    // How questions reach the agent (GridCraft also starts the agent itself).
    let mode = runner::mode();
    let bg = runner::status(d.uid).unwrap_or_default();
    column(ui, |ui| {
        let name = match mode {
            Mode::Off => "Mode: I Connect",
            Mode::Channel => "Mode: My Session",
            Mode::Background => "Mode: Background",
        };
        let r = row_button(ui, Icon::Settings, name, "How questions reach the agent", true);
        egui::Popup::menu(&r).show(|ui| {
            for (m, label) in [(Mode::Off, "I connect my agent (MCP, Cowork, /events)"), (Mode::Channel, "Send to my Claude Code session (channel)"), (Mode::Background, "Background agent for this workbook")] {
                if ui.radio(mode == m, label).clicked() {
                    acts.push(Act::Run("agent.agentMode", json!({"mode": m.id()})));
                    ui.close();
                }
            }
            ui.separator();
            let exe = std::env::current_exe().map(|x| x.display().to_string()).unwrap_or_else(|_| "gridcraft".into());
            if ui.button("Copy channel setup (Claude Code)").clicked() {
                acts.push(Act::Copy(format!("claude mcp add gridcraft-channel -- '{exe}' --agent-stdio && claude --dangerously-load-development-channels server:gridcraft-channel")));
                ui.close();
            }
        });
        if mode == Mode::Background {
            let (text, color, detail) = if !bg.last_error.is_empty() && !bg.running {
                ("Background · stopped".to_string(), p.material, bg.last_error.clone())
            } else if bg.running && bg.in_flight > 0 {
                (format!("Background · working ({})", bg.in_flight), p.working, bg.recent.join("\n"))
            } else if bg.running {
                ("Background · running".to_string(), p.answered, bg.recent.join("\n"))
            } else {
                ("Background · ready".to_string(), t.text_dim, "Starts with your next question".to_string())
            };
            let detail = if bg.turns > 0 { format!("{detail}\n{} turns · ${:.2}", bg.turns, bg.cost_usd) } else { detail };
            status_line(ui, &text, color, &detail);
            let (label, id, tip) = if bg.running {
                ("Stop Agent", "agent.agentStop", "Stop the background agent (its session is kept)")
            } else {
                ("New Session", "agent.agentReset", "Start the background agent over, from a summary of this workbook's history")
            };
            if row_button(ui, Icon::Script, label, tip, false).clicked() {
                acts.push(Act::Run(id, json!({})));
            }
        }
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
