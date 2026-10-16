//! The Agent pane: the selected cell (ask, answers, notes, where the number comes from, its
//! sensitivity), all questions, all notes, the history, and how to connect an agent.

use egui::{Color32, RichText, Ui, vec2};
use gridcraft_agent::analysis;
use gridcraft_agent::anchor::{self, Place};
use gridcraft_agent::layer::{self, Actor, Layer, Link, Note, Question, QuestionStatus, VariantKind, VariantSet};
use gridcraft_agent::stress::{self, Stress};
use gridcraft_agent::variants::Comparison;
use gridcraft_engine::core::{CellRef, RangeRef};
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::theme::{self, Tokens};
use serde_json::{Value as Json, json};

use crate::overlay::{self, clip, kind_label};
use crate::{AgentUi, Heat, Palette, Tab, fmt_at, fmt_num, follow, palette};

#[derive(Default)]
pub(crate) struct State {
    pub trace_open: bool,
    /// What the last Share as Web Page said (the file written, or why not).
    shared: Option<String>,
    trace: Option<((u64, usize, u64), usize, CellRef, Json)>,
}

/// Something the user clicked, acted on once drawing is done (drawing borrows the app).
enum Act {
    Follow(String),
    Run(&'static str, Json),
    /// Export the workbook as a web page to this path.
    Share(String),
    CopyLink,
}

pub(crate) fn show(st: &mut AgentUi, app: &mut SheetApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let max = (ui.available_width() / 2.0).max(320.0);
    let mut acts: Vec<Act> = vec![];
    let mut close = false;
    egui::Panel::right("agent_pane")
        .default_size(340.0)
        .min_size(280.0)
        .max_size(max)
        .resizable(true)
        .frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Agent").font(theme::ui_bold(15.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if gridcraft_ui_egui::widgets::icon_button(ui, gridcraft_ui_egui::icons::Icon::Close, t.text_dim, "Close", vec2(22.0, 22.0))
                        .clicked()
                    {
                        close = true;
                    }
                });
            });
            let Some(d) = app.session.active() else { return };
            let l = layer::get(d);
            let ready = l.questions.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
            ui.horizontal_wrapped(|ui| {
                for (tab, name) in [
                    (Tab::Cell, "Cell".to_string()),
                    (Tab::Questions, if ready > 0 { format!("Questions ({ready})") } else { "Questions".into() }),
                    (Tab::Notes, "Notes".into()),
                    (Tab::Variants, if l.variants.is_empty() { "Variants".into() } else { format!("Variants ({})", l.variants.len()) }),
                    (Tab::Risk, "Risk".into()),
                    (Tab::History, "History".into()),
                    (Tab::Connect, "Connect".into()),
                ] {
                    if ui.selectable_label(st.tab == tab, name).clicked() {
                        st.tab = tab;
                    }
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match st.tab {
                Tab::Cell => cell_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::Questions => questions_tab(app, ui, &l, &t, &p, &mut acts),
                Tab::Notes => notes_tab(st, app, ui, &l, &p, &mut acts),
                Tab::Variants => variants_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::Risk => risk_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::History => history_tab(app, ui, &l, &t, &p, &mut acts),
                Tab::Connect => connect_tab(st, app, ui, &t, &p, &mut acts),
            });
        });
    if close {
        st.open = false;
    }
    let ctx = ui.ctx().clone();
    for a in acts {
        match a {
            Act::Follow(href) => follow(st, app, &ctx, &href),
            Act::Run(id, params) => app.run_or_alert(id, params),
            Act::Share(path) => {
                st.pane.shared = Some(match app.run("agent.exportHtml", json!({"path": path})) {
                    Ok(_) => format!("Saved {path}"),
                    Err(e) => e.to_string(),
                });
            }
            Act::CopyLink => app.copy_to_clipboard(&ctx, "link.copy"),
        }
    }
}

fn place_of(app: &SheetApp, l: &Layer, anchor_id: &str) -> Option<Place> {
    let d = app.session.active()?;
    l.anchor(anchor_id).and_then(|a| anchor::resolve(&d.wb, a))
}

fn links(ui: &mut Ui, list: &[Link], acts: &mut Vec<Act>) {
    if list.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for k in list {
            // Glyphs the web build's fonts have too.
            let icon = if k.href.starts_with("cell:") {
                "» "
            } else if k.href.starts_with("note:") {
                "¶ "
            } else {
                "» "
            };
            let mut r = ui.link(format!("{icon}{}", k.label));
            if !k.summary.is_empty() {
                r = r.on_hover_text(&k.summary);
            } else {
                r = r.on_hover_text(&k.href);
            }
            if r.clicked() {
                acts.push(Act::Follow(k.href.clone()));
            }
        }
    });
}

fn card(ui: &mut Ui, t: &Tokens, stripe: Color32, add: impl FnOnce(&mut Ui)) {
    egui::Frame::NONE.fill(t.window).corner_radius(6.0).inner_margin(8.0).stroke(egui::Stroke::new(1.0, t.separator)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let top = ui.cursor().min;
        add(ui);
        let bottom = ui.min_rect().bottom();
        ui.painter().line_segment([egui::pos2(top.x - 6.0, top.y), egui::pos2(top.x - 6.0, bottom)], egui::Stroke::new(2.5, stripe));
    });
    ui.add_space(6.0);
}

fn question_card(app: &SheetApp, ui: &mut Ui, l: &Layer, q: &Question, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, show_place: bool) {
    let (status, color) = match q.status {
        QuestionStatus::Pending => ("Waiting for the agent", p.pending),
        QuestionStatus::Working => ("The agent is working on it", p.working),
        QuestionStatus::Answered if !q.seen => ("Answer ready", p.answered),
        QuestionStatus::Answered => ("Answered", p.seen),
    };
    card(ui, t, color, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(status).color(color).small().strong());
            if show_place
                && let Some(pl) = place_of(app, l, &q.anchor)
                && ui.link(RichText::new(pl.a1()).small()).clicked()
            {
                acts.push(Act::Follow(format!("cell:{}", pl.a1())));
            }
        });
        ui.label(RichText::new(&q.text).italics());
        if let Some(a) = &q.answer {
            ui.add_space(3.0);
            ui.label(&a.text);
            links(ui, &a.links, acts);
            if !q.seen && ui.small_button("Got it").clicked() {
                acts.push(Act::Run("agent.seen", json!({"id": q.id})));
            }
        }
    });
}

fn note_card(app: &SheetApp, ui: &mut Ui, l: &Layer, n: &Note, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, show_place: bool, highlight: bool) {
    let stripe = if highlight { p.answered } else { p.note };
    card(ui, t, stripe, |ui| {
        ui.horizontal_wrapped(|ui| {
            let title = if n.title.is_empty() { kind_label(&n.kind).to_string() } else { format!("{} · {}", kind_label(&n.kind), n.title) };
            ui.label(RichText::new(title).color(p.note).small().strong());
            if show_place
                && let Some(pl) = place_of(app, l, &n.anchor)
                && ui.link(RichText::new(pl.a1()).small()).clicked()
            {
                acts.push(Act::Follow(format!("cell:{}", pl.a1())));
            }
        });
        ui.label(&n.text);
        links(ui, &n.links, acts);
    });
}

fn cell_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let Some(sh) = d.wb.sheet(si) else { return };
    let sel = d.selection.current();
    let c = d.selection.active;
    let here = Place { sheet: si, sheet_name: sh.name.clone(), range: sel };
    let label = analysis::label_of(sh, c);
    let text = gridcraft_engine::display::cell_text(&d.wb, sh, c);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(here.a1()).strong());
        if !label.is_empty() {
            ui.label(RichText::new(&label).color(t.text_dim));
        }
        if !text.is_empty() {
            ui.label(RichText::new(format!("= {text}")).color(t.text));
        }
    });
    if let Some(f) = sh.cell(c).and_then(|x| x.formula.as_ref()) {
        ui.label(RichText::new(format!("={}", f.text.trim_start_matches('='))).monospace().small().color(t.text_dim));
    }
    let private_cells = analysis::private_cells(&d.wb, l);
    let is_private = private_cells.contains(&(si, c));
    if is_private {
        ui.label(RichText::new("Client data · your agent sees stand-in values here").color(p.private).small());
    }
    ui.add_space(6.0);

    // Ask (not in a read-only copy: the author's agent isn't reachable from it).
    if gridcraft_agent::read_only() {
        ask_closed(ui, t);
    } else {
        ask_box(st, ui, t, &here, acts);
    }
    ui.add_space(6.0);

    let hits = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a)).is_some_and(|pl| pl.sheet == si && pl.range.intersects(&sel));
    let qs: Vec<&Question> = l.questions.iter().filter(|q| hits(&q.anchor)).collect();
    for q in &qs {
        question_card(app, ui, l, q, t, p, acts, false);
        // Reading it here is reading it.
        if q.status == QuestionStatus::Answered && !q.seen {
            acts.push(Act::Run("agent.seen", json!({"id": q.id})));
        }
    }
    for n in l.notes.iter().filter(|n| hits(&n.anchor)) {
        note_card(app, ui, l, n, t, p, acts, false, false);
    }
    if let Some(i) = l.inputs.iter().find(|i| hits(&i.anchor)) {
        let range = if i.relative {
            format!("{:+.0}% … {:+.0}%", (i.low - 1.0) * 100.0, (i.high - 1.0) * 100.0)
        } else {
            format!("{} … {}", fmt_num(i.low), fmt_num(i.high))
        };
        card(ui, t, p.input, |ui| {
            ui.label(RichText::new(format!("Input · plausible range {range}")).color(p.input).small().strong());
            if !i.rationale.is_empty() {
                ui.label(&i.rationale);
            }
        });
    }
    if let Some(conf) = l.confidence.iter().find(|x| hits(&x.anchor)) {
        card(ui, t, overlay::heat_color(conf.level, 255), |ui| {
            ui.label(RichText::new(format!("Agent's confidence {:.0}%", conf.level * 100.0)).small().strong());
            if !conf.reason.is_empty() {
                ui.label(&conf.reason);
            }
        });
    }

    // Slides that use these cells.
    let slides = gridcraft_agent::links::uses_at(&d.wb, l, si, Some(&sel));
    if !slides.is_empty() {
        card(ui, t, p.slide, |ui| {
            ui.label(
                RichText::new(format!("Used on {} slide{}", slides.len(), if slides.len() == 1 { "" } else { "s" })).color(p.slide).small().strong(),
            );
            for (pl, u) in &slides {
                ui.horizontal_wrapped(|ui| {
                    let title = if u.slide_title.is_empty() { String::new() } else { format!(" · {}", u.slide_title) };
                    ui.label(RichText::new(format!("{} · slide {}{title}", u.deck_title, u.slide)).small());
                    if ui.small_button("Go to slide").on_hover_text(format!("{} ({}, {})", u.deck, pl.a1(), u.mode)).clicked() {
                        acts.push(Act::Run("link.gotoSlide", json!({"link": u.link})));
                    }
                });
            }
        });
    }

    // Alternatives and calculation methods on the selection.
    if l.variants.iter().any(|v| hits(&v.anchor)) {
        for c in st.overlay.comparisons(d, l).iter().filter(|c| c.place.sheet == si && c.place.range.intersects(&sel)) {
            if let Some(set) = l.variants.iter().find(|v| v.id == c.id) {
                variant_card(app, ui, set, c, t, p, acts, false);
            }
        }
    }

    // Where does this number come from.
    let is_formula = sh.cell(c).is_some_and(|x| x.formula.is_some());
    if is_formula {
        let ver = overlay::version(d, l);
        let fresh = st.pane.trace.as_ref().is_some_and(|(v, s, k, _)| *v == ver && *s == si && *k == c);
        if !fresh {
            let derived = analysis::derived_from(&d.calc, &private_cells);
            let tree = analysis::trace(&d.wb, &d.calc, (si, c), 8, &private_cells, &derived);
            st.pane.trace = Some((ver, si, c, tree));
        }
        let open = std::mem::take(&mut st.pane.trace_open);
        let h = egui::CollapsingHeader::new(RichText::new("Where does this number come from?").strong()).id_salt("agent_trace");
        let h = if open { h.open(Some(true)) } else { h };
        h.show(ui, |ui| {
            if let Some((_, _, _, tree)) = &st.pane.trace {
                trace_node(ui, tree, t, p, acts, 0);
            }
        });
        // Sensitivity.
        let s = st.overlay.sensitivity(d, l, si, c);
        if !s.swings.is_empty() {
            egui::CollapsingHeader::new(RichText::new("Sensitivity").strong()).id_salt("agent_sens").default_open(true).show(ui, |ui| {
                overlay::tornado(ui, &s, t, ui.available_width().min(420.0), 8);
                for w in s.swings.iter().take(8) {
                    let place = Place {
                        sheet: w.input.0,
                        sheet_name: d.wb.sheet(w.input.0).map(|x| x.name.clone()).unwrap_or_default(),
                        range: RangeRef::cell(w.input.1),
                    };
                    ui.horizontal_wrapped(|ui| {
                        if ui.link(RichText::new(if w.label.is_empty() { place.a1() } else { w.label.clone() }).small()).clicked() {
                            acts.push(Act::Follow(format!("cell:{}", place.a1())));
                        }
                        ui.label(
                            RichText::new(format!(
                                "{} … {} gives {} … {}",
                                fmt_num(w.low_input),
                                fmt_num(w.high_input),
                                fmt_num(w.at_low),
                                fmt_num(w.at_high)
                            ))
                            .small()
                            .color(t.text_dim),
                        );
                        if !w.reasoned {
                            ui.label(RichText::new("default ±10%").small().italics().color(t.text_dim));
                        }
                    });
                    if !w.rationale.is_empty() {
                        ui.label(RichText::new(clip(&w.rationale, 160)).small());
                    }
                }
            });
        }
    }
    if is_formula
        && overlay::is_formula_number(d, si, c)
        && let Some(r) = st.overlay.stress(d, l, si, c)
    {
        let n = r.dims.len();
        egui::CollapsingHeader::new(RichText::new(format!("Worst and best case · {n} input{} together", if n == 1 { "" } else { "s" })).strong())
            .id_salt("agent_stress")
            .default_open(true)
            .show(ui, |ui| stress_view(ui, d, &r, t, p, acts));
    }
    ui.add_space(8.0);
    ui.separator();
    if !gridcraft_agent::read_only() {
        ui.horizontal_wrapped(|ui| {
            if ui.button(if is_private { "Unmark client data" } else { "Mark selection as client data" }).clicked() {
                acts.push(Act::Run("agent.private", json!({"range": here.a1(), "on": !is_private})));
            }
        });
        safe_file(ui, d, l, t, p, acts);
        if ui
            .button("Copy link to these cells for a slide")
            .on_hover_text("⌘⌥C · paste it on a DeckCraft slide: the slide stays linked to these cells")
            .clicked()
        {
            acts.push(Act::CopyLink);
        }
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Heat map").small());
        for (h, name) in [(Heat::Off, "Off"), (Heat::Confidence, "Confidence"), (Heat::Impact, "Impact on this cell")] {
            if ui.selectable_label(st.heat == h, name).clicked() {
                st.heat = h;
            }
        }
    });
}

fn ask_box(st: &mut AgentUi, ui: &mut Ui, t: &Tokens, here: &Place, acts: &mut Vec<Act>) {
    ui.label(RichText::new("Ask the agent about this").small().strong());
    let r = ui.add(
        egui::TextEdit::multiline(&mut st.draft).hint_text("Why is this growing faster than revenue?").desired_rows(2).desired_width(f32::INFINITY),
    );
    if st.focus_draft {
        r.request_focus();
        st.focus_draft = false;
    }
    let send_key = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
    ui.horizontal(|ui| {
        let can = !st.draft.trim().is_empty();
        if (ui.add_enabled(can, egui::Button::new("Ask")).clicked() || send_key) && can {
            acts.push(Act::Run("agent.ask", json!({"range": here.a1(), "text": st.draft.trim()})));
            st.draft.clear();
        }
        let send = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+Enter" };
        ui.label(RichText::new(format!("{send} · keep working, the badge tells you when it's answered")).small().color(t.text_dim));
    });
}

fn ask_closed(ui: &mut Ui, t: &Tokens) {
    ui.label(
        RichText::new(
            "Read-only copy: the answers and notes the author's agent prepared are here; new questions go through the workbook in GridCraft.",
        )
        .small()
        .color(t.text_dim),
    );
}

fn trace_node(ui: &mut Ui, n: &Json, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, depth: usize) {
    let cell = n.get("cell").and_then(Json::as_str).or_else(|| n.get("range").and_then(Json::as_str)).unwrap_or("");
    let label = n.get("label").and_then(Json::as_str).unwrap_or("");
    let text = n.get("text").and_then(Json::as_str).unwrap_or("");
    let formula = n.get("formula").and_then(Json::as_str);
    let private = n.get("private").and_then(Json::as_bool).unwrap_or(false);
    let kind = n.get("kind").and_then(Json::as_str).unwrap_or("");
    let children: Vec<Json> = n.get("reads").or_else(|| n.get("cells")).and_then(Json::as_array).cloned().unwrap_or_default();
    let head = |ui: &mut Ui, acts: &mut Vec<Act>| {
        ui.horizontal_wrapped(|ui| {
            if ui.link(RichText::new(short_cell(cell)).small().strong()).clicked() {
                acts.push(Act::Follow(format!("cell:{cell}")));
            }
            if !label.is_empty() {
                ui.label(RichText::new(clip(label, 40)).small());
            }
            if !text.is_empty() {
                ui.label(RichText::new(text).small().color(t.text));
            }
            if kind == "input" {
                ui.label(RichText::new("input").small().color(p.input));
            }
            if private {
                ui.label(RichText::new("client data").small().color(p.private));
            }
            if let Some(filled) = n.get("filled").and_then(Json::as_u64) {
                ui.label(RichText::new(format!("{filled} cells")).small().color(t.text_dim));
            }
        });
        if let Some(f) = formula {
            ui.label(RichText::new(f).monospace().small().color(t.text_dim));
        }
    };
    if children.is_empty() {
        ui.indent(("leaf", cell, depth), |ui| head(ui, acts));
        return;
    }
    let id = ui.make_persistent_id(("trace", cell, depth));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, depth < 2)
        .show_header(ui, |ui| {
            ui.vertical(|ui| head(ui, acts));
        })
        .body(|ui| {
            for c in &children {
                trace_node(ui, c, t, p, acts, depth + 1);
            }
        });
}

fn short_cell(a1: &str) -> String {
    a1.to_string()
}

fn questions_tab(app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if l.questions.is_empty() {
        ui.label(
            RichText::new(if gridcraft_agent::read_only() {
                "No prepared answers in this copy."
            } else {
                "No questions yet. Right-click a cell and choose Ask the Agent, or use the Cell tab."
            })
            .italics(),
        );
        return;
    }
    let order = |q: &Question| match (q.status, q.seen) {
        (QuestionStatus::Answered, false) => 0,
        (QuestionStatus::Working, _) => 1,
        (QuestionStatus::Pending, _) => 2,
        _ => 3,
    };
    let mut list: Vec<&Question> = l.questions.iter().collect();
    list.sort_by_key(|q| (order(q), std::cmp::Reverse(q.asked_at)));
    for q in list {
        question_card(app, ui, l, q, t, p, acts, true);
    }
}

fn notes_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, p: &Palette, acts: &mut Vec<Act>) {
    let t = Tokens::get(ui.ctx());
    if l.notes.is_empty() {
        ui.label(RichText::new("The agent hasn't left notes in this workbook yet.").italics());
        return;
    }
    let target = st.highlight_note.take();
    for kind in ["explanation", "assumption", "source", "faq"] {
        let list: Vec<&Note> = l.notes.iter().filter(|n| n.kind == kind).collect();
        if list.is_empty() {
            continue;
        }
        ui.label(
            RichText::new(match kind {
                "faq" => "Prepared answers",
                "assumption" => "Assumptions",
                "source" => "Sources",
                _ => "Explanations",
            })
            .strong(),
        );
        for n in list {
            let hl = target.as_deref() == Some(n.id.as_str());
            note_card(app, ui, l, n, &t, p, acts, true, hl);
            if hl {
                ui.scroll_to_cursor(Some(egui::Align::Center));
            }
        }
    }
}

fn pct(x: f64) -> String {
    if !x.is_finite() {
        "∞".into()
    } else if x < 0.001 {
        format!("{:.2}%", x * 100.0)
    } else {
        format!("{:.1}%", x * 100.0)
    }
}

/// A set of alternatives or a calculation method: switch between the options, and see what each
/// does to the results.
#[allow(clippy::too_many_arguments)]
fn variant_card(app: &SheetApp, ui: &mut Ui, set: &VariantSet, c: &Comparison, t: &Tokens, p: &Palette, acts: &mut Vec<Act>, show_place: bool) {
    let method = c.kind == VariantKind::Method;
    let stripe = if method && c.material() { p.material } else { p.variant };
    let Some(d) = app.session.active() else { return };
    card(ui, t, stripe, |ui| {
        ui.horizontal_wrapped(|ui| {
            let head = match (method, c.title.is_empty()) {
                (true, true) => "Calculation method".to_string(),
                (true, false) => format!("Calculation method · {}", c.title),
                (false, true) => "Alternatives · A or B?".to_string(),
                (false, false) => format!("Alternatives · {}", c.title),
            };
            ui.label(RichText::new(head).color(p.variant).small().strong());
            if show_place && ui.link(RichText::new(c.place.a1()).small()).clicked() {
                acts.push(Act::Follow(format!("cell:{}", c.place.a1())));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(if method { "Use" } else { "In the model:" }).small().color(t.text_dim));
            for (i, name) in c.names.iter().enumerate() {
                let on = c.current == Some(i);
                let text = if on { RichText::new(name).strong() } else { RichText::new(name) };
                if ui.selectable_label(on, text).on_hover_text(c.notes.get(i).map(String::as_str).unwrap_or("")).clicked() && !on {
                    acts.push(Act::Run("agent.useVariant", json!({"id": c.id, "option": i})));
                }
            }
        });
        if c.current.is_none() {
            ui.label(RichText::new("Edited since: the cells match none of the options").small().italics().color(p.pending));
        }
        if method {
            for (o, name) in set.options.iter().zip(&c.names) {
                let f = o
                    .values
                    .first()
                    .and_then(|r| r.first())
                    .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
                    .unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{name:<6}")).small().monospace().color(t.text_dim));
                    ui.label(RichText::new(clip(&f, 70)).small().monospace());
                });
            }
        }
        for (o, name) in set.options.iter().zip(&c.names) {
            if !o.note.is_empty() {
                ui.label(RichText::new(format!("{name}: {}", clip(&o.note, 160))).small().color(t.text_dim));
            }
        }
        if c.rows.is_empty() {
            return;
        }
        ui.add_space(3.0);
        egui::Grid::new(("variant_grid", &c.id)).num_columns(c.names.len() + 2).spacing(vec2(10.0, 2.0)).striped(true).show(ui, |ui| {
            ui.label("");
            for (i, name) in c.names.iter().enumerate() {
                let r = RichText::new(clip(name, 12)).small().strong();
                ui.label(if c.current == Some(i) { r.color(p.variant) } else { r.color(t.text_dim) });
            }
            ui.label(RichText::new("Δ").small().strong().color(t.text_dim));
            ui.end_row();
            for row in &c.rows {
                let place = Place {
                    sheet: row.key.0,
                    sheet_name: d.wb.sheet(row.key.0).map(|x| x.name.clone()).unwrap_or_default(),
                    range: RangeRef::cell(row.key.1),
                };
                let name = if row.label.is_empty() { place.range.a1() } else { clip(&row.label, 18) };
                if ui.link(RichText::new(name).small()).on_hover_text(place.a1()).clicked() {
                    acts.push(Act::Follow(format!("cell:{}", place.a1())));
                }
                for (i, v) in row.values.iter().enumerate() {
                    let text = v.map(|v| fmt_at(&d.wb, row.key, v)).unwrap_or_else(|| "—".into());
                    let r = RichText::new(text).small();
                    ui.label(if c.current == Some(i) { r.strong() } else { r });
                }
                let diff = RichText::new(pct(row.rel_diff)).small();
                ui.label(if row.material { diff.color(p.material).strong() } else { diff.color(t.text_dim) });
                ui.end_row();
            }
        });
        if let Some(th) = c.threshold {
            let worst = c.rows.iter().filter(|r| r.material).max_by(|a, b| a.rel_diff.partial_cmp(&b.rel_diff).unwrap_or(std::cmp::Ordering::Equal));
            let text = match worst {
                Some(r) => {
                    let what = if r.label.is_empty() { r.key.1.a1() } else { r.label.clone() };
                    RichText::new(format!("Material: {} on {what} (threshold {})", pct(r.rel_diff), pct(th))).color(p.material).small().strong()
                }
                None => RichText::new(format!("Immaterial: at most {} (threshold {})", pct(c.max_rel_diff()), pct(th))).color(t.text_dim).small(),
            };
            ui.label(text);
        }
    });
}

fn variants_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if l.variants.is_empty() {
        ui.label(
            RichText::new("No alternatives yet. Your agent adds them (\"A or B?\", a simple and an exact formula) and you switch here.").italics(),
        );
        return;
    }
    let Some(d) = app.session.active() else { return };
    let list = st.overlay.comparisons(d, l);
    for kind in [VariantKind::Alternatives, VariantKind::Method] {
        let group: Vec<&Comparison> = list.iter().filter(|c| c.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        ui.label(RichText::new(if kind == VariantKind::Method { "Calculation methods" } else { "Alternatives" }).strong());
        for c in group {
            if let Some(set) = l.variants.iter().find(|v| v.id == c.id) {
                variant_card(app, ui, set, c, t, p, acts, true);
            }
        }
    }
}

/// The range an output can take with its inputs moved together, against each input alone, and
/// where each input sits at the two extremes.
fn stress_view(ui: &mut Ui, d: &gridcraft_engine::DocState, r: &Stress, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let target = r.target;
    let f = |v: f64| fmt_at(&d.wb, target, v);
    ui.label(RichText::new(format!("With every input inside its range: {} … {} (now {})", f(r.lowest.value), f(r.highest.value), f(r.base))).small());
    // The bar: each input alone (light) inside all of them together (strong).
    let width = ui.available_width().min(420.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let lo = r.lowest.value.min(r.additive_low).min(r.base);
    let hi = r.highest.value.max(r.additive_high).max(r.base);
    let span = (hi - lo).max(f64::EPSILON);
    let x = |v: f64| rect.left() + 4.0 + ((v - lo) / span) as f32 * (rect.width() - 8.0);
    let down = Color32::from_rgb(0xC8, 0x3E, 0x3E);
    let up = Color32::from_rgb(0x2E, 0x8B, 0x57);
    let y0 = rect.top() + 4.0;
    let band = |a: f64, b: f64, y: f32, h: f32, alpha: f32| {
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x(a.min(r.base)), y), egui::pos2(x(r.base), y + h)), 1.0, down.gamma_multiply(alpha));
        painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x(r.base), y), egui::pos2(x(b.max(r.base)), y + h)), 1.0, up.gamma_multiply(alpha));
    };
    band(r.lowest.value, r.highest.value, y0, 10.0, 0.9);
    band(r.additive_low, r.additive_high, y0 + 13.0, 6.0, 0.35);
    painter.line_segment([egui::pos2(x(r.base), y0 - 2.0), egui::pos2(x(r.base), y0 + 21.0)], egui::Stroke::new(1.2, t.text));
    painter.text(egui::pos2(rect.left() + 4.0, rect.bottom() - 1.0), egui::Align2::LEFT_BOTTOM, "together", theme::ui_font(9.5), t.text_dim);
    painter.text(
        egui::pos2(rect.right() - 4.0, rect.bottom() - 1.0),
        egui::Align2::RIGHT_BOTTOM,
        "light: each input alone",
        theme::ui_font(9.5),
        t.text_dim,
    );
    let alone = (r.additive_high - r.additive_low).abs();
    let together = (r.highest.value - r.lowest.value).abs();
    if alone > 0.0 && (together / alone - 1.0).abs() > 0.05 {
        let k = together / alone - 1.0;
        let text = if k > 0.0 {
            format!("Together the inputs reach {:.0}% further than each alone: they amplify each other.", k * 100.0)
        } else {
            format!("Together the range is {:.0}% narrower than the single moves add up to: they partly offset.", -k * 100.0)
        };
        ui.label(RichText::new(text).small().color(p.material));
    }
    egui::Grid::new("agent_stress_grid").num_columns(3).spacing(vec2(12.0, 2.0)).striped(true).show(ui, |ui| {
        ui.label(RichText::new("Input").small().strong().color(t.text_dim));
        ui.label(RichText::new("Lowest").small().strong().color(down));
        ui.label(RichText::new("Highest").small().strong().color(up));
        ui.end_row();
        for (i, dim) in r.dims.iter().enumerate() {
            let place = Place {
                sheet: dim.key.0,
                sheet_name: d.wb.sheet(dim.key.0).map(|x| x.name.clone()).unwrap_or_default(),
                range: RangeRef::cell(dim.key.1),
            };
            let name = if dim.label.is_empty() { place.range.a1() } else { clip(&dim.label, 22) };
            let link = ui.link(RichText::new(name).small()).on_hover_text(format!(
                "{} · range {} … {}{}",
                place.a1(),
                fmt_at(&d.wb, dim.key, dim.low),
                fmt_at(&d.wb, dim.key, dim.high),
                if dim.reasoned { "" } else { " (default ±10%)" }
            ));
            if link.clicked() {
                acts.push(Act::Follow(format!("cell:{}", place.a1())));
            }
            for e in [&r.lowest, &r.highest] {
                let v = e.setting.get(i).copied().unwrap_or(dim.base);
                let mark = match stress::position(dim, v) {
                    "low" => "▼ ",
                    "high" => "▲ ",
                    "base" => "· ",
                    _ => "◆ ",
                };
                ui.label(RichText::new(format!("{mark}{}", fmt_at(&d.wb, dim.key, v))).small());
            }
            ui.end_row();
        }
    });
    let how = if r.all_corners { format!("all {} corners", r.corners) } else { format!("{} of the corners", r.corners) };
    ui.label(
        RichText::new(format!("{how} and points inside checked · {} evaluations · ▼ low end ▲ high end ◆ inside", r.evaluations))
            .small()
            .color(t.text_dim),
    );
}

fn risk_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let c = d.selection.active;
    if !overlay::is_formula_number(d, si, c) {
        ui.label(RichText::new("Select a result (a formula) to see its inputs on the risk matrix: how much each can move it, against how sure your agent is about it.").italics());
        return;
    }
    let (base, rows) = stress::risk(&d.wb, &d.calc, l, (si, c));
    let label = d.wb.sheet(si).map(|sh| analysis::label_of(sh, c)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("Risk to {}", if label.is_empty() { c.a1() } else { label })).strong());
        if let Some(b) = base {
            ui.label(RichText::new(format!("= {}", fmt_at(&d.wb, (si, c), b))).color(t.text_dim));
        }
    });
    if rows.is_empty() {
        ui.label(RichText::new("No numeric inputs under this cell.").italics());
        return;
    }
    let _ = st;
    // The matrix: confidence → (left: guess, right: verified), impact ↑.
    let w = ui.available_width().min(420.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, w * 0.78), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let plot = egui::Rect::from_min_max(egui::pos2(rect.left() + 4.0, rect.top() + 16.0), egui::pos2(rect.right() - 4.0, rect.bottom() - 30.0));
    let mid = plot.center();
    let red = Color32::from_rgb(0xD0, 0x38, 0x38);
    let amber = p.material;
    let green = Color32::from_rgb(0x2E, 0x8B, 0x57);
    for (q, color) in [
        (egui::Rect::from_min_max(plot.left_top(), mid), red),
        (egui::Rect::from_min_max(egui::pos2(mid.x, plot.top()), egui::pos2(plot.right(), mid.y)), amber),
        (egui::Rect::from_min_max(egui::pos2(plot.left(), mid.y), egui::pos2(mid.x, plot.bottom())), amber),
        (egui::Rect::from_min_max(mid, plot.right_bottom()), green),
    ] {
        painter.rect_filled(q, 0.0, color.gamma_multiply(0.10));
    }
    // Quadrant names outside the plot, so they never sit under a dot.
    let font = theme::ui_bold(10.0);
    painter.text(egui::pos2(plot.left(), plot.top() - 2.0), egui::Align2::LEFT_BOTTOM, "▲ high impact, unsure: check first", font.clone(), red);
    painter.text(egui::pos2(plot.right(), plot.top() - 2.0), egui::Align2::RIGHT_BOTTOM, "watch", font.clone(), amber);
    painter.text(egui::pos2(plot.left(), plot.bottom() + 2.0), egui::Align2::LEFT_TOP, "refine later", font.clone(), amber);
    painter.text(egui::pos2(plot.right(), plot.bottom() + 2.0), egui::Align2::RIGHT_TOP, "fine", font, green);
    painter.rect_stroke(plot, 0.0, egui::Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
    painter.text(
        egui::pos2(plot.center().x, rect.bottom() - 1.0),
        egui::Align2::CENTER_BOTTOM,
        "guess ← agent's confidence → verified",
        theme::ui_font(10.0),
        t.text_dim,
    );
    let pos = |r: &stress::RiskRow| {
        let x = r.confidence.unwrap_or(0.0) as f32;
        let y = r.impact as f32;
        egui::pos2(plot.left() + 10.0 + x * (plot.width() - 20.0), plot.bottom() - 10.0 - y * (plot.height() - 20.0))
    };
    for r in &rows {
        let at = pos(r);
        let color = match r.quadrant() {
            "check first" => red,
            "fine" => green,
            _ => amber,
        };
        if r.confidence.is_some() {
            painter.circle_filled(at, 5.0, color);
        } else {
            painter.circle_stroke(at, 5.0, egui::Stroke::new(1.6, color));
        }
        let name = if r.label.is_empty() { r.key.1.a1() } else { clip(&r.label, 18) };
        let right = at.x < plot.right() - 90.0;
        let (anchor, off) = if right { (egui::Align2::LEFT_CENTER, vec2(8.0, 0.0)) } else { (egui::Align2::RIGHT_CENTER, vec2(-8.0, 0.0)) };
        painter.text(at + off, anchor, name, theme::ui_font(10.0), t.text);
    }
    ui.label(
        RichText::new("Impact: how far the input moves the result over its range (largest = top). Hollow: no confidence given yet.")
            .small()
            .color(t.text_dim),
    );
    ui.add_space(4.0);
    egui::Grid::new("agent_risk_grid").num_columns(4).spacing(vec2(10.0, 2.0)).striped(true).show(ui, |ui| {
        for h in ["Input", "Impact", "Confidence", ""] {
            ui.label(RichText::new(h).small().strong().color(t.text_dim));
        }
        ui.end_row();
        for r in &rows {
            let place =
                Place { sheet: r.key.0, sheet_name: d.wb.sheet(r.key.0).map(|x| x.name.clone()).unwrap_or_default(), range: RangeRef::cell(r.key.1) };
            let name = if r.label.is_empty() { place.range.a1() } else { clip(&r.label, 22) };
            if ui.link(RichText::new(name).small()).on_hover_text(place.a1()).clicked() {
                acts.push(Act::Follow(format!("cell:{}", place.a1())));
            }
            ui.label(RichText::new(format!("{:.0}%", r.impact * 100.0)).small());
            let conf = r.confidence.map_or("—".to_string(), |c| format!("{:.0}%", c * 100.0));
            ui.label(RichText::new(conf).small()).on_hover_text(&r.reason);
            let q = r.quadrant();
            let color = match q {
                "check first" => red,
                "fine" => green,
                _ => amber,
            };
            ui.label(RichText::new(q).small().color(color));
            ui.end_row();
        }
    });
}

/// Safe file mode: what a save writes, the export for standard Excel, unlocking.
fn safe_file(ui: &mut Ui, d: &gridcraft_engine::DocState, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let private = analysis::private_cells(&d.wb, l).len();
    ui.add_space(4.0);
    match &l.safe {
        Some(s) if s.sealed.is_some() => {
            card(ui, t, p.material, |ui| {
                ui.label(RichText::new("Client data locked").color(p.material).small().strong());
                ui.label(RichText::new(format!("This is a safe file: its {} client cells show stand-ins. Their real values are sealed under a key that isn't on this computer.", s.sealed.as_ref().map_or(0, |x| x.cells))).small());
                if ui.small_button("Try to unlock").clicked() {
                    acts.push(Act::Run("agent.unlock", json!({})));
                }
            });
        }
        Some(s) => {
            card(ui, t, p.private, |ui| {
                ui.label(RichText::new("Safe file · on").color(p.private).small().strong());
                ui.label(RichText::new(format!("Saving writes the {private} client cells as stand-ins (everything computed from them too). The real values are sealed in the file; the key stays on this computer ({}).", s.key_id)).small());
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .small_button("Export for standard Excel")
                        .on_hover_text("A copy beside the workbook with the real values and nothing sealed")
                        .clicked()
                    {
                        acts.push(Act::Run("agent.exportExcel", json!({})));
                    }
                    if ui.small_button("Turn off").clicked() {
                        acts.push(Act::Run("agent.safeMode", json!({"on": false})));
                    }
                });
            });
        }
        None if private > 0 => {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Safe file: save client data as stand-ins")
                    .on_hover_text("The real values are sealed in the file under a key kept on this computer")
                    .clicked()
                {
                    acts.push(Act::Run("agent.safeMode", json!({"on": true})));
                }
            });
        }
        None => {}
    }
}

/// How the app brings the agent in: off, into the user's own session, or as a background agent.
#[cfg(not(target_arch = "wasm32"))]
fn agent_mode(app: &SheetApp, ui: &mut Ui, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    use gridcraft_agent::runner::{self, Mode};
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);
    let mode = runner::mode();
    ui.label(RichText::new("When you ask a question").strong());
    ui.horizontal_wrapped(|ui| {
        for (m, name, tip) in [
            (Mode::Off, "I connect my agent", "Your agent watches /events or calls wait_for_questions"),
            (Mode::Channel, "Send to my session", "Questions arrive in your open Claude Code session (Channels, research preview)"),
            (Mode::Background, "Background agent", "GridCraft runs Claude Code for this workbook and answers in the background"),
        ] {
            if ui.selectable_label(mode == m, name).on_hover_text(tip).clicked() && mode != m {
                acts.push(Act::Run("agent.agentMode", json!({"mode": m.id()})));
            }
        }
    });
    let copy_row = |ui: &mut Ui, title: &str, value: String| {
        ui.label(RichText::new(title).small().strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut value.clone()).desired_width(ui.available_width() - 60.0).font(egui::TextStyle::Monospace));
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(value.clone());
            }
        });
    };
    match mode {
        Mode::Off => {}
        Mode::Channel => {
            let exe = std::env::current_exe().map(|x| x.display().to_string()).unwrap_or_else(|_| "gridcraft".into());
            ui.label(RichText::new("Each question arrives as a new turn in your interactive Claude Code session. Once:").small());
            copy_row(ui, "Add GridCraft as a channel server", format!("claude mcp add gridcraft-channel -- '{exe}' --agent-stdio"));
            copy_row(ui, "Then start your session with", "claude --dangerously-load-development-channels server:gridcraft-channel".into());
            ui.label(RichText::new("Channels are a research preview: interactive sessions only, and Claude Code asks you to confirm the development channel at start.").small().color(t.text_dim));
        }
        Mode::Background => {
            let st = runner::status(d.uid).unwrap_or_default();
            card(ui, t, if st.running { p.answered } else { p.seen }, |ui| {
                let state = if st.running {
                    if st.in_flight > 0 { format!("Working · {} in progress", st.in_flight) } else { "Running · waiting for questions".to_string() }
                } else {
                    "Not running · starts with your next question".to_string()
                };
                ui.label(RichText::new(state).small().strong());
                let session = if st.session_id.is_empty() { l.agent_session.clone() } else { st.session_id.clone() };
                if !session.is_empty() {
                    ui.label(RichText::new(format!("Session {session}")).small().monospace().color(t.text_dim));
                }
                if !st.cwd.is_empty() {
                    ui.label(RichText::new(format!("In {} · also listed in Claude Desktop's Code tab", st.cwd)).small().color(t.text_dim));
                }
                if st.turns > 0 {
                    ui.label(RichText::new(format!("{} turns · ${:.2}", st.turns, st.cost_usd)).small().color(t.text_dim));
                }
                for line in st.recent.iter().rev().take(4).rev() {
                    ui.label(RichText::new(clip(line, 120)).small());
                }
                if !st.last_error.is_empty() {
                    ui.label(RichText::new(clip(&st.last_error, 240)).small().color(p.material));
                }
                ui.horizontal_wrapped(|ui| {
                    if st.running {
                        if ui.small_button("Stop").clicked() {
                            acts.push(Act::Run("agent.agentStop", json!({})));
                        }
                    } else if ui.small_button("Start").clicked() {
                        acts.push(Act::Run("agent.agentStart", json!({})));
                    }
                    if ui.small_button("New session").on_hover_text("Start over from a summary of this workbook's history").clicked() {
                        acts.push(Act::Run("agent.agentReset", json!({})));
                    }
                });
                if !session.is_empty() {
                    let cmd = runner::resume_command(&runner::Status { session_id: session.clone(), ..st.clone() });
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Copy resume command")
                            .on_hover_text("Continue this conversation yourself: stop the background agent first (one writer per session)")
                            .clicked()
                        {
                            ui.ctx().copy_text(cmd.clone());
                        }
                    });
                }
            });
            ui.label(
                RichText::new(format!(
                    "Runs `{}` with only GridCraft's tools, at most {} turns and ${:.2} per question.",
                    runner::config().command,
                    runner::config().max_turns,
                    runner::config().max_budget_usd
                ))
                .small()
                .color(t.text_dim),
            );
        }
    }
    ui.add_space(6.0);
    ui.separator();
}

fn history_tab(app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if l.log.is_empty() {
        ui.label(RichText::new("No history yet: decisions and changes by you and the agent are recorded here.").italics());
        return;
    }
    for e in l.log.iter().rev().take(300) {
        let (who, color) = match e.actor {
            Actor::Agent => ("Agent", p.agent),
            Actor::Human => ("You", p.person),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(who).small().strong().color(color));
            ui.label(RichText::new(if e.kind == "decision" { "decided" } else { "changed" }).small().color(t.text_dim));
            if let Some(a) = &e.anchor
                && let Some(pl) = place_of(app, l, a)
                && ui.link(RichText::new(pl.a1()).small()).clicked()
            {
                acts.push(Act::Follow(format!("cell:{}", pl.a1())));
            }
        });
        ui.label(&e.text);
        if !e.source.is_empty() {
            if e.source.starts_with("http") {
                if ui.link(RichText::new(format!("based on {}", e.source)).small()).clicked() {
                    acts.push(Act::Follow(e.source.clone()));
                }
            } else {
                ui.label(RichText::new(format!("based on {}", e.source)).small().color(t.text_dim));
            }
        }
        ui.add_space(4.0);
    }
}

fn connect_tab(st: &AgentUi, app: &SheetApp, ui: &mut Ui, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    if gridcraft_agent::read_only() {
        ui.label(RichText::new("Read-only copy").strong());
        ui.label("This page was exported from GridCraft with the answers and notes the author's agent prepared. No agent is connected here, so there are no live questions.");
        ui.add_space(6.0);
        ui.label(
            RichText::new("To ask your own questions, open the workbook in GridCraft (File > Save As keeps the agent layer in the .xlsx) and connect your agent there.")
                .small()
                .color(t.text_dim),
        );
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    agent_mode(app, ui, t, p, acts);
    #[cfg(target_arch = "wasm32")]
    let _ = p;
    let Some(url) = crate::server_url() else {
        ui.label("The agent server isn't running. Start GridCraft with --agent-port <port>.");
        return;
    };
    let base = url.trim_end_matches("/mcp");
    ui.label("Your own agent works on this workbook through MCP. Questions you ask wait in the workbook until it answers.");
    ui.add_space(6.0);
    let row = |ui: &mut Ui, title: &str, value: String| {
        ui.label(RichText::new(title).small().strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut value.clone()).desired_width(ui.available_width() - 60.0).font(egui::TextStyle::Monospace));
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(value.clone());
            }
        });
    };
    row(ui, "MCP server", url.to_string());
    row(ui, "Claude Code", format!("claude mcp add --transport http gridcraft {url}"));
    row(ui, "Background monitor (new questions, one line each)", format!("curl -sN {base}/events"));
    ui.add_space(6.0);
    ui.label(
        RichText::new("Client data you mark stays here: the agent sees stand-in values, with the model recalculated from them.")
            .small()
            .color(t.text_dim),
    );
    share(st, app, ui, t, acts);
}

/// Share as a web page: one HTML file beside the workbook, opened in any browser with the
/// prepared answers and notes, read-only.
fn share(st: &AgentUi, app: &SheetApp, ui: &mut Ui, t: &Tokens, acts: &mut Vec<Act>) {
    if cfg!(target_arch = "wasm32") {
        return;
    }
    ui.add_space(10.0);
    ui.label(RichText::new("Share as a web page").strong());
    ui.label(
        RichText::new(
            "One HTML file: the workbook in GridCraft for the browser, with the answers and notes your agent prepared. Open questions stay here.",
        )
        .small()
        .color(t.text_dim),
    );
    let path = app.session.active().and_then(|d| d.path.as_deref()).map(|p| std::path::Path::new(p).with_extension("html").display().to_string());
    match path {
        Some(path) => {
            if ui.button("Export web page").on_hover_text(&path).clicked() {
                acts.push(Act::Share(path));
            }
        }
        None => {
            ui.add_enabled(false, egui::Button::new("Export web page"))
                .on_disabled_hover_text("Save the workbook first: the page is written beside it");
        }
    }
    if let Some(msg) = &st.pane.shared {
        ui.label(RichText::new(msg).small().color(t.text_dim));
    }
}
