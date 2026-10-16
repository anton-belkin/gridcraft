//! Safe file mode: privacy at rest.
//!
//! With the mode on, every save writes client data as stand-ins (the same ones the agent sees:
//! same sign and magnitude, text as `{{private:B2}}`, everything computed from them recalculated),
//! and the real values go into the file sealed: AES-256-CTR with an HMAC-SHA256, under a key
//! that never enters the workbook. The key lives on this computer, in a key folder outside the
//! workbook's (or, opted in with `GRIDCRAFT_AGENT_KEYCHAIN=1`, in the macOS keychain). Opening the
//! file here restores the real values; anywhere else it opens with the stand-ins.
//!
//! "Export for standard Excel" writes the real values, with nothing sealed.
//!
//! The document in memory always keeps the real values: the save swaps a safe copy in for the
//! length of the command and puts the real workbook back right after (and before the next
//! command, should the save have failed half way).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::core::{CellRef, Value};
use gridcraft_engine::model::{Cell, Workbook};
use gridcraft_engine::{DocState, Result, Session};
use serde_json::{Value as Json, json};

use crate::analysis;
use crate::commands::{actor_p, bad, spec, str_p};
use crate::layer::{self, Actor, Layer, SafeFile, Sealed};

pub static SPECS: &[CommandSpec] = &[
    spec!(change "agent.safeMode", "Safe File Mode", "{on?: bool (default true)} — saves write stand-ins for client data and seal the real values under a key kept on this computer (only a person can turn it off)", safe_mode),
    spec!(query "agent.exportExcel", "Export for Standard Excel", "{path?: (default \"<name> (Excel).xlsx\" beside the workbook)} — a copy with the real values and nothing sealed", export_excel),
    spec!(change "agent.unlock", "Unlock Client Data", "{} — restores the real values of a safe file whose key is on this computer", unlock),
];

const ALG: &str = "aes-256-ctr+hmac-sha256";
/// Most sealed cells (the payload stays well inside the layer's size limit).
const MAX_SEALED: usize = 200_000;

// ---------------------------------------------------------------- crypto

mod crypto {
    use aes::cipher::{BlockCipherEncrypt, KeyInit};
    use sha2::{Digest, Sha256};

    pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p);
        }
        h.finalize().into()
    }

    pub fn hmac(key: &[u8; 32], parts: &[&[u8]]) -> [u8; 32] {
        let mut k = [0u8; 64];
        k[..32].copy_from_slice(key);
        let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        let mut inner_parts: Vec<&[u8]> = vec![&ipad];
        inner_parts.extend_from_slice(parts);
        let inner = sha256(&inner_parts);
        sha256(&[&opad, &inner])
    }

    /// AES-256 in counter mode (encrypts and decrypts).
    pub fn ctr(key: &[u8; 32], nonce: &[u8; 12], data: &mut [u8]) -> Result<(), String> {
        let c = aes::Aes256::new_from_slice(key).map_err(|e| e.to_string())?;
        for (i, chunk) in data.chunks_mut(16).enumerate() {
            let mut block = [0u8; 16];
            block[..12].copy_from_slice(nonce);
            block[12..].copy_from_slice(&(i as u32).to_be_bytes());
            c.encrypt_block(aes::Block::cast_from_core_mut(&mut block));
            for (d, k) in chunk.iter_mut().zip(block) {
                *d ^= k;
            }
        }
        Ok(())
    }

    pub fn eq(a: &[u8], b: &[u8]) -> bool {
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

fn enc_key(master: &[u8; 32]) -> [u8; 32] {
    crypto::sha256(&[b"gridcraft agent layer: encryption", master])
}

fn mac_key(master: &[u8; 32]) -> [u8; 32] {
    crypto::sha256(&[b"gridcraft agent layer: authentication", master])
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| s.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok())).collect()
}

/// Seals `payload` under `master`.
pub fn seal_bytes(master: &[u8; 32], nonce: [u8; 12], payload: &[u8], cells: usize) -> std::result::Result<Sealed, String> {
    let mut data = payload.to_vec();
    crypto::ctr(&enc_key(master), &nonce, &mut data)?;
    let mac = crypto::hmac(&mac_key(master), &[ALG.as_bytes(), &nonce, &data]);
    let b64 = gridcraft_engine::io::base64_encode;
    Ok(Sealed { alg: ALG.into(), nonce: b64(&nonce), data: b64(&data), mac: b64(&mac), cells })
}

/// Opens a sealed payload; an altered one, or the wrong key, is refused.
pub fn open_bytes(master: &[u8; 32], s: &Sealed) -> std::result::Result<Vec<u8>, String> {
    if s.alg != ALG {
        return Err(format!("unknown sealing `{}`", s.alg));
    }
    let b64 = gridcraft_engine::io::base64_decode;
    let (Some(nonce), Some(mut data), Some(mac)) = (b64(&s.nonce), b64(&s.data), b64(&s.mac)) else {
        return Err("the sealed data is damaged".into());
    };
    let nonce: [u8; 12] = nonce.try_into().map_err(|_| "the sealed data is damaged".to_string())?;
    let want = crypto::hmac(&mac_key(master), &[ALG.as_bytes(), &nonce, &data]);
    if !crypto::eq(&want, &mac) {
        return Err("the key doesn't open this file (or the sealed data was changed)".into());
    }
    crypto::ctr(&enc_key(master), &nonce, &mut data)?;
    Ok(data)
}

// ---------------------------------------------------------------- the key store

static KEY_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
thread_local! {
    static THREAD_KEY_DIR: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Where keys are kept (the app may set it; `GRIDCRAFT_AGENT_KEYS` too).
pub fn set_key_dir(p: PathBuf) {
    if let Ok(mut g) = KEY_DIR.lock() {
        *g = Some(p);
    }
}

/// A key folder for this thread only (tests).
pub fn set_thread_key_dir(p: Option<PathBuf>) {
    THREAD_KEY_DIR.with(|t| *t.borrow_mut() = p);
}

pub fn key_dir() -> Option<PathBuf> {
    if let Some(p) = THREAD_KEY_DIR.with(|t| t.borrow().clone()) {
        return Some(p);
    }
    if let Some(p) = KEY_DIR.lock().ok().and_then(|g| g.clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("GRIDCRAFT_AGENT_KEYS") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/GridCraft/agent-keys"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("GridCraft").join("agent-keys"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("gridcraft/agent-keys"))
    }
}

fn valid_id(id: &str) -> bool {
    id.len() >= 8 && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[cfg(not(target_arch = "wasm32"))]
mod store {
    use std::path::PathBuf;

    const SERVICE: &str = "GridCraft agent layer";

    pub fn random<const N: usize>() -> Result<[u8; N], String> {
        let mut b = [0u8; N];
        getrandom::fill(&mut b).map_err(|e| format!("no secure random numbers: {e}"))?;
        Ok(b)
    }

    fn keychain() -> bool {
        cfg!(target_os = "macos") && std::env::var("GRIDCRAFT_AGENT_KEYCHAIN").is_ok_and(|v| v == "1")
    }

    fn file(dir: &std::path::Path, id: &str) -> PathBuf {
        dir.join(format!("{id}.key"))
    }

    pub fn save(dir: Option<PathBuf>, id: &str, key: &[u8; 32]) -> Result<String, String> {
        if keychain() {
            let ok = std::process::Command::new("/usr/bin/security")
                .args(["add-generic-password", "-U", "-s", SERVICE, "-a", id, "-w", &super::hex(key)])
                .output()
                .is_ok_and(|o| o.status.success());
            if ok {
                return Ok("the macOS keychain".into());
            }
        }
        let dir = dir.ok_or("no folder for keys (set GRIDCRAFT_AGENT_KEYS)")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = file(&dir, id);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            opts.mode(0o600);
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        use std::io::Write;
        let mut f = opts.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(super::hex(key).as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path.display().to_string())
    }

    pub fn load(dir: Option<PathBuf>, id: &str) -> Option<[u8; 32]> {
        let text = dir.and_then(|d| std::fs::read_to_string(file(&d, id)).ok()).or_else(|| {
            if !cfg!(target_os = "macos") {
                return None;
            }
            let o = std::process::Command::new("/usr/bin/security").args(["find-generic-password", "-s", SERVICE, "-a", id, "-w"]).output().ok()?;
            if o.status.success() { String::from_utf8(o.stdout).ok() } else { None }
        })?;
        super::unhex(&text)?.try_into().ok()
    }
}

#[cfg(target_arch = "wasm32")]
mod store {
    use std::path::PathBuf;
    pub fn random<const N: usize>() -> Result<[u8; N], String> {
        Err("safe files need the desktop app (no key store in the browser)".into())
    }
    pub fn save(_: Option<PathBuf>, _: &str, _: &[u8; 32]) -> Result<String, String> {
        Err("safe files need the desktop app (no key store in the browser)".into())
    }
    pub fn load(_: Option<PathBuf>, _: &str) -> Option<[u8; 32]> {
        None
    }
}

/// A new key in the store: its id and where it went.
pub fn new_key() -> std::result::Result<(String, [u8; 32], String), String> {
    let id = format!("k{}", hex(&store::random::<10>()?));
    let key = store::random::<32>()?;
    let at = store::save(key_dir(), &id, &key)?;
    Ok((id, key, at))
}

pub fn load_key(id: &str) -> Option<[u8; 32]> {
    if !valid_id(id) {
        return None;
    }
    store::load(key_dir(), id)
}

// ---------------------------------------------------------------- sealing a workbook

/// The real values of the private constants of `wb`.
fn payload(wb: &Workbook, l: &Layer) -> (Vec<u8>, usize) {
    let mut cells: Vec<Json> = vec![];
    let mut private: Vec<_> = analysis::private_cells(wb, l).into_iter().collect();
    private.sort();
    for (si, c) in private.into_iter().take(MAX_SEALED) {
        let Some(sh) = wb.sheet(si) else { continue };
        let Some(cell) = sh.cell(c) else { continue };
        if cell.formula.is_some() {
            continue;
        }
        let v = match &cell.value {
            Value::Number(n) => json!({"n": n}),
            Value::Text(t) => json!({"t": t.as_ref()}),
            Value::Bool(b) => json!({"b": b}),
            _ => continue,
        };
        let mut o = v;
        o["s"] = json!(sh.name);
        o["c"] = json!(c.a1());
        cells.push(o);
    }
    let n = cells.len();
    (serde_json::to_vec(&cells).unwrap_or_default(), n)
}

/// The workbook as a safe save writes it: client data as stand-ins, the real values sealed in
/// the layer. Makes a new key when the document's is gone.
pub fn safe_copy(d: &DocState, l: &Layer) -> std::result::Result<Workbook, String> {
    let mut l2 = l.clone();
    let mut safe = l.safe.clone().unwrap_or_default();
    let key = match load_key(&safe.key_id) {
        Some(k) => k,
        None => {
            let (id, k, at) = new_key()?;
            log::warn!("safe file: the key `{}` is gone; sealing under a new one ({at})", safe.key_id);
            safe.key_id = id;
            k
        }
    };
    let (bytes, n) = payload(&d.wb, l);
    safe.sealed = Some(seal_bytes(&key, store::random::<12>()?, &bytes, n)?);
    l2.safe = Some(safe);
    let (mut wb, _) = analysis::masked(d, l);
    layer::write_into(&mut wb, &l2);
    Ok(wb)
}

/// Puts sealed values back into the document. `Ok(cells)` or why not.
pub fn unseal(d: &mut DocState) -> std::result::Result<usize, String> {
    let l = layer::get(d);
    let Some(safe) = l.safe.clone() else { return Ok(0) };
    let Some(sealed) = safe.sealed else { return Ok(0) };
    let key = load_key(&safe.key_id).ok_or_else(|| format!("the key `{}` isn't on this computer", safe.key_id))?;
    let bytes = open_bytes(&key, &sealed)?;
    let cells: Vec<Json> = serde_json::from_slice(&bytes).map_err(|e| format!("the sealed data is damaged: {e}"))?;
    let mut keys = vec![];
    {
        let wb = Arc::make_mut(&mut d.wb);
        for o in cells.iter().take(MAX_SEALED) {
            let (Some(sheet), Some(c)) = (o.get("s").and_then(Json::as_str), o.get("c").and_then(Json::as_str).and_then(CellRef::parse)) else {
                continue;
            };
            let Some(si) = wb.sheet_index(sheet) else { continue };
            let value = if let Some(n) = o.get("n").and_then(Json::as_f64) {
                Value::Number(n)
            } else if let Some(t) = o.get("t").and_then(Json::as_str) {
                Value::text(t)
            } else if let Some(b) = o.get("b").and_then(Json::as_bool) {
                Value::Bool(b)
            } else {
                continue;
            };
            let Some(sh) = wb.sheets.get_mut(si) else { continue };
            let style = sh.cell(c).map(|x| x.style).unwrap_or_default();
            Arc::make_mut(sh).set_cell(c, Cell { value, formula: None, style });
            keys.push((si, c));
        }
    }
    let DocState { wb, calc, .. } = d;
    calc.cells_changed(Arc::make_mut(wb), &keys);
    d.saved = d.wb.clone();
    layer::update(d, |l| {
        if let Some(s) = l.safe.as_mut() {
            s.sealed = None;
        }
    });
    Ok(keys.len())
}

// ---------------------------------------------------------------- the save swap

thread_local! {
    /// Real workbooks put aside while a safe copy is being saved: (document, real, safe copy).
    /// Per thread: a session and its observers run on one thread.
    static PENDING: std::cell::RefCell<Vec<(u64, Arc<Workbook>, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Safe mode is on and the real values are here (not locked away).
pub fn is_on(l: &Layer) -> bool {
    l.safe.as_ref().is_some_and(|s| s.sealed.is_none())
}

/// Before a save: swaps the safe copy in. `false` when the mode is off.
pub fn swap_in(s: &mut Session) -> bool {
    let Some(d) = s.active_mut() else { return false };
    let l = layer::get(d);
    if !is_on(&l) {
        return false;
    }
    let wb = match safe_copy(d, &l) {
        Ok(wb) => wb,
        Err(e) => {
            // No key store (the browser): never write the real values; stand-ins only.
            log::error!("safe file: {e}; saving stand-ins without the sealed values");
            let mut l2 = (*l).clone();
            l2.safe = None;
            let (mut wb, _) = analysis::masked(d, &l);
            layer::write_into(&mut wb, &l2);
            wb
        }
    };
    let safe = Arc::new(wb);
    let real = std::mem::replace(&mut d.wb, safe.clone());
    PENDING.with(|p| p.borrow_mut().push((d.uid, real, Arc::as_ptr(&safe) as usize)));
    true
}

/// Puts real workbooks back (after the save, and before any later command).
pub fn restore(s: &mut Session) {
    let mut list = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if list.is_empty() {
        return;
    }
    let active = s.active_index();
    for i in 0..s.documents().len() {
        s.set_active(i);
        let Some(d) = s.active_mut() else { continue };
        for (_, real, safe) in list.iter().filter(|e| e.0 == d.uid) {
            if Arc::as_ptr(&d.saved) as usize == *safe {
                d.saved = real.clone();
            }
            if Arc::as_ptr(&d.wb) as usize == *safe {
                d.wb = real.clone();
            }
        }
    }
    s.set_active(active);
    // Every entry is settled now (a document closed meanwhile has nothing to put back).
    list.clear();
}

// ---------------------------------------------------------------- commands

fn safe_mode(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.safeMode";
    let on = p.get("on").and_then(Json::as_bool).unwrap_or(true);
    let actor = actor_p(p, Actor::Human);
    let d = s.doc()?;
    let l = layer::get(d);
    if !on {
        if actor == Actor::Agent {
            return Err(bad(CMD, "only a person can turn safe file mode off"));
        }
        if l.safe.as_ref().is_some_and(|x| x.sealed.is_some()) {
            return Err(bad(CMD, "the client data is still locked (its key isn't on this computer): unlock it first"));
        }
        layer::update(d, |l| {
            l.safe = None;
            l.log(Actor::Human, "decision", "Safe file mode off: saves write the real values".into(), CMD, None, String::new());
        });
        return Ok(json!({"on": false}));
    }
    if let Some(x) = &l.safe {
        return Ok(json!({"on": true, "key": x.key_id, "locked": x.sealed.is_some()}));
    }
    // Keys never go inside the workbook's own folder.
    if let (Some(dir), Some(path)) = (key_dir(), d.path.as_deref()) {
        let folder = std::path::Path::new(path).parent().map(std::path::Path::to_path_buf);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Some(f) = folder.filter(|f| !f.as_os_str().is_empty() && Some(f) != home.as_ref() && f.parent().is_some())
            && dir.starts_with(&f)
        {
            return Err(bad(CMD, format!("the key folder {} is inside the workbook's folder: move one of them", dir.display())));
        }
    }
    let (id, _, at) = new_key().map_err(|e| bad(CMD, e))?;
    let n = analysis::private_cells(&d.wb, &l).len();
    layer::update(s.doc()?, |l| {
        l.safe = Some(SafeFile { key_id: id.clone(), sealed: None });
        l.log(
            actor,
            "decision",
            format!("Safe file mode on: client data ({n} cells) is saved as stand-ins, the real values sealed"),
            CMD,
            None,
            String::new(),
        );
    });
    Ok(json!({"on": true, "key": id, "keyStore": at, "privateCells": n}))
}

fn unlock(s: &mut Session, _p: &Json) -> Result<Json> {
    let d = s.doc_mut()?;
    let n = unseal(d).map_err(|e| bad("agent.unlock", e))?;
    Ok(json!({"cells": n}))
}

fn export_excel(s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.exportExcel";
    let d = s.doc()?;
    let l = layer::get(d);
    if l.safe.as_ref().is_some_and(|x| x.sealed.is_some()) {
        return Err(bad(CMD, "the client data is locked (its key isn't on this computer): there are no real values to export"));
    }
    let path = match str_p(p, "path") {
        Some(p) => p.to_string(),
        None => {
            let doc = d.path.as_deref().ok_or_else(|| bad(CMD, "the workbook has no file yet: give `path`"))?;
            let pb = std::path::Path::new(doc);
            let stem = pb.file_stem().and_then(|x| x.to_str()).unwrap_or("Book");
            pb.with_file_name(format!("{stem} (Excel).xlsx")).display().to_string()
        }
    };
    let mut wb: Workbook = (*d.wb).clone();
    let mut l2 = (*l).clone();
    l2.safe = None;
    layer::write_into(&mut wb, &l2);
    let bytes = gridcraft_engine::io::save_bytes(&wb, &path)?;
    gridcraft_engine::io::write_file(&path, &bytes)?;
    let actor = actor_p(p, Actor::Human);
    let text = format!("Exported for standard Excel, with the real client data: {path}");
    layer::update(s.doc()?, |l| l.log(actor, "decision", text, CMD, None, String::new()));
    Ok(json!({"path": path, "bytes": bytes.len()}))
}
