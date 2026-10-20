//! The AItoCraft hub as GridCraft points to it: the one MCP server configured once in Claude
//! Desktop / Cowork and Claude Code, serving every open document of every Craft app. GridCraft
//! only finds its binary, asks it to install itself in Claude Desktop, and gives the user the
//! phrase that starts a conversation on a document (protocol §3).

use std::path::PathBuf;
use std::sync::Mutex;

/// The phrase the user pastes into Claude to work on a document (protocol §3).
pub fn phrase(title: &str) -> String {
    format!("Work with me on \"{title}\" in GridCraft through AItoCraft: attach to it and answer my questions from the document as they come.")
}

/// The `aitocraft` binary: `AITOCRAFT_BIN`, the PATH, or the usual places.
pub fn bin() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AITOCRAFT_BIN").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let on_path = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join("aitocraft")).find(|p| p.is_file()));
    if on_path.is_some() {
        return on_path;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    [
        home.as_ref().map(|h| h.join("aitocraft/target/release/aitocraft")),
        Some(PathBuf::from("/Applications/AItoCraft.app/Contents/MacOS/aitocraft")),
        home.as_ref().map(|h| h.join(".local/bin/aitocraft")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}

/// `claude mcp add` for Claude Code (user scope: every project).
pub fn claude_code_command() -> String {
    let b = bin().map(|p| p.display().to_string()).unwrap_or_else(|| "aitocraft".into());
    format!("claude mcp add --scope user aitocraft -- '{b}'")
}

/// Starting Claude Code so that questions arrive in the session as channel events.
pub const CHANNEL_COMMAND: &str = "claude --dangerously-load-development-channels server:aitocraft";

/// What installing in Claude Desktop said (set when it finishes).
static RESULT: Mutex<Option<String>> = Mutex::new(None);

/// The result of the last [`install_desktop`], once (taken).
pub fn take_result() -> Option<String> {
    RESULT.lock().ok().and_then(|mut g| g.take())
}

/// Runs `aitocraft --install-desktop` in the background (it may wait for Claude to quit). `Err`
/// says where to get AItoCraft when it isn't here.
pub fn install_desktop() -> Result<(), String> {
    let Some(b) = bin() else {
        return Err("AItoCraft isn't on this computer. Put the `aitocraft` program on your PATH (or set AITOCRAFT_BIN to it), then press Install again. It is the one connection for all Craft apps: Claude Desktop, Cowork and Claude Code.".into());
    };
    std::thread::Builder::new()
        .name("aitocraft-install".into())
        .spawn(move || {
            let msg = match std::process::Command::new(&b).arg("--install-desktop").output() {
                Ok(o) => {
                    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)).trim().to_string();
                    if o.status.success() {
                        if text.is_empty() {
                            "AItoCraft is installed in Claude Desktop. Open Claude (quit it with ⌘Q first if it was open).".to_string()
                        } else {
                            text
                        }
                    } else {
                        format!("aitocraft --install-desktop failed: {text}")
                    }
                }
                Err(e) => format!("Couldn't run {}: {e}", b.display()),
            };
            if let Ok(mut g) = RESULT.lock() {
                *g = Some(msg);
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}
