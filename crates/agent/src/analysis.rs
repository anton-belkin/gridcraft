//! Deterministic analysis of the model: the agent's privacy-masked view, "where does this number
//! come from" (the precedent tree down to inputs) and sensitivity of an output to its inputs.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use gridcraft_engine::DocState;
use gridcraft_engine::calc::{Calc, Key};
use gridcraft_engine::core::{CellRef, RangeRef, Value};
use gridcraft_engine::model::{Cell, Sheet, Workbook};
use serde_json::{Value as Json, json};

use crate::anchor::{self, Place};
use crate::layer::Layer;

/// Most cells walked by one trace.
const MAX_TRACE_NODES: usize = 400;
/// Most cells of one precedent range listed in a trace.
const MAX_AREA_CELLS: usize = 24;
/// Most cells masked or read at once.
const MAX_CELLS: usize = 200_000;

/// Private cells of a layer, by sheet.
pub fn private_cells(wb: &Workbook, l: &Layer) -> HashSet<Key> {
    let mut out = HashSet::new();
    for id in &l.private {
        let Some(p) = l.anchor(id).and_then(|a| anchor::resolve(wb, a)) else { continue };
        for c in p.range.iter().take(MAX_CELLS) {
            out.insert((p.sheet, c));
        }
    }
    out
}

/// Cells whose value depends (directly or not) on a private cell.
pub fn derived_from(calc: &Calc, private: &HashSet<Key>) -> HashSet<Key> {
    let mut seen: HashSet<Key> = HashSet::new();
    let mut queue: VecDeque<Key> = private.iter().copied().collect();
    let mut buf = Vec::new();
    while let Some((s, c)) = queue.pop_front() {
        if seen.len() >= MAX_CELLS {
            break;
        }
        buf.clear();
        calc.graph.dependents(s, c, &mut buf);
        for k in buf.drain(..) {
            if !private.contains(&k) && seen.insert(k) {
                queue.push_back(k);
            }
        }
    }
    seen
}

/// A stand-in for a private number: same sign, same order of magnitude, same "integer-ness",
/// different value (stable per cell, so the agent sees a consistent model).
pub fn stand_in(v: f64, salt: u64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    // splitmix64 over the salt → a factor in [0.55, 1.45].
    let mut z = salt.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    let unit = (z % 10_000) as f64 / 10_000.0;
    let factor = 0.55 + 0.9 * unit;
    let x = v * factor;
    if v.fract() == 0.0 {
        let r = x.round();
        if r == v { r + v.signum() } else { r }
    } else {
        // Keep about as many significant digits as a typical input.
        let mag = 10f64.powi(x.abs().log10().floor() as i32 - 2);
        if mag.is_finite() && mag > 0.0 { (x / mag).round() * mag } else { x }
    }
}

fn salt(k: Key) -> u64 {
    (k.0 as u64).wrapping_mul(1_000_003) ^ (u64::from(k.1.row) << 20) ^ u64::from(k.1.col)
}

/// A copy of the workbook as the agent may see it: private constants replaced by stand-ins (text
/// by placeholders), everything recalculated from them. Formulas are untouched.
pub fn masked(d: &DocState, l: &Layer) -> (Workbook, Calc) {
    let mut wb: Workbook = (*d.wb).clone();
    let private = private_cells(&wb, l);
    for &(si, c) in &private {
        let Some(sh) = wb.sheets.get_mut(si) else { continue };
        let Some(cell) = sh.cell(c).cloned() else { continue };
        if cell.formula.is_some() {
            continue;
        }
        let value = match &cell.value {
            Value::Number(n) => Value::Number(stand_in(*n, salt((si, c)))),
            Value::Text(_) => Value::Text(Arc::from(format!("{{{{private:{}}}}}", c.a1()))),
            Value::Bool(_) => Value::Bool(false),
            other => other.clone(),
        };
        Arc::make_mut(sh).set_cell(c, Cell { value, ..cell });
    }
    let mut calc = Calc::new();
    calc.recalc_all(&mut wb);
    (wb, calc)
}

/// Text shown for a cell's value.
pub fn value_json(v: &Value) -> Json {
    match v {
        Value::Empty => Json::Null,
        Value::Number(n) => json!(n),
        Value::Text(t) => json!(t.as_ref()),
        Value::Bool(b) => json!(b),
        Value::Error(e) => json!(format!("{e:?}")),
        Value::Array(_) => json!("{array}"),
    }
}

/// A label for a cell from the sheet: the nearest text to its left in the row, else above it in
/// the column ("Revenue growth", "2027").
pub fn label_of(sh: &Sheet, c: CellRef) -> String {
    let text = |r: u32, col: u32| match sh.cell(CellRef::new(r, col)).map(|x| &x.value) {
        Some(Value::Text(t)) if !t.trim().is_empty() => Some(t.to_string()),
        _ => None,
    };
    let row_label = (0..c.col).rev().take(12).find_map(|col| text(c.row, col));
    // A column label counts only in a header row: other headings beside it (not the row labels
    // in column A), so a value written above in a vertical model isn't taken for one.
    let header_row =
        |r: u32| (c.col.saturating_sub(6)..=c.col.saturating_add(6)).filter(|&k| k != c.col && k != 0 && text(r, k).is_some()).count() >= 1;
    // Up the column through the block the cell is in: a blank cell ends it.
    let filled = |r: u32| sh.cell(CellRef::new(r, c.col)).is_some_and(|x| !x.value.is_empty() || x.formula.is_some());
    let col_label = (0..c.row).rev().take(40).take_while(|&r| filled(r)).find_map(|r| text(r, c.col).filter(|_| header_row(r)));
    match (row_label, col_label) {
        (Some(r), Some(k)) if k != r => format!("{r} · {k}"),
        (Some(r), _) => r,
        (None, Some(k)) => k,
        (None, None) => String::new(),
    }
}

/// One cell of a trace.
fn node(wb: &Workbook, si: usize, c: CellRef, private: &HashSet<Key>, derived: &HashSet<Key>) -> Json {
    let Some(sh) = wb.sheet(si) else { return Json::Null };
    let cell = sh.cell(c);
    let formula = cell.and_then(|x| x.formula.as_ref()).map(|f| format!("={}", f.text.trim_start_matches('=')));
    let value = cell.map(|x| value_json(&x.value)).unwrap_or(Json::Null);
    let kind = if formula.is_some() {
        "formula"
    } else if cell.is_some_and(|x| matches!(x.value, Value::Number(_))) {
        "input"
    } else {
        "constant"
    };
    let mut o = json!({
        "cell": Place { sheet: si, sheet_name: sh.name.clone(), range: RangeRef::cell(c) }.a1(),
        "label": label_of(sh, c),
        "kind": kind,
        "value": value,
        "text": gridcraft_engine::display::cell_text(wb, sh, c),
    });
    if let Some(f) = formula {
        o["formula"] = json!(f);
    }
    if private.contains(&(si, c)) {
        o["private"] = json!(true);
    }
    if derived.contains(&(si, c)) {
        o["derivedFromPrivate"] = json!(true);
    }
    o
}

/// The calculation tree of a cell down to its inputs: every formula with what it reads.
/// Deterministic; the agent adds the explanation.
pub fn trace(wb: &Workbook, calc: &Calc, start: Key, depth: usize, private: &HashSet<Key>, derived: &HashSet<Key>) -> Json {
    let mut budget = MAX_TRACE_NODES;
    let mut path = HashSet::new();
    trace_rec(wb, calc, start, depth.min(12), private, derived, &mut budget, &mut path)
}

#[allow(clippy::too_many_arguments)]
fn trace_rec(
    wb: &Workbook,
    calc: &Calc,
    k: Key,
    depth: usize,
    private: &HashSet<Key>,
    derived: &HashSet<Key>,
    budget: &mut usize,
    path: &mut HashSet<Key>,
) -> Json {
    let mut n = node(wb, k.0, k.1, private, derived);
    *budget = budget.saturating_sub(1);
    if depth == 0 || *budget == 0 || !path.insert(k) {
        if n.get("formula").is_some() {
            n["truncated"] = json!(true);
        }
        return n;
    }
    let mut children = vec![];
    for area in calc.graph.precedents_of(k) {
        let sheet_name = wb.sheet(area.sheet).map(|s| s.name.clone()).unwrap_or_default();
        if area.range.is_single() {
            children.push(trace_rec(wb, calc, (area.sheet, area.range.start), depth - 1, private, derived, budget, path));
            continue;
        }
        let mut cells = vec![];
        let mut count = 0usize;
        for c in area.range.iter().take(100_000) {
            let has = wb.sheet(area.sheet).and_then(|s| s.cell(c)).is_some_and(|x| !x.value.is_empty() || x.formula.is_some());
            if !has {
                continue;
            }
            count += 1;
            if cells.len() < MAX_AREA_CELLS && *budget > 0 {
                cells.push(trace_rec(wb, calc, (area.sheet, c), depth - 1, private, derived, budget, path));
            }
        }
        let place = Place { sheet: area.sheet, sheet_name, range: area.range };
        children.push(json!({"range": place.a1(), "label": wb.sheet(area.sheet).map(|s| label_of(s, area.range.start)).unwrap_or_default(), "cells": cells, "filled": count}));
    }
    path.remove(&k);
    if !children.is_empty() {
        n["reads"] = Json::Array(children);
    }
    n
}

/// Input cells (numbers without formulas) a cell depends on, nearest first.
pub fn leaf_inputs(wb: &Workbook, calc: &Calc, start: Key, limit: usize) -> Vec<Key> {
    let mut out = vec![];
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(k) = queue.pop_front() {
        if seen.len() > 5_000 || out.len() >= limit {
            break;
        }
        if !seen.insert(k) {
            continue;
        }
        let cell = wb.sheet(k.0).and_then(|s| s.cell(k.1));
        if k != start && cell.is_some_and(|c| c.formula.is_none() && matches!(c.value, Value::Number(_))) {
            out.push(k);
            continue;
        }
        for a in calc.graph.precedents_of(k) {
            for c in a.range.iter().take(2_000) {
                queue.push_back((a.sheet, c));
            }
        }
    }
    out
}

/// One bar of a tornado chart.
#[derive(Clone, Debug, PartialEq)]
pub struct Swing {
    pub input: Key,
    pub label: String,
    pub base_input: f64,
    pub low_input: f64,
    pub high_input: f64,
    pub at_low: f64,
    pub at_high: f64,
    pub rationale: String,
    /// The range came from the agent (not the default ±10%).
    pub reasoned: bool,
}

fn number(wb: &Workbook, k: Key) -> Option<f64> {
    wb.sheet(k.0).and_then(|s| s.cell(k.1)).and_then(|c| c.value.as_f64())
}

fn set_number(wb: &mut Workbook, k: Key, v: f64) {
    if let Some(sh) = wb.sheets.get_mut(k.0) {
        let style = sh.cell(k.1).map(|c| c.style).unwrap_or_default();
        Arc::make_mut(sh).set_cell(k.1, Cell { value: Value::Number(v), formula: None, style });
    }
}

/// Sensitivity of `target` to the layer's inputs it depends on (or, with none, to its nearest
/// numeric inputs at ±10%): the value at each end of each input's range, the others held.
pub fn sensitivity(wb: &Workbook, calc: &Calc, l: &Layer, target: Key, limit: usize) -> (Option<f64>, Vec<Swing>) {
    let base = number(wb, target);
    let leaves = leaf_inputs(wb, calc, target, 64);
    let mut inputs: Vec<(Key, f64, f64, bool, String, bool)> = vec![];
    for i in &l.inputs {
        let Some(p) = l.anchor(&i.anchor).and_then(|a| anchor::resolve(wb, a)) else { continue };
        let k = (p.sheet, p.range.start);
        if leaves.contains(&k) {
            inputs.push((k, i.low, i.high, i.relative, i.rationale.clone(), true));
        }
    }
    for k in leaves {
        if inputs.len() >= limit.max(1) * 2 {
            break;
        }
        if !inputs.iter().any(|x| x.0 == k) {
            inputs.push((k, 0.9, 1.1, true, String::new(), false));
        }
    }
    let Some(base_v) = base else { return (None, vec![]) };
    let mut scratch = wb.clone();
    let mut c = calc.clone();
    let mut out = vec![];
    for (k, low, high, relative, rationale, reasoned) in inputs {
        let Some(x) = number(&scratch, k) else { continue };
        let (lo, hi) = if relative { (x * low, x * high) } else { (low, high) };
        let mut at = |v: f64| -> f64 {
            set_number(&mut scratch, k, v);
            c.cells_changed(&mut scratch, &[k]);
            number(&scratch, target).unwrap_or(f64::NAN)
        };
        let at_low = at(lo);
        let at_high = at(hi);
        at(x);
        let label = wb.sheet(k.0).map(|s| label_of(s, k.1)).unwrap_or_default();
        out.push(Swing { input: k, label, base_input: x, low_input: lo, high_input: hi, at_low, at_high, rationale, reasoned });
    }
    out.retain(|s| s.at_low.is_finite() && s.at_high.is_finite());
    out.sort_by(|a, b| {
        let sa = (a.at_high - a.at_low).abs();
        let sb = (b.at_high - b.at_low).abs();
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    (Some(base_v), out)
}

pub fn swing_json(wb: &Workbook, s: &Swing, private: &HashSet<Key>) -> Json {
    let place =
        Place { sheet: s.input.0, sheet_name: wb.sheet(s.input.0).map(|x| x.name.clone()).unwrap_or_default(), range: RangeRef::cell(s.input.1) };
    json!({
        "input": place.a1(),
        "label": s.label,
        "inputValue": s.base_input,
        "low": s.low_input,
        "high": s.high_input,
        "outputAtLow": s.at_low,
        "outputAtHigh": s.at_high,
        "rationale": s.rationale,
        "reasoned": s.reasoned,
        "private": private.contains(&s.input),
    })
}
