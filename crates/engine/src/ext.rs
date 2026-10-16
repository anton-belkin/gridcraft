//! Extension points: commands and command observers registered at startup by crates built on top
//! of the engine (an app links them in; the engine knows nothing about them).
//!
//! - [`register_commands`] adds commands that behave like built-in ones: [`crate::find_command`]
//!   finds them, [`crate::Session::commands`] lists them, so the control channel, CLI and MCP reach
//!   them like every other command.
//! - [`register_observer`] is told before and after every command [`crate::Session::execute`]
//!   runs, so an extension can keep its own per-document state in step with the workbook (load it
//!   when a file opens, write it into the workbook before a save, record who changed what).
//!
//! - [`push_undo_step`] puts an extension's own step into a document's undo history, between its
//!   workbook snapshots: undo and redo run it in order with the ordinary steps, and it never
//!   restores cells (nor does undoing a cell edit touch the extension's state).
//!
//! Registration is process-wide and additive; registering the same id twice keeps the first.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value as Json;

use crate::cmd::CommandSpec;
use crate::{Result, Session};

/// Told about every command [`Session::execute`] runs.
pub trait Observer: Send + Sync {
    /// Before the command runs (after its `enabled` check passed). Changes made to the active
    /// document here are part of the state the command starts from and are not an undo step.
    fn before(&self, _s: &mut Session, _id: &str, _params: &Json) {}
    /// After the command ran (its undo step, if any, is already recorded).
    fn after(&self, _s: &mut Session, _id: &str, _params: &Json, _result: &Result<Json>) {}
}

static COMMANDS: Mutex<Vec<&'static CommandSpec>> = Mutex::new(Vec::new());
static OBSERVERS: Mutex<Vec<&'static dyn Observer>> = Mutex::new(Vec::new());

fn lock<T>(m: &'static Mutex<T>) -> MutexGuard<'static, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Adds extension commands. Ids already known (built-in or registered before) are skipped.
pub fn register_commands(specs: &'static [CommandSpec]) {
    let mut v = lock(&COMMANDS);
    for spec in specs {
        let known = crate::cmd::command_specs().iter().any(|c| c.id == spec.id) || v.iter().any(|c| c.id == spec.id);
        if !known {
            v.push(spec);
        }
    }
}

/// Adds a command observer.
pub fn register_observer(o: &'static dyn Observer) {
    lock(&OBSERVERS).push(o);
}

/// The registered extension commands.
pub fn commands() -> Vec<&'static CommandSpec> {
    lock(&COMMANDS).clone()
}

pub(crate) fn find(id: &str) -> Option<&'static CommandSpec> {
    lock(&COMMANDS).iter().find(|c| c.id == id).copied()
}

fn observers() -> Vec<&'static dyn Observer> {
    lock(&OBSERVERS).clone()
}

pub(crate) fn before(s: &mut Session, id: &str, params: &Json) {
    for o in observers() {
        // An observer that panics is skipped; the command still runs.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.before(s, id, params)));
    }
}

pub(crate) fn after(s: &mut Session, id: &str, params: &Json, result: &Result<Json>) {
    for o in observers() {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.after(s, id, params, result)));
    }
}

/// An extension's own undo step (pushed with [`push_undo_step`]).
pub trait UndoStep: std::fmt::Debug + Send + Sync {
    fn undo(&self, s: &mut Session);
    fn redo(&self, s: &mut Session);
}

/// Pushes a step onto the active document's undo history (the redo history is cleared, as by any
/// new action). `label` is what Undo says ("Undo Ask the Agent").
pub fn push_undo_step(s: &mut Session, label: &str, step: Arc<dyn UndoStep>) -> Result<()> {
    let d = s.doc_mut()?;
    d.undo.push(crate::HistoryEntry { label: label.to_string(), wb: d.wb.clone(), selection: d.selection.clone(), step: Some(step) });
    if d.undo.len() > crate::UNDO_LIMIT {
        d.undo.remove(0);
    }
    d.redo.clear();
    d.revision += 1;
    Ok(())
}

/// Undo/redo: a step entry moves to the other history and its step is collected to run (the
/// workbook stays as it is); a snapshot entry is handed back.
pub(crate) fn take_step(
    e: crate::HistoryEntry,
    other: &mut Vec<crate::HistoryEntry>,
    label: &mut String,
    run: &mut Vec<Arc<dyn UndoStep>>,
) -> Option<crate::HistoryEntry> {
    let Some(step) = e.step.clone() else { return Some(e) };
    label.clone_from(&e.label);
    run.push(step);
    other.push(e);
    None
}

/// Runs the collected steps (in the order they were taken).
pub(crate) fn run_steps(s: &mut Session, run: Vec<Arc<dyn UndoStep>>, undo: bool) {
    for step in run {
        // A step that panics is skipped; the history stays consistent.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| if undo { step.undo(s) } else { step.redo(s) }));
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    /// A step that records what ran.
    #[derive(Debug)]
    struct Log(&'static Mutex<Vec<String>>, &'static str);
    impl UndoStep for Log {
        fn undo(&self, _s: &mut Session) {
            self.0.lock().unwrap().push(format!("undo {}", self.1));
        }
        fn redo(&self, _s: &mut Session) {
            self.0.lock().unwrap().push(format!("redo {}", self.1));
        }
    }

    #[test]
    fn own_steps_interleave_with_snapshots() {
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let mut s = Session::new();
        s.new_workbook();
        let a1 = |s: &Session| s.active().unwrap().wb.active().unwrap().cell(gridcraft_core::CellRef::parse("A1").unwrap()).map(|c| c.value.clone());
        s.execute("cell.set", json!({"cell": "A1", "input": 1})).unwrap();
        push_undo_step(&mut s, "Step X", Arc::new(Log(&LOG, "x"))).unwrap();
        s.execute("cell.set", json!({"cell": "A1", "input": 2})).unwrap();
        // Undo: the cell edit, then the step (cells untouched), then the first edit.
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(a1(&s), Some(gridcraft_core::Value::Number(1.0)));
        let r = s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(r["undone"], json!("Step X"));
        assert_eq!(a1(&s), Some(gridcraft_core::Value::Number(1.0)));
        assert_eq!(*LOG.lock().unwrap(), vec!["undo x"]);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(a1(&s), None);
        // Redo in order: the edit, the step, the second edit.
        s.execute("edit.redo", json!({"steps": 3})).unwrap();
        assert_eq!(a1(&s), Some(gridcraft_core::Value::Number(2.0)));
        assert_eq!(*LOG.lock().unwrap(), vec!["undo x", "redo x"]);
        // A new action clears the redo history, steps included.
        push_undo_step(&mut s, "Step Y", Arc::new(Log(&LOG, "y"))).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("cell.set", json!({"cell": "A2", "input": 3})).unwrap();
        assert!(s.active().unwrap().redo.is_empty());
    }
}
