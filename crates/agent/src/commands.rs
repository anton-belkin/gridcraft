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
use crate::layer::{self, Actor, Answer, Confidence, Input, Layer, Link, Note, Question, QuestionRef, QuestionStatus};

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
                    $crate::commands::agent_allowed(crate::agent_off(), p)?;
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
    crate::docs::SPECS,
    crate::topics::SPECS,
    crate::variants::SPECS,
    crate::stress::SPECS,
    crate::safe::SPECS,
    crate::links::SPECS,
    #[cfg(not(target_arch = "wasm32"))]
    crate::runner::SPECS,
    #[cfg(not(target_arch = "wasm32"))]
    crate::cowork::SPECS,
    #[cfg(not(target_arch = "wasm32"))]
    crate::context::SPECS,
];

pub static SPECS: &[CommandSpec] = &[
    // the reader's side
    spec!(change "agent.ask", "Ask the Agent", "{range?: \"B5\" | \"Sheet1!B2:C4\" (default: selection), text, topic?: id or number (default: the topic already on the selection), newTopic?: bool, refs?: [{n, range}] (what ⟦n⟧ in the text points at), spoken?: bool, transcribing?: bool (spoken, text still coming: not with the agent until agent.spoken)}", ask),
    spec!(change "agent.spoken", "Spoken Question Transcribed", "{id, text} — the text is ready: the question goes to the agent; {id, error} — the transcription failed; {id} — transcribing again", spoken),
    spec!(change "agent.removeQuestion", "Dismiss Spoken Question", "{id} — a spoken question still being transcribed, or whose transcription failed", remove_question),
    spec!(change "agent.seen", "Mark Answer Read", "{id}", seen),
    spec!(change "agent.ping", "Check the Agent", "{} — asks \"is anyone there?\" (a question of kind ping, answered with \"here\"); the Agent tab shows whether and how fast an agent answered", ping),
    spec!(change "agent.private", "Mark as Client Data", "{range?, on?: bool (default true)} — the agent sees stand-in values there", private),
    spec!(change "agent.removeNote", "Remove Agent Note", "{id}", remove_note),
    // the agent's side
    spec!(query "agent.questions", "Agent Questions", "{status?: pending|working|answered|open|all (default all), waiting?: bool (wait_for_questions: an agent listening), poll?: bool (a stream polling; not a tool call)} → questions with their cell; kind \"ping\" is a connection check", questions),
    spec!(change "agent.claim", "Take Question", "{id} — marks it as being worked on", claim),
    spec!(change "agent.answer", "Answer Question", "{id, text, links?: [{label, href, summary?}]}", answer),
    spec!(change "agent.note", "Add Agent Note", "{topic? (id or number) | range, text, title?, kind?: explanation|source|assumption|decision, url?, summary? (a source), links?: [{label, href, summary?}], actor?} → {id, topic}", note),
    spec!(change "agent.input", "Set Input Range", "{cell, low, high, relative?: bool (multipliers of the value), rationale?} — or {cell, remove: true}", input),
    spec!(change "agent.confidence", "Set Confidence", "{range, level: 0..1, reason?} — or {range, remove: true}", confidence),
    spec!(change "agent.decision", "Log Decision", "{text, range?, source?, actor?}", decision),
    spec!(query "agent.layer", "Agent Layer", "{} → anchors (resolved), questions, notes, inputs, private ranges, confidence, log", layer_cmd),
    spec!(query "agent.context", "Agent Context", "{masked?: bool} → sheets with used ranges and formulas, the layer summary", context),
    spec!(query "agent.read", "Agent Read", "{range, masked?: bool (default true)} → cells with value, text, formula, private/derivedFromPrivate", read),
    spec!(query "agent.trace", "Where Does This Number Come From", "{cell?, depth?: 1..12 (default 6), masked?: bool} → the calculation tree down to its inputs", trace),
    spec!(query "agent.sensitivity", "Sensitivity", "{cell?, limit?: (default 8), masked?: bool} → output at the low and high end of each input", sensitivity),
    spec!(query "agent.exportHtml", "Export as Web Page", "{path: \"out.html\", includeDiscussion?: bool (default false: topics with status and notes only), web?: folder with the web build (default $GRIDCRAFT_WEB_BUNDLE, then target/agent-web)} — one HTML file: the workbook in GridCraft for the web, prepared answers and notes read-only", crate::export::export_html),
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

/// The words of the refusal while the agent connection is off.
pub const OFF_MESSAGE: &str = "the agent connection is off in GridCraft (Agent tab ▸ Connection ▸ Mode)";

/// Off mode: every agent-side command (actor agent) is refused; a person's still runs.
pub fn agent_allowed(off: bool, p: &Json) -> Result<()> {
    if off && p.get("actor").and_then(Json::as_str) == Some("agent") {
        return Err(EngineError::Other(OFF_MESSAGE.into()));
    }
    Ok(())
}

/// What the agent hears about a question the reader took back.
pub const WITHDRAWN: &str = "this question was withdrawn by the user; drop it";

/// A question's text as stored: what was pointed at (⟦n⟧) and cell references become links.
pub(crate) fn stored_question(s: &mut Session, text: &str, refs: &[QuestionRef]) -> Result<String> {
    let mut t = text.to_string();
    for r in refs {
        t = t.replace(&format!("⟦{}⟧", r.n), &format!("[@](anchor:{})", r.anchor));
    }
    crate::textlinks::store(s, &t)
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
    let transcribing = p.get("transcribing").and_then(Json::as_bool).unwrap_or(false);
    let text = if transcribing { str_p(p, "text").unwrap_or("").chars().take(20_000).collect() } else { text_p(p, "text", "agent.ask")? };
    let spoken = transcribing || p.get("spoken").and_then(Json::as_bool).unwrap_or(false);
    let place = place_p(s, p, "agent.ask")?;
    let a = anchor::anchor_at(s, &place)?;
    let mut refs: Vec<QuestionRef> = vec![];
    for r in p.get("refs").and_then(Json::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(50) {
        let n = r.get("n").and_then(Json::as_u64).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| bad("agent.ask", "each ref needs `n`"))?;
        let at = str_p(r, "range").or_else(|| str_p(r, "cell")).ok_or_else(|| bad("agent.ask", "each ref needs `range`"))?;
        let d = s.doc()?;
        let rp = anchor::parse_place(&d.wb, at, d.wb.active_sheet).ok_or_else(|| bad("agent.ask", format!("not a range: `{at}`")))?;
        if refs.iter().any(|x| x.n == n) {
            continue;
        }
        refs.push(QuestionRef { n, anchor: anchor::anchor_at(s, &rp)? });
    }
    // Its topic: the one named, else one already on the selection (a follow-up), else a new one.
    let new_topic = p.get("newTopic").and_then(Json::as_bool).unwrap_or(false);
    let named = str_p(p, "topic").map(str::to_string).or_else(|| p.get("topic").and_then(Json::as_u64).map(|n| n.to_string()));
    let found = {
        let d = s.doc()?;
        let l = layer::get(d);
        match named {
            Some(r) => Some(crate::topics::find_topic(&d.wb, &l, &r).ok_or_else(|| bad("agent.ask", format!("no topic `{r}`")))?),
            None if new_topic => None,
            None => crate::topics::topics_at(&d.wb, &l, &place).into_iter().next(),
        }
    };
    let by = actor_p(p, Actor::Human);
    // The person asked: the document's background agent may start for it.
    #[cfg(not(target_arch = "wasm32"))]
    if by == Actor::Human {
        crate::runner::arm(s.doc()?.uid);
    }
    let (topic, created) = match found {
        Some(t) => (t, false),
        None => (crate::topics::new_topic(s, &place, layer::Status::Open, by)?, true),
    };
    // Pointing (⟦n⟧) and cell references become links; what was pointed at becomes places.
    let text = if transcribing { text } else { stored_question(s, &text, &refs)? };
    let topic_before = if created { None } else { layer::get(s.doc()?).topic(&topic).cloned() };
    let now = crate::now_ms();
    let question = Question {
        id: String::new(),
        anchor: a.clone(),
        text,
        status: if transcribing { QuestionStatus::Transcribing } else { QuestionStatus::Pending },
        asked_at: now,
        answer: None,
        seen: false,
        refs: refs.clone(),
        spoken,
        error: String::new(),
        kind: String::new(),
        answered_by: String::new(),
        topic: topic.clone(),
        hidden: false,
    };
    let (id, q, after) = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("q");
        let mut q = question;
        q.id = id.clone();
        if let Some(t) = l.topics.iter_mut().find(|t| t.id == topic) {
            if !t.places.iter().any(|x| x.anchor == a) {
                t.places.push(layer::TopicPlace { anchor: a.clone(), reason: String::new(), by, at: now });
            }
            for r in &refs {
                if !t.places.iter().any(|x| x.anchor == r.anchor) {
                    t.places.push(layer::TopicPlace { anchor: r.anchor.clone(), reason: "pointed at while asking".into(), by, at: now });
                }
            }
        }
        l.questions.push(q.clone());
        (id, q, l.topic(&topic).cloned())
    });
    if crate::topics::by_person(p)
        && let Some(after) = after
    {
        crate::topics::record_ask(s, "Ask the Agent", crate::topics::AskStep { question: q, topic_before, topic_after: after });
    }
    Ok(json!({"id": id, "range": place.a1(), "topic": topic}))
}

/// A spoken question's transcription: done (the question goes to the agent), failed, or again.
fn spoken(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.spoken")?;
    let text: Option<String> = str_p(p, "text").map(str::trim).filter(|t| !t.is_empty()).map(|t| t.chars().take(20_000).collect());
    let refs = layer::get(s.doc()?).questions.iter().find(|q| q.id == id).map(|q| q.refs.clone()).unwrap_or_default();
    let text = match text {
        Some(t) => Some(stored_question(s, &t, &refs)?),
        None => None,
    };
    let error: Option<String> = str_p(p, "error").map(|e| e.chars().take(2_000).collect());
    let r = layer::update(s.doc()?, |l| {
        let Some(q) = l.questions.iter_mut().find(|q| q.id == id) else { return Err(format!("no question `{id}`")) };
        if !q.status.is_draft() {
            return Err(format!("question `{id}` is already with the agent"));
        }
        match (text, error) {
            (Some(t), _) => {
                q.text = t;
                q.error.clear();
                q.status = QuestionStatus::Pending;
                q.asked_at = crate::now_ms();
            }
            (None, Some(e)) => {
                q.error = if e.trim().is_empty() { "The transcription failed".into() } else { e };
                q.status = QuestionStatus::Failed;
            }
            (None, None) => {
                q.error.clear();
                q.status = QuestionStatus::Transcribing;
            }
        }
        Ok(format!("{:?}", q.status).to_lowercase())
    });
    r.map(|status| json!({"id": id, "status": status})).map_err(|m| bad("agent.spoken", m))
}

fn remove_question(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeQuestion")?;
    let r = layer::update(s.doc()?, |l| match l.questions.iter().position(|q| q.id == id) {
        Some(i) if l.questions.get(i).is_some_and(|q| q.status.is_draft()) => {
            let q = l.questions.remove(i);
            // A topic only this draft made goes with it.
            if !l.questions.iter().any(|x| x.topic == q.topic) && !l.notes.iter().any(|n| n.topic == q.topic) {
                l.topics.retain(|t| t.id != q.topic);
            }
            Ok(())
        }
        Some(_) => Err(format!("question `{id}` is with the agent already")),
        None => Err(format!("no question `{id}`")),
    });
    r.map(|()| json!({"id": id})).map_err(|m| bad("agent.removeQuestion", m))
}

/// The ping's text, as the agent reads it.
pub const PING_TEXT: &str = "Is anyone there? (connection check; answer \"here\")";

fn ping(s: &mut Session, _p: &Json) -> Result<Json> {
    let id = layer::update(s.doc()?, |l| {
        // One check at a time.
        l.questions.retain(|q| !q.is_ping());
        let id = format!("ping{}", crate::now_ms());
        l.questions.push(Question {
            id: id.clone(),
            anchor: String::new(),
            text: PING_TEXT.into(),
            status: QuestionStatus::Pending,
            asked_at: crate::now_ms(),
            answer: None,
            seen: true,
            kind: layer::PING.into(),
            ..Question::default()
        });
        id
    });
    Ok(json!({"id": id}))
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
    let before = crate::topics::Part::Private(layer::get(s.doc()?).private.clone());
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
    if crate::topics::by_person(p) {
        let after = crate::topics::Part::Private(layer::get(s.doc()?).private.clone());
        crate::topics::record(s, if on { "Mark as Client Data" } else { "Unmark Client Data" }, vec![before], vec![after]);
    }
    Ok(json!({"range": place.a1(), "private": on}))
}

fn remove_note(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeNote")?;
    let Some(before) = layer::get(s.doc()?).notes.iter().find(|x| x.id == id).cloned() else {
        return Err(bad("agent.removeNote", format!("no note `{id}`")));
    };
    layer::update(s.doc()?, |l| l.notes.retain(|x| x.id != id));
    if crate::topics::by_person(p) {
        crate::topics::record(
            s,
            "Remove Note",
            vec![crate::topics::Part::Note(id.clone(), Some(before))],
            vec![crate::topics::Part::Note(id.clone(), None)],
        );
    }
    Ok(json!({"id": id}))
}

// ---------------------------------------------------------------- agent's side

/// A question with what the reader is looking at, so the agent can start without another call.
/// `wb` is the workbook as the caller may see it (masked for the agent).
fn question_json(s: &Session, l: &Layer, q: &Question, wb: &Workbook) -> Json {
    let mut o = serde_json::to_value(q).unwrap_or(Json::Null);
    o["range"] = where_json(s, l, &q.anchor);
    if let Ok(d) = s.doc() {
        o["document"] = json!(crate::docs::doc_id(d.uid));
        o["documentTitle"] = json!(d.display_title());
    }
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
    // Links in the text, with current addresses.
    let home = wb.active_sheet;
    o["text"] = json!(crate::textlinks::render(wb, l, &q.text, home));
    if let Some(a) = &q.answer {
        o["answer"]["text"] = json!(crate::textlinks::render(wb, l, &a.text, home));
    }
    if !q.refs.is_empty() {
        // What the reader pointed at while speaking (links in the text, and listed).
        o["refs"] = json!(q.refs.iter().map(|r| ref_json(l, r, wb)).collect::<Vec<_>>());
    }
    // Its topic, so a follow-up is answered in context.
    if let Some(t) = l.topic(&q.topic) {
        let n = crate::topics::numbering(wb, l);
        o["topic"] = json!({"id": t.id, "number": n.topic.get(&t.id), "status": t.status.status.id(), "statusText": crate::textlinks::render(wb, l, &t.status.text, home)});
    }
    o
}

/// A cell pointed at while speaking, as the caller may see it (masked for the agent).
fn ref_json(l: &Layer, r: &QuestionRef, wb: &Workbook) -> Json {
    let Some(place) = l.anchor(&r.anchor).and_then(|a| anchor::resolve(wb, a)) else {
        return json!({"n": r.n, "range": Json::Null, "deleted": true});
    };
    let mut o = json!({"n": r.n, "range": place.a1()});
    if let Some(sh) = wb.sheet(place.sheet) {
        let c = place.range.start;
        o["label"] = json!(analysis::label_of(sh, c));
        if place.range.is_single() {
            o["cellText"] = json!(gridcraft_engine::display::cell_text(wb, sh, c));
            if let Some(f) = sh.cell(c).and_then(|x| x.formula.as_ref()) {
                o["formula"] = json!(format!("={}", f.text.trim_start_matches('=')));
            }
        }
    }
    o
}

static LAST_WAIT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// When an agent last waited for questions (`wait_for_questions`, `/events`): ms since the epoch.
pub fn last_wait_ms() -> u64 {
    LAST_WAIT.load(std::sync::atomic::Ordering::Relaxed)
}

fn questions(s: &mut Session, p: &Json) -> Result<Json> {
    crate::docs::touch_from(s, p);
    if p.get("waiting").and_then(Json::as_bool) == Some(true) && actor_p(p, Actor::Human) == Actor::Agent {
        LAST_WAIT.store(crate::now_ms(), std::sync::atomic::Ordering::Relaxed);
    }
    let l = layer::get(s.doc()?);
    let want = str_p(p, "status").unwrap_or("all");
    let d = s.doc()?;
    let now = crate::now_ms();
    // A check nobody answered in a minute is over: no agent should wake up for it.
    let shown = |q: &&Question| {
        status_matches(q, want) && !q.hidden && !(q.is_ping() && q.status != QuestionStatus::Answered && now.saturating_sub(q.asked_at) >= 60_000)
    };
    let has_any = l.questions.iter().any(|q| shown(&q));
    // One masked copy for all of them (and none when there is nothing to show).
    let wb = if !has_any {
        Workbook::new()
    } else if masked_p(p) {
        analysis::masked(d, &l).0
    } else {
        (*d.wb).clone()
    };
    let list: Vec<Json> = l.questions.iter().filter(shown).map(|q| question_json(s, &l, q, &wb)).collect();
    Ok(Json::Array(list))
}

fn status_matches(q: &Question, want: &str) -> bool {
    // A spoken question still being transcribed has no text yet: not the agent's.
    if q.status.is_draft() {
        return want == "drafts";
    }
    match want {
        "pending" => q.status == QuestionStatus::Pending,
        "working" => q.status == QuestionStatus::Working,
        "answered" => q.status == QuestionStatus::Answered,
        "withdrawn" => q.status == QuestionStatus::Withdrawn,
        "open" => !matches!(q.status, QuestionStatus::Answered | QuestionStatus::Withdrawn),
        _ => true,
    }
}

fn claim(s: &mut Session, p: &Json) -> Result<Json> {
    crate::docs::touch_from(s, p);
    let id = text_p(p, "id", "agent.claim")?;
    let r = layer::update(s.doc()?, |l| match l.questions.iter_mut().find(|q| q.id == id) {
        Some(q) if q.status == QuestionStatus::Pending => {
            q.status = QuestionStatus::Working;
            Ok(())
        }
        Some(q) if q.status == QuestionStatus::Withdrawn => Err(WITHDRAWN.to_string()),
        Some(q) => Err(format!("question `{id}` is {:?}", q.status).to_lowercase()),
        None => Err(format!("no question `{id}`")),
    });
    r.map(|()| json!({"id": id})).map_err(|m| bad("agent.claim", m))
}

fn answer(s: &mut Session, p: &Json) -> Result<Json> {
    crate::docs::touch_from(s, p);
    let by = match (str_p(p, "listener"), s.doc()) {
        (Some(l), Ok(d)) => crate::docs::label_of(d.uid, l).unwrap_or_default(),
        _ => String::new(),
    };
    let id = text_p(p, "id", "agent.answer")?;
    let mut text = text_p(p, "text", "agent.answer")?;
    if layer::get(s.doc()?).questions.iter().any(|q| q.id == id && q.status == QuestionStatus::Withdrawn) {
        return Err(bad("agent.answer", WITHDRAWN));
    }
    // Links go inside the text; a `links` list becomes a sentence of links.
    let links = links_p(p);
    if !links.is_empty() {
        let list: Vec<String> = links.iter().map(|k| format!("[{}]({})", k.label.replace(']', ")"), k.href)).collect();
        text.push_str(&format!("\n\nSee {}.", list.join(", ")));
    }
    let text = crate::textlinks::store(s, &text)?;
    let links = links.into_iter().filter(|k| !k.summary.is_empty()).collect::<Vec<_>>();
    let ok = layer::update(s.doc()?, |l| {
        let q = l.questions.iter_mut().find(|q| q.id == id)?;
        q.status = QuestionStatus::Answered;
        q.seen = false;
        let at = crate::now_ms();
        q.answer = Some(Answer { text, links, at });
        if q.is_ping() {
            // A connection check: answered in so long (and by whom), nothing for the history.
            q.seen = true;
            q.answered_by = by;
            return Some(Some(at.saturating_sub(q.asked_at)));
        }
        let (anchor, qtext) = (q.anchor.clone(), q.text.clone());
        l.log(Actor::Agent, "decision", format!("Answered “{qtext}”"), "agent.answer", Some(anchor), id.clone());
        Some(None)
    });
    match ok {
        Some(Some(ms)) => Ok(json!({"id": id, "ping": true, "answeredInMs": ms})),
        Some(None) => Ok(json!({"id": id})),
        None => Err(bad("agent.answer", format!("no question `{id}`"))),
    }
}

fn note(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.note";
    let text = text_p(p, "text", CMD)?;
    let author = actor_p(p, Actor::Human);
    // Its topic: the one named, else a new one (For information) on the range.
    let named = str_p(p, "topic").map(str::to_string).or_else(|| p.get("topic").and_then(Json::as_u64).map(|n| n.to_string()));
    let topic = match named {
        Some(r) => {
            let d = s.doc()?;
            crate::topics::find_topic(&d.wb, &layer::get(d), &r).ok_or_else(|| bad(CMD, format!("no topic `{r}`")))?
        }
        None => {
            let place = place_p(s, p, CMD)?;
            crate::topics::new_topic(s, &place, layer::Status::Info, author)?
        }
    };
    let a = layer::get(s.doc()?).topic(&topic).and_then(|t| t.places.first().map(|x| x.anchor.clone())).unwrap_or_default();
    let kind = match str_p(p, "kind") {
        Some(k @ ("explanation" | "faq" | "assumption" | "source" | "decision")) => k.to_string(),
        _ => "explanation".into(),
    };
    let title: String = str_p(p, "title").unwrap_or("").chars().take(200).collect();
    let mut links = links_p(p);
    if let Some(url) = str_p(p, "url").filter(|u| u.starts_with("http")) {
        let summary: String = str_p(p, "summary").unwrap_or("").chars().take(2_000).collect();
        links.insert(0, Link { label: if title.is_empty() { url.to_string() } else { title.clone() }, href: url.to_string(), summary });
    }
    let text = crate::textlinks::store(s, &text)?;
    let before = layer::get(s.doc()?).topic(&topic).cloned();
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("n");
        l.notes.push(Note { id: id.clone(), anchor: a, kind, title, text, links, author, at: crate::now_ms(), topic: topic.clone() });
        id
    });
    if crate::topics::by_person(p) {
        let note = layer::get(s.doc()?).notes.iter().find(|x| x.id == id).cloned();
        let after_topic = layer::get(s.doc()?).topic(&topic).cloned();
        crate::topics::record(
            s,
            "Add Note",
            vec![crate::topics::Part::Note(id.clone(), None), crate::topics::Part::Topic(topic.clone(), before)],
            vec![crate::topics::Part::Topic(topic.clone(), after_topic), crate::topics::Part::Note(id.clone(), note)],
        );
    }
    Ok(json!({"id": id, "topic": topic}))
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
    let before = crate::topics::Part::Inputs(layer::get(s.doc()?).inputs.clone());
    layer::update(s.doc()?, |l| {
        l.inputs.retain(|i| i.anchor != a);
        l.inputs.push(Input { anchor: a, low: low.min(high), high: low.max(high), relative, rationale });
    });
    if crate::topics::by_person(p) {
        let after = crate::topics::Part::Inputs(layer::get(s.doc()?).inputs.clone());
        crate::topics::record(s, "Set Input Range", vec![before], vec![after]);
    }
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
        "openQuestions": l.questions.iter().filter(|q| matches!(q.status, QuestionStatus::Pending | QuestionStatus::Working) && !q.is_ping()).count(),
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
