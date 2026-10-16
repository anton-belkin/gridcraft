//! What the agent layer paints over the cells, and the hover card with a number's sensitivity.

use std::collections::HashMap;

use egui::{Align2, Color32, Painter, Rect, Stroke, StrokeKind, Ui, pos2, vec2};
use gridcraft_agent::analysis::{self, Swing};
use gridcraft_agent::anchor;
use gridcraft_agent::layer::{self, Layer, QuestionStatus, VariantKind};
use gridcraft_agent::links;
use gridcraft_agent::stress::{self, Stress};
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

    // Notes: a triangle in the top-left corner (Excel's comments use the top right).
    for n in &l.notes {
        let Some(pl) = on_sheet(&n.anchor) else { continue };
        let r = cells.rect(RangeRef::cell(pl.range.start));
        let pts = vec![r.left_top(), pos2(r.left() + 7.0, r.top()), pos2(r.left(), r.top() + 7.0)];
        painter.add(egui::Shape::convex_polygon(pts, p.note, Stroke::NONE));
    }

    // Questions: a badge in the top-right corner.
    for q in &l.questions {
        let Some(pl) = on_sheet(&q.anchor) else { continue };
        let r = cells.rect(pl.range);
        if !pl.range.is_single() {
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, status_color(&p, q.status, q.seen).gamma_multiply(0.6)), StrokeKind::Inside);
        }
        badge(painter, r, q.status, q.seen, &p, ui.input(|i| i.time));
    }
    if l.questions.iter().any(|q| q.status == QuestionStatus::Working && !q.is_ping()) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
    }

    hover(st, app, ui, d, &l, cells, &t, &p);
}

fn status_color(p: &Palette, s: QuestionStatus, seen: bool) -> Color32 {
    match s {
        QuestionStatus::Pending => p.pending,
        QuestionStatus::Working => p.working,
        QuestionStatus::Answered if seen => p.seen,
        QuestionStatus::Answered => p.answered,
    }
}

/// A flag in the top-right corner of the cell (where Excel puts its comment marker, in the
/// question's colour): it never covers the cell's text. A fresh answer gets a bigger flag; a
/// question being worked on pulses.
fn badge(painter: &Painter, cell: Rect, s: QuestionStatus, seen: bool, p: &Palette, time: f64) {
    let mut color = status_color(p, s, seen);
    let size = match s {
        QuestionStatus::Answered if !seen => 11.0_f32,
        QuestionStatus::Answered => 7.0,
        _ => 9.0,
    };
    if s == QuestionStatus::Working {
        let k = 0.55 + 0.45 * ((time * 4.0).sin() * 0.5 + 0.5) as f32;
        color = color.gamma_multiply(k);
    }
    let size = size.min(cell.height() * 0.6).min(cell.width() * 0.5);
    let tr = cell.right_top();
    let pts = vec![tr, pos2(tr.x, tr.y + size), pos2(tr.x - size, tr.y)];
    painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
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
    let notes: Vec<_> = l.notes.iter().filter(|n| here(&n.anchor)).collect();
    let questions: Vec<_> = l.questions.iter().filter(|q| here(&q.anchor)).collect();
    let private = l.private.iter().any(|id| here(id));
    let input = l.inputs.iter().find(|i| here(&i.anchor));
    let slides: Vec<_> = links::uses_at(&d.wb, l, si, Some(&RangeRef::cell(c))).into_iter().map(|(_, u)| u).collect();
    let sens = if is_formula_number(d, si, c) { Some(st.overlay.sensitivity(d, l, si, c)) } else { None };
    let sens = sens.filter(|s| !s.swings.is_empty());
    if notes.is_empty() && questions.is_empty() && !private && input.is_none() && sens.is_none() && slides.is_empty() {
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
                for q in questions.iter().take(2) {
                    let (status, color) = match q.status {
                        QuestionStatus::Pending => ("Question waiting", p.pending),
                        QuestionStatus::Working => ("The agent is working on it", p.working),
                        QuestionStatus::Answered => ("Answered", p.answered),
                    };
                    ui.label(egui::RichText::new(status).color(color).small().strong());
                    ui.label(egui::RichText::new(&q.text).italics());
                    if let Some(a) = &q.answer {
                        ui.label(clip(&a.text, 180));
                    }
                    ui.add_space(4.0);
                }
                for n in notes.iter().take(2) {
                    let title = if n.title.is_empty() { kind_label(&n.kind).to_string() } else { format!("{} · {}", kind_label(&n.kind), n.title) };
                    ui.label(egui::RichText::new(title).color(p.note).small().strong());
                    ui.label(clip(&n.text, 280));
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
