//! "My session" mode: questions asked in the workbook arrive in the user's own interactive Claude
//! Code session as channel events (Claude Code Channels, research preview).
//!
//! Claude Code spawns channel servers itself and talks to them over stdio, so `gridcraft
//! --agent-stdio` runs this bridge instead of the app: MCP requests from stdin are forwarded to the
//! running app's server (`POST /mcp`), and the questions the app pushes on its `GET /mcp` stream go
//! out on stdout as `notifications/claude/channel`. Set up once:
//!
//! ```text
//! claude mcp add gridcraft -- /path/to/gridcraft --agent-stdio
//! claude --dangerously-load-development-channels server:gridcraft
//! ```

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};

/// The channel notification for a question (as the app's `GET /mcp` stream sends it).
pub fn notification(q: &Json) -> Json {
    let get = |k: &str| q.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let label = get("label");
    let mut content = format!("Question on {}", get("range"));
    if !label.is_empty() {
        content.push_str(&format!(" ({label})"));
    }
    content.push_str(&format!(": {}", get("text")));
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": content,
            // Keys must be identifiers (letters, digits, underscores).
            "meta": {"question_id": get("id"), "range": get("range"), "label": label},
        },
    })
}

/// Sends one HTTP request and returns (status, body).
fn http(port: u16, method: &str, path: &str, body: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
    let mut c = TcpStream::connect(("127.0.0.1", port))?;
    let _ = c.set_read_timeout(Some(Duration::from_secs(120)));
    write!(
        c,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    c.write_all(body)?;
    c.flush()?;
    let mut r = BufReader::new(c);
    let mut status_line = String::new();
    r.read_line(&mut status_line)?;
    let status = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut len: Option<usize> = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok();
        }
    }
    let mut out = vec![];
    match len {
        Some(n) => {
            out.resize(n.min(64 * 1024 * 1024), 0);
            r.read_exact(&mut out)?;
        }
        None => {
            r.take(64 * 1024 * 1024).read_to_end(&mut out)?;
        }
    }
    Ok((status, out))
}

fn emit(out: &Arc<Mutex<std::io::Stdout>>, v: &Json) {
    let g = match out.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let mut w = g.lock();
    let _ = writeln!(w, "{v}");
    let _ = w.flush();
}

/// Follows the app's `GET /mcp` stream and passes each event on; reconnects while the app is
/// away.
fn follow(port: u16, out: Arc<Mutex<std::io::Stdout>>) {
    loop {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) {
            let req = format!("GET /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n");
            if c.write_all(req.as_bytes()).is_ok() {
                for line in BufReader::new(c).lines().map_while(std::result::Result::ok) {
                    if let Some(data) = line.strip_prefix("data:")
                        && let Ok(v) = serde_json::from_str::<Json>(data.trim())
                    {
                        emit(&out, &v);
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Runs the stdio bridge until stdin closes. Returns the process exit code.
pub fn run_stdio(port: u16) -> i32 {
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let follower = out.clone();
    let _ = std::thread::Builder::new().name("channel-follow".into()).spawn(move || follow(port, follower));
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(std::result::Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Json>(line) else { continue };
        let id = msg.get("id").cloned();
        match http(port, "POST", "/mcp", line.as_bytes()) {
            Ok((200, body)) => {
                if let Ok(v) = serde_json::from_slice::<Json>(&body) {
                    emit(&out, &v);
                }
            }
            Ok(_) => {}
            Err(e) => {
                if let Some(id) = id {
                    let m = format!("GridCraft isn't running (agent server on 127.0.0.1:{port}): {e}. Start GridCraft, then /mcp reconnect.");
                    emit(&out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32603, "message": m}}));
                }
            }
        }
    }
    0
}
