//! Multi-dimensional analysis: several inputs moved together within their reasoned ranges, to
//! find the combinations that give an output its lowest and highest value.
//!
//! The search is deterministic and bounded: every corner of the box when there are few enough
//! (else a fixed sample of them), a fixed sample of points inside, then a coordinate search from
//! the best points found. All of it on a scratch copy, like [`analysis::sensitivity`]; the work
//! is capped by the number of evaluations and by the size of the model under the inputs.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use gridcraft_engine::calc::{Calc, Key};
use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::{RangeRef, Value};
use gridcraft_engine::model::{Cell, Workbook};
use gridcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::analysis;
use crate::anchor::{self, Place};
use crate::commands::{bad, masked_p, place_p, spec};
use crate::layer::{self, Layer};

/// Most inputs moved together.
pub const MAX_DIMS: usize = 12;
/// Evaluations when none are asked for, and the most that can be.
pub const DEFAULT_EVALS: usize = 3_000;
pub const MAX_EVALS: usize = 20_000;
/// Cells recalculated in one search, at most (evaluations × cells under the inputs).
const WORK_CAP: usize = 6_000_000;

pub static SPECS: &[CommandSpec] = &[
    spec!(query "agent.stress", "Worst and Best Combination", "{cell?, inputs?: [cells] (default: the marked inputs it depends on, 3-12), maxEvals?: (default 3000, max 20000), masked?: bool (default true)} → the lowest and highest value of the output with every input inside its range, and the setting of each input there", stress_cmd),
];

/// One input of the search.
#[derive(Clone, Debug, PartialEq)]
pub struct Dim {
    pub key: Key,
    pub label: String,
    pub base: f64,
    pub low: f64,
    pub high: f64,
    /// The range came from the agent (not the default ±10%).
    pub reasoned: bool,
}

/// An extreme found: the output and where each input was.
#[derive(Clone, Debug, PartialEq)]
pub struct Extreme {
    pub value: f64,
    pub setting: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stress {
    pub target: Key,
    pub base: f64,
    pub dims: Vec<Dim>,
    pub lowest: Extreme,
    pub highest: Extreme,
    /// What moving each input alone to its worse end adds up to (no interaction).
    pub additive_low: f64,
    pub additive_high: f64,
    pub evaluations: usize,
    /// Corners tried, and whether that was all of them.
    pub corners: usize,
    pub all_corners: bool,
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

/// The inputs to move: those asked for, else the marked inputs the output depends on (padded
/// with its nearest other inputs at ±10% to at least three), the most influential first.
pub fn pick_dims(wb: &Workbook, calc: &Calc, l: &Layer, target: Key, chosen: &[Key]) -> Vec<Dim> {
    let (_, swings) = analysis::sensitivity(wb, calc, l, target, MAX_DIMS * 2);
    let to_dim = |w: &analysis::Swing| Dim {
        key: w.input,
        label: w.label.clone(),
        base: w.base_input,
        low: w.low_input.min(w.high_input),
        high: w.low_input.max(w.high_input),
        reasoned: w.reasoned,
    };
    let mut dims: Vec<Dim> = if chosen.is_empty() {
        let mut d: Vec<Dim> = swings.iter().filter(|w| w.reasoned).map(to_dim).collect();
        for w in swings.iter().filter(|w| !w.reasoned) {
            if d.len() >= 3 {
                break;
            }
            d.push(to_dim(w));
        }
        d
    } else {
        chosen
            .iter()
            .filter_map(|k| {
                swings.iter().find(|w| w.input == *k).map(to_dim).or_else(|| {
                    let x = number(wb, *k)?;
                    let label = wb.sheet(k.0).map(|s| analysis::label_of(s, k.1)).unwrap_or_default();
                    Some(Dim { key: *k, label, base: x, low: (x * 0.9).min(x * 1.1), high: (x * 0.9).max(x * 1.1), reasoned: false })
                })
            })
            .collect()
    };
    dims.retain(|d| d.low.is_finite() && d.high.is_finite());
    let mut seen = HashSet::new();
    dims.retain(|d| seen.insert(d.key));
    dims.truncate(MAX_DIMS);
    dims
}

/// Formula cells under the inputs (what one evaluation recalculates, at most).
fn downstream(calc: &Calc, keys: &[Key]) -> usize {
    let mut seen: HashSet<Key> = HashSet::new();
    let mut queue: VecDeque<Key> = keys.iter().copied().collect();
    let mut buf = Vec::new();
    while let Some(k) = queue.pop_front() {
        if seen.len() >= 50_000 {
            break;
        }
        buf.clear();
        calc.graph.dependents(k.0, k.1, &mut buf);
        for x in buf.drain(..) {
            if seen.insert(x) {
                queue.push_back(x);
            }
        }
    }
    seen.len()
}

/// A deterministic stream of numbers in [0, 1).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The scratch model the search moves.
struct Probe<'a> {
    wb: Workbook,
    calc: Calc,
    dims: &'a [Dim],
    target: Key,
    now: Vec<f64>,
    evals: usize,
    budget: usize,
}

impl Probe<'_> {
    fn left(&self) -> bool {
        self.evals < self.budget
    }
    fn eval(&mut self, x: &[f64]) -> f64 {
        let mut changed = vec![];
        for (i, d) in self.dims.iter().enumerate() {
            let v = x.get(i).copied().unwrap_or(d.base);
            if self.now.get(i) != Some(&v) {
                set_number(&mut self.wb, d.key, v);
                if let Some(slot) = self.now.get_mut(i) {
                    *slot = v;
                }
                changed.push(d.key);
            }
        }
        if !changed.is_empty() {
            self.calc.cells_changed(&mut self.wb, &changed);
        }
        self.evals += 1;
        number(&self.wb, self.target).unwrap_or(f64::NAN)
    }
}

fn at(d: &Dim, f: f64) -> f64 {
    d.low + (d.high - d.low) * f
}

/// Coordinate search: one input at a time over a grid of its range, keeping what improves.
fn refine(p: &mut Probe, start: &Extreme, lower: bool) -> Extreme {
    let mut best = start.clone();
    let better = |v: f64, b: f64| v.is_finite() && if lower { v < b } else { v > b };
    for _pass in 0..4 {
        let mut improved = false;
        for i in 0..p.dims.len() {
            let Some(d) = p.dims.get(i).cloned() else { continue };
            for f in [0.0, 0.25, 0.5, 0.75, 1.0] {
                if !p.left() {
                    return best;
                }
                let mut x = best.setting.clone();
                let v = at(&d, f);
                if x.get(i) == Some(&v) {
                    continue;
                }
                if let Some(slot) = x.get_mut(i) {
                    *slot = v;
                }
                let y = p.eval(&x);
                if better(y, best.value) {
                    best = Extreme { value: y, setting: x };
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
    best
}

/// The lowest and highest value of `target` with every input of `dims` inside its range.
pub fn search(wb: &Workbook, calc: &Calc, target: Key, dims: Vec<Dim>, max_evals: usize) -> Option<Stress> {
    let base = number(wb, target)?;
    if dims.is_empty() {
        return None;
    }
    let n = dims.len();
    let keys: Vec<Key> = dims.iter().map(|d| d.key).collect();
    let per_eval = downstream(calc, &keys).max(1);
    let budget = max_evals.clamp(1, MAX_EVALS).min(WORK_CAP / per_eval).max(2 * n + 2);
    let base_x: Vec<f64> = dims.iter().map(|d| d.base).collect();
    let mut p = Probe { wb: wb.clone(), calc: calc.clone(), dims: &dims, target, now: base_x.clone(), evals: 0, budget };
    let mut lowest = Extreme { value: base, setting: base_x.clone() };
    let mut highest = lowest.clone();
    let consider = |x: Vec<f64>, y: f64, lowest: &mut Extreme, highest: &mut Extreme| {
        if !y.is_finite() {
            return;
        }
        if y < lowest.value {
            *lowest = Extreme { value: y, setting: x.clone() };
        }
        if y > highest.value {
            *highest = Extreme { value: y, setting: x };
        }
    };
    // One input at a time: the no-interaction estimate.
    let (mut add_low, mut add_high) = (base, base);
    for (i, d) in dims.iter().enumerate() {
        let mut lo_hi = vec![];
        for v in [d.low, d.high] {
            let mut x = base_x.clone();
            if let Some(slot) = x.get_mut(i) {
                *slot = v;
            }
            let y = p.eval(&x);
            consider(x, y, &mut lowest, &mut highest);
            lo_hi.push(y);
        }
        let moves: Vec<f64> = lo_hi.iter().filter(|y| y.is_finite()).map(|y| y - base).collect();
        add_low += moves.iter().copied().fold(0.0, f64::min);
        add_high += moves.iter().copied().fold(0.0, f64::max);
    }
    // Corners: all of them in Gray-code order (one input changes per step), or a fixed sample.
    let total: usize = 1usize << n.min(20);
    let corner_budget = (p.budget.saturating_sub(p.evals)) * 6 / 10;
    let all_corners = total <= corner_budget;
    let mut rng = Rng(0x5EED ^ (target.0 as u64) << 40 ^ u64::from(target.1.row) << 20 ^ u64::from(target.1.col));
    let corner = |bits: usize| -> Vec<f64> { dims.iter().enumerate().map(|(i, d)| if bits >> i & 1 == 1 { d.high } else { d.low }).collect() };
    let mut corners = 0;
    if all_corners {
        for g in 0..total {
            let x = corner(g ^ (g >> 1));
            let y = p.eval(&x);
            consider(x, y, &mut lowest, &mut highest);
            corners += 1;
        }
    } else {
        for _ in 0..corner_budget {
            let x = corner(rng.next() as usize);
            let y = p.eval(&x);
            consider(x, y, &mut lowest, &mut highest);
            corners += 1;
        }
    }
    // Points inside the box, for outputs that peak between the ends of a range.
    let inside = (p.budget.saturating_sub(p.evals)) / 4;
    for _ in 0..inside {
        let x: Vec<f64> = dims.iter().map(|d| at(d, rng.unit())).collect();
        let y = p.eval(&x);
        consider(x, y, &mut lowest, &mut highest);
    }
    // Polish both ends.
    let l2 = refine(&mut p, &lowest, true);
    let h2 = refine(&mut p, &highest, false);
    let evaluations = p.evals;
    Some(Stress { target, base, dims, lowest: l2, highest: h2, additive_low: add_low, additive_high: add_high, evaluations, corners, all_corners })
}

/// Where an input sits in its range: `low`, `high`, `base` or `inside`.
pub fn position(d: &Dim, v: f64) -> &'static str {
    let tol = (d.high - d.low).abs() * 1e-9;
    if (v - d.low).abs() <= tol {
        "low"
    } else if (v - d.high).abs() <= tol {
        "high"
    } else if (v - d.base).abs() <= tol {
        "base"
    } else {
        "inside"
    }
}

fn cell_a1(wb: &Workbook, k: Key) -> String {
    Place { sheet: k.0, sheet_name: wb.sheet(k.0).map(|s| s.name.clone()).unwrap_or_default(), range: RangeRef::cell(k.1) }.a1()
}

pub fn stress_json(wb: &Workbook, s: &Stress) -> Json {
    let extreme = |e: &Extreme| {
        json!({
            "value": e.value,
            "inputs": s.dims.iter().zip(&e.setting).map(|(d, v)| json!({"cell": cell_a1(wb, d.key), "label": d.label, "value": v, "at": position(d, *v)})).collect::<Vec<_>>(),
        })
    };
    json!({
        "cell": cell_a1(wb, s.target),
        "label": wb.sheet(s.target.0).map(|x| analysis::label_of(x, s.target.1)).unwrap_or_default(),
        "value": s.base,
        "inputs": s.dims.iter().map(|d| json!({"cell": cell_a1(wb, d.key), "label": d.label, "value": d.base, "low": d.low, "high": d.high, "reasoned": d.reasoned})).collect::<Vec<_>>(),
        "lowest": extreme(&s.lowest),
        "highest": extreme(&s.highest),
        "oneAtATime": {"lowest": s.additive_low, "highest": s.additive_high, "note": "the sum of each input's own move to its worse end: the gap to lowest/highest is what the inputs do together"},
        "evaluations": s.evaluations,
        "corners": s.corners,
        "allCorners": s.all_corners,
    })
}

fn view(s: &Session, p: &Json) -> Result<(Workbook, Calc, std::sync::Arc<Layer>)> {
    let d = s.doc()?;
    let l = layer::get(d);
    let (wb, calc) = if masked_p(p) { analysis::masked(d, &l) } else { ((*d.wb).clone(), d.calc.clone()) };
    Ok((wb, calc, l))
}

fn stress_cmd(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.stress";
    let place = place_p(s, p, CMD)?;
    let target: Key = (place.sheet, place.range.start);
    let d = s.doc()?;
    let mut chosen = vec![];
    for v in p.get("inputs").and_then(Json::as_array).into_iter().flatten().take(MAX_DIMS * 2) {
        let Some(t) = v.as_str() else { continue };
        let pl = anchor::parse_place(&d.wb, t, d.wb.active_sheet).ok_or_else(|| bad(CMD, format!("not a cell: `{t}`")))?;
        chosen.push((pl.sheet, pl.range.start));
    }
    let max_evals = p.get("maxEvals").and_then(Json::as_u64).map_or(DEFAULT_EVALS, |n| n.min(MAX_EVALS as u64) as usize);
    let (wb, calc, l) = view(s, p)?;
    if number(&wb, target).is_none() {
        return Err(bad(CMD, format!("{} is not a number", place.a1())));
    }
    let dims = pick_dims(&wb, &calc, &l, target, &chosen);
    let r = search(&wb, &calc, target, dims, max_evals).ok_or_else(|| bad(CMD, format!("{} has no numeric inputs to move", place.a1())))?;
    Ok(stress_json(&wb, &r))
}
