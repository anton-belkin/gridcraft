//! The agent layer of one workbook: what the agent and the reader added on top of the cells.
//!
//! It lives in a per-document store while the workbook is open (so undo of cell edits never takes
//! a question or an answer away) and is written into the workbook as a custom XML part just before
//! every save. Custom XML parts are kept by GridCraft and by Excel, so the layer travels with the
//! `.xlsx` file. On open it is read back from that part.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use gridcraft_engine::DocState;
use gridcraft_engine::model::{Preserved, PreservedPart, PreservedRel, Workbook};
use serde::{Deserialize, Serialize};

/// Namespace of the custom XML part (and how we recognise it, whatever its part name).
pub const NS: &str = "urn:gridcraft:agent-layer:v1";
const REL_CUSTOM_XML: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml";
const PART_NAME: &str = "customXml/gridcraftAgent.xml";
const REL_ID: &str = "rIdGcAgentLayer";
/// Largest layer read back from a file.
const MAX_PART: usize = 32 * 1024 * 1024;

/// Who did something.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    #[default]
    Human,
    Agent,
}

/// A range the layer points at. `name` is a hidden defined name in the workbook, so the engine
/// moves it with inserted and deleted rows and columns (and Excel does too); `sheet`/`range` are
/// where it was last seen, used when the name is gone.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub id: String,
    pub name: String,
    pub sheet: String,
    pub range: String,
    /// The anchored cells were deleted.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lost: bool,
}

/// A link in a note or an answer: a web page (`https://…`), a place in the workbook
/// (`cell:Sheet1!B5`) or another note (`note:n3`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub label: String,
    pub href: String,
    /// A short summary of what is behind the link (for sources).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionStatus {
    #[default]
    Pending,
    /// An agent has taken it.
    Working,
    Answered,
    /// Spoken, being transcribed: not with the agent yet (kept in memory only, never saved).
    Transcribing,
    /// The transcription failed: the reader can retry or dismiss it (memory only).
    Failed,
    /// The reader took it back (undo) while the agent was on it: the agent drops it.
    Withdrawn,
}

impl QuestionStatus {
    /// Not a question for the agent yet: a spoken one still turning into text.
    pub fn is_draft(self) -> bool {
        matches!(self, QuestionStatus::Transcribing | QuestionStatus::Failed)
    }
}

/// What the reader pointed at while speaking: marker `⟦n⟧` in the question's text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuestionRef {
    pub n: u32,
    pub anchor: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub at: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub anchor: String,
    pub text: String,
    #[serde(default)]
    pub status: QuestionStatus,
    #[serde(default)]
    pub asked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    /// The reader has opened the answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    /// Cells pointed at while the question was spoken (`anchor` stays the main one).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<QuestionRef>,
    /// Asked by voice.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub spoken: bool,
    /// Why the transcription failed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Empty for a reader's question; [`PING`] for a connection check (never shown as a question,
    /// never saved).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Who answered (a listener's label), for a connection check.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub answered_by: String,
    /// The topic it belongs to (its discussion).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// Undone after it was answered: hidden, kept for redo.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// The kind of a connection check ("is anyone there?").
pub const PING: &str = "ping";

impl Question {
    pub fn is_ping(&self) -> bool {
        self.kind == PING
    }
}

/// Where the last connection check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ping {
    /// Asked at, no answer yet.
    Waiting {
        asked_at: u64,
    },
    Answered {
        asked_at: u64,
        answered_at: u64,
    },
}

/// An explanation the agent left on a range: why it is done this way, a prepared answer to a
/// likely question, an assumption or a source.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub anchor: String,
    /// `explanation`, `faq`, `assumption` or `source`.
    #[serde(default = "explanation")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    #[serde(default)]
    pub author: Actor,
    #[serde(default)]
    pub at: u64,
    /// The topic it belongs to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub topic: String,
}

fn explanation() -> String {
    "explanation".into()
}

/// An input of the model with the range it can reasonably take (for the sensitivity panel).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub anchor: String,
    /// Lowest and highest plausible value. With `relative`, multipliers of the current value
    /// (0.9 = −10%), which keeps a private input's real value out of the layer.
    pub low: f64,
    pub high: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relative: bool,
    /// Why this range (the nature of the parameter, a source…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rationale: String,
}

/// How sure the agent is about a range (0 = guess, 1 = verified against a primary source).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Confidence {
    pub anchor: String,
    pub level: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// One entry of the decision history / audit trail.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub seq: u64,
    pub at: u64,
    pub actor: Actor,
    /// `decision` (why the document is the way it is) or `change` (a command that changed it).
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// What the change is based on (a source, a question id…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

/// What a set of alternatives is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VariantKind {
    /// "A or B?": alternative assumptions or formulas for a range, both kept in the document.
    #[default]
    Alternatives,
    /// A readable formula and an exact one for the same cell (options 0 and 1).
    Method,
}

/// One alternative of a [`VariantSet`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VariantOption {
    pub name: String,
    /// The range's contents in this option, row by row, as typed: numbers, booleans, text or
    /// `=formulas` (the canonical spelling).
    pub values: Vec<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Alternatives for a range that live in the document: the reader switches between them and
/// sees the model recalculate.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VariantSet {
    pub id: String,
    pub anchor: String,
    #[serde(default)]
    pub kind: VariantKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    pub options: Vec<VariantOption>,
    /// Cells to compare the options on (anchors); empty: the model's final outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    /// For a method: the relative difference on an output above which the choice matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub author: Actor,
    #[serde(default)]
    pub at: u64,
}

/// Real values of client data, encrypted (AES-256-CTR, HMAC-SHA256) under a key kept outside
/// the workbook.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sealed {
    pub alg: String,
    pub nonce: String,
    pub data: String,
    pub mac: String,
    /// How many cells are sealed.
    pub cells: usize,
}

/// Safe file mode: saves write stand-ins for client data and seal the real values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SafeFile {
    /// The key's id in the local key store (never the key).
    pub key_id: String,
    /// Present in the file; in memory only while the key isn't available here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<Sealed>,
}

/// One place a linked range of this workbook is used (a slide of a deck), as DeckCraft reports it
/// (`agent-layer` links contract, §5).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkUse {
    pub link: String,
    pub at: u64,
    pub app: String,
    pub workbook: String,
    pub workbook_id: String,
    /// The `_GcLink_` name of the range.
    pub anchor: String,
    pub sheet: String,
    pub range: String,
    pub deck: String,
    pub deck_title: String,
    pub slide: u32,
    pub slide_title: String,
    pub shape: u64,
    pub shape_key: String,
    pub mode: String,
}

/// Where a topic stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A question or doubt, no decision yet.
    #[default]
    Open,
    /// Something here must change.
    Revise,
    /// Settled and accepted (only a person sets it).
    Agreed,
    /// Nothing to decide: an explanation, a source, a caveat.
    Info,
}

impl Status {
    pub fn parse(s: &str) -> Option<Status> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Status::Open),
            "revise" => Some(Status::Revise),
            "agreed" => Some(Status::Agreed),
            "info" | "for information" | "information" => Some(Status::Info),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Revise => "revise",
            Status::Agreed => "agreed",
            Status::Info => "info",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Status::Open => "Open",
            Status::Revise => "Revise",
            Status::Agreed => "Agreed",
            Status::Info => "For information",
        }
    }
}

/// A topic's status, with its free text and who set it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicStatus {
    #[serde(default)]
    pub status: Status,
    /// "agreed with the CFO on 14.10, if capex is confirmed" (links stored as anchors).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default)]
    pub by: Actor,
    #[serde(default)]
    pub at: u64,
}

/// The agent's proposal to mark a topic Agreed, which a person confirms.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    pub reasons: String,
    #[serde(default)]
    pub at: u64,
}

/// One place of a topic: an anchor, and why it is there when the agent added it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TopicPlace {
    pub anchor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default)]
    pub by: Actor,
    #[serde(default)]
    pub at: u64,
}

/// One question or theme about the workbook: its places, status, notes and discussion (the
/// questions and notes that name it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Topic {
    pub id: String,
    #[serde(default)]
    pub places: Vec<TopicPlace>,
    #[serde(default)]
    pub status: TopicStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<Proposal>,
    /// Taken back by an undo (its question was answered): kept for redo.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    #[serde(default)]
    pub created_at: u64,
}

/// Where deleted cells were: the anchors of their neighbours (before and after the deleted rows or
/// columns) and the address they last had.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Whereabouts {
    pub anchor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    pub was: String,
}

/// The layer format this build writes.
pub const VERSION: u32 = 2;

/// Most log entries kept (oldest dropped first).
pub const MAX_LOG: usize = 5_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub questions: Vec<Question>,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub inputs: Vec<Input>,
    /// Anchors of ranges holding client data: the agent sees stand-in values there.
    #[serde(default)]
    pub private: Vec<String>,
    #[serde(default)]
    pub confidence: Vec<Confidence>,
    #[serde(default)]
    pub log: Vec<LogEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<VariantSet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe: Option<SafeFile>,
    /// The workbook's own id, made the first time it is a linked source (survives Save As and moves).
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "workbookId")]
    pub workbook_id: String,
    /// Ranges copied as linked sources: `_GcLink_` names (never cleaned up: other files use them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Anchor>,
    /// The background agent's session (Claude Code session id), resumed when the workbook reopens.
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "agentSession")]
    pub agent_session: String,
    /// Where those ranges are used (decks and slides).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<LinkUse>,
    /// Topics (format v2): places, status, notes and discussion of each question or theme.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub topics: Vec<Topic>,
    /// Where deleted places and links were.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub whereabouts: Vec<Whereabouts>,
    /// Bumped on every change (not saved meaningfully; lets views cache).
    #[serde(skip)]
    pub revision: u64,
}

impl Layer {
    pub fn fresh_id(&mut self, prefix: &str) -> String {
        self.next_id = self.next_id.saturating_add(1);
        format!("{prefix}{}", self.next_id)
    }

    pub fn anchor(&self, id: &str) -> Option<&Anchor> {
        self.anchors.iter().find(|a| a.id == id)
    }

    pub fn log(&mut self, actor: Actor, kind: &str, text: String, command: &str, anchor: Option<String>, source: String) {
        let seq = self.log.last().map_or(1, |e| e.seq.saturating_add(1));
        self.log.push(LogEntry { seq, at: crate::now_ms(), actor, kind: kind.into(), text, command: command.into(), anchor, source });
        if self.log.len() > MAX_LOG {
            let extra = self.log.len() - MAX_LOG;
            self.log.drain(..extra);
        }
    }

    /// The last connection check, if one was made.
    pub fn ping(&self) -> Option<Ping> {
        let q = self.questions.iter().rev().find(|q| q.is_ping())?;
        Some(match &q.answer {
            Some(a) => Ping::Answered { asked_at: q.asked_at, answered_at: a.at },
            None => Ping::Waiting { asked_at: q.asked_at },
        })
    }

    /// Who answered the last connection check (a listener's label), if anyone said.
    pub fn ping_answered_by(&self) -> Option<String> {
        self.questions.iter().rev().find(|q| q.is_ping()).map(|q| q.answered_by.clone()).filter(|b| !b.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty() && self.log.is_empty() && self.safe.is_none() && self.workbook_id.is_empty() && self.links.is_empty()
    }

    pub fn topic(&self, id: &str) -> Option<&Topic> {
        self.topics.iter().find(|t| t.id == id)
    }

    /// Format v2: every question and note belongs to a topic. A v1 layer gets one topic per
    /// question (its cells as places; Open, or For information when answered and read) and one per
    /// note (For information).
    pub fn migrate(&mut self) {
        if self.version >= VERSION {
            return;
        }
        let now = crate::now_ms();
        for i in 0..self.questions.len() {
            let Some(q) = self.questions.get(i) else { continue };
            if q.is_ping() || !q.topic.is_empty() {
                continue;
            }
            let mut places = vec![TopicPlace { anchor: q.anchor.clone(), reason: String::new(), by: Actor::Human, at: q.asked_at }];
            places.extend(q.refs.iter().map(|r| TopicPlace {
                anchor: r.anchor.clone(),
                reason: "pointed at while asking".into(),
                by: Actor::Human,
                at: q.asked_at,
            }));
            let status = if q.status == QuestionStatus::Answered && q.seen { Status::Info } else { Status::Open };
            let asked = q.asked_at;
            let id = self.fresh_id("t");
            self.topics.push(Topic {
                id: id.clone(),
                places,
                status: TopicStatus { status, text: String::new(), by: Actor::Human, at: asked },
                proposed: None,
                hidden: false,
                created_at: asked.max(1).min(now),
            });
            if let Some(q) = self.questions.get_mut(i) {
                q.topic = id;
            }
        }
        for i in 0..self.notes.len() {
            let Some(n) = self.notes.get(i) else { continue };
            if !n.topic.is_empty() {
                continue;
            }
            let (anchor, by, at) = (n.anchor.clone(), n.author, n.at);
            let id = self.fresh_id("t");
            self.topics.push(Topic {
                id: id.clone(),
                places: vec![TopicPlace { anchor, reason: String::new(), by, at }],
                status: TopicStatus { status: Status::Info, text: String::new(), by, at },
                proposed: None,
                hidden: false,
                created_at: at,
            });
            if let Some(n) = self.notes.get_mut(i) {
                n.topic = id;
            }
        }
        self.version = VERSION;
    }

    /// Anchors nothing refers to any more.
    pub fn unused_anchors(&self) -> Vec<String> {
        // Anchors named inside texts (links): kept while a text names them.
        let mut in_text: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut scan = |t: &str| {
            let mut rest = t;
            while let Some(i) = rest.find("(anchor:") {
                rest = &rest[i + 8..];
                let id: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
                in_text.insert(id);
            }
        };
        for q in &self.questions {
            scan(&q.text);
            if let Some(a) = &q.answer {
                scan(&a.text);
            }
        }
        for n in &self.notes {
            scan(&n.text);
            scan(&n.title);
        }
        for t in &self.topics {
            scan(&t.status.text);
            if let Some(p) = &t.proposed {
                scan(&p.reasons);
            }
        }
        let used = |id: &str| {
            in_text.contains(id)
                || self.topics.iter().any(|t| t.places.iter().any(|p| p.anchor == id))
                || self.whereabouts.iter().any(|w| w.anchor == id || w.before.as_deref() == Some(id) || w.after.as_deref() == Some(id))
                || self.questions.iter().any(|q| q.anchor == id || q.refs.iter().any(|r| r.anchor == id))
                || self.notes.iter().any(|n| n.anchor == id)
                || self.inputs.iter().any(|i| i.anchor == id)
                || self.private.iter().any(|p| p == id)
                || self.confidence.iter().any(|c| c.anchor == id)
                || self.log.iter().any(|e| e.anchor.as_deref() == Some(id))
                || self.variants.iter().any(|v| v.anchor == id || v.outputs.iter().any(|o| o == id))
        };
        self.anchors.iter().filter(|a| !used(&a.id)).map(|a| a.id.clone()).collect()
    }
}

// ---------------------------------------------------------------- the store

static STORE: Mutex<Option<HashMap<u64, Arc<Layer>>>> = Mutex::new(None);

fn store() -> MutexGuard<'static, Option<HashMap<u64, Arc<Layer>>>> {
    match STORE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// A fresh layer (no part in the workbook) is already in the current format.
fn fresh() -> Layer {
    Layer { version: VERSION, ..Layer::default() }
}

/// The layer of a document (read from its workbook the first time).
pub fn get(d: &DocState) -> Arc<Layer> {
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    map.entry(d.uid).or_insert_with(|| Arc::new(read_from(&d.wb))).clone()
}

/// Changes the layer of a document.
pub fn update<R>(d: &DocState, f: impl FnOnce(&mut Layer) -> R) -> R {
    let mut g = store();
    let map = g.get_or_insert_with(HashMap::new);
    let entry = map.entry(d.uid).or_insert_with(|| Arc::new(read_from(&d.wb)));
    let layer = Arc::make_mut(entry);
    let r = f(layer);
    layer.revision = layer.revision.wrapping_add(1);
    r
}

/// Forgets a document's layer (it is read from the workbook again next time).
pub fn forget(uid: u64) {
    if let Some(map) = store().as_mut() {
        map.remove(&uid);
    }
}

/// Uids with a layer in the store.
pub fn known_docs() -> Vec<u64> {
    store().as_ref().map(|m| m.keys().copied().collect()).unwrap_or_default()
}

// ---------------------------------------------------------------- in the file

/// The layer kept in a workbook's custom XML part, or an empty one.
pub fn read_from(wb: &Workbook) -> Layer {
    let Some(p) = wb.preserved.as_deref() else { return fresh() };
    for part in &p.parts {
        if part.data.len() > MAX_PART {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&part.data) else { continue };
        if !text.contains(NS) {
            continue;
        }
        if let Some(json) = between(text, "<![CDATA[", "]]>")
            && let Ok(mut layer) = serde_json::from_str::<Layer>(json)
        {
            // A v1 workbook: questions and notes become topics.
            layer.migrate();
            return layer;
        }
    }
    fresh()
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].rfind(close)? + start;
    Some(&s[start..end])
}

/// Writes the layer into the workbook's kept parts (replacing an earlier copy). An empty layer
/// removes the part.
pub fn write_into(wb: &mut Workbook, layer: &Layer) {
    let mut p: Preserved = wb.preserved.as_deref().cloned().unwrap_or_default();
    let ours: Vec<String> = p.parts.iter().filter(|q| std::str::from_utf8(&q.data).is_ok_and(|t| t.contains(NS))).map(|q| q.name.clone()).collect();
    p.parts.retain(|q| !ours.contains(&q.name));
    p.rels.retain(|r| !(r.rel_type == REL_CUSTOM_XML && ours.iter().any(|n| n == &r.target || r.target.ends_with(n.as_str()))) && r.id != REL_ID);
    if !layer.is_empty() {
        // Connection checks belong to this session, not to the file; spoken questions still
        // being transcribed live in memory only.
        let mut layer = layer.clone();
        // A topic made by a question still being spoken waits with it.
        let drafts: Vec<String> = layer.questions.iter().filter(|q| q.status.is_draft()).map(|q| q.topic.clone()).collect();
        let saved: Vec<String> = layer
            .questions
            .iter()
            .filter(|q| !q.status.is_draft())
            .map(|q| q.topic.clone())
            .chain(layer.notes.iter().map(|n| n.topic.clone()))
            .collect();
        layer.topics.retain(|t| !drafts.contains(&t.id) || saved.contains(&t.id));
        layer.questions.retain(|q| !q.is_ping() && !q.status.is_draft());
        layer.version = VERSION;
        let json = serde_json::to_string(&layer).unwrap_or_default();
        // CDATA can't hold `]]>`; JSON only has it inside strings, where `]` means the same.
        let json = json.replace("]]>", "\\u005d\\u005d>");
        let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<agentLayer xmlns=\"{NS}\"><![CDATA[{json}]]></agentLayer>");
        p.parts.push(PreservedPart {
            name: PART_NAME.into(),
            content_type: "application/xml".into(),
            data: Arc::from(xml.into_bytes()),
            rels: vec![],
        });
        p.rels.push(PreservedRel { id: REL_ID.into(), rel_type: REL_CUSTOM_XML.into(), target: PART_NAME.into(), external: false });
    }
    wb.preserved = if p.is_empty() { None } else { Some(Arc::new(p)) };
}
