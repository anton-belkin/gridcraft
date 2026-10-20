//! The Agent tab on the ribbon: what isn't about one cell. The pane, client data, what the agent
//! knows (Context), the heat map, sharing. Groups are named the same in WordCraft and DeckCraft:
//! Agent, Client data, Context, View (the heat map is GridCraft's), Share.

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
    Share(String),
    /// A step of Get ready.
    #[cfg(not(target_arch = "wasm32"))]
    Step(crate::context::Step),
    /// Open a topic (the lamp while the agent works on it).
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Topic(String),
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
        let (sub, color, detail) = status(app, &t, &p);
        let tip = format!("Show or hide the Agent pane: topics on the selection, their status, notes and discussion\n\n{detail}");
        #[cfg(not(target_arch = "wasm32"))]
        let working = app.session.active().is_some_and(|d| crate::context::working(d, gridcraft_agent::now_ms()).is_some());
        #[cfg(target_arch = "wasm32")]
        let working = false;
        let time = ui.input(|i| i.time);
        let resp = tall_button(ui, "Agent", Some((&sub, color)), st.open, true, tip.trim_end(), |pa, r, _| {
            sparkle(pa, r, p.agent);
            // The agent at work: the same small blue pulse as the lamp.
            if working {
                let k = ((time * 2.2).sin() * 0.5 + 0.5) as f32;
                let c = pos2(r.left() + 2.0, r.top() + 3.0);
                pa.circle_filled(c, 3.0 + 2.5 * k, crate::BLUE.gamma_multiply(0.35 * (1.0 - k) + 0.1));
                pa.circle_filled(c, 3.0, crate::BLUE);
            }
        });
        if working {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
        if resp.clicked() {
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

    // Context: what the agent knows about this workbook, and whether it's ready.
    group(ui, "Context", |ui| context_group(st, app, ui, &t, &p, read_only, &mut acts));

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
            #[cfg(not(target_arch = "wasm32"))]
            Act::Step(step) => crate::context::run(st, app, &ctx, step),
            Act::Topic(id) => {
                crate::open_topic(st, app, &id, None);
                st.pane.scroll_focus = true;
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
fn context_group(_st: &AgentUi, _app: &SheetApp, ui: &mut Ui, t: &Tokens, _p: &crate::Palette, _read_only: bool, _acts: &mut Vec<Act>) {
    column(ui, |ui| {
        status_line(
            ui,
            "Read-only copy",
            t.text_dim,
            "Exported from GridCraft with the answers and notes the author's agent prepared. No agent is connected here.",
        );
    });
}

/// Context: what the agent knows about this document. The mode button (with its Ready dot) and,
/// beside it, what the mode needs: Web, My session | Project folder, the Ready lamp, Get ready.
#[cfg(not(target_arch = "wasm32"))]
fn context_group(st: &AgentUi, app: &SheetApp, ui: &mut Ui, t: &Tokens, p: &crate::Palette, read_only: bool, acts: &mut Vec<Act>) {
    use crate::context::Lamp;
    use gridcraft_agent::layer::{CodeVia, ContextKind};
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
    let Some(v) = crate::context::current(st, app) else { return };
    let lamp = match v.lamp {
        Lamp::Working => crate::BLUE,
        Lamp::Ready => p.agreed,
        Lamp::Waiting => p.open,
        Lamp::NotReady => t.text_disabled,
    };
    let kind = v.ctx.kind;
    let (name, line, _) = kind.label();
    let r = mode_button(ui, kind, lamp, &format!("What the agent knows about this document: {name}. {line}"));
    egui::Popup::menu(&r).show(|ui| {
        for k in [ContextKind::None, ContextKind::Cowork, ContextKind::Code] {
            if mode_row(ui, k, k == kind).clicked() {
                if k != kind {
                    acts.push(Act::Run("agent.agentContext", json!({"kind": k.id()})));
                }
                ui.close();
            }
        }
    });
    column(ui, |ui| {
        match kind {
            ContextKind::None => {
                if check(ui, "Web", v.ctx.web, "Also allow the agent to search the web and read pages (WebSearch and WebFetch)").clicked() {
                    acts.push(Act::Run("agent.agentContext", json!({"web": !v.ctx.web})));
                }
            }
            ContextKind::Code => {
                let on = usize::from(v.ctx.via == CodeVia::Folder);
                let picked = segmented(
                    ui,
                    &[
                        ("My session", "Questions arrive in your own Claude Code session, through the AItoCraft channel"),
                        ("Project folder", "A Claude Code agent runs in a folder you choose: it sees that folder's CLAUDE.md, memory and files"),
                    ],
                    on,
                );
                if let Some(i) = picked.filter(|i| *i != on) {
                    acts.push(Act::Run("agent.agentContext", json!({"via": if i == 1 { "folder" } else { "session" }})));
                }
            }
            _ => {}
        }
        let working = v.lamp == Lamp::Working;
        if lamp_row(ui, &v.label, lamp, &v.detail, working).clicked()
            && let Some(t) = &v.topic
        {
            acts.push(Act::Topic(t.clone()));
        }
        if !v.steps.is_empty() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let next = v.next();
                let tip = next.map_or(String::new(), |s| format!("Next: {}. {}", s.label(), s.tip()));
                if row_button(ui, Icon::Right, "Get ready", &tip, false).clicked()
                    && let Some(s) = next
                {
                    acts.push(Act::Step(s));
                }
                let arrow = chevron_button(ui, "Every step");
                egui::Popup::menu(&arrow).show(|ui| {
                    for (s, done) in &v.steps {
                        if step_row(ui, s.label(), *done, s.tip()).clicked() {
                            acts.push(Act::Step(*s));
                            ui.close();
                        }
                    }
                });
                if kind == ContextKind::Code
                    && v.ctx.via == CodeVia::Folder
                    && let Some(f) = &v.folder
                {
                    ui.add_space(6.0);
                    let short = short_path(f, 34);
                    ui.label(egui::RichText::new(short).font(theme::ui_font(11.0)).color(t.text_dim))
                        .on_hover_text(format!("{f}\nThe agent runs here: it sees this folder's CLAUDE.md, memory and files"));
                }
            });
        }
    });
}

/// `~/deals/acme`, its end kept when long.
#[cfg(not(target_arch = "wasm32"))]
fn short_path(f: &str, n: usize) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let f = match f.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => f.to_string(),
    };
    let k = f.chars().count();
    if k <= n { f } else { format!("…{}", f.chars().skip(k - n + 1).collect::<String>()) }
}

/// The mode, large: its drawn sign with the Ready dot, its name, a chevron for the menu.
#[cfg(not(target_arch = "wasm32"))]
fn mode_button(ui: &mut Ui, kind: gridcraft_agent::layer::ContextKind, lamp: Color32, tip: &str) -> Response {
    use gridcraft_agent::layer::ContextKind;
    let t = Tokens::get(ui.ctx());
    let label = match kind {
        ContextKind::None => "No\ncontext",
        ContextKind::Cowork => "Claude\nCowork",
        ContextKind::Code => "Claude\nCode",
        ContextKind::Off => "Agent\noff",
    };
    let font = theme::ui_font(11.0);
    let w = label.split('\n').map(|l| ui.painter().layout_no_wrap(l.to_string(), font.clone(), t.text).size().x).fold(0.0, f32::max);
    let (rect, resp) = ui.allocate_exact_size(vec2(w.max(40.0) + 14.0, BODY), Sense::click());
    if resp.is_pointer_button_down_on() {
        ui.painter().rect_filled(rect, 5.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, t.hover);
    }
    let icon = Rect::from_center_size(pos2(rect.center().x, rect.top() + 2.0 + 10.0), vec2(20.0, 20.0));
    mode_sign(ui.painter(), icon, kind, t.text);
    // The Ready dot, on the sign's corner.
    let dot = pos2(icon.right() + 1.0, icon.top() + 2.0);
    ui.painter().circle_filled(dot, 4.6, t.ribbon);
    ui.painter().circle_filled(dot, 3.4, lamp);
    let mut y = rect.top() + 20.0 + 9.0;
    for l in label.split('\n') {
        ui.painter().text(pos2(rect.center().x, y), Align2::CENTER_CENTER, l, font.clone(), t.text);
        y += 11.5;
    }
    icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.center().x, y - 1.0), vec2(9.0, 9.0)), Icon::Chevron, t.text_dim);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.replace('\n', " ")));
    resp.on_hover_text(tip)
}

/// A sign per mode, drawn: a page (only the document), a chat bubble (Cowork), a prompt (Code).
#[cfg(not(target_arch = "wasm32"))]
fn mode_sign(p: &egui::Painter, r: Rect, kind: gridcraft_agent::layer::ContextKind, color: Color32) {
    use gridcraft_agent::layer::ContextKind;
    let s = Stroke::new(1.5, color);
    match kind {
        ContextKind::None => {
            let page = Rect::from_center_size(r.center(), vec2(r.width() * 0.66, r.height() * 0.86));
            let fold = page.width() * 0.32;
            let pts = vec![
                page.left_top(),
                pos2(page.right() - fold, page.top()),
                pos2(page.right(), page.top() + fold),
                page.right_bottom(),
                page.left_bottom(),
                page.left_top(),
            ];
            p.add(egui::Shape::line(pts, s));
            p.line_segment([pos2(page.right() - fold, page.top()), pos2(page.right() - fold, page.top() + fold)], s);
            p.line_segment([pos2(page.right() - fold, page.top() + fold), pos2(page.right(), page.top() + fold)], s);
            for k in 0..3 {
                let y = page.top() + page.height() * (0.48 + 0.16 * k as f32);
                p.line_segment([pos2(page.left() + 3.0, y), pos2(page.right() - 3.0, y)], Stroke::new(1.2, color));
            }
        }
        ContextKind::Cowork => {
            let b = Rect::from_min_max(pos2(r.left() + 1.0, r.top() + 3.0), pos2(r.right() - 1.0, r.bottom() - 6.0));
            p.rect_stroke(b, 4.0, s, egui::StrokeKind::Middle);
            p.add(egui::Shape::line(
                vec![pos2(b.left() + 5.0, b.bottom()), pos2(b.left() + 4.0, r.bottom() - 1.0), pos2(b.left() + 10.0, b.bottom())],
                s,
            ));
            for k in 0..3 {
                p.circle_filled(pos2(b.center().x + (k as f32 - 1.0) * 4.5, b.center().y), 1.3, color);
            }
        }
        ContextKind::Code => {
            let b = r.shrink2(vec2(0.5, 2.5));
            p.rect_stroke(b, 3.0, s, egui::StrokeKind::Middle);
            let x = b.left() + 4.0;
            let y = b.center().y;
            p.add(egui::Shape::line(vec![pos2(x, y - 3.5), pos2(x + 4.0, y), pos2(x, y + 3.5)], s));
            p.line_segment([pos2(x + 6.0, y + 3.5), pos2(x + 11.0, y + 3.5)], s);
        }
        ContextKind::Off => {
            p.circle_stroke(r.center(), r.width() * 0.4, s);
            let d = r.width() * 0.28;
            p.line_segment([r.center() + vec2(-d, d), r.center() + vec2(d, -d)], s);
        }
    }
}

/// A row of the mode menu: the name, one line, chips for what the agent knows.
#[cfg(not(target_arch = "wasm32"))]
fn mode_row(ui: &mut Ui, kind: gridcraft_agent::layer::ContextKind, on: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let (name, line, chips) = kind.label();
    let (rect, resp) = ui.allocate_exact_size(vec2(320.0, 58.0), Sense::click());
    if on {
        ui.painter().rect_filled(rect, 5.0, t.pressed);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 5.0, t.hover);
    }
    let sign = Rect::from_center_size(pos2(rect.left() + 18.0, rect.top() + 16.0), vec2(18.0, 18.0));
    mode_sign(ui.painter(), sign, kind, t.text);
    let x = rect.left() + 36.0;
    ui.painter().text(pos2(x, rect.top() + 11.0), Align2::LEFT_CENTER, name, theme::ui_bold(12.5), t.text);
    ui.painter().text(pos2(x, rect.top() + 27.0), Align2::LEFT_CENTER, line, theme::ui_font(11.0), t.text_dim);
    let mut cx = x;
    for c in chips {
        let g = ui.painter().layout_no_wrap(c.to_string(), theme::ui_font(10.0), t.text);
        let chip = Rect::from_min_size(pos2(cx, rect.top() + 37.0), vec2(g.size().x + 12.0, 15.0));
        ui.painter().rect_filled(chip, 7.5, t.window);
        ui.painter().rect_stroke(chip, 7.5, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        ui.painter().galley(chip.center() - g.size() / 2.0, g, t.text);
        cx = chip.right() + 4.0;
    }
    if on {
        icons::paint(ui.painter(), Rect::from_center_size(pos2(rect.right() - 14.0, rect.top() + 16.0), vec2(12.0, 12.0)), Icon::Check, t.accent);
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, on, name));
    resp
}

/// A checkbox as a ribbon row.
#[cfg(not(target_arch = "wasm32"))]
fn check(ui: &mut Ui, label: &str, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(12.0);
    let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 26.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let b = Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(11.0, 11.0));
    if on {
        ui.painter().rect_filled(b, 2.0, t.accent);
        icons::paint(ui.painter(), b.shrink(1.0), Icon::Check, Color32::WHITE);
    } else {
        ui.painter().rect_stroke(b, 2.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Inside);
    }
    ui.painter().text(pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, label, font, t.text);
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, label));
    resp.on_hover_text(tip)
}

/// Two or three choices side by side; the one clicked.
#[cfg(not(target_arch = "wasm32"))]
fn segmented(ui: &mut Ui, items: &[(&str, &str)], on: usize) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_font(11.5);
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let n = items.len();
        for (i, (label, tip)) in items.iter().enumerate() {
            let w = ui.painter().layout_no_wrap(label.to_string(), font.clone(), t.text).size().x + 16.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), Sense::click());
            let cr = egui::CornerRadius {
                nw: if i == 0 { 4 } else { 0 },
                sw: if i == 0 { 4 } else { 0 },
                ne: if i + 1 == n { 4 } else { 0 },
                se: if i + 1 == n { 4 } else { 0 },
            };
            let fill = if i == on {
                t.pressed
            } else if resp.hovered() {
                t.hover
            } else {
                t.window
            };
            ui.painter().rect_filled(rect, cr, fill);
            ui.painter().rect_stroke(rect, cr, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, *label, font.clone(), if i == on { t.text } else { t.text_dim });
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, i == on, *label));
            if resp.on_hover_text(*tip).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// The Ready lamp: a dot and a word, the details on hover. While the agent works it pulses (the
/// only thing that repaints for it, at about 30 fps) and a click opens the topic.
#[cfg(not(target_arch = "wasm32"))]
fn lamp_row(ui: &mut Ui, text: &str, color: Color32, tip: &str, working: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = theme::ui_bold(12.0);
    let w = ui.painter().layout_no_wrap(text.to_string(), font.clone(), t.text).size().x + 22.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 19.0), if working { Sense::click() } else { Sense::hover() });
    let c = pos2(rect.left() + 9.0, rect.center().y);
    if working {
        if resp.hovered() {
            ui.painter().rect_filled(rect, 4.0, t.hover);
        }
        pulse(ui, c, 4.5, color);
    } else {
        ui.painter().circle_filled(c, 4.5, color);
    }
    ui.painter().text(pos2(rect.left() + 20.0, rect.center().y), Align2::LEFT_CENTER, text, font, if working { color } else { t.text });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    resp.on_hover_text(tip)
}

/// A blue light that breathes: a halo growing and fading around a steady dot. Repaints itself at
/// about 30 fps while shown.
#[cfg(not(target_arch = "wasm32"))]
fn pulse(ui: &Ui, c: egui::Pos2, r: f32, color: Color32) {
    let time = ui.input(|i| i.time);
    let k = ((time * 2.2).sin() * 0.5 + 0.5) as f32;
    ui.painter().circle_filled(c, r + 3.5 * k, color.gamma_multiply(0.35 * (1.0 - k) + 0.1));
    ui.painter().circle_filled(c, r, color.gamma_multiply(0.75 + 0.25 * k));
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
}

/// The ▾ half of a split button.
#[cfg(not(target_arch = "wasm32"))]
fn chevron_button(ui: &mut Ui, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 19.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    icons::paint(ui.painter(), Rect::from_center_size(rect.center() + vec2(0.0, 1.0), vec2(9.0, 9.0)), Icon::Chevron, t.text_dim);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
    resp.on_hover_text(tip)
}

/// A step in Get ready's menu: ticked when done.
#[cfg(not(target_arch = "wasm32"))]
fn step_row(ui: &mut Ui, label: &str, done: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let (rect, resp) = ui.allocate_exact_size(vec2(240.0, 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    let c = pos2(rect.left() + 11.0, rect.center().y);
    if done {
        icons::paint(ui.painter(), Rect::from_center_size(c, vec2(13.0, 13.0)), Icon::Check, p.agreed);
    } else {
        ui.painter().circle_stroke(c, 5.0, Stroke::new(1.0, t.text_dim));
    }
    ui.painter().text(
        pos2(rect.left() + 24.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::ui_font(12.0),
        if done { t.text_dim } else { t.text },
    );
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, done, label));
    resp.on_hover_text(tip)
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
