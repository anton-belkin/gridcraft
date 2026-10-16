//! Connect Cowork: puts GridCraft into Claude Desktop's local MCP servers
//! (`claude_desktop_config.json`, entry `gridcraft` → `gridcraft --agent-stdio`), so Claude Desktop
//! and Cowork reach the open workbook. In Cowork there is no background monitor: the agent waits
//! for questions with `wait_for_questions`.
//!
//! Claude Desktop keeps that file in memory and writes it back when it quits, so an edit made
//! while it runs is lost: the entry is written only while Claude is closed. The file is backed
//! up first, and no other server or setting is touched.

use std::path::PathBuf;

use gridcraft_engine::cmd::CommandSpec;
use gridcraft_engine::{Result, Session};
use serde_json::{Value as Json, json};

use crate::commands::{bad, spec, str_p};

/// The server's name in Claude Desktop (WordCraft and DeckCraft use `wordcraft` and `deckcraft`).
pub const SERVER_NAME: &str = "gridcraft";

pub static SPECS: &[CommandSpec] = &[spec!(
    query "agent.connectCowork",
    "Connect Cowork",
    "{exe?: path of gridcraft (default: this app)} — adds `gridcraft` to Claude Desktop's MCP servers (backing the file up first); refused while Claude Desktop runs, since it rewrites the file when it quits",
    connect_cmd
)];

thread_local! {
    static CONFIG_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static RUNNING: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Overrides for tests: the config file, and whether Claude Desktop "is running".
pub fn set_thread_overrides(path: Option<PathBuf>, running: Option<bool>) {
    CONFIG_PATH.with(|c| *c.borrow_mut() = path);
    RUNNING.with(|r| r.set(running));
}

/// Claude Desktop's config file.
pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = CONFIG_PATH.with(|c| c.borrow().clone()) {
        return Some(p);
    }
    // Tests never touch the real file.
    if cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/Claude/claude_desktop_config.json"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Claude").join("claude_desktop_config.json"))
    } else {
        home.map(|h| h.join(".config/Claude/claude_desktop_config.json"))
    }
}

/// Claude Desktop is open.
pub fn claude_running() -> bool {
    if let Some(r) = RUNNING.with(std::cell::Cell::get) {
        return r;
    }
    if cfg!(test) {
        return true;
    }
    if cfg!(target_os = "macos") {
        // The app's own process (its helpers are "Claude Helper.app/…", Claude Code is lowercase).
        std::process::Command::new("/usr/bin/pgrep").args(["-f", "Claude.app/Contents/MacOS/Claude"]).output().is_ok_and(|o| o.status.success())
    } else {
        false
    }
}

/// GridCraft is already in Claude Desktop's config.
pub fn connected() -> bool {
    config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        .is_some_and(|v| v.pointer(&format!("/mcpServers/{SERVER_NAME}")).is_some())
}

/// What connecting did.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Written; the backup of the previous file, if there was one.
    Written { path: PathBuf, backup: Option<PathBuf> },
    /// Already there with the same command.
    Unchanged { path: PathBuf },
    /// Claude Desktop is open: quit it first.
    ClaudeRunning,
}

/// Adds (or updates) the `gridcraft` entry, touching nothing else.
pub fn connect(exe: &str) -> std::result::Result<Outcome, String> {
    let path = config_path().ok_or("no folder for Claude Desktop's settings")?;
    let entry = json!({"command": exe, "args": ["--agent-stdio"]});
    let mut config: Json = match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => {
            serde_json::from_str(&text).map_err(|e| format!("{} isn't valid JSON ({e}): fix or move it first", path.display()))?
        }
        Ok(_) => json!({}),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !config.is_object() {
        return Err(format!("{} isn't a JSON object: fix or move it first", path.display()));
    }
    if config.pointer(&format!("/mcpServers/{SERVER_NAME}")) == Some(&entry) {
        return Ok(Outcome::Unchanged { path });
    }
    if claude_running() {
        return Ok(Outcome::ClaudeRunning);
    }
    let backup = if path.exists() {
        let b = path.with_file_name(format!("claude_desktop_config.json.bak-{}", crate::now_ms()));
        std::fs::copy(&path, &b).map_err(|e| format!("backing up {}: {e}", path.display()))?;
        Some(b)
    } else {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        None
    };
    let servers = config.as_object_mut().map(|o| o.entry("mcpServers").or_insert_with(|| json!({})));
    match servers {
        Some(Json::Object(m)) => {
            m.insert(SERVER_NAME.into(), entry);
        }
        _ => return Err("`mcpServers` in Claude Desktop's settings isn't an object".into()),
    }
    let text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    gridcraft_engine::io::write_file(&path.display().to_string(), text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(Outcome::Written { path, backup })
}

fn connect_cmd(_s: &mut Session, p: &Json) -> Result<Json> {
    const CMD: &str = "agent.connectCowork";
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(CMD, "only a person connects Claude Desktop"));
    }
    let exe = match str_p(p, "exe") {
        Some(e) => e.to_string(),
        None => std::env::current_exe().map(|x| x.display().to_string()).map_err(|e| bad(CMD, e.to_string()))?,
    };
    Ok(match connect(&exe).map_err(|e| bad(CMD, e))? {
        Outcome::Written { path, backup } => json!({
            "written": true,
            "path": path.display().to_string(),
            "backup": backup.map(|b| b.display().to_string()),
            "message": "GridCraft is in Claude Desktop's MCP servers (gridcraft). Open Claude: in Cowork and chats, ask it to watch this workbook (it waits for your questions with wait_for_questions).",
        }),
        Outcome::Unchanged { path } => json!({
            "written": false,
            "path": path.display().to_string(),
            "message": "GridCraft is already connected to Claude Desktop. If Claude doesn't list it, quit Claude with ⌘Q and open it again.",
        }),
        Outcome::ClaudeRunning => json!({
            "written": false,
            "claudeRunning": true,
            "message": "Claude Desktop is open, and it rewrites its settings when it quits: an edit now would be lost. Quit Claude with ⌘Q (not just the window), then press Connect Cowork again, then open Claude.",
        }),
    })
}
