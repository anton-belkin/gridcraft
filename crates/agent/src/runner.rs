//! The app starts the agent itself, in one of two modes (desktop only):
//!
//! - **My session** ([`Mode::Channel`]): new questions go to the user's own interactive Claude Code
//!   session as channel events (Claude Code Channels, research preview). The in-app server pushes
//!   them on `GET /mcp`, and `gridcraft --agent-stdio` ([`crate::channel`]) bridges them to the
//!   stdio transport Claude Code spawns channel servers with.
//! - **Background agent** ([`Mode::Background`]): one `claude -p` process per document, fed the
//!   questions as stream-json on its stdin. It runs in the workbook's folder (so its session sits
//!   with the deal's other files in Claude Desktop), sees only the GridCraft MCP server
//!   (`--strict-mcp-config`), gets the agent skill as its system prompt, and is bounded by
//!   `--max-turns` and `--max-budget-usd`. Its session id lives in the layer, so reopening the
//!   workbook resumes the same conversation. Never `--bare` (it needs an API key; a subscription
//!   login works without one).
//!
//! The mode is the app's, never the file's: opening a workbook someone sent must not start an
//! agent that spends the reader's money.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use gridcraft_engine::Session;
use serde_json::{Value as Json, json};

use crate::layer::{self, QuestionStatus};

/// How the app brings the agent in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// The user connects an agent themselves (MCP: Claude Code, Cowork, any client).
    #[default]
    Manual,
    /// Questions go to the user's interactive Claude Code session as channel events.
    Channel,
    /// The app runs a background agent per document.
    Background,
    /// No agent: tools refuse, nothing is sent out.
    Off,
}

static MODE: AtomicU8 = AtomicU8::new(0);

pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        1 => Mode::Channel,
        2 => Mode::Background,
        3 => Mode::Off,
        _ => Mode::Manual,
    }
}

/// Where the chosen mode is kept between runs (`<settings>/agent/mode`; none in tests).
fn mode_file() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    default_data_dir().map(|d| d.join("mode"))
}

/// Remembers the mode for the next run.
pub fn remember_mode(m: Mode) {
    if let Some(f) = mode_file() {
        if let Some(dir) = f.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(f, m.id());
    }
}

/// The mode of the last run (when `--agent-mode` didn't say).
pub fn load_saved_mode() {
    if let Some(m) = mode_file().and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| Mode::parse(&t)) {
        set_mode(m);
    }
}

/// Switches the mode; leaving the background mode stops its agents.
pub fn set_mode(m: Mode) {
    MODE.store(
        match m {
            Mode::Manual => 0,
            Mode::Channel => 1,
            Mode::Background => 2,
            Mode::Off => 3,
        },
        Ordering::Relaxed,
    );
    if m != Mode::Background {
        stop_all();
    }
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Manual, Mode::Channel, Mode::Background, Mode::Off];

    pub fn parse(s: &str) -> Option<Mode> {
        match s.trim().to_ascii_lowercase().as_str() {
            "connect" | "manual" | "mcp" => Some(Mode::Manual),
            "channel" | "session" | "my-session" => Some(Mode::Channel),
            "background" | "bg" => Some(Mode::Background),
            "off" | "none" => Some(Mode::Off),
            _ => None,
        }
    }
    /// The mode's name in the list, and one line of what it means.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Mode::Manual => ("Claude Desktop / Cowork (AItoCraft)", "Start the chat where the work belongs and say which document"),
            Mode::Channel => ("My Claude Code session (AItoCraft channel)", "Questions arrive in your open Claude Code session"),
            Mode::Background => ("Background agent", "The app runs Claude on your subscription. It sees only this document. Uses your limits."),
            Mode::Off => ("Off", "No agent: tools refuse, nothing is sent"),
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Mode::Manual => "connect",
            Mode::Channel => "channel",
            Mode::Background => "background",
            Mode::Off => "off",
        }
    }
}

/// How the background agent is started.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The agent CLI (`claude` by default; another CLI that speaks the same stream-json fits).
    pub command: String,
    /// The in-app MCP server.
    pub mcp_url: String,
    pub max_turns: u32,
    pub max_budget_usd: f64,
    /// Where the MCP config and the skill are written.
    pub data_dir: Option<PathBuf>,
    /// Run in the document's folder (its CLAUDE.md, memory and settings join the context) instead
    /// of the neutral `ArtCraft/agent-sessions/`.
    pub project_folder: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            // Tests never start a real agent.
            command: if cfg!(test) {
                "gridcraft-test-no-agent".into()
            } else {
                std::env::var("GRIDCRAFT_AGENT_COMMAND").unwrap_or_else(|_| "claude".into())
            },
            mcp_url: format!("http://127.0.0.1:{}/mcp", crate::server::DEFAULT_PORT),
            max_turns: 40,
            max_budget_usd: 3.0,
            data_dir: default_data_dir(),
            project_folder: false,
        }
    }
}

/// The neutral folder background sessions run in (and are grouped under in Claude Desktop).
pub fn sessions_dir() -> Option<PathBuf> {
    crate::registry::folder().and_then(|f| f.parent().map(|p| p.join("agent-sessions")))
}

fn default_data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/GridCraft/agent"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("GridCraft").join("agent"))
    } else {
        std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| home.map(|h| h.join(".local/share"))).map(|d| d.join("gridcraft/agent"))
    }
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

pub fn config() -> Config {
    CONFIG.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

pub fn set_config(c: Config) {
    if let Ok(mut g) = CONFIG.lock() {
        *g = Some(c);
    }
}

/// The agent skill, given to the background agent as its system prompt.
pub const SKILL: &str = include_str!("../../../agent-layer/skill/gridcraft-agent/SKILL.md");

// ---------------------------------------------------------------- one agent per document

/// What a document's background agent is doing (for the Connect tab).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub running: bool,
    pub session_id: String,
    /// The folder the agent runs in (its project in Claude Desktop).
    pub cwd: String,
    /// Questions sent and not yet finished.
    pub in_flight: usize,
    pub turns: usize,
    pub cost_usd: f64,
    pub last_error: String,
    /// The last few things it said or did, newest last.
    pub recent: Vec<String>,
}

struct Agent {
    child: Child,
    stdin: ChildStdin,
    sent: HashSet<String>,
    shared: Arc<Mutex<Status>>,
    /// Started with `--resume`: a quick failure means the session isn't on this machine.
    resumed: bool,
}

#[derive(Default)]
struct Agents {
    by_doc: HashMap<u64, Agent>,
    /// Statuses of agents that stopped, so their last error stays visible.
    ended: HashMap<u64, Status>,
    /// Documents whose resume failed: start a fresh session next time.
    fresh: HashSet<u64>,
    queue: HashMap<u64, VecDeque<String>>,
}

static AGENTS: Mutex<Option<Agents>> = Mutex::new(None);

fn agents() -> MutexGuard<'static, Option<Agents>> {
    match AGENTS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

fn lock(s: &Mutex<Status>) -> MutexGuard<'_, Status> {
    match s.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    }
}

/// The background agent's status for a document.
pub fn status(uid: u64) -> Option<Status> {
    let g = agents();
    let a = g.as_ref()?;
    a.by_doc.get(&uid).map(|x| lock(&x.shared).clone()).or_else(|| a.ended.get(&uid).cloned())
}

/// The arguments the agent CLI is started with.
pub fn args(c: &Config, session: &str, resume: bool, mcp_config: &str, skill: Option<&str>, name: &str) -> Vec<String> {
    let mut a: Vec<String> =
        ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"].iter().map(|s| s.to_string()).collect();
    a.extend([if resume { "--resume" } else { "--session-id" }.to_string(), session.to_string()]);
    // Named for the session lists ("GridCraft · budget.xlsx").
    a.extend(["--name".to_string(), name.to_string()]);
    a.extend(["--mcp-config".to_string(), mcp_config.to_string(), "--strict-mcp-config".to_string()]);
    a.extend(["--allowedTools".to_string(), "mcp__gridcraft__*".to_string()]);
    if let Some(s) = skill {
        a.extend(["--append-system-prompt-file".to_string(), s.to_string()]);
    }
    a.extend(["--max-turns".to_string(), c.max_turns.to_string(), "--max-budget-usd".to_string(), format!("{:.2}", c.max_budget_usd)]);
    a
}

fn user_message(text: &str) -> String {
    json!({"type": "user", "message": {"role": "user", "content": text}, "parent_tool_use_id": null}).to_string()
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// Reads the agent's stream-json: turns finished, cost, errors, what it said.
fn read_stdout(out: std::process::ChildStdout, shared: Arc<Mutex<Status>>) {
    for line in BufReader::new(out).lines().map_while(std::result::Result::ok) {
        let Ok(v) = serde_json::from_str::<Json>(&line) else { continue };
        let mut st = lock(&shared);
        match v.get("type").and_then(Json::as_str) {
            Some("system") if v.get("subtype").and_then(Json::as_str) == Some("init") => {
                if let Some(id) = v.get("session_id").and_then(Json::as_str) {
                    st.session_id = id.to_string();
                }
            }
            Some("assistant") => {
                let parts = v.pointer("/message/content").and_then(Json::as_array).cloned().unwrap_or_default();
                for p in parts {
                    let line = match p.get("type").and_then(Json::as_str) {
                        Some("text") => p.get("text").and_then(Json::as_str).map(|t| short(t.trim(), 160)),
                        Some("tool_use") => p.get("name").and_then(Json::as_str).map(|n| format!("→ {}", n.trim_start_matches("mcp__gridcraft__"))),
                        _ => None,
                    };
                    if let Some(l) = line.filter(|l| !l.is_empty()) {
                        st.recent.push(l);
                    }
                }
            }
            Some("result") => {
                st.in_flight = st.in_flight.saturating_sub(1);
                st.turns += 1;
                if let Some(c) = v.get("total_cost_usd").and_then(Json::as_f64) {
                    st.cost_usd = c;
                }
                if v.get("is_error").and_then(Json::as_bool) == Some(true) {
                    let sub = v.get("subtype").and_then(Json::as_str).unwrap_or("error");
                    let text = v.get("result").and_then(Json::as_str).unwrap_or("");
                    st.last_error = short(&format!("{sub}: {text}"), 300);
                }
            }
            _ => {}
        }
        let n = st.recent.len();
        if n > 12 {
            st.recent.drain(..n - 12);
        }
    }
    lock(&shared).running = false;
}

fn read_stderr(err: std::process::ChildStderr, shared: Arc<Mutex<Status>>) {
    for line in BufReader::new(err).lines().map_while(std::result::Result::ok) {
        let l = line.trim();
        if !l.is_empty() {
            lock(&shared).last_error = short(l, 300);
        }
    }
}

/// A summary of the document's history, to start a fresh session from.
fn seed(l: &layer::Layer) -> String {
    let mut s = String::from("You are continuing work on this workbook in a fresh session. What happened so far, from the document's history:\n");
    for e in l.log.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev() {
        let who = if e.actor == layer::Actor::Agent { "agent" } else { "reader" };
        s.push_str(&format!("- {who} {}: {}\n", e.kind, short(&e.text, 200)));
    }
    let open = l.questions.iter().filter(|q| q.status != QuestionStatus::Answered && !q.is_ping()).count();
    s.push_str(&format!("{open} question(s) are open. Call get_context before anything else.\n"));
    s
}

fn write_support_files(c: &Config) -> Result<(String, Option<String>), String> {
    let dir = c.data_dir.clone().ok_or("no folder for the agent's files")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mcp = dir.join("mcp.json");
    let conf = json!({"mcpServers": {"gridcraft": {"type": "http", "url": c.mcp_url}}});
    std::fs::write(&mcp, conf.to_string()).map_err(|e| format!("{}: {e}", mcp.display()))?;
    let skill = dir.join("gridcraft-agent-skill.md");
    let skill = std::fs::write(&skill, SKILL).ok().map(|()| skill.display().to_string());
    Ok((mcp.display().to_string(), skill))
}

/// Starts the background agent of the active document (resuming its session when it has one).
pub fn start(s: &Session) -> Result<Status, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    let uid = d.uid;
    if agents().as_ref().is_some_and(|a| a.by_doc.contains_key(&uid)) {
        return status(uid).ok_or_else(|| "starting".into());
    }
    let c = config();
    let (mcp, skill) = write_support_files(&c)?;
    let l = layer::get(d);
    let fresh = agents().as_ref().is_some_and(|a| a.fresh.contains(&uid));
    let (session, resume) = if l.agent_session.is_empty() || fresh { (crate::links::new_uuid(uid), false) } else { (l.agent_session.clone(), true) };
    // A neutral, empty folder by default, so no project's CLAUDE.md, memory or settings become its
    // context (AItoCraft §4); the document's folder only when asked.
    let doc_folder = d.path.as_deref().and_then(|p| std::path::absolute(p).ok()).and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let cwd = match (c.project_folder, doc_folder) {
        (true, Some(f)) => f,
        _ => {
            let n = sessions_dir().unwrap_or_else(std::env::temp_dir);
            let _ = std::fs::create_dir_all(&n);
            n
        }
    };
    let mut child = Command::new(&c.command)
        .args(args(&c, &session, resume, &mcp, skill.as_deref(), &format!("GridCraft · {}", d.display_title())))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't start `{}`: {e}", c.command))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("the agent's pipes didn't open".into());
    };
    let shared = Arc::new(Mutex::new(Status { running: true, session_id: session.clone(), cwd: cwd.display().to_string(), ..Default::default() }));
    {
        let sh = shared.clone();
        let _ = std::thread::Builder::new().name("agent-stdout".into()).spawn(move || read_stdout(stdout, sh));
        let sh = shared.clone();
        let _ = std::thread::Builder::new().name("agent-stderr".into()).spawn(move || read_stderr(stderr, sh));
    }
    let mut agent = Agent { child, stdin, sent: HashSet::new(), shared, resumed: resume };
    // A new session gets the document's history, when there is one.
    let mut first = vec![];
    if !resume && !l.log.is_empty() {
        first.push(seed(&l));
    }
    for m in first {
        if writeln!(agent.stdin, "{}", user_message(&m)).and_then(|()| agent.stdin.flush()).is_ok() {
            lock(&agent.shared).in_flight += 1;
        }
    }
    let st = lock(&agent.shared).clone();
    {
        let mut g = agents();
        let a = g.get_or_insert_with(Agents::default);
        a.fresh.remove(&uid);
        a.ended.remove(&uid);
        a.by_doc.insert(uid, agent);
    }
    if l.agent_session != session {
        layer::update(d, |l| l.agent_session = session.clone());
    }
    Ok(st)
}

/// Stops a document's agent: end of input, then SIGINT, then (after a grace period) kill.
pub fn stop(uid: u64) {
    let agent = agents().as_mut().and_then(|a| a.by_doc.remove(&uid));
    let Some(mut agent) = agent else { return };
    drop(agent.stdin);
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-INT", &agent.child.id().to_string()]).status();
    }
    let mut done = false;
    for _ in 0..30 {
        if matches!(agent.child.try_wait(), Ok(Some(_))) {
            done = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if !done {
        let _ = agent.child.kill();
        let _ = agent.child.wait();
    }
    let mut st = lock(&agent.shared).clone();
    st.running = false;
    st.in_flight = 0;
    if let Some(a) = agents().as_mut() {
        a.ended.insert(uid, st);
    }
}

pub fn stop_all() {
    let uids: Vec<u64> = agents().as_ref().map(|a| a.by_doc.keys().copied().collect()).unwrap_or_default();
    for u in uids {
        stop(u);
    }
}

/// A fresh session for the active document: the old one stays in the agent's history; the new
/// one starts from a summary of the document's log.
pub fn reset(s: &Session) -> Result<Status, String> {
    let d = s.doc().map_err(|e| e.to_string())?;
    stop(d.uid);
    agents().get_or_insert_with(Agents::default).fresh.insert(d.uid);
    start(s)
}

/// The text a question is sent as.
pub fn question_text(s: &Session, q: &Json) -> String {
    if q.get("kind").and_then(Json::as_str) == Some(crate::layer::PING) {
        let id = q.get("id").and_then(Json::as_str).unwrap_or("");
        return format!(
            "Connection check from GridCraft (question {id}, kind ping): answer it at once with answer_question, text \"here\". Don't investigate."
        );
    }
    let title = s.doc().map(|d| d.display_title()).unwrap_or_default();
    let get = |k: &str| q.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let mut t = format!("A question was asked in the workbook “{title}”, on {}", get("range"));
    let label = get("label");
    if !label.is_empty() {
        t.push_str(&format!(" ({label})"));
    }
    t.push_str(&format!(":\n\n“{}”\n\nQuestion id: {}. Document: {}.", get("text"), get("id"), get("document")));
    t.push_str(" Claim it (claim_question), investigate with the gridcraft tools, and answer with answer_question, linking the cells you used.");
    t
}

/// Called every frame (and by tests): in the background mode, starts the active document's agent
/// when it has open questions and sends each new question once, in order.
pub fn tick(s: &mut Session) {
    if mode() != Mode::Background {
        return;
    }
    let Some(d) = s.active() else { return };
    let uid = d.uid;
    // A finished process: keep its status; a resume that failed starts fresh next time.
    let exited = {
        let mut g = agents();
        let a = g.get_or_insert_with(Agents::default);
        let gone = a.by_doc.get_mut(&uid).is_some_and(|x| !matches!(x.child.try_wait(), Ok(None)));
        if gone && let Some(mut x) = a.by_doc.remove(&uid) {
            let mut st = lock(&x.shared).clone();
            st.running = false;
            st.in_flight = 0;
            if x.resumed && st.turns <= 1 {
                a.fresh.insert(uid);
                if st.last_error.is_empty() {
                    st.last_error = "the saved session couldn't be resumed here; the next question starts a fresh one".into();
                }
            }
            let _ = x.child.wait();
            a.ended.insert(uid, st);
        }
        gone
    };
    let l = layer::get(d);
    let pending: Vec<String> = l.questions.iter().filter(|q| q.status == QuestionStatus::Pending).map(|q| q.id.clone()).collect();
    if pending.is_empty() {
        return;
    }
    let running = agents().as_ref().is_some_and(|a| a.by_doc.contains_key(&uid));
    if !running {
        // Don't restart in a loop on the frame after a crash: once per new question.
        let unseen = {
            let g = agents();
            let ended = g.as_ref().and_then(|a| a.queue.get(&uid)).cloned().unwrap_or_default();
            pending.iter().any(|p| !ended.contains(p))
        };
        if exited || !unseen {
            return;
        }
        if let Err(e) = start(s) {
            let mut g = agents();
            let a = g.get_or_insert_with(Agents::default);
            a.ended.insert(uid, Status { last_error: e, ..Default::default() });
            a.queue.entry(uid).or_default().extend(pending);
            return;
        }
    }
    let new: Vec<String> = {
        let g = agents();
        g.as_ref().and_then(|a| a.by_doc.get(&uid)).map(|x| pending.iter().filter(|p| !x.sent.contains(*p)).cloned().collect()).unwrap_or_default()
    };
    if new.is_empty() {
        return;
    }
    let list = match s.execute("agent.questions", json!({"status": "pending", "masked": true, "poll": true})) {
        Ok(Json::Array(v)) => v,
        _ => return,
    };
    let texts: Vec<(String, String)> = list
        .iter()
        .filter_map(|q| q.get("id").and_then(Json::as_str).filter(|id| new.iter().any(|n| n == id)).map(|id| (id.to_string(), question_text(s, q))))
        .collect();
    let mut g = agents();
    let Some(a) = g.as_mut() else { return };
    let Some(x) = a.by_doc.get_mut(&uid) else { return };
    for (id, text) in texts {
        if writeln!(x.stdin, "{}", user_message(&text)).and_then(|()| x.stdin.flush()).is_ok() {
            x.sent.insert(id.clone());
            let mut st = lock(&x.shared);
            st.in_flight += 1;
            st.recent.push(format!("← question {id}"));
        } else {
            lock(&x.shared).last_error = "the agent stopped reading its input".into();
        }
        a.queue.entry(uid).or_default().push_back(id);
    }
}

/// `claude --resume <id>` in the agent's folder: continue its conversation by hand (stop the
/// background agent first: one writer per session).
pub fn resume_command(st: &Status) -> String {
    let c = config();
    let cd = if st.cwd.is_empty() { String::new() } else { format!("cd '{}' && ", st.cwd.replace('\'', "'\\''")) };
    format!("{cd}{} --resume {}", c.command, st.session_id)
}

// ---------------------------------------------------------------- commands (the person's, not the agent's)

use gridcraft_engine::cmd::CommandSpec;

use crate::commands::{bad, spec, str_p};

pub static SPECS: &[CommandSpec] = &[
    spec!(query "agent.agentMode", "Agent Mode", "{mode?: connect|channel|background|off, projectFolder?: bool (the background agent runs in the document's folder)} → the mode (how the agent comes in: you connect it over MCP, into your own Claude Code session, as a background agent per document, or not at all)", mode_cmd),
    spec!(query "agent.agentStart", "Start Background Agent", "{} — starts (or resumes) the active document's background agent", start_cmd),
    spec!(query "agent.agentStop", "Stop Background Agent", "{}", stop_cmd),
    spec!(query "agent.agentReset", "New Agent Session", "{} — a fresh session for the active document, started from a summary of its history", reset_cmd),
    spec!(query "agent.agentStatus", "Background Agent Status", "{} → running, session id, folder, cost, last error, what it did lately", status_cmd),
];

/// The status as JSON (with the mode and the command to continue the session by hand).
pub fn status_json(uid: u64, session: &str) -> Json {
    let st = status(uid).unwrap_or_else(|| Status { session_id: session.to_string(), ..Default::default() });
    json!({
        "mode": mode().id(),
        "command": config().command,
        "projectFolder": config().project_folder,
        "running": st.running,
        "sessionId": st.session_id,
        "cwd": st.cwd,
        "inFlight": st.in_flight,
        "turns": st.turns,
        "costUsd": st.cost_usd,
        "lastError": st.last_error,
        "recent": st.recent,
        "resumeCommand": if st.session_id.is_empty() { Json::Null } else { json!(resume_command(&st)) },
    })
}

fn refuse_agent(p: &Json, cmd: &str) -> gridcraft_engine::Result<()> {
    if str_p(p, "actor") == Some("agent") || crate::commands::AGENT_DEPTH.with(std::cell::Cell::get) > 0 {
        return Err(bad(cmd, "only a person decides how the agent runs"));
    }
    Ok(())
}

fn here(s: &Session) -> gridcraft_engine::Result<(u64, String)> {
    let d = s.doc()?;
    Ok((d.uid, layer::get(d).agent_session.clone()))
}

fn mode_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    if let Some(on) = p.get("projectFolder").and_then(Json::as_bool) {
        refuse_agent(p, "agent.agentMode")?;
        set_config(Config { project_folder: on, ..config() });
    }
    if let Some(m) = str_p(p, "mode") {
        refuse_agent(p, "agent.agentMode")?;
        let m = Mode::parse(m).ok_or_else(|| bad("agent.agentMode", "`mode` is connect, channel, background or off"))?;
        set_mode(m);
        remember_mode(m);
    }
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn start_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    refuse_agent(p, "agent.agentStart")?;
    start(s).map_err(|e| bad("agent.agentStart", e))?;
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn stop_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    refuse_agent(p, "agent.agentStop")?;
    let (uid, session) = here(s)?;
    stop(uid);
    Ok(status_json(uid, &session))
}

fn reset_cmd(s: &mut Session, p: &Json) -> gridcraft_engine::Result<Json> {
    refuse_agent(p, "agent.agentReset")?;
    reset(s).map_err(|e| bad("agent.agentReset", e))?;
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

fn status_cmd(s: &mut Session, _p: &Json) -> gridcraft_engine::Result<Json> {
    let (uid, session) = here(s)?;
    Ok(status_json(uid, &session))
}

// ---------------------------------------------------------------- is anyone listening?

/// Signs of a listener, without asking.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Signals {
    /// `/events` streams open now.
    pub event_streams: usize,
    /// Channel streams of Claude Code sessions open now (counted in "my session" mode).
    pub channel_streams: usize,
    /// The last time an agent waited for questions (`wait_for_questions`), ms since the epoch.
    pub last_wait_ms: u64,
    /// The background agent's process is alive.
    pub background: bool,
    /// The last call of an agent tool, ms since the epoch.
    pub last_call_ms: u64,
    /// The document's attached listeners: (label, last seen), most recent first.
    pub listeners: Vec<(String, u64)>,
    /// `/events?document=` streams following this document.
    pub document_streams: usize,
}

/// What the Agent tab says about the connection.
#[derive(Clone, Debug, PartialEq)]
pub enum Presence {
    /// A check was answered (in `latency_ms`), `checked_ms` ago.
    Answering {
        checked_ms: u64,
        latency_ms: u64,
    },
    /// A check is out, `since_ms`.
    Checking {
        since_ms: u64,
    },
    /// A check went unanswered for a minute.
    NoAnswer {
        asked_ms: u64,
    },
    /// A conversation is attached to the document: its label, how long since it was seen, and
    /// how many others listen too.
    Listening {
        label: String,
        seen_ms: u64,
        more: usize,
    },
    /// No check, but something listens: what.
    Listener(Vec<&'static str>),
    Nobody,
}

/// A check unanswered for this long is "no answer".
pub const PING_TIMEOUT_MS: u64 = 60_000;
/// An answered check counts for this long.
const PING_FRESH_MS: u64 = 10 * 60_000;

/// What the signals and the last check say.
pub fn classify(ping: Option<layer::Ping>, s: &Signals, mode: Mode, now: u64) -> Presence {
    match ping {
        Some(layer::Ping::Waiting { asked_at }) if now.saturating_sub(asked_at) >= PING_TIMEOUT_MS => {
            return Presence::NoAnswer { asked_ms: now.saturating_sub(asked_at) };
        }
        Some(layer::Ping::Waiting { asked_at }) => return Presence::Checking { since_ms: now.saturating_sub(asked_at) },
        Some(layer::Ping::Answered { asked_at, answered_at }) if now.saturating_sub(answered_at) < PING_FRESH_MS => {
            return Presence::Answering { checked_ms: now.saturating_sub(answered_at), latency_ms: answered_at.saturating_sub(asked_at) };
        }
        _ => {}
    }
    if let Some((label, seen)) = s.listeners.first() {
        return Presence::Listening { label: label.clone(), seen_ms: now.saturating_sub(*seen), more: s.listeners.len() - 1 };
    }
    let mut what = vec![];
    if s.document_streams > 0 {
        what.push("document stream");
    }
    if s.event_streams > 0 {
        what.push("event stream");
    }
    if s.channel_streams > 0 && mode == Mode::Channel {
        what.push("Claude Code channel");
    }
    if s.last_wait_ms > 0 && now.saturating_sub(s.last_wait_ms) < 60_000 {
        what.push("waiting for questions");
    }
    if s.background {
        what.push("background agent");
    }
    if s.last_call_ms > 0 && now.saturating_sub(s.last_call_ms) < 120_000 {
        what.push("tool calls");
    }
    if what.is_empty() { Presence::Nobody } else { Presence::Listener(what) }
}

/// The signals for the document `uid` now.
pub fn signals(uid: u64) -> Signals {
    let (event_streams, channel_streams) = crate::server::streams();
    Signals {
        event_streams,
        channel_streams,
        last_wait_ms: crate::commands::last_wait_ms(),
        background: status(uid).is_some_and(|s| s.running),
        last_call_ms: crate::last_agent_call_ms(),
        listeners: crate::docs::listeners(uid).into_iter().map(|l| (l.label, l.last_seen)).collect(),
        document_streams: crate::server::document_streams(&crate::docs::doc_id(uid)),
    }
}

/// Where the connection of the active document stands.
pub fn presence(s: &Session) -> Presence {
    let Some(d) = s.active() else { return Presence::Nobody };
    classify(layer::get(d).ping(), &signals(d.uid), mode(), crate::now_ms())
}
