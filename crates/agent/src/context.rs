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

use std::path::PathBuf;
use std::sync::Mutex;

use gridcraft_engine::Session;
use gridcraft_engine::cmd::CommandSpec;
use serde_json::{Value as Json, json};

use crate::commands::{bad, spec, str_p};
use crate::layer::{self, AgentContext, CodeVia, ContextKind, Layer};
use crate::runner::{self, Mode};

pub static SPECS: &[CommandSpec] = &[spec!(
    change "agent.agentContext",
    "Agent Context",
    "{kind?: none|cowork|code, via?: session|folder, web?: bool, folder?: path} — what the agent knows about this document: only the document (a background client; `web` also allows web search), your Cowork chat, or your Claude Code session or a project folder. A person's choice, saved in the document → the context",
    context_cmd
)];

impl ContextKind {
    pub fn parse(s: &str) -> Option<ContextKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "no context" | "document" => Some(ContextKind::None),
            "cowork" | "claude cowork" => Some(ContextKind::Cowork),
            "code" | "claude code" => Some(ContextKind::Code),
            "off" => Some(ContextKind::Off),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            ContextKind::None => "none",
            ContextKind::Cowork => "cowork",
            ContextKind::Code => "code",
            ContextKind::Off => "off",
        }
    }
    /// The name on the tab, the line under it, and what the agent knows (the chips).
    pub fn label(self) -> (&'static str, &'static str, &'static [&'static str]) {
        match self {
            ContextKind::None => ("No context", "Only this document. Runs in the background.", &["Document"]),
            ContextKind::Cowork => ("Claude Cowork", "Your Cowork chat, its project and memory.", &["Document", "Your chat"]),
            ContextKind::Code => ("Claude Code", "Your Claude Code session or a project folder.", &["Document", "Project folder"]),
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
        (ContextKind::Cowork, _) => Mode::Manual,
        (ContextKind::Code, CodeVia::Session) => Mode::Channel,
        (ContextKind::Off, _) => Mode::Off,
    }
}

/// The context a runner mode stands for (an app started with `--agent-mode`, or before Context).
pub fn from_mode(m: Mode) -> AgentContext {
    let kind = match m {
        Mode::Manual => ContextKind::Cowork,
        Mode::Channel => ContextKind::Code,
        Mode::Background => ContextKind::None,
        Mode::Off => ContextKind::Off,
    };
    AgentContext { kind, ..AgentContext::default() }
}

// ---------------------------------------------------------------- the app's last choice

static LAST: Mutex<Option<AgentContext>> = Mutex::new(None);

/// Where the last choice is kept between runs (`<settings>/agent/context.json`; none in tests).
fn last_file() -> Option<PathBuf> {
    if cfg!(test) {
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
    let keep = AgentContext { folder: String::new(), ..c.clone() };
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

/// The document's context: its own, else the app's last.
pub fn effective(l: &Layer) -> AgentContext {
    l.context.clone().unwrap_or_else(last)
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
        c.kind = ContextKind::parse(k).ok_or_else(|| bad(CMD, "`kind` is none, cowork or code"))?;
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
        "mode": mode_of(c).id(),
    })
}

fn context_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    let changes = ["kind", "via", "web", "folder"].iter().any(|k| p.get(*k).is_some());
    let c = if changes { set(s, p)? } else { effective(&layer::get(s.doc()?)) };
    Ok(context_json(&c))
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
    let s = SETUP.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    if crate::now_ms().saturating_sub(s.checked_at) > SETUP_FRESH_MS {
        recheck();
    }
    s
}

/// Checks the setup again now, in the background.
pub fn recheck() {
    if cfg!(test) || CHECKING.swap(true, std::sync::atomic::Ordering::SeqCst) {
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

fn claude() -> String {
    runner::config().command
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
