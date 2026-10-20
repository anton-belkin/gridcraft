//! "My session" mode (Claude Code Channels, research preview): the app pushes each new question on
//! its `GET /mcp` stream, and the stdio server ([`crate::stdio`]) passes them on to the Claude Code
//! session that started it as `notifications/claude/channel`:
//!
//! ```text
//! claude mcp add gridcraft-channel -- /path/to/gridcraft --agent-stdio
//! claude --dangerously-load-development-channels server:gridcraft-channel
//! ```

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value as Json, json};

/// The channel notification for a question (as the app's `GET /mcp` stream sends it).
pub fn notification(q: &Json) -> Json {
    let get = |k: &str| q.get(k).and_then(Json::as_str).unwrap_or("").to_string();
    let label = get("label");
    let kind = q.get("kind").and_then(Json::as_str).unwrap_or("question").to_string();
    if kind == crate::layer::PING {
        return json!({
            "jsonrpc": "2.0",
            "method": "notifications/claude/channel",
            "params": {"content": get("text"), "meta": {"question_id": get("id"), "kind": kind, "document": get("document")}},
        });
    }
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
            "meta": {"question_id": get("id"), "kind": kind, "document": get("document"), "range": get("range"), "label": label},
        },
    })
}

/// Follows the app's `GET /mcp` stream and passes each event on; reconnects while the app is
/// away.
pub fn follow(port: u16, out: Arc<Mutex<dyn Write + Send>>) {
    // Say which client this is (the app counts only Claude Code as a channel listener).
    let mut client = None;
    for _ in 0..150 {
        client = crate::stdio::client_name();
        if client.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let client: String = client.unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect();
    loop {
        if let Ok(mut c) = TcpStream::connect(("127.0.0.1", port)) {
            let req = format!("GET /mcp?client={client} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n");
            if c.write_all(req.as_bytes()).is_ok() {
                for line in BufReader::new(c).lines().map_while(std::result::Result::ok) {
                    if let Some(data) = line.strip_prefix("data:")
                        && let Ok(v) = serde_json::from_str::<Json>(data.trim())
                    {
                        let mut w = match out.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        let _ = writeln!(w, "{v}");
                        let _ = w.flush();
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}
