#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gridcraft_engine::Session;
use serde_json::{Value, json};

use crate::layer;

/// A small model: price × volume − costs, with a client name and a private volume.
fn model() -> Session {
    crate::install();
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
    for spec in crate::commands::ALL.iter().flat_map(|s| s.iter()) {
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
