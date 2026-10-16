//! Every open document by id, and who listens to each (AItoCraft protocol v1, §2).
//!
//! - A document's id is `gridcraft:<pid>:<doc uid>`: unique across apps, stable while it is open.
//! - `agent.onDocument` runs a command on a given document, active or not: it makes that document
//!   active for the length of the command and puts the active one back after. Commands from the
//!   control channel run on the UI thread between frames, so the switch is never drawn.
//! - A listener is a conversation bound to a document (`attach`, with a label the user sees:
//!   "Cowork · Acme"). A call that carries it refreshes it; one not seen for 90 s is dropped.
//!   Listeners live in the running app only, never in the file.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::{DocState, Result, Session};
use serde_json::{Value as Json, json};

use crate::commands::{bad, spec, str_p};
use crate::layer::{self, QuestionStatus};

/// A listener not seen for this long is dropped.
pub const LISTENER_TTL_MS: u64 = 90_000;

pub static SPECS: &[CommandSpec] = &[
    spec!(query "agent.onDocument", "Run on Document", "{document: id, command, params?} — runs a command on that open document, active or not", on_document),
    spec!(query "agent.listDocuments", "Open Documents", "{} → the open documents: id, title, path, frontmost, openQuestions, listeners", list_documents),
    spec!(query "agent.attach", "Attach Listener", "{label?} → {listener, document, title, app}: binds a conversation to the active document (with agent.onDocument: to any)", attach),
    spec!(query "agent.detach", "Detach Listener", "{listener}", detach),
];

/// This process's id (0 where there is none, in the browser).
fn pid() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::process::id()
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

/// A document's id.
pub fn doc_id(uid: u64) -> String {
    format!("gridcraft:{}:{uid}", pid())
}

/// The uid of a document id of this app instance.
pub fn uid_of(id: &str) -> Option<u64> {
    let mut parts = id.trim().split(':');
    let (Some("gridcraft"), Some(p), Some(u), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return None };
    if p.parse::<u32>().ok()? != pid() {
        return None;
    }
    u.parse().ok()
}

/// The index of an open document, or the error the protocol asks for.
pub fn index_of(s: &Session, id: &str) -> std::result::Result<usize, String> {
    uid_of(id).and_then(|uid| s.documents().iter().position(|d| d.uid == uid)).ok_or_else(|| format!("document {id} is not open in GridCraft"))
}

// ---------------------------------------------------------------- listeners

#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub id: String,
    pub label: String,
    pub last_seen: u64,
}

static LISTENERS: Mutex<Option<HashMap<u64, Vec<Listener>>>> = Mutex::new(None);

fn store() -> MutexGuard<'static, Option<HashMap<u64, Vec<Listener>>>> {
    match LISTENERS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// Live listeners of a document (stale ones dropped), most recently seen first.
pub fn listeners(uid: u64) -> Vec<Listener> {
    let now = crate::now_ms();
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    let list = map.entry(uid).or_default();
    list.retain(|l| now.saturating_sub(l.last_seen) < LISTENER_TTL_MS);
    let mut out = list.clone();
    out.sort_by_key(|l| std::cmp::Reverse(l.last_seen));
    out
}

/// A call from a listener: it is still there.
pub fn touch(uid: u64, listener: &str) {
    let now = crate::now_ms();
    if let Some(l) = store().get_or_insert_with(HashMap::new).get_mut(&uid).and_then(|v| v.iter_mut().find(|l| l.id == listener)) {
        l.last_seen = now;
    }
}

/// Ages a listener (tests: the 90 s rule without waiting).
#[doc(hidden)]
pub fn set_last_seen(uid: u64, listener: &str, ms: u64) {
    if let Some(l) = store().get_or_insert_with(HashMap::new).get_mut(&uid).and_then(|v| v.iter_mut().find(|l| l.id == listener)) {
        l.last_seen = ms;
    }
}

/// The label of a listener of a document.
pub fn label_of(uid: u64, listener: &str) -> Option<String> {
    store().as_ref()?.get(&uid)?.iter().find(|l| l.id == listener).map(|l| l.label.clone())
}

fn new_listener_id() -> String {
    let n = crate::now_ms() ^ (u64::from(pid()) << 32);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let k = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("L{:x}", (n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k) & 0xFF_FFFF_FFFF)
}

/// Touches the listener a command carries, on the active document.
pub fn touch_from(s: &Session, p: &Json) {
    if let (Some(l), Ok(d)) = (str_p(p, "listener"), s.doc()) {
        touch(d.uid, l);
    }
}

// ---------------------------------------------------------------- focus

/// When each document last had focus (its window focused while it was the active one), and what
/// the last frame saw: (active document, window focused).
static FOCUS: Mutex<(Option<HashMap<u64, u64>>, Option<(u64, bool)>)> = Mutex::new((None, None));

/// Called every frame: when the window gains focus, or the active document changes while it has
/// focus, that document's `focusedAt` is now.
pub fn note_focus(s: &Session, window_focused: bool, now: u64) {
    let Some(uid) = s.active().map(|d| d.uid) else { return };
    let mut g = match FOCUS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let changed = g.1 != Some((uid, window_focused));
    g.1 = Some((uid, window_focused));
    if window_focused && changed {
        g.0.get_or_insert_with(HashMap::new).insert(uid, now);
    }
}

/// When a document last had focus (ms since the epoch).
pub fn focused_at(uid: u64) -> Option<u64> {
    let g = match FOCUS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    g.0.as_ref().and_then(|m| m.get(&uid).copied())
}

// ---------------------------------------------------------------- documents

/// A document as the registry and `list_documents` show it: no content, titles and paths only.
pub fn summary(d: &DocState, frontmost: bool) -> Json {
    let l = layer::get(d);
    let open = l.questions.iter().filter(|q| q.status != QuestionStatus::Answered && !q.is_ping()).count();
    let path = d.path.as_deref().map(|p| std::path::absolute(p).map(|x| x.display().to_string()).unwrap_or_else(|_| p.to_string()));
    json!({
        "id": doc_id(d.uid),
        "title": d.display_title(),
        "path": path,
        "frontmost": frontmost,
        "focusedAt": focused_at(d.uid),
        "openQuestions": open,
        "listeners": listeners(d.uid).iter().map(|x| json!({"listener": x.id, "label": x.label, "lastSeen": x.last_seen})).collect::<Vec<_>>(),
    })
}

/// Every open document.
pub fn documents(s: &Session) -> Vec<Json> {
    let active = s.active_index();
    s.documents().iter().enumerate().map(|(i, d)| summary(d, i == active)).collect()
}

fn list_documents(s: &mut Session, _p: &Json) -> Result<Json> {
    Ok(Json::Array(documents(s)))
}

fn on_document(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.onDocument";
    let id = str_p(p, "document").ok_or_else(|| bad(CMD, "missing `document`"))?;
    let command = str_p(p, "command").ok_or_else(|| bad(CMD, "missing `command`"))?.to_string();
    if command == CMD {
        return Err(bad(CMD, "nested"));
    }
    let i = index_of(s, id).map_err(|m| bad(CMD, m))?;
    let params = p.get("params").cloned().unwrap_or_else(|| json!({}));
    let active = s.active_index();
    s.set_active(i);
    let r = s.execute(&command, params);
    // Put the user's document back (unless the command closed or opened documents).
    if active < s.documents().len() && s.active_index() == i {
        s.set_active(active);
    }
    r
}

fn attach(s: &mut Session, p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let label: String = str_p(p, "label").map(str::trim).filter(|l| !l.is_empty()).unwrap_or("Agent").chars().take(80).collect();
    let listener = Listener { id: new_listener_id(), label, last_seen: crate::now_ms() };
    let out = json!({"listener": listener.id, "document": doc_id(d.uid), "title": d.display_title(), "app": "GridCraft"});
    store().get_or_insert_with(HashMap::new).entry(d.uid).or_default().push(listener);
    Ok(out)
}

fn detach(s: &mut Session, p: &Json) -> Result<Json> {
    let id = str_p(p, "listener").ok_or_else(|| bad("agent.detach", "missing `listener`"))?.to_string();
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    let mut found = false;
    for list in map.values_mut() {
        let n = list.len();
        list.retain(|l| l.id != id);
        found |= n != list.len();
    }
    let _ = s;
    if found { Ok(json!({"listener": id, "detached": true})) } else { Err(bad("agent.detach", format!("no listener `{id}`"))) }
}
