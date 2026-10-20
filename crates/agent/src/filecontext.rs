//! The file's own context (brief part E): what the document is, for whom, house rules,
//! background, facts the agent gathered. Not tied to a place; kept in the file and when it is sent.
//! The agent reads a short summary with `get_context` and every question; a person's edits are
//! undoable.

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::commands::{actor_p, bad, spec, str_p, text_p};
use crate::layer::{self, Actor, FileContext, Layer};
use crate::topics::{self, Part};

pub static SPECS: &[CommandSpec] = &[
    spec!(query "agent.fileContext", "File Context", "{} → the file's context entries: id, title, kind, text (links inside), who added it, when", get),
    spec!(change "agent.addFileContext", "Add File Context", "{title, kind?, text} — context about the whole document (what it is, for whom, rules, background, facts) → id", add),
    spec!(change "agent.updateFileContext", "Edit File Context", "{id, title?, kind?, text?}", update),
    spec!(change "agent.removeFileContext", "Remove File Context", "{id}", remove),
];

/// Most entries, and characters of an entry's text.
const MAX_ENTRIES: usize = 200;
const MAX_TEXT: usize = 20_000;

pub fn entry_json(wb: &gridcraft_engine::model::Workbook, l: &Layer, f: &FileContext) -> Json {
    json!({"id": f.id, "title": f.title, "kind": f.kind, "text": crate::textlinks::render(wb, l, &f.text, wb.active_sheet), "by": f.by, "at": f.at})
}

/// A short summary for the agent: each entry's title and the start of its text.
pub fn summary(wb: &gridcraft_engine::model::Workbook, l: &Layer) -> String {
    let mut out = String::new();
    for f in &l.file_context {
        let text = crate::textlinks::plain(wb, l, &f.text, wb.active_sheet);
        let short: String = text.chars().take(140).collect();
        let more = if text.chars().count() > 140 { "…" } else { "" };
        let line = if f.title.is_empty() { format!("- {short}{more}\n") } else { format!("- {}: {short}{more}\n", f.title) };
        if out.chars().count() + line.chars().count() > 900 {
            out.push_str("- … (get_file_context has the rest)\n");
            break;
        }
        out.push_str(&line);
    }
    out.trim_end().to_string()
}

fn get(s: &mut Session, _p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let l = layer::get(d);
    Ok(Json::Array(l.file_context.iter().map(|f| entry_json(&d.wb, &l, f)).collect()))
}

/// Applies a change; a person's is an undo step.
fn change(s: &mut Session, p: &Json, label: &str, f: impl FnOnce(&mut Vec<FileContext>) -> Result<Json>) -> Result<Json> {
    let by = actor_p(p, Actor::Human);
    let before = layer::get(s.doc()?).file_context.clone();
    let mut after = before.clone();
    let out = f(&mut after)?;
    if by == Actor::Human {
        topics::record(s, label, vec![Part::FileContext(before)], vec![Part::FileContext(after.clone())]);
    }
    let d = s.doc()?;
    layer::update(d, |l| {
        l.file_context = after;
        l.log(by, "change", format!("File context: {label}"), "agent.fileContext", None, String::new());
    });
    Ok(out)
}

fn add(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.addFileContext";
    let text: String = text_p(p, "text", CMD)?.chars().take(MAX_TEXT).collect();
    let text = crate::textlinks::store(s, &text)?;
    let title: String = str_p(p, "title").unwrap_or("").trim().chars().take(200).collect();
    let kind: String = str_p(p, "kind").unwrap_or("background").trim().chars().take(40).collect();
    let by = actor_p(p, Actor::Human);
    let id = layer::update(s.doc()?, |l| l.fresh_id("f"));
    if layer::get(s.doc()?).file_context.len() >= MAX_ENTRIES {
        return Err(bad(CMD, format!("the file context holds at most {MAX_ENTRIES} entries")));
    }
    let entry = FileContext { id: id.clone(), title, kind, text, by, at: crate::now_ms() };
    change(s, p, "Add File Context", |v| {
        v.push(entry);
        Ok(json!({"id": id}))
    })
}

fn update(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.updateFileContext";
    let id = text_p(p, "id", CMD)?;
    let text = match str_p(p, "text") {
        Some(t) => Some(crate::textlinks::store(s, &t.chars().take(MAX_TEXT).collect::<String>())?),
        None => None,
    };
    let title = str_p(p, "title").map(|t| t.trim().chars().take(200).collect::<String>());
    let kind = str_p(p, "kind").map(|t| t.trim().chars().take(40).collect::<String>());
    let by = actor_p(p, Actor::Human);
    change(s, p, "Edit File Context", |v| {
        let Some(f) = v.iter_mut().find(|f| f.id == id) else { return Err(bad(CMD, format!("no entry `{id}`"))) };
        if let Some(t) = text {
            f.text = t;
        }
        if let Some(t) = title {
            f.title = t;
        }
        if let Some(k) = kind {
            f.kind = k;
        }
        f.by = by;
        f.at = crate::now_ms();
        Ok(json!({"id": id}))
    })
}

fn remove(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.removeFileContext";
    let id = text_p(p, "id", CMD)?;
    change(s, p, "Remove File Context", |v| {
        let n = v.len();
        v.retain(|f| f.id != id);
        if v.len() == n { Err(bad(CMD, format!("no entry `{id}`"))) } else { Ok(json!({"id": id, "removed": true})) }
    })
}
