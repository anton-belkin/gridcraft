//! What the agent knows about a document: the Agent tab's Context group (desktop only).
//!
//! - **No context**: only this document; the background client runs in a neutral, empty folder
//!   (optionally allowed WebSearch and WebFetch).
//! - **Claude Cowork**: the person's Cowork chat, its project and memory, through AItoCraft and
//!   the `wait_for_questions` loop.
//! - **Claude Code**: the person's own Claude Code session (the AItoCraft channel), or a background
//!   client in a project folder they chose (its CLAUDE.md, memory and files join the context).
//!
//! The choice is per document, kept in its agent layer; a document that has none follows the last
//! choice made in the app. [`sync`] turns the active document's choice into the runner's mode and
//! configuration. A person's change is undoable like their other layer actions.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use gridcraft_engine::Session;
use gridcraft_engine::cmd::CommandSpec;
use serde_json::{Value as Json, json};

use crate::commands::{bad, spec, str_p};
use crate::layer::{self, AgentContext, CodeVia, ContextKind, Layer};
use crate::runner::{self, Mode};

pub static SPECS: &[CommandSpec] = &[
    spec!(
        change "agent.agentContext",
        "Agent Context",
        "{kind?: none|cowork|code|codex|off, via?: session|folder, web?: bool, folder?: path} — what the agent knows about this document: only the document (a background client; `web` also allows web search), your Cowork chat, or your Claude Code session or a project folder. A person's choice, saved in the document → the context",
        context_cmd
    ),
    spec!(query "agent.codexConnect", "Connect Codex", "{} — connects this app's Codex question worker (a person's action)", codex_connect),
    spec!(change "agent.codexOrigin", "Codex Origin Chat", "{threadId?, title?} — records the Desktop chat that created this file; no params reads the saved origin", codex_origin),
    spec!(query "agent.codexRoute", "Codex Delivery Status", "{status?, threadId?, title?, detail?} — live Desktop delivery status; not saved in the workbook", codex_route),
    spec!(query "agent.codexOpenChat", "Open Codex Chat", "{} — opens this document's actual Desktop chat (a person's action)", codex_open_chat),
];

impl ContextKind {
    pub fn parse(s: &str) -> Option<ContextKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "no context" | "document" => Some(ContextKind::None),
            "cowork" | "claude cowork" => Some(ContextKind::Cowork),
            "code" | "claude code" => Some(ContextKind::Code),
            "codex" => Some(ContextKind::Codex),
            "off" => Some(ContextKind::Off),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            ContextKind::None => "none",
            ContextKind::Cowork => "cowork",
            ContextKind::Code => "code",
            ContextKind::Codex => "codex",
            ContextKind::Off => "off",
        }
    }
    /// The name on the tab, the line under it, and what the agent knows (the chips).
    pub fn label(self) -> (&'static str, &'static str, &'static [&'static str]) {
        match self {
            ContextKind::None => ("No context", "Only this document. Runs in the background.", &["Document"]),
            ContextKind::Cowork => ("Claude Cowork", "Your Cowork chat, its project and memory.", &["Document", "Your chat"]),
            ContextKind::Code => ("Claude Code", "Your Claude Code session or a project folder.", &["Document", "Project folder"]),
            ContextKind::Codex => ("Codex", "Codex answers through AItoCraft. New and open documents connect automatically.", &["Document", "Codex"]),
            ContextKind::Off => ("Agent off", "No agent: its tools refuse, nothing is sent.", &[]),
        }
    }
}

impl CodeVia {
    pub fn parse(s: &str) -> Option<CodeVia> {
        match s.trim().to_ascii_lowercase().as_str() {
            "session" | "my session" => Some(CodeVia::Session),
            "folder" | "project folder" => Some(CodeVia::Folder),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            CodeVia::Session => "session",
            CodeVia::Folder => "folder",
        }
    }
}

/// The runner's mode for a context.
pub fn mode_of(c: &AgentContext) -> Mode {
    match (c.kind, c.via) {
        (ContextKind::None, _) | (ContextKind::Code, CodeVia::Folder) => Mode::Background,
        (ContextKind::Cowork | ContextKind::Codex, _) => Mode::Manual,
        (ContextKind::Code, CodeVia::Session) => Mode::Channel,
        (ContextKind::Off, _) => Mode::Off,
    }
}

/// The context a runner mode stands for (an app started with `--agent-mode`, or before Context).
pub fn from_mode(m: Mode) -> AgentContext {
    let kind = match m {
        _ if codex_enabled() && m != Mode::Off => ContextKind::Codex,
        Mode::Manual => ContextKind::Cowork,
        Mode::Channel => ContextKind::Code,
        Mode::Background => ContextKind::None,
        Mode::Off => ContextKind::Off,
    };
    AgentContext { kind, ..AgentContext::default() }
}

// ---------------------------------------------------------------- the app's last choice

static LAST: Mutex<Option<AgentContext>> = Mutex::new(None);
/// Whether the last choice is written to the app's settings (off for offscreen renders).
static PERSIST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Keeps choices in memory only: a snapshot or a script never changes the person's settings.
pub fn persist(on: bool) {
    PERSIST.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Where the last choice is kept between runs (`<settings>/agent/context.json`; none in tests).
fn last_file() -> Option<PathBuf> {
    if cfg!(test) || !PERSIST.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    runner::default_data_dir().map(|d| d.join("context.json"))
}

/// The last context chosen in the app: what a document without its own starts with.
pub fn last() -> AgentContext {
    LAST.lock().ok().and_then(|g| g.clone()).unwrap_or_else(|| from_mode(runner::mode()))
}

fn remember(c: &AgentContext) {
    // The mode carries over to new documents; a project folder is the document's own.
    let keep = AgentContext { folder: String::new(), codex_thread_id: String::new(), codex_thread_title: String::new(), ..c.clone() };
    if let Some(f) = last_file() {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, json!(keep).to_string());
    }
    if let Ok(mut g) = LAST.lock() {
        *g = Some(keep);
    }
}

/// The last run's choice (at start, unless `--agent-mode` said otherwise).
pub fn load_saved() {
    let c = last_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| serde_json::from_str::<AgentContext>(&t).ok());
    if let Some(c) = c
        && let Ok(mut g) = LAST.lock()
    {
        *g = Some(c);
    }
}

/// Forgets the app's last choice (an `--agent-mode` given on the command line rules).
pub fn forget_last() {
    if let Ok(mut g) = LAST.lock() {
        *g = None;
    }
}

/// This app instance is served by Codex, without consulting another client's settings.
pub fn codex_enabled() -> bool {
    std::env::var("GRIDCRAFT_AGENT_PROVIDER").is_ok_and(|v| v == "codex")
}

/// The document's context: its own, else the app's last.
pub fn effective(l: &Layer) -> AgentContext {
    let mut c = l.context.clone().unwrap_or_else(last);
    // The Codex launcher owns the runtime. Legacy saved contexts cannot start another client.
    if codex_enabled() && c.kind != ContextKind::Off {
        c.kind = ContextKind::Codex;
        c.folder.clear();
        c.web = false;
    }
    c
}

/// Brings the runner in line with the active document's context: its mode, the folder the
/// background client runs in and whether it may use the web. A changed folder or web permission
/// restarts that document's client with them.
pub fn sync(s: &Session) {
    let Some(d) = s.active() else { return };
    let c = effective(&layer::get(d));
    let m = mode_of(&c);
    let folder = (c.kind == ContextKind::Code && c.via == CodeVia::Folder && !c.folder.is_empty()).then(|| PathBuf::from(&c.folder));
    let web = c.kind == ContextKind::None && c.web;
    let cfg = runner::config();
    if cfg.folder != folder || cfg.web != web {
        runner::set_config(runner::Config { folder, web, ..cfg });
        runner::stop(d.uid);
    }
    if runner::mode() != m {
        runner::set_mode(m);
    }
}

/// Sets the active document's context (a person's choice): undoable, logged, remembered as the
/// app's last. `params` as `agent.agentContext`.
pub fn set(s: &mut Session, p: &Json) -> gridcraft_engine::Result<AgentContext> {
    const CMD: &str = "agent.agentContext";
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(CMD, "only a person decides what the agent knows"));
    }
    let d = s.doc()?;
    let uid = d.uid;
    let before = layer::get(d).context.clone();
    let mut c = before.clone().unwrap_or_else(last);
    if let Some(k) = str_p(p, "kind") {
        c.kind = ContextKind::parse(k).ok_or_else(|| bad(CMD, "`kind` is none, cowork, code, codex or off"))?;
    }
    if codex_enabled() && !matches!(c.kind, ContextKind::Codex | ContextKind::Off) {
        return Err(bad(CMD, "this app instance uses Codex; choose codex or off"));
    }
    if let Some(v) = str_p(p, "via") {
        c.via = CodeVia::parse(v).ok_or_else(|| bad(CMD, "`via` is session or folder"))?;
    }
    if let Some(w) = p.get("web").and_then(Json::as_bool) {
        c.web = w;
    }
    if let Some(f) = str_p(p, "folder") {
        let f = f.trim();
        if !f.is_empty() && !std::path::Path::new(f).is_dir() {
            return Err(bad(CMD, format!("no folder `{f}`")));
        }
        c.folder = f.chars().take(4_096).collect();
    }
    if before.as_ref() != Some(&c) {
        let what = describe(&c);
        crate::topics::record(
            s,
            &format!("Context: {}", c.kind.label().0),
            vec![crate::topics::Part::Context(before)],
            vec![crate::topics::Part::Context(Some(c.clone()))],
        );
        let d = s.doc()?;
        layer::update(d, |l| {
            l.context = Some(c.clone());
            l.log(layer::Actor::Human, "change", format!("Context: {what}"), CMD, None, String::new());
        });
    }
    remember(&c);
    runner::arm(uid);
    sync(s);
    Ok(c)
}

/// "Claude Code · project folder ~/deals/acme", for the history.
pub fn describe(c: &AgentContext) -> String {
    let name = c.kind.label().0;
    match (c.kind, c.via) {
        (ContextKind::None, _) if c.web => format!("{name} · web"),
        (ContextKind::Code, CodeVia::Session) => format!("{name} · my session"),
        (ContextKind::Code, CodeVia::Folder) if !c.folder.is_empty() => format!("{name} · project folder {}", c.folder),
        (ContextKind::Code, CodeVia::Folder) => format!("{name} · project folder"),
        _ => name.to_string(),
    }
}

pub fn context_json(c: &AgentContext) -> Json {
    json!({
        "kind": c.kind.id(),
        "via": c.via.id(),
        "web": c.web,
        "folder": c.folder,
        "codexThreadId": c.codex_thread_id,
        "codexThreadTitle": c.codex_thread_title,
        "mode": mode_of(c).id(),
    })
}

fn context_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    let changes = ["kind", "via", "web", "folder"].iter().any(|k| p.get(*k).is_some());
    let c = if changes { set(s, p)? } else { effective(&layer::get(s.doc()?)) };
    Ok(context_json(&c))
}

/// Restrict deep links to a technical local chat UUID, including UUID v7.
pub fn valid_codex_thread(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() })
}

fn codex_origin(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    const CMD: &str = "agent.codexOrigin";
    let d = s.doc()?;
    let before = layer::get(d).context.clone();
    let mut c = before.clone().unwrap_or_else(|| from_mode(runner::mode()));
    if let Some(id) = str_p(p, "threadId") {
        if crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 || str_p(p, "actor") == Some("agent") {
            return Err(bad(CMD, "origin is recorded by the workbook creator, not an answering agent"));
        }
        if !id.is_empty() && !valid_codex_thread(id) {
            return Err(bad(CMD, "threadId must be a local Codex chat UUID"));
        }
        c.codex_thread_id = id.to_ascii_lowercase();
        c.codex_thread_title = str_p(p, "title").unwrap_or("Codex Desktop").chars().take(160).collect();
        if id.is_empty() {
            c.codex_thread_title.clear();
        }
        if before.as_ref() != Some(&c) {
            crate::topics::record(
                s,
                "Codex origin chat",
                vec![crate::topics::Part::Context(before)],
                vec![crate::topics::Part::Context(Some(c.clone()))],
            );
            layer::update(s.doc()?, |l| l.context = Some(c.clone()));
        }
    }
    Ok(json!({"threadId": c.codex_thread_id, "title": c.codex_thread_title}))
}

#[derive(Clone, Debug, Default)]
pub struct CodexRoute {
    pub status: String,
    pub thread_id: String,
    pub title: String,
    pub detail: String,
    pub updated_at: u64,
}

static CODEX_ROUTES: Mutex<Option<HashMap<u64, CodexRoute>>> = Mutex::new(None);

pub fn codex_route_for(uid: u64) -> Option<CodexRoute> {
    CODEX_ROUTES.lock().ok()?.as_ref()?.get(&uid).filter(|r| crate::now_ms().saturating_sub(r.updated_at) < 15_000).cloned()
}

fn codex_route(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    const CMD: &str = "agent.codexRoute";
    let uid = s.doc()?.uid;
    if let Some(status) = str_p(p, "status") {
        if crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 || str_p(p, "actor") == Some("agent") {
            return Err(bad(CMD, "delivery status is maintained by the Desktop bridge"));
        }
        if !matches!(
            status,
            "matched" | "fresh" | "missing" | "unbound" | "disconnected" | "queued" | "attention" | "linked" | "linkMissing" | "fallback"
        ) {
            return Err(bad(CMD, "invalid delivery status"));
        }
        let id = str_p(p, "threadId").unwrap_or("");
        if !id.is_empty() && !valid_codex_thread(id) {
            return Err(bad(CMD, "invalid chat UUID"));
        }
        let r = CodexRoute {
            status: status.into(),
            thread_id: id.into(),
            title: str_p(p, "title").unwrap_or("").chars().take(160).collect(),
            detail: str_p(p, "detail").unwrap_or("").chars().take(1000).collect(),
            updated_at: crate::now_ms(),
        };
        if let Ok(mut g) = CODEX_ROUTES.lock() {
            let map = g.get_or_insert_with(HashMap::new);
            map.retain(|_, old| r.updated_at.saturating_sub(old.updated_at) < 120_000);
            map.insert(uid, r);
        }
    }
    let r = codex_route_for(uid).unwrap_or_default();
    Ok(json!({"status": r.status, "threadId": r.thread_id, "title": r.title, "detail": r.detail, "updatedAt": r.updated_at}))
}

fn codex_open_chat(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    const CMD: &str = "agent.codexOpenChat";
    if crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 || str_p(p, "actor") == Some("agent") {
        return Err(bad(CMD, "only a person opens the Desktop chat"));
    }
    let d = s.doc()?;
    let c = effective(&layer::get(d));
    let id = codex_route_for(d.uid).map(|r| r.thread_id).filter(|id| !id.is_empty()).unwrap_or(c.codex_thread_id);
    if !valid_codex_thread(&id) {
        return Err(bad(CMD, "no Desktop chat is attached yet"));
    }
    let url = format!("codex://threads/{id}");
    if !cfg!(test) {
        #[cfg(target_os = "macos")]
        let result = std::process::Command::new("/usr/bin/open").arg(&url).spawn();
        #[cfg(target_os = "windows")]
        let result = std::process::Command::new("explorer.exe").arg(&url).spawn();
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result = std::process::Command::new("xdg-open").arg(&url).spawn();
        let mut child = result.map_err(|e| bad(CMD, e.to_string()))?;
        std::thread::Builder::new()
            .name("codex-open-chat".into())
            .spawn(move || {
                let _ = child.wait();
            })
            .map_err(|e| bad(CMD, e.to_string()))?;
    }
    Ok(json!({"url": url}))
}

// ---------------------------------------------------------------- is it ready?

/// What the app knows about the person's Claude setup, checked in the background.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setup {
    /// The `claude` CLI runs.
    pub cli: bool,
    /// It is logged in (`claude auth status`).
    pub logged_in: bool,
    /// AItoCraft is installed in Claude Code (`claude mcp get aitocraft`).
    pub code_hub: bool,
    /// AItoCraft is in Claude Desktop's configuration (Cowork).
    pub desktop_hub: bool,
    /// When this was checked (ms since the epoch); 0: never.
    pub checked_at: u64,
}

static SETUP: Mutex<Option<Setup>> = Mutex::new(None);
static CHECKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// How long a check counts before it is made again.
const SETUP_FRESH_MS: u64 = 60_000;

/// The last check of the person's setup; starts a new one when it is old (never in tests).
pub fn setup() -> Setup {
    if codex_enabled() {
        return Setup { checked_at: crate::now_ms(), ..Setup::default() };
    }
    let s = SETUP.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    if crate::now_ms().saturating_sub(s.checked_at) > SETUP_FRESH_MS {
        recheck();
    }
    s
}

/// Checks the setup again now, in the background.
pub fn recheck() {
    if codex_enabled() || cfg!(test) || CHECKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let spawned = std::thread::Builder::new().name("agent-setup-check".into()).spawn(|| {
        let s = check();
        if let Ok(mut g) = SETUP.lock() {
            *g = Some(s);
        }
        CHECKING.store(false, std::sync::atomic::Ordering::SeqCst);
    });
    if spawned.is_err() {
        CHECKING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The Claude CLI the checks, the login and the install use (the same the background client runs
/// by default).
fn claude() -> String {
    std::env::var("GRIDCRAFT_AGENT_COMMAND").unwrap_or_else(|_| "claude".into())
}

/// Runs the CLI with a time limit; its output when it ran.
fn run(args: &[&str]) -> Option<std::process::Output> {
    let mut child = std::process::Command::new(claude())
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    for _ in 0..150 {
        if let Ok(Some(_)) = child.try_wait() {
            return child.wait_with_output().ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn check() -> Setup {
    let status = run(&["auth", "status"]);
    let cli = status.is_some();
    let logged_in = status
        .as_ref()
        .and_then(|o| serde_json::from_slice::<Json>(&o.stdout).ok())
        .and_then(|v| v.get("loggedIn").and_then(Json::as_bool))
        .unwrap_or(false);
    let code_hub = cli && run(&["mcp", "get", "aitocraft"]).is_some_and(|o| o.status.success());
    Setup { cli, logged_in, code_hub, desktop_hub: desktop_hub(), checked_at: crate::now_ms() }
}

/// AItoCraft is one of Claude Desktop's MCP servers.
pub fn desktop_hub() -> bool {
    crate::cowork::config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Json>(&t).ok())
        .is_some_and(|v| v.pointer("/mcpServers/aitocraft").is_some())
}

/// What installing in Claude Code said (set when it finishes).
static RESULT: Mutex<Option<String>> = Mutex::new(None);

/// The result of the last [`install_code`] or [`log_in`], once.
pub fn take_result() -> Option<String> {
    RESULT.lock().ok().and_then(|mut g| g.take())
}

fn say(msg: String) {
    if let Ok(mut g) = RESULT.lock() {
        *g = Some(msg);
    }
}

/// `claude mcp add --scope user aitocraft -- <hub>`, in the background.
pub fn install_code() -> Result<(), String> {
    let Some(hub) = crate::hub::bin() else {
        return Err("AItoCraft isn't on this computer. Put the `aitocraft` program on your PATH (or set AITOCRAFT_BIN to it), then try again. It is the one connection for all Craft apps: Claude Desktop, Cowork and Claude Code.".into());
    };
    if cfg!(test) {
        return Ok(());
    }
    std::thread::Builder::new()
        .name("aitocraft-code-install".into())
        .spawn(move || {
            let hub = hub.display().to_string();
            let msg = match run(&["mcp", "add", "--scope", "user", "aitocraft", "--", &hub]) {
                Some(o) if o.status.success() => None,
                Some(o) => Some(format!(
                    "claude mcp add failed: {}",
                    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim()
                )),
                None => Some(format!("Couldn't run `{}`: is Claude Code installed?", claude())),
            };
            if let Some(m) = msg {
                say(m);
            }
            recheck();
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Opens a terminal running `claude auth login`.
pub fn log_in() -> Result<(), String> {
    if cfg!(test) {
        return Ok(());
    }
    let cmd = format!("{} auth login", claude());
    let r = if cfg!(target_os = "macos") {
        let script = format!("tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell", cmd.replace('\\', "\\\\").replace('"', "\\\""));
        std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).spawn()
    } else if cfg!(windows) {
        std::process::Command::new("cmd").args(["/c", "start", "cmd", "/k", &cmd]).spawn()
    } else {
        std::process::Command::new("x-terminal-emulator").args(["-e", &cmd]).spawn()
    };
    r.map(|_| ()).map_err(|e| format!("Couldn't open a terminal: {e}. Run `{cmd}` in one."))
}

/// Start the app-scoped worker. Its singleton check prevents duplicate model consumers.
fn codex_connect(_s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    const CMD: &str = "agent.codexConnect";
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(CMD, "only a person connects Codex"));
    }
    if !codex_enabled() {
        return Err(bad(CMD, "start the Codex edition of GridCraft"));
    }
    let script = std::env::var("GRIDCRAFT_CODEX_BRIDGE").map_err(|_| bad(CMD, "restart the Codex GridCraft launcher to reconnect Codex"))?;
    if !std::path::Path::new(&script).is_file() {
        return Err(bad(CMD, "the Codex worker script is missing"));
    }
    let mut cmd = std::process::Command::new("/usr/bin/python3");
    cmd.args([script, "--instance".into(), std::process::id().to_string()]);
    if let Some(registry) = std::env::var_os("AITOCRAFT_REGISTRY_DIR") {
        cmd.arg("--registry").arg(registry);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| bad(CMD, e.to_string()))?;
    let pid = child.id();
    std::thread::Builder::new()
        .name("codex-worker".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|e| bad(CMD, e.to_string()))?;
    Ok(json!({"pid": pid, "connecting": true}))
}
