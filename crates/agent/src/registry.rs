//! The AItoCraft registry (protocol v1, §1): `<ArtCraft agents folder>/gridcraft-<pid>.json` says
//! this app is running, where its MCP server is, and which documents are open, with their ids,
//! titles, paths and listeners. No content, no client data.
//!
//! Written atomically (a temporary file in the same folder, then a rename) when something changes
//! (at most once a second, from the UI's frames) and every 10 s as a heartbeat (from its own
//! thread, so an idle or hidden window stays listed); deleted on exit. Readers treat a file as
//! stale when its pid isn't alive or its heartbeat is older than 30 s.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gridcraft_engine::Session;
use serde_json::{Value as Json, json};

pub const PROTOCOL: &str = "aitocraft/1";
/// A heartbeat older than this makes a file stale.
pub const STALE_MS: u64 = 30_000;
const HEARTBEAT_MS: u64 = 10_000;
const DEBOUNCE_MS: u64 = 1_000;

/// The shared folder of all Craft apps.
pub fn folder() -> Option<PathBuf> {
    if let Some(p) = OVERRIDE.with(|o| o.borrow().clone()) {
        return Some(p);
    }
    if let Some(p) = std::env::var_os("AITOCRAFT_REGISTRY_DIR").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/ArtCraft/agents"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ArtCraft").join("agents"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("ArtCraft/agents"))
    }
}

thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Another folder for this thread (tests).
pub fn set_folder(p: Option<PathBuf>) {
    OVERRIDE.with(|o| *o.borrow_mut() = p);
}

pub fn file_name() -> String {
    format!("gridcraft-{}.json", std::process::id())
}

/// This app's entry.
pub fn contents(s: &Session, mcp: &str, now: u64) -> Json {
    json!({
        "protocol": PROTOCOL,
        "app": "gridcraft",
        "appTitle": "GridCraft",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "mcp": mcp,
        "heartbeat": now,
        "documents": crate::docs::documents(s),
    })
}

/// Writes `v` as `name` in `dir`: a temporary file, then a rename.
pub fn write(dir: &Path, name: &str, v: &Json) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.tmp"));
    let text = serde_json::to_string_pretty(v).unwrap_or_default();
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// A process with this pid is running.
pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(unix)]
    {
        std::process::Command::new("/bin/kill").args(["-0", &pid.to_string()]).output().is_ok_and(|o| o.status.success())
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// A registry entry no reader should trust.
pub fn is_stale(v: &Json, now: u64, alive: impl Fn(u32) -> bool) -> bool {
    let heartbeat = v.get("heartbeat").and_then(Json::as_u64).unwrap_or(0);
    let pid = v.get("pid").and_then(Json::as_u64).and_then(|p| u32::try_from(p).ok()).unwrap_or(0);
    now.saturating_sub(heartbeat) > STALE_MS || pid == 0 || !alive(pid)
}

/// Removes stale GridCraft entries (left by a crash) from `dir`.
pub fn sweep(dir: &Path, now: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !(name.starts_with("gridcraft-") && name.ends_with(".json")) || name == file_name() {
            continue;
        }
        let stale =
            std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Json>(&t).ok()).is_none_or(|v| is_stale(&v, now, pid_alive));
        if stale {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// What the app last wrote, and when.
struct State {
    mcp: String,
    signature: String,
    written: u64,
    /// The entry last written, for the heartbeat thread.
    last: Option<Json>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Starts announcing this app (its MCP server at `mcp`); clears stale entries left by crashes.
pub fn enable(mcp: &str) {
    if let Ok(mut g) = STATE.lock() {
        *g = Some(State { mcp: mcp.to_string(), signature: String::new(), written: 0, last: None });
    }
    // The heartbeat doesn't wait for frames: an idle or hidden window draws none, and the hub
    // would take the app for gone after 30 s.
    static BEATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !cfg!(test) && !BEATING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let _ = std::thread::Builder::new().name("aitocraft-heartbeat".into()).spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                if !beat(crate::now_ms()) {
                    BEATING.store(false, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            }
        });
    }
    if let Some(dir) = folder() {
        sweep(&dir, crate::now_ms());
    }
}

/// What changes the entry, apart from times.
fn signature(v: &Json) -> String {
    let docs: Vec<Json> = v
        .get("documents")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .map(|d| {
                    let mut d = d.clone();
                    if let Some(ls) = d.get_mut("listeners").and_then(Json::as_array_mut) {
                        for l in ls {
                            if let Some(o) = l.as_object_mut() {
                                o.remove("lastSeen");
                            }
                        }
                    }
                    d
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::to_string(&docs).unwrap_or_default()
}

/// Called every frame: writes the entry when it changed (at most once a second) and as a
/// heartbeat. Does nothing until [`enable`].
pub fn tick(s: &Session) {
    let now = crate::now_ms();
    let Ok(mut g) = STATE.lock() else { return };
    let Some(st) = g.as_mut() else { return };
    if now.saturating_sub(st.written) < DEBOUNCE_MS {
        return;
    }
    let v = contents(s, &st.mcp, now);
    let sig = signature(&v);
    if sig == st.signature && now.saturating_sub(st.written) < HEARTBEAT_MS {
        return;
    }
    let Some(dir) = folder() else { return };
    match write(&dir, &file_name(), &v) {
        Ok(_) => {
            st.signature = sig;
            st.written = now;
            st.last = Some(v);
        }
        Err(e) => {
            log::warn!("AItoCraft registry {}: {e}", dir.display());
            st.written = now;
        }
    }
}

/// Rewrites the last entry with a fresh heartbeat when it's due; `false` once the entry is gone.
fn beat(now: u64) -> bool {
    let Ok(mut g) = STATE.lock() else { return false };
    let Some(st) = g.as_mut() else { return false };
    if now.saturating_sub(st.written) < HEARTBEAT_MS {
        return true;
    }
    let (Some(mut v), Some(dir)) = (st.last.clone(), folder()) else { return true };
    v["heartbeat"] = serde_json::json!(now);
    if write(&dir, &file_name(), &v).is_ok() {
        st.written = now;
        st.last = Some(v);
    }
    true
}

/// On exit: the entry goes.
pub fn remove() {
    // The state first: the heartbeat thread stops before the file goes, and doesn't bring it back.
    if let Ok(mut g) = STATE.lock() {
        *g = None;
    }
    if let Some(dir) = folder() {
        let _ = std::fs::remove_file(dir.join(file_name()));
    }
}
