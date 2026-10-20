//! Exports a workbook with its agent layer as one web page (`agent.exportHtml`), headless.
//!
//! `cargo run -p gridcraft-agent --example export_html -- book.xlsx [--web target/agent-web] out.html`
//! (`agent-layer/export-web.sh` builds the web app first and runs this.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

use gridcraft_engine::Session;
use serde_json::json;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut input, mut out, mut web) = (None, None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--web" => web = args.next(),
            _ if input.is_none() => input = Some(a),
            _ => out = Some(a),
        }
    }
    let (Some(input), Some(out)) = (input, out) else {
        eprintln!("usage: export_html book.xlsx [--web <web build>] out.html");
        return std::process::ExitCode::FAILURE;
    };
    gridcraft_agent::install();
    let mut s = Session::new();
    let run = |s: &mut Session, id: &str, p: serde_json::Value| s.execute(id, p).map_err(|e| eprintln!("{id}: {e}"));
    if run(&mut s, "file.open", json!({"path": input})).is_err() {
        return std::process::ExitCode::FAILURE;
    }
    let mut params = json!({"path": out});
    if let Some(w) = web {
        params["web"] = json!(w);
    }
    match run(&mut s, "agent.exportHtml", params) {
        Ok(r) => {
            println!("{r}");
            std::process::ExitCode::SUCCESS
        }
        Err(()) => std::process::ExitCode::FAILURE,
    }
}
