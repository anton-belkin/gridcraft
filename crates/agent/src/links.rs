//! GridCraft's side of Excel → slide links (the ArtCraft links contract v1, kept by DeckCraft in
//! `agent-layer/links.md`).
//!
//! - `link.copy`: Copy as Linked Source. The range gets a hidden name `_GcLink_<id>` (moved by the
//!   engine and Excel with inserted rows and columns, never cleaned up), the workbook gets a stable
//!   id, and the clipboard gets a plain table whose `data-artcraft-link` attribute carries the
//!   payload (values, displayed text, where they come from).
//! - `link.read`: DeckCraft asks a running GridCraft for the current values (saved or not).
//! - `link.addUses` / the registry file: which slides use which range, kept in the layer so the
//!   back-reference travels with the workbook; `link.gotoSlide` jumps there.
//! - `link.reveal` (and `--goto` in the app): open the workbook at the linked cells.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::{CellRef, RangeRef, Value};
use gridcraft_engine::model::Workbook;
use gridcraft_engine::{DocState, Result, Session};
use serde_json::{Value as Json, json};

use crate::analysis;
use crate::anchor::{self, Place};
use crate::commands::{bad, place_p, spec, str_p, text_p};
use crate::layer::{self, Actor, Anchor, LinkUse};

/// Prefix of link names (hidden, workbook-scoped).
pub const NAME_PREFIX: &str = "_GcLink_";
/// The contract's limits for one linked copy.
pub const MAX_ROWS: u32 = 75;
pub const MAX_COLS: u32 = 75;
pub const MAX_CELLS: u64 = 2_000;
/// Most uses kept per workbook.
const MAX_USES: usize = 2_000;
/// Largest registry file read.
const MAX_REGISTRY: u64 = 16 * 1024 * 1024;

pub static SPECS: &[CommandSpec] = &[
    spec!(change "link.copy", "Copy Link to Cells", "{range?: (default: selection)} → {html, text, payload, anchor}: a table for the clipboard whose data-artcraft-link attribute links a slide back to these cells (the workbook must be saved)", copy),
    spec!(query "link.read", "Read Linked Cells", "{path, workbookId, anchor, sheet, range} → {path, sheet, range, values, text}: the linked cells now (an error when that workbook isn't open here)", read),
    spec!(change "link.addUses", "Add Link Uses", "{uses: [{link, workbookId, workbook, anchor, deck, deckTitle, slide, slideTitle, shapeKey, mode, …}]} — where a linked range is used (DeckCraft reports it)", add_uses),
    spec!(query "link.uses", "Link Uses", "{range?} → where the linked ranges of the workbook (or those touching `range`) are used", uses),
    spec!(query "link.reveal", "Show Linked Cells", "{path, workbookId?, anchor?, sheet?, range?} — opens the workbook if needed and selects the linked cells", reveal),
    spec!(query "link.gotoSlide", "Go to Slide", "{link} — selects the slide that uses this range in DeckCraft (starting it on the deck when it isn't running)", goto_slide),
];

// ---------------------------------------------------------------- helpers

/// Where a link name points now: `Err(true)` when its cells were deleted (`#REF!`).
pub fn resolve_name(wb: &Workbook, name: &str) -> std::result::Result<Place, bool> {
    let Some(n) = wb.names.iter().find(|n| n.name.eq_ignore_ascii_case(name)) else { return Err(false) };
    if n.formula.contains("#REF!") {
        return Err(true);
    }
    anchor::parse_place(wb, &n.formula, 0).ok_or(false)
}

fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let canon = |p: &str| std::fs::canonicalize(p).ok().or_else(|| std::path::absolute(p).ok());
    matches!((canon(a), canon(b)), (Some(x), Some(y)) if x == y)
}

/// The open document a link source names: by workbook id, else by path.
fn find_doc(s: &Session, workbook_id: &str, path: &str) -> Option<usize> {
    let docs = s.documents();
    if !workbook_id.is_empty()
        && let Some(i) = docs.iter().position(|d| layer::get(d).workbook_id == workbook_id)
    {
        return Some(i);
    }
    if path.is_empty() {
        return None;
    }
    docs.iter().position(|d| d.path.as_deref().is_some_and(|p| same_path(p, path)))
}

fn cell_value(v: &Value) -> Json {
    match v {
        Value::Empty => Json::Null,
        Value::Number(n) => json!(n),
        Value::Text(t) => json!(t.as_ref()),
        Value::Bool(b) => json!(b),
        Value::Error(_) | Value::Array(_) => Json::Null,
    }
}

/// Values and displayed text of a range, row by row (errors as their text).
fn grid(wb: &Workbook, place: &Place) -> (Vec<Vec<Json>>, Vec<Vec<String>>) {
    let Some(sh) = wb.sheet(place.sheet) else { return (vec![], vec![]) };
    let r = place.range;
    let mut values = vec![];
    let mut texts = vec![];
    for row in r.start.row..=r.end.row.min(r.start.row.saturating_add(MAX_ROWS)) {
        let mut vr = vec![];
        let mut tr = vec![];
        for col in r.start.col..=r.end.col.min(r.start.col.saturating_add(MAX_COLS)) {
            let c = CellRef::new(row, col);
            let text = gridcraft_engine::display::cell_text(wb, sh, c);
            let v = sh.cell(c).map(|x| &x.value).cloned().unwrap_or(Value::Empty);
            vr.push(if matches!(v, Value::Error(_)) { json!(text) } else { cell_value(&v) });
            tr.push(text);
        }
        values.push(vr);
        texts.push(tr);
    }
    (values, texts)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A random UUID (v4 layout): from the secure source where there is one, else from the
/// clock and the document.
pub fn new_uuid(salt: u64) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    let mut b: [u8; 16] = {
        let mut b = [0u8; 16];
        if getrandom::fill(&mut b).is_err() {
            b = fallback_bytes(salt);
        }
        b
    };
    #[cfg(target_arch = "wasm32")]
    let mut b: [u8; 16] = fallback_bytes(salt);
    if let Some(x) = b.get_mut(6) {
        *x = (*x & 0x0f) | 0x40;
    }
    if let Some(x) = b.get_mut(8) {
        *x = (*x & 0x3f) | 0x80;
    }
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let part = |a: usize, z: usize| h.get(a..z).unwrap_or("");
    format!("{}-{}-{}-{}-{}", part(0, 8), part(8, 12), part(12, 16), part(16, 20), part(20, 32))
}

fn fallback_bytes(salt: u64) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let d: [u8; 32] = Sha256::new().chain_update(crate::now_ms().to_le_bytes()).chain_update(salt.to_le_bytes()).finalize().into();
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b
}

/// What the range is about: a named range on exactly it, else the labels the sheet gives it.
fn label(wb: &Workbook, place: &Place) -> String {
    let named = wb.names.iter().find(|n| !n.hidden && anchor::parse_place(wb, &n.formula, place.sheet).as_ref() == Some(place));
    if let Some(n) = named {
        return n.name.replace('_', " ");
    }
    let Some(sh) = wb.sheet(place.sheet) else { return String::new() };
    let l = analysis::label_of(sh, place.range.start);
    if !l.is_empty() {
        return l;
    }
    // A table with a header row: its first heading.
    match sh.cell(place.range.start).map(|c| &c.value) {
        Some(Value::Text(t)) => t.to_string(),
        _ => sh.name.clone(),
    }
}

// ---------------------------------------------------------------- link.copy

fn copy(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "link.copy";
    let place = place_p(s, p, CMD)?;
    let d = s.doc()?;
    let Some(path) = d.path.clone() else {
        return Err(bad(CMD, "Save the workbook first: a slide links to cells in a saved file."));
    };
    let r = place.range;
    if r.height() > MAX_ROWS || r.width() > MAX_COLS || r.count() > MAX_CELLS {
        return Err(bad(
            CMD,
            format!("A linked copy holds at most {MAX_ROWS} rows, {MAX_COLS} columns and {MAX_CELLS} cells: a slide table is for people to read."),
        ));
    }
    // Reuse the name already on exactly this range.
    let existing =
        d.wb.names
            .iter()
            .find(|n| {
                n.name.starts_with(NAME_PREFIX) && !n.formula.contains("#REF!") && anchor::parse_place(&d.wb, &n.formula, 0).as_ref() == Some(&place)
            })
            .map(|n| n.name.clone());
    let uid = d.uid;
    let name = match existing {
        Some(n) => n,
        None => {
            let id = layer::update(d, |l| l.fresh_id("l"));
            let name = format!("{NAME_PREFIX}{id}");
            layer::update(s.doc()?, |l| {
                l.links.push(Anchor { id, name: name.clone(), sheet: place.sheet_name.clone(), range: place.range.a1(), lost: false })
            });
            let d = s.doc_mut()?;
            anchor::add_name(Arc::make_mut(&mut d.wb), &name, &place);
            if let Some(n) = Arc::make_mut(&mut d.wb).names.iter_mut().find(|n| n.name == name) {
                n.comment = "ArtCraft link source".into();
            }
            name
        }
    };
    let d = s.doc()?;
    let mut workbook_id = layer::get(d).workbook_id.clone();
    if workbook_id.is_empty() {
        workbook_id = new_uuid(uid);
        let w = workbook_id.clone();
        layer::update(d, |l| l.workbook_id = w);
    }
    let d = s.doc()?;
    let l = layer::get(d);
    let (values, text) = grid(&d.wb, &place);
    let private_cells = analysis::private_cells(&d.wb, &l);
    let private = place.range.iter().any(|c| private_cells.contains(&(place.sheet, c)));
    let abs_path = std::path::absolute(&path).map(|x| x.display().to_string()).unwrap_or(path);
    let payload = json!({
        "kind": "artcraft.link",
        "version": 1,
        "source": {
            "app": "GridCraft",
            "path": abs_path,
            "workbookId": workbook_id,
            "anchor": name,
            "sheet": place.sheet_name,
            "range": place.range.a1(),
            "label": label(&d.wb, &place),
        },
        "values": values,
        "text": text,
        "copiedAt": crate::now_ms(),
        "private": private,
    });
    let b64 = gridcraft_engine::io::base64_encode(serde_json::to_string(&payload).unwrap_or_default().as_bytes());
    let mut html = format!("<table data-artcraft-link=\"{b64}\">");
    for row in &text {
        html.push_str("<tr>");
        for t in row {
            html.push_str("<td>");
            html.push_str(&html_escape(t));
            html.push_str("</td>");
        }
        html.push_str("</tr>");
    }
    html.push_str("</table>");
    let tsv = text.iter().map(|r| r.join("\t")).collect::<Vec<_>>().join("\n");
    layer::update(d, |l| l.log(Actor::Human, "decision", format!("Copied {} as a linked source for slides", place.a1()), CMD, None, String::new()));
    Ok(json!({"html": html, "text": tsv, "payload": payload, "anchor": name}))
}

/// After a command: remembers where link names are, and puts back a name an undo took away.
pub fn refresh(d: &mut DocState) {
    let l = layer::get(d);
    if l.links.is_empty() {
        return;
    }
    let wb = d.wb.clone();
    let mut missing: Vec<(String, Place)> = vec![];
    let mut moved: Vec<(String, Option<Place>)> = vec![];
    for a in &l.links {
        match resolve_name(&wb, &a.name) {
            Ok(p) => {
                if p.sheet_name != a.sheet || p.range.a1() != a.range || a.lost {
                    moved.push((a.id.clone(), Some(p)));
                }
            }
            Err(true) => {
                if !a.lost {
                    moved.push((a.id.clone(), None));
                }
            }
            Err(false) => {
                if let Some(p) = anchor::resolve(&wb, &Anchor { name: String::new(), ..a.clone() }) {
                    missing.push((a.name.clone(), p));
                }
            }
        }
    }
    if !moved.is_empty() {
        layer::update(d, |l| {
            for (id, p) in &moved {
                if let Some(a) = l.links.iter_mut().find(|a| &a.id == id) {
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
        });
    }
    if !missing.is_empty() {
        let wb = Arc::make_mut(&mut d.wb);
        for (name, place) in &missing {
            anchor::add_name(wb, name, place);
        }
    }
}

// ---------------------------------------------------------------- link.read

fn source_p(p: &Json) -> (String, String, String, String, String) {
    let g = |k: &str| str_p(p, k).unwrap_or("").chars().take(4_096).collect::<String>();
    (g("path"), g("workbookId"), g("anchor"), g("sheet"), g("range"))
}

/// Where a link source is now in an open workbook.
fn locate(wb: &Workbook, anchor_name: &str, sheet: &str, range: &str, cmd: &str) -> Result<Place> {
    match resolve_name(wb, anchor_name) {
        Ok(p) => Ok(p),
        Err(true) => Err(bad(cmd, "the linked cells were deleted")),
        Err(false) => {
            let si = wb.sheet_index(sheet).ok_or_else(|| bad(cmd, format!("no sheet `{sheet}` and no name `{anchor_name}`")))?;
            let r = RangeRef::parse(&range.replace('$', "")).ok_or_else(|| bad(cmd, format!("not a range: `{range}`")))?;
            Ok(Place { sheet: si, sheet_name: sheet.to_string(), range: r })
        }
    }
}

fn read(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "link.read";
    let (path, wid, name, sheet, range) = source_p(p);
    let i = find_doc(s, &wid, &path).ok_or_else(|| bad(CMD, "that workbook isn't open here"))?;
    let d = s.documents().get(i).ok_or_else(|| bad(CMD, "that workbook isn't open here"))?;
    let place = locate(&d.wb, &name, &sheet, &range, CMD)?;
    if place.range.count() > MAX_CELLS {
        return Err(bad(CMD, "the linked range grew past 2,000 cells"));
    }
    let (values, text) = grid(&d.wb, &place);
    Ok(json!({"path": d.path, "sheet": place.sheet_name, "range": place.range.a1(), "values": values, "text": text}))
}

// ---------------------------------------------------------------- uses

/// Keeps the latest use per link.
fn merge(list: &mut Vec<LinkUse>, u: LinkUse) {
    match list.iter_mut().find(|x| x.link == u.link) {
        Some(x) if x.at <= u.at => *x = u,
        Some(_) => {}
        None => list.push(u),
    }
    if list.len() > MAX_USES {
        let extra = list.len() - MAX_USES;
        list.drain(..extra);
    }
}

fn add_uses(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "link.addUses";
    let list = p.get("uses").and_then(Json::as_array).ok_or_else(|| bad(CMD, "missing `uses`"))?;
    let mut added = 0;
    let active = s.active_index();
    for v in list.iter().take(MAX_USES) {
        let Ok(u) = serde_json::from_value::<LinkUse>(v.clone()) else { continue };
        if u.link.is_empty() {
            continue;
        }
        let Some(i) = find_doc(s, &u.workbook_id, &u.workbook) else { continue };
        s.set_active(i);
        if let Ok(d) = s.doc() {
            layer::update(d, |l| merge(&mut l.uses, u));
            added += 1;
        }
    }
    s.set_active(active);
    Ok(json!({"added": added}))
}

/// Uses of the links whose range touches `range` (all of them for `None`).
pub fn uses_at(wb: &Workbook, l: &layer::Layer, sheet: usize, range: Option<&RangeRef>) -> Vec<(Place, LinkUse)> {
    let mut out = vec![];
    for u in &l.uses {
        let Ok(p) = resolve_name(wb, &u.anchor) else { continue };
        if range.is_none_or(|r| p.sheet == sheet && p.range.intersects(r)) {
            out.push((p, u.clone()));
        }
    }
    out
}

fn uses(s: &mut Session, p: &Json) -> Result<Json> {
    let d = s.doc()?;
    let l = layer::get(d);
    let place = if str_p(p, "range").is_some() { Some(place_p(s, p, "link.uses")?) } else { None };
    let list = uses_at(&d.wb, &l, place.as_ref().map_or(0, |x| x.sheet), place.as_ref().map(|x| &x.range));
    Ok(Json::Array(list.into_iter().map(|(p, u)| json!({"range": p.a1(), "use": u})).collect()))
}

/// The registry of uses (one JSON object per line), written by DeckCraft.
pub fn registry_path() -> Option<PathBuf> {
    if let Some(p) = REGISTRY.with(|r| r.borrow().clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("ARTCRAFT_LINKS_FILE") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/ArtCraft/links.jsonl"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ArtCraft").join("links.jsonl"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".config"))).map(|d| d.join("artcraft/links.jsonl"))
    }
}

thread_local! {
    static REGISTRY: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// A registry file for this thread only (tests).
pub fn set_thread_registry(p: Option<PathBuf>) {
    REGISTRY.with(|r| *r.borrow_mut() = p);
}

/// After a workbook opens: the uses the registry knows for it (decks that still exist).
pub fn load_registry(d: &DocState) {
    let l = layer::get(d);
    if l.workbook_id.is_empty() && l.links.is_empty() {
        return;
    }
    let Some(path) = registry_path() else { return };
    if std::fs::metadata(&path).map_or(true, |m| m.len() > MAX_REGISTRY) {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let mine =
        |u: &LinkUse| (!l.workbook_id.is_empty() && u.workbook_id == l.workbook_id) || d.path.as_deref().is_some_and(|p| same_path(p, &u.workbook));
    let mut found: Vec<LinkUse> = vec![];
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Json>(line) else { continue };
        if v.get("event").and_then(Json::as_str).is_some_and(|e| e != "use") {
            continue;
        }
        let Ok(u) = serde_json::from_value::<LinkUse>(v) else { continue };
        if !u.link.is_empty() && mine(&u) {
            merge(&mut found, u);
        }
    }
    let decks: HashSet<String> = found.iter().map(|u| u.deck.clone()).filter(|p| Path::new(p).exists()).collect();
    found.retain(|u| decks.contains(&u.deck));
    if found.is_empty() {
        return;
    }
    layer::update(d, |l| {
        for u in found {
            merge(&mut l.uses, u);
        }
    });
}

// ---------------------------------------------------------------- reveal and go to slide

static REVEALED: AtomicBool = AtomicBool::new(false);

/// The UI scrolls to the selection and comes to the front once after a reveal.
pub fn take_revealed() -> bool {
    REVEALED.swap(false, Ordering::Relaxed)
}

/// Selects a place in the active document.
pub fn select(s: &mut Session, place: &Place) -> Result<()> {
    if s.doc()?.wb.active_sheet != place.sheet {
        s.execute("sheet.activate", json!({"sheet": place.sheet}))?;
    }
    s.execute("selection.set", json!({"range": place.range.a1()}))?;
    REVEALED.store(true, Ordering::Relaxed);
    Ok(())
}

fn reveal(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "link.reveal";
    let (path, wid, name, sheet, range) = source_p(p);
    if path.is_empty() && wid.is_empty() {
        return Err(bad(CMD, "missing `path`"));
    }
    let i = match find_doc(s, &wid, &path) {
        Some(i) => i,
        None => {
            s.execute("file.open", json!({"path": path}))?;
            find_doc(s, &wid, &path).unwrap_or_else(|| s.active_index())
        }
    };
    s.set_active(i);
    let place = locate(&s.doc()?.wb, &name, &sheet, &range, CMD)?;
    select(s, &place)?;
    Ok(json!({"path": s.doc()?.path, "sheet": place.sheet_name, "range": place.range.a1()}))
}

/// `--goto <name | Sheet!A1>`: a defined name (hidden ones too) or a place.
pub fn goto(s: &mut Session, target: &str) -> Result<Json> {
    let d = s.doc()?;
    let place = match resolve_name(&d.wb, target.trim()) {
        Ok(p) => p,
        Err(_) => anchor::parse_place(&d.wb, target, d.wb.active_sheet).ok_or_else(|| bad("--goto", format!("no name or cell `{target}`")))?,
    };
    select(s, &place)?;
    Ok(json!({"range": place.a1()}))
}

fn goto_slide(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "link.gotoSlide";
    let id = text_p(p, "link", CMD)?;
    let l = layer::get(s.doc()?);
    let u = l.uses.iter().find(|u| u.link == id).cloned().ok_or_else(|| bad(CMD, format!("no slide uses link `{id}`")))?;
    if u.deck.is_empty() {
        return Err(bad(CMD, "the use names no deck"));
    }
    deckcraft::goto(&u).map_err(|e| bad(CMD, e))?;
    Ok(json!({"deck": u.deck, "slide": u.slide}))
}

#[cfg(not(target_arch = "wasm32"))]
mod deckcraft {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::Duration;

    use serde_json::json;

    use crate::layer::LinkUse;

    fn addr() -> SocketAddr {
        std::env::var("DECKCRAFT_CONTROL").ok().and_then(|a| a.parse().ok()).unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 7985)))
    }

    /// One `link.goto` call on DeckCraft's control channel.
    fn call(link: &str) -> Result<(), String> {
        let mut c = TcpStream::connect_timeout(&addr(), Duration::from_millis(250)).map_err(|e| e.to_string())?;
        let _ = c.set_read_timeout(Some(Duration::from_secs(3)));
        let msg = json!({"id": 1, "method": "engine.execute", "params": {"command": "link.goto", "params": {"link": link}}});
        writeln!(c, "{msg}").map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(c).read_line(&mut line).map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if v.get("error").is_some_and(|e| !e.is_null()) || v.get("ok") == Some(&json!(false)) {
            return Err(v.get("error").map(|e| e.to_string()).unwrap_or_else(|| "refused".into()));
        }
        Ok(())
    }

    /// Selects the slide; when DeckCraft isn't running, starts it on the deck and asks again once
    /// it is up. Works in the background: the UI never waits for another app.
    pub fn goto(u: &LinkUse) -> Result<(), String> {
        let link = u.link.clone();
        let deck = u.deck.clone();
        std::thread::Builder::new()
            .name("deckcraft-goto".into())
            .spawn(move || {
                if call(&link).is_ok() {
                    return;
                }
                let bin = std::env::var("DECKCRAFT_BIN").unwrap_or_else(|_| "deckcraft".into());
                if let Err(e) = std::process::Command::new(&bin).arg(&deck).spawn() {
                    log::warn!("go to slide: can't start {bin} {deck}: {e}");
                    return;
                }
                for _ in 0..30 {
                    std::thread::sleep(Duration::from_secs(1));
                    if call(&link).is_ok() {
                        return;
                    }
                }
                log::warn!("go to slide: DeckCraft didn't answer on {}", addr());
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[cfg(target_arch = "wasm32")]
mod deckcraft {
    use crate::layer::LinkUse;
    pub fn goto(_: &LinkUse) -> Result<(), String> {
        Err("going to a slide needs the desktop apps".into())
    }
}
