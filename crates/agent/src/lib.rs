//! The agent layer (a GridCraft fork feature): a layer of interaction with the user's own agent
//! that lives in the document, not in a chat.
//!
//! - Questions asked on a cell go into a queue in the workbook; the agent picks them up through
//!   the in-app MCP server (watching `/events`), answers, and the cell shows "answer ready".
//! - Notes the agent leaves on ranges: explanations, prepared answers, assumptions, sources, with
//!   links to other cells, other notes and the web.
//! - A privacy gate: ranges marked as client data are replaced by stand-ins in everything the
//!   agent reads, with the model recalculated from them.
//! - Deterministic model analysis: the precedent tree of a number, and its sensitivity to inputs
//!   with ranges the agent reasons about.
//! - A decision history and an audit trail of who (person or agent) changed what.
//!
//! Wiring: [`install`] registers the `agent.*` commands and an observer with the engine
//! (`gridcraft_engine::ext`); the app starts the MCP server with [`server::start`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

pub mod analysis;
pub mod anchor;
#[cfg(not(target_arch = "wasm32"))]
pub mod channel;
pub mod commands;
#[cfg(not(target_arch = "wasm32"))]
pub mod context;
#[cfg(not(target_arch = "wasm32"))]
pub mod cowork;
pub mod docs;
pub mod export;
pub mod filecontext;
#[cfg(not(target_arch = "wasm32"))]
pub mod hub;
pub mod layer;
pub mod links;
#[cfg(not(target_arch = "wasm32"))]
pub mod mcp;
pub mod proposals;
#[cfg(not(target_arch = "wasm32"))]
pub mod registry;
#[cfg(not(target_arch = "wasm32"))]
pub mod runner;
pub mod safe;
#[cfg(not(target_arch = "wasm32"))]
pub mod server;
pub mod setranges;
#[cfg(not(target_arch = "wasm32"))]
pub mod stdio;
pub mod stress;
pub mod textlinks;
pub mod topics;
pub mod variants;

use std::sync::Arc;

use gridcraft_engine::ext::Observer;
use gridcraft_engine::{Result, Session};
use serde_json::Value as Json;

pub use layer::{Actor, Layer};

thread_local! {
    /// The layer is read-only on this thread (the app's: a browser page has only one).
    static READ_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the layer read-only (an exported web page): commands that would change it are refused,
/// except marking an answer read. Call it on the thread that runs the app.
pub fn set_read_only(on: bool) {
    READ_ONLY.with(|r| r.set(on));
}

/// Whether the layer is read-only (see [`set_read_only`]).
pub fn read_only() -> bool {
    READ_ONLY.with(std::cell::Cell::get)
}

/// A tool call of an agent ("last tool call"): an `agent.*` command with actor agent; a stream's
/// polling (`poll`) or a wait loop (`waiting`) isn't one.
pub fn is_tool_call(id: &str, params: &Json) -> bool {
    let polling = ["waiting", "poll"].iter().any(|k| params.get(*k).and_then(Json::as_bool) == Some(true));
    id.starts_with("agent.") && params.get("actor").and_then(Json::as_str) == Some("agent") && !polling
}

/// The agent connection is off (Agent tab ▸ Connection ▸ Mode): agent-side commands refuse.
pub fn agent_off() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        runner::mode() == runner::Mode::Off
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

/// Milliseconds since the Unix epoch (0 where the platform has no clock).
pub fn now_ms() -> u64 {
    gridcraft_engine::core::date::unix_now_ms().map_or(0, |ms| ms.max(0.0) as u64)
}

/// Registers the agent commands and the observer that keeps the layer in step with the
/// workbook. Safe to call more than once.
pub fn install() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        for specs in commands::ALL {
            gridcraft_engine::ext::register_commands(specs);
        }
        gridcraft_engine::ext::register_observer(&KEEPER);
    });
}

/// The layer of the active document.
pub fn active_layer(s: &Session) -> Option<Arc<Layer>> {
    s.active().map(layer::get)
}

static LAST_AGENT_CALL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// When an agent last called the layer (ms since the epoch; 0 for never), through MCP over HTTP
/// or stdio alike.
pub fn last_agent_call_ms() -> u64 {
    LAST_AGENT_CALL.load(std::sync::atomic::Ordering::Relaxed)
}

struct Keeper;
static KEEPER: Keeper = Keeper;

thread_local! {
    /// Workbook pointers before the commands running now (nested commands stack up).
    static BEFORE: std::cell::RefCell<Vec<Option<(u64, usize)>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn wb_ptr(s: &Session) -> Option<(u64, usize)> {
    s.active().map(|d| (d.uid, Arc::as_ptr(&d.wb) as usize))
}

/// Commands that never change what the document says (the audit log skips them).
fn is_view_only(id: &str) -> bool {
    id.starts_with("agent.")
        || id.starts_with("selection.")
        || id.starts_with("view.")
        || id.starts_with("file.")
        || id.starts_with("sheet.activate")
        || id.starts_with("ui.")
        || id.starts_with("link.")
        || id == "edit.copy"
}

impl Observer for Keeper {
    fn before(&self, s: &mut Session, id: &str, params: &Json) {
        if is_tool_call(id, params) {
            LAST_AGENT_CALL.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
        }
        // A safe save that didn't finish puts the real workbook back first.
        safe::restore(s);
        // Deleted places and links keep their whereabouts.
        if matches!(id, "home.deleteRows" | "home.deleteColumns" | "home.deleteCells" | "home.deleteSheet") {
            topics::before_delete(s, id, params);
        }
        BEFORE.with(|b| b.borrow_mut().push(wb_ptr(s)));
        // Write the layer into the workbook just before it is saved; in safe file mode, save a
        // copy with stand-ins for client data and the real values sealed instead.
        let saving = matches!(id, "file.save" | "file.saveAs" | "file.saveBytes");
        if saving && safe::swap_in(s) {
            return;
        }
        if saving && let Some(d) = s.active_mut() {
            let l = layer::get(d);
            let has_part =
                d.wb.preserved.as_deref().is_some_and(|p| p.parts.iter().any(|q| std::str::from_utf8(&q.data).is_ok_and(|t| t.contains(layer::NS))));
            if !l.is_empty() || has_part {
                layer::write_into(Arc::make_mut(&mut d.wb), &l);
            }
        }
    }

    fn after(&self, s: &mut Session, id: &str, params: &Json, result: &Result<Json>) {
        let before = BEFORE.with(|b| b.borrow_mut().pop()).flatten();
        safe::restore(s);
        if id == "file.open"
            && result.is_ok()
            && let Some(d) = s.active_mut()
            && let Err(e) = safe::unseal(d)
        {
            log::warn!("safe file: {e}; the client data stays locked (stand-ins)");
        }
        if id == "file.open"
            && result.is_ok()
            && let Some(d) = s.active()
        {
            links::load_registry(d);
        }
        let Some(d) = s.active_mut() else { return };
        anchor::refresh(d);
        links::refresh(d);
        let changed = result.is_ok() && before.is_some_and(|(uid, ptr)| uid == d.uid && ptr != Arc::as_ptr(&d.wb) as usize);
        let quiet = commands::QUIET.with(std::cell::Cell::get) > 0;
        // Commands that write their own entry (one for many changes) aren't logged again.
        let self_logged = matches!(id, "agent.setRanges" | "agent.applyProposal");
        if !changed || quiet || self_logged || is_view_only(id) || id == "edit.undo" || id == "edit.redo" {
            return;
        }
        // The audit trail: who changed what, on what basis. People's own edits are recorded once
        // the document has an agent layer (a plain workbook stays plain).
        let agent = commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0;
        if !agent && layer::get(d).is_empty() {
            return;
        }
        // The agent's edits stay in the person's history, labelled as the agent's.
        if agent
            && let Some(e) = d.undo.last_mut()
            && before.is_some_and(|(_, ptr)| Arc::as_ptr(&e.wb) as usize == ptr)
            && !e.label.starts_with("(agent)")
        {
            e.label = format!("(agent) {}", e.label);
        }
        let (reason, source) = if agent { commands::PENDING_REASON.with(|r| r.borrow().clone()).unwrap_or_default() } else { Default::default() };
        let label = gridcraft_engine::find_command(id).map(|c| c.label.trim_end_matches('…').to_string()).unwrap_or_else(|| id.to_string());
        let target = ["cell", "range"]
            .iter()
            .find_map(|k| params.get(*k).and_then(Json::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| d.selection.current().a1());
        let text = if reason.is_empty() { format!("{label} {target}") } else { format!("{label} {target}: {reason}") };
        let actor = if agent { Actor::Agent } else { Actor::Human };
        layer::update(d, |l| l.log(actor, "change", text, id, None, source));
    }
}

#[cfg(test)]
mod tests;
