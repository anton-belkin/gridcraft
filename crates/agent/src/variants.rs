//! Alternatives that live in the document: "A or B?" for a range (two growth assumptions, two
//! formulas), and the choice of calculation method for a cell (a readable formula and an exact
//! one side by side, with where the difference matters).
//!
//! Switching writes the option into the cells with an ordinary, undoable command, so the model
//! recalculates and undo works as for any edit. Comparing never touches the workbook: every
//! option is evaluated on a scratch copy, the way [`crate::analysis::sensitivity`] is.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use gridcraft_engine::calc::{Calc, Key};
use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::{CellRef, RangeRef, Value};
use gridcraft_engine::model::{Cell, Formula, Workbook};
use gridcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::analysis;
use crate::anchor::{self, Place};
use crate::commands::{QUIET, actor_p, bad, masked_p, place_p, spec, str_p, text_p};
use crate::layer::{self, Actor, Layer, VariantKind, VariantOption, VariantSet};

/// Most cells in a range with alternatives.
pub const MAX_CELLS: usize = 400;
/// Most options in a set.
pub const MAX_OPTIONS: usize = 8;
/// Most outputs compared (the ones the choice moves most).
const MAX_OUTPUTS: usize = 6;
/// Most cells walked when looking for the model's final outputs.
const MAX_WALK: usize = 20_000;
/// Threshold of a method when none is given: 1% on an output.
pub const DEFAULT_THRESHOLD: f64 = 0.01;

pub static SPECS: &[CommandSpec] = &[
    spec!(change "agent.variants", "Add Alternatives", "{range, options: [{name, values: value | [[row…]…], note?}], title?, outputs?: [cells], current?: name for the range's present contents (default \"As built\"), actor?} — \"A or B?\" kept in the document", add),
    spec!(change "agent.method", "Add Calculation Method", "{cell, exact: \"=formula\", simple?: \"=formula\" (default: the cell's formula), threshold?: relative difference that matters (default 0.01), outputs?: [cells], note?, title?, actor?}", method),
    spec!(change "agent.useVariant", "Use Alternative", "{id, option: name | index}", use_variant),
    spec!(query "agent.compareVariants", "Compare Alternatives", "{id?, masked?: bool (default true)} → each set with its options, the current one, and the outputs' values under each (material differences flagged)", compare_cmd),
    spec!(change "agent.removeVariants", "Remove Alternatives", "{id}", remove),
];

// ---------------------------------------------------------------- contents

/// A formula's canonical spelling (`=a1*2` → `=A1*2`), or the text as given.
fn canonical_formula(text: &str) -> String {
    let body = text.trim().trim_start_matches('=');
    match gridcraft_engine::formula::parse(body) {
        Ok(e) => format!("={}", gridcraft_engine::formula::print(&e)),
        Err(_) => format!("={body}"),
    }
}

/// What a value given for an option means: numbers (also `"5%"`, `"1,200"`), booleans,
/// `=formulas` (canonical), text.
pub fn normalize(v: &Json) -> Json {
    match v {
        Json::String(s) => {
            let t = s.trim();
            if t.starts_with('=') {
                return json!(canonical_formula(t));
            }
            let plain = t.replace(',', "");
            if let Some(p) = plain.strip_suffix('%').and_then(|x| x.trim().parse::<f64>().ok()).filter(|x| x.is_finite()) {
                return json!(p / 100.0);
            }
            match plain.parse::<f64>() {
                Ok(n) if n.is_finite() && !plain.is_empty() => json!(n),
                _ => json!(s),
            }
        }
        Json::Number(n) => json!(n.as_f64().unwrap_or(0.0)),
        Json::Bool(_) | Json::Null => v.clone(),
        _ => Json::Null,
    }
}

/// A cell's contents as an option holds them.
pub fn cell_input(cell: Option<&Cell>) -> Json {
    let Some(cell) = cell else { return Json::Null };
    if let Some(f) = &cell.formula {
        return json!(canonical_formula(&f.text));
    }
    match &cell.value {
        Value::Number(n) => json!(n),
        Value::Bool(b) => json!(b),
        Value::Text(t) => json!(t.as_ref()),
        _ => Json::Null,
    }
}

fn same(a: &Json, b: &Json) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y || (x - y).abs() <= 1e-12 * x.abs().max(y.abs()),
        _ => a == b,
    }
}

/// The range's present contents, row by row.
pub fn contents(wb: &Workbook, place: &Place) -> Vec<Vec<Json>> {
    let Some(sh) = wb.sheet(place.sheet) else { return vec![] };
    let r = place.range;
    (r.start.row..=r.end.row).map(|row| (r.start.col..=r.end.col).map(|col| cell_input(sh.cell(CellRef::new(row, col)))).collect()).collect()
}

/// The option the cells hold now (`None`: edited since, they match none).
pub fn current_option(wb: &Workbook, place: &Place, set: &VariantSet) -> Option<usize> {
    let now = contents(wb, place);
    set.options.iter().position(|o| {
        o.values.len() == now.len() && o.values.iter().zip(&now).all(|(a, b)| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y)))
    })
}

/// `values` for an option of a `rows`×`cols` range: one value for every cell, or rows.
fn shape(v: &Json, rows: usize, cols: usize) -> std::result::Result<Vec<Vec<Json>>, String> {
    match v {
        Json::Array(list) if list.iter().all(Json::is_array) => {
            let out: Vec<Vec<Json>> = list.iter().map(|r| r.as_array().map(|r| r.iter().map(normalize).collect()).unwrap_or_default()).collect();
            if out.len() != rows || out.iter().any(|r| r.len() != cols) {
                return Err(format!("`values` must be {rows} row(s) of {cols} value(s), like the range"));
            }
            Ok(out)
        }
        // A flat list fills a single row or a single column.
        Json::Array(list) if rows == 1 && list.len() == cols => Ok(vec![list.iter().map(normalize).collect()]),
        Json::Array(list) if cols == 1 && list.len() == rows => Ok(list.iter().map(|x| vec![normalize(x)]).collect()),
        Json::Array(_) => Err(format!("`values` must be {rows} row(s) of {cols} value(s), like the range")),
        Json::Object(_) => Err("`values` must be a value or rows of values".into()),
        one => Ok(vec![vec![normalize(one); cols]; rows]),
    }
}

// ---------------------------------------------------------------- evaluation on a scratch copy

/// Writes an option into a scratch workbook and recalculates what depends on it.
pub fn apply(wb: &mut Workbook, calc: &mut Calc, place: &Place, values: &[Vec<Json>]) {
    let Some(sh) = wb.sheets.get_mut(place.sheet) else { return };
    let sh = Arc::make_mut(sh);
    let mut keys = vec![];
    for (ri, row) in values.iter().enumerate() {
        for (ci, v) in row.iter().enumerate() {
            let Some(c) = place.range.start.offset(ri as i64, ci as i64).filter(|c| place.range.contains(*c)) else { continue };
            let style = sh.cell(c).map(|x| x.style).unwrap_or_default();
            let cell = match v {
                Json::Number(n) => Some(Cell { value: Value::Number(n.as_f64().unwrap_or(0.0)), formula: None, style }),
                Json::Bool(b) => Some(Cell { value: Value::Bool(*b), formula: None, style }),
                Json::String(s) if s.starts_with('=') => match gridcraft_engine::formula::parse(s.trim_start_matches('=')) {
                    Ok(e) => Some(Cell { value: Value::Empty, formula: Some(Arc::new(Formula::from_expr(e))), style }),
                    Err(_) => Some(Cell { value: Value::Error(gridcraft_engine::core::CellError::Name), formula: None, style }),
                },
                Json::String(s) => Some(Cell { value: Value::text(s.as_str()), formula: None, style }),
                _ => None,
            };
            match cell {
                Some(x) => sh.set_cell(c, x),
                None => {
                    sh.remove_cell(c);
                }
            }
            keys.push((place.sheet, c));
        }
    }
    calc.cells_changed(wb, &keys);
}

fn number(wb: &Workbook, k: Key) -> Option<f64> {
    wb.sheet(k.0).and_then(|s| s.cell(k.1)).and_then(|c| c.value.as_f64())
}

/// The results downstream of a range, the furthest first (EBITDA before the revenue it is made
/// of): numbers computed from the range, outside it.
pub fn final_outputs(wb: &Workbook, calc: &Calc, place: &Place, limit: usize) -> Vec<Key> {
    let inside = |x: &Key| x.0 == place.sheet && place.range.contains(x.1);
    let mut seen: HashSet<Key> = HashSet::new();
    let mut queue: VecDeque<(Key, usize)> = place.range.iter().take(MAX_CELLS).map(|c| ((place.sheet, c), 0)).collect();
    let mut found: Vec<(Key, usize)> = vec![];
    let mut buf = Vec::new();
    while let Some((k, depth)) = queue.pop_front() {
        if seen.len() >= MAX_WALK {
            break;
        }
        if !inside(&k) && number(wb, k).is_some() {
            found.push((k, depth));
        }
        buf.clear();
        calc.graph.dependents(k.0, k.1, &mut buf);
        for x in buf.drain(..) {
            if !inside(&x) && seen.insert(x) {
                queue.push_back((x, depth + 1));
            }
        }
    }
    // Furthest first; among equals, in the order they were reached.
    found.reverse();
    found.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
    found.into_iter().take(limit).map(|(k, _)| k).collect()
}

/// One output under every option.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputRow {
    pub key: Key,
    pub label: String,
    pub values: Vec<Option<f64>>,
    /// Spread across the options relative to the current (or first) option's value.
    pub rel_diff: f64,
    pub material: bool,
}

/// A set compared: the options' outcomes.
#[derive(Clone, Debug, PartialEq)]
pub struct Comparison {
    pub id: String,
    pub kind: VariantKind,
    pub title: String,
    pub place: Place,
    pub names: Vec<String>,
    pub notes: Vec<String>,
    pub current: Option<usize>,
    pub threshold: Option<f64>,
    /// The cell itself under each option (a method's own value), then the outputs.
    pub rows: Vec<OutputRow>,
}

impl Comparison {
    /// Some output moves more than the threshold.
    pub fn material(&self) -> bool {
        self.rows.iter().any(|r| r.material)
    }
    pub fn max_rel_diff(&self) -> f64 {
        self.rows.iter().map(|r| r.rel_diff).fold(0.0, f64::max)
    }
}

fn rel_spread(values: &[Option<f64>], base: Option<f64>) -> f64 {
    let nums: Vec<f64> = values.iter().flatten().copied().filter(|v| v.is_finite()).collect();
    if nums.len() < 2 {
        return 0.0;
    }
    let lo = nums.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = nums.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let scale = base.filter(|b| b.is_finite() && *b != 0.0).map(f64::abs).unwrap_or_else(|| lo.abs().max(hi.abs()));
    if scale == 0.0 { if hi == lo { 0.0 } else { f64::INFINITY } } else { (hi - lo) / scale }
}

/// Every option of `set` evaluated on a scratch copy of `wb`: the cell and the outputs the
/// choice moves most.
pub fn compare(wb: &Workbook, calc: &Calc, l: &Layer, set: &VariantSet) -> Option<Comparison> {
    let place = l.anchor(&set.anchor).and_then(|a| anchor::resolve(wb, a))?;
    let current = current_option(wb, &place, set);
    let mut outputs: Vec<Key> = set
        .outputs
        .iter()
        .filter_map(|id| l.anchor(id).and_then(|a| anchor::resolve(wb, a)))
        .map(|p| (p.sheet, p.range.start))
        .take(MAX_OUTPUTS * 4)
        .collect();
    let explicit = !outputs.is_empty();
    if !explicit {
        outputs = final_outputs(wb, calc, &place, 48);
    }
    // A method's own cell is compared first.
    let own: Vec<Key> = if place.range.count() <= 4 { place.range.iter().map(|c| (place.sheet, c)).collect() } else { vec![] };
    let mut keys: Vec<Key> = own.iter().copied().filter(|k| number(wb, *k).is_some() || set.kind == VariantKind::Method).collect();
    for k in outputs {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut scratch = wb.clone();
    let mut c = calc.clone();
    let mut table: Vec<Vec<Option<f64>>> = vec![vec![]; keys.len()];
    for o in &set.options {
        apply(&mut scratch, &mut c, &place, &o.values);
        for (i, k) in keys.iter().enumerate() {
            if let Some(row) = table.get_mut(i) {
                row.push(number(&scratch, *k));
            }
        }
    }
    let threshold = match set.kind {
        VariantKind::Method => Some(set.threshold.unwrap_or(DEFAULT_THRESHOLD)),
        VariantKind::Alternatives => set.threshold,
    };
    let base_of = |vals: &[Option<f64>]| current.and_then(|i| vals.get(i).copied().flatten()).or_else(|| vals.first().copied().flatten());
    let mut rows: Vec<OutputRow> = keys
        .iter()
        .zip(table)
        .map(|(k, values)| {
            let rel_diff = rel_spread(&values, base_of(&values));
            let label = wb.sheet(k.0).map(|s| analysis::label_of(s, k.1)).unwrap_or_default();
            OutputRow { key: *k, label, material: threshold.is_some_and(|t| rel_diff > t), rel_diff, values }
        })
        .collect();
    // Keep the cell itself, then the outputs the choice moves most.
    let n_own = keys.iter().filter(|k| own.contains(k)).count();
    let mut tail = rows.split_off(n_own.min(rows.len()));
    if !explicit {
        // The furthest results the choice actually moves.
        tail.retain(|r| r.values.iter().any(Option::is_some) && r.rel_diff > 0.0);
    }
    tail.truncate(MAX_OUTPUTS);
    rows.extend(tail);
    Some(Comparison {
        id: set.id.clone(),
        kind: set.kind,
        title: set.title.clone(),
        place,
        names: set.options.iter().map(|o| o.name.clone()).collect(),
        notes: set.options.iter().map(|o| o.note.clone()).collect(),
        current,
        threshold,
        rows,
    })
}

pub fn comparison_json(wb: &Workbook, c: &Comparison) -> Json {
    let cell_a1 =
        |k: Key| Place { sheet: k.0, sheet_name: wb.sheet(k.0).map(|s| s.name.clone()).unwrap_or_default(), range: RangeRef::cell(k.1) }.a1();
    json!({
        "id": c.id,
        "kind": c.kind,
        "title": c.title,
        "range": c.place.a1(),
        "current": c.current.and_then(|i| c.names.get(i)),
        "options": c.names.iter().zip(&c.notes).map(|(n, note)| json!({"name": n, "note": note})).collect::<Vec<_>>(),
        "threshold": c.threshold,
        "material": c.material(),
        "outputs": c.rows.iter().map(|r| json!({
            "cell": cell_a1(r.key),
            "label": r.label,
            "values": r.values,
            "relDiff": if r.rel_diff.is_finite() { json!(r.rel_diff) } else { json!("inf") },
            "material": r.material,
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------- commands

fn refuse_private(s: &Session, place: &Place, cmd: &str) -> Result<()> {
    let d = s.doc()?;
    let l = layer::get(d);
    let private = analysis::private_cells(&d.wb, &l);
    if place.range.iter().take(MAX_CELLS).any(|c| private.contains(&(place.sheet, c))) {
        return Err(bad(cmd, format!("{} holds client data: alternatives would keep its real values in the layer", place.a1())));
    }
    Ok(())
}

fn outputs_p(s: &mut Session, p: &Json, cmd: &str) -> Result<Vec<String>> {
    let Some(list) = p.get("outputs").and_then(Json::as_array) else { return Ok(vec![]) };
    let mut out = vec![];
    for v in list.iter().take(MAX_OUTPUTS * 4) {
        let Some(t) = v.as_str() else { continue };
        let d = s.doc()?;
        let place = anchor::parse_place(&d.wb, t, d.wb.active_sheet).ok_or_else(|| bad(cmd, format!("not a cell: `{t}`")))?;
        let place = Place { range: RangeRef::cell(place.range.start), ..place };
        out.push(anchor::anchor_at(s, &place)?);
    }
    Ok(out)
}

fn label(s: &str) -> String {
    s.trim().chars().take(120).collect()
}

fn add(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.variants";
    let place = place_p(s, p, CMD)?;
    let (rows, cols) = (place.range.height() as usize, place.range.width() as usize);
    if rows.saturating_mul(cols) > MAX_CELLS {
        return Err(bad(CMD, format!("a range with alternatives has at most {MAX_CELLS} cells")));
    }
    refuse_private(s, &place, CMD)?;
    let list = p.get("options").and_then(Json::as_array).ok_or_else(|| bad(CMD, "missing `options` ([{name, values}])"))?;
    if list.is_empty() || list.len() > MAX_OPTIONS {
        return Err(bad(CMD, format!("give 1 to {MAX_OPTIONS} options")));
    }
    let mut options = vec![];
    for (i, o) in list.iter().enumerate() {
        let name = str_p(o, "name").map(label).filter(|n| !n.is_empty()).unwrap_or_else(|| ((b'A' + i as u8) as char).to_string());
        let values = o.get("values").or_else(|| o.get("value")).ok_or_else(|| bad(CMD, format!("option `{name}` has no `values`")))?;
        let values = shape(values, rows, cols).map_err(|m| bad(CMD, format!("option `{name}`: {m}")))?;
        let note: String = str_p(o, "note").unwrap_or("").chars().take(4_000).collect();
        options.push(VariantOption { name, values, note });
    }
    let d = s.doc()?;
    let now = contents(&d.wb, &place);
    let keeps_current = options.iter().any(|o| o.values.iter().zip(&now).all(|(a, b)| a.iter().zip(b).all(|(x, y)| same(x, y))));
    if !keeps_current && p.get("current") != Some(&Json::Bool(false)) && options.len() < MAX_OPTIONS {
        let name = str_p(p, "current").map(label).filter(|n| !n.is_empty()).unwrap_or_else(|| "As built".into());
        options.insert(0, VariantOption { name, values: now, note: String::new() });
    }
    let outputs = outputs_p(s, p, CMD)?;
    let a = anchor::anchor_at(s, &place)?;
    let title = str_p(p, "title").map(label).unwrap_or_default();
    let actor = actor_p(p, Actor::Human);
    let names: Vec<String> = options.iter().map(|o| o.name.clone()).collect();
    let id = layer::update(s.doc()?, |l| {
        let id = l.fresh_id("v");
        let what = if title.is_empty() { place.a1() } else { format!("“{title}”") };
        l.log(actor, "decision", format!("Alternatives for {what}: {}", names.join(" / ")), CMD, Some(a.clone()), String::new());
        l.variants.push(VariantSet {
            id: id.clone(),
            anchor: a,
            kind: VariantKind::Alternatives,
            title,
            options,
            outputs,
            threshold: None,
            author: actor,
            at: crate::now_ms(),
        });
        id
    });
    Ok(json!({"id": id, "range": place.a1(), "options": names}))
}

fn method(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.method";
    let place = place_p(s, p, CMD)?;
    let place = Place { range: RangeRef::cell(place.range.start), ..place };
    refuse_private(s, &place, CMD)?;
    let exact = normalize(&json!(text_p(p, "exact", CMD)?));
    let d = s.doc()?;
    let simple = match str_p(p, "simple") {
        Some(t) if !t.trim().is_empty() => normalize(&json!(t)),
        _ => contents(&d.wb, &place).first().and_then(|r| r.first()).cloned().unwrap_or(Json::Null),
    };
    if simple.is_null() {
        return Err(bad(CMD, "the cell is empty: give `simple` too"));
    }
    if same(&simple, &exact) {
        return Err(bad(CMD, "`simple` and `exact` are the same"));
    }
    let threshold = p.get("threshold").and_then(Json::as_f64).filter(|t| t.is_finite()).unwrap_or(DEFAULT_THRESHOLD).clamp(0.0, 100.0);
    let note: String = str_p(p, "note").unwrap_or("").chars().take(4_000).collect();
    let outputs = outputs_p(s, p, CMD)?;
    let a = anchor::anchor_at(s, &place)?;
    let title = str_p(p, "title").map(label).unwrap_or_default();
    let actor = actor_p(p, Actor::Human);
    let id = layer::update(s.doc()?, |l| {
        // One method per cell: a new one replaces the old.
        l.variants.retain(|v| !(v.kind == VariantKind::Method && v.anchor == a));
        let id = l.fresh_id("m");
        l.log(actor, "decision", format!("Exact method for {}: {}", place.a1(), exact.as_str().unwrap_or("")), CMD, Some(a.clone()), String::new());
        l.variants.push(VariantSet {
            id: id.clone(),
            anchor: a,
            kind: VariantKind::Method,
            title,
            options: vec![
                VariantOption { name: "Simple".into(), values: vec![vec![simple]], note: String::new() },
                VariantOption { name: "Exact".into(), values: vec![vec![exact]], note },
            ],
            outputs,
            threshold: Some(threshold),
            author: actor,
            at: crate::now_ms(),
        });
        id
    });
    Ok(json!({"id": id, "cell": place.a1(), "threshold": threshold}))
}

fn find_set(s: &Session, p: &Json, cmd: &str) -> Result<(Arc<Layer>, VariantSet)> {
    let id = text_p(p, "id", cmd)?;
    let l = layer::get(s.doc()?);
    let set = l.variants.iter().find(|v| v.id == id).cloned().ok_or_else(|| bad(cmd, format!("no alternatives `{id}`")))?;
    Ok((l, set))
}

fn use_variant(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.useVariant";
    let (l, set) = find_set(s, p, CMD)?;
    let i = match p.get("option") {
        Some(Json::Number(n)) => n.as_u64().map(|n| n as usize),
        Some(Json::String(name)) => {
            set.options.iter().position(|o| o.name.eq_ignore_ascii_case(name.trim())).or_else(|| name.trim().parse::<usize>().ok())
        }
        _ => None,
    }
    .filter(|i| *i < set.options.len())
    .ok_or_else(|| bad(CMD, format!("`option` must be one of: {}", set.options.iter().map(|o| o.name.as_str()).collect::<Vec<_>>().join(", "))))?;
    let d = s.doc()?;
    let place = l.anchor(&set.anchor).and_then(|a| anchor::resolve(&d.wb, a)).ok_or_else(|| bad(CMD, "the range is gone"))?;
    let actor = actor_p(p, Actor::Human);
    if actor == Actor::Agent {
        refuse_private(s, &place, CMD)?;
    }
    let Some(o) = set.options.get(i) else { return Err(bad(CMD, "no such option")) };
    let values: Vec<Json> = o.values.iter().map(|r| Json::Array(r.clone())).collect();
    QUIET.with(|q| q.set(q.get().saturating_add(1)));
    let r = s.execute("range.setValues", json!({"sheet": place.sheet, "range": place.range.a1(), "values": values}));
    QUIET.with(|q| q.set(q.get().saturating_sub(1)));
    r?;
    let name = o.name.clone();
    let what = if set.title.is_empty() { place.a1() } else { format!("“{}”", set.title) };
    layer::update(s.doc()?, |l| l.log(actor, "change", format!("{what}: switched to {name}"), CMD, Some(set.anchor.clone()), String::new()));
    Ok(json!({"id": set.id, "option": name, "range": place.a1()}))
}

fn compare_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let l = layer::get(d);
    let want = str_p(p, "id");
    let sets: Vec<&VariantSet> = l.variants.iter().filter(|v| want.is_none_or(|w| v.id == w)).collect();
    if let Some(w) = want
        && sets.is_empty()
    {
        return Err(bad("agent.compareVariants", format!("no alternatives `{w}`")));
    }
    if sets.is_empty() {
        return Ok(json!([]));
    }
    let (wb, calc) = if masked_p(p) { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    let list: Vec<Json> = sets.iter().filter_map(|set| compare(&wb, &calc, &l, set)).map(|c| comparison_json(&wb, &c)).collect();
    Ok(Json::Array(list))
}

fn remove(s: &mut Session, p: &Json) -> Result<Json> {
    let id = text_p(p, "id", "agent.removeVariants")?;
    let ok = layer::update(s.doc()?, |l| {
        let n = l.variants.len();
        l.variants.retain(|v| v.id != id);
        n != l.variants.len()
    });
    if ok { Ok(json!({"id": id})) } else { Err(bad("agent.removeVariants", format!("no alternatives `{id}`"))) }
}

/// Sets whose range touches `range` on `sheet` (for the pane and the overlay).
pub fn sets_at<'a>(wb: &Workbook, l: &'a Layer, sheet: usize, range: &RangeRef) -> Vec<&'a VariantSet> {
    l.variants
        .iter()
        .filter(|v| l.anchor(&v.anchor).and_then(|a| anchor::resolve(wb, a)).is_some_and(|p| p.sheet == sheet && p.range.intersects(range)))
        .collect()
}
