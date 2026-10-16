#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gridcraft_engine::Session;
use serde_json::{Value, json};

use crate::layer;

/// A small model: price × volume − costs, with a client name and a private volume.
fn model() -> Session {
    crate::install();
    test_keys();
    let mut s = Session::new();
    s.new_workbook();
    for (cell, input) in [
        ("A1", json!("Client")),
        ("B1", json!("Acme Holding AG")),
        ("A2", json!("Price")),
        ("B2", json!(120)),
        ("A3", json!("Volume")),
        ("B3", json!(5000)),
        ("A4", json!("Unit cost")),
        ("B4", json!(70)),
        ("A5", json!("Fixed costs")),
        ("B5", json!(100000)),
        ("A6", json!("Revenue")),
        ("B6", json!("=B2*B3")),
        ("A7", json!("Profit")),
        ("B7", json!("=B6-B3*B4-B5")),
    ] {
        s.execute("cell.set", json!({"cell": cell, "input": input})).unwrap();
    }
    s
}

/// Keys made by tests go to a temporary folder, never the user's key store.
fn test_keys() {
    crate::safe::set_thread_key_dir(Some(std::env::temp_dir().join(format!("gridcraft-agent-test-keys-{}", std::process::id()))));
}

fn num(s: &Session, cell: &str) -> f64 {
    let d = s.active().unwrap();
    let c = gridcraft_engine::core::CellRef::parse(cell).unwrap();
    d.wb.active().unwrap().cell(c).unwrap().value.as_f64().unwrap()
}

#[test]
fn question_round_trip() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit negative?"})).unwrap();
    let id = q["id"].as_str().unwrap().to_string();
    let pending = s.execute("agent.questions", json!({"status": "pending"})).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["range"], json!("Sheet1!B7"));
    assert_eq!(pending[0]["label"], json!("Profit"));
    s.execute("agent.claim", json!({"id": id})).unwrap();
    assert!(s.execute("agent.claim", json!({"id": id})).is_err());
    s.execute(
        "agent.answer",
        json!({"id": id, "text": "Fixed costs exceed the margin.", "links": [{"label": "Fixed costs", "href": "cell:Sheet1!B5"}]}),
    )
    .unwrap();
    let all = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(all[0]["status"], json!("answered"));
    assert_eq!(all[0]["answer"]["links"][0]["href"], json!("cell:Sheet1!B5"));
}

#[test]
fn anchors_follow_inserted_rows_and_survive_undo() {
    let mut s = model();
    s.execute("agent.ask", json!({"range": "B7", "text": "?"})).unwrap();
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    let q = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q[0]["range"], json!("Sheet1!B8"), "{q}");
    // Undoing the insert moves the anchor back; undoing past the question keeps the question.
    s.execute("edit.undo", json!({})).unwrap();
    let q = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q[0]["range"], json!("Sheet1!B7"));
    for _ in 0..20 {
        let _ = s.execute("edit.undo", json!({}));
    }
    let q = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q.as_array().unwrap().len(), 1);
}

#[test]
fn private_cells_are_masked_for_the_agent() {
    let mut s = model();
    s.execute("agent.private", json!({"range": "B1:B3"})).unwrap();
    let r = s.execute("agent.read", json!({"range": "A1:B7"})).unwrap();
    let cells = r["cells"].as_array().unwrap();
    let get = |c: &str| cells.iter().find(|x| x["cell"] == json!(c)).unwrap().clone();
    assert_ne!(get("B1")["value"], json!("Acme Holding AG"));
    assert_eq!(get("B1")["private"], json!(true));
    let price = get("B2")["value"].as_f64().unwrap();
    let volume = get("B3")["value"].as_f64().unwrap();
    assert_ne!(price, 120.0);
    assert_ne!(volume, 5000.0);
    assert!(price > 0.0 && (10.0..1000.0).contains(&price));
    // Derived values come from the stand-ins, so they don't leak the real numbers either.
    assert_eq!(get("B6")["value"].as_f64().unwrap(), price * volume);
    assert_eq!(get("B6")["derivedFromPrivate"], json!(true));
    assert!(get("B5").get("derivedFromPrivate").is_none());
    // The real workbook is untouched; the person sees real numbers.
    assert_eq!(num(&s, "B6"), 600000.0);
    let real = s.execute("agent.read", json!({"range": "B2", "masked": false})).unwrap();
    assert_eq!(real["cells"][0]["value"], json!(120.0));
    // The agent can't write client data or read through other commands.
    assert!(s.execute("agent.exec", json!({"command": "cell.set", "params": {"cell": "B2", "input": 1}})).is_err());
    assert!(s.execute("agent.exec", json!({"command": "file.saveBytes"})).is_err());
    assert!(s.execute("agent.private", json!({"range": "B1:B3", "on": false, "actor": "agent"})).is_err());
}

#[test]
fn trace_reaches_the_inputs() {
    let mut s = model();
    let t = s.execute("agent.trace", json!({"cell": "B7"})).unwrap();
    assert_eq!(t["formula"], json!("=B6-B3*B4-B5"));
    let text = t.to_string();
    for label in ["Revenue", "Price", "Volume", "Unit cost", "Fixed costs"] {
        assert!(text.contains(label), "{label} missing in {t}");
    }
}

#[test]
fn sensitivity_ranks_inputs() {
    let mut s = model();
    s.execute("agent.input", json!({"cell": "B2", "low": 100, "high": 140, "rationale": "list price band"})).unwrap();
    let r = s.execute("agent.sensitivity", json!({"cell": "B7", "masked": false})).unwrap();
    assert_eq!(r["value"], json!(150000.0));
    let inputs = r["inputs"].as_array().unwrap();
    let price = inputs.iter().find(|i| i["input"] == json!("Sheet1!B2")).unwrap();
    assert_eq!(price["outputAtLow"], json!(50000.0));
    assert_eq!(price["outputAtHigh"], json!(250000.0));
    assert_eq!(price["reasoned"], json!(true));
    // Price ±20 moves profit by ±100k: the widest bar comes first.
    assert_eq!(inputs[0]["input"], json!("Sheet1!B2"));
    // The real model is unchanged.
    assert_eq!(num(&s, "B7"), 150000.0);
}

#[test]
fn layer_travels_in_the_xlsx() {
    let mut s = model();
    s.execute("agent.ask", json!({"range": "B7", "text": "Source of fixed costs?"})).unwrap();
    s.execute("agent.note", json!({"range": "B5", "text": "From the 2025 budget, line 14.", "kind": "source", "actor": "agent"})).unwrap();
    s.execute("agent.private", json!({"range": "B1"})).unwrap();
    let bytes = s.execute("file.saveBytes", json!({})).unwrap();
    let b64 = bytes["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    let l = layer::get(t.active().unwrap());
    assert_eq!(l.questions.len(), 1);
    assert_eq!(l.notes[0].text, "From the 2025 budget, line 14.");
    assert_eq!(l.private.len(), 1);
    let q = t.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q[0]["range"], json!("Sheet1!B7"));
}

#[test]
fn audit_records_who_changed_what() {
    let mut s = model();
    s.execute("agent.decision", json!({"text": "Volume from the signed LOI", "range": "B3", "actor": "agent"})).unwrap();
    s.execute("agent.exec", json!({"command": "cell.set", "params": {"cell": "B4", "input": 72}, "reason": "supplier price list 2026", "source": "https://example.com/prices"})).unwrap();
    s.execute("cell.set", json!({"cell": "B5", "input": 90000})).unwrap();
    let l = layer::get(s.active().unwrap());
    let changes: Vec<_> = l.log.iter().filter(|e| e.kind == "change").collect();
    assert_eq!(changes.len(), 2, "{:?}", l.log);
    assert_eq!(changes[0].actor, crate::Actor::Agent);
    assert!(changes[0].text.contains("supplier price list"));
    assert_eq!(changes[0].source, "https://example.com/prices");
    assert_eq!(changes[1].actor, crate::Actor::Human);
}

#[test]
fn every_agent_command_survives_empty_params() {
    crate::install();
    test_keys();
    // Starting an agent process or editing Claude Desktop's settings is not for a fuzz test (their
    // own tests drive a fake agent and a temporary settings file).
    for spec in crate::commands::ALL
        .iter()
        .flat_map(|s| s.iter())
        .filter(|s| !matches!(s.id, "agent.agentStart" | "agent.agentReset" | "agent.connectCowork"))
    {
        let mut s = Session::new();
        let _ = s.execute(spec.id, json!({}));
        let _ = s.execute(spec.id, Value::Null);
        let mut s = model();
        let _ = s.execute(spec.id, json!({}));
        let _ = s.execute(
            spec.id,
            json!({"range": "ZZ!A1", "cell": "XFD1048576", "id": "q999", "text": "x", "low": 1e308, "high": -1e308, "depth": 99999}),
        );
    }
}

#[test]
fn mcp_tools_reach_the_commands() {
    struct Local(Session);
    impl gridcraft_mcp::Backend for Local {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "engine.execute" => self.0.run(params["command"].as_str().unwrap_or(""), params["params"].clone()),
                "engine.commands" => Ok(serde_json::to_value(self.0.commands()).unwrap()),
                _ => Err("unknown".into()),
            }
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }
    let mut s = model();
    s.execute("agent.private", json!({"range": "B3"})).unwrap();
    let id = s.execute("agent.ask", json!({"range": "B7", "text": "why?"})).unwrap()["id"].clone();
    let mut b = Local(s);
    let init = crate::mcp::handle(&mut b, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})).unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], json!("gridcraft-agent"));
    let r = crate::mcp::call(&mut b, "read_range", &json!({"range": "B3"})).unwrap();
    assert_ne!(r["cells"][0]["value"], json!(5000.0));
    let q = crate::mcp::call(&mut b, "wait_for_questions", &json!({"timeout_seconds": 0})).unwrap();
    assert_eq!(q[0]["id"], id);
    crate::mcp::call(&mut b, "answer_question", &json!({"id": id, "text": "because"})).unwrap();
    let cmds = crate::mcp::call(&mut b, "list_commands", &json!({"search": "bold"})).unwrap();
    assert!(cmds.as_array().unwrap().iter().all(|c| !c["id"].as_str().unwrap().starts_with("agent.")));
}

#[test]
fn shared_layer_keeps_answers_and_drops_open_questions() {
    let mut s = model();
    let open = s.execute("agent.ask", json!({"range": "B5", "text": "Where do fixed costs come from?"})).unwrap();
    let asked = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit negative?"})).unwrap();
    let id = asked["id"].as_str().unwrap();
    s.execute("agent.answer", json!({"id": id, "text": "Fixed costs exceed the margin."})).unwrap();
    s.execute("agent.seen", json!({"id": id})).unwrap();
    s.execute("agent.note", json!({"range": "B2", "text": "List price.", "kind": "source"})).unwrap();
    let l = crate::export::shared_layer(&layer::get(s.active().unwrap()));
    assert_eq!(l.questions.len(), 1);
    assert_eq!(l.questions[0].id, id);
    assert!(!l.questions[0].seen, "a prepared answer is new to the recipient");
    assert_eq!(l.notes.len(), 1);
    let open_anchor = &layer::get(s.active().unwrap()).questions.iter().find(|q| Some(q.id.as_str()) == open["id"].as_str()).unwrap().anchor.clone();
    assert!(l.anchor(open_anchor).is_none(), "the open question's anchor goes with it");
}

#[test]
fn export_html_embeds_the_workbook_and_the_app() {
    let mut s = model();
    let asked = s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    s.execute("agent.answer", json!({"id": asked["id"], "text": "Because."})).unwrap();
    s.execute("agent.ask", json!({"range": "B5", "text": "Still open"})).unwrap();
    let dir = std::env::temp_dir().join(format!("gc-agent-export-{}", std::process::id()));
    let web = dir.join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("app.js"), "export default async function () {}").unwrap();
    std::fs::write(web.join("app_bg.wasm"), b"\0asm").unwrap();
    let out = dir.join("book.html");
    let r = s.execute("agent.exportHtml", json!({"path": out.to_str().unwrap(), "web": web.to_str().unwrap()})).unwrap();
    assert!(r["bytes"].as_u64().unwrap() > 0);
    let html = std::fs::read_to_string(&out).unwrap();
    let start = html.find(&format!("id=\"{}\"", crate::export::WORKBOOK_ELEMENT)).unwrap();
    let body = &html[start..];
    let b64 = &body[body.find('>').unwrap() + 1..body.find("</script>").unwrap()];
    let bytes = gridcraft_engine::io::base64_decode(b64).unwrap();
    let mut r = Session::new();
    r.execute("file.open", json!({"name": "book.xlsx", "base64": gridcraft_engine::io::base64_encode(&bytes)})).unwrap();
    let l = layer::get(r.active().unwrap());
    assert_eq!(l.questions.len(), 1, "only the answered question travels");
    assert!(s.execute("agent.exportHtml", json!({"path": out.to_str().unwrap(), "web": dir.join("none").to_str().unwrap()})).is_err());
    assert!(s.execute("agent.exportHtml", json!({})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn variants_switch_and_compare() {
    let mut s = model();
    // A or B for the price: the present 120 is kept as "As built".
    let r = s
        .execute(
            "agent.variants",
            json!({"range": "B2", "title": "Price", "options": [{"name": "Low", "values": 100, "note": "competitor price"}, {"name": "High", "values": "140"}], "actor": "agent"}),
        )
        .unwrap();
    assert_eq!(r["options"], json!(["As built", "Low", "High"]));
    let id = r["id"].as_str().unwrap().to_string();
    let c = s.execute("agent.compareVariants", json!({"id": id, "masked": false})).unwrap();
    assert_eq!(c[0]["current"], json!("As built"));
    let profit = c[0]["outputs"].as_array().unwrap().iter().find(|o| o["cell"] == json!("Sheet1!B7")).unwrap().clone();
    assert_eq!(profit["values"], json!([150000.0, 50000.0, 250000.0]));
    // Comparing doesn't touch the workbook; switching does, as one undoable step.
    assert_eq!(num(&s, "B7"), 150000.0);
    s.execute("agent.useVariant", json!({"id": id, "option": "High"})).unwrap();
    assert_eq!(num(&s, "B7"), 250000.0);
    let c = s.execute("agent.compareVariants", json!({"id": id, "masked": false})).unwrap();
    assert_eq!(c[0]["current"], json!("High"));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(num(&s, "B7"), 150000.0);
    // Formulas are options too, on a range.
    let r = s.execute("agent.variants", json!({"range": "B6", "options": [{"name": "Net of 5% discount", "values": "=b2*b3*0.95"}]})).unwrap();
    let id2 = r["id"].as_str().unwrap().to_string();
    s.execute("agent.useVariant", json!({"id": id2, "option": 1})).unwrap();
    assert_eq!(num(&s, "B6"), 570000.0);
    s.execute("agent.useVariant", json!({"id": id2, "option": "As built"})).unwrap();
    assert_eq!(num(&s, "B6"), 600000.0);
    // The switch is in the history once, as a switch.
    let l = layer::get(s.active().unwrap());
    assert_eq!(l.log.iter().filter(|e| e.command == "agent.useVariant").count(), 3);
    assert!(l.log.iter().all(|e| e.command != "range.setValues"));
    // Shapes are checked; private ranges are refused.
    assert!(s.execute("agent.variants", json!({"range": "B2:B3", "options": [{"values": [[1, 2]]}]})).is_err());
    s.execute("agent.private", json!({"range": "B3"})).unwrap();
    assert!(s.execute("agent.variants", json!({"range": "B3", "options": [{"values": 1}]})).is_err());
}

#[test]
fn variants_travel_in_the_xlsx() {
    let mut s = model();
    s.execute("agent.variants", json!({"range": "B4", "options": [{"name": "Supplier B", "values": 65}]})).unwrap();
    s.execute("agent.method", json!({"cell": "B6", "exact": "=B2*B3*0.98"})).unwrap();
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    let c = t.execute("agent.compareVariants", json!({"masked": false})).unwrap();
    assert_eq!(c.as_array().unwrap().len(), 2);
    assert_eq!(c[0]["current"], json!("As built"));
    assert_eq!(c[1]["kind"], json!("method"));
}

#[test]
fn calculation_method_flags_material_differences() {
    let mut s = model();
    // Exact revenue nets a 0.5% payment fee: immaterial on revenue at a 1% threshold…
    let r = s.execute("agent.method", json!({"cell": "B6", "exact": "=B2*B3*(1-0.005)", "note": "card fees"})).unwrap();
    let id = r["id"].as_str().unwrap().to_string();
    let c = s.execute("agent.compareVariants", json!({"id": id, "masked": false})).unwrap();
    assert_eq!(c[0]["current"], json!("Simple"));
    let rows = c[0]["outputs"].as_array().unwrap();
    assert_eq!(rows[0]["cell"], json!("Sheet1!B6"));
    assert_eq!(rows[0]["values"], json!([600000.0, 597000.0]));
    assert_eq!(rows[0]["material"], json!(false));
    // …but profit is thin, so on profit it is 2%: material.
    let profit = rows.iter().find(|o| o["cell"] == json!("Sheet1!B7")).unwrap();
    assert_eq!(profit["values"], json!([150000.0, 147000.0]));
    assert_eq!(profit["material"], json!(true));
    assert_eq!(c[0]["material"], json!(true));
    // Switch to the exact one and back.
    s.execute("agent.useVariant", json!({"id": id, "option": "Exact"})).unwrap();
    assert_eq!(num(&s, "B7"), 147000.0);
    // A cell edited by hand matches neither.
    s.execute("cell.set", json!({"cell": "B6", "input": "=B2*B3*0.9"})).unwrap();
    let c = s.execute("agent.compareVariants", json!({"id": id, "masked": false})).unwrap();
    assert_eq!(c[0]["current"], Value::Null);
    assert!(s.execute("agent.method", json!({"cell": "B6", "exact": "=B2*B3*0.9"})).is_err());
}

/// Profit = price × volume × (1 − churn)² − fixed: the worst case needs every input at its bad
/// end at once, and churn interacts with price and volume.
fn stress_model() -> Session {
    let mut s = model();
    s.execute("cell.set", json!({"cell": "A8", "input": "Churn"})).unwrap();
    s.execute("cell.set", json!({"cell": "B8", "input": 0.1})).unwrap();
    s.execute("cell.set", json!({"cell": "B7", "input": "=B6*(1-B8)^2-B3*B4-B5"})).unwrap();
    for (cell, low, high) in [("B2", 100, 140), ("B3", 4000, 6000), ("B4", 60, 80)] {
        s.execute("agent.input", json!({"cell": cell, "low": low, "high": high, "actor": "agent"})).unwrap();
    }
    s.execute("agent.input", json!({"cell": "B8", "low": 0.05, "high": 0.2, "actor": "agent"})).unwrap();
    s
}

#[test]
fn stress_finds_the_worst_combination() {
    let mut s = stress_model();
    let base = num(&s, "B7");
    let r = s.execute("agent.stress", json!({"cell": "B7", "masked": false})).unwrap();
    assert_eq!(r["inputs"].as_array().unwrap().len(), 4);
    assert_eq!(r["allCorners"], json!(true));
    // Brute force over a grid agrees with the search.
    let f = |p: f64, v: f64, c: f64, ch: f64| p * v * (1.0 - ch).powi(2) - v * c - 100000.0;
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for p in [100.0, 140.0] {
        for v in [4000.0, 6000.0] {
            for c in [60.0, 80.0] {
                for ch in [0.05, 0.2] {
                    lo = lo.min(f(p, v, c, ch));
                    hi = hi.max(f(p, v, c, ch));
                }
            }
        }
    }
    let lowest = r["lowest"]["value"].as_f64().unwrap();
    let highest = r["highest"]["value"].as_f64().unwrap();
    assert!((lowest - lo).abs() < 1e-6, "{lowest} vs {lo}");
    assert!((highest - hi).abs() < 1e-6, "{highest} vs {hi}");
    // The inputs interact: with price low and churn high each unit loses money, so the worst
    // case has volume at its *high* end, though alone a lower volume hurts; and the single moves
    // added up miss the real worst case.
    let at = |c: &str| r["lowest"]["inputs"].as_array().unwrap().iter().find(|i| i["cell"] == json!(c)).unwrap()["at"].clone();
    assert_eq!(at("Sheet1!B2"), json!("low"));
    assert_eq!(at("Sheet1!B8"), json!("high"));
    assert_eq!(at("Sheet1!B3"), json!("high"));
    assert!((lowest - r["oneAtATime"]["lowest"].as_f64().unwrap()).abs() > 1.0);
    // Deterministic, capped, and the real model is untouched.
    let again = s.execute("agent.stress", json!({"cell": "B7", "masked": false})).unwrap();
    assert_eq!(r, again);
    assert_eq!(num(&s, "B7"), base);
    let small = s.execute("agent.stress", json!({"cell": "B7", "masked": false, "maxEvals": 5})).unwrap();
    assert!(small["evaluations"].as_u64().unwrap() <= 10);
}

#[test]
fn stress_finds_an_interior_peak() {
    // Revenue = price × demand, with demand falling linearly in price: the highest value is inside
    // the price range, not at either end.
    let mut s = model();
    s.execute("cell.set", json!({"cell": "B3", "input": "=10000-50*B2"})).unwrap();
    s.execute("agent.input", json!({"cell": "B2", "low": 50, "high": 150})).unwrap();
    let r = s.execute("agent.stress", json!({"cell": "B6", "masked": false, "inputs": ["B2"]})).unwrap();
    assert_eq!(r["highest"]["value"], json!(500000.0));
    assert_eq!(r["highest"]["inputs"][0]["at"], json!("inside"));
}

#[test]
fn risk_matrix_ranks_impact_against_confidence() {
    let mut s = stress_model();
    s.execute("agent.confidence", json!({"range": "B2", "level": 0.95, "reason": "price list"})).unwrap();
    s.execute("agent.confidence", json!({"range": "B8", "level": 0.2, "reason": "one quarter of data"})).unwrap();
    let r = s.execute("agent.risk", json!({"cell": "B7", "masked": false})).unwrap();
    let rows = r["inputs"].as_array().unwrap();
    let get = |c: &str| rows.iter().find(|x| x["cell"] == json!(c)).unwrap().clone();
    assert_eq!(get("Sheet1!B2")["confidence"], json!(0.95));
    assert_eq!(get("Sheet1!B3")["confidence"], Value::Null);
    // Ranked by impact × (1 − confidence): a confident input sinks.
    let scores: Vec<f64> = rows.iter().map(|x| x["score"].as_f64().unwrap()).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]));
    assert_eq!(rows.last().unwrap()["cell"], json!("Sheet1!B2"));
    assert!(rows.iter().any(|x| x["impact"] == json!(1.0)));
}

/// A key folder of this test's own (keys never go near the workbook).
fn key_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("gridcraft-agent-keys-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    crate::safe::set_thread_key_dir(Some(d.clone()));
    d
}

fn xlsx_text(b64: &str) -> String {
    // The parts are deflated: look at them through the reader instead of the raw bytes.
    let bytes = gridcraft_engine::io::base64_decode(b64).unwrap();
    let (wb, _) = gridcraft_engine::io::open_bytes("x.xlsx", &bytes).unwrap();
    serde_json::to_string(&wb).unwrap()
}

#[test]
fn safe_file_keeps_client_data_out_of_the_file() {
    let mut s = model();
    let keys = key_dir("safe");
    s.execute("agent.private", json!({"range": "B1:B3"})).unwrap();
    let on = s.execute("agent.safeMode", json!({})).unwrap();
    let key_id = on["key"].as_str().unwrap().to_string();
    assert!(keys.join(format!("{key_id}.key")).exists());
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    // The document keeps its real values; the file has stand-ins, derived values included.
    assert_eq!(num(&s, "B2"), 120.0);
    assert_eq!(num(&s, "B6"), 600000.0);
    let text = xlsx_text(&b64);
    assert!(!text.contains("Acme Holding"), "client name in the file");
    assert!(text.contains("{{private:B1}}"));
    assert!(!text.contains("600000"), "derived revenue in the file");
    // Opened here (the key is here): the real values are back, and nothing is dirty.
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    assert_eq!(num(&t, "B2"), 120.0);
    assert_eq!(num(&t, "B6"), 600000.0);
    assert!(!t.active().unwrap().is_dirty());
    let l = layer::get(t.active().unwrap());
    assert!(l.safe.as_ref().unwrap().sealed.is_none());
    // Elsewhere (no key): stand-ins, locked, nothing to export; with the key back, unlocked.
    crate::safe::set_thread_key_dir(Some(std::env::temp_dir().join(format!("gridcraft-agent-no-keys-{}", std::process::id()))));
    let mut u = Session::new();
    u.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    assert_ne!(num(&u, "B2"), 120.0);
    assert!(layer::get(u.active().unwrap()).safe.as_ref().unwrap().sealed.is_some());
    assert!(u.execute("agent.exportExcel", json!({"path": "/tmp/never.xlsx"})).is_err());
    assert!(u.execute("agent.safeMode", json!({"on": false})).is_err());
    assert!(u.execute("agent.unlock", json!({})).is_err());
    crate::safe::set_thread_key_dir(Some(keys.clone()));
    assert_eq!(u.execute("agent.unlock", json!({})).unwrap()["cells"], json!(3));
    assert_eq!(num(&u, "B6"), 600000.0);
    // Only a person turns it off.
    assert!(s.execute("agent.safeMode", json!({"on": false, "actor": "agent"})).is_err());
    let _ = std::fs::remove_dir_all(&keys);
}

#[test]
fn safe_save_to_disk_and_export_for_excel() {
    let mut s = model();
    let keys = key_dir("disk");
    let dir = std::env::temp_dir().join(format!("gridcraft-agent-book-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("model.xlsx");
    s.execute("agent.private", json!({"range": "B2"})).unwrap();
    s.execute("agent.safeMode", json!({})).unwrap();
    s.execute("file.saveAs", json!({"path": path.to_str().unwrap()})).unwrap();
    let d = s.active().unwrap();
    assert!(!d.is_dirty());
    assert_eq!(num(&s, "B2"), 120.0);
    // Saving again after an edit still writes a safe file.
    s.execute("cell.set", json!({"cell": "B4", "input": 71})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    assert!(!s.active().unwrap().is_dirty());
    assert_eq!(num(&s, "B2"), 120.0);
    // On disk: the stand-in, as anyone without the key sees it.
    crate::safe::set_thread_key_dir(Some(std::env::temp_dir().join(format!("gridcraft-agent-no-keys-{}", std::process::id()))));
    let mut v = Session::new();
    v.execute("file.open", json!({"path": path.to_str().unwrap()})).unwrap();
    assert_ne!(num(&v, "B2"), 120.0);
    assert_eq!(num(&v, "B4"), 71.0);
    crate::safe::set_thread_key_dir(Some(keys.clone()));
    // The export for standard Excel has the real values and nothing sealed.
    let r = s.execute("agent.exportExcel", json!({})).unwrap();
    let out = r["path"].as_str().unwrap().to_string();
    assert!(out.ends_with("model (Excel).xlsx"));
    crate::safe::set_thread_key_dir(None);
    let mut t = Session::new();
    t.execute("file.open", json!({"path": out})).unwrap();
    assert_eq!(num(&t, "B2"), 120.0);
    assert!(layer::get(t.active().unwrap()).safe.is_none());
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&keys);
}

#[test]
fn sealed_data_refuses_a_wrong_key_or_a_change() {
    let k1 = [7u8; 32];
    let k2 = [8u8; 32];
    let mut sealed = crate::safe::seal_bytes(&k1, [1u8; 12], b"[{\"n\":120}]", 1).unwrap();
    assert_eq!(crate::safe::open_bytes(&k1, &sealed).unwrap(), b"[{\"n\":120}]");
    assert!(crate::safe::open_bytes(&k2, &sealed).is_err());
    let mut data = gridcraft_engine::io::base64_decode(&sealed.data).unwrap();
    data[0] ^= 1;
    sealed.data = gridcraft_engine::io::base64_encode(&data);
    assert!(crate::safe::open_bytes(&k1, &sealed).is_err());
}

/// The model saved to a file of its own (a linked source must have one).
fn saved_model(name: &str) -> (Session, std::path::PathBuf) {
    let mut s = model();
    let dir = std::env::temp_dir().join(format!("gridcraft-links-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("q3.xlsx");
    s.execute("file.saveAs", json!({"path": path.to_str().unwrap()})).unwrap();
    (s, path)
}

/// The payload a reader finds in the clipboard HTML (first `data-artcraft-link=`, base64).
fn payload_of(html: &str) -> Value {
    let start = html.find("data-artcraft-link=\"").unwrap() + "data-artcraft-link=\"".len();
    let end = start + html[start..].find('"').unwrap();
    let bytes = gridcraft_engine::io::base64_decode(&html[start..end]).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn link_copy_puts_a_linked_table_on_the_clipboard() {
    let (mut s, path) = saved_model("copy");
    s.execute("agent.private", json!({"range": "B3"})).unwrap();
    let r = s.execute("link.copy", json!({"range": "A2:B4"})).unwrap();
    let html = r["html"].as_str().unwrap();
    assert!(html.starts_with("<table data-artcraft-link=\""));
    assert!(html.contains("<td>Price</td><td>120</td>"));
    assert_eq!(r["text"], json!("Price\t120\nVolume\t5000\nUnit cost\t70"));
    let p = payload_of(html);
    assert_eq!(p["kind"], json!("artcraft.link"));
    assert_eq!(p["version"], json!(1));
    assert_eq!(p["source"]["app"], json!("GridCraft"));
    assert_eq!(p["source"]["path"], json!(std::path::absolute(&path).unwrap().display().to_string()));
    assert_eq!(p["source"]["sheet"], json!("Sheet1"));
    assert_eq!(p["source"]["range"], json!("A2:B4"));
    assert_eq!(p["values"], json!([["Price", 120.0], ["Volume", 5000.0], ["Unit cost", 70.0]]));
    assert_eq!(p["private"], json!(true));
    let anchor = p["source"]["anchor"].as_str().unwrap().to_string();
    assert!(anchor.starts_with("_GcLink_"));
    let id = p["source"]["workbookId"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 36);
    // The same range again: the same name; the name is hidden and survives the agent's clean-up.
    let again = s.execute("link.copy", json!({"range": "A2:B4"})).unwrap();
    assert_eq!(again["anchor"], json!(anchor));
    s.execute("agent.ask", json!({"range": "B7", "text": "?"})).unwrap();
    s.execute("cell.set", json!({"cell": "B5", "input": 90000})).unwrap();
    let names = &s.active().unwrap().wb.names;
    assert!(names.iter().any(|n| n.name == anchor && n.hidden));
    // The id and the link travel in the file.
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "q3.xlsx", "base64": b64})).unwrap();
    assert_eq!(layer::get(t.active().unwrap()).workbook_id, id);
    // Refused: an unsaved workbook, a range a slide can't hold.
    let mut u = model();
    assert!(u.execute("link.copy", json!({"range": "A1:B2"})).is_err());
    assert!(s.execute("link.copy", json!({"range": "A1:BZ10"})).is_err());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn link_read_follows_rows_and_unsaved_edits() {
    let (mut s, path) = saved_model("read");
    let p = payload_of(s.execute("link.copy", json!({"range": "B6:B7"})).unwrap()["html"].as_str().unwrap());
    let src = p["source"].clone();
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    s.execute("cell.set", json!({"cell": "B3", "input": 130})).unwrap();
    let r = s.execute("link.read", src.clone()).unwrap();
    assert_eq!(r["range"], json!("B7:B8"));
    assert_eq!(r["values"], json!([[650000.0], [200000.0]]));
    assert_eq!(r["text"][0][0], json!("650000"));
    // By path when the id doesn't match; an error when the workbook isn't open here.
    let mut by_path = src.clone();
    by_path["workbookId"] = json!("other");
    assert!(s.execute("link.read", by_path).is_ok());
    let mut elsewhere = src.clone();
    elsewhere["workbookId"] = json!("other");
    elsewhere["path"] = json!("/nowhere/q3.xlsx");
    assert!(s.execute("link.read", elsewhere).is_err());
    // An undo past the copy keeps the name.
    for _ in 0..10 {
        let _ = s.execute("edit.undo", json!({}));
    }
    assert!(s.execute("link.read", src).is_ok());
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn link_uses_and_reveal() {
    let (mut s, path) = saved_model("uses");
    let p = payload_of(s.execute("link.copy", json!({"range": "B6"})).unwrap()["html"].as_str().unwrap());
    let src = p["source"].clone();
    let deck = path.parent().unwrap().join("board.pptx");
    std::fs::write(&deck, b"deck").unwrap();
    let use_line = json!({"event": "use", "at": 1, "app": "DeckCraft", "link": "l18f", "workbook": src["path"], "workbookId": src["workbookId"],
        "anchor": src["anchor"], "sheet": "Sheet1", "range": "B6", "deck": deck.to_str().unwrap(), "deckTitle": "board", "slide": 3,
        "slideTitle": "Revenue", "shape": 412, "shapeKey": "k4f2a9", "mode": "table"});
    assert_eq!(s.execute("link.addUses", json!({"uses": [use_line.clone()]})).unwrap()["added"], json!(1));
    let uses = s.execute("link.uses", json!({"range": "B6"})).unwrap();
    assert_eq!(uses[0]["use"]["slide"], json!(3));
    assert_eq!(uses[0]["range"], json!("Sheet1!B6"));
    assert!(s.execute("link.uses", json!({"range": "B2"})).unwrap().as_array().unwrap().is_empty());
    // The registry file brings uses in when the workbook opens (latest line per link, decks that exist).
    s.execute("file.save", json!({})).unwrap();
    let registry = path.parent().unwrap().join("links.jsonl");
    let mut later = use_line.clone();
    later["at"] = json!(2);
    later["slide"] = json!(5);
    let mut gone = use_line.clone();
    gone["link"] = json!("l2");
    gone["deck"] = json!("/nowhere/old.pptx");
    std::fs::write(&registry, format!("{use_line}\n{later}\n{gone}\nnot json\n")).unwrap();
    crate::links::set_thread_registry(Some(registry));
    let mut t = Session::new();
    t.new_workbook();
    t.execute("file.open", json!({"path": path.to_str().unwrap()})).unwrap();
    let l = layer::get(t.active().unwrap());
    assert_eq!(l.uses.len(), 1);
    assert_eq!(l.uses[0].slide, 5);
    // Reveal: the right workbook, the linked cell, from another document.
    t.set_active(0);
    let r = t.execute("link.reveal", src).unwrap();
    assert_eq!(r["range"], json!("B6"));
    assert_eq!(t.active().unwrap().selection.current().a1(), "B6");
    assert!(crate::links::take_revealed());
    crate::links::set_thread_registry(None);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// Waits (up to 5 s) for a condition another process brings about.
fn wait_for(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[cfg(unix)]
#[test]
fn background_agent_gets_questions_and_resumes_its_session() {
    use crate::runner::{self, Config, Mode};
    use std::os::unix::fs::PermissionsExt;
    let (mut s, path) = saved_model("runner");
    let dir = path.parent().unwrap().to_path_buf();
    let rec = dir.join("rec");
    std::fs::create_dir_all(&rec).unwrap();
    // A fake agent CLI: records how it was started and what it was told; one result per message.
    let script = dir.join("fake-claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{r}/args.txt'\npwd -P > '{r}/cwd.txt'\nwhile IFS= read -r line; do printf '%s\\n' \"$line\" >> '{r}/stdin.txt'; echo '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"total_cost_usd\":0.02}}'; done\n",
            r = rec.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    runner::set_config(Config { command: script.display().to_string(), data_dir: Some(dir.join("data")), ..Config::default() });
    runner::set_mode(Mode::Background);
    let read = |f: &str| std::fs::read_to_string(rec.join(f)).unwrap_or_default();
    // A question starts the agent in the workbook's folder and is sent to it once.
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit 150k?"})).unwrap()["id"].as_str().unwrap().to_string();
    runner::tick(&mut s);
    runner::tick(&mut s);
    assert!(wait_for(|| read("stdin.txt").lines().count() >= 2), "stdin: {}", read("stdin.txt"));
    let lines: Vec<Value> = read("stdin.txt").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["type"], json!("user"));
    assert!(lines[0]["message"]["content"].as_str().unwrap().starts_with("/rename GridCraft · q3.xlsx"));
    let text = lines[1]["message"]["content"].as_str().unwrap();
    assert!(text.contains("Why is profit 150k?") && text.contains(&q) && text.contains("Sheet1!B7"), "{text}");
    assert_eq!(lines[1]["parent_tool_use_id"], Value::Null);
    let args = read("args.txt");
    let a: Vec<&str> = args.lines().collect();
    let session = layer::get(s.active().unwrap()).agent_session.clone();
    assert_eq!(session.len(), 36);
    for want in [
        "-p",
        "--input-format",
        "stream-json",
        "--verbose",
        "--strict-mcp-config",
        "mcp__gridcraft__*",
        "--append-system-prompt-file",
        "--max-turns",
        "--max-budget-usd",
    ] {
        assert!(a.contains(&want), "{want} missing in {a:?}");
    }
    assert!(!a.contains(&"--bare"));
    let at = |flag: &str| a.iter().position(|x| *x == flag).and_then(|i| a.get(i + 1)).copied().unwrap_or("");
    assert_eq!(at("--session-id"), session);
    let mcp: Value = serde_json::from_str(&std::fs::read_to_string(at("--mcp-config")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(mcp["mcpServers"]["gridcraft"]["type"], json!("http"));
    assert_eq!(read("cwd.txt").trim(), std::fs::canonicalize(&dir).unwrap().display().to_string());
    // Both messages were answered; the same question isn't sent twice.
    assert!(wait_for(|| runner::status(s.active().unwrap().uid).is_some_and(|x| x.turns == 2 && x.in_flight == 0)));
    runner::tick(&mut s);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(read("stdin.txt").lines().count(), 2);
    // Stopped and started again: the same session, resumed.
    let st = s.execute("agent.agentStop", json!({})).unwrap();
    assert_eq!(st["running"], json!(false));
    assert!(s.execute("agent.agentMode", json!({"mode": "off", "actor": "agent"})).is_err());
    std::fs::remove_file(rec.join("args.txt")).unwrap();
    s.execute("agent.agentStart", json!({})).unwrap();
    assert!(wait_for(|| read("args.txt").contains("--resume")));
    assert!(read("args.txt").lines().any(|l| l == session));
    // A new session starts from the document's history.
    std::fs::remove_file(rec.join("stdin.txt")).unwrap();
    s.execute("agent.agentReset", json!({})).unwrap();
    assert!(wait_for(|| read("stdin.txt").contains("fresh session")));
    assert_ne!(layer::get(s.active().unwrap()).agent_session, session);
    runner::set_mode(Mode::Off);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn read_only_layer_refuses_changes() {
    let mut s = model();
    let asked = s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    s.execute("agent.answer", json!({"id": asked["id"], "text": "Because."})).unwrap();
    crate::set_read_only(true);
    assert!(s.execute("agent.ask", json!({"range": "B7", "text": "Again?"})).is_err());
    assert!(s.execute("agent.private", json!({"range": "B3"})).is_err());
    assert!(s.execute("agent.note", json!({"range": "B2", "text": "x"})).is_err());
    assert!(s.execute("agent.exec", json!({"command": "cell.set", "params": {"cell": "B2", "input": 1}})).is_err());
    assert!(s.execute("agent.seen", json!({"id": asked["id"]})).is_ok(), "reading an answer is fine");
    crate::set_read_only(false);
    let v = s.execute("agent.variants", json!({"range": "B2", "options": [{"name": "Low", "values": 100}]})).unwrap();
    crate::set_read_only(true);
    s.execute("agent.useVariant", json!({"id": v["id"], "option": "Low"})).unwrap();
    assert_eq!(num(&s, "B2"), 100.0, "the reader can switch the author's alternatives");
    assert!(s.execute("agent.layer", json!({})).is_ok());
    crate::set_read_only(false);
    assert!(s.execute("agent.ask", json!({"range": "B7", "text": "Again?"})).is_ok());
}

#[test]
fn questions_become_channel_events() {
    let n = crate::channel::notification(&json!({"id": "q3", "range": "Sheet1!B7", "label": "Profit", "text": "Why negative?"}));
    assert_eq!(n["method"], json!("notifications/claude/channel"));
    assert_eq!(n["params"]["content"], json!("Question on Sheet1!B7 (Profit): Why negative?"));
    assert_eq!(n["params"]["meta"]["question_id"], json!("q3"));
    assert!(n["params"]["meta"].as_object().unwrap().keys().all(|k| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')));
    struct Nothing;
    impl gridcraft_mcp::Backend for Nothing {
        fn call(&mut self, _: &str, _: Value) -> Result<Value, String> {
            Err("no".into())
        }
        fn has_ui(&self) -> bool {
            false
        }
        fn describe(&self) -> String {
            "test".into()
        }
    }
    let init = crate::mcp::handle(&mut Nothing, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})).unwrap();
    assert_eq!(init["result"]["capabilities"]["experimental"]["claude/channel"], json!({}));
}

#[test]
fn stdio_server_answers_without_the_app() {
    // Nothing listens on port 1, as when GridCraft isn't open.
    let mut b = crate::stdio::Lazy::new("127.0.0.1:1");
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "not json",
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_context","arguments":{}}}"#,
    ]
    .join("\n");
    let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let w: std::sync::Arc<std::sync::Mutex<dyn std::io::Write + Send>> = out.clone();
    crate::stdio::serve(&mut b, input.as_bytes(), w).unwrap();
    let text = String::from_utf8(out.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 4, "the notification gets no reply");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], json!("gridcraft-agent"));
    assert!(lines[1]["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == json!("wait_for_questions")));
    assert_eq!(lines[2]["error"]["code"], json!(-32700));
    assert_eq!(lines[3]["result"]["isError"], json!(true));
    let msg = lines[3]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(msg.contains("not reachable") && msg.contains("Is GridCraft open?"), "{msg}");
}

#[test]
fn connect_cowork_adds_one_entry_and_keeps_the_rest() {
    use crate::cowork::{self, Configured};
    use std::path::Path;
    let dir = std::env::temp_dir().join(format!("gridcraft-cowork-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("claude_desktop_config.json");
    let before = json!({"globalShortcut": "Alt+Space", "mcpServers": {"wordcraft": {"command": "/x/wordcraft", "env": {"TOKEN": "secret"}}}, "preferences": {"theme": "dark"}});
    std::fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();
    let want = cowork::entry(Path::new("/Applications/GridCraft.app/Contents/MacOS/gridcraft"), 7979);
    assert_eq!(want["args"], json!(["--agent-stdio", "--control", "127.0.0.1:7979"]));
    assert_eq!(cowork::configured(&path, &want), Configured::Missing);
    let backup = cowork::write(&path, &want).unwrap().unwrap();
    assert!(backup.display().to_string().contains(".bak-gridcraft-"));
    assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(backup).unwrap()).unwrap(), before);
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after["mcpServers"]["gridcraft"], want);
    assert_eq!(after["mcpServers"]["wordcraft"], before["mcpServers"]["wordcraft"]);
    assert_eq!(after["preferences"], before["preferences"]);
    assert_eq!(after["globalShortcut"], before["globalShortcut"]);
    assert_eq!(cowork::configured(&path, &want), Configured::Yes);
    assert_eq!(cowork::configured(&path, &cowork::entry(Path::new("/elsewhere"), 7979)), Configured::Different);
    // A broken file is left alone; a missing one is made.
    std::fs::write(&path, "{ not json").unwrap();
    assert!(cowork::write(&path, &want).is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ not json");
    let fresh = dir.join("new/claude_desktop_config.json");
    assert_eq!(cowork::configured(&fresh, &want), Configured::NoClaude);
    assert_eq!(cowork::write(&fresh, &want).unwrap(), None);
    assert_eq!(cowork::configured(&fresh, &want), Configured::Yes);
    // Tests never reach the real file, and the agent can't connect anything.
    assert!(cowork::config_path().is_none());
    let mut s = model();
    assert!(s.execute("agent.connectCowork", json!({"actor": "agent"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
