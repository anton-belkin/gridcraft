//! The in-app agent server: HTTP on 127.0.0.1, started by the app itself, so connecting an agent
//! is one line of MCP config (`http://127.0.0.1:7980/mcp`) and no extra process.
//!
//! - `POST /mcp` — MCP over Streamable HTTP (JSON responses).
//! - `GET /mcp` — the server-initiated stream (SSE): in "my session" mode each new question as a
//!   `notifications/claude/channel` event (Claude Code Channels).
//! - `GET /events` — one JSON line per new question, kept open: what an agent's background
//!   monitor watches (`curl -sN http://127.0.0.1:7980/events`).
//! - `GET /health` — `ok`.
//!
//! It reaches the document through the app's loopback control channel, like any other client.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use gridcraft_mcp::{Backend, Remote};
use serde_json::{Value, Value as Json, json};

/// Open `/events` streams, and channel streams (`GET /mcp`) of clients that take channel events.
static EVENT_STREAMS: AtomicUsize = AtomicUsize::new(0);
static CHANNEL_STREAMS: AtomicUsize = AtomicUsize::new(0);

/// Counts an open stream while it lives.
struct Open(&'static AtomicUsize);
impl Open {
    fn new(c: &'static AtomicUsize) -> Open {
        c.fetch_add(1, Ordering::Relaxed);
        Open(c)
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `/events?document=` streams open now, by document id.
static DOC_STREAMS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct DocStream(String);
impl DocStream {
    fn open(doc: &str) -> DocStream {
        if let Ok(mut g) = DOC_STREAMS.lock() {
            g.push(doc.to_string());
        }
        DocStream(doc.to_string())
    }
}
impl Drop for DocStream {
    fn drop(&mut self) {
        if let Ok(mut g) = DOC_STREAMS.lock()
            && let Some(i) = g.iter().position(|d| *d == self.0)
        {
            g.remove(i);
        }
    }
}

/// Whichever stream is open (held for its lifetime).
#[allow(dead_code)]
enum Opened {
    All(Open),
    Doc(DocStream),
}

/// `/events` streams following one document now.
pub fn document_streams(doc: &str) -> usize {
    DOC_STREAMS.lock().map(|g| g.iter().filter(|d| *d == doc).count()).unwrap_or(0)
}

/// Listeners connected to this server now: (`/events` streams, channel streams).
pub fn streams() -> (usize, usize) {
    (EVENT_STREAMS.load(Ordering::Relaxed), CHANNEL_STREAMS.load(Ordering::Relaxed))
}

/// Default port of the agent server.
pub const DEFAULT_PORT: u16 = 7980;
/// Largest request body accepted.
const MAX_BODY: usize = 16 * 1024 * 1024;
/// Largest request head accepted.
const MAX_HEAD: usize = 64 * 1024;

/// Starts the server on `port` (0 = any) talking to the control channel at `control`.
/// Returns the port it listens on.
pub fn start(port: u16, control: String) -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    std::thread::Builder::new().name("agent-server".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let control = control.clone();
            let _ = std::thread::Builder::new().name("agent-conn".into()).spawn(move || serve(stream, &control));
        }
    })?;
    log::info!("agent MCP server on http://127.0.0.1:{port}/mcp");
    Ok(port)
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = r.by_ref().take(MAX_HEAD as u64).read_until(b'\n', &mut line).ok()?;
        if n == 0 {
            return None;
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEAD {
            return None;
        }
    }
    let head = String::from_utf8(head).ok()?;
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let target = first.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request { method, path, query, headers, body })
}

fn respond(w: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    write!(w, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n", body.len())?;
    w.write_all(body)?;
    w.flush()
}

/// Browsers may only reach us from pages on this machine (DNS-rebinding guard, as MCP asks).
fn origin_ok(req: &Request) -> bool {
    match req.header("origin") {
        None => true,
        Some(o) => {
            let host = o.split("://").nth(1).unwrap_or(o).split(':').next().unwrap_or("");
            matches!(host, "localhost" | "127.0.0.1" | "[::1]") || o == "null"
        }
    }
}

fn serve(stream: TcpStream, control: &str) {
    let _ = stream.set_nodelay(true);
    let Ok(mut w) = stream.try_clone() else { return };
    let mut r = BufReader::new(stream);
    let mut backend: Option<Remote> = None;
    while let Some(req) = read_request(&mut r) {
        if !origin_ok(&req) {
            let _ = respond(&mut w, "403 Forbidden", "text/plain", b"origin not allowed");
            return;
        }
        let result = match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/mcp") => post_mcp(&mut w, &req, control, &mut backend),
            ("GET", "/mcp") => {
                // The stdio server says which client it serves: only Claude Code takes channel events.
                let client = req.query.split('&').find_map(|kv| kv.strip_prefix("client=")).unwrap_or("").to_string();
                channel_stream(w, control, &client);
                return;
            }
            ("DELETE", "/mcp") => respond(&mut w, "200 OK", "text/plain", b""),
            ("GET", "/events") => {
                events(w, control, &req.query);
                return;
            }
            ("GET", "/health") => respond(&mut w, "200 OK", "text/plain", b"ok"),
            ("GET", "/") => respond(&mut w, "200 OK", "text/plain", HELP.as_bytes()),
            _ => respond(&mut w, "404 Not Found", "text/plain", b"not found"),
        };
        if result.is_err() || req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close")) {
            return;
        }
    }
}

const HELP: &str = "GridCraft agent layer\n\nPOST /mcp     MCP (Streamable HTTP)\nGET  /mcp     questions as channel events (SSE, in \"my session\" mode)\nGET  /events  one JSON line per new question (for a background monitor)\nGET  /health  ok\n";

fn backend<'a>(slot: &'a mut Option<Remote>, control: &str) -> Result<&'a mut Remote, String> {
    if slot.is_none() {
        *slot = Some(Remote::connect(control).map_err(|e| format!("GridCraft control channel at {control} is not reachable: {e}"))?);
    }
    slot.as_mut().ok_or_else(|| "not connected".to_string())
}

fn post_mcp(w: &mut TcpStream, req: &Request, control: &str, slot: &mut Option<Remote>) -> std::io::Result<()> {
    let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
        let e = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}});
        return respond(w, "400 Bad Request", "application/json", e.to_string().as_bytes());
    };
    let reply = match backend(slot, control) {
        Ok(b) => handle_any(b, &msg),
        Err(e) => msg.get("id").map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": e}})),
    };
    match reply {
        Some(v) => respond(w, "200 OK", "application/json", v.to_string().as_bytes()),
        None => respond(w, "202 Accepted", "text/plain", b""),
    }
}

pub(crate) fn handle_any(b: &mut dyn Backend, msg: &Value) -> Option<Value> {
    match msg {
        Value::Array(batch) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| crate::mcp::handle(b, m)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => crate::mcp::handle(b, m),
    }
}

/// The `GET /mcp` stream: in "my session" mode, each new pending question as a channel event.
/// Questions that wait while the mode is off are sent when it is switched on.
fn channel_stream(mut w: TcpStream, control: &str, client: &str) {
    // Claude Desktop and Cowork don't take channel events: their stdio servers aren't listeners.
    let listens = client.is_empty() || client.to_ascii_lowercase().contains("claude-code");
    let _open = listens.then(|| Open::new(&CHANNEL_STREAMS));
    if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n: connected\n\n")
        .is_err()
    {
        return;
    }
    let _ = w.flush();
    let mut seen: HashSet<String> = HashSet::new();
    let mut gone: HashSet<String> = HashSet::new();
    let mut slot: Option<Remote> = None;
    let mut quiet = 0u32;
    loop {
        if crate::runner::mode() == crate::runner::Mode::Channel
            && let Ok(b) = backend(&mut slot, control)
        {
            match b.call(
                "engine.execute",
                json!({"command": "agent.questions", "params": {"status": "pending", "actor": "agent", "masked": true, "poll": true}}),
            ) {
                Ok(Value::Array(list)) => {
                    for q in list {
                        let Some(id) = q.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
                        if !seen.insert(id) {
                            continue;
                        }
                        let event = format!("event: message\ndata: {}\n\n", crate::channel::notification(&q));
                        if w.write_all(event.as_bytes()).and_then(|()| w.flush()).is_err() {
                            return;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => slot = None,
            }
            // Questions the user took back: the session drops them.
            if let Some(b) = slot.as_mut() {
                for q in withdrawn(b, None, &mut gone) {
                    let n = json!({"jsonrpc": "2.0", "method": "notifications/claude/channel", "params": {
                        "content": format!("Question {} was withdrawn by the user: drop it.", q.0),
                        "meta": {"question_id": q.0, "kind": "withdrawn", "document": q.1},
                    }});
                    if w.write_all(format!("event: message\ndata: {n}\n\n").as_bytes()).and_then(|()| w.flush()).is_err() {
                        return;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(800));
        quiet += 1;
        // A comment now and then: notices a client that went away.
        if quiet >= 20 {
            quiet = 0;
            if w.write_all(b": ping\n\n").and_then(|()| w.flush()).is_err() {
                return;
            }
        }
    }
}

/// Questions withdrawn since the last look (each once): (id, document).
fn withdrawn(b: &mut Remote, document: Option<&str>, gone: &mut HashSet<String>) -> Vec<(String, Json)> {
    let params = json!({"status": "withdrawn", "actor": "agent", "masked": true, "poll": true});
    let call = match document {
        Some(d) => {
            json!({"command": "agent.onDocument", "params": {"document": d, "command": "agent.questions", "params": params, "actor": "agent"}})
        }
        None => json!({"command": "agent.questions", "params": params}),
    };
    let Ok(Json::Array(list)) = b.call("engine.execute", call) else { return vec![] };
    list.into_iter()
        .filter_map(|q| {
            let id = q.get("id").and_then(Json::as_str)?.to_string();
            let doc = q.get("document").cloned().unwrap_or(Json::Null);
            gone.insert(format!("{doc}#{id}")).then_some((id, doc))
        })
        .collect()
}

/// Streams new pending questions, one JSON object per line, until the client goes away.
fn events(mut w: TcpStream, control: &str, query: &str) {
    // `?document=<id>` streams only that document's questions; `&listener=` keeps that listener
    // listed while the stream is open.
    let param = |k: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(|v| v.replace("%3A", ":").replace("%3a", ":")).filter(|v| !v.is_empty())
    };
    let (document, listener) = (param("document"), param("listener"));
    let _open = match &document {
        Some(d) => Opened::Doc(DocStream::open(d)),
        None => Opened::All(Open::new(&EVENT_STREAMS)),
    };
    if w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n").is_err() {
        return;
    }
    let _ = w.flush();
    let mut seen: HashSet<String> = HashSet::new();
    let mut gone: HashSet<String> = HashSet::new();
    let mut slot: Option<Remote> = None;
    loop {
        if crate::runner::mode() != crate::runner::Mode::Off
            && let Ok(b) = backend(&mut slot, control)
        {
            let mut params = json!({"status": "pending", "actor": "agent", "masked": true, "poll": true});
            if let Some(l) = &listener {
                params["listener"] = json!(l);
            }
            let call = match &document {
                Some(d) => {
                    json!({"command": "agent.onDocument", "params": {"document": d, "command": "agent.questions", "params": params, "actor": "agent"}})
                }
                None => json!({"command": "agent.questions", "params": params}),
            };
            match b.call("engine.execute", call) {
                Ok(Value::Array(list)) => {
                    for q in list {
                        let Some(id) = q.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
                        // Ids are per document: the active one may change under an unfiltered stream.
                        let key = format!("{}#{id}", q.get("document").and_then(Value::as_str).unwrap_or(""));
                        if !seen.insert(key) {
                            continue;
                        }
                        let line = json!({
                            "event": "question",
                            "id": id,
                            "document": q.get("document"),
                            "documentTitle": q.get("documentTitle"),
                            "kind": q.get("kind").cloned().unwrap_or_else(|| json!("question")),
                            "range": q.get("range"),
                            "label": q.get("label"),
                            "question": q.get("text"),
                            "hint": "claim_question, investigate, answer_question",
                        });
                        if writeln!(w, "{line}").and_then(|()| w.flush()).is_err() {
                            return;
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => slot = None,
            }
            if let Some(b) = slot.as_mut() {
                for (id, doc) in withdrawn(b, document.as_deref(), &mut gone) {
                    let line = json!({"event": "withdrawn", "id": id, "document": doc, "hint": "the user took this question back: drop it"});
                    if writeln!(w, "{line}").and_then(|()| w.flush()).is_err() {
                        return;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(800));
        // Notice a client that went away even when nothing is being written.
        if w.peer_addr().is_err() {
            return;
        }
    }
}
