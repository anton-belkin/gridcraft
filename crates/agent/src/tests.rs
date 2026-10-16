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
    // The links go inside the text, as a sentence.
    let text = all[0]["answer"]["text"].as_str().unwrap();
    assert!(text.contains("[Fixed costs](cell:Sheet1!B5)"), "{text}");
}

#[test]
fn anchors_follow_inserted_rows_and_survive_undo() {
    let mut s = model();
    s.execute("agent.ask", json!({"range": "B7", "text": "?"})).unwrap();
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    let q = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q[0]["range"], json!("Sheet1!B8"), "{q}");
    // Undoing the insert moves the anchor back; undoing the question takes it back (it was the
    // person's action, in the same history); redo asks it again.
    s.execute("edit.undo", json!({})).unwrap();
    let q = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(q[0]["range"], json!("Sheet1!B7"));
    let r = s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(r["undone"], json!("Ask the Agent"));
    assert!(s.execute("agent.questions", json!({})).unwrap().as_array().unwrap().is_empty());
    for _ in 0..20 {
        let _ = s.execute("edit.undo", json!({}));
    }
    s.execute("edit.redo", json!({"steps": 20})).unwrap();
    let q = s.execute("agent.questions", json!({"status": "pending"})).unwrap();
    assert_eq!(q.as_array().unwrap().len(), 1);
    assert_eq!(q[0]["range"], json!("Sheet1!B8"));
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
fn sending_drops_the_discussion_unless_included() {
    let mut s = model();
    s.execute("agent.ask", json!({"range": "B5", "text": "Where do fixed costs come from?"})).unwrap();
    let asked = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit negative?"})).unwrap();
    let id = asked["id"].as_str().unwrap();
    s.execute("agent.answer", json!({"id": id, "text": "Fixed costs exceed the margin."})).unwrap();
    s.execute("agent.seen", json!({"id": id})).unwrap();
    s.execute("agent.setStatus", json!({"topic": asked["topic"], "status": "revise", "text": "cut fixed costs in B5"})).unwrap();
    s.execute("agent.note", json!({"range": "B2", "text": "List price.", "kind": "source"})).unwrap();
    // Sent as is: topics with places, status and notes; no discussion.
    let l = crate::export::shared_layer(&layer::get(s.active().unwrap()), false);
    assert!(l.questions.is_empty());
    assert_eq!(l.topics.len(), 3);
    assert_eq!(l.notes.len(), 1);
    let t = l.topic(asked["topic"].as_str().unwrap()).unwrap();
    assert_eq!(t.status.status, layer::Status::Revise);
    assert!(l.anchor(&t.places[0].anchor).is_some(), "places travel");
    // With the discussion: the answered question, shown as new; the open one stays behind.
    let l = crate::export::shared_layer(&layer::get(s.active().unwrap()), true);
    assert_eq!(l.questions.len(), 1);
    assert_eq!(l.questions[0].id, id);
    assert!(!l.questions[0].seen, "a prepared answer is new to the recipient");
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
    assert!(l.questions.is_empty(), "the discussion stays behind");
    assert_eq!(l.topics.len(), 2, "the topics travel");
    s.execute("agent.exportHtml", json!({"path": out.to_str().unwrap(), "web": web.to_str().unwrap(), "includeDiscussion": true})).unwrap();
    let html = std::fs::read_to_string(&out).unwrap();
    let start = html.find(&format!("id=\"{}\"", crate::export::WORKBOOK_ELEMENT)).unwrap();
    let body = &html[start..];
    let b64 = &body[body.find('>').unwrap() + 1..body.find("</script>").unwrap()];
    let mut r = Session::new();
    r.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    assert_eq!(layer::get(r.active().unwrap()).questions.len(), 1, "with the discussion: the answered question");
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
    let _g = CONTEXT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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
    // The neutral folder lives beside the registry's.
    crate::registry::set_folder(Some(dir.join("ArtCraft/agents")));
    runner::set_mode(Mode::Background);
    let read = |f: &str| std::fs::read_to_string(rec.join(f)).unwrap_or_default();
    // A question starts the agent in the workbook's folder and is sent to it once.
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit 150k?"})).unwrap()["id"].as_str().unwrap().to_string();
    runner::tick(&mut s);
    runner::tick(&mut s);
    assert!(wait_for(|| !read("stdin.txt").is_empty()), "stdin: {}", read("stdin.txt"));
    std::thread::sleep(std::time::Duration::from_millis(200));
    // No history yet: no seed, just the question.
    let lines: Vec<Value> = read("stdin.txt").lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], json!("user"));
    let text = lines[0]["message"]["content"].as_str().unwrap();
    assert!(text.contains("Why is profit 150k?") && text.contains(&q) && text.contains("Sheet1!B7"), "{text}");
    assert_eq!(lines[0]["parent_tool_use_id"], Value::Null);
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
    assert_eq!(at("--name"), "GridCraft · q3.xlsx");
    let mcp: Value = serde_json::from_str(&std::fs::read_to_string(at("--mcp-config")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(mcp["mcpServers"]["gridcraft"]["type"], json!("http"));
    // A neutral folder by default: no project's CLAUDE.md or memory joins its context.
    assert_eq!(read("cwd.txt").trim(), std::fs::canonicalize(dir.join("ArtCraft/agent-sessions")).unwrap().display().to_string());
    // The question tells it which document it's from.
    assert!(text.contains(&crate::docs::doc_id(s.active().unwrap().uid)), "{text}");
    // Answered; the same question isn't sent twice.
    assert!(wait_for(|| runner::status(s.active().unwrap().uid).is_some_and(|x| x.turns == 1 && x.in_flight == 0)));
    runner::tick(&mut s);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(read("stdin.txt").lines().count(), 1);
    // Stopped and started again: the same session, resumed.
    let st = s.execute("agent.agentStop", json!({})).unwrap();
    assert_eq!(st["running"], json!(false));
    assert!(s.execute("agent.agentMode", json!({"mode": "off", "actor": "agent"})).is_err());
    std::fs::remove_file(rec.join("args.txt")).unwrap();
    // The document's folder as its project, when asked.
    s.execute("agent.agentMode", json!({"projectFolder": true})).unwrap();
    s.execute("agent.agentStart", json!({})).unwrap();
    assert!(wait_for(|| read("args.txt").contains("--resume")));
    assert!(read("args.txt").lines().any(|l| l == session));
    assert!(wait_for(|| read("cwd.txt").trim() == std::fs::canonicalize(&dir).unwrap().display().to_string()));
    s.execute("agent.agentMode", json!({"projectFolder": false})).unwrap();
    // A new session starts from the document's history (there is some now).
    s.execute("agent.decision", json!({"text": "Churn from the 2026 cohort analysis"})).unwrap();
    std::fs::remove_file(rec.join("stdin.txt")).unwrap();
    s.execute("agent.agentReset", json!({})).unwrap();
    assert!(wait_for(|| read("stdin.txt").contains("fresh session")));
    assert_ne!(layer::get(s.active().unwrap()).agent_session, session);
    runner::set_mode(Mode::Manual);
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

/// A spoken question: on the cells at once, with the agent only when its text is ready, the
/// cells pointed at expanded (and masked) for the agent, never saved while it is a draft.
#[test]
fn spoken_question_with_pointing() {
    let mut s = model();
    s.execute("agent.private", json!({"range": "B3"})).unwrap();
    let q = s
        .execute("agent.ask", json!({"range": "B7", "transcribing": true, "refs": [{"n": 1, "range": "B6"}, {"n": 2, "range": "Sheet1!B3"}]}))
        .unwrap();
    let id = q["id"].as_str().unwrap().to_string();
    // Not the agent's yet: no text.
    assert!(s.execute("agent.questions", json!({"status": "pending"})).unwrap().as_array().unwrap().is_empty());
    assert!(s.execute("agent.questions", json!({})).unwrap().as_array().unwrap().is_empty());
    assert_eq!(s.execute("agent.context", json!({})).unwrap()["openQuestions"], json!(0));
    // A draft is never written into the file.
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    assert!(layer::get(t.active().unwrap()).questions.is_empty());
    // A row inserted while transcribing moves the pointed cells too.
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    // Failed, then retried, then done.
    s.execute("agent.spoken", json!({"id": id, "error": "no speech"})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).questions[0].status, layer::QuestionStatus::Failed);
    s.execute("agent.spoken", json!({"id": id})).unwrap();
    s.execute("agent.spoken", json!({"id": id, "text": "Why is ⟦1⟧ so much higher than ⟦2⟧ times the price?"})).unwrap();
    let pending = s.execute("agent.questions", json!({"status": "pending"})).unwrap();
    let p = &pending[0];
    assert_eq!(p["range"], json!("Sheet1!B8"));
    // Where they pointed became links (and places of the topic), following the moved rows.
    let text = p["text"].as_str().unwrap();
    assert_eq!(text, "Why is [B7](cell:Sheet1!B7) so much higher than [B4](cell:Sheet1!B4) times the price?");
    // The volume is client data: the agent sees a stand-in, never 5000.
    assert!(!text.contains("5000") && !text.contains("5,000"), "{text}");
    assert_eq!(p["refs"][1]["range"], json!("Sheet1!B4"));
    assert_eq!(p["refs"][0]["label"], json!("Revenue"));
    // With the agent now: no longer a draft.
    assert!(s.execute("agent.spoken", json!({"id": id, "text": "again"})).is_err());
    assert!(s.execute("agent.removeQuestion", json!({"id": id})).is_err());
    // The events stream, the channel and the background agent all read this text.
    let note = crate::channel::notification(p);
    assert!(note["params"]["content"].as_str().unwrap().contains("[B7](cell:Sheet1!B7)"), "{note}");
    // Saved now, refs and all.
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    let l = layer::get(t.active().unwrap());
    assert_eq!(l.questions[0].refs.len(), 2);
    assert!(l.questions[0].spoken);
}

#[test]
fn a_failed_spoken_question_can_be_dismissed() {
    let mut s = model();
    let id = s.execute("agent.ask", json!({"range": "B7", "transcribing": true, "refs": [{"n": 1, "range": "B5"}]})).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    s.execute("agent.spoken", json!({"id": id, "error": ""})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).questions[0].error, "The transcription failed");
    s.execute("agent.removeQuestion", json!({"id": id})).unwrap();
    let l = layer::get(s.active().unwrap());
    assert!(l.questions.is_empty());
    // Its anchors went with it.
    assert!(l.anchors.is_empty(), "{:?}", l.anchors);
    // Bad refs are errors, not panics.
    assert!(s.execute("agent.ask", json!({"text": "x", "refs": [{"range": "B5"}]})).is_err());
    assert!(s.execute("agent.ask", json!({"text": "x", "refs": [{"n": 1, "range": "not a cell!"}]})).is_err());
    assert!(s.execute("agent.ask", json!({"text": "", "refs": []})).is_err());
}

#[test]
fn ping_checks_the_connection_and_stays_out_of_the_file() {
    let mut s = model();
    s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    let log_before = layer::get(s.active().unwrap()).log.len();
    let id = s.execute("agent.ping", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    // It goes out like any question, marked as a ping.
    let pending = s.execute("agent.questions", json!({"status": "pending", "actor": "agent"})).unwrap();
    let ping = pending.as_array().unwrap().iter().find(|q| q["id"] == json!(id)).unwrap().clone();
    assert_eq!(ping["kind"], json!("ping"));
    assert!(ping["text"].as_str().unwrap().contains("Is anyone there?"));
    assert!(matches!(layer::get(s.active().unwrap()).ping(), Some(layer::Ping::Waiting { .. })));
    // A second check replaces the first.
    let id2 = s.execute("agent.ping", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    assert_eq!(layer::get(s.active().unwrap()).questions.iter().filter(|q| q.is_ping()).count(), 1);
    let r = s.execute("agent.answer", json!({"id": id2, "text": "here", "actor": "agent"})).unwrap();
    assert_eq!(r["ping"], json!(true));
    assert!(r["answeredInMs"].as_u64().is_some());
    let l = layer::get(s.active().unwrap());
    assert!(matches!(l.ping(), Some(layer::Ping::Answered { .. })));
    assert_eq!(l.log.len(), log_before, "a connection check isn't history");
    // Ids are ping<ms>; an unanswered check drops out of the questions after a minute (the
    // status still says "no answer").
    let id3 = s.execute("agent.ping", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    assert!(id3.strip_prefix("ping").unwrap().parse::<u64>().unwrap() > 1_700_000_000_000);
    layer::update(s.active().unwrap(), |l| {
        if let Some(q) = l.questions.iter_mut().find(|q| q.id == id3) {
            q.asked_at -= 61_000;
        }
    });
    let pending = s.execute("agent.questions", json!({"status": "pending", "actor": "agent"})).unwrap();
    assert!(pending.as_array().unwrap().iter().all(|q| q["id"] != json!(id3)));
    assert!(matches!(layer::get(s.active().unwrap()).ping(), Some(layer::Ping::Waiting { .. })));
    // Never saved.
    let b64 = s.execute("file.saveBytes", json!({})).unwrap()["base64"].as_str().unwrap().to_string();
    let mut t = Session::new();
    t.execute("file.open", json!({"name": "book.xlsx", "base64": b64})).unwrap();
    let l = layer::get(t.active().unwrap());
    assert_eq!(l.questions.len(), 1);
    assert!(l.ping().is_none());
}

#[test]
fn presence_says_who_listens() {
    use crate::layer::Ping;
    use crate::runner::{Mode, Presence, Signals, classify};
    let now = 10_000_000;
    let none = Signals::default();
    assert_eq!(classify(None, &none, Mode::Manual, now), Presence::Nobody);
    assert_eq!(classify(Some(Ping::Waiting { asked_at: now - 5_000 }), &none, Mode::Manual, now), Presence::Checking { since_ms: 5_000 });
    assert_eq!(classify(Some(Ping::Waiting { asked_at: now - 61_000 }), &none, Mode::Manual, now), Presence::NoAnswer { asked_ms: 61_000 });
    assert_eq!(
        classify(Some(Ping::Answered { asked_at: now - 15_000, answered_at: now - 12_000 }), &none, Mode::Manual, now),
        Presence::Answering { checked_ms: 12_000, latency_ms: 3_000 }
    );
    // An old answer no longer counts; then the passive signals do.
    let old = Some(Ping::Answered { asked_at: now - 3_600_000, answered_at: now - 3_590_000 });
    assert_eq!(classify(old, &none, Mode::Manual, now), Presence::Nobody);
    let events = Signals { event_streams: 1, ..Default::default() };
    assert_eq!(classify(old, &events, Mode::Manual, now), Presence::Listener(vec!["event stream"]));
    let waiting = Signals { last_wait_ms: now - 30_000, ..Default::default() };
    assert_eq!(classify(None, &waiting, Mode::Manual, now), Presence::Listener(vec!["waiting for questions"]));
    let stale = Signals { last_wait_ms: now - 90_000, ..Default::default() };
    assert_eq!(classify(None, &stale, Mode::Manual, now), Presence::Nobody);
    // A channel stream counts in "my session" mode only.
    let channel = Signals { channel_streams: 1, ..Default::default() };
    assert_eq!(classify(None, &channel, Mode::Manual, now), Presence::Nobody);
    assert_eq!(classify(None, &channel, Mode::Channel, now), Presence::Listener(vec!["Claude Code channel"]));
    // An attached conversation names itself.
    let att = Signals {
        listeners: vec![("Cowork · Acme".into(), now - 20_000), ("Claude Code".into(), now - 50_000)],
        event_streams: 1,
        ..Default::default()
    };
    assert_eq!(classify(None, &att, Mode::Manual, now), Presence::Listening { label: "Cowork · Acme".into(), seen_ms: 20_000, more: 1 });
    let bg = Signals { background: true, ..Default::default() };
    assert_eq!(classify(None, &bg, Mode::Background, now), Presence::Listener(vec!["background agent"]));
}

#[test]
fn waiting_for_questions_is_a_signal_and_off_refuses_tools() {
    let mut s = model();
    let before = crate::commands::last_wait_ms();
    s.execute("agent.questions", json!({"status": "pending", "waiting": true, "actor": "agent"})).unwrap();
    assert!(crate::commands::last_wait_ms() >= before.max(1));
    // Waiting and polling aren't tool calls; an agent's command is; a person's isn't.
    assert!(!crate::is_tool_call("agent.questions", &json!({"waiting": true, "actor": "agent"})));
    assert!(!crate::is_tool_call("agent.questions", &json!({"poll": true, "actor": "agent"})));
    assert!(crate::is_tool_call("agent.questions", &json!({"actor": "agent"})));
    assert!(!crate::is_tool_call("agent.questions", &json!({})));
    // Off: every agent-side command refuses with the same words; a person's still runs.
    let off = crate::commands::agent_allowed(true, &json!({"actor": "agent"})).unwrap_err().to_string();
    assert!(off.contains("the agent connection is off in GridCraft (Agent tab ▸ Connection ▸ Mode)"), "{off}");
    assert!(crate::commands::agent_allowed(true, &json!({})).is_ok());
    assert!(crate::commands::agent_allowed(false, &json!({"actor": "agent"})).is_ok());
}

/// Two workbooks open: the model (not active) and a blank one (active).
fn two_documents() -> (Session, String, String) {
    let mut s = model();
    s.execute("agent.private", json!({"range": "B1"})).unwrap();
    let first = crate::docs::doc_id(s.active().unwrap().uid);
    s.new_workbook();
    let second = crate::docs::doc_id(s.active().unwrap().uid);
    (s, first, second)
}

#[test]
fn commands_reach_a_document_that_isnt_active() {
    let (mut s, first, second) = two_documents();
    assert_ne!(first, second);
    let active = s.active_index();
    // Read the model while the blank workbook stays active.
    let r = s.execute("agent.onDocument", json!({"document": first, "command": "agent.read", "params": {"range": "B6", "masked": false}})).unwrap();
    assert_eq!(r["cells"][0]["value"], json!(600000.0));
    assert_eq!(s.active_index(), active);
    // A question asked there carries its document.
    s.execute("agent.onDocument", json!({"document": first, "command": "agent.ask", "params": {"range": "B7", "text": "Why?"}})).unwrap();
    let q = s.execute("agent.onDocument", json!({"document": first, "command": "agent.questions", "params": {"status": "pending"}})).unwrap();
    assert_eq!(q[0]["document"], json!(first));
    assert_eq!(q[0]["documentTitle"], json!("Book1"));
    assert!(s.execute("agent.questions", json!({"status": "pending"})).unwrap().as_array().unwrap().is_empty());
    // Unknown or closed: the protocol's error.
    let e = s.execute("agent.onDocument", json!({"document": "gridcraft:1:999", "command": "agent.context"})).unwrap_err().to_string();
    assert!(e.contains("is not open in GridCraft"), "{e}");
    // list_documents: ids, titles, frontmost, open questions.
    let docs = s.execute("agent.listDocuments", json!({})).unwrap();
    let d1 = docs.as_array().unwrap().iter().find(|d| d["id"] == json!(first)).unwrap().clone();
    assert_eq!(d1["openQuestions"], json!(1));
    assert_eq!(d1["frontmost"], json!(false));
    assert_eq!(d1["path"], Value::Null);
}

#[test]
fn listeners_attach_stay_while_seen_and_answer_pings() {
    let (mut s, first, _second) = two_documents();
    let a = s.execute("agent.onDocument", json!({"document": first, "command": "agent.attach", "params": {"label": "Cowork · Acme"}})).unwrap();
    let listener = a["listener"].as_str().unwrap().to_string();
    assert_eq!(a["document"], json!(first));
    assert_eq!(a["title"], json!("Book1"));
    let uid = crate::docs::uid_of(&first).unwrap();
    assert_eq!(crate::docs::listeners(uid)[0].label, "Cowork · Acme");
    // Seen through a question call; dropped 90 s after the last one.
    crate::docs::set_last_seen(uid, &listener, crate::now_ms() - 80_000);
    s.execute("agent.onDocument", json!({"document": first, "command": "agent.questions", "params": {"listener": listener, "actor": "agent"}}))
        .unwrap();
    assert!(crate::now_ms() - crate::docs::listeners(uid)[0].last_seen < 5_000);
    crate::docs::set_last_seen(uid, &listener, crate::now_ms() - 91_000);
    assert!(crate::docs::listeners(uid).is_empty());
    // A ping on that document, answered by a listener: who answered is recorded.
    let a =
        s.execute("agent.onDocument", json!({"document": first, "command": "agent.attach", "params": {"label": "Claude Code · gridcraft"}})).unwrap();
    let listener = a["listener"].as_str().unwrap().to_string();
    let i = crate::docs::index_of(&s, &first).unwrap();
    s.set_active(i);
    let ping = s.execute("agent.ping", json!({})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("agent.answer", json!({"id": ping, "text": "here", "listener": listener, "actor": "agent"})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).ping_answered_by().as_deref(), Some("Claude Code · gridcraft"));
    s.execute("agent.detach", json!({"listener": listener})).unwrap();
    assert!(crate::docs::listeners(uid).is_empty());
    assert!(s.execute("agent.detach", json!({"listener": listener})).is_err());
}

#[test]
fn mcp_routes_by_document_and_listener() {
    struct Local(Session);
    impl gridcraft_mcp::Backend for Local {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "engine.execute" => self.0.run(params["command"].as_str().unwrap_or(""), params["params"].clone()),
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
    let (mut s, first, second) = two_documents();
    s.execute("agent.onDocument", json!({"document": first, "command": "agent.ask", "params": {"range": "B7", "text": "In the model"}})).unwrap();
    s.execute("agent.ask", json!({"range": "A1", "text": "In the blank one"})).unwrap();
    let mut b = Local(s);
    // Every tool takes `document`; the question tools take `listener`.
    let tools = crate::mcp::tools();
    assert!(tools.iter().all(|t| t["inputSchema"]["properties"]["document"].is_object()));
    let wait = tools.iter().find(|t| t["name"] == json!("wait_for_questions")).unwrap();
    assert!(wait["inputSchema"]["properties"]["listener"].is_object());
    let att = crate::mcp::call(&mut b, "attach", &json!({"document": first, "label": "Cowork · Acme"})).unwrap();
    let listener = att["listener"].as_str().unwrap().to_string();
    let q = crate::mcp::call(&mut b, "wait_for_questions", &json!({"document": first, "listener": listener, "timeout_seconds": 0})).unwrap();
    assert_eq!(q.as_array().unwrap().len(), 1);
    assert_eq!(q[0]["text"], json!("In the model"));
    assert_eq!(q[0]["document"], json!(first));
    let q2 = crate::mcp::call(&mut b, "list_questions", &json!({"document": second})).unwrap();
    assert_eq!(q2[0]["text"], json!("In the blank one"));
    // Reads of the other document stay masked.
    let r = crate::mcp::call(&mut b, "read_range", &json!({"document": first, "range": "B1"})).unwrap();
    assert_ne!(r["cells"][0]["value"], json!("Acme Holding AG"));
    assert!(crate::mcp::call(&mut b, "get_context", &json!({"document": "gridcraft:0:1"})).unwrap_err().contains("is not open"));
    let docs = crate::mcp::call(&mut b, "list_documents", &json!({})).unwrap();
    assert_eq!(docs.as_array().unwrap().len(), 2);
}

#[test]
fn registry_entry_contents_staleness_and_removal() {
    let (mut s, first, _second) = two_documents();
    let dir = std::env::temp_dir().join(format!("gridcraft-registry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    crate::registry::set_folder(Some(dir.clone()));
    s.execute("agent.onDocument", json!({"document": first, "command": "agent.attach", "params": {"label": "Cowork · Acme"}})).unwrap();
    let now = crate::now_ms();
    let v = crate::registry::contents(&s, "http://127.0.0.1:7980/mcp", now);
    assert_eq!(v["protocol"], json!("aitocraft/1"));
    assert_eq!(v["app"], json!("gridcraft"));
    assert_eq!(v["pid"], json!(std::process::id()));
    assert_eq!(v["mcp"], json!("http://127.0.0.1:7980/mcp"));
    let docs = v["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!(docs.iter().filter(|d| d["frontmost"] == json!(true)).count(), 1);
    let d1 = docs.iter().find(|d| d["id"] == json!(first)).unwrap();
    assert_eq!(d1["listeners"][0]["label"], json!("Cowork · Acme"));
    // No content, no client data.
    let text = v.to_string();
    assert!(!text.contains("Acme Holding") && !text.contains("600000"), "{text}");
    // Written atomically (no temporary file left), then removed.
    let path = crate::registry::write(&dir, &crate::registry::file_name(), &v).unwrap();
    assert!(path.exists());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    // Staleness: an old heartbeat, or a pid that isn't running.
    assert!(!crate::registry::is_stale(&v, now + 5_000, |_| true));
    assert!(crate::registry::is_stale(&v, now + 31_000, |_| true));
    assert!(crate::registry::is_stale(&v, now, |_| false));
    // A crash leaves a stale entry: swept; ours stays.
    let mut dead = v.clone();
    dead["pid"] = json!(999_999);
    crate::registry::write(&dir, "gridcraft-999999.json", &dead).unwrap();
    crate::registry::sweep(&dir, now);
    assert!(!dir.join("gridcraft-999999.json").exists());
    assert!(path.exists());
    // tick writes once enabled; remove deletes.
    crate::registry::enable("http://127.0.0.1:7980/mcp");
    std::fs::remove_file(&path).unwrap();
    crate::registry::tick(&s);
    assert!(path.exists());
    crate::registry::remove();
    assert!(!path.exists());
    crate::registry::set_folder(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn registry_says_when_each_document_had_focus() {
    let (mut s, first, second) = two_documents();
    let (u1, u2) = (crate::docs::uid_of(&first).unwrap(), crate::docs::uid_of(&second).unwrap());
    // Never focused yet: null.
    let doc = |s: &Session, id: &str| crate::docs::documents(s).into_iter().find(|d| d["id"] == json!(id)).unwrap();
    assert_eq!(doc(&s, &second)["focusedAt"], Value::Null);
    // The window gains focus on the second document.
    crate::docs::note_focus(&s, false, 1_000);
    crate::docs::note_focus(&s, true, 2_000);
    assert_eq!(crate::docs::focused_at(u2), Some(2_000));
    // Staying focused doesn't move it (no rewrite every frame).
    crate::docs::note_focus(&s, true, 3_000);
    assert_eq!(crate::docs::focused_at(u2), Some(2_000));
    // Switching documents while focused: the new one is now.
    let i = crate::docs::index_of(&s, &first).unwrap();
    s.set_active(i);
    crate::docs::note_focus(&s, true, 4_000);
    assert_eq!(crate::docs::focused_at(u1), Some(4_000));
    assert_eq!(crate::docs::focused_at(u2), Some(2_000));
    // Unfocused, then focused again.
    crate::docs::note_focus(&s, false, 5_000);
    crate::docs::note_focus(&s, true, 6_000);
    assert_eq!(doc(&s, &first)["focusedAt"], json!(6_000));
    let v = crate::registry::contents(&s, "http://127.0.0.1:7980/mcp", 7_000);
    assert!(v["documents"].as_array().unwrap().iter().any(|d| d["focusedAt"] == json!(6_000)));
}

// ---------------------------------------------------------------- Topics v1

fn topic_of(s: &Session, id: &str) -> crate::layer::Topic {
    layer::get(s.active().unwrap()).topic(id).cloned().unwrap()
}

fn render_answer(s: &mut Session, qid: &str) -> String {
    let all = s.execute("agent.questions", json!({"masked": false})).unwrap();
    all.as_array().unwrap().iter().find(|q| q["id"] == json!(qid)).unwrap()["answer"]["text"].as_str().unwrap().to_string()
}

#[test]
fn links_in_text_follow_the_cells_silently() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    let id = q["id"].as_str().unwrap().to_string();
    // A cell link, plain references (a range, another sheet's cell, a cell that holds something),
    // and a plain word that only looks like a cell (Q3 is empty: not a link).
    s.execute("sheet.add", json!({})).ok();
    s.execute(
        "agent.answer",
        json!({"id": id, "text": "Profit is [the margin](cell:Sheet1!B6) less B5; see B2:B4 for inputs. Q3 numbers aren't in yet.", "actor": "agent"}),
    )
    .unwrap();
    let before = render_answer(&mut s, &id);
    assert!(before.contains("[the margin](cell:Sheet1!B6)"), "{before}");
    assert!(before.contains("[B5](cell:Sheet1!B5)"), "{before}");
    assert!(before.contains("[B2:B4](cell:Sheet1!B2:B4)"), "{before}");
    assert!(before.contains("Q3 numbers"), "{before}");
    assert!(!before.contains("cell:Sheet1!Q3"), "{before}");
    // A row inserted above: the same text now says B6 and B3:B5, nothing marking the change.
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    let after = render_answer(&mut s, &id);
    assert!(after.contains("less [B6](cell:Sheet1!B6)"), "{after}");
    assert!(after.contains("[B3:B5](cell:Sheet1!B3:B5)"), "{after}");
    assert!(after.contains("[the margin](cell:Sheet1!B7)"), "{after}");
}

#[test]
fn topics_are_numbered_like_footnotes_and_places_within_them() {
    let mut s = model();
    let a = s.execute("agent.ask", json!({"range": "B7", "text": "Profit?"})).unwrap();
    let b = s.execute("agent.ask", json!({"range": "B5", "text": "Fixed costs?"})).unwrap();
    let (ta, tb) = (a["topic"].as_str().unwrap().to_string(), b["topic"].as_str().unwrap().to_string());
    let n = |s: &Session| {
        let d = s.active().unwrap();
        crate::topics::numbering(&d.wb, &layer::get(d))
    };
    // B5 comes before B7: topic numbers follow the sheet, not the order of asking.
    assert_eq!(n(&s).topic[&tb], 1);
    assert_eq!(n(&s).topic[&ta], 2);
    // A follow-up on the same cell joins the topic; the agent adds a place: 2.1, 2.2.
    let c = s.execute("agent.ask", json!({"range": "B7", "text": "And next year?"})).unwrap();
    assert_eq!(c["topic"], json!(ta));
    s.execute("agent.addPlaces", json!({"topic": "2", "places": [{"range": "B3", "reason": "depends on volume"}], "actor": "agent"})).unwrap();
    let t = topic_of(&s, &ta);
    assert_eq!(t.places.len(), 2);
    let nn = n(&s);
    // B3 now comes first: topic `ta` is 1, its places 1.1 (B3) and 1.2 (B7).
    assert_eq!(nn.topic[&ta], 1);
    assert_eq!(nn.place[&(ta.clone(), t.places[1].anchor.clone())], "1.1");
    assert_eq!(nn.place[&(ta.clone(), t.places[0].anchor.clone())], "1.2");
    // A new topic on purpose, on the same cell.
    let d = s.execute("agent.ask", json!({"range": "B7", "text": "Separate thing", "newTopic": true})).unwrap();
    assert_ne!(d["topic"], json!(ta));
    // The agent widens a place to the whole row, with its reason.
    s.execute("agent.widenPlace", json!({"place": "1.2", "to": "A7:B7", "reason": "the whole profit line", "actor": "agent"})).unwrap();
    let t = topic_of(&s, &ta);
    let d = s.active().unwrap();
    let widened = t.places.iter().find(|p| p.reason.contains("profit line")).unwrap();
    assert_eq!(crate::anchor::resolve(&d.wb, layer::get(d).anchor(&widened.anchor).unwrap()).unwrap().a1(), "Sheet1!A7:B7");
}

#[test]
fn a_deleted_place_remembers_where_it_was() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B5", "text": "Fixed costs?"})).unwrap();
    let t = q["topic"].as_str().unwrap().to_string();
    s.execute("selection.set", json!({"range": "A5"})).unwrap();
    s.execute("home.deleteRows", json!({})).unwrap();
    let topic = s.execute("agent.topic", json!({"topic": t, "masked": false})).unwrap();
    let removed = topic["places"][0]["removed"].as_str().unwrap();
    assert!(removed.contains("removed (was B5, between [B4](cell:Sheet1!B4) and [B5](cell:Sheet1!B5))"), "{removed}");
    // The neighbours are live: a row inserted above moves them.
    s.execute("selection.set", json!({"range": "A1"})).unwrap();
    s.execute("home.insertRows", json!({})).unwrap();
    let topic = s.execute("agent.topic", json!({"topic": t, "masked": false})).unwrap();
    assert!(topic["places"][0]["removed"].as_str().unwrap().contains("between [B5](cell:Sheet1!B5) and [B6](cell:Sheet1!B6)"), "{topic}");
    // Undo of the delete brings the place back as it was.
    s.execute("edit.undo", json!({"steps": 2})).unwrap();
    let topic = s.execute("agent.topic", json!({"topic": t, "masked": false})).unwrap();
    assert_eq!(topic["places"][0]["range"], json!("Sheet1!B5"));
}

#[test]
fn cut_moves_a_place_and_copy_or_fill_doesnt_duplicate_it() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B4", "text": "Unit cost?"})).unwrap();
    let t = q["topic"].as_str().unwrap().to_string();
    let places = |s: &mut Session| s.execute("agent.topic", json!({"topic": t, "masked": false})).unwrap()["places"].clone();
    s.execute("selection.set", json!({"range": "B4"})).unwrap();
    s.execute("edit.cut", json!({})).unwrap();
    s.execute("selection.set", json!({"range": "D10"})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    let p = places(&mut s);
    assert_eq!(p.as_array().unwrap().len(), 1);
    assert_eq!(p[0]["range"], json!("Sheet1!D10"));
    s.execute("selection.set", json!({"range": "D10"})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.execute("selection.set", json!({"range": "F12"})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    s.execute("selection.set", json!({"range": "D10:D14"})).unwrap();
    let _ = s.execute("home.fillDown", json!({}));
    let p = places(&mut s);
    assert_eq!(p.as_array().unwrap().len(), 1, "{p}");
    assert_eq!(p[0]["range"], json!("Sheet1!D10"));
    assert_eq!(layer::get(s.active().unwrap()).topics.len(), 1);
}

#[test]
fn undo_takes_a_question_back_and_redo_sends_it_again() {
    let mut s = model();
    // Not taken yet: gone, with its topic; redo asks again.
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    let id = q["id"].as_str().unwrap().to_string();
    s.execute("edit.undo", json!({})).unwrap();
    let l = layer::get(s.active().unwrap());
    assert!(l.questions.is_empty() && l.topics.is_empty());
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.execute("agent.questions", json!({"status": "pending"})).unwrap()[0]["id"], json!(id));
    // Being worked on: withdrawn; the agent's answer is refused; redo sends it again.
    s.execute("agent.claim", json!({"id": id, "actor": "agent"})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.execute("agent.questions", json!({"status": "withdrawn"})).unwrap()[0]["id"], json!(id));
    let e = s.execute("agent.answer", json!({"id": id, "text": "x", "actor": "agent"})).unwrap_err().to_string();
    assert!(e.contains("this question was withdrawn by the user; drop it"), "{e}");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.execute("agent.questions", json!({"status": "pending"})).unwrap()[0]["id"], json!(id));
    // Answered: undo hides it with its answer; redo shows it as it was, without asking again.
    s.execute("agent.answer", json!({"id": id, "text": "Because.", "actor": "agent"})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.execute("agent.questions", json!({})).unwrap().as_array().unwrap().is_empty());
    assert!(layer::get(s.active().unwrap()).topics[0].hidden);
    s.execute("edit.redo", json!({})).unwrap();
    let all = s.execute("agent.questions", json!({})).unwrap();
    assert_eq!(all[0]["status"], json!("answered"));
    assert_eq!(all[0]["answer"]["text"], json!("Because."));
    // The agent's contributions aren't on the person's stack.
    let depth = s.active().unwrap().undo.len();
    s.execute("agent.note", json!({"topic": q["topic"], "text": "An explanation", "actor": "agent"})).unwrap();
    s.execute("agent.setStatus", json!({"topic": q["topic"], "status": "revise", "actor": "agent"})).unwrap();
    assert_eq!(s.active().unwrap().undo.len(), depth);
}

#[test]
fn a_persons_layer_actions_undo_in_order() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why?"})).unwrap();
    let t = q["topic"].as_str().unwrap().to_string();
    s.execute("agent.setStatus", json!({"topic": t, "status": "revise", "text": "B5 must come down"})).unwrap();
    s.execute("cell.set", json!({"cell": "B5", "input": 90000})).unwrap();
    s.execute("agent.private", json!({"range": "B1"})).unwrap();
    // Undo: client data, the cell, the status: each in turn, nothing else touched.
    s.execute("edit.undo", json!({})).unwrap();
    assert!(layer::get(s.active().unwrap()).private.is_empty());
    assert_eq!(num(&s, "B5"), 90000.0);
    assert_eq!(topic_of(&s, &t).status.status, layer::Status::Revise);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(num(&s, "B5"), 100000.0);
    assert_eq!(topic_of(&s, &t).status.status, layer::Status::Revise);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(topic_of(&s, &t).status.status, layer::Status::Open);
    s.execute("edit.redo", json!({"steps": 3})).unwrap();
    assert_eq!(topic_of(&s, &t).status.status, layer::Status::Revise);
    assert_eq!(num(&s, "B5"), 90000.0);
    assert_eq!(layer::get(s.active().unwrap()).private.len(), 1);
    // Removing a note is undoable too.
    let n = s.execute("agent.note", json!({"topic": t, "text": "From the 2025 budget", "actor": "agent"})).unwrap();
    s.execute("agent.removeNote", json!({"id": n["id"]})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).notes.len(), 1);
}

#[test]
fn only_a_person_sets_agreed() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "OK?"})).unwrap();
    let t = q["topic"].as_str().unwrap().to_string();
    let e = s.execute("agent.setStatus", json!({"topic": t, "status": "agreed", "actor": "agent"})).unwrap_err().to_string();
    assert!(e.contains("only a person sets Agreed"), "{e}");
    assert!(s.execute("agent.acceptProposal", json!({"topic": t, "actor": "agent"})).is_err());
    s.execute("agent.proposeAgreed", json!({"topic": t, "reasons": "B5 matches the 2025 budget", "actor": "agent"})).unwrap();
    assert!(topic_of(&s, &t).proposed.is_some());
    s.execute("agent.acceptProposal", json!({"topic": t})).unwrap();
    let tp = topic_of(&s, &t);
    assert_eq!(tp.status.status, layer::Status::Agreed);
    assert_eq!(tp.status.by, crate::Actor::Human);
    assert!(tp.proposed.is_none());
    // Every status change is in the log, with who made it.
    let l = layer::get(s.active().unwrap());
    assert!(l.log.iter().any(|e| e.text.contains("Agreed") && e.actor == crate::Actor::Human));
    assert!(l.log.iter().any(|e| e.text.contains("Proposed Agreed") && e.actor == crate::Actor::Agent));
    // Sign-off: agreed 1 of 1.
    assert_eq!(crate::topics::signoff(&l).0, 1);
}

#[test]
fn version_1_workbooks_become_topics_on_load() {
    let mut s = model();
    // A v1 layer as an older build wrote it: two questions (one answered and read) and a note.
    let v1 = json!({
        "version": 1, "next_id": 9,
        "anchors": [{"id": "a1", "name": "_GcAgent_a1", "sheet": "Sheet1", "range": "B7"}, {"id": "a2", "name": "_GcAgent_a2", "sheet": "Sheet1", "range": "B5"}],
        "questions": [
            {"id": "q3", "anchor": "a1", "text": "Why?", "status": "answered", "seen": true, "answer": {"text": "Because.", "at": 5}},
            {"id": "q4", "anchor": "a2", "text": "Source?", "status": "pending"}
        ],
        "notes": [{"id": "n5", "anchor": "a2", "kind": "source", "text": "2025 budget", "author": "agent"}],
        "log": [{"seq": 1, "at": 1, "actor": "human", "kind": "decision", "text": "started"}]
    });
    let d = s.active_mut().unwrap();
    let wb = std::sync::Arc::make_mut(&mut d.wb);
    for (name, cell) in [("_GcAgent_a1", "$B$7"), ("_GcAgent_a2", "$B$5")] {
        wb.names.push(gridcraft_engine::model::DefinedName {
            name: name.into(),
            scope: None,
            formula: format!("Sheet1!{cell}"),
            comment: String::new(),
            hidden: true,
        });
    }
    let mut p: gridcraft_engine::model::Preserved = Default::default();
    let xml = format!("<agentLayer xmlns=\"{}\"><![CDATA[{}]]></agentLayer>", layer::NS, v1);
    p.parts.push(gridcraft_engine::model::PreservedPart {
        name: "customXml/gridcraftAgent.xml".into(),
        content_type: "application/xml".into(),
        data: std::sync::Arc::from(xml.into_bytes()),
        rels: vec![],
    });
    wb.preserved = Some(std::sync::Arc::new(p));
    let l = layer::read_from(&s.active().unwrap().wb);
    assert_eq!(l.version, layer::VERSION);
    assert_eq!(l.topics.len(), 3);
    let q3 = l.questions.iter().find(|q| q.id == "q3").unwrap();
    let q4 = l.questions.iter().find(|q| q.id == "q4").unwrap();
    assert_eq!(l.topic(&q3.topic).unwrap().status.status, layer::Status::Info, "answered and read");
    assert_eq!(l.topic(&q4.topic).unwrap().status.status, layer::Status::Open);
    let n5 = l.notes.iter().find(|n| n.id == "n5").unwrap();
    let tn = l.topic(&n5.topic).unwrap();
    assert_eq!(tn.status.status, layer::Status::Info);
    assert_eq!(tn.places[0].anchor, "a2");
    // Saved again: v2.
    let mut wb2 = (*s.active().unwrap().wb).clone();
    layer::write_into(&mut wb2, &l);
    assert_eq!(layer::read_from(&wb2), {
        let mut x = l.clone();
        x.revision = 0;
        x
    });
}

#[test]
fn voice_refs_become_places_of_the_topic() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "transcribing": true, "refs": [{"n": 1, "range": "B6"}, {"n": 2, "range": "B3"}]})).unwrap();
    let id = q["id"].as_str().unwrap().to_string();
    s.execute("agent.spoken", json!({"id": id, "text": "Why is ⟦1⟧ bigger than ⟦2⟧?"})).unwrap();
    let t = topic_of(&s, q["topic"].as_str().unwrap());
    assert_eq!(t.places.len(), 3, "the main cell and the two pointed at");
    assert_eq!(t.places.iter().filter(|p| p.reason == "pointed at while asking").count(), 2);
    let d = s.active().unwrap();
    let n = crate::topics::numbering(&d.wb, &layer::get(d));
    let mut labels: Vec<String> = t.places.iter().map(|p| n.place[&(t.id.clone(), p.anchor.clone())].clone()).collect();
    labels.sort();
    assert_eq!(labels, vec!["1.1", "1.2", "1.3"]);
    // The markers in the text are place links now.
    let text = layer::get(s.active().unwrap()).questions[0].text.clone();
    assert!(!text.contains('⟦') && text.matches("(anchor:").count() == 2, "{text}");
}

#[test]
fn mcp_topics_tools() {
    struct Local(Session);
    impl gridcraft_mcp::Backend for Local {
        fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "engine.execute" => self.0.run(params["command"].as_str().unwrap_or(""), params["params"].clone()),
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
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Why is profit 150k?"})).unwrap();
    let mut b = Local(s);
    let call = |b: &mut Local, name: &str, args: Value| crate::mcp::call(b, name, &args);
    let w = call(&mut b, "wait_for_questions", json!({"timeout_seconds": 0})).unwrap();
    assert_eq!(w[0]["topic"]["number"], json!(1));
    assert_eq!(w[0]["topic"]["status"], json!("open"));
    call(&mut b, "add_note", json!({"topic": "1", "title": "Budget", "kind": "source", "url": "https://example.com/budget", "summary": "2025 budget, line 14", "text": "Fixed costs B5 come from the budget."})).unwrap();
    call(&mut b, "add_places", json!({"topic": "1", "places": [{"range": "B5", "reason": "fixed costs drive it"}]})).unwrap();
    call(&mut b, "set_status", json!({"topic": "1", "status": "revise", "text": "B5 is last year's"})).unwrap();
    assert!(call(&mut b, "set_status", json!({"topic": "1", "status": "agreed"})).is_err());
    call(&mut b, "propose_agreed", json!({"topic": "1", "reasons": "checked against the budget"})).unwrap();
    let t = call(&mut b, "get_topic", json!({"topic": "1"})).unwrap();
    assert_eq!(t["status"], json!("revise"));
    assert_eq!(t["proposed"]["status"], json!("agreed"));
    assert_eq!(t["places"].as_array().unwrap().len(), 2);
    assert_eq!(t["notes"][0]["links"][0]["href"], json!("https://example.com/budget"));
    assert!(t["notes"][0]["text"].as_str().unwrap().contains("[B5](cell:Sheet1!B5)"));
    assert_eq!(t["discussion"][0]["id"], q["id"]);
    let list = call(&mut b, "list_topics", json!({"status": "revise"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    call(&mut b, "widen_place", json!({"place": "1.2", "to": "A7:B7", "reason": "the whole row"})).unwrap();
}

// ---------------------------------------------------------------- Context (what the agent knows)

/// Context tests change the app-wide runner mode: one at a time.
static CONTEXT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn context_is_per_document_saved_in_it_and_undoable() {
    use crate::context;
    use crate::layer::{CodeVia, ContextKind};
    use crate::runner::{self, Mode};
    let _g = CONTEXT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    context::forget_last();
    runner::set_mode(Mode::Manual);
    let mut s = model();
    // Before any choice the document follows the app (Claude Cowork for `connect`).
    let c = s.execute("agent.agentContext", json!({})).unwrap();
    assert_eq!(c["kind"], json!("cowork"));
    // Claude Code, my session: the runner hands questions to the session's channel.
    s.execute("agent.agentContext", json!({"kind": "code"})).unwrap();
    assert_eq!(runner::mode(), Mode::Channel);
    // A project folder: a background client in that folder.
    let dir = std::env::temp_dir().join(format!("gc-context-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let c = s.execute("agent.agentContext", json!({"via": "folder", "folder": dir.display().to_string()})).unwrap();
    assert_eq!((c["kind"].clone(), c["via"].clone(), c["mode"].clone()), (json!("code"), json!("folder"), json!("background")));
    assert_eq!(runner::mode(), Mode::Background);
    assert_eq!(runner::config().folder, Some(dir.clone()));
    assert!(s.execute("agent.agentContext", json!({"folder": "/no/such/folder/here"})).is_err());
    // No context, with the web: WebSearch and WebFetch join the allowed tools; no folder.
    s.execute("agent.agentContext", json!({"kind": "none", "web": true})).unwrap();
    assert_eq!(runner::config().folder, None);
    let args = runner::args(&runner::config(), "id", false, "mcp.json", None, "x");
    assert!(args.contains(&"WebSearch".to_string()) && args.contains(&"WebFetch".to_string()), "{args:?}");
    s.execute("agent.agentContext", json!({"web": false})).unwrap();
    assert!(!runner::args(&runner::config(), "id", false, "mcp.json", None, "x").contains(&"WebSearch".to_string()));
    // Each change is the person's, on their ⌘Z: back to web on, then to the project folder.
    s.execute("edit.undo", json!({})).unwrap();
    assert!(layer::get(s.active().unwrap()).context.as_ref().unwrap().web);
    assert!(runner::config().web);
    s.execute("edit.undo", json!({})).unwrap();
    let c = layer::get(s.active().unwrap()).context.clone().unwrap();
    assert_eq!((c.kind, c.via), (ContextKind::Code, CodeVia::Folder));
    assert_eq!(runner::mode(), Mode::Background);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).context.as_ref().unwrap().kind, ContextKind::None);
    // Logged in the history, and only a person decides it.
    assert!(layer::get(s.active().unwrap()).log.iter().any(|e| e.text.starts_with("Context: No context")));
    assert!(s.execute("agent.agentContext", json!({"kind": "cowork", "actor": "agent"})).is_err());
    // Saved in the file and read back.
    let mut wb = (*s.active().unwrap().wb).clone();
    layer::write_into(&mut wb, &layer::get(s.active().unwrap()));
    assert_eq!(layer::read_from(&wb).context, layer::get(s.active().unwrap()).context);
    // A new document starts with the last choice made in the app (its mode, not a folder).
    s.execute("agent.agentContext", json!({"kind": "code", "via": "folder", "folder": dir.display().to_string()})).unwrap();
    s.new_workbook();
    let c = s.execute("agent.agentContext", json!({})).unwrap();
    assert_eq!((c["kind"].clone(), c["via"].clone(), c["folder"].clone()), (json!("code"), json!("folder"), json!("")));
    context::forget_last();
    runner::set_mode(Mode::Manual);
    runner::set_config(runner::Config { folder: None, web: false, ..runner::config() });
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_saved_context_never_starts_an_agent_by_itself() {
    use crate::runner::{self, Mode};
    let _g = CONTEXT_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut s = model();
    // A workbook someone sent: questions pending, no context chosen and nothing asked in this run.
    let uid = s.active().unwrap().uid;
    let q = layer::Question {
        id: "q90".into(),
        anchor: String::new(),
        text: "Why?".into(),
        status: layer::QuestionStatus::Pending,
        ..Default::default()
    };
    layer::update(s.active().unwrap(), |l| l.questions.push(q));
    runner::set_mode(Mode::Background);
    runner::tick(&mut s);
    assert!(runner::status(uid).is_none(), "nothing started for a file just opened");
    runner::set_mode(Mode::Manual);
}

// ---------------------------------------------------------------- proposals instead of direct edits

struct McpLocal(Session);
impl gridcraft_mcp::Backend for McpLocal {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        match method {
            "engine.execute" => self.0.run(params["command"].as_str().unwrap_or(""), params["params"].clone()),
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

#[test]
fn in_a_topic_the_agent_proposes_and_the_person_applies() {
    let mut s = model();
    let q = s.execute("agent.ask", json!({"range": "B7", "text": "Can profit reach 200k?"})).unwrap();
    let t = q["topic"].as_str().unwrap().to_string();
    let mut b = McpLocal(s);
    let call = |b: &mut McpLocal, name: &str, args: Value| crate::mcp::call(b, name, &args);
    // Option A: a lower unit cost; option B: a higher price. The workbook doesn't change.
    let r =
        call(&mut b, "run_command", json!({"topic": "1", "command": "cell.set", "params": {"cell": "B4", "input": 60}, "reason": "supplier offer"}))
            .unwrap();
    assert_eq!(r["applied"], json!(false));
    call(&mut b, "describe_option", json!({"topic": "1", "option": "A", "title": "Cheaper supplier", "why": "B4 drops to 60 with the new supplier"}))
        .unwrap();
    call(
        &mut b,
        "run_command",
        json!({"topic": "1", "option": "B", "command": "cell.set", "params": {"cell": "B2", "input": 130}, "reason": "price list"}),
    )
    .unwrap();
    call(&mut b, "describe_option", json!({"topic": "1", "option": "B", "title": "Raise the price", "why": "+10 on B2"})).unwrap();
    assert_eq!(num(&b.0, "B4"), 70.0);
    assert_eq!(num(&b.0, "B2"), 120.0);
    // A command that doesn't run is refused and not recorded.
    assert!(call(&mut b, "run_command", json!({"topic": "1", "command": "cell.set", "params": {"cell": "ZZZZ9"}})).is_err());
    // The agent sees the proposal: in the topic, in reads of the accepted workbook, in the draft.
    let topic = call(&mut b, "get_topic", json!({"topic": "1"})).unwrap();
    assert_eq!(topic["proposal"]["state"], json!("pending"));
    assert_eq!(topic["proposal"]["options"][0]["title"], json!("Cheaper supplier"));
    assert_eq!(topic["proposal"]["options"][0]["changes"], json!(1));
    assert!(topic["proposal"]["options"][0]["why"].as_str().unwrap().contains("[B4](cell:Sheet1!B4)"), "{topic}");
    let list = call(&mut b, "list_topics", json!({})).unwrap();
    assert_eq!(list[0]["proposal"]["options"].as_array().unwrap().len(), 2);
    let read = call(&mut b, "read_range", json!({"range": "B4:B7"})).unwrap();
    let cell = |r: &Value, a: &str| r["cells"].as_array().unwrap().iter().find(|c| c["cell"] == json!(a)).cloned().unwrap();
    assert_eq!(cell(&read, "B4")["value"], json!(70.0));
    assert_eq!(cell(&read, "B4")["pending"][0]["option"], json!("A"));
    assert_eq!(cell(&read, "B4")["pending"][0]["summary"], json!("Cheaper supplier (1 change)"));
    // B7 recalculates under both options.
    assert_eq!(cell(&read, "B7")["pending"].as_array().unwrap().len(), 2);
    let draft = call(&mut b, "read_range", json!({"range": "B4:B7", "view": "proposal", "topic": "1", "option": "A"})).unwrap();
    assert_eq!(cell(&draft, "B4")["value"], json!(60.0));
    assert_eq!(cell(&draft, "B7")["value"], json!(200000.0));
    // A new question on the same cells carries the pending proposals.
    let mut s = b.0;
    s.execute("agent.ask", json!({"range": "B7", "text": "And with both?", "newTopic": true})).unwrap();
    let w = s.execute("agent.questions", json!({"status": "pending", "actor": "agent"})).unwrap();
    let last = w.as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["pendingNearby"].as_array().unwrap().len(), 2, "{last}");
    // Only the person applies; the change is one undo step with a decision note.
    assert!(s.execute("agent.applyProposal", json!({"topic": t, "option": "B", "actor": "agent"})).is_err());
    let depth = s.active().unwrap().undo.len();
    s.execute("agent.applyProposal", json!({"topic": t, "option": "B"})).unwrap();
    assert_eq!(num(&s, "B2"), 130.0);
    assert_eq!(num(&s, "B7"), 200000.0);
    assert_eq!(s.active().unwrap().undo.len(), depth + 1);
    let l = layer::get(s.active().unwrap());
    let e = l.topic(&t).unwrap().edits.clone().unwrap();
    assert_eq!((e.state, e.chosen.as_str()), (layer::ProposalState::Applied, "B"));
    assert!(l.notes.iter().any(|n| n.topic == t && n.kind == "decision" && n.text == "chose B: Raise the price"));
    // Undo takes it all back, workbook and proposal; redo applies it again.
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(num(&s, "B2"), 120.0);
    let l = layer::get(s.active().unwrap());
    assert_eq!(l.topic(&t).unwrap().edits.as_ref().unwrap().state, layer::ProposalState::Pending);
    assert!(!l.notes.iter().any(|n| n.kind == "decision"));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(num(&s, "B2"), 130.0);
}

#[test]
fn applying_one_proposal_outdates_the_ones_it_overlaps() {
    let mut s = model();
    let a = s.execute("agent.ask", json!({"range": "B4", "text": "Unit cost?"})).unwrap()["topic"].as_str().unwrap().to_string();
    let b = s.execute("agent.ask", json!({"range": "B5", "text": "Fixed costs?"})).unwrap()["topic"].as_str().unwrap().to_string();
    let c = s.execute("agent.ask", json!({"range": "B2", "text": "Price?"})).unwrap()["topic"].as_str().unwrap().to_string();
    let rec = |s: &mut Session, t: &str, cell: &str, v: f64| {
        s.execute("agent.exec", json!({"topic": t, "command": "cell.set", "params": {"cell": cell, "input": v}, "actor": "agent"})).unwrap();
    };
    rec(&mut s, &a, "B4", 65.0);
    rec(&mut s, &b, "B4", 68.0);
    rec(&mut s, &c, "C20", 1.0);
    s.execute("agent.applyProposal", json!({"topic": a})).unwrap();
    let l = layer::get(s.active().unwrap());
    let state = |id: &str| l.topic(id).unwrap().edits.as_ref().unwrap().state;
    assert_eq!(state(&a), layer::ProposalState::Applied);
    assert_eq!(state(&b), layer::ProposalState::Outdated, "both change B4");
    assert_eq!(state(&c), layer::ProposalState::Pending, "C20 doesn't overlap");
    // A step that no longer fits applies nothing: the sheet it wrote to is gone.
    s.execute("home.insertSheet", json!({})).unwrap();
    let names: Vec<String> = s.active().unwrap().wb.sheets.iter().map(|x| x.name.clone()).collect();
    let other = names.iter().find(|n| n.as_str() != "Sheet1").unwrap().clone();
    let at = |s: &Session, n: &str| s.active().unwrap().wb.sheets.iter().position(|x| x.name == n).unwrap();
    s.execute("sheet.activate", json!({"sheet": at(&s, "Sheet1")})).unwrap();
    rec(&mut s, &c, &format!("{other}!B1"), 5.0);
    s.execute("sheet.activate", json!({"sheet": at(&s, &other)})).unwrap();
    s.execute("home.deleteSheet", json!({})).unwrap();
    s.execute("sheet.activate", json!({"sheet": at(&s, "Sheet1")})).unwrap();
    let c20 = |s: &Session| s.active().unwrap().wb.sheet(0).unwrap().cell(gridcraft_engine::core::CellRef::parse("C20").unwrap()).is_some();
    let depth = s.active().unwrap().undo.len();
    let e = s.execute("agent.applyProposal", json!({"topic": c})).unwrap_err().to_string();
    assert!(e.contains("out of date, ask the agent to redo"), "{e}");
    // Nothing applied, not even the steps that still fit; nothing on the undo stack.
    assert!(!c20(&s));
    assert_eq!(s.active().unwrap().undo.len(), depth);
    assert_eq!(layer::get(s.active().unwrap()).topic(&c).unwrap().edits.as_ref().unwrap().state, layer::ProposalState::Outdated);
    // Recording again starts a fresh proposal.
    rec(&mut s, &c, "C21", 2.0);
    let e = layer::get(s.active().unwrap()).topic(&c).unwrap().edits.clone().unwrap();
    assert_eq!((e.state, e.options.len(), e.options[0].steps.len()), (layer::ProposalState::Pending, 1, 1));
}

#[test]
fn a_rejected_proposal_tells_the_agent_why() {
    let mut s = model();
    let t = s.execute("agent.ask", json!({"range": "B5", "text": "Cut fixed costs?"})).unwrap()["topic"].as_str().unwrap().to_string();
    s.execute("agent.exec", json!({"topic": t, "command": "cell.set", "params": {"cell": "B5", "input": 80000}, "actor": "agent"})).unwrap();
    assert!(s.execute("agent.rejectProposal", json!({"topic": t, "actor": "agent"})).is_err());
    s.execute("agent.rejectProposal", json!({"topic": t, "comment": "Rent can't go down before 2027"})).unwrap();
    let l = layer::get(s.active().unwrap());
    let e = l.topic(&t).unwrap().edits.clone().unwrap();
    assert_eq!((e.state, e.comment.as_str()), (layer::ProposalState::Rejected, "Rent can't go down before 2027"));
    // The comment reaches the agent as a follow-up in the same topic.
    let q = l.questions.iter().rev().find(|q| q.topic == t).unwrap();
    assert!(q.text.contains("Rent can't go down before 2027") && q.status == layer::QuestionStatus::Pending, "{}", q.text);
    assert_eq!(num(&s, "B5"), 100000.0);
    // The person's reject is undoable; discarding is the agent's.
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(layer::get(s.active().unwrap()).topic(&t).unwrap().edits.as_ref().unwrap().state, layer::ProposalState::Pending);
    s.execute("agent.discardProposal", json!({"topic": t, "actor": "agent"})).unwrap();
    assert!(layer::get(s.active().unwrap()).topic(&t).unwrap().edits.is_none());
    // Proposals don't go out with the workbook unless the discussion does.
    s.execute("agent.exec", json!({"topic": t, "command": "cell.set", "params": {"cell": "B5", "input": 90000}, "actor": "agent"})).unwrap();
    let l = layer::get(s.active().unwrap());
    assert!(crate::export::shared_layer(&l, false).topics.iter().all(|t| t.edits.is_none()));
    assert!(crate::export::shared_layer(&l, true).topics.iter().any(|t| t.edits.is_some()));
}
