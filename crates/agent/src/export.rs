//! Export to a rich web page: one HTML file the recipient opens in a browser, with nothing to
//! install. It holds GridCraft for the web (the `gridcraft-web` WASM build and its JavaScript glue,
//! gzipped) and the workbook with its agent layer, opened on start with the layer read-only.
//!
//! Topics go with their places, status and notes; the discussion (questions and answers) stays
//! behind unless the sender includes it, and then only answered questions, shown as new (the
//! recipient can't reach the author's agent).
//!
//! The web build comes from a folder holding `<name>.js` and `<name>_bg.wasm` (wasm-bindgen's
//! output, or Trunk's `dist/web`): the `web` param, `$GRIDCRAFT_WEB_BUNDLE`, else `target/agent-web`
//! near the working directory or the executable. `agent-layer/export-web.sh` builds it.

use gridcraft_engine::{EngineError, Result, Session};
use serde_json::{Value as Json, json};

use crate::layer::{self, Layer, QuestionStatus};

/// The id of the element holding the workbook (base64) in an exported page.
pub const WORKBOOK_ELEMENT: &str = "gridcraft_workbook";

/// The layer as the recipient gets it: topics with places, status and notes, inputs, client data
/// marks, confidence and history; the discussion only with `discussion` (answered questions,
/// shown as new). Undone topics and anchors nothing uses any more are left out.
pub fn shared_layer(l: &Layer, discussion: bool) -> Layer {
    let mut out = l.clone();
    out.topics.retain(|t| !t.hidden);
    // Proposals belong to the conversation; the author's context setting (and folder) stays home.
    if !discussion {
        for t in &mut out.topics {
            t.edits = None;
        }
    }
    out.context = None;
    out.questions.retain(|q| discussion && q.status == QuestionStatus::Answered && q.answer.is_some() && !q.hidden);
    for q in &mut out.questions {
        q.seen = false;
    }
    let unused = out.unused_anchors();
    out.anchors.retain(|a| !unused.contains(&a.id));
    out
}

/// `agent.exportHtml {path, web?, includeDiscussion?}`.
pub fn export_html(s: &mut Session, p: &Json) -> Result<Json> {
    let bad = |msg: String| EngineError::BadParams { cmd: "agent.exportHtml".into(), msg };
    let path = p.get("path").and_then(Json::as_str).map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad("missing `path`".into()))?;
    let d = s.doc()?;
    let mut wb = (*d.wb).clone();
    let discussion = p.get("includeDiscussion").and_then(Json::as_bool).unwrap_or(false);
    layer::write_into(&mut wb, &shared_layer(&layer::get(d), discussion));
    let xlsx = gridcraft_engine::io::save_bytes_as(&wb, "book.xlsx", gridcraft_engine::io::FileKind::Xlsx)?;
    let title = std::path::Path::new(&d.title).file_stem().and_then(|t| t.to_str()).filter(|t| !t.is_empty()).unwrap_or("Workbook").to_string();
    let bundle = bundle::find(p.get("web").and_then(Json::as_str))?;
    let html = page(&title, &format!("{title}.xlsx"), &xlsx, &bundle)?;
    gridcraft_engine::io::write_file(path, html.as_bytes())?;
    Ok(json!({"path": path, "bytes": html.len(), "web": bundle.dir}))
}

/// The page: a canvas, the workbook, the gzipped app, and a loader that starts it.
fn page(title: &str, file_name: &str, xlsx: &[u8], bundle: &bundle::Bundle) -> Result<String> {
    use gridcraft_engine::io::base64_encode as b64;
    let js = b64(&bundle::gzip(&bundle.js)?);
    let wasm = b64(&bundle::gzip(&bundle.wasm)?);
    let title = escape(title);
    let file_name = escape(file_name);
    let workbook = b64(xlsx);
    Ok(format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, user-scalable=no">
<meta name="generator" content="GridCraft {version} (agent layer)">
<title>{title} · GridCraft</title>
<style>
  html, body {{ margin: 0; padding: 0; width: 100%; height: 100%; overflow: hidden; background: #f3f3f3; }}
  #gridcraft_canvas {{ position: absolute; inset: 0; width: 100%; height: 100%; display: block; }}
  #gridcraft_loading {{ position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center;
    color: #555; font: 14px system-ui, sans-serif; pointer-events: none; text-align: center; }}
</style>
</head>
<body>
<canvas id="gridcraft_canvas"></canvas>
<div id="gridcraft_loading">Opening {title}&hellip;</div>
<script id="{WORKBOOK_ELEMENT}" type="application/octet-stream" data-name="{file_name}">{workbook}</script>
<script id="gridcraft_js" type="application/octet-stream">{js}</script>
<script id="gridcraft_wasm" type="application/octet-stream">{wasm}</script>
<script type="module">
const take = (id) => {{
  const el = document.getElementById(id);
  const s = atob(el.textContent.trim());
  el.remove();
  const b = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) b[i] = s.charCodeAt(i);
  return b;
}};
const gunzip = async (b) => new Uint8Array(await new Response(new Blob([b]).stream().pipeThrough(new DecompressionStream("gzip"))).arrayBuffer());
try {{
  const glue = await gunzip(take("gridcraft_js"));
  const app = await import(URL.createObjectURL(new Blob([glue], {{ type: "text/javascript" }})));
  await app.default({{ module_or_path: await gunzip(take("gridcraft_wasm")) }});
}} catch (e) {{
  document.getElementById("gridcraft_loading").textContent = "GridCraft couldn't start in this browser: " + e;
}}
</script>
</body>
</html>
"#,
        version = env!("CARGO_PKG_VERSION"),
    ))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(not(target_arch = "wasm32"))]
mod bundle {
    use std::io::Write as _;
    use std::path::{Path, PathBuf};

    use gridcraft_engine::{EngineError, Result};

    /// Biggest file taken from the web build (the app is a few tens of MB).
    const MAX_FILE: u64 = 512 * 1024 * 1024;

    pub struct Bundle {
        pub dir: String,
        pub js: Vec<u8>,
        pub wasm: Vec<u8>,
    }

    pub fn find(web: Option<&str>) -> Result<Bundle> {
        let mut dirs: Vec<PathBuf> = vec![];
        if let Some(w) = web.map(str::trim).filter(|w| !w.is_empty()) {
            dirs.push(w.into());
        } else {
            if let Ok(w) = std::env::var("GRIDCRAFT_WEB_BUNDLE") {
                dirs.push(w.into());
            }
            if let Ok(cwd) = std::env::current_dir() {
                dirs.extend(cwd.ancestors().map(|a| a.join("target").join("agent-web")));
            }
            if let Ok(exe) = std::env::current_exe() {
                dirs.extend(exe.ancestors().skip(1).take(3).flat_map(|a| [a.join("agent-web"), a.join("web")]));
            }
        }
        for dir in &dirs {
            if let Some(b) = load(dir) {
                return Ok(b);
            }
        }
        Err(EngineError::Other(format!(
            "GridCraft for the web isn't built: run agent-layer/export-web.sh, or pass `web` (looked in {})",
            dirs.first().map(|d| d.display().to_string()).unwrap_or_default()
        )))
    }

    /// `<name>.js` beside `<name>_bg.wasm`.
    fn load(dir: &Path) -> Option<Bundle> {
        let entries = std::fs::read_dir(dir).ok()?;
        for e in entries.flatten() {
            let path = e.path();
            let Some(stem) = path.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_suffix(".js")) else { continue };
            let wasm = dir.join(format!("{stem}_bg.wasm"));
            if !wasm.is_file() {
                continue;
            }
            return Some(Bundle { dir: dir.display().to_string(), js: read(&path)?, wasm: read(&wasm)? });
        }
        None
    }

    fn read(path: &Path) -> Option<Vec<u8>> {
        (std::fs::metadata(path).ok()?.len() <= MAX_FILE).then(|| std::fs::read(path).ok()).flatten()
    }

    pub fn gzip(data: &[u8]) -> Result<Vec<u8>> {
        let mut z = flate2::write::GzEncoder::new(Vec::with_capacity(data.len() / 3), flate2::Compression::best());
        z.write_all(data).and_then(|()| z.finish()).map_err(|e| EngineError::Other(format!("compressing the web build: {e}")))
    }
}

#[cfg(target_arch = "wasm32")]
mod bundle {
    use gridcraft_engine::{EngineError, Result};

    pub struct Bundle {
        pub dir: String,
        pub js: Vec<u8>,
        pub wasm: Vec<u8>,
    }

    pub fn find(_web: Option<&str>) -> Result<Bundle> {
        Err(EngineError::Other("Export as Web Page works in the desktop app".into()))
    }

    pub fn gzip(_data: &[u8]) -> Result<Vec<u8>> {
        Err(EngineError::Other("Export as Web Page works in the desktop app".into()))
    }
}
