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

/// A large button that fits a group: icon, label, and a status line in its colour.
fn tall_button(ui: &mut Ui, icon: Icon, label: &str, status: Option<(&str, Color32)>, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(11.5);
    let lw = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x;
    let sw = status.map_or(0.0, |(s, _)| ui.painter().layout_no_wrap(s.to_string(), theme::ui_font(10.0), t.text).size().x);
    let (rect, resp) = ui.allocate_exact_size(vec2(lw.max(sw).max(40.0) + 14.0, BODY), Sense::click());
    if on {
        ui.painter().rect_filled(rect, 5.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, t.hover);
    }
    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.center().x, rect.top() + 15.0), vec2(24.0, 24.0)), icon, t.text);
    ui.painter().text(pos2(rect.center().x, rect.top() + 36.0), Align2::CENTER_CENTER, label, font, t.text);
    if let Some((s, c)) = status {
        ui.painter().text(pos2(rect.center().x, rect.top() + 50.0), Align2::CENTER_CENTER, s, theme::ui_bold(10.0), c);
    }
    resp.on_hover_text(tip)
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

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// Something the user clicked, done once drawing is finished.
enum Act {
    Run(&'static str, serde_json::Value),
    Pane(bool, Tab),
    Heat(Heat),
    CopyLink,
    Copy(String),
    Cowork,
    Share(String),
    OpenClaude,
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
        let status = if ready > 0 {
            (plural(ready, "answer") + " ready", p.answered)
        } else if waiting > 0 {
            (format!("{waiting} with the agent"), p.pending)
        } else {
            ("idle".to_string(), t.text_dim)
        };
        if tall_button(
            ui,
            Icon::Comment,
            "Agent Pane",
            Some((&status.0, status.1)),
            st.open,
            "Questions, answers, notes and analysis of the selected cells",
        )
        .clicked()
        {
            acts.push(Act::Pane(!st.open, if ready > 0 || waiting > 0 { Tab::Questions } else { Tab::Cell }));
        }
        column(ui, |ui| {
            if !read_only && row_button(ui, Icon::Comment, "Ask About Cell", "Ask your agent about the selected cells", false).clicked() {
                acts.push(Act::Pane(true, Tab::Cell));
            }
            let q = if l.questions.is_empty() { "Questions".to_string() } else { format!("Questions ({})", l.questions.len()) };
            if row_button(ui, Icon::Note, &q, "Every question and answer in this workbook", false).clicked() {
                acts.push(Act::Pane(true, Tab::Questions));
            }
            if row_button(ui, Icon::Book, "History", "Decisions and changes by you and the agent", false).clicked() {
                acts.push(Act::Pane(true, Tab::History));
            }
        });
    });

    // Voice: ask by speaking and pointing.
    #[cfg(not(target_arch = "wasm32"))]
    if !read_only {
        group(ui, "Voice", |ui| voice_group(st, ui, &t, &mut acts));
    }

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
            Act::Cowork => match app.run("agent.connectCowork", json!({})) {
                Ok(v) => app.message = Some(("Connect Cowork".into(), v.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string())),
                Err(e) => app.message = Some(("Connect Cowork".into(), e)),
            },
            Act::Share(path) => match app.run("agent.exportHtml", json!({"path": path})) {
                Ok(_) => app.toast = Some((format!("Saved {path}"), gridcraft_ui_egui::now_ms())),
                Err(e) => app.message = Some(("Export Web Page".into(), e)),
            },
            #[cfg(not(target_arch = "wasm32"))]
            Act::VoiceModel(m) => st.voice.choose(app, &ctx, m),
            #[cfg(not(target_arch = "wasm32"))]
            Act::VoiceToggle => st.voice.toggle(app, &ctx),
            Act::OpenClaude => {
                #[cfg(target_os = "macos")]
                {
                    let _ = std::process::Command::new("/usr/bin/open").args(["-a", "Claude"]).spawn();
                }
            }
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
    let Some(url) = crate::server_url() else {
        column(ui, |ui| status_line(ui, "No agent server", p.material, "Start GridCraft without --no-agent"));
        return;
    };
    let Some(d) = app.session.active() else { return };
    let mode = runner::mode();
    let bg = runner::status(d.uid).unwrap_or_default();
    column(ui, |ui| {
        let port = url.trim_end_matches("/mcp").rsplit(':').next().unwrap_or("");
        let (text, color, tip) = match mode {
            Mode::Background if !bg.last_error.is_empty() && !bg.running => ("Agent stopped".to_string(), p.material, bg.last_error.clone()),
            Mode::Background if bg.running && bg.in_flight > 0 => (format!("Agent working ({})", bg.in_flight), p.working, bg.recent.join("\n")),
            Mode::Background if bg.running => ("Agent running".to_string(), p.answered, bg.recent.join("\n")),
            Mode::Background => ("Background · ready".to_string(), t.text_dim, "Starts with your next question".to_string()),
            _ if gridcraft_agent::server::agent_connected() => ("Agent connected".to_string(), p.answered, format!("An agent is using {url}")),
            _ => (format!("Listening · {port}"), t.text_dim, format!("{url}: no agent connected yet")),
        };
        let tip = if bg.turns > 0 { format!("{tip}\n{} turns · ${:.2}", bg.turns, bg.cost_usd) } else { tip };
        status_line(ui, &text, color, &tip);
        let name = match mode {
            Mode::Off => "Mode: I Connect",
            Mode::Channel => "Mode: My Session",
            Mode::Background => "Mode: Background",
        };
        let r = row_button(ui, Icon::Settings, name, "How questions reach the agent", true);
        egui::Popup::menu(&r).show(|ui| {
            for (m, label) in [(Mode::Off, "I connect my agent (MCP, /events)"), (Mode::Channel, "Send to my Claude Code session (channel)"), (Mode::Background, "Background agent for this workbook")] {
                if ui.radio(mode == m, label).clicked() {
                    acts.push(Act::Run("agent.agentMode", json!({"mode": m.id()})));
                    ui.close();
                }
            }
            ui.separator();
            let exe = std::env::current_exe().map(|x| x.display().to_string()).unwrap_or_else(|_| "gridcraft".into());
            if ui.button("Copy channel setup (Claude Code)").on_hover_text("Add GridCraft as a channel server; then start Claude Code with --dangerously-load-development-channels server:gridcraft-channel").clicked() {
                acts.push(Act::Copy(format!("claude mcp add gridcraft-channel -- '{exe}' --agent-stdio && claude --dangerously-load-development-channels server:gridcraft-channel")));
                ui.close();
            }
            if ui.button("Copy event monitor command").clicked() {
                acts.push(Act::Copy(format!("curl -sN {}/events", url.trim_end_matches("/mcp"))));
                ui.close();
            }
        });
        if mode == Mode::Background {
            let (label, id, tip) = if bg.running {
                ("Stop Agent", "agent.agentStop", "Stop the background agent (its session is kept)")
            } else {
                ("New Session", "agent.agentReset", "Start the background agent over, from a summary of this workbook's history")
            };
            if row_button(ui, Icon::Script, label, tip, false).clicked() {
                acts.push(Act::Run(id, json!({})));
            }
        } else if row_button(ui, Icon::Copy, "Copy MCP Command", &format!("claude mcp add --transport http gridcraft {url}"), false).clicked() {
            acts.push(Act::Copy(format!("claude mcp add --transport http gridcraft {url}")));
        }
    });
    column(ui, |ui| {
        let connected = gridcraft_agent::cowork::connected();
        let tip = if connected {
            "GridCraft is in Claude Desktop's MCP servers (gridcraft). Click to check again"
        } else {
            "Add GridCraft to Claude Desktop and Cowork (claude_desktop_config.json). Claude must be closed while it is written: it rewrites the file when it quits"
        };
        if row_button(ui, Icon::Share, if connected { "Cowork Connected" } else { "Connect Cowork" }, tip, false).clicked() {
            acts.push(Act::Cowork);
        }
        if row_button(
            ui,
            Icon::Book,
            "Open Claude",
            "Open Claude Desktop: background sessions are in its Code tab, under this workbook's folder",
            false,
        )
        .clicked()
        {
            acts.push(Act::OpenClaude);
        }
        if mode == Mode::Background && !bg.session_id.is_empty() {
            let cmd = runner::resume_command(&bg);
            if row_button(
                ui,
                Icon::Copy,
                "Copy Resume",
                "Continue this conversation yourself (stop the background agent first: one writer per session)",
                false,
            )
            .clicked()
            {
                acts.push(Act::Copy(cmd));
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
    let resp = mic_button(ui, "Ask by Voice", (&status, color), v.recording(), &tip);
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

/// A large button with a microphone drawn in code (the icon set has none).
#[cfg(not(target_arch = "wasm32"))]
fn mic_button(ui: &mut Ui, label: &str, status: (&str, Color32), on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(11.5);
    let lw = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x;
    let sw = ui.painter().layout_no_wrap(status.0.to_string(), theme::ui_bold(10.0), t.text).size().x;
    let (rect, resp) = ui.allocate_exact_size(vec2(lw.max(sw).max(40.0) + 14.0, BODY), Sense::click());
    if on {
        ui.painter().rect_filled(rect, 5.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, t.hover);
    }
    let c = pos2(rect.center().x, rect.top() + 14.0);
    let ink = if on { Color32::from_rgb(0xD9, 0x30, 0x25) } else { t.text };
    let stroke = Stroke::new(1.6, ink);
    // Capsule, cradle, stem and base.
    let body = Rect::from_center_size(pos2(c.x, c.y - 3.0), vec2(8.0, 13.0));
    if on {
        ui.painter().rect_filled(body, 4.0, ink);
    } else {
        ui.painter().rect_stroke(body, 4.0, stroke, egui::StrokeKind::Middle);
    }
    let pts: Vec<egui::Pos2> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * (i as f32 / 16.0);
            pos2(c.x + 7.0 * a.cos(), c.y + 1.0 + 6.0 * a.sin())
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, stroke));
    ui.painter().line_segment([pos2(c.x, c.y + 7.0), pos2(c.x, c.y + 10.0)], stroke);
    ui.painter().line_segment([pos2(c.x - 4.0, c.y + 10.0), pos2(c.x + 4.0, c.y + 10.0)], stroke);
    ui.painter().text(pos2(rect.center().x, rect.top() + 36.0), Align2::CENTER_CENTER, label, font, t.text);
    ui.painter().text(pos2(rect.center().x, rect.top() + 50.0), Align2::CENTER_CENTER, status.0, theme::ui_bold(10.0), status.1);
    resp.on_hover_text(tip)
}
