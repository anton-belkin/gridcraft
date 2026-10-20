//! Topics (Topics v1): one question or theme about the workbook, with its places, a status with
//! free text, notes and a discussion. Questions and notes name their topic; a topic holds its
//! places and status.
//!
//! - Numbering follows the document, like footnotes: topics by the position of their first place
//!   (sheet, row, column), places within a topic in the same order (`3`, or `3.1`, `3.2`…).
//! - Only a person sets Agreed; the agent proposes it with its reasons.
//! - What a person does is undoable ([`record`]): each action pushes its own step into the
//!   document's undo history (`gridcraft_engine::ext::push_undo_step`), beside the cell edits.
//!   Undoing a question takes it back: gone while nobody took it, withdrawn while the agent works
//!   on it, hidden (answer and all) once answered; redo sends it again, or shows it again.

use std::collections::HashMap;
use std::sync::Arc;

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::model::Workbook;
use gridcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Place};
use crate::commands::{actor_p, bad, place_p, spec, str_p, text_p};
use crate::layer::{self, Actor, Input, Layer, Note, Proposal, Question, QuestionStatus, SafeFile, Status, Topic, TopicPlace, TopicStatus};
use crate::textlinks;

pub static SPECS: &[CommandSpec] = &[
    spec!(query "agent.topics", "Topics", "{status?: open|revise|agreed|info} → the topics in document order: number, status, places, note titles, unread", topics_cmd),
    spec!(query "agent.topic", "Topic", "{topic: id or number} → everything about it, the discussion included", topic_cmd),
    spec!(change "agent.addPlaces", "Add Places to Topic", "{topic, places: [{range, reason}]} — places a change here affects", add_places),
    spec!(change "agent.widenPlace", "Widen Place", "{place: \"3.1\" | anchor id, to: range, reason} — the place becomes what the answer is really about (a whole row, a table)", widen_place),
    spec!(change "agent.rejectPlace", "Remove Place", "{topic, place} — a place leaves the topic", reject_place),
    spec!(change "agent.setStatus", "Set Topic Status", "{topic, status: open|revise|agreed|info, text?} — only a person sets agreed", set_status),
    spec!(change "agent.proposeAgreed", "Propose Agreed", "{topic, reasons} — the agent suggests Agreed; a person confirms", propose_agreed),
    spec!(change "agent.acceptProposal", "Accept Proposal", "{topic} — Agreed, as the agent proposed", accept_proposal),
];

// ---------------------------------------------------------------- numbering

/// Where an anchor is, for document order; a deleted one sits where its neighbours are.
fn position(wb: &Workbook, l: &Layer, anchor_id: &str) -> Option<(usize, u32, u32)> {
    let at = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(wb, a)).map(|p| (p.sheet, p.range.start.row, p.range.start.col));
    at(anchor_id).or_else(|| {
        let w = l.whereabouts.iter().rev().find(|w| w.anchor == anchor_id)?;
        w.before.as_deref().and_then(at).or_else(|| w.after.as_deref().and_then(at))
    })
}

/// Topic numbers and place numbers, in document order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Numbering {
    /// Topic id → number.
    pub topic: HashMap<String, u32>,
    /// (topic id, anchor id) → "3" or "3.2".
    pub place: HashMap<(String, String), String>,
    /// Topic ids in order.
    pub order: Vec<String>,
}

pub fn numbering(wb: &Workbook, l: &Layer) -> Numbering {
    let mut tops: Vec<(Option<(usize, u32, u32)>, u64, &Topic)> =
        l.topics.iter().filter(|t| !t.hidden).map(|t| (t.places.iter().filter_map(|p| position(wb, l, &p.anchor)).min(), t.created_at, t)).collect();
    tops.sort_by(|a, b| match (a.0, b.0) {
        (Some(x), Some(y)) => x.cmp(&y).then(a.1.cmp(&b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    });
    let mut n = Numbering::default();
    for (i, (_, _, t)) in tops.iter().enumerate() {
        let num = u32::try_from(i + 1).unwrap_or(u32::MAX);
        n.topic.insert(t.id.clone(), num);
        n.order.push(t.id.clone());
        let mut places: Vec<(Option<(usize, u32, u32)>, &TopicPlace)> = t.places.iter().map(|p| (position(wb, l, &p.anchor), p)).collect();
        places.sort_by_key(|a| a.0);
        let one = places.len() == 1;
        for (k, (_, p)) in places.iter().enumerate() {
            let label = if one { num.to_string() } else { format!("{num}.{}", k + 1) };
            n.place.insert((t.id.clone(), p.anchor.clone()), label);
        }
    }
    n
}

/// A topic by id (`t3`) or number (`3`).
pub fn find_topic(wb: &Workbook, l: &Layer, r: &str) -> Option<String> {
    let r = r.trim().trim_start_matches('#');
    if l.topics.iter().any(|t| t.id == r) {
        return Some(r.to_string());
    }
    let n: u32 = r.parse().ok()?;
    numbering(wb, l).topic.iter().find(|(_, v)| **v == n).map(|(k, _)| k.clone())
}

/// A place by number (`3.2`, or `3` for a one-place topic) or by anchor id: (topic, anchor).
pub fn find_place(wb: &Workbook, l: &Layer, r: &str) -> Option<(String, String)> {
    let r = r.trim();
    let n = numbering(wb, l);
    if let Some(((t, a), _)) = n.place.iter().find(|(_, v)| v.as_str() == r) {
        return Some((t.clone(), a.clone()));
    }
    l.topics.iter().find_map(|t| t.places.iter().find(|p| p.anchor == r).map(|p| (t.id.clone(), p.anchor.clone())))
}

/// The visible topics with a place touching `place`.
pub fn topics_at(wb: &Workbook, l: &Layer, place: &Place) -> Vec<String> {
    let n = numbering(wb, l);
    n.order
        .iter()
        .filter(|id| {
            l.topic(id).is_some_and(|t| {
                t.places.iter().any(|p| {
                    l.anchor(&p.anchor)
                        .and_then(|a| anchor::resolve(wb, a))
                        .is_some_and(|x| x.sheet == place.sheet && x.range.intersects(&place.range))
                })
            })
        })
        .cloned()
        .collect()
}

/// Sign-off: (agreed, all, revise, open, info) of the visible topics.
pub fn signoff(l: &Layer) -> (usize, usize, usize, usize, usize) {
    let vis: Vec<&Topic> = l.topics.iter().filter(|t| !t.hidden).collect();
    let count = |s: Status| vis.iter().filter(|t| t.status.status == s).count();
    (count(Status::Agreed), vis.len(), count(Status::Revise), count(Status::Open), count(Status::Info))
}

/// The discussion of a topic: its questions, oldest first (hidden ones left out).
pub fn discussion<'a>(l: &'a Layer, topic: &str) -> Vec<&'a Question> {
    l.questions.iter().filter(|q| q.topic == topic && !q.hidden && !q.is_ping()).collect()
}

pub fn notes<'a>(l: &'a Layer, topic: &str) -> Vec<&'a Note> {
    l.notes.iter().filter(|n| n.topic == topic).collect()
}

/// The agent is working on a question of the topic.
pub fn working(l: &Layer, topic: &str) -> bool {
    discussion(l, topic).iter().any(|q| q.status == QuestionStatus::Working)
}

/// An answer in the topic the reader hasn't read.
pub fn unread(l: &Layer, topic: &str) -> bool {
    discussion(l, topic).iter().any(|q| q.status == QuestionStatus::Answered && !q.seen)
}

// ---------------------------------------------------------------- what the agent reads

fn place_json(wb: &Workbook, l: &Layer, n: &Numbering, t: &Topic, p: &TopicPlace, home: usize) -> Json {
    let num = n.place.get(&(t.id.clone(), p.anchor.clone())).cloned().unwrap_or_default();
    let mut o = json!({"place": num, "anchor": p.anchor, "by": p.by, "reason": textlinks::render(wb, l, &p.reason, home)});
    match l.anchor(&p.anchor).and_then(|a| anchor::resolve(wb, a)) {
        Some(x) => o["range"] = json!(x.a1()),
        None => o["removed"] = json!(textlinks::render(wb, l, &format!("[@](anchor:{})", p.anchor), home)),
    }
    o
}

/// A topic as the agent reads it; `full` adds the notes' texts and the discussion.
pub fn topic_json(wb: &Workbook, l: &Layer, n: &Numbering, t: &Topic, full: bool) -> Json {
    let home = wb.active_sheet;
    let mut o = json!({
        "id": t.id,
        "number": n.topic.get(&t.id),
        "status": t.status.status.id(),
        "statusText": textlinks::render(wb, l, &t.status.text, home),
        "statusBy": t.status.by,
        "places": t.places.iter().map(|p| place_json(wb, l, n, t, p, home)).collect::<Vec<_>>(),
        "notes": notes(l, &t.id).iter().map(|x| {
            let mut j = json!({"id": x.id, "title": x.title, "kind": x.kind});
            if full {
                j["text"] = json!(textlinks::render(wb, l, &x.text, home));
                if !x.links.is_empty() {
                    j["links"] = json!(x.links);
                }
            }
            j
        }).collect::<Vec<_>>(),
        "unread": unread(l, &t.id),
        "working": working(l, &t.id),
        "questions": discussion(l, &t.id).len(),
    });
    if let Some(p) = &t.proposed {
        o["proposed"] = json!({"status": "agreed", "reasons": textlinks::render(wb, l, &p.reasons, home)});
    }
    if let Some(e) = &t.edits {
        let mut pr = crate::proposals::proposal_json(e);
        if let Some(opts) = pr.get_mut("options").and_then(Json::as_array_mut) {
            for o in opts.iter_mut() {
                if let Some(w) = o.get("why").and_then(Json::as_str).map(str::to_string) {
                    o["why"] = json!(textlinks::render(wb, l, &w, home));
                }
            }
        }
        o["proposal"] = pr;
    }
    if full {
        o["discussion"] = json!(
            discussion(l, &t.id)
                .iter()
                .map(|q| json!({
                    "id": q.id,
                    "text": textlinks::render(wb, l, &q.text, home),
                    "status": format!("{:?}", q.status).to_lowercase(),
                    "answer": q.answer.as_ref().map(|a| textlinks::render(wb, l, &a.text, home)),
                }))
                .collect::<Vec<_>>()
        );
    }
    o
}

fn view(s: &Session, p: &Json) -> Result<(Workbook, Arc<Layer>)> {
    let d = s.doc()?;
    let l = layer::get(d);
    let wb = if p.get("masked").and_then(Json::as_bool).unwrap_or(true) { crate::analysis::masked(d, &l).0 } else { (*d.wb).clone() };
    Ok((wb, l))
}

fn topics_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    let (wb, l) = view(s, p)?;
    let want = str_p(p, "status").and_then(Status::parse);
    let n = numbering(&wb, &l);
    let list: Vec<Json> = n
        .order
        .iter()
        .filter_map(|id| l.topic(id))
        .filter(|t| want.is_none_or(|w| t.status.status == w))
        .map(|t| topic_json(&wb, &l, &n, t, false))
        .collect();
    Ok(Json::Array(list))
}

fn topic_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    let r = text_p(p, "topic", "agent.topic")?;
    let (wb, l) = view(s, p)?;
    let id = find_topic(&wb, &l, &r).ok_or_else(|| bad("agent.topic", format!("no topic `{r}`")))?;
    let t = l.topic(&id).ok_or_else(|| bad("agent.topic", format!("no topic `{r}`")))?;
    Ok(topic_json(&wb, &l, &numbering(&wb, &l), t, true))
}

// ---------------------------------------------------------------- undo of a person's actions

/// A part of the layer as it was or will be.
#[derive(Clone, Debug)]
pub enum Part {
    Topic(String, Option<Topic>),
    Note(String, Option<Note>),
    Private(Vec<String>),
    Inputs(Vec<Input>),
    Safe(Option<SafeFile>),
    /// What the agent knows (the Agent tab's Context).
    Context(Option<layer::AgentContext>),
    /// The file's own context.
    FileContext(Vec<layer::FileContext>),
}

impl Part {
    /// A different state of the same part (safe file on vs off).
    pub fn differs(&self, other: &Part) -> bool {
        match (self, other) {
            (Part::Safe(a), Part::Safe(b)) => a.is_some() != b.is_some(),
            _ => true,
        }
    }

    pub(crate) fn apply(&self, l: &mut Layer) {
        match self {
            Part::Topic(id, t) => match (l.topics.iter().position(|x| &x.id == id), t) {
                (Some(i), Some(t)) => {
                    if let Some(x) = l.topics.get_mut(i) {
                        *x = t.clone();
                    }
                }
                (Some(i), None) => {
                    l.topics.remove(i);
                }
                (None, Some(t)) => l.topics.push(t.clone()),
                (None, None) => {}
            },
            Part::Note(id, n) => match (l.notes.iter().position(|x| &x.id == id), n) {
                (Some(i), Some(n)) => {
                    if let Some(x) = l.notes.get_mut(i) {
                        *x = n.clone();
                    }
                }
                (Some(i), None) => {
                    l.notes.remove(i);
                }
                (None, Some(n)) => l.notes.push(n.clone()),
                (None, None) => {}
            },
            Part::Private(v) => l.private.clone_from(v),
            Part::Inputs(v) => l.inputs.clone_from(v),
            Part::Safe(v) => l.safe.clone_from(v),
            Part::Context(v) => l.context.clone_from(v),
            Part::FileContext(v) => l.file_context.clone_from(v),
        }
    }
}

/// One undoable step of a person in the layer.
#[derive(Debug)]
pub struct Step {
    uid: u64,
    kind: StepKind,
    /// The anchors its parts name: put back if they were cleaned up meanwhile (a question taken
    /// back leaves its anchors unused until redo).
    anchors: Vec<layer::Anchor>,
}

/// Anchors the topics, notes and questions of a step name.
fn anchors_of(l: &Layer, kind: &StepKind) -> Vec<layer::Anchor> {
    let mut ids: Vec<String> = vec![];
    let topic = |ids: &mut Vec<String>, t: &Topic| ids.extend(t.places.iter().map(|p| p.anchor.clone()));
    match kind {
        StepKind::Parts { before, after } => {
            for p in before.iter().chain(after) {
                match p {
                    Part::Topic(_, Some(t)) => topic(&mut ids, t),
                    Part::Note(_, Some(n)) => ids.push(n.anchor.clone()),
                    Part::Private(v) => ids.extend(v.iter().cloned()),
                    Part::Inputs(v) => ids.extend(v.iter().map(|i| i.anchor.clone())),
                    _ => {}
                }
            }
        }
        StepKind::Ask(a) => {
            topic(&mut ids, &a.topic_after);
            ids.push(a.question.anchor.clone());
            ids.extend(a.question.refs.iter().map(|r| r.anchor.clone()));
        }
    }
    l.anchors.iter().filter(|x| ids.contains(&x.id)).cloned().collect()
}

fn restore_anchors(l: &mut Layer, anchors: &[layer::Anchor]) {
    for a in anchors {
        if !l.anchors.iter().any(|x| x.id == a.id) {
            l.anchors.push(a.clone());
        }
    }
}

#[derive(Debug)]
enum StepKind {
    Parts { before: Vec<Part>, after: Vec<Part> },
    Ask(Box<AskStep>),
}

/// A question asked: what to take back, and what to send again.
#[derive(Debug)]
pub struct AskStep {
    pub question: Question,
    pub topic_before: Option<Topic>,
    pub topic_after: Topic,
}

fn with_doc(s: &mut Session, uid: u64, f: impl FnOnce(&mut Layer)) {
    if let Some(d) = s.documents().iter().find(|d| d.uid == uid) {
        layer::update(d, f);
    }
}

impl gridcraft_engine::ext::UndoStep for Step {
    fn undo(&self, s: &mut Session) {
        self.undo_layer(s);
        // The runner follows the document's context.
        #[cfg(not(target_arch = "wasm32"))]
        crate::context::sync(s);
    }

    fn redo(&self, s: &mut Session) {
        self.redo_layer(s);
        #[cfg(not(target_arch = "wasm32"))]
        crate::context::sync(s);
    }
}

impl Step {
    fn undo_layer(&self, s: &mut Session) {
        match &self.kind {
            StepKind::Parts { before, .. } => with_doc(s, self.uid, |l| {
                restore_anchors(l, &self.anchors);
                before.iter().for_each(|p| p.apply(l));
            }),
            StepKind::Ask(a) => with_doc(s, self.uid, |l| {
                let state = l.questions.iter().find(|q| q.id == a.question.id).map(|q| q.status);
                match state {
                    // Being worked on: withdrawn (the agent drops it).
                    Some(QuestionStatus::Working) => {
                        if let Some(q) = l.questions.iter_mut().find(|q| q.id == a.question.id) {
                            q.status = QuestionStatus::Withdrawn;
                        }
                    }
                    // Answered: hidden with its answer (and its topic, if it made one).
                    Some(QuestionStatus::Answered) => {
                        if let Some(q) = l.questions.iter_mut().find(|q| q.id == a.question.id) {
                            q.hidden = true;
                        }
                        if a.topic_before.is_none()
                            && let Some(t) = l.topics.iter_mut().find(|t| t.id == a.topic_after.id)
                        {
                            t.hidden = true;
                        }
                    }
                    // Not taken yet (or still being transcribed): gone, with the places it added.
                    Some(_) => {
                        l.questions.retain(|q| q.id != a.question.id);
                        Part::Topic(a.topic_after.id.clone(), a.topic_before.clone()).apply(l);
                    }
                    None => {}
                }
            }),
        }
    }

    fn redo_layer(&self, s: &mut Session) {
        match &self.kind {
            StepKind::Parts { after, .. } => with_doc(s, self.uid, |l| {
                restore_anchors(l, &self.anchors);
                after.iter().for_each(|p| p.apply(l));
            }),
            StepKind::Ask(a) => with_doc(s, self.uid, |l| {
                restore_anchors(l, &self.anchors);
                match l.questions.iter_mut().find(|q| q.id == a.question.id) {
                    // Taken back while answered: shown again as it was, answer included.
                    Some(q) if q.hidden => {
                        q.hidden = false;
                        if let Some(t) = l.topics.iter_mut().find(|t| t.id == a.topic_after.id) {
                            t.hidden = false;
                        }
                    }
                    // Withdrawn: sent again.
                    Some(q) => {
                        if q.status == QuestionStatus::Withdrawn {
                            q.status = QuestionStatus::Pending;
                            q.answer = None;
                            q.asked_at = crate::now_ms();
                        }
                    }
                    // Taken back before anyone took it: asked again, with its places.
                    None => {
                        let mut q = a.question.clone();
                        if !q.status.is_draft() {
                            q.status = QuestionStatus::Pending;
                        }
                        q.asked_at = crate::now_ms();
                        q.answer = None;
                        l.questions.push(q);
                        let merged = match l.topics.iter().find(|t| t.id == a.topic_after.id) {
                            Some(cur) => {
                                let mut t = cur.clone();
                                for p in &a.topic_after.places {
                                    if !t.places.iter().any(|x| x.anchor == p.anchor) {
                                        t.places.push(p.clone());
                                    }
                                }
                                t
                            }
                            None => a.topic_after.clone(),
                        };
                        Part::Topic(merged.id.clone(), Some(merged)).apply(l);
                    }
                }
            }),
        }
    }
}

/// A person's action (not the agent's, not one replayed by undo).
pub fn by_person(p: &Json) -> bool {
    actor_p(p, Actor::Human) == Actor::Human
}

/// Records a person's action as one undo step of the active document.
pub fn record(s: &mut Session, label: &str, before: Vec<Part>, after: Vec<Part>) {
    let Ok(d) = s.doc() else { return };
    let uid = d.uid;
    let kind = StepKind::Parts { before, after };
    let anchors = anchors_of(&layer::get(d), &kind);
    let _ = gridcraft_engine::ext::push_undo_step(s, label, Arc::new(Step { uid, kind, anchors }));
}

/// Records a question asked by a person.
pub fn record_ask(s: &mut Session, label: &str, a: AskStep) {
    let Ok(d) = s.doc() else { return };
    let uid = d.uid;
    let kind = StepKind::Ask(Box::new(a));
    let anchors = anchors_of(&layer::get(d), &kind);
    let _ = gridcraft_engine::ext::push_undo_step(s, label, Arc::new(Step { uid, kind, anchors }));
}

fn topic_part(l: &Layer, id: &str) -> Part {
    Part::Topic(id.to_string(), l.topic(id).cloned())
}

// ---------------------------------------------------------------- deleted places keep their whereabouts

/// `"2:4"`, `"3"` (rows, 1-based) or `"B:C"`, `"B"` (columns) → zero-based first and last.
fn span(text: &str, cols: bool) -> Option<(u32, u32)> {
    let one = |t: &str| -> Option<u32> {
        let t = t.trim().trim_start_matches('$');
        if cols {
            let mut n: u32 = 0;
            for c in t.chars() {
                if !c.is_ascii_alphabetic() {
                    return None;
                }
                n = n.checked_mul(26)?.checked_add(u32::from(c.to_ascii_uppercase() as u8 - b'A') + 1)?;
            }
            n.checked_sub(1)
        } else {
            t.parse::<u32>().ok()?.checked_sub(1)
        }
    };
    let (a, b) = text.split_once(':').unwrap_or((text, text));
    let (a, b) = (one(a)?, one(b)?);
    Some((a.min(b), a.max(b)))
}

/// Just before cells are deleted: every place and link wholly inside them remembers its
/// neighbours (anchored, so they follow later edits) and its address.
pub fn before_delete(s: &mut Session, id: &str, p: &Json) {
    let Ok(d) = s.doc() else { return };
    let si = d.wb.active_sheet;
    let sel = d.selection.current();
    // (rows?, first, last, within columns or rows of the other axis)
    let target = match id {
        "home.deleteRows" => str_p(p, "rows").and_then(|r| span(r, false)).or(Some((sel.start.row, sel.end.row))).map(|(a, b)| (true, a, b, None)),
        "home.deleteColumns" => str_p(p, "cols").and_then(|r| span(r, true)).or(Some((sel.start.col, sel.end.col))).map(|(a, b)| (false, a, b, None)),
        "home.deleteCells" => {
            let r = str_p(p, "range").and_then(|t| anchor::parse_place(&d.wb, t, si)).map_or(sel, |x| x.range);
            match str_p(p, "shift").unwrap_or("up") {
                "row" => Some((true, r.start.row, r.end.row, None)),
                "column" => Some((false, r.start.col, r.end.col, None)),
                "left" => Some((false, r.start.col, r.end.col, Some((r.start.row, r.end.row)))),
                _ => Some((true, r.start.row, r.end.row, Some((r.start.col, r.end.col)))),
            }
        }
        _ => None,
    };
    let l = layer::get(d);
    let wb = d.wb.clone();
    let mut todo: Vec<(String, String, Option<Place>, Option<Place>)> = vec![];
    for a in &l.anchors {
        let Some(at) = anchor::resolve(&wb, a) else { continue };
        let sheet_name = at.sheet_name.clone();
        let cell = |r: u32, c: u32| Place {
            sheet: at.sheet,
            sheet_name: sheet_name.clone(),
            range: gridcraft_engine::core::RangeRef::cell(gridcraft_engine::core::CellRef::new(r, c)),
        };
        if id == "home.deleteSheet" {
            let which =
                p.get("sheet").and_then(Json::as_u64).map(|x| x as usize).or_else(|| str_p(p, "sheet").and_then(|n| wb.sheet_index(n))).unwrap_or(si);
            if at.sheet == which {
                todo.push((a.id.clone(), format!("{} (its sheet was deleted)", at.a1()), None, None));
            }
            continue;
        }
        let Some((rows, first, last, within)) = target else { continue };
        if at.sheet != si {
            continue;
        }
        let r = at.range;
        let inside = if rows {
            r.start.row >= first && r.end.row <= last && within.is_none_or(|(c0, c1)| r.start.col >= c0 && r.end.col <= c1)
        } else {
            r.start.col >= first && r.end.col <= last && within.is_none_or(|(r0, r1)| r.start.row >= r0 && r.end.row <= r1)
        };
        if !inside {
            continue;
        }
        let (before, after) = if rows {
            (first.checked_sub(1).map(|x| cell(x, r.start.col)), last.checked_add(1).filter(|x| *x < 1_048_576).map(|x| cell(x, r.start.col)))
        } else {
            (first.checked_sub(1).map(|x| cell(r.start.row, x)), last.checked_add(1).filter(|x| *x < 16_384).map(|x| cell(r.start.row, x)))
        };
        todo.push((a.id.clone(), at.a1(), before, after));
    }
    for (anchor_id, was, before, after) in todo {
        let b = before.and_then(|x| anchor::anchor_at(s, &x).ok());
        let a = after.and_then(|x| anchor::anchor_at(s, &x).ok());
        if let Ok(d) = s.doc() {
            layer::update(d, |l| {
                l.whereabouts.retain(|w| w.anchor != anchor_id);
                l.whereabouts.push(layer::Whereabouts { anchor: anchor_id, before: b, after: a, was });
            });
        }
    }
}

// ---------------------------------------------------------------- changing topics

fn topic_p(s: &Session, p: &Json, cmd: &str) -> Result<String> {
    let r = str_p(p, "topic")
        .map(str::to_string)
        .or_else(|| p.get("topic").and_then(Json::as_u64).map(|n| n.to_string()))
        .ok_or_else(|| bad(cmd, "missing `topic`"))?;
    let d = s.doc()?;
    let l = layer::get(d);
    find_topic(&d.wb, &l, &r).ok_or_else(|| bad(cmd, format!("no topic `{r}`")))
}

/// Applies a change to one topic; a person's change is one undo step.
fn change_topic(s: &mut Session, p: &Json, id: &str, label: &str, f: impl FnOnce(&mut Topic)) -> Result<()> {
    let before = topic_part(&layer::get(s.doc()?), id);
    layer::update(s.doc()?, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            f(t);
        }
    });
    if by_person(p) {
        let after = topic_part(&layer::get(s.doc()?), id);
        record(s, label, vec![before], vec![after]);
    }
    Ok(())
}

/// A new topic on `place`; its id.
pub fn new_topic(s: &mut Session, place: &Place, status: Status, by: Actor) -> Result<String> {
    let a = anchor::anchor_at(s, place)?;
    let now = crate::now_ms();
    Ok(layer::update(s.doc()?, |l| {
        let id = l.fresh_id("t");
        l.topics.push(Topic {
            id: id.clone(),
            places: vec![TopicPlace { anchor: a, reason: String::new(), by, at: now }],
            status: TopicStatus { status, text: String::new(), by, at: now },
            proposed: None,
            hidden: false,
            created_at: now,
            edits: None,
        });
        id
    }))
}

fn add_places(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.addPlaces";
    let id = topic_p(s, p, CMD)?;
    let list = p.get("places").and_then(Json::as_array).ok_or_else(|| bad(CMD, "missing `places` ([{range, reason}])"))?.clone();
    let by = actor_p(p, Actor::Human);
    let mut added = vec![];
    for x in list.iter().take(50) {
        let place = place_p(s, x, CMD)?;
        let a = anchor::anchor_at(s, &place)?;
        let reason = match str_p(x, "reason") {
            Some(r) => textlinks::store(s, &r.chars().take(2_000).collect::<String>())?,
            None => String::new(),
        };
        added.push(TopicPlace { anchor: a, reason, by, at: crate::now_ms() });
    }
    let n = added.len();
    change_topic(s, p, &id, "Add Places", |t| {
        for a in added {
            if !t.places.iter().any(|x| x.anchor == a.anchor) {
                t.places.push(a);
            }
        }
    })?;
    Ok(json!({"topic": id, "added": n}))
}

fn widen_place(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.widenPlace";
    let r = text_p(p, "place", CMD)?;
    let (topic, old) = {
        let d = s.doc()?;
        find_place(&d.wb, &layer::get(d), &r).ok_or_else(|| bad(CMD, format!("no place `{r}`")))?
    };
    let to = str_p(p, "to").ok_or_else(|| bad(CMD, "missing `to`"))?;
    let place = {
        let d = s.doc()?;
        anchor::parse_place(&d.wb, to, d.wb.active_sheet).ok_or_else(|| bad(CMD, format!("not a range: `{to}`")))?
    };
    let a = anchor::anchor_at(s, &place)?;
    let reason = match str_p(p, "reason") {
        Some(x) => textlinks::store(s, &x.chars().take(2_000).collect::<String>())?,
        None => String::new(),
    };
    let by = actor_p(p, Actor::Human);
    change_topic(s, p, &topic, "Widen Place", |t| {
        if let Some(x) = t.places.iter_mut().find(|x| x.anchor == old) {
            x.anchor = a;
            x.reason = reason;
            x.by = by;
        }
    })?;
    Ok(json!({"topic": topic, "place": place.a1()}))
}

fn reject_place(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.rejectPlace";
    let r = text_p(p, "place", CMD)?;
    let (topic, anchor_id) = {
        let d = s.doc()?;
        find_place(&d.wb, &layer::get(d), &r).ok_or_else(|| bad(CMD, format!("no place `{r}`")))?
    };
    change_topic(s, p, &topic, "Remove Place", |t| t.places.retain(|x| x.anchor != anchor_id))?;
    Ok(json!({"topic": topic}))
}

fn set_status(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.setStatus";
    let id = topic_p(s, p, CMD)?;
    let status = str_p(p, "status").and_then(Status::parse).ok_or_else(|| bad(CMD, "`status` is open, revise, agreed or info"))?;
    let by = actor_p(p, Actor::Human);
    if by == Actor::Agent && status == Status::Agreed {
        return Err(bad(CMD, "only a person sets Agreed: propose it with propose_agreed {topic, reasons}"));
    }
    let text = match str_p(p, "text") {
        Some(t) => textlinks::store(s, &t.chars().take(4_000).collect::<String>())?,
        None => String::new(),
    };
    let shown = textlinks::plain(&s.doc()?.wb, &layer::get(s.doc()?), &text, 0);
    change_topic(s, p, &id, "Set Status", |t| {
        t.status = TopicStatus { status, text, by, at: crate::now_ms() };
        // A person's choice settles what the agent proposed.
        if by == Actor::Human {
            t.proposed = None;
        }
    })?;
    let what = if shown.is_empty() { status.label().to_string() } else { format!("{}: {shown}", status.label()) };
    layer::update(s.doc()?, |l| l.log(by, "decision", format!("Topic status {what}"), CMD, None, id.clone()));
    Ok(json!({"topic": id, "status": status.id()}))
}

fn propose_agreed(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.proposeAgreed";
    let id = topic_p(s, p, CMD)?;
    let reasons = textlinks::store(s, &text_p(p, "reasons", CMD)?)?;
    change_topic(s, p, &id, "Propose Agreed", |t| t.proposed = Some(Proposal { reasons, at: crate::now_ms() }))?;
    layer::update(s.doc()?, |l| l.log(Actor::Agent, "decision", "Proposed Agreed".into(), CMD, None, id.clone()));
    Ok(json!({"topic": id, "proposed": "agreed"}))
}

fn accept_proposal(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.acceptProposal";
    if actor_p(p, Actor::Human) == Actor::Agent {
        return Err(bad(CMD, "only a person sets Agreed"));
    }
    let id = topic_p(s, p, CMD)?;
    let Some(reasons) = layer::get(s.doc()?).topic(&id).and_then(|t| t.proposed.as_ref().map(|x| x.reasons.clone())) else {
        return Err(bad(CMD, "nothing proposed"));
    };
    change_topic(s, p, &id, "Agree", |t| {
        t.status = TopicStatus { status: Status::Agreed, text: reasons, by: Actor::Human, at: crate::now_ms() };
        t.proposed = None;
    })?;
    layer::update(s.doc()?, |l| l.log(Actor::Human, "decision", "Topic status Agreed (as the agent proposed)".into(), CMD, None, id.clone()));
    Ok(json!({"topic": id, "status": "agreed"}))
}
