//! Push to talk in the running UI (egui_kittest): hold ⌥ and a recording starts, release and the
//! question is asked; ⌥ with another key is a shortcut, not a question. A stand-in microphone,
//! no transcription: this is about the key. Its own test binary, so the UI's globals are its own.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::Harness;
use gridcraft_agent::layer::{self, QuestionStatus};
use gridcraft_engine::Session;
use gridcraft_engine::core::CellRef;
use gridcraft_ui_egui::SheetApp;
use serde_json::json;

fn harness() -> Harness<'static, SheetApp> {
    gridcraft_agent_ui::install();
    let dir = std::env::temp_dir().join(format!("gridcraft-voice-key-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    gridcraft_agent_ui::voice::set_settings_path(Some(dir.join("voice.json")));
    // A second and a half of something above silence.
    gridcraft_agent_ui::voice::use_fake_microphone(Some((0..24_000).map(|i| ((i as f32) * 0.05).sin() * 0.1).collect()));
    let mut s = Session::new();
    s.new_workbook();
    for (cell, input) in [("A5", json!("Revenue")), ("B5", json!(1_250_000)), ("A6", json!("Costs")), ("B6", json!(870_000))] {
        s.execute("cell.set", json!({"cell": cell, "input": input})).unwrap();
    }
    s.execute("agent.voiceModel", json!({"model": "small"})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut SheetApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        SheetApp::new(s, Default::default()),
    );
    h.run_steps(4);
    h
}

fn click(h: &mut Harness<'static, SheetApp>, cell: &str) {
    let c = CellRef::parse(cell).unwrap();
    let app = h.state();
    let p = app.grid.geo.as_ref().and_then(|g| app.session.active().and_then(|d| d.wb.active()).map(|sh| g.cell_rect(sh, c).center())).unwrap();
    h.event(Event::PointerMoved(p));
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
}

/// macOS: the modifiers change first, ⌥'s own key event comes a frame later.
fn press_option(h: &mut Harness<'static, SheetApp>) {
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.run_steps(1);
    h.event(Event::Key { key: Key::AltLeft, pressed: true, modifiers: Modifiers::ALT, repeat: false, physical_key: None });
    h.run_steps(1);
}

fn release_option(h: &mut Harness<'static, SheetApp>) {
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.event(Event::Key { key: Key::AltLeft, pressed: false, modifiers: Modifiers::NONE, repeat: false, physical_key: None });
    h.run_steps(3);
}

fn recording() -> bool {
    let mut s = Session::new();
    s.execute("agent.voiceStatus", json!({})).unwrap()["recording"] == json!(true)
}

fn questions(h: &Harness<'static, SheetApp>) -> Vec<layer::Question> {
    layer::get(h.state().session.active().unwrap()).questions.clone()
}

#[test]
fn holding_option_records_and_releasing_asks() {
    let mut h = harness();
    // A click on a cell gives the grid keyboard focus: holding ⌥ must work from there.
    click(&mut h, "B5");
    assert_eq!(h.state().session.active().unwrap().selection.current().a1(), "B5");
    assert!(h.ctx.memory(|m| m.focused().is_some()), "the grid should have focus after a click");

    // ⌥ alone, held: nothing at first (a tap isn't a question), recording after a moment.
    press_option(&mut h);
    assert!(!recording());
    h.run_steps(20);
    assert!(recording(), "holding ⌥ should start recording");
    // Pointing while speaking.
    click(&mut h, "B6");
    // Release: the question is asked at once, on B5 with B6 pointed at.
    release_option(&mut h);
    assert!(!recording(), "releasing ⌥ should stop recording");
    let qs = questions(&h);
    assert_eq!(qs.len(), 1, "{qs:?}");
    assert!(qs[0].spoken);
    assert_eq!(qs[0].refs.len(), 1);
    // The stand-in microphone transcribes nothing: a failed flag, no dialog.
    assert_eq!(qs[0].status, QuestionStatus::Failed);
    assert!(h.state().message.is_none());

    // ⌥ with another key is a shortcut: no recording, no question.
    press_option(&mut h);
    h.event(Event::Key { key: Key::E, pressed: true, modifiers: Modifiers::ALT, repeat: false, physical_key: None });
    h.run_steps(25);
    assert!(!recording());
    release_option(&mut h);
    assert_eq!(questions(&h).len(), 1);

    // A tap: no recording, no question.
    press_option(&mut h);
    release_option(&mut h);
    h.run_steps(20);
    assert!(!recording());
    assert_eq!(questions(&h).len(), 1);
}
