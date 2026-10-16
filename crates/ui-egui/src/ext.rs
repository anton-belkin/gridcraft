//! UI extension points: ribbon tabs, panels, cell overlays and context-menu items contributed by
//! crates an app links in (the UI knows nothing about them). Register with [`register`] before the first frame.
//!
//! Extensions act like the rest of the UI: they read engine state from `app.session` and act
//! through `app.run(id, params)`.

use std::cell::RefCell;

use egui::{Painter, Pos2, Rect, Ui};
use gridcraft_engine::core::{CellRef, RangeRef};
use gridcraft_engine::model::Sheet;

use crate::SheetApp;
use crate::grid::Geo;

/// Where the active sheet's cells are on screen this frame.
pub struct CellGeometry<'a> {
    geo: &'a Geo,
    sheet: &'a Sheet,
    /// Index of the sheet being painted.
    pub sheet_index: usize,
    /// The cell under the pointer, if the pointer is over the cells.
    pub hovered: Option<CellRef>,
    /// The pointer position, if it is over the cells.
    pub pointer: Option<Pos2>,
}

impl CellGeometry<'_> {
    /// The on-screen rectangle of a range (merged cells included), in the painter's coordinates.
    pub fn rect(&self, r: RangeRef) -> Rect {
        self.geo.range_rect(self.sheet, r)
    }
    /// The visible cell area.
    pub fn cells(&self) -> Rect {
        self.geo.cells
    }
}

/// A UI extension. Every method has a default that does nothing.
pub trait UiExtension {
    /// Once per frame, before anything is drawn (shortcuts, polling, opening panels).
    fn frame(&mut self, _app: &mut SheetApp, _ctx: &egui::Context) {}
    /// A right-hand side panel; show one with `egui::Panel::right(..).show(ui, ..)` or nothing.
    fn side_panel(&mut self, _app: &mut SheetApp, _ui: &mut Ui) {}
    /// Paint over the active sheet's cells (after cells, objects and built-in overlays).
    fn paint_cells(&mut self, _app: &SheetApp, _ui: &Ui, _painter: &Painter, _cells: &CellGeometry) {}
    /// Items appended to the cell context menu. Return true to close the menu.
    fn context_menu(&mut self, _app: &mut SheetApp, _ui: &mut Ui) -> bool {
        false
    }
    /// Ribbon tabs of the extension's own, shown after the built-in ones.
    fn ribbon_tabs(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Draws the content of one of [`UiExtension::ribbon_tabs`] when it is the active tab, inside
    /// the ribbon card (a horizontal row, 76 px high, like the built-in tabs).
    fn ribbon_tab(&mut self, _app: &mut SheetApp, _ui: &mut Ui, _tab: &str) {}
}

thread_local! {
    static EXTENSIONS: RefCell<Vec<Box<dyn UiExtension>>> = RefCell::new(Vec::new());
    /// The extensions' ribbon tabs, by name.
    static TABS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Adds a UI extension (on the UI thread).
pub fn register(ext: Box<dyn UiExtension>) {
    TABS.with(|t| t.borrow_mut().extend(ext.ribbon_tabs()));
    EXTENSIONS.with(|e| e.borrow_mut().push(ext));
}

/// Runs `f` on every extension. The list is taken out while they run, so an extension may use
/// the whole app (and even register another extension).
fn each(mut f: impl FnMut(&mut dyn UiExtension)) {
    let mut list = EXTENSIONS.with(|e| std::mem::take(&mut *e.borrow_mut()));
    for ext in &mut list {
        f(ext.as_mut());
    }
    EXTENSIONS.with(|e| {
        let mut slot = e.borrow_mut();
        list.append(&mut slot);
        *slot = list;
    });
}

pub(crate) fn frame(app: &mut SheetApp, ctx: &egui::Context) {
    each(|x| x.frame(app, ctx));
}

pub(crate) fn side_panels(app: &mut SheetApp, ui: &mut Ui) {
    each(|x| x.side_panel(app, ui));
}

pub(crate) fn paint_cells(app: &SheetApp, ui: &Ui, painter: &Painter, geo: &Geo, sheet_index: usize, sheet: &Sheet) {
    let pointer = ui.input(|i| i.pointer.hover_pos()).filter(|p| geo.cells.contains(*p));
    let hovered = pointer.map(|p| geo.cell_at(sheet, p));
    let cells = CellGeometry { geo, sheet, sheet_index, hovered, pointer };
    each(|x| x.paint_cells(app, ui, painter, &cells));
}

/// The extensions' ribbon tabs.
pub(crate) fn ribbon_tabs() -> Vec<&'static str> {
    TABS.with(|t| t.borrow().clone())
}

/// `tab` belongs to an extension.
pub(crate) fn is_ribbon_tab(tab: &str) -> bool {
    TABS.with(|t| t.borrow().contains(&tab))
}

/// Draws the active ribbon tab when it is an extension's.
pub(crate) fn ribbon_tab(app: &mut SheetApp, ui: &mut Ui) {
    let tab = app.ui.ribbon_tab.clone();
    if is_ribbon_tab(&tab) {
        each(|x| x.ribbon_tab(app, ui, &tab));
    }
}

/// A vertical separator between ribbon groups, as the built-in tabs draw it.
pub fn ribbon_separator(ui: &mut Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(egui::vec2(13.0, 72.0), egui::Sense::hover());
    ui.painter()
        .line_segment([egui::pos2(r.center().x, r.top() + 6.0), egui::pos2(r.center().x, r.bottom() - 6.0)], egui::Stroke::new(1.0, t.separator));
}

pub(crate) fn context_menu(app: &mut SheetApp, ui: &mut Ui) -> bool {
    let mut close = false;
    each(|x| close |= x.context_menu(app, ui));
    close
}
