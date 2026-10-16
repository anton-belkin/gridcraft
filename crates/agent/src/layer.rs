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
    /// Empty for a reader's question; [`PING`] for a connection check (never shown as a question,
    /// never saved).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
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

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty() && self.log.is_empty() && self.safe.is_none() && self.workbook_id.is_empty() && self.links.is_empty()
    }

    /// Anchors nothing refers to any more.
    pub fn unused_anchors(&self) -> Vec<String> {
        let used = |id: &str| {
            self.questions.iter().any(|q| q.anchor == id)
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
    let Some(p) = wb.preserved.as_deref() else { return Layer::default() };
    for part in &p.parts {
        if part.data.len() > MAX_PART {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&part.data) else { continue };
        if !text.contains(NS) {
            continue;
        }
        if let Some(json) = between(text, "<![CDATA[", "]]>")
            && let Ok(layer) = serde_json::from_str::<Layer>(json)
        {
            return layer;
        }
    }
    Layer::default()
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
        // Connection checks belong to this session, not to the file.
        let mut layer = layer.clone();
        layer.questions.retain(|q| !q.is_ping());
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
