//! The Agent pane: Selection (the selected cell, the topics touching it, and folded below them
//! where the number comes from, sensitivity, alternatives and risk; the question box at the
//! bottom), Overview (every topic, sign-off) and History.

use std::collections::HashMap;

use egui::{Color32, RichText, Ui, vec2};
use gridcraft_agent::analysis;
use gridcraft_agent::anchor::{self, Place};
use gridcraft_agent::layer::{self, Actor, Layer, Link, Question, QuestionStatus, Status, Topic, VariantKind, VariantSet};
use gridcraft_agent::stress::{self, Stress};
use gridcraft_agent::textlinks::{self, Seg};
use gridcraft_agent::topics::{self, Numbering};
use gridcraft_agent::variants::Comparison;
use gridcraft_engine::core::{CellRef, RangeRef};
use gridcraft_engine::model::Workbook;
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::theme::{self, Tokens};
use serde_json::{Value as Json, json};

use crate::overlay::{self, clip, kind_label};
use crate::{AgentUi, Filter, Palette, Tab, fmt_at, fmt_num, follow, palette};

#[derive(Default)]
pub(crate) struct State {
    pub trace_open: bool,
    /// Open the analysis once (a request for alternatives or risk).
    pub analysis_open: bool,
    /// Scroll the focused topic into view once.
    pub scroll_focus: bool,
    trace: Option<((u64, usize, u64), usize, CellRef, Json)>,
    /// A status note being typed, per topic.
    status_text: HashMap<String, String>,
    /// The option shown per topic, and a reject comment being typed.
    option: HashMap<String, String>,
    reject_text: HashMap<String, String>,
}

/// Something the user clicked, acted on once drawing is done (drawing borrows the app).
enum Act {
    Follow(String),
    Run(&'static str, Json),
    Topic(String),
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
            let unread = l.topics.iter().filter(|x| !x.hidden && topics::unread(&l, &x.id)).count();
            ui.horizontal_wrapped(|ui| {
                for (tab, name) in [
                    (Tab::Selection, "Selection".to_string()),
                    (Tab::Overview, if unread > 0 { format!("Overview ({unread})") } else { "Overview".into() }),
                    (Tab::History, "History".into()),
                ] {
                    if ui.selectable_label(st.tab == tab, name).clicked() {
                        st.tab = tab;
                    }
                }
            });
            ui.separator();
            if st.tab == Tab::Selection {
                // The question box stays at the very bottom.
                egui::Panel::bottom("agent_ask")
                    .frame(egui::Frame::NONE.fill(t.ribbon).inner_margin(egui::Margin { top: 6, ..Default::default() }))
                    .show(ui, |ui| {
                        if gridcraft_agent::read_only() {
                            ask_closed(ui, &t);
                        } else {
                            ask_box(st, app, ui, &l, &t, &p, &mut acts);
                        }
                    });
            }
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match st.tab {
                Tab::Selection => selection_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::Overview => overview_tab(st, app, ui, &l, &t, &p, &mut acts),
                Tab::History => history_tab(app, ui, &l, &t, &p, &mut acts),
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
            Act::Topic(id) => {
                crate::open_topic(st, app, &id, None);
                st.pane.scroll_focus = true;
            }
        }
    }
}

fn place_of(app: &SheetApp, l: &Layer, anchor_id: &str) -> Option<Place> {
    let d = app.session.active()?;
    l.anchor(anchor_id).and_then(|a| anchor::resolve(&d.wb, a))
}

/// A text with its links inside it (§2): words wrap, links select cells, open topics or the web.
/// `tips`: the summaries of web links, shown on hover.
fn rich(ui: &mut Ui, segs: &[Seg], acts: &mut Vec<Act>, tips: &[Link], style: impl Fn(RichText) -> RichText) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in segs {
            match s {
                Seg::Text(text) => {
                    for (i, line) in text.split('\n').enumerate() {
                        if i > 0 {
                            ui.end_row();
                        }
                        for w in line.split_inclusive(' ') {
                            ui.label(style(RichText::new(w)));
                        }
                    }
                }
                Seg::Link { label, href, .. } => {
                    let tip = tips
                        .iter()
                        .find(|k| &k.href == href && !k.summary.is_empty())
                        .map_or_else(|| href.strip_prefix("cell:").unwrap_or(href).to_string(), |k| k.summary.clone());
                    let color = ui.visuals().hyperlink_color;
                    if ui.link(style(RichText::new(label)).color(color)).on_hover_text(tip).clicked() {
                        acts.push(Act::Follow(href.clone()));
                    }
                }
            }
        }
    });
}

fn text(ui: &mut Ui, wb: &Workbook, l: &Layer, s: &str, acts: &mut Vec<Act>, tips: &[Link], style: impl Fn(RichText) -> RichText) {
    let segs = textlinks::segments(wb, l, s, wb.active_sheet);
    rich(ui, &segs, acts, tips, style);
}

fn links(ui: &mut Ui, list: &[Link], acts: &mut Vec<Act>) {
    if list.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for k in list {
            let mut r = ui.link(format!("» {}", k.label));
            r = if k.summary.is_empty() { r.on_hover_text(&k.href) } else { r.on_hover_text(&k.summary) };
            if r.clicked() {
                acts.push(Act::Follow(k.href.clone()));
            }
        }
    });
}

fn card(ui: &mut Ui, t: &Tokens, stripe: Color32, add: impl FnOnce(&mut Ui)) -> egui::Rect {
    let r = egui::Frame::NONE.fill(t.window).corner_radius(6.0).inner_margin(8.0).stroke(egui::Stroke::new(1.0, t.separator)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let top = ui.cursor().min;
        add(ui);
        let bottom = ui.min_rect().bottom();
        ui.painter().line_segment([egui::pos2(top.x - 6.0, top.y), egui::pos2(top.x - 6.0, bottom)], egui::Stroke::new(2.5, stripe));
    });
    ui.add_space(6.0);
    r.response.rect
}

/// A marker as in the sheet, inline.
fn pill(ui: &mut Ui, label: &str, fill: Color32, unread: bool, t: &Tokens) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(label.to_string(), theme::ui_bold(10.0), Color32::WHITE);
    let w = (galley.size().x + 8.0).max(16.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w + 2.0, 16.0), egui::Sense::click());
    let r = egui::Rect::from_min_size(rect.min + vec2(2.0, 0.0), vec2(w, 16.0));
    ui.painter().rect_filled(r, 8.0, fill);
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
    if unread {
        ui.painter().circle_filled(r.left_top() + vec2(1.0, 2.0), 3.4, t.window);
        ui.painter().circle_filled(r.left_top() + vec2(1.0, 2.0), 2.5, crate::BLUE);
    }
    resp
}

fn status_word(ui: &mut Ui, s: Status, p: &Palette) {
    ui.label(RichText::new(s.label()).color(p.status(s)).small().strong());
}

/// A topic: number and status (with its text), places, notes, then the discussion, folded once a
/// status is set.
#[allow(clippy::too_many_arguments)]
fn topic_card(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, n: &Numbering, tp: &Topic, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let wb = &d.wb;
    let editable = !gridcraft_agent::read_only();
    let num = n.topic.get(&tp.id).copied().unwrap_or(0);
    let status = tp.status.status;
    let working = topics::working(l, &tp.id);
    let unread = topics::unread(l, &tp.id);
    let focused = st.focus_topic.as_deref() == Some(tp.id.as_str());
    let rect = card(ui, t, p.status(status), |ui| {
        ui.horizontal_wrapped(|ui| {
            pill(ui, &num.to_string(), p.status(status), unread, t);
            ui.label(RichText::new(format!("Topic {num}")).strong());
            status_word(ui, status, p);
            if tp.status.by == Actor::Agent && tp.status.at > 0 {
                ui.label(RichText::new("· set by the agent").small().color(t.text_dim));
            }
            if working {
                ui.label(RichText::new("· the agent is working on it").small().color(p.working));
            }
        });
        if !tp.status.text.is_empty() {
            text(ui, wb, l, &tp.status.text, acts, &[], |r| r);
        }
        // The person sets the status; Agreed is theirs alone.
        if editable {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let draft = st.pane.status_text.entry(tp.id.clone()).or_default();
                for s in [Status::Open, Status::Revise, Status::Agreed, Status::Info] {
                    let on = s == status;
                    let label = RichText::new(if s == Status::Info { "Info" } else { s.label() }).small();
                    let label = if on { label.color(p.status(s)).strong() } else { label };
                    let tip = match s {
                        Status::Open => "A question or doubt, no decision yet",
                        Status::Revise => "Decided: something here must change",
                        Status::Agreed => "Settled and accepted",
                        Status::Info => "Nothing to decide: an explanation, a source, a caveat",
                    };
                    if ui.selectable_label(on, label).on_hover_text(tip).clicked() && (!on || !draft.trim().is_empty()) {
                        let mut params = json!({"topic": tp.id, "status": s.id()});
                        if !draft.trim().is_empty() {
                            params["text"] = json!(draft.trim());
                        }
                        acts.push(Act::Run("agent.setStatus", params));
                        draft.clear();
                    }
                }
            });
            let draft = st.pane.status_text.entry(tp.id.clone()).or_default();
            let r = ui.add(
                egui::TextEdit::singleline(draft)
                    .hint_text("Why (optional), then pick a status")
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Small),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !draft.trim().is_empty() {
                acts.push(Act::Run("agent.setStatus", json!({"topic": tp.id, "status": status.id(), "text": draft.trim()})));
                draft.clear();
            }
        }
        // The agent's proposal, confirmed with one click.
        if let Some(pr) = &tp.proposed {
            egui::Frame::NONE.fill(p.agreed.gamma_multiply(0.10)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Agent suggests:").small().color(t.text_dim));
                    ui.label(RichText::new("Agreed").small().strong().color(p.agreed));
                    if editable && ui.small_button("Accept").on_hover_text("Mark the topic Agreed, with the agent's reasons as its note").clicked() {
                        acts.push(Act::Run("agent.acceptProposal", json!({"topic": tp.id})));
                    }
                });
                text(ui, wb, l, &pr.reasons, acts, &[], |r| r.small());
            });
        }
        // The agent's proposed edits: options, what they change, Apply or Reject.
        if let Some(e) = &tp.edits {
            proposal_block(st, ui, d, l, tp, e, t, p, acts, editable, focused);
        }
        // Places, in document order.
        ui.add_space(3.0);
        let mut places: Vec<(&String, &layer::TopicPlace)> =
            tp.places.iter().filter_map(|x| n.place.get(&(tp.id.clone(), x.anchor.clone())).map(|k| (k, x))).collect();
        places.sort_by_key(|a| natural(a.0));
        let many = places.len() > 1;
        for (label, place) in &places {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let live = place_of(app, l, &place.anchor);
                match &live {
                    Some(pl) => {
                        ui.label(RichText::new(label.as_str()).small().strong().color(p.status(status)));
                        if ui.link(RichText::new(textlinks::address(pl, wb.active_sheet)).small()).clicked() {
                            acts.push(Act::Follow(format!("cell:{}", pl.a1())));
                        }
                    }
                    None => {
                        ui.label(RichText::new(label.as_str()).small().strikethrough().color(t.text_dim));
                        let segs = textlinks::removed(wb, l, &place.anchor, wb.active_sheet);
                        rich(ui, &segs, acts, &[], |r| r.small().italics().color(t.text_dim));
                    }
                }
                if editable
                    && many
                    && place.by == Actor::Agent
                    && ui.small_button("Reject").on_hover_text("This place doesn't belong to the topic").clicked()
                {
                    acts.push(Act::Run("agent.rejectPlace", json!({"topic": tp.id, "place": place.anchor})));
                }
            });
            if !place.reason.is_empty() {
                ui.indent(("reason", &tp.id, &place.anchor), |ui| {
                    text(ui, wb, l, &place.reason, acts, &[], |r| r.small().color(t.text_dim));
                });
            }
        }
        // Notes: titles that expand.
        let notes = topics::notes(l, &tp.id);
        if !notes.is_empty() {
            ui.add_space(3.0);
        }
        for note in notes {
            let title =
                if note.title.is_empty() { kind_label(&note.kind).to_string() } else { format!("{} · {}", kind_label(&note.kind), note.title) };
            let open = st.highlight_note.as_deref() == Some(note.id.as_str());
            let h = egui::CollapsingHeader::new(RichText::new(title).small().color(p.note).strong()).id_salt(("note", &note.id));
            let h = if open { h.open(Some(true)) } else { h };
            let r = h.show(ui, |ui| {
                text(ui, wb, l, &note.text, acts, &note.links, |r| r);
                let web: Vec<Link> = note.links.iter().filter(|k| !note.text.contains(k.href.as_str())).cloned().collect();
                links(ui, &web, acts);
                if editable && ui.small_button("Remove").on_hover_text("Remove this note (undo brings it back)").clicked() {
                    acts.push(Act::Run("agent.removeNote", json!({"id": note.id})));
                }
            });
            if open {
                r.header_response.scroll_to_me(Some(egui::Align::Center));
                st.highlight_note = None;
            }
        }
        // The discussion: open while the topic is, folded once a status is set.
        let disc = topics::discussion(l, &tp.id);
        if !disc.is_empty() {
            ui.add_space(3.0);
            let default = status == Status::Open || working || unread;
            let h = egui::CollapsingHeader::new(RichText::new(format!("Discussion ({})", disc.len())).small().strong())
                .id_salt(("discussion", &tp.id, status == Status::Open))
                .default_open(default);
            h.show(ui, |ui| {
                for q in disc {
                    question(ui, wb, l, q, t, p, acts);
                    // Reading it here is reading it.
                    if q.status == QuestionStatus::Answered && !q.seen {
                        acts.push(Act::Run("agent.seen", json!({"id": q.id})));
                    }
                }
            });
        }
    });
    if focused && std::mem::take(&mut st.pane.scroll_focus) {
        ui.scroll_to_rect(rect, Some(egui::Align::TOP));
    }
}

/// A topic's proposal: "The agent proposes N changes", its options, what each changes (old → new),
/// then Apply option X / Reject; out of date, applied or rejected once settled.
#[allow(clippy::too_many_arguments)]
fn proposal_block(
    st: &mut AgentUi,
    ui: &mut Ui,
    d: &gridcraft_engine::DocState,
    l: &Layer,
    tp: &Topic,
    e: &layer::EditProposal,
    t: &Tokens,
    p: &Palette,
    acts: &mut Vec<Act>,
    editable: bool,
    focused: bool,
) {
    use gridcraft_agent::layer::ProposalState;
    let wb = &d.wb;
    let blue = crate::BLUE;
    egui::Frame::NONE.fill(blue.gamma_multiply(0.08)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        match e.state {
            ProposalState::Applied => {
                let title = e.option(&e.chosen).map(|o| o.title.clone()).unwrap_or_default();
                ui.label(
                    RichText::new(format!("Applied option {}{}", e.chosen, if title.is_empty() { String::new() } else { format!(": {title}") }))
                        .small()
                        .color(t.text_dim),
                );
                return;
            }
            ProposalState::Rejected => {
                ui.label(RichText::new("You rejected the agent's proposal").small().color(t.text_dim));
                if !e.comment.is_empty() {
                    ui.label(RichText::new(format!("“{}”", e.comment)).small().italics().color(t.text_dim));
                }
                return;
            }
            ProposalState::Outdated => {
                ui.label(RichText::new("The agent's proposal is out of date, ask the agent to redo").small().strong().color(p.open));
                return;
            }
            ProposalState::Pending => {}
        }
        let shown = st.pane.option.get(&tp.id).filter(|o| e.option(o).is_some()).cloned().or_else(|| e.options.first().map(|o| o.option.clone()));
        let Some(shown) = shown else { return };
        let Some(o) = e.option(&shown) else { return };
        // The sheet previews it: the card in focus, else the first one shown.
        if st.preview.is_none() || focused {
            st.preview = Some((tp.id.clone(), shown.clone()));
        }
        let n = o.steps.len();
        ui.label(RichText::new(format!("The agent proposes {n} change{}", if n == 1 { "" } else { "s" })).small().strong().color(blue));
        if e.options.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for x in &e.options {
                    let label = if x.title.is_empty() { x.option.clone() } else { format!("{} · {}", x.option, clip(&x.title, 28)) };
                    if ui.selectable_label(x.option == shown, RichText::new(label).small()).on_hover_text("Show this option on the sheet").clicked() {
                        st.pane.option.insert(tp.id.clone(), x.option.clone());
                    }
                }
            });
        } else if !o.title.is_empty() {
            ui.label(RichText::new(&o.title).small().strong());
        }
        if !o.why.is_empty() {
            text(ui, wb, l, &o.why, acts, &[], |r| r.small());
        }
        // What it changes, old → new.
        match gridcraft_agent::proposals::draft(d, tp, &shown).as_deref() {
            Some(Ok(dr)) => {
                let edited: Vec<_> = dr.changes.iter().filter(|c| c.edited).collect();
                let results = dr.changes.len() - edited.len();
                for c in edited.iter().take(6) {
                    let place = Place::of(wb, c.sheet, c.cell);
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if ui.link(RichText::new(textlinks::address(&place, wb.active_sheet)).small()).clicked() {
                            acts.push(Act::Follow(format!("cell:{}", place.a1())));
                        }
                        ui.label(
                            RichText::new(if c.old.is_empty() { "empty".to_string() } else { clip(&c.old, 24) })
                                .small()
                                .strikethrough()
                                .color(t.text_dim),
                        );
                        ui.label(RichText::new("→").small().color(t.text_dim));
                        ui.label(RichText::new(if c.new.is_empty() { "empty".to_string() } else { clip(&c.new, 24) }).small().strong().color(blue));
                    });
                }
                if edited.len() > 6 {
                    ui.label(RichText::new(format!("and {} more", edited.len() - 6)).small().color(t.text_dim));
                }
                if results > 0 {
                    ui.label(
                        RichText::new(if results == 1 {
                            "1 result recalculates (shown on the sheet)".to_string()
                        } else {
                            format!("{results} results recalculate (shown on the sheet)")
                        })
                        .small()
                        .color(t.text_dim),
                    );
                }
            }
            Some(Err(_)) => {
                ui.label(RichText::new("Out of date, ask the agent to redo").small().strong().color(p.open));
                return;
            }
            None => {}
        }
        if !editable {
            return;
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let apply = egui::Button::new(RichText::new(format!("Apply option {shown}")).color(Color32::WHITE).strong()).fill(blue);
            if ui.add(apply).on_hover_text("Make these changes in the workbook: one step you can undo").clicked() {
                acts.push(Act::Run("agent.applyProposal", json!({"topic": tp.id, "option": shown})));
            }
            let comment = st.pane.reject_text.entry(tp.id.clone()).or_default();
            if ui.button("Reject").on_hover_text("Turn the proposal down; your comment (optional) goes to the agent").clicked() {
                acts.push(Act::Run("agent.rejectProposal", json!({"topic": tp.id, "comment": comment.trim()})));
                comment.clear();
            }
        });
        let comment = st.pane.reject_text.entry(tp.id.clone()).or_default();
        ui.add(
            egui::TextEdit::singleline(comment)
                .hint_text("Comment for a reject (optional)")
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Small),
        );
    });
}

/// `3.10` after `3.9`.
pub(crate) fn natural(label: &str) -> Vec<u32> {
    label.split('.').map(|x| x.parse().unwrap_or(0)).collect()
}

fn question(ui: &mut Ui, wb: &Workbook, l: &Layer, q: &Question, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let (state, color) = match q.status {
        QuestionStatus::Pending => ("Waiting for the agent", p.pending),
        QuestionStatus::Working => ("The agent is working on it", p.working),
        QuestionStatus::Answered => ("", p.seen),
        QuestionStatus::Transcribing => ("Transcribing what you said…", p.transcribing),
        QuestionStatus::Failed => ("Not transcribed", p.failed),
        QuestionStatus::Withdrawn => ("Withdrawn", t.text_dim),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(if q.spoken { "You asked · spoken" } else { "You asked" }).small().color(p.person));
        if !state.is_empty() {
            ui.label(RichText::new(state).small().color(color));
        }
    });
    if q.status == QuestionStatus::Failed {
        ui.label(RichText::new(&q.error).small().color(p.failed));
        ui.horizontal(|ui| {
            if ui.small_button("Retry").on_hover_text("Transcribe the recording again").clicked() {
                acts.push(Act::Follow(format!("voice-retry:{}", q.id)));
            }
            if ui.small_button("Dismiss").on_hover_text("Drop this question and its recording").clicked() {
                acts.push(Act::Follow(format!("voice-dismiss:{}", q.id)));
            }
        });
    }
    if !q.text.is_empty() {
        text(ui, wb, l, &q.text, acts, &[], |r| r.italics());
    }
    if let Some(a) = &q.answer {
        ui.add_space(2.0);
        ui.label(RichText::new("Agent").small().color(p.agent));
        text(ui, wb, l, &a.text, acts, &a.links, |r| r);
    }
    ui.add_space(5.0);
}

fn selection_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let Some(sh) = d.wb.sheet(si) else { return };
    let sel = d.selection.current();
    let c = d.selection.active;
    let here = Place { sheet: si, sheet_name: sh.name.clone(), range: sel };
    let label = analysis::label_of(sh, c);
    let shown = gridcraft_engine::display::cell_text(&d.wb, sh, c);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(here.range.a1()).strong());
        if !label.is_empty() {
            ui.label(RichText::new(&label).color(t.text_dim));
        }
        if !shown.is_empty() {
            ui.label(RichText::new(format!("= {shown}")).color(t.text));
        }
    });
    if let Some(f) = sh.cell(c).and_then(|x| x.formula.as_ref()) {
        ui.label(RichText::new(format!("={}", f.text.trim_start_matches('='))).monospace().small().color(t.text_dim));
    }
    let private_cells = analysis::private_cells(&d.wb, l);
    if private_cells.contains(&(si, c)) {
        ui.label(RichText::new("Client data · your agent sees stand-in values here").color(p.private).small());
    }
    ui.add_space(6.0);

    // The topics touching the selection; one opened from a marker or a link first.
    let n = topics::numbering(&d.wb, l);
    let mut ids = topics::topics_at(&d.wb, l, &here);
    if let Some(f) = st.focus_topic.clone() {
        let gone = l.topic(&f).is_some_and(|x| x.places.iter().all(|p| place_of(app, l, &p.anchor).is_none()));
        if ids.contains(&f) || gone {
            ids.retain(|x| *x != f);
            ids.insert(0, f);
        }
    }
    if ids.is_empty() {
        ui.label(
            RichText::new(if gridcraft_agent::read_only() {
                "Nothing prepared about these cells."
            } else {
                "No topics here yet. Ask about these cells below: the question starts a topic, numbered on the sheet."
            })
            .small()
            .italics()
            .color(t.text_dim),
        );
        ui.add_space(6.0);
    }
    for id in &ids {
        if let Some(tp) = l.topic(id) {
            topic_card(st, app, ui, l, &n, tp, t, p, acts);
        }
    }

    analysis_section(st, app, ui, l, t, p, acts);
}

/// Folded below the topics: what the agent knows about the number itself.
fn analysis_section(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let Some(sh) = d.wb.sheet(si) else { return };
    let sel = d.selection.current();
    let c = d.selection.active;
    let hits = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a)).is_some_and(|pl| pl.sheet == si && pl.range.intersects(&sel));
    let is_formula = sh.cell(c).is_some_and(|x| x.formula.is_some());
    let input = l.inputs.iter().find(|i| hits(&i.anchor));
    let conf = l.confidence.iter().find(|x| hits(&x.anchor));
    let slides = gridcraft_agent::links::uses_at(&d.wb, l, si, Some(&sel));
    let has_variants = l.variants.iter().any(|v| hits(&v.anchor));
    if !is_formula && input.is_none() && conf.is_none() && slides.is_empty() && !has_variants {
        return;
    }
    let open = std::mem::take(&mut st.pane.analysis_open) || st.pane.trace_open;
    let h = egui::CollapsingHeader::new(RichText::new("Analysis").strong()).id_salt("agent_analysis");
    let h = if open { h.open(Some(true)) } else { h };
    h.show(ui, |ui| {
        if let Some(i) = input {
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
        if let Some(conf) = conf {
            card(ui, t, overlay::heat_color(conf.level, 255), |ui| {
                ui.label(RichText::new(format!("Agent's confidence {:.0}%", conf.level * 100.0)).small().strong());
                if !conf.reason.is_empty() {
                    ui.label(&conf.reason);
                }
            });
        }
        if !slides.is_empty() {
            card(ui, t, p.slide, |ui| {
                ui.label(
                    RichText::new(format!("Used on {} slide{}", slides.len(), if slides.len() == 1 { "" } else { "s" }))
                        .color(p.slide)
                        .small()
                        .strong(),
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
        if has_variants {
            for cmp in st.overlay.comparisons(d, l).iter().filter(|x| x.place.sheet == si && x.place.range.intersects(&sel)) {
                if let Some(set) = l.variants.iter().find(|v| v.id == cmp.id) {
                    variant_card(app, ui, set, cmp, t, p, acts, false);
                }
            }
        }
        if !is_formula {
            return;
        }
        // Where does this number come from.
        let ver = overlay::version(d, l);
        let fresh = st.pane.trace.as_ref().is_some_and(|(v, s, k, _)| *v == ver && *s == si && *k == c);
        if !fresh {
            let private_cells = analysis::private_cells(&d.wb, l);
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
            egui::CollapsingHeader::new(RichText::new("Sensitivity").strong()).id_salt("agent_sens").show(ui, |ui| {
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
        if overlay::is_formula_number(d, si, c) {
            if let Some(r) = st.overlay.stress(d, l, si, c) {
                let k = r.dims.len();
                egui::CollapsingHeader::new(
                    RichText::new(format!("Worst and best case · {k} input{} together", if k == 1 { "" } else { "s" })).strong(),
                )
                .id_salt("agent_stress")
                .show(ui, |ui| stress_view(ui, d, &r, t, p, acts));
            }
            egui::CollapsingHeader::new(RichText::new("Risk matrix").strong()).id_salt("agent_risk").show(ui, |ui| risk_view(app, ui, l, t, p, acts));
        }
    });
}

fn ask_box(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let Some(sh) = d.wb.sheet(si) else { return };
    let here = Place { sheet: si, sheet_name: sh.name.clone(), range: d.selection.current() };
    let n = topics::numbering(&d.wb, l);
    let at = topics::topics_at(&d.wb, l, &here);
    // Where it goes: the chosen topic, the one opened, or the selection's first; or a new one.
    let into = if st.ask_new {
        None
    } else {
        st.ask_into.clone().filter(|x| at.contains(x)).or_else(|| st.focus_topic.clone().filter(|x| at.contains(x))).or_else(|| at.first().cloned())
    };
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        match &into {
            Some(id) => {
                let num = n.topic.get(id).copied().unwrap_or(0);
                ui.label(RichText::new(format!("Ask in topic {num}")).small().strong());
            }
            None => {
                ui.label(RichText::new(format!("Ask about {}", here.range.a1())).small().strong());
            }
        }
        if !at.is_empty() {
            ui.label(RichText::new("·").small().color(t.text_dim));
            for id in &at {
                let num = n.topic.get(id).copied().unwrap_or(0);
                let color = l.topic(id).map_or(t.text_dim, |x| p.status(x.status.status));
                let on = into.as_deref() == Some(id.as_str());
                let label = RichText::new(num.to_string()).small().strong();
                if ui
                    .selectable_label(on, if on { label.color(color) } else { label })
                    .on_hover_text(format!("Add the question to topic {num}"))
                    .clicked()
                {
                    st.ask_into = Some(id.clone());
                    st.ask_new = false;
                }
            }
            if ui
                .selectable_label(into.is_none(), RichText::new("New topic").small())
                .on_hover_text("Start a separate topic on these cells")
                .clicked()
            {
                st.ask_new = true;
            }
        }
    });
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
            let mut params = json!({"range": here.a1(), "text": st.draft.trim()});
            match &into {
                Some(id) => params["topic"] = json!(id),
                None if !at.is_empty() => params["newTopic"] = json!(true),
                None => {}
            }
            acts.push(Act::Run("agent.ask", params));
            st.draft.clear();
            st.ask_new = false;
            st.ask_into = None;
        }
        let send = if cfg!(target_os = "macos") { "⌘↩" } else { "Ctrl+Enter" };
        ui.label(RichText::new(format!("{send} · keep working, the marker tells you when it's answered")).small().color(t.text_dim));
    });
}

fn ask_closed(ui: &mut Ui, t: &Tokens) {
    ui.label(
        RichText::new(
            "Read-only copy: the topics and notes the author's agent prepared are here; new questions go through the workbook in GridCraft.",
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
            if ui.link(RichText::new(cell).small().strong()).clicked() {
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

/// Readiness for sign-off: (agreed, to decide, revise, open, for information).
pub(crate) fn signoff(l: &Layer) -> (usize, usize, usize, usize, usize) {
    let (agreed, all, revise, open, info) = topics::signoff(l);
    (agreed, all.saturating_sub(info), revise, open, info)
}

pub(crate) fn signoff_line(l: &Layer) -> String {
    let (agreed, decide, revise, open, info) = signoff(l);
    let mut s = format!("Agreed {agreed} of {decide}");
    if revise > 0 {
        s.push_str(&format!(" · {revise} revise"));
    }
    if open > 0 {
        s.push_str(&format!(" · {open} open"));
    }
    if info > 0 {
        s.push_str(&format!(" · {info} for information"));
    }
    s
}

fn overview_tab(st: &mut AgentUi, app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let wb = &d.wb;
    let n = topics::numbering(wb, l);
    if n.order.is_empty() {
        ui.label(
            RichText::new(if gridcraft_agent::read_only() {
                "Nothing prepared in this copy."
            } else {
                "No topics yet. Select cells and ask the agent in the Selection tab, or right-click and choose Ask the Agent."
            })
            .italics(),
        );
    } else {
        // Readiness for sign-off, as a line and a bar.
        let (agreed, decide, revise, open, _) = signoff(l);
        ui.label(RichText::new(signoff_line(l)).strong());
        if decide > 0 {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 6.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 3.0, t.separator);
            let mut x = rect.left();
            for (k, color) in [(agreed, p.agreed), (revise, p.revise), (open, p.open)] {
                let w = rect.width() * k as f32 / decide as f32;
                painter.rect_filled(egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, rect.height())), 0.0, color);
                x += w;
            }
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (f, label) in [
                (Filter::All, "All"),
                (Filter::Status(Status::Open), "Open"),
                (Filter::Status(Status::Revise), "Revise"),
                (Filter::Status(Status::Agreed), "Agreed"),
                (Filter::Status(Status::Info), "For information"),
                (Filter::Unread, "Unread"),
                (Filter::Working, "With the agent"),
            ] {
                if ui.selectable_label(st.filter == f, RichText::new(label).small()).clicked() {
                    st.filter = f;
                }
            }
        });
        ui.add_space(4.0);
        let mut shown = 0;
        for id in &n.order {
            let Some(tp) = l.topic(id) else { continue };
            let unread = topics::unread(l, id);
            let disc = topics::discussion(l, id);
            let with_agent = disc.iter().any(|q| matches!(q.status, QuestionStatus::Pending | QuestionStatus::Working));
            let keep = match st.filter {
                Filter::All => true,
                Filter::Status(s) => tp.status.status == s,
                Filter::Unread => unread,
                Filter::Working => with_agent,
            };
            if !keep {
                continue;
            }
            shown += 1;
            let num = n.topic.get(id).copied().unwrap_or(0);
            let status = tp.status.status;
            let notes = topics::notes(l, id);
            let rect = card(ui, t, p.status(status), |ui| {
                ui.horizontal_wrapped(|ui| {
                    pill(ui, &num.to_string(), p.status(status), unread, t);
                    status_word(ui, status, p);
                    if tp.proposed.is_some() {
                        ui.label(RichText::new("· agent suggests Agreed").small().color(p.agreed));
                    }
                    if with_agent {
                        ui.label(RichText::new("· with the agent").small().color(p.working));
                    }
                });
                // What it's about: the status text, else the latest question, else a note's title.
                let about = if !tp.status.text.is_empty() {
                    textlinks::plain(wb, l, &tp.status.text, wb.active_sheet)
                } else if let Some(q) = disc.iter().rev().find(|q| !q.text.is_empty()) {
                    textlinks::plain(wb, l, &q.text, wb.active_sheet)
                } else {
                    notes
                        .first()
                        .map(|x| if x.title.is_empty() { textlinks::plain(wb, l, &x.text, wb.active_sheet) } else { x.title.clone() })
                        .unwrap_or_default()
                };
                if !about.is_empty() {
                    ui.label(clip(&about, 140));
                }
                let mut counts = vec![];
                let k = tp.places.len();
                let addrs: Vec<String> =
                    tp.places.iter().filter_map(|x| place_of(app, l, &x.anchor)).take(3).map(|pl| textlinks::address(&pl, wb.active_sheet)).collect();
                counts.push(format!(
                    "{k} place{}{}",
                    if k == 1 { "" } else { "s" },
                    if addrs.is_empty() { String::new() } else { format!(" ({})", addrs.join(", ")) }
                ));
                if !notes.is_empty() {
                    counts.push(format!("{} note{}", notes.len(), if notes.len() == 1 { "" } else { "s" }));
                }
                let ur = disc.iter().filter(|q| q.status == QuestionStatus::Answered && !q.seen).count();
                if ur > 0 {
                    counts.push(format!("{ur} unread"));
                }
                ui.label(RichText::new(counts.join(" · ")).small().color(t.text_dim));
            });
            if ui.interact(rect, ui.id().with(("overview", id)), egui::Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                acts.push(Act::Topic(id.clone()));
            }
        }
        if shown == 0 {
            ui.label(RichText::new("No topics match.").italics().color(t.text_dim));
        }
    }
    // Every set of alternatives, wherever it is.
    if !l.variants.is_empty() {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new(format!("Alternatives ({})", l.variants.len())).strong()).id_salt("agent_all_variants").show(
            ui,
            |ui| {
                let list = st.overlay.comparisons(d, l);
                for kind in [VariantKind::Alternatives, VariantKind::Method] {
                    for c in list.iter().filter(|c| c.kind == kind) {
                        if let Some(set) = l.variants.iter().find(|v| v.id == c.id) {
                            variant_card(app, ui, set, c, t, p, acts, true);
                        }
                    }
                }
            },
        );
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

fn risk_view(app: &SheetApp, ui: &mut Ui, l: &Layer, t: &Tokens, p: &Palette, acts: &mut Vec<Act>) {
    let Some(d) = app.session.active() else { return };
    let si = d.wb.active_sheet;
    let c = d.selection.active;
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
