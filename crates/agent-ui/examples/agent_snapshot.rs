//! Offscreen render of GridCraft with the agent layer, for looking at the UI without a window.
//!
//! `cargo run -p gridcraft-agent-ui --example agent_snapshot -- --in book.xlsx [--cmd id=json]…
//!  [--pane cell|questions|notes|history|connect] [--heat off|confidence|impact] [--hover B23]
//!  [--size 1440x900] [--dark] [--tab Agent] [--ping checking|answered|noanswer] [--signal wait]
//!  [--click <label>]… [--listener <label>] out.png`
#![allow(clippy::unwrap_used, clippy::expect_used)]

use egui_kittest::kittest::Queryable;
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::SheetApp;
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut input, mut cmds, mut pane, mut heat, mut hover, mut dark) = (None, vec![], None, "off".to_string(), None, false);
    let mut size = (1440.0f32, 900.0f32);
    let mut out = "agent.png".to_string();
    let mut tab: Option<String> = None;
    let (mut ping, mut signal, mut listener): (Option<String>, Option<String>, Option<String>) = (None, None, None);
    let mut click: Vec<String> = vec![];
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--in" => input = it.next(),
            "--cmd" => cmds.extend(it.next()),
            "--pane" => pane = it.next(),
            "--heat" => heat = it.next().unwrap_or_default(),
            "--hover" => hover = it.next(),
            "--dark" => dark = true,
            "--tab" => tab = it.next(),
            "--ping" => ping = it.next(),
            "--signal" => signal = it.next(),
            "--click" => click.extend(it.next()),
            "--listener" => listener = it.next(),
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|s| s.split_once('x').map(|(w, h)| (w.to_string(), h.to_string()))) {
                    size = (w.parse().unwrap_or(1440.0), h.parse().unwrap_or(900.0));
                }
            }
            other => out = other.to_string(),
        }
    }
    gridcraft_agent_ui::install();
    gridcraft_agent_ui::set_server_url("http://127.0.0.1:7980/mcp".into());
    if let Some(e) = gridcraft_agent::cowork::this_entry(7979) {
        gridcraft_agent_ui::set_stdio_entry(e);
    }
    let mut session = Session::new();
    match &input {
        Some(p) => {
            session.execute("file.open", json!({"path": p})).expect("open");
        }
        None => {
            session.new_workbook();
        }
    }
    for c in &cmds {
        let (id, p) = c.split_once('=').map(|(a, b)| (a.to_string(), serde_json::from_str(b).unwrap_or(json!({})))).unwrap_or((c.clone(), json!({})));
        if let Err(e) = session.execute(&id, p) {
            eprintln!("{id}: {e}");
        }
    }
    // A conversation attached to the active document, seen 20 s ago.
    let mut listener_id = None;
    if let Some(label) = &listener {
        let r = session.execute("agent.attach", json!({"label": label})).expect("attach");
        let id = r["listener"].as_str().unwrap_or("").to_string();
        let uid = session.active().expect("doc").uid;
        gridcraft_agent::docs::set_last_seen(uid, &id, gridcraft_agent::now_ms().saturating_sub(20_000));
        listener_id = Some(id);
    }
    // A connection check in a given state: checking, answered (3 s, 12 s ago), noanswer (75 s).
    if let Some(state) = ping.as_deref() {
        session.execute("agent.ping", json!({})).expect("ping");
        let d = session.active().expect("doc");
        let id = gridcraft_agent::layer::get(d).questions.iter().rev().find(|q| q.is_ping()).map(|q| q.id.clone()).unwrap_or_default();
        if state == "answered" {
            session.execute("agent.answer", json!({"id": id, "text": "here", "actor": "agent", "listener": listener_id})).expect("answer");
        }
        let back = match state {
            "answered" => 15_000,
            "noanswer" => 75_000,
            _ => 4_000,
        };
        gridcraft_agent::layer::update(session.active().expect("doc"), |l| {
            if let Some(q) = l.questions.iter_mut().find(|q| q.id == id) {
                q.asked_at = q.asked_at.saturating_sub(back);
                if let Some(a) = q.answer.as_mut() {
                    a.at = a.at.saturating_sub(12_000);
                }
            }
        });
    }
    // A listener without a check: an agent waiting for questions.
    if signal.as_deref() == Some("wait") {
        let _ = session.execute("agent.questions", json!({"status": "pending", "wait": true, "actor": "agent"}));
    }
    if let Some(tab) = &pane {
        gridcraft_agent_ui::request_pane(true, tab, &heat);
    } else {
        gridcraft_agent_ui::request_pane(false, "cell", &heat);
    }
    let mut harness =
        egui_kittest::Harness::builder().with_size(egui::vec2(size.0, size.1)).with_pixels_per_point(2.0).with_max_steps(12).wgpu().build_ui_state(
            move |ui, app: &mut SheetApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            {
                let mut app = SheetApp::new(session, Default::default());
                app.ui.dark = dark;
                if let Some(t) = tab {
                    app.ui.ribbon_tab = t;
                }
                app
            },
        );
    harness.run_steps(6);
    if let Some(cell) = hover.as_deref().and_then(CellRef::parse) {
        let app = harness.state();
        let pos = app.grid.geo.as_ref().and_then(|g| app.session.active().and_then(|d| d.wb.active()).map(|sh| g.cell_rect(sh, cell).center()));
        if let Some(p) = pos {
            harness.hover_at(p);
            // Rest on the cell past the hover delay (the harness clock steps 1/60 s per frame).
            harness.run_steps(40);
        }
    }
    // Open a ribbon menu: click the button whose label contains this.
    for label in &click {
        match harness.query_by_label(label) {
            Some(node) => node.click(),
            None => harness.get_by_label_contains(label).click(),
        }
        harness.run_steps(8);
    }
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    eprintln!("wrote {out} ({}x{})", img.width(), img.height());
}
