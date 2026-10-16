//! The `agent.*` commands. The reader's side (asking, marking private data and inputs) and the
//! agent's side (answers, notes, masked reads, traces) are both commands, so the UI, the control
//! channel and the in-app MCP server reach them the same way.
//!
//! Agent-facing reads take `masked` (default true): private cells show stand-ins and everything
//! computed from them is recalculated from those, so no real client number leaves the app.

use std::cell::Cell as StdCell;

use gridcraft_engine::calc::Key;
use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::RangeRef;
use gridcraft_engine::model::Workbook;
use gridcraft_engine::{EngineError, Result, Session};
use serde_json::{Value as Json, json};

use crate::analysis;
use crate::anchor::{self, Place};
use crate::layer::{self, Actor, Answer, Confidence, Input, Layer, Link, Note, Question, QuestionStatus};

macro_rules! spec {
    ($kind:ident $id:literal, $label:literal, $params:literal, $run:expr) => {
        gridcraft_engine::cmd::CommandSpec {
            id: $id,
            label: $label,
            menu: &[],
            shortcut: None,
            params: $params,
            enabled: gridcraft_engine::cmd::has_doc,
            run: {
                fn guarded(s: &mut gridcraft_engine::Session, p: &serde_json::Value) -> gridcraft_engine::Result<serde_json::Value> {
                    $crate::commands::writable($id, spec!(@journal $kind))?;
                    $run(s, p)
                }
                guarded
            },
            journal: spec!(@journal $kind),
            undoable: false,
        }
    };
    (@journal query) => { false };
    (@journal change) => { true };
}
pub(crate) use spec;

/// Every command of the layer, by module.
pub static ALL: &[&[CommandSpec]] = &[
    SPECS,
    crate::variants::SPECS,
    crate::stress::SPECS,
    crate::safe::SPECS,
    crate::links::SPECS,
    #[cfg(not(target_arch = "wasm32"))]
    crate::runner::SPECS,
    #[cfg(not(target_arch = "wasm32"))]
    crate::cowork::SPECS,
];

pub static SPECS: &[CommandSpec] = &[
    // the reader's side
    spec!(change "agent.ask", "Ask the Agent", "{range?: \"B5\" | \"Sheet1!B2:C4\" (default: selection), text}", ask),
    spec!(change "agent.seen", "Mark Answer Read", "{id}", seen),
    spec!(change "agent.private", "Mark as Client Data", "{range?, on?: bool (default true)} — the agent sees stand-in values there", private),
    spec!(change "agent.removeNote", "Remove Agent Note", "{id}", remove_note),
    // the agent's side
    spec!(query "agent.questions", "Agent Questions", "{status?: pending|working|answered|all (default all)} → questions with their cell", questions),
    spec!(change "agent.claim", "Take Question", "{id} — marks it as being worked on", claim),
    spec!(change "agent.answer", "Answer Question", "{id, text, links?: [{label, href, summary?}]}", answer),
    spec!(change "agent.note", "Add Agent Note", "{range, text, title?, kind?: explanation|faq|assumption|source, links?: [{label, href, summary?}], actor?}", note),
    spec!(change "agent.input", "Set Input Range", "{cell, low, high, relative?: bool (multipliers of the value), rationale?} — or {cell, remove: true}", input),
    spec!(change "agent.confidence", "Set Confidence", "{range, level: 0..1, reason?} — or {range, remove: true}", confidence),
    spec!(change "agent.decision", "Log Decision", "{text, range?, source?, actor?}", decision),
    spec!(query "agent.layer", "Agent Layer", "{} → anchors (resolved), questions, notes, inputs, private ranges, confidence, log", layer_cmd),
    spec!(query "agent.context", "Agent Context", "{masked?: bool} → sheets with used ranges and formulas, the layer summary", context),
    spec!(query "agent.read", "Agent Read", "{range, masked?: bool (default true)} → cells with value, text, formula, private/derivedFromPrivate", read),
    spec!(query "agent.trace", "Where Does This Number Come From", "{cell?, depth?: 1..12 (default 6), masked?: bool} → the calculation tree down to its inputs", trace),
    spec!(query "agent.sensitivity", "Sensitivity", "{cell?, limit?: (default 8), masked?: bool} → output at the low and high end of each input", sensitivity),
    spec!(query "agent.exportHtml", "Export as Web Page", "{path: \"out.html\", web?: folder with the web build (default $GRIDCRAFT_WEB_BUNDLE, then target/agent-web)} — one HTML file: the workbook in GridCraft for the web, prepared answers and notes read-only", crate::export::export_html),
    spec!(change "agent.exec", "Run Command as Agent", "{command, params?, reason?, source?} — runs a changing command, logged as the agent's", exec),
];

// ---------------------------------------------------------------- params

/// A read-only layer (an exported page) refuses every change but marking an answer read and
/// switching between the alternatives the author left (that only changes the reader's copy).
pub(crate) fn writable(id: &str, changes: bool) -> Result<()> {
    if changes && crate::read_only() && !matches!(id, "agent.seen" | "agent.useVariant") {
        return Err(EngineError::Other("This is a read-only copy: questions go to the agent from the workbook in GridCraft.".into()));
    }
    Ok(())
}

pub(crate) fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

pub(crate) fn str_p<'a>(p: &'a Json, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Json::as_str)
}

pub(crate) fn masked_p(p: &Json) -> bool {
    p.get("masked").and_then(Json::as_bool).unwrap_or(true)
}

pub(crate) fn actor_p(p: &Json, default: Actor) -> Actor {
    match str_p(p, "actor") {
        Some("agent") => Actor::Agent,
        Some("human") => Actor::Human,
        _ if AGENT_DEPTH.with(StdCell::get) > 0 => Actor::Agent,
        _ => default,
    }
}

/// `range` / `cell` param (on the active sheet unless it names one), else the selection.
pub(crate) fn place_p(s: &Session, p: &Json, cmd: &str) -> Result<Place> {
    let d = s.doc()?;
    let text = str_p(p, "range").or_else(|| str_p(p, "cell"));
    match text {
        Some(t) => anchor::parse_place(&d.wb, t, d.wb.active_sheet).ok_or_else(|| bad(cmd, format!("not a range: `{t}`"))),
        None => {
            let si = d.wb.active_sheet;
            let sheet_name = d.wb.sheet(si).map(|x| x.name.clone()).unwrap_or_default();
            Ok(Place { sheet: si, sheet_name, range: d.selection.current() })
        }
    }
}

pub(crate) fn links_p(p: &Json) -> Vec<Link> {
    p.get("links")
        .and_then(Json::as_array)
        .map(|a| a.iter().take(50).filter_map(|v| serde_json::from_value(v.clone()).ok()).collect())
        .unwrap_or_default()
}

pub(crate) fn text_p(p: &Json, k: &str, cmd: &str) -> Result<String> {
    let t = str_p(p, k).map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad(cmd, format!("missing `{k}`")))?;
    // Long enough for an essay, short enough to never be a problem in the file.
    Ok(t.chars().take(20_000).collect())
}

pub(crate) fn where_json(s: &Session, l: &Layer, anchor_id: &str) -> Json {
    let Ok(d) = s.doc() else { return Json::Null };
    match l.anchor(anchor_id).and_then(|a| anchor::resolve(&d.wb, a)) {
        Some(p) => json!(p.a1()),
        None => Json::Null,
    }
}

// ---------------------------------------------------------------- reader's side

fn ask(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.ask")?;
    let place = place_p(s, p, "agent.ask")?;
    let a = anchor::anchor_at(s, &place)?;
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("q");
        l.questions.push(Question {
            id: id.clone(),
            anchor: a,
            text,
            status: QuestionStatus::Pending,
            asked_at: crate::now_ms(),
            answer: None,
            seen: false,
        });
        id
    });
    Ok(json!({"id": id, "range": place.a1()}))
}

fn seen(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.seen")?;
    let ok = layer::update(s.doc()?, |l| l.questions.iter_mut().find(|q| q.id == id).map(|q| q.seen = true).is_some());
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.seen", format!("no question `{id}`"))) }
}

fn private(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.private")?;
    let on = p.get("on").and_then(Json::as_bool).unwrap_or(true);
    if actor_p(p, Actor::Human) == Actor::Agent && !on {
        return Err(bad("agent.private", "only a person can lift the privacy mark"));
    }
    let wb = s.doc()?.wb.clone();
    if on {
        let a = anchor::anchor_at(s, &place)?;
        layer::update(s.doc()?, |l| {
            if !l.private.contains(&a) {
                l.private.push(a);
            }
        });
    } else {
        layer::update(s.doc()?, |l| {
            let hits: Vec<String> = l
                .anchors
                .iter()
                .filter(|a| anchor::resolve(&wb, a).is_some_and(|x| x.sheet == place.sheet && x.range.intersects(&place.range)))
                .map(|a| a.id.clone())
                .collect();
            l.private.retain(|id| !hits.contains(id));
        });
    }
    Ok(json!({"range": place.a1(), "private": on}))
}

fn remove_note(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeNote")?;
    let ok = layer::update(s.doc()?, |l| {
        let n = l.notes.len();
        l.notes.retain(|x| x.id != id);
        n != l.notes.len()
    });
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.removeNote", format!("no note `{id}`"))) }
}

// ---------------------------------------------------------------- agent's side

/// A question with what the reader is looking at, so the agent can start without another call.
/// `wb` is the workbook as the caller may see it (masked for the agent).
fn question_json(s: &Session, l: &Layer, q: &Question, wb: &Workbook) -> Json {
    let mut o = serde_json::to_value(q).unwrap_or(Json::Null);
    o["range"] = where_json(s, l, &q.anchor);
    if let Some(place) = l.anchor(&q.anchor).and_then(|a| anchor::resolve(wb, a))
        && let Some(sh) = wb.sheet(place.sheet)
    {
        let c = place.range.start;
        o["label"] = json!(analysis::label_of(sh, c));
        o["cellText"] = json!(gridcraft_engine::display::cell_text(wb, sh, c));
        if let Some(f) = sh.cell(c).and_then(|x| x.formula.as_ref()) {
            o["formula"] = json!(format!("={}", f.text.trim_start_matches('=')));
        }
    }
    o
}

fn questions(s: &mut Session, p: &Json) -> Result<Json> {
    let l = layer::get(s.doc()?);
    let want = str_p(p, "status").unwrap_or("all");
    let d = s.doc()?;
    let has_any = l.questions.iter().any(|q| status_matches(q, want));
    // One masked copy for all of them (and none when there is nothing to show).
    let wb = if !has_any {
        Workbook::new()
    } else if masked_p(p) {
        analysis::masked(d, &l).0
    } else {
        (*d.wb).clone()
    };
    let list: Vec<Json> = l.questions.iter().filter(|q| status_matches(q, want)).map(|q| question_json(s, &l, q, &wb)).collect();
    Ok(Json::Array(list))
}

fn status_matches(q: &Question, want: &str) -> bool {
    match want {
        "pending" => q.status == QuestionStatus::Pending,
        "working" => q.status == QuestionStatus::Working,
        "answered" => q.status == QuestionStatus::Answered,
        "open" => q.status != QuestionStatus::Answered,
        _ => true,
    }
}

fn claim(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.claim")?;
    let r = layer::update(s.doc()?, |l| match l.questions.iter_mut().find(|q| q.id == id) {
        Some(q) if q.status == QuestionStatus::Pending => {
            q.status = QuestionStatus::Working;
            Ok(())
        }
        Some(q) => Err(format!("question `{id}` is {:?}", q.status).to_lowercase()),
        None => Err(format!("no question `{id}`")),
    });
    r.map(|()| json!({"id": id})).map_err(|m| bad("agent.claim", m))
}

fn answer(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.answer")?;
    let text = text_p(p, "text", "agent.answer")?;
    let links = links_p(p);
    let ok = layer::update(s.doc()?, |l| {
        let Some(q) = l.questions.iter_mut().find(|q| q.id == id) else { return false };
        q.status = QuestionStatus::Answered;
        q.seen = false;
        q.answer = Some(Answer { text, links, at: crate::now_ms() });
        let (anchor, qtext) = (q.anchor.clone(), q.text.clone());
        l.log(Actor::Agent, "decision", format!("Answered “{qtext}”"), "agent.answer", Some(anchor), id.clone());
        true
    });
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.answer", format!("no question `{id}`"))) }
}

fn note(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.note")?;
    let place = place_p(s, p, "agent.note")?;
    let a = anchor::anchor_at(s, &place)?;
    let kind = match str_p(p, "kind") {
        Some(k @ ("explanation" | "faq" | "assumption" | "source")) => k.to_string(),
        _ => "explanation".into(),
    };
    let title: String = str_p(p, "title").unwrap_or("").chars().take(200).collect();
    let links = links_p(p);
    let author = actor_p(p, Actor::Human);
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("n");
        l.notes.push(Note { id: id.clone(), anchor: a, kind, title, text, links, author, at: crate::now_ms() });
        id
    });
    Ok(json!({"id": id, "range": place.a1()}))
}

fn input(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.input")?;
    let place = Place { range: RangeRef::cell(place.range.start), ..place };
    if p.get("remove").and_then(Json::as_bool) == Some(true) {
        let wb = s.doc()?.wb.clone();
        layer::update(s.doc()?, |l| {
            let hits: Vec<String> = l.anchors.iter().filter(|a| anchor::resolve(&wb, a).as_ref() == Some(&place)).map(|a| a.id.clone()).collect();
            l.inputs.retain(|i| !hits.contains(&i.anchor));
        });
        return Ok(json!({"cell": place.a1(), "removed": true}));
    }
    let low = p.get("low").and_then(Json::as_f64).ok_or_else(|| bad("agent.input", "missing `low`"))?;
    let high = p.get("high").and_then(Json::as_f64).ok_or_else(|| bad("agent.input", "missing `high`"))?;
    if !low.is_finite() || !high.is_finite() {
        return Err(bad("agent.input", "`low` and `high` must be numbers"));
    }
    let relative = p.get("relative").and_then(Json::as_bool).unwrap_or(false);
    let rationale: String = str_p(p, "rationale").unwrap_or("").chars().take(4_000).collect();
    let a = anchor::anchor_at(s, &place)?;
    layer::update(s.doc()?, |l| {
        l.inputs.retain(|i| i.anchor != a);
        l.inputs.push(Input { anchor: a, low: low.min(high), high: low.max(high), relative, rationale });
    });
    Ok(json!({"cell": place.a1()}))
}

fn confidence(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.confidence")?;
    let a = anchor::anchor_at(s, &place)?;
    if p.get("remove").and_then(Json::as_bool) == Some(true) {
        layer::update(s.doc()?, |l| l.confidence.retain(|c| c.anchor != a));
        return Ok(json!({"range": place.a1(), "removed": true}));
    }
    let level = p
        .get("level")
        .and_then(Json::as_f64)
        .filter(|v| v.is_finite())
        .ok_or_else(|| bad("agent.confidence", "missing `level` (0..1)"))?
        .clamp(0.0, 1.0);
    let reason: String = str_p(p, "reason").unwrap_or("").chars().take(4_000).collect();
    layer::update(s.doc()?, |l| {
        l.confidence.retain(|c| c.anchor != a);
        l.confidence.push(Confidence { anchor: a, level, reason });
    });
    Ok(json!({"range": place.a1(), "level": level}))
}

fn decision(s: &mut Session, p: &Json) -> Result<Json> {
    let text = text_p(p, "text", "agent.decision")?;
    let anchor = if str_p(p, "range").is_some() {
        let place = place_p(s, p, "agent.decision")?;
        Some(anchor::anchor_at(s, &place)?)
    } else {
        None
    };
    let source: String = str_p(p, "source").unwrap_or("").chars().take(2_000).collect();
    let actor = actor_p(p, Actor::Human);
    layer::update(s.doc()?, |l| l.log(actor, "decision", text, "", anchor, source));
    Ok(json!({}))
}

pub fn layer_json(s: &Session) -> Result<Json> {
    let d = s.doc()?;
    let l = layer::get(d);
    let mut v = serde_json::to_value(&*l).unwrap_or(Json::Null);
    if let Some(list) = v.get_mut("anchors").and_then(Json::as_array_mut) {
        for a in list.iter_mut() {
            let id = a.get("id").and_then(Json::as_str).unwrap_or("").to_string();
            a["resolved"] = where_json(s, &l, &id);
        }
    }
    Ok(v)
}

fn layer_cmd(s: &mut Session, _p: &Json) -> Result<Json> {
    layer_json(s)
}

fn context(s: &mut Session, p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let l = layer::get(d);
    let (wb, _) = if masked_p(p) { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    let sheets: Vec<Json> = wb
        .sheets
        .iter()
        .map(|sh| {
            let formulas = sh.cells.iter().filter(|(_, c)| c.formula.is_some()).count();
            json!({"name": sh.name, "used": sh.used_range().map(|r| r.a1()), "formulaCells": formulas})
        })
        .collect();
    let private: Vec<Json> = l.private.iter().map(|id| where_json(s, &l, id)).collect();
    let inputs: Vec<Json> = l
        .inputs
        .iter()
        .map(|i| json!({"cell": where_json(s, &l, &i.anchor), "low": i.low, "high": i.high, "relative": i.relative, "rationale": i.rationale}))
        .collect();
    Ok(json!({
        "title": d.title,
        "path": d.path,
        "activeSheet": wb.sheet(wb.active_sheet).map(|x| x.name.clone()),
        "selection": d.selection.current().a1(),
        "sheets": sheets,
        "privateRanges": private,
        "privacyNote": "Cells in privateRanges hold client data. You see stand-in values there (same sign and magnitude), and everything computed from them is recalculated from the stand-ins: never quote these numbers as real. Formulas are real.",
        "inputs": inputs,
        "safeFile": l.safe.as_ref().map(|x| json!({"on": true, "locked": x.sealed.is_some()})),
        "openQuestions": l.questions.iter().filter(|q| q.status != QuestionStatus::Answered).count(),
        "notes": l.notes.len(),
        "log": l.log.iter().rev().take(20).collect::<Vec<_>>(),
    }))
}

fn read(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.read")?;
    let d = s.doc()?;
    let l = layer::get(d);
    let masked = masked_p(p);
    let (wb, calc) = if masked { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    let private = analysis::private_cells(&wb, &l);
    let derived = analysis::derived_from(&calc, &private);
    let Some(sh) = wb.sheet(place.sheet) else { return Err(bad("agent.read", "no such sheet")) };
    if place.range.count() > 20_000 {
        return Err(bad("agent.read", "range too large (20,000 cells max): read it in parts"));
    }
    let mut cells = vec![];
    for c in place.range.iter() {
        let Some(cell) = sh.cell(c) else { continue };
        if cell.value.is_empty() && cell.formula.is_none() {
            continue;
        }
        let mut o = json!({"cell": c.a1(), "value": analysis::value_json(&cell.value), "text": gridcraft_engine::display::cell_text(&wb, sh, c)});
        if let Some(f) = &cell.formula {
            o["formula"] = json!(format!("={}", f.text.trim_start_matches('=')));
        }
        if private.contains(&(place.sheet, c)) {
            o["private"] = json!(true);
        }
        if derived.contains(&(place.sheet, c)) {
            o["derivedFromPrivate"] = json!(true);
        }
        cells.push(o);
    }
    Ok(json!({"range": place.a1(), "masked": masked, "cells": cells}))
}

fn trace(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.trace")?;
    let depth = p.get("depth").and_then(Json::as_u64).unwrap_or(6).clamp(1, 12) as usize;
    let d = s.doc()?;
    let l = layer::get(d);
    let (wb, calc) = if masked_p(p) { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    let private = analysis::private_cells(&wb, &l);
    let derived = analysis::derived_from(&calc, &private);
    let k: Key = (place.sheet, place.range.start);
    Ok(analysis::trace(&wb, &calc, k, depth, &private, &derived))
}

fn sensitivity(s: &mut Session, p: &Json) -> Result<Json> {
    let place = place_p(s, p, "agent.sensitivity")?;
    let limit = p.get("limit").and_then(Json::as_u64).unwrap_or(8).clamp(1, 32) as usize;
    let d = s.doc()?;
    let l = layer::get(d);
    let (wb, calc) = if masked_p(p) { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    let private = analysis::private_cells(&wb, &l);
    let k: Key = (place.sheet, place.range.start);
    let (base, swings) = analysis::sensitivity(&wb, &calc, &l, k, limit);
    let label = wb.sheet(k.0).map(|x| analysis::label_of(x, k.1)).unwrap_or_default();
    Ok(
        json!({"cell": place.a1(), "label": label, "value": base, "inputs": swings.iter().map(|x| analysis::swing_json(&wb, x, &private)).collect::<Vec<_>>()}),
    )
}

// ---------------------------------------------------------------- acting as the agent

thread_local! {
    /// > 0 while `agent.exec` runs a command: the audit log records it as the agent's.
    pub(crate) static AGENT_DEPTH: StdCell<u32> = const { StdCell::new(0) };
}

/// Params that name cells a command writes to.
fn targets(p: &Json) -> Vec<String> {
    ["cell", "range", "target", "destination", "to"].iter().filter_map(|k| str_p(p, k).map(str::to_string)).collect()
}

fn exec(s: &mut Session, p: &Json) -> Result<Json> {
    let command = text_p(p, "command", "agent.exec")?;
    let params = p.get("params").cloned().unwrap_or_else(|| json!({}));
    let Some(spec) = gridcraft_engine::find_command(&command) else { return Err(EngineError::UnknownCommand(command)) };
    if command.starts_with("agent.") {
        return Err(bad("agent.exec", "call agent.* commands directly"));
    }
    if !spec.journal || command.starts_with("file.save") || command == "file.exportCsv" || command == "file.exportHtml" {
        // Query commands return cell contents; reads go through agent.read so privacy holds.
        return Err(bad("agent.exec", format!("`{command}` reads the workbook: use agent.read / agent.trace (they respect private data)")));
    }
    let d = s.doc()?;
    let l = layer::get(d);
    let private = analysis::private_cells(&d.wb, &l);
    for t in targets(&params) {
        if let Some(place) = anchor::parse_place(&d.wb, &t, d.wb.active_sheet)
            && place.range.iter().take(1_000_000).any(|c| private.contains(&(place.sheet, c)))
        {
            return Err(bad("agent.exec", format!("{} holds client data: the agent can't write there", place.a1())));
        }
    }
    let reason: String = str_p(p, "reason").unwrap_or("").chars().take(4_000).collect();
    let source: String = str_p(p, "source").unwrap_or("").chars().take(2_000).collect();
    PENDING_REASON.with(|r| *r.borrow_mut() = Some((reason, source)));
    AGENT_DEPTH.with(|x| x.set(x.get().saturating_add(1)));
    let r = s.execute(&command, params);
    AGENT_DEPTH.with(|x| x.set(x.get().saturating_sub(1)));
    PENDING_REASON.with(|r| r.borrow_mut().take());
    // Only success is reported back: a command's result can carry cell contents.
    r.map(|_| json!({"ok": true, "command": command}))
}

thread_local! {
    /// > 0 while a layer command makes a change it records itself (the audit log skips it).
    pub(crate) static QUIET: StdCell<u32> = const { StdCell::new(0) };
    pub(crate) static PENDING_REASON: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}
