//! The agent layer's MCP over stdio: `gridcraft --agent-stdio [--control HOST:PORT]`, for clients
//! that start local servers as processes (Claude Desktop and Cowork, `claude mcp add` without
//! `--transport http`, Claude Code channels). The same tools as the in-app HTTP server; the
//! workbook is reached through the app's control channel, connected on the first tool call, so the
//! client can start before GridCraft and keeps working across app restarts. Nothing but protocol
//! goes to stdout. Listening is `wait_for_questions` in a loop; in "my session" mode the questions
//! also arrive as `notifications/claude/channel` ([`crate::channel`]).

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use gridcraft_mcp::{Backend, Remote};
use serde_json::{Value, json};

/// The client this stdio server serves (`clientInfo.name` of its `initialize`).
static CLIENT: Mutex<Option<String>> = Mutex::new(None);

/// The client's name, once it said hello.
pub fn client_name() -> Option<String> {
    match CLIENT.lock() {
        Ok(g) => g.clone(),
        Err(p) => p.into_inner().clone(),
    }
}

/// A control-channel backend that connects on first use, and again after the app went away.
pub struct Lazy {
    addr: String,
    slot: Option<Remote>,
}

impl Lazy {
    pub fn new(addr: impl Into<String>) -> Lazy {
        Lazy { addr: addr.into(), slot: None }
    }
}

impl Backend for Lazy {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if self.slot.is_none() {
            let r = Remote::connect(&self.addr)
                .map_err(|e| format!("GridCraft control channel at {} is not reachable ({e}). Is GridCraft open?", self.addr))?;
            self.slot = Some(r);
        }
        let Some(b) = self.slot.as_mut() else { return Err("not connected".into()) };
        let r = b.call(method, params);
        if r.is_err() {
            // Reconnect next time: the app may have quit (and come back).
            self.slot = None;
        }
        r
    }
    fn has_ui(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "the GridCraft app (agent layer)".into()
    }
}

/// Serves newline-delimited JSON-RPC from `input` until it closes; replies (and anything written
/// through the same `output`, like channel events) never interleave.
pub fn serve(b: &mut dyn Backend, input: impl BufRead, output: Arc<Mutex<dyn Write + Send>>) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => {
                if msg.get("method").and_then(Value::as_str) == Some("initialize") {
                    let name = msg.pointer("/params/clientInfo/name").and_then(Value::as_str).unwrap_or("unknown").to_string();
                    if let Ok(mut g) = CLIENT.lock() {
                        *g = Some(name);
                    }
                }
                crate::server::handle_any(b, &msg)
            }
            Err(_) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}})),
        };
        if let Some(v) = reply {
            let mut w = match output.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            writeln!(w, "{v}")?;
            w.flush()?;
        }
    }
    Ok(())
}

/// `--agent-stdio`: serve on stdin/stdout until stdin closes. `control` is the app's control
/// channel, `agent_port` its HTTP server (for channel events). Returns the exit code.
pub fn run(control: &str, agent_port: u16) -> i32 {
    let out: Arc<Mutex<dyn Write + Send>> = Arc::new(Mutex::new(std::io::stdout()));
    let follower = out.clone();
    let _ = std::thread::Builder::new().name("channel-follow".into()).spawn(move || crate::channel::follow(agent_port, follower));
    let mut b = Lazy::new(control);
    match serve(&mut b, std::io::stdin().lock(), out) {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "gridcraft --agent-stdio: {e}");
            1
        }
    }
}
