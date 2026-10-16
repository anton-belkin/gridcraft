//! Proposals instead of direct edits (the brief's part B). In a topic the agent doesn't change the
//! workbook: `run_command {topic, option?}` runs the command on a draft (a copy of the accepted
//! workbook, recalculated) and records it in the topic's proposal. The person sees old → new on the
//! changed cells, switches between options, and applies one: its commands are replayed on the
//! workbook as one undo step, their own action, with a decision note "chose B: …". A step that no
//! longer fits applies nothing and marks the proposal out of date.

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::CellRef;
use gridcraft_engine::model::Workbook;
use gridcraft_engine::{DocState, Result, Session};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Place};
use crate::commands::{bad, spec, str_p, text_p};
use crate::layer::{self, Actor, EditProposal, Layer, Note, ProposalOption, ProposalState, ProposalStep, Topic};
use crate::topics::{self, Part};

pub static SPECS: &[CommandSpec] = &[
    spec!(change "agent.describeOption", "Describe Proposal Option", "{topic, option, title, why} — names an option of the topic's proposal and says why", describe),
    spec!(change "agent.discardProposal", "Discard Proposal", "{topic, option?} — drops one option, or the whole proposal", discard),
    spec!(change "agent.applyProposal", "Apply Proposal", "{topic, option?} — replays the option's commands on the workbook as one undo step (a person's action)", apply),
    spec!(change "agent.rejectProposal", "Reject Proposal", "{topic, comment?} — the person turns the proposal down; a comment reaches the agent as a follow-up in the topic", reject),
    spec!(query "agent.proposalChanges", "Proposal Changes", "{topic, option?} → the cells an option changes, old → new (the person's preview)", changes_cmd),
];

/// Most options a proposal holds (A…H), and commands an option holds.
const MAX_OPTIONS: usize = 8;
const MAX_STEPS: usize = 200;
/// Most changed cells listed.
const MAX_CHANGES: usize = 5_000;

fn option_p(p: &Json) -> String {
    str_p(p, "option").map(|o| o.trim().to_ascii_uppercase()).filter(|o| !o.is_empty()).unwrap_or_else(|| "A".into())
}

fn person_only(p: &Json, cmd: &str) -> Result<()> {
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(cmd, "only a person applies or rejects a proposal"));
    }
    Ok(())
}

fn topic_id(s: &Session, p: &Json, cmd: &str) -> Result<String> {
    let r = text_p(p, "topic", cmd)?;
    let d = s.doc()?;
    topics::find_topic(&d.wb, &layer::get(d), &r).ok_or_else(|| bad(cmd, format!("no topic `{r}`")))
}

// ---------------------------------------------------------------- the draft

/// A cell an option changes: its value or formula (`edited`), or a result that recalculates.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub sheet: usize,
    pub cell: CellRef,
    pub old: String,
    pub new: String,
    pub edited: bool,
}

/// An option run on a copy of the accepted workbook.
pub struct Draft {
    session: Session,
    pub changes: Vec<Change>,
}

impl Draft {
    pub fn doc(&self) -> Option<&DocState> {
        self.session.active()
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        // The copy's own (empty) layer goes with it.
        for d in self.session.documents() {
            layer::forget(d.uid);
        }
    }
}

/// Runs `steps` on a copy of `d`'s workbook: the draft, or the step that failed and why.
pub fn build(d: &DocState, steps: &[ProposalStep]) -> std::result::Result<Draft, (usize, String)> {
    let mut session = Session::new();
    session.add_document(DocState::new((*d.wb).clone(), None, d.title.clone()));
    for (i, st) in steps.iter().enumerate() {
        crate::commands::AGENT_DEPTH.with(|x| x.set(x.get().saturating_add(1)));
        let r = session.execute(&st.command, st.params.clone());
        crate::commands::AGENT_DEPTH.with(|x| x.set(x.get().saturating_sub(1)));
        if let Err(e) = r {
            for x in session.documents() {
                layer::forget(x.uid);
            }
            return Err((i, e.to_string()));
        }
    }
    let changes = session.active().map(|x| diff(&d.wb, &x.wb)).unwrap_or_default();
    Ok(Draft { session, changes })
}

/// The cells whose value or formula differ, by sheet name (sheet indexes of `base`).
pub fn diff(base: &Workbook, draft: &Workbook) -> Vec<Change> {
    let mut out = vec![];
    for (bi, bsh) in base.sheets.iter().enumerate() {
        let Some(dsh) = draft.sheets.iter().find(|s| s.name == bsh.name) else { continue };
        let keys: BTreeSet<CellRef> = bsh.cells.iter().map(|(c, _)| c).chain(dsh.cells.iter().map(|(c, _)| c)).take(2_000_000).collect();
        for c in keys {
            let (a, b) = (bsh.cell(c), dsh.cell(c));
            let fa = a.and_then(|x| x.formula.as_ref()).map(|f| f.text.clone());
            let fb = b.and_then(|x| x.formula.as_ref()).map(|f| f.text.clone());
            let va = a.map(|x| x.value.clone()).unwrap_or_default();
            let vb = b.map(|x| x.value.clone()).unwrap_or_default();
            if fa == fb && va == vb {
                continue;
            }
            let edited = fa != fb || (fa.is_none() && va != vb);
            out.push(Change {
                sheet: bi,
                cell: c,
                old: gridcraft_engine::display::cell_text(base, bsh, c),
                new: gridcraft_engine::display::cell_text(draft, dsh, c),
                edited,
            });
            if out.len() >= MAX_CHANGES {
                return out;
            }
        }
    }
    out
}

type Key = (u64, usize, String, String, String);

/// Drafts by (document, workbook version, topic, option, its steps), newest last.
static CACHE: Mutex<Vec<(Key, Arc<std::result::Result<Draft, (usize, String)>>)>> = Mutex::new(Vec::new());

/// The draft of an option, cached until the workbook or the option changes.
pub fn draft(d: &DocState, t: &Topic, option: &str) -> Option<Arc<std::result::Result<Draft, (usize, String)>>> {
    let o = t.edits.as_ref()?.option(option)?;
    let key: Key = (d.uid, Arc::as_ptr(&d.wb) as usize, t.id.clone(), o.option.clone(), json!(o.steps).to_string());
    if let Ok(g) = CACHE.lock()
        && let Some((_, v)) = g.iter().find(|(k, _)| *k == key)
    {
        return Some(v.clone());
    }
    let v = Arc::new(build(d, &o.steps));
    if let Ok(mut g) = CACHE.lock() {
        g.push((key, v.clone()));
        let n = g.len();
        if n > 24 {
            g.drain(..n - 24);
        }
    }
    Some(v)
}

/// Every pending option's changed cells: (topic, option, summary, cells).
pub fn pending(d: &DocState, l: &Layer) -> Vec<(String, String, String, HashSet<(usize, CellRef)>)> {
    let mut out = vec![];
    for t in l.topics.iter().filter(|t| !t.hidden) {
        let Some(e) = t.edits.as_ref().filter(|e| e.state == ProposalState::Pending) else { continue };
        for o in &e.options {
            let Some(dr) = draft(d, t, &o.option) else { continue };
            let Ok(dr) = dr.as_ref() else { continue };
            let cells = dr.changes.iter().map(|c| (c.sheet, c.cell)).collect();
            out.push((t.id.clone(), o.option.clone(), summary(o), cells));
        }
    }
    out
}

/// "Lower fixed costs (3 changes)".
pub fn summary(o: &ProposalOption) -> String {
    let n = o.steps.len();
    let what = format!("{n} change{}", if n == 1 { "" } else { "s" });
    if o.title.is_empty() { what } else { format!("{} ({what})", o.title) }
}

/// The pending proposals touching `places`: `[{topic, number, option, summary}]`.
pub fn nearby(d: &DocState, l: &Layer, places: &[Place]) -> Json {
    let n = topics::numbering(&d.wb, l);
    let list: Vec<Json> = pending(d, l)
        .into_iter()
        .filter(|(_, _, _, cells)| cells.iter().any(|(si, c)| places.iter().any(|p| p.sheet == *si && p.range.contains(*c))))
        .map(|(t, o, sm, _)| json!({"topic": t, "number": n.topic.get(&t), "option": o, "summary": sm}))
        .collect();
    Json::Array(list)
}

/// What a topic's proposal shows the agent: `{options: [{option, title, why, changes}], state}`.
pub fn proposal_json(e: &EditProposal) -> Json {
    let mut o = json!({
        "options": e.options.iter().map(|o| json!({"option": o.option, "title": o.title, "why": o.why, "changes": o.steps.len()})).collect::<Vec<_>>(),
        "state": e.state.id(),
        "at": e.at,
    });
    if !e.chosen.is_empty() {
        o["chosen"] = json!(e.chosen);
    }
    if !e.comment.is_empty() {
        o["comment"] = json!(e.comment);
    }
    o
}

// ---------------------------------------------------------------- the agent's side

/// `run_command {topic, option?}`: runs the command on the option's draft and records it.
pub fn record(s: &mut Session, p: &Json, command: &str, params: Json, reason: String) -> Result<Json> {
    const CMD: &str = "agent.exec";
    let id = topic_id(s, p, CMD)?;
    let option = option_p(p);
    let d = s.doc()?;
    let l = layer::get(d);
    let Some(t) = l.topic(&id) else { return Err(bad(CMD, format!("no topic `{id}`"))) };
    // A proposal that was applied, rejected or out of date gives way to a new one.
    let mut e = t.edits.clone().filter(|e| e.state == ProposalState::Pending).unwrap_or_default();
    if e.option(&option).is_none() {
        if e.options.len() >= MAX_OPTIONS {
            return Err(bad(CMD, format!("a proposal has at most {MAX_OPTIONS} options")));
        }
        e.options.push(ProposalOption { option: option.clone(), ..ProposalOption::default() });
    }
    let Some(o) = e.options.iter_mut().find(|x| x.option.eq_ignore_ascii_case(&option)) else { return Err(bad(CMD, "no such option")) };
    if o.steps.len() >= MAX_STEPS {
        return Err(bad(CMD, format!("an option holds at most {MAX_STEPS} commands")));
    }
    o.steps.push(ProposalStep { command: command.to_string(), params, reason });
    // Validated on the draft before it is kept.
    if let Err((i, err)) = build(d, &o.steps) {
        return Err(bad(CMD, format!("step {} doesn't run on the workbook: {err}", i + 1)));
    }
    let n = o.steps.len();
    e.state = ProposalState::Pending;
    e.at = crate::now_ms();
    layer::update(d, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            t.edits = Some(e);
        }
    });
    Ok(json!({"ok": true, "command": command, "topic": id, "option": option, "changes": n, "applied": false,
        "note": "recorded in the topic's proposal, not applied: the person applies it in the document"}))
}

fn describe(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.describeOption";
    let id = topic_id(s, p, CMD)?;
    let option = option_p(p);
    let title: String = str_p(p, "title").unwrap_or("").chars().take(200).collect();
    let why: String = str_p(p, "why").unwrap_or("").chars().take(4_000).collect();
    let why = if why.is_empty() { why } else { crate::textlinks::store(s, &why)? };
    let d = s.doc()?;
    let ok = layer::update(d, |l| {
        let Some(e) = l.topics.iter_mut().find(|t| t.id == id).and_then(|t| t.edits.as_mut()) else { return false };
        match e.options.iter_mut().find(|o| o.option.eq_ignore_ascii_case(&option)) {
            Some(o) => {
                o.title = title;
                o.why = why;
                true
            }
            None => false,
        }
    });
    if ok {
        Ok(json!({"topic": id, "option": option}))
    } else {
        Err(bad(CMD, format!("topic `{id}` has no option {option}: record a command in it first")))
    }
}

fn discard(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.discardProposal";
    let id = topic_id(s, p, CMD)?;
    let option = str_p(p, "option").map(|o| o.trim().to_ascii_uppercase()).filter(|o| !o.is_empty());
    let d = s.doc()?;
    layer::update(d, |l| {
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == id) {
            match &option {
                Some(o) => {
                    if let Some(e) = t.edits.as_mut() {
                        e.options.retain(|x| !x.option.eq_ignore_ascii_case(o));
                        if e.options.is_empty() {
                            t.edits = None;
                        }
                    }
                }
                None => t.edits = None,
            }
        }
    });
    Ok(json!({"topic": id, "discarded": option.unwrap_or_else(|| "all".into())}))
}

fn changes_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.proposalChanges";
    // Real values: the person's preview only.
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(CMD, "read the draft with read_range {view: \"proposal\"}"));
    }
    let id = topic_id(s, p, CMD)?;
    let option = option_p(p);
    let d = s.doc()?;
    let l = layer::get(d);
    let t = l.topic(&id).ok_or_else(|| bad(CMD, "no topic"))?;
    let dr = draft(d, t, &option).ok_or_else(|| bad(CMD, format!("no option {option}")))?;
    match dr.as_ref() {
        Ok(dr) => Ok(json!({
            "topic": id,
            "option": option,
            "changes": dr.changes.iter().map(|c| json!({"cell": Place::of(&d.wb, c.sheet, c.cell).a1(), "old": c.old, "new": c.new, "edited": c.edited})).collect::<Vec<_>>(),
        })),
        Err((i, e)) => Ok(json!({"topic": id, "option": option, "outdated": true, "step": i + 1, "error": e})),
    }
}

// ---------------------------------------------------------------- the person's side

/// Apply as one undo step: the workbook before and after, and the layer's parts.
#[derive(Debug)]
struct ApplyStep {
    uid: u64,
    before: Arc<Workbook>,
    after: Arc<Workbook>,
    sel_before: gridcraft_engine::Selection,
    sel_after: gridcraft_engine::Selection,
    parts_before: Vec<Part>,
    parts_after: Vec<Part>,
}

impl ApplyStep {
    fn set(&self, s: &mut Session, undo: bool) {
        let Some(i) = s.documents().iter().position(|d| d.uid == self.uid) else { return };
        let active = s.active_index();
        s.set_active(i);
        if let Ok(d) = s.doc_mut() {
            d.wb = if undo { self.before.clone() } else { self.after.clone() };
            d.selection = if undo { self.sel_before.clone() } else { self.sel_after.clone() };
            d.calc.rebuild(&d.wb);
            d.revision += 1;
        }
        if let Some(d) = s.documents().iter().find(|d| d.uid == self.uid) {
            let parts = if undo { &self.parts_before } else { &self.parts_after };
            layer::update(d, |l| parts.iter().for_each(|p| p.apply(l)));
        }
        s.set_active(active);
    }
}

impl gridcraft_engine::ext::UndoStep for ApplyStep {
    fn undo(&self, s: &mut Session) {
        self.set(s, true);
    }
    fn redo(&self, s: &mut Session) {
        self.set(s, false);
    }
}

fn apply(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.applyProposal";
    person_only(p, CMD)?;
    let id = topic_id(s, p, CMD)?;
    let (uid, l) = {
        let d = s.doc()?;
        (d.uid, layer::get(d))
    };
    let t = l.topic(&id).cloned().ok_or_else(|| bad(CMD, "no topic"))?;
    let e = t.edits.clone().ok_or_else(|| bad(CMD, format!("topic `{id}` has no proposal")))?;
    let option = match str_p(p, "option") {
        Some(_) => option_p(p),
        None => e.options.first().map(|o| o.option.clone()).unwrap_or_else(|| "A".into()),
    };
    let o = e.option(&option).cloned().ok_or_else(|| bad(CMD, format!("no option {option}")))?;
    if e.state == ProposalState::Applied {
        return Err(bad(CMD, "already applied"));
    }
    // The other pending proposals, to mark the overlapping ones out of date.
    let others: Vec<(String, HashSet<(usize, CellRef)>)> = {
        let d = s.doc()?;
        pending(d, &l).into_iter().filter(|x| x.0 != id).map(|x| (x.0, x.3)).collect()
    };
    // Replay on the workbook; its own history entries are set aside: Apply is one step.
    let (before, sel_before, undo, redo) = {
        let d = s.doc_mut()?;
        (d.wb.clone(), d.selection.clone(), std::mem::take(&mut d.undo), std::mem::take(&mut d.redo))
    };
    let mut failed = None;
    for (i, st) in o.steps.iter().enumerate() {
        if let Err(err) = s.execute(&st.command, st.params.clone()) {
            failed = Some((i, err.to_string()));
            break;
        }
    }
    let d = s.doc_mut()?;
    d.undo = undo;
    d.redo = redo;
    if let Some((i, err)) = failed {
        d.wb = before;
        d.selection = sel_before;
        d.calc.rebuild(&d.wb);
        d.revision += 1;
        let d = s.doc()?;
        layer::update(d, |l| {
            if let Some(x) = l.topics.iter_mut().find(|x| x.id == id).and_then(|x| x.edits.as_mut()) {
                x.state = ProposalState::Outdated;
                x.at = crate::now_ms();
            }
        });
        return Err(bad(CMD, format!("out of date, ask the agent to redo (step {}: {err})", i + 1)));
    }
    let (after, sel_after) = (d.wb.clone(), d.selection.clone());
    let changed: HashSet<(usize, CellRef)> = diff(&before, &after).into_iter().map(|c| (c.sheet, c.cell)).collect();
    let now = crate::now_ms();
    // The layer: this proposal applied, a decision note, overlapping proposals out of date.
    let l = layer::get(s.doc()?);
    let mut parts_before = vec![];
    let mut parts_after = vec![];
    let mut applied = t.clone();
    if let Some(x) = applied.edits.as_mut() {
        x.state = ProposalState::Applied;
        x.chosen = o.option.clone();
        x.at = now;
    }
    parts_before.push(Part::Topic(id.clone(), Some(t.clone())));
    parts_after.push(Part::Topic(id.clone(), Some(applied)));
    let note_id = format!("n{}", l.next_id.saturating_add(1));
    let title = if o.title.is_empty() { summary(&o) } else { o.title.clone() };
    let anchor = t.places.first().map(|x| x.anchor.clone()).unwrap_or_default();
    let note = Note {
        id: note_id.clone(),
        anchor,
        topic: id.clone(),
        kind: "decision".into(),
        title: format!("Chose {}", o.option),
        text: format!("chose {}: {title}", o.option),
        author: Actor::Human,
        at: now,
        ..Note::default()
    };
    parts_before.push(Part::Note(note_id.clone(), None));
    parts_after.push(Part::Note(note_id, Some(note)));
    for (other, cells) in &others {
        if cells.is_disjoint(&changed) {
            continue;
        }
        if let Some(ot) = l.topic(other) {
            let mut x = ot.clone();
            if let Some(ed) = x.edits.as_mut() {
                ed.state = ProposalState::Outdated;
                ed.at = now;
            }
            parts_before.push(Part::Topic(other.clone(), Some(ot.clone())));
            parts_after.push(Part::Topic(other.clone(), Some(x)));
        }
    }
    let d = s.doc()?;
    layer::update(d, |l| {
        l.next_id = l.next_id.saturating_add(1);
        parts_after.iter().for_each(|p| p.apply(l));
        l.log(Actor::Human, "decision", format!("Applied option {} of the agent's proposal: {title}", o.option), CMD, None, id.clone());
    });
    let step = ApplyStep { uid, before, after, sel_before, sel_after, parts_before, parts_after };
    gridcraft_engine::ext::push_undo_step(s, &format!("Apply Option {}", o.option), Arc::new(step))?;
    Ok(json!({"topic": id, "option": o.option, "applied": true, "cells": changed.len()}))
}

fn reject(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.rejectProposal";
    person_only(p, CMD)?;
    let id = topic_id(s, p, CMD)?;
    let comment: String = str_p(p, "comment").unwrap_or("").trim().chars().take(4_000).collect();
    let d = s.doc()?;
    let l = layer::get(d);
    let t = l.topic(&id).cloned().ok_or_else(|| bad(CMD, "no topic"))?;
    if t.edits.as_ref().is_none_or(|e| e.state != ProposalState::Pending) {
        return Err(bad(CMD, "nothing pending to reject"));
    }
    let mut after = t.clone();
    if let Some(e) = after.edits.as_mut() {
        e.state = ProposalState::Rejected;
        e.comment.clone_from(&comment);
        e.at = crate::now_ms();
    }
    topics::record(s, "Reject Proposal", vec![Part::Topic(id.clone(), Some(t.clone()))], vec![Part::Topic(id.clone(), Some(after.clone()))]);
    let d = s.doc()?;
    layer::update(d, |l| {
        if let Some(x) = l.topics.iter_mut().find(|x| x.id == id) {
            *x = after;
        }
        l.log(
            Actor::Human,
            "decision",
            if comment.is_empty() { "Rejected the agent's proposal".into() } else { format!("Rejected the agent's proposal: {comment}") },
            CMD,
            None,
            id.clone(),
        );
    });
    // The comment reaches the agent as a follow-up in the topic.
    if !comment.is_empty() {
        let d = s.doc()?;
        let place = t.places.iter().find_map(|x| l.anchor(&x.anchor).and_then(|a| anchor::resolve(&d.wb, a)));
        let mut q = json!({"topic": id, "text": format!("I rejected your proposal: {comment}")});
        if let Some(pl) = place {
            q["range"] = json!(pl.a1());
        }
        s.execute("agent.ask", q)?;
    }
    Ok(json!({"topic": id, "rejected": true}))
}
