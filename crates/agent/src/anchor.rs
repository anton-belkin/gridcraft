//! Anchors: hidden defined names the engine (and Excel) keep pointing at the same cells while
//! rows and columns are inserted and deleted.

use std::sync::Arc;

use gridcraft_engine::core::RangeRef;
use gridcraft_engine::model::{DefinedName, Workbook};
use gridcraft_engine::{DocState, Session};

use crate::layer::{self, Anchor, Layer};

/// Prefix of the hidden names (Excel accepts names starting with `_`).
pub const NAME_PREFIX: &str = "_GcAgent_";

/// A resolved place in the workbook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub sheet: usize,
    pub sheet_name: String,
    pub range: RangeRef,
}

impl Place {
    pub fn a1(&self) -> String {
        format!("{}!{}", quote_sheet(&self.sheet_name), self.range.a1())
    }
}

pub fn quote_sheet(name: &str) -> String {
    if !name.is_empty()
        && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !name.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

/// `Sheet1!$B$2:$C$3`, `'My sheet'!B2`, `B2` (on `default_sheet`) → sheet index and range.
pub fn parse_place(wb: &Workbook, text: &str, default_sheet: usize) -> Option<Place> {
    let text = text.trim().trim_start_matches('=');
    let (sheet, range) = match text.rfind('!') {
        Some(i) => {
            let raw = &text[..i];
            let name = if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
                raw[1..raw.len() - 1].replace("''", "'")
            } else {
                raw.to_string()
            };
            (wb.sheet_index(&name)?, &text[i + 1..])
        }
        None => (default_sheet, text),
    };
    let range = RangeRef::parse(&range.replace('$', ""))?;
    let sheet_name = wb.sheet(sheet)?.name.clone();
    Some(Place { sheet, sheet_name, range })
}

pub(crate) fn absolute(r: RangeRef) -> String {
    let cell = |c: gridcraft_engine::core::CellRef| {
        let a = c.a1();
        let split = a.find(|ch: char| ch.is_ascii_digit()).unwrap_or(a.len());
        format!("${}${}", &a[..split], &a[split..])
    };
    if r.is_single() { cell(r.start) } else { format!("{}:{}", cell(r.start), cell(r.end)) }
}

/// Where an anchor is now: its hidden name if the workbook still has it, else where it was last.
pub fn resolve(wb: &Workbook, a: &Anchor) -> Option<Place> {
    if let Some(n) = wb.names.iter().find(|n| n.name == a.name) {
        if n.formula.contains("#REF!") {
            return None;
        }
        if let Some(p) = parse_place(wb, &n.formula, 0) {
            return Some(p);
        }
    }
    if a.lost {
        return None;
    }
    let si = wb.sheet_index(&a.sheet)?;
    let range = RangeRef::parse(&a.range)?;
    Some(Place { sheet: si, sheet_name: a.sheet.clone(), range })
}

/// Anchors `place`, reusing an anchor already on exactly that range.
pub fn anchor_at(s: &mut Session, place: &Place) -> gridcraft_engine::Result<String> {
    let d = s.doc()?;
    let wb = d.wb.clone();
    let existing = layer::get(d).anchors.iter().find(|a| resolve(&wb, a).as_ref() == Some(place)).map(|a| a.id.clone());
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = layer::update(d, |l| l.fresh_id("a"));
    let name = format!("{NAME_PREFIX}{id}");
    let anchor = Anchor { id: id.clone(), name: name.clone(), sheet: place.sheet_name.clone(), range: place.range.a1(), lost: false };
    layer::update(s.doc()?, |l| l.anchors.push(anchor));
    let d = s.doc_mut()?;
    add_name(Arc::make_mut(&mut d.wb), &name, place);
    Ok(id)
}

pub(crate) fn add_name(wb: &mut Workbook, name: &str, place: &Place) {
    wb.names.retain(|n| n.name != name);
    wb.names.push(DefinedName {
        name: name.to_string(),
        scope: None,
        formula: format!("{}!{}", quote_sheet(&place.sheet_name), absolute(place.range)),
        comment: "GridCraft agent layer anchor".into(),
        hidden: true,
    });
}

/// After a command: remembers where every anchor is, puts back hidden names an undo took away,
/// and drops names of anchors nothing uses.
pub fn refresh(d: &mut DocState) {
    let l = layer::get(d);
    if l.anchors.is_empty() {
        return;
    }
    let wb = d.wb.clone();
    let unused = l.unused_anchors();
    let mut missing: Vec<(String, Place)> = vec![];
    let mut seen: Vec<(String, Option<Place>, bool)> = vec![];
    for a in &l.anchors {
        let has_name = wb.names.iter().any(|n| n.name == a.name);
        let place = resolve(&wb, a);
        if !has_name && let Some(p) = &place {
            missing.push((a.name.clone(), p.clone()));
        }
        seen.push((a.id.clone(), place, has_name));
    }
    let changed = seen.iter().any(|(id, p, _)| {
        let a = l.anchor(id);
        match (a, p) {
            (Some(a), Some(p)) => a.sheet != p.sheet_name || a.range != p.range.a1() || a.lost,
            (Some(a), None) => !a.lost,
            _ => false,
        }
    });
    if changed || !unused.is_empty() {
        layer::update(d, |l: &mut Layer| {
            for (id, p, _) in &seen {
                if let Some(a) = l.anchors.iter_mut().find(|a| &a.id == id) {
                    match p {
                        Some(p) => {
                            a.sheet = p.sheet_name.clone();
                            a.range = p.range.a1();
                            a.lost = false;
                        }
                        None => a.lost = true,
                    }
                }
            }
            l.anchors.retain(|a| !unused.contains(&a.id));
        });
    }
    let drop_names: Vec<String> = l.anchors.iter().filter(|a| unused.contains(&a.id)).map(|a| a.name.clone()).collect();
    let missing: Vec<_> = missing.into_iter().filter(|(n, _)| !drop_names.contains(n)).collect();
    if !missing.is_empty() || (!drop_names.is_empty() && wb.names.iter().any(|n| drop_names.contains(&n.name))) {
        let wb = Arc::make_mut(&mut d.wb);
        wb.names.retain(|n| !drop_names.contains(&n.name));
        for (name, place) in &missing {
            add_name(wb, name, place);
        }
    }
}
