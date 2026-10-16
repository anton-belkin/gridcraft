//! Offscreen render of GridCraft with the agent layer, for looking at the UI without a window.
//!
//! `cargo run -p gridcraft-agent-ui --example agent_snapshot -- --in book.xlsx [--cmd id=json]…
//!  [--pane cell|questions|notes|history|connect] [--heat off|confidence|impact] [--hover B23]
//!  [--size 1440x900] [--dark] [--tab Agent] out.png`
#![allow(clippy::unwrap_used, clippy::expect_used)]

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
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    eprintln!("wrote {out} ({}x{})", img.width(), img.height());
}
