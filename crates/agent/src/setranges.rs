//! `set_ranges`: many cells in one call. Values, formulas and formats of several ranges as ONE
//! command: one undo step ("(agent): set 3 ranges"), one entry in the history, refused whole when
//! any range touches client data. With `topic` it is one step of that topic's proposal. An agent
//! pays a model turn per tool call; a sheet filled cell by cell takes a second a cell.

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::{HistoryEntry, Result, Session};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Place};
use crate::commands::{AGENT_DEPTH, QUIET, actor_p, bad, spec, str_p};
use crate::layer::{self, Actor};

pub static SPECS: &[CommandSpec] = &[spec!(
    change "agent.setRanges",
    "Set Ranges",
    "{ranges: [{range, values?, formulas?, format?}], reason, source?, topic?, option?} — values and formulas are 2-D arrays matching the range (a single value fills it); format {bold, italic, underline, strikethrough, wrap, numberFormat, fill, fontColor, fontSize, fontName, align: left|center|right, valign: top|middle|bottom}. One command, one undo step; with `topic`, one step of its proposal",
    set_ranges
)];

const CMD: &str = "agent.setRanges";
/// Most ranges in a call, and cells in all of them.
const MAX_RANGES: usize = 200;
pub const MAX_CELLS: u64 = 100_000;
/// Undo depth kept (the engine's own limit).
const UNDO_LIMIT: usize = 100;

/// The engine commands one range turns into.
fn steps_for(place: &Place, r: &Json) -> Result<Vec<(&'static str, Json)>> {
    let a1 = place.a1();
    let (h, w) = (place.range.end.row - place.range.start.row + 1, place.range.end.col - place.range.start.col + 1);
    let mut out = vec![];
    let values = r.get("values").filter(|v| !v.is_null());
    let formulas = r.get("formulas").filter(|v| !v.is_null());
    if values.is_some() || formulas.is_some() {
        let grid_of = |v: Option<&Json>, what: &str| -> Result<Option<Vec<Vec<Json>>>> {
            let Some(v) = v else { return Ok(None) };
            let rows: Vec<Vec<Json>> = match v {
                // One value fills the range.
                Json::Array(rows) if rows.iter().all(Json::is_array) => rows.iter().map(|x| x.as_array().cloned().unwrap_or_default()).collect(),
                // A flat list: one row, or one column.
                Json::Array(list) if h == 1 => vec![list.clone()],
                Json::Array(list) if w == 1 => list.iter().map(|x| vec![x.clone()]).collect(),
                Json::Array(_) => return Err(bad(CMD, format!("{a1}: `{what}` is a 2-D array of {h} row(s) × {w} column(s)"))),
                one => vec![vec![one.clone(); w as usize]; h as usize],
            };
            if rows.len() != h as usize || rows.iter().any(|x| x.len() != w as usize) {
                return Err(bad(CMD, format!("{a1}: `{what}` must match the range: {h} row(s) × {w} column(s)")));
            }
            Ok(Some(rows))
        };
        let vg = grid_of(values, "values")?;
        let fg = grid_of(formulas, "formulas")?;
        let mut grid: Vec<Vec<Json>> = vec![vec![Json::Null; w as usize]; h as usize];
        for (ri, row) in grid.iter_mut().enumerate() {
            for (ci, cell) in row.iter_mut().enumerate() {
                let f = fg.as_ref().and_then(|g| g.get(ri)).and_then(|x| x.get(ci)).filter(|x| !x.is_null());
                let v = vg.as_ref().and_then(|g| g.get(ri)).and_then(|x| x.get(ci));
                *cell = match (f, v) {
                    (Some(Json::String(t)), _) if !t.trim().is_empty() => {
                        let t = t.trim();
                        json!(if t.starts_with('=') { t.to_string() } else { format!("={t}") })
                    }
                    (Some(other), _) => other.clone(),
                    // A value that looks like a formula is text.
                    (None, Some(Json::String(t))) if t.starts_with('=') => json!(format!("'{t}")),
                    (None, Some(v)) => v.clone(),
                    (None, None) => Json::Null,
                };
            }
        }
        out.push(("range.setValues", json!({"range": a1, "values": grid})));
    }
    if let Some(f) = r.get("format").filter(|f| !f.is_null()) {
        let Some(f) = f.as_object() else { return Err(bad(CMD, format!("{a1}: `format` is an object"))) };
        for (k, v) in f {
            let on = v.as_bool().unwrap_or(true);
            let step = match k.as_str() {
                "bold" => ("home.bold", json!({"range": a1, "on": on})),
                "italic" => ("home.italic", json!({"range": a1, "on": on})),
                "underline" => ("home.underline", json!({"range": a1, "on": on})),
                "strikethrough" => ("home.strikethrough", json!({"range": a1, "on": on})),
                "wrap" | "wrapText" => ("home.wrapText", json!({"range": a1, "on": on})),
                "numberFormat" => {
                    let code = v.as_str().unwrap_or("General");
                    const NAMED: [&str; 12] = [
                        "General",
                        "Number",
                        "Currency",
                        "Accounting",
                        "Short Date",
                        "Long Date",
                        "Time",
                        "Percentage",
                        "Fraction",
                        "Scientific",
                        "Text",
                        "Percent",
                    ];
                    match NAMED.iter().find(|n| n.eq_ignore_ascii_case(code)) {
                        Some(n) => ("home.numberFormat", json!({"range": a1, "format": if *n == "Percent" { "Percentage" } else { n }})),
                        None => ("home.numberFormat", json!({"range": a1, "code": code})),
                    }
                }
                "fill" => ("home.fillColor", json!({"range": a1, "color": v})),
                "fontColor" => ("home.fontColor", json!({"range": a1, "color": v})),
                "fontSize" => ("home.fontSize", json!({"range": a1, "size": v})),
                "fontName" => ("home.fontName", json!({"range": a1, "name": v})),
                "align" => match v.as_str().unwrap_or("") {
                    "left" => ("home.alignLeft", json!({"range": a1})),
                    "center" => ("home.alignCenter", json!({"range": a1})),
                    "right" => ("home.alignRight", json!({"range": a1})),
                    x => return Err(bad(CMD, format!("{a1}: align is left, center or right, not `{x}`"))),
                },
                "valign" => match v.as_str().unwrap_or("") {
                    "top" => ("home.alignTop", json!({"range": a1})),
                    "middle" | "center" => ("home.alignMiddle", json!({"range": a1})),
                    "bottom" => ("home.alignBottom", json!({"range": a1})),
                    x => return Err(bad(CMD, format!("{a1}: valign is top, middle or bottom, not `{x}`"))),
                },
                other => {
                    return Err(bad(
                        CMD,
                        format!(
                            "{a1}: unknown format `{other}` (bold, italic, underline, strikethrough, wrap, numberFormat, fill, fontColor, fontSize, fontName, align, valign)"
                        ),
                    ));
                }
            };
            out.push(step);
        }
    }
    Ok(out)
}

fn set_ranges(s: &mut Session, p: &Json) -> Result<Json> {
    let agent = actor_p(p, Actor::Human) == Actor::Agent || AGENT_DEPTH.with(std::cell::Cell::get) > 0;
    let Some(list) = p.get("ranges").and_then(Json::as_array).filter(|l| !l.is_empty()) else {
        return Err(bad(CMD, "`ranges` is a list of {range, values?, formulas?, format?}"));
    };
    if list.len() > MAX_RANGES {
        return Err(bad(CMD, format!("at most {MAX_RANGES} ranges in a call")));
    }
    let reason: String = str_p(p, "reason").unwrap_or("").chars().take(4_000).collect();
    if agent && reason.trim().is_empty() {
        return Err(bad(CMD, "give a `reason`"));
    }
    let source: String = str_p(p, "source").unwrap_or("").chars().take(2_000).collect();
    // Every range first: the whole call is refused before anything changes.
    let d = s.doc()?;
    let l = layer::get(d);
    let private = crate::analysis::private_cells(&d.wb, &l);
    let mut places = vec![];
    let mut cells = 0u64;
    let mut touched = vec![];
    for r in list {
        let Some(a) = r.get("range").and_then(Json::as_str) else { return Err(bad(CMD, "each range needs `range`")) };
        let place = anchor::parse_place(&d.wb, a, d.wb.active_sheet).ok_or_else(|| bad(CMD, format!("`{a}` isn't a range")))?;
        cells = cells.saturating_add(place.range.count());
        if cells > MAX_CELLS {
            return Err(bad(CMD, format!("at most {MAX_CELLS} cells in a call: split it")));
        }
        if agent {
            touched
                .extend(place.range.iter().filter(|c| private.contains(&(place.sheet, *c))).take(20).map(|c| Place::of(&d.wb, place.sheet, c).a1()));
        }
        places.push((place, r));
    }
    if !touched.is_empty() {
        touched.truncate(10);
        return Err(bad(CMD, format!("{} hold client data: the agent can't write there; nothing was changed", touched.join(", "))));
    }
    let mut steps = vec![];
    for (place, r) in &places {
        steps.extend(steps_for(place, r)?);
    }
    let n = places.len();
    let what = format!("set {n} range{}", if n == 1 { "" } else { "s" });
    // In a topic: one step of its proposal, nothing changes now.
    if p.get("topic").is_some_and(|t| !t.is_null()) {
        let ranges = Json::Array(list.clone());
        return crate::proposals::record(s, p, CMD, json!({"ranges": ranges, "reason": reason, "source": source}), reason);
    }
    // Run them as one: their own history entries set aside, quietly; one step and one entry.
    let (before, sel, undo, redo) = {
        let d = s.doc_mut()?;
        (d.wb.clone(), d.selection.clone(), std::mem::take(&mut d.undo), std::mem::take(&mut d.redo))
    };
    QUIET.with(|x| x.set(x.get().saturating_add(1)));
    let mut failed = None;
    for (cmd, params) in &steps {
        if let Err(e) = s.execute(cmd, params.clone()) {
            failed = Some(format!("{cmd}: {e}"));
            break;
        }
    }
    QUIET.with(|x| x.set(x.get().saturating_sub(1)));
    let d = s.doc_mut()?;
    d.undo = undo;
    if let Some(e) = failed {
        d.redo = redo;
        d.wb = before;
        d.selection = sel;
        d.calc.rebuild(&d.wb);
        d.revision += 1;
        return Err(bad(CMD, format!("{e}; nothing was changed")));
    }
    let label = if agent { format!("(agent): {what}") } else { format!("Set {n} Range{}", if n == 1 { "" } else { "s" }) };
    d.undo.push(HistoryEntry { label, wb: before, selection: sel, step: None });
    if d.undo.len() > UNDO_LIMIT {
        d.undo.remove(0);
    }
    let names: Vec<String> = places.iter().map(|(pl, _)| crate::textlinks::address(pl, d.wb.active_sheet)).collect();
    let text = if reason.is_empty() { format!("Set {}", names.join(", ")) } else { format!("Set {}: {reason}", names.join(", ")) };
    let d = s.doc()?;
    layer::update(d, |l| l.log(if agent { Actor::Agent } else { Actor::Human }, "change", text, CMD, None, source));
    Ok(json!({"ok": true, "ranges": n, "cells": cells, "commands": steps.len()}))
}
