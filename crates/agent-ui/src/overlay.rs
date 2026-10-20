//! What the agent layer paints over the cells, and the hover card with a number's sensitivity.

use std::collections::HashMap;

use egui::{Align2, Color32, Painter, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use gridcraft_agent::analysis::{self, Swing};
use gridcraft_agent::anchor;
use gridcraft_agent::layer::{self, Layer, QuestionStatus, VariantKind};
use gridcraft_agent::links;
use gridcraft_agent::stress::{self, Stress};
use gridcraft_agent::textlinks;
use gridcraft_agent::topics;
use gridcraft_agent::variants::{self, Comparison};
use gridcraft_engine::DocState;
use gridcraft_engine::core::{CellRef, RangeRef, Value};
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::ext::CellGeometry;
use gridcraft_ui_egui::theme::{self, Tokens};

use crate::{AgentUi, Heat, Palette, fmt_num, palette};

/// How long the pointer rests on a cell before its card shows.
const HOVER_DELAY: f64 = 0.45;

/// The analysis behind a hover card or a heat map, cached until the workbook or layer changes.
#[derive(Clone)]
pub(crate) struct Sens {
    pub base: Option<f64>,
    pub swings: Vec<Swing>,
}

#[derive(Default)]
pub(crate) struct State {
    hover: Option<(usize, CellRef, f64)>,
    cache: HashMap<(u64, usize, u64, usize, CellRef), Sens>,
    comparisons: Option<((u64, usize, u64), Vec<Comparison>)>,
    stress: Option<((u64, usize, u64), usize, CellRef, Option<Stress>)>,
    /// Topic markers painted last frame: (where, topic, place anchor), for clicks.
    pub markers: Vec<(Rect, String, String)>,
}

/// Version of what an analysis depends on: the document, its workbook and its layer.
pub(crate) fn version(d: &DocState, l: &Layer) -> (u64, usize, u64) {
    (d.uid, std::sync::Arc::as_ptr(&d.wb) as usize, l.revision)
}

impl State {
    pub(crate) fn sensitivity(&mut self, d: &DocState, l: &Layer, sheet: usize, c: CellRef) -> Sens {
        let (uid, wb, rev) = version(d, l);
        let key = (uid, wb, rev, sheet, c);
        if let Some(s) = self.cache.get(&key) {
            return s.clone();
        }
        if self.cache.len() > 256 {
            self.cache.clear();
        }
        let (base, swings) = analysis::sensitivity(&d.wb, &d.calc, l, (sheet, c), 6);
        let s = Sens { base, swings };
        self.cache.insert(key, s.clone());
        s
    }

    /// The worst and best combination of the inputs of a cell, cached until something changes.
    pub(crate) fn stress(&mut self, d: &DocState, l: &Layer, sheet: usize, c: CellRef) -> Option<Stress> {
        let ver = version(d, l);
        if let Some((v, s, k, r)) = &self.stress
            && *v == ver
            && *s == sheet
            && *k == c
        {
            return r.clone();
        }
        let dims = stress::pick_dims(&d.wb, &d.calc, l, (sheet, c), &[]);
        let r = stress::search(&d.wb, &d.calc, (sheet, c), dims, 2_000);
        self.stress = Some((ver, sheet, c, r.clone()));
        r
    }

    /// Every set of alternatives and calculation methods compared, cached until something changes.
    pub(crate) fn comparisons(&mut self, d: &DocState, l: &Layer) -> Vec<Comparison> {
        let ver = version(d, l);
        if let Some((v, list)) = &self.comparisons
            && *v == ver
        {
            return list.clone();
        }
        let list: Vec<Comparison> = l.variants.iter().filter_map(|set| variants::compare(&d.wb, &d.calc, l, set)).collect();
        self.comparisons = Some((ver, list.clone()));
        list
    }
}

/// A place of the topic holds this cell.
pub(crate) fn topic_has_cell(d: &DocState, l: &Layer, t: &layer::Topic, sheet: usize, c: CellRef) -> bool {
    t.places.iter().any(|p| l.anchor(&p.anchor).and_then(|a| anchor::resolve(&d.wb, a)).is_some_and(|x| x.sheet == sheet && x.range.contains(c)))
}

pub(crate) fn is_formula_number(d: &DocState, sheet: usize, c: CellRef) -> bool {
    d.wb.sheet(sheet).and_then(|s| s.cell(c)).is_some_and(|x| x.formula.is_some() && matches!(x.value, Value::Number(_)))
}

pub(crate) fn paint(st: &mut AgentUi, app: &SheetApp, ui: &Ui, painter: &Painter, cells: &CellGeometry) {
    let Some(d) = app.session.active() else { return };
    let l = layer::get(d);
    let t = Tokens::get(ui.ctx());
    let p = palette(&t);
    let si = cells.sheet_index;
    let on_sheet = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a)).filter(|pl| pl.sheet == si);

    // Heat maps under everything else.
    match st.heat {
        Heat::Confidence => {
            for c in &l.confidence {
                if let Some(pl) = on_sheet(&c.anchor) {
                    painter.rect_filled(cells.rect(pl.range), 0.0, heat_color(c.level, 70));
                }
            }
        }
        Heat::Impact => {
            let target = d.selection.active;
            if is_formula_number(d, si, target) {
                let s = st.overlay.sensitivity(d, &l, si, target);
                let max = s.swings.iter().map(|w| (w.at_high - w.at_low).abs()).fold(0.0, f64::max);
                if max > 0.0 {
                    for w in s.swings.iter().filter(|w| w.input.0 == si) {
                        let share = ((w.at_high - w.at_low).abs() / max) as f32;
                        let a = (40.0 + 140.0 * share) as u8;
                        painter.rect_filled(cells.rect(RangeRef::cell(w.input.1)), 0.0, Color32::from_rgba_unmultiplied(0xD0, 0x30, 0x60, a));
                    }
                }
                painter.rect_stroke(
                    cells.rect(RangeRef::cell(target)),
                    0.0,
                    Stroke::new(2.0, Color32::from_rgb(0xD0, 0x30, 0x60)),
                    StrokeKind::Inside,
                );
            }
        }
        Heat::Off => {}
    }

    // Client data: a hatch, so it reads as "not for the agent" at a glance.
    for id in &l.private {
        let Some(pl) = on_sheet(id) else { continue };
        let r = cells.rect(pl.range);
        painter.rect_filled(r, 0.0, p.private.gamma_multiply(0.10));
        let hatch = painter.with_clip_rect(r.intersect(cells.cells()));
        let step = 7.0;
        let mut x = r.left() - r.height();
        while x < r.right() {
            hatch.line_segment([pos2(x, r.bottom()), pos2(x + r.height(), r.top())], Stroke::new(0.8, p.private.gamma_multiply(0.45)));
            x += step;
        }
        painter.rect_stroke(r, 0.0, Stroke::new(1.0, p.private.gamma_multiply(0.7)), StrokeKind::Inside);
    }

    // Inputs with a range: a small diamond at the bottom left.
    for i in &l.inputs {
        let Some(pl) = on_sheet(&i.anchor) else { continue };
        let r = cells.rect(RangeRef::cell(pl.range.start));
        let c = pos2(r.left() + 5.0, r.bottom() - 5.0);
        let pts = vec![pos2(c.x, c.y - 3.5), pos2(c.x + 3.5, c.y), pos2(c.x, c.y + 3.5), pos2(c.x - 3.5, c.y)];
        painter.add(egui::Shape::convex_polygon(pts, p.input, Stroke::NONE));
    }

    // Alternatives: two small tabs at the bottom right; a calculation method: "≈", amber where
    // the choice changes a result materially (and those results get an amber underline).
    if !l.variants.is_empty() {
        for c in st.overlay.comparisons(d, &l) {
            if c.place.sheet != si {
                continue;
            }
            let r = cells.rect(c.place.range);
            let color = if c.kind == VariantKind::Method && c.material() { p.material } else { p.variant };
            match c.kind {
                VariantKind::Alternatives => {
                    let br = r.right_bottom();
                    for (i, k) in [(0.0, 0.55), (1.0, 1.0)] {
                        let x = br.x - 4.0 - (1.0 - i) * 7.0;
                        let tab = Rect::from_min_max(pos2(x - 5.0, br.y - 7.0), pos2(x, br.y - 2.0));
                        painter.rect_filled(tab, 1.0, color.gamma_multiply(k));
                    }
                }
                VariantKind::Method => {
                    painter.text(pos2(r.right() - 3.0, r.bottom() - 1.0), Align2::RIGHT_BOTTOM, "≈", theme::ui_bold(11.0), color);
                    for row in c.rows.iter().filter(|x| x.material && x.key.0 == si && !c.place.range.contains(x.key.1)) {
                        let o = cells.rect(RangeRef::cell(row.key.1));
                        let y = o.bottom() - 1.5;
                        let mut x = o.left() + 2.0;
                        while x < o.right() - 2.0 {
                            painter.line_segment([pos2(x, y), pos2((x + 3.0).min(o.right() - 2.0), y)], Stroke::new(1.5, p.material));
                            x += 5.0;
                        }
                    }
                }
            }
        }
    }

    // Ranges used on slides: an outline and "▸ 2 slides".
    if !l.uses.is_empty() {
        let mut by_range: Vec<(RangeRef, usize)> = vec![];
        for (pl, _) in links::uses_at(&d.wb, &l, si, None).into_iter().filter(|(pl, _)| pl.sheet == si) {
            match by_range.iter_mut().find(|(r, _)| *r == pl.range) {
                Some((_, n)) => *n += 1,
                None => by_range.push((pl.range, 1)),
            }
        }
        for (range, n) in by_range {
            let r = cells.rect(range);
            painter.rect_stroke(r, 0.0, Stroke::new(1.2, p.slide.gamma_multiply(0.8)), StrokeKind::Inside);
            let text = format!("▸ {n} slide{}", if n == 1 { "" } else { "s" });
            let galley = painter.layout_no_wrap(text, theme::ui_bold(9.0), Color32::WHITE);
            // Beside the range, so it never covers a cell's text.
            let pill = Rect::from_min_size(pos2(r.right() + 4.0, r.top() + 2.0), galley.size() + vec2(8.0, 2.0));
            painter.rect_filled(pill, 4.0, p.slide);
            painter.galley(pill.min + vec2(4.0, 1.0), galley, Color32::WHITE);
        }
    }

    // A proposal being looked at: old → new on the cells it changes (stronger where it edits,
    // lighter where results recalculate), like a heat map over the sheet.
    if let Some((topic, option)) = &st.preview
        && let Some(tp) = l.topic(topic)
        && let Some(dr) = gridcraft_agent::proposals::draft(d, tp, option)
        && let Ok(dr) = dr.as_ref()
    {
        // Cells carrying markers show only the new value (the markers sit where the old one would).
        let marked: std::collections::HashSet<CellRef> = l
            .topics
            .iter()
            .filter(|t| !t.hidden)
            .flat_map(|t| t.places.iter())
            .filter_map(|p| on_sheet(&p.anchor))
            .map(|p| CellRef { row: p.range.start.row, col: p.range.end.col })
            .collect();
        for c in dr.changes.iter().filter(|c| c.sheet == si) {
            let r = cells.rect(RangeRef::cell(c.cell));
            if !r.intersects(cells.cells()) {
                continue;
            }
            let clip = painter.with_clip_rect(r.intersect(cells.cells()));
            clip.rect_filled(r, 0.0, t.window);
            clip.rect_filled(r, 0.0, crate::BLUE.gamma_multiply(if c.edited { 0.20 } else { 0.08 }));
            if c.edited {
                clip.rect_stroke(r, 0.0, Stroke::new(1.2, crate::BLUE), StrokeKind::Inside);
            }
            let size = (r.height() * 0.62).clamp(7.0, 15.0);
            let new = painter.layout_no_wrap(c.new.clone(), theme::ui_bold(size), crate::BLUE);
            let old = painter.layout_no_wrap(format!("{} → ", c.old), theme::ui_font(size * 0.85), t.text_dim);
            let y = r.center().y;
            let right = r.right() - 3.0;
            clip.galley(pos2(right - new.size().x, y - new.size().y / 2.0), new.clone(), crate::BLUE);
            // The old value too, when there's room.
            if !c.old.is_empty() && !marked.contains(&c.cell) && old.size().x + new.size().x + 6.0 < r.width() {
                let x = right - new.size().x - old.size().x;
                let oy = y - old.size().y / 2.0;
                clip.galley(pos2(x, oy), old.clone(), t.text_dim);
                let strike = painter.layout_no_wrap(c.old.clone(), theme::ui_font(size * 0.85), t.text_dim).size().x;
                clip.line_segment([pos2(x, y), pos2(x + strike, y)], Stroke::new(1.0, t.text_dim));
            }
        }
    }

    // Topics: a numbered circle per place in its top-right cell's top-right corner, in the topic's
    // status colour; several topics on one cell stack leftwards. A range place gets a thin outline.
    let time = ui.input(|i| i.time);
    let n = topics::numbering(&d.wb, &l);
    let mut by_cell: Vec<(CellRef, Vec<(String, Color32, bool, String, String)>)> = vec![];
    let mut pulsing = false;
    // The active cell inside a place lights up its topic: every place outlined, markers ringed.
    let lit: Vec<String> = if d.selection.current().is_single() {
        n.order.iter().filter(|id| l.topic(id).is_some_and(|t| topic_has_cell(d, &l, t, si, d.selection.active))).cloned().collect()
    } else {
        vec![]
    };
    for id in &n.order {
        let Some(tp) = l.topic(id) else { continue };
        let color = p.status(tp.status.status);
        let working = topics::discussion(&l, id).iter().any(|q| matches!(q.status, QuestionStatus::Working | QuestionStatus::Transcribing));
        let unread = topics::unread(&l, id);
        for place in &tp.places {
            let Some(pl) = on_sheet(&place.anchor) else { continue };
            if lit.contains(id) {
                painter.rect_stroke(cells.rect(pl.range), 0.0, Stroke::new(2.0, color), StrokeKind::Inside);
            } else if !pl.range.is_single() {
                painter.rect_stroke(cells.rect(pl.range), 0.0, Stroke::new(1.0, color.gamma_multiply(0.55)), StrokeKind::Inside);
            }
            let fill = if working {
                pulsing = true;
                p.working.gamma_multiply(0.55 + 0.45 * ((time * 4.0).sin() * 0.5 + 0.5) as f32)
            } else {
                color
            };
            let label = n.place.get(&(id.clone(), place.anchor.clone())).cloned().unwrap_or_default();
            let corner = CellRef { row: pl.range.start.row, col: pl.range.end.col };
            let entry = (label, fill, unread, id.clone(), place.anchor.clone());
            match by_cell.iter_mut().find(|(c, _)| *c == corner) {
                Some((_, list)) => list.push(entry),
                None => by_cell.push((corner, vec![entry])),
            }
        }
    }
    let mut markers = vec![];
    if let Some(sh) = d.wb.sheet(si) {
        for (corner, list) in by_cell {
            let cell = cells.rect(RangeRef::cell(corner));
            if !cell.intersects(cells.cells()) {
                continue;
            }
            let h = 11.0_f32.min((cell.height() * 0.65).max(6.0));
            let sizes: Vec<f32> = list.iter().map(|e| marker_width(painter, &e.0, h)).collect();
            let total = sizes.iter().sum::<f32>() + sizes.len().saturating_sub(1) as f32;
            // Beside the value, never on it: the corner when it's clear, else just left of a
            // right-aligned value; a full cell gets one small dot.
            let span = value_span(painter, &d.wb, sh, corner, cell, h);
            let clear = |right: f32| span.is_none_or(|(l, r)| right - total > r + 1.0 || right < l - 1.0);
            let right = if clear(cell.right() - 1.0) {
                Some(cell.right() - 1.0)
            } else {
                span.map(|(l, _)| l - 2.0).filter(|x| *x - total >= cell.left() + 1.0)
            };
            match right {
                Some(mut x) => {
                    for (e, w) in list.iter().zip(sizes) {
                        let r = Rect::from_min_size(pos2(x - w, cell.top() + 1.0), vec2(w, h));
                        if lit.contains(&e.3) {
                            painter.rect_stroke(r.expand(1.5), r.height() / 2.0 + 1.5, Stroke::new(1.5, e.1), StrokeKind::Outside);
                        }
                        marker(painter, r, &e.0, e.1, e.2, &t);
                        markers.push((r, e.3.clone(), e.4.clone()));
                        x -= w + 1.0;
                    }
                }
                None => {
                    // No room beside the value: a corner flag (numbers on hover and in the pane).
                    let first = list.first().map_or(p.open, |e| e.1);
                    let tr = cell.right_top();
                    let k = 6.0_f32.min(cell.height() * 0.4);
                    painter.add(egui::Shape::convex_polygon(vec![tr, pos2(tr.x, tr.y + k), pos2(tr.x - k, tr.y)], first, Stroke::NONE));
                    if let Some(e) = list.first() {
                        markers.push((Rect::from_min_max(pos2(tr.x - 9.0, tr.y), pos2(tr.x, tr.y + 9.0)), e.3.clone(), e.4.clone()));
                    }
                }
            }
        }
    }
    st.overlay.markers = markers;
    if pulsing {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
    }

    hover_link(st, ui, painter, d, &l, cells, &t);
    hover(st, app, ui, d, &l, cells, &t, &p);
}

fn marker_width(painter: &Painter, label: &str, h: f32) -> f32 {
    let galley = painter.layout_no_wrap(label.to_string(), theme::ui_bold(h - 1.5), Color32::WHITE);
    (galley.size().x + 4.0).max(h)
}

/// The target of the link under the pointer in the pane, outlined on the sheet without moving the
/// selection or the view: a cell or range, a topic's places, a place; a removed place shows the
/// neighbours it was between. Off screen, an edge hint says where ("↓ B40").
#[allow(clippy::too_many_arguments)]
fn hover_link(st: &AgentUi, ui: &Ui, painter: &Painter, d: &DocState, l: &Layer, cells: &CellGeometry, t: &Tokens) {
    let Some(href) = st.hover_link.as_deref() else { return };
    let si = cells.sheet_index;
    let resolve = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a));
    // (place, removed: shown as a neighbour)
    let mut targets: Vec<(anchor::Place, bool)> = vec![];
    let place_or_neighbours = |id: &str, out: &mut Vec<(anchor::Place, bool)>| match resolve(id) {
        Some(p) => out.push((p, false)),
        None => {
            if let Some(w) = l.whereabouts.iter().rev().find(|w| w.anchor == id) {
                out.extend([w.before.as_deref(), w.after.as_deref()].into_iter().flatten().filter_map(resolve).map(|p| (p, true)));
            }
        }
    };
    if let Some(target) = href.strip_prefix("cell:") {
        if let Some(p) = anchor::parse_place(&d.wb, target, d.wb.active_sheet) {
            targets.push((p, false));
        }
    } else if let Some(r) = href.strip_prefix("topic:") {
        if let Some(t) = topics::find_topic(&d.wb, l, r).and_then(|id| l.topic(&id).cloned()) {
            for p in &t.places {
                place_or_neighbours(&p.anchor, &mut targets);
            }
        }
    } else if let Some(r) = href.strip_prefix("place:")
        && let Some((_, a)) = topics::find_place(&d.wb, l, r)
    {
        place_or_neighbours(&a, &mut targets);
    }
    let view = cells.cells();
    let mut hints: Vec<(egui::Pos2, Align2, String)> = vec![];
    for (p, removed) in &targets {
        let label = textlinks::address(p, si);
        if p.sheet != si {
            hints.push((pos2(view.right() - 8.0, view.top() + 8.0), Align2::RIGHT_TOP, format!("→ {label}")));
            continue;
        }
        let r = cells.rect(p.range);
        if r.intersects(view) {
            let color = if *removed { t.text_dim } else { crate::BLUE };
            painter.rect_filled(r, 0.0, color.gamma_multiply(0.10));
            painter.rect_stroke(r, 0.0, Stroke::new(2.0, color), StrokeKind::Outside);
            continue;
        }
        // Off screen: a hint on the edge it lies beyond.
        let (at, align, arrow) = if r.bottom() < view.top() {
            (pos2(r.center().x.clamp(view.left() + 40.0, view.right() - 40.0), view.top() + 6.0), Align2::CENTER_TOP, "↑")
        } else if r.top() > view.bottom() {
            (pos2(r.center().x.clamp(view.left() + 40.0, view.right() - 40.0), view.bottom() - 6.0), Align2::CENTER_BOTTOM, "↓")
        } else if r.right() < view.left() {
            (pos2(view.left() + 6.0, r.center().y.clamp(view.top() + 12.0, view.bottom() - 12.0)), Align2::LEFT_CENTER, "←")
        } else {
            (pos2(view.right() - 6.0, r.center().y.clamp(view.top() + 12.0, view.bottom() - 12.0)), Align2::RIGHT_CENTER, "→")
        };
        hints.push((at, align, format!("{arrow} {label}")));
    }
    for (at, align, text) in hints {
        let g = painter.layout_no_wrap(text, theme::ui_bold(11.0), Color32::WHITE);
        let size = g.size() + vec2(12.0, 6.0);
        let rect = align.anchor_size(at, size);
        painter.rect_filled(rect, size.y / 2.0, crate::BLUE);
        painter.galley(rect.min + vec2(6.0, 3.0), g, Color32::WHITE);
    }
    if !targets.is_empty() {
        ui.ctx().request_repaint();
    }
}

/// A topic marker: a small pill with the number (`3`, `3.2`); an unread answer adds a dot.
pub(crate) fn marker(painter: &Painter, r: Rect, label: &str, fill: Color32, unread: bool, t: &Tokens) {
    let galley = painter.layout_no_wrap(label.to_string(), theme::ui_bold(r.height() - 1.5), Color32::WHITE);
    painter.rect_filled(r, r.height() / 2.0, fill);
    painter.galley(r.center() - galley.size() / 2.0, galley, Color32::WHITE);
    if unread {
        let c = pos2(r.left(), r.top() + 1.0);
        painter.circle_filled(c, 2.6, t.window);
        painter.circle_filled(c, 1.9, crate::BLUE);
    }
}

/// Where a cell's shown value sits across the cell (left and right x) when it reaches up into the
/// strip a marker of height `h` takes at the top; `None` when the strip is clear.
fn value_span(
    painter: &Painter,
    wb: &gridcraft_engine::model::Workbook,
    sh: &gridcraft_engine::model::Sheet,
    c: CellRef,
    rect: Rect,
    h: f32,
) -> Option<(f32, f32)> {
    use gridcraft_engine::model::{HAlign, VAlign};
    let cell = sh.cell(c)?;
    if cell.value.is_empty() {
        return None;
    }
    let text = gridcraft_engine::display::cell_text(wb, sh, c);
    if text.is_empty() {
        return None;
    }
    let z = rect.height() / sh.row_height(c.row).max(1.0);
    let st = wb.styles.get(sh.style_id(c));
    let size = (st.font.size * 96.0 / 72.0 * z).clamp(4.0, 200.0);
    let font = egui::FontId::new(size, theme::cell_family(&st.font.name, st.font.bold, st.font.italic));
    let g = painter.layout_no_wrap(text, font, Color32::PLACEHOLDER).size();
    // Text sits at the bottom unless styled otherwise; a tall row leaves the top strip clear.
    let top = match st.align.v {
        VAlign::Top => rect.top(),
        VAlign::Bottom => rect.bottom() - g.y,
        _ => rect.center().y - g.y / 2.0,
    };
    if top > rect.top() + 1.0 + h {
        return None;
    }
    let pad = 2.5 * z;
    let h_align = match st.align.h {
        HAlign::General => match cell.value {
            Value::Number(_) => HAlign::Right,
            Value::Bool(_) | Value::Error(_) => HAlign::Center,
            _ => HAlign::Left,
        },
        x => x,
    };
    Some(match h_align {
        HAlign::Right => (rect.right() - pad - g.x, rect.right()),
        HAlign::Center | HAlign::CenterAcross | HAlign::Distributed => (rect.center().x - g.x / 2.0, rect.center().x + g.x / 2.0),
        // Left-aligned text may run on into the next cells.
        _ => (rect.left(), (rect.left() + pad + g.x).max(rect.left())),
    })
}

/// Red (0) → amber → green (1).
pub(crate) fn heat_color(level: f64, alpha: u8) -> Color32 {
    let x = level.clamp(0.0, 1.0) as f32;
    let (r, g, b) = if x < 0.5 {
        let k = x / 0.5;
        (0.86, 0.25 + 0.45 * k, 0.2)
    } else {
        let k = (x - 0.5) / 0.5;
        (0.86 - 0.66 * k, 0.70 - 0.05 * k, 0.2 + 0.15 * k)
    };
    Color32::from_rgba_unmultiplied((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8, alpha)
}

#[allow(clippy::too_many_arguments)]
fn hover(st: &mut AgentUi, app: &SheetApp, ui: &Ui, d: &DocState, l: &Layer, cells: &CellGeometry, t: &Tokens, p: &Palette) {
    let now = ui.input(|i| i.time);
    let si = cells.sheet_index;
    let (Some(c), Some(pointer)) = (cells.hovered, cells.pointer) else {
        st.overlay.hover = None;
        return;
    };
    // No card while editing or dragging.
    if app.editor.is_some() || ui.input(|i| i.pointer.any_down()) {
        st.overlay.hover = None;
        return;
    }
    let since = match st.overlay.hover {
        Some((s, h, since)) if s == si && h == c => since,
        _ => {
            st.overlay.hover = Some((si, c, now));
            now
        }
    };
    if now - since < HOVER_DELAY {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(((HOVER_DELAY - (now - since)) * 1000.0) as u64 + 10));
        return;
    }
    let here = |id: &str| l.anchor(id).and_then(|a| anchor::resolve(&d.wb, a)).is_some_and(|pl| pl.sheet == si && pl.range.contains(c));
    let n = topics::numbering(&d.wb, l);
    let at: Vec<_> = n.order.iter().filter_map(|id| l.topic(id)).filter(|t| t.places.iter().any(|p| here(&p.anchor))).collect();
    let private = l.private.iter().any(|id| here(id));
    let input = l.inputs.iter().find(|i| here(&i.anchor));
    let slides: Vec<_> = links::uses_at(&d.wb, l, si, Some(&RangeRef::cell(c))).into_iter().map(|(_, u)| u).collect();
    let sens = if is_formula_number(d, si, c) { Some(st.overlay.sensitivity(d, l, si, c)) } else { None };
    let sens = sens.filter(|s| !s.swings.is_empty());
    let proposed = st.preview.as_ref().and_then(|(topic, option)| {
        let tp = l.topic(topic)?;
        let dr = gridcraft_agent::proposals::draft(d, tp, option)?;
        let ch = dr.as_ref().as_ref().ok()?.changes.iter().find(|x| x.sheet == si && x.cell == c)?.clone();
        Some((n.topic.get(topic).copied().unwrap_or(0), option.clone(), ch))
    });
    if at.is_empty() && !private && proposed.is_none() && input.is_none() && sens.is_none() && slides.is_empty() {
        return;
    }
    let pos = pointer + vec2(18.0, 18.0);
    egui::Area::new(egui::Id::new("agent_hover_card")).fixed_pos(pos).order(egui::Order::Tooltip).interactable(false).constrain(true).show(
        ui.ctx(),
        |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(320.0);
                if private {
                    ui.label(egui::RichText::new("Client data · the agent sees stand-in values").color(p.private).small().strong());
                }
                if !slides.is_empty() {
                    ui.label(egui::RichText::new("Used on slides").color(p.slide).small().strong());
                    for u in slides.iter().take(4) {
                        let title = if u.slide_title.is_empty() { String::new() } else { format!(" · {}", u.slide_title) };
                        ui.label(egui::RichText::new(format!("{} · slide {}{title}", u.deck_title, u.slide)).small());
                    }
                    ui.add_space(4.0);
                }
                if let Some((num, option, ch)) = &proposed {
                    ui.label(egui::RichText::new(format!("Proposed in topic {num}, option {option}")).color(crate::BLUE).small().strong());
                    ui.label(format!(
                        "{} → {}",
                        if ch.old.is_empty() { "empty" } else { &ch.old },
                        if ch.new.is_empty() { "empty" } else { &ch.new }
                    ));
                    if !ch.edited {
                        ui.label(egui::RichText::new("recalculated from the proposed changes").small().color(t.text_dim));
                    }
                    ui.add_space(4.0);
                }
                for tp in at.iter().take(3) {
                    let num = n.topic.get(&tp.id).copied().unwrap_or(0);
                    let color = p.status(tp.status.status);
                    ui.label(egui::RichText::new(format!("{num} · {}", tp.status.status.label())).color(color).small().strong());
                    if !tp.status.text.is_empty() {
                        ui.label(clip(&textlinks::plain(&d.wb, l, &tp.status.text, si), 160));
                    }
                    let disc = topics::discussion(l, &tp.id);
                    if let Some(q) = disc.last() {
                        if !q.text.is_empty() {
                            ui.label(egui::RichText::new(clip(&textlinks::plain(&d.wb, l, &q.text, si), 120)).italics());
                        }
                        match &q.answer {
                            Some(a) => {
                                ui.label(clip(&textlinks::plain(&d.wb, l, &a.text, si), 180));
                            }
                            None if q.status == QuestionStatus::Working => {
                                ui.label(egui::RichText::new("The agent is working on it").color(p.working).small());
                            }
                            None => {}
                        }
                    }
                    for note in topics::notes(l, &tp.id).iter().take(2) {
                        let title = if note.title.is_empty() {
                            kind_label(&note.kind).to_string()
                        } else {
                            format!("{} · {}", kind_label(&note.kind), note.title)
                        };
                        ui.label(egui::RichText::new(title).color(p.note).small());
                    }
                    ui.add_space(4.0);
                }
                if let Some(i) = input {
                    let range = if i.relative {
                        format!("{:+.0}% … {:+.0}%", (i.low - 1.0) * 100.0, (i.high - 1.0) * 100.0)
                    } else {
                        format!("{} … {}", fmt_num(i.low), fmt_num(i.high))
                    };
                    ui.label(egui::RichText::new(format!("Input · plausible range {range}")).color(p.input).small().strong());
                    if !i.rationale.is_empty() {
                        ui.label(clip(&i.rationale, 200));
                    }
                }
                if let Some(s) = sens {
                    ui.label(egui::RichText::new("Sensitivity to its inputs").color(t.text_dim).small().strong());
                    tornado(ui, &s, t, 300.0, 5);
                    if s.swings.iter().any(|w| !w.reasoned) {
                        ui.label(egui::RichText::new("Outlined bars: default ±10%, no range from the agent yet").small().color(t.text_dim));
                    }
                }
            });
        },
    );
}

pub(crate) fn kind_label(kind: &str) -> &'static str {
    match kind {
        "faq" => "Prepared answer",
        "assumption" => "Assumption",
        "source" => "Source",
        "decision" => "Decision",
        _ => "Explanation",
    }
}

pub(crate) fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>().trim_end()) }
}

/// A tornado chart: one bar per input from the output at its low end to its high end, around the
/// current value; the widest swing on top.
pub(crate) fn tornado(ui: &mut Ui, s: &Sens, t: &Tokens, width: f32, rows: usize) {
    let Some(base) = s.base else { return };
    let swings: Vec<&Swing> = s.swings.iter().take(rows).collect();
    if swings.is_empty() {
        return;
    }
    let lo = swings.iter().map(|w| w.at_low.min(w.at_high)).fold(base, f64::min);
    let hi = swings.iter().map(|w| w.at_low.max(w.at_high)).fold(base, f64::max);
    let span = (hi - lo).max(f64::EPSILON);
    let label_w = (width * 0.38).min(130.0);
    let row_h = 18.0;
    let (rect, _) = ui.allocate_exact_size(vec2(width, row_h * swings.len() as f32 + 16.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let bars = Rect::from_min_max(pos2(rect.left() + label_w + 6.0, rect.top()), pos2(rect.right() - 4.0, rect.bottom() - 16.0));
    let x = |v: f64| bars.left() + ((v - lo) / span) as f32 * bars.width();
    let down = Color32::from_rgb(0xC8, 0x3E, 0x3E);
    let up = Color32::from_rgb(0x2E, 0x8B, 0x57);
    for (i, w) in swings.iter().enumerate() {
        let y = bars.top() + i as f32 * row_h;
        let mid = y + row_h / 2.0;
        let name = if w.label.is_empty() { "input".to_string() } else { clip(&w.label, 22) };
        painter.text(pos2(rect.left(), mid), Align2::LEFT_CENTER, name, theme::ui_font(11.0), t.text);
        let (a, b) = (x(w.at_low), x(w.at_high));
        let bx = x(base);
        // The part below the current value in red, above it in green, whichever end gives it.
        let low_end = a.min(b);
        let high_end = a.max(b);
        let r_low = Rect::from_min_max(pos2(low_end, y + 3.0), pos2(bx, y + row_h - 3.0));
        let r_high = Rect::from_min_max(pos2(bx, y + 3.0), pos2(high_end, y + row_h - 3.0));
        painter.rect_filled(r_low, 1.0, down.gamma_multiply(0.85));
        painter.rect_filled(r_high, 1.0, up.gamma_multiply(0.85));
        if !w.reasoned {
            let r = Rect::from_min_max(pos2(low_end, y + 3.0), pos2(high_end, y + row_h - 3.0));
            painter.rect_stroke(r, 1.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
        }
    }
    painter.line_segment([pos2(x(base), bars.top()), pos2(x(base), bars.bottom())], Stroke::new(1.2, t.text));
    let foot = bars.bottom() + 8.0;
    painter.text(pos2(bars.left(), foot), Align2::LEFT_CENTER, fmt_num(lo), theme::ui_font(10.0), t.text_dim);
    painter.text(pos2(x(base), foot), Align2::CENTER_CENTER, fmt_num(base), theme::ui_bold(10.0), t.text);
    painter.text(pos2(bars.right(), foot), Align2::RIGHT_CENTER, fmt_num(hi), theme::ui_font(10.0), t.text_dim);
}
