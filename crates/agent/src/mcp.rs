//! The agent's MCP surface: JSON-RPC 2.0 (MCP `2025-06-18`) whose tools map onto `agent.*`
//! commands of the running app. Every read is masked (private cells show stand-ins); there is no
//! tool that returns real client values.

use gridcraft_mcp::Backend;
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "You are connected to the GridCraft spreadsheet the user has open, through its agent layer. \
The user reviews and signs off on this workbook; your job is to make that fast. They ask questions on cells: watch for them \
(GET http://127.0.0.1:<port>/events streams one line per new question; without a background monitor, as in Cowork, \
call wait_for_questions in a loop), claim_question, \
investigate with get_context / read_range / trace_number / sensitivity, then answer_question with links to the cells and \
sources you used (href \"cell:Sheet1!B5\", \"note:n3\" or https URLs). Leave prepared explanations with add_note, \
mark inputs with reasoned ranges (set_input_range), say how sure you are (set_confidence) and log decisions (log_decision). \
When the model rests on a choice, put both sides in the document (add_alternatives) instead of picking one; when a readable \
formula approximates, add the exact one beside it (add_calculation_method) and say where the difference matters. Use \
worst_case and risk_matrix to find what to check first. \
Change the workbook only with run_command, giving a reason and a source. PRIVACY: ranges listed in get_context.privateRanges \
hold client data; you see stand-in values with the same sign and magnitude, and everything computed from them is computed \
from the stand-ins. Never present those numbers as real; reason about the model's mechanics instead. You cannot write to \
private cells. \
CHANNEL: when GridCraft sends questions to this session, each arrives as <channel source=\"gridcraft\" question_id=\"q3\" \
range=\"Sheet1!B7\">question text</channel>: claim_question with that id, investigate, answer_question. Keep it short; \
the user is waiting in the spreadsheet. \
VOICE: ⟦n⟧ in a question is what the user pointed at while speaking (expanded as ⟦n: Sheet1!C16 · label = value⟧), see refs; \
resolve \"this\", \"that one\", \"это\" from those and the context.";

fn tool(name: &str, desc: &str, props: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name,
        "description": desc,
        "inputSchema": {"type": "object", "properties": props, "required": required, "additionalProperties": false},
        "annotations": {"readOnlyHint": read_only, "destructiveHint": false, "openWorldHint": false},
    })
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}
fn n(desc: &str) -> Value {
    json!({"type": "number", "description": desc})
}
fn links() -> Value {
    json!({"type": "array", "description": "Links: {label, href, summary?}. href is https://…, cell:Sheet1!B5 or note:<id>", "items": {
        "type": "object", "properties": {"label": {"type": "string"}, "href": {"type": "string"}, "summary": {"type": "string"}}, "required": ["label", "href"]}})
}

pub fn tools() -> Vec<Value> {
    vec![
        tool(
            "get_context",
            "Overview of the open workbook: sheets, used ranges, formula counts, private (client-data) ranges, marked inputs, open questions, recent decisions.",
            json!({}),
            &[],
            true,
        ),
        tool(
            "read_range",
            "Read cells (values, displayed text, formulas). Private cells and everything computed from them show stand-in values (flagged private / derivedFromPrivate).",
            json!({"range": s("A1 range, e.g. \"B2:F20\" or \"Model!A1:D9\"")}),
            &["range"],
            true,
        ),
        tool(
            "trace_number",
            "Where does this number come from: the calculation tree of a cell down to its inputs, with labels taken from the sheet. Deterministic: explain it, don't recompute it.",
            json!({"cell": s("Cell, e.g. \"Summary!C12\""), "depth": n("Levels to follow (1-12, default 6)")}),
            &["cell"],
            true,
        ),
        tool(
            "sensitivity",
            "How an output moves with its inputs: the output at the low and high end of each input's range (the ranges set with set_input_range, else ±10% defaults flagged reasoned=false).",
            json!({"cell": s("Output cell"), "limit": n("Most inputs (default 8)")}),
            &["cell"],
            true,
        ),
        tool(
            "list_questions",
            "Questions the user asked on cells. status: pending (new), working (claimed), answered, open (pending+working) or all. A spoken question carries ⟦n: Sheet1!C16 · label = value⟧ where the user pointed at a cell while speaking; refs lists them.",
            json!({"status": {"type": "string", "enum": ["pending", "working", "answered", "open", "all"]}}),
            &[],
            true,
        ),
        tool(
            "wait_for_questions",
            "Block until there is a pending question (or the timeout passes) and return the pending ones. An alternative to watching /events.",
            json!({"timeout_seconds": n("Up to 50 (default 25)")}),
            &[],
            true,
        ),
        tool(
            "claim_question",
            "Take a pending question so the user sees it is being worked on.",
            json!({"id": s("Question id, e.g. \"q3\"")}),
            &["id"],
            false,
        ),
        tool(
            "answer_question",
            "Answer a question; the cell shows \"answer ready\". Link to the cells and sources you used.",
            json!({"id": s("Question id"), "text": s("The answer (plain text, short paragraphs)"), "links": links()}),
            &["id", "text"],
            false,
        ),
        tool(
            "add_note",
            "Leave a note on a range: why it is done this way (explanation), a prepared answer to a likely question (faq), an assumption, or a source with a summary.",
            json!({"range": s("Range"), "text": s("Note text"), "title": s("Short title"), "kind": {"type": "string", "enum": ["explanation", "faq", "assumption", "source"]}, "links": links()}),
            &["range", "text"],
            false,
        ),
        tool(
            "set_input_range",
            "Mark a model input with the range it can plausibly take and why (from the nature of the parameter, not a flat ±10%). Use relative=true (multipliers, 0.8 = -20%) for private inputs.",
            json!({"cell": s("Input cell"), "low": n("Low end"), "high": n("High end"), "relative": {"type": "boolean"}, "rationale": s("Why this range")}),
            &["cell", "low", "high"],
            false,
        ),
        tool(
            "set_confidence",
            "How sure you are about a range: 0 = guess, 1 = checked against a primary source. Shown as a heat map.",
            json!({"range": s("Range"), "level": n("0..1"), "reason": s("Why")}),
            &["range", "level"],
            false,
        ),
        tool(
            "log_decision",
            "Record a decision in the document's history (why the model is the way it is).",
            json!({"text": s("The decision and its reason"), "range": s("Where it applies (optional)"), "source": s("What it is based on")}),
            &["text"],
            false,
        ),
        tool(
            "add_alternatives",
            "Put \"A or B?\" into the document: alternative values or formulas for a range (two growth assumptions, two cost formulas). The range's present contents stay as an option (\"As built\" unless you name it with `current`). The reader switches in the Agent pane and sees the model recalculate. Not allowed on private ranges.",
            json!({
                "range": s("Range, e.g. \"Model!C4\" or \"C4:C8\""),
                "title": s("What the choice is about, e.g. \"Growth assumption\""),
                "options": {"type": "array", "description": "1-8 options: {name, values, note?}. values: one value for every cell, a flat list for a single row/column, or rows of values; strings starting with = are formulas, \"5%\" is 0.05.", "items": {"type": "object", "properties": {"name": {"type": "string"}, "values": {}, "note": {"type": "string"}}, "required": ["name", "values"]}},
                "current": s("Name for the range's present contents (default \"As built\")"),
                "outputs": {"type": "array", "items": {"type": "string"}, "description": "Cells to compare the options on (default: the final outputs the choice moves most)"},
            }),
            &["range", "options"],
            false,
        ),
        tool(
            "add_calculation_method",
            "Give a cell a second calculation method side by side: the simple, readable formula (default: the cell's own) and an exact one. compare_alternatives then shows the relative difference on the outputs and flags where it is material (above `threshold`).",
            json!({"cell": s("Cell"), "exact": s("The exact formula, e.g. \"=XNPV(…)\""), "simple": s("The readable formula (default: the cell's)"), "threshold": n("Relative difference on an output that matters (default 0.01 = 1%)"), "note": s("Why the exact method differs"), "title": s("Short title"), "outputs": {"type": "array", "items": {"type": "string"}}}),
            &["cell", "exact"],
            false,
        ),
        tool(
            "compare_alternatives",
            "Every set of alternatives and calculation methods (or one, by id) evaluated on a copy of the model: the current option and the outputs' values under each option, with material differences flagged. Deterministic.",
            json!({"id": s("Set id (default: all)")}),
            &[],
            true,
        ),
        tool(
            "use_alternative",
            "Switch a range to one of its alternatives (an ordinary, undoable edit, logged as yours).",
            json!({"id": s("Set id"), "option": s("Option name (or index)")}),
            &["id", "option"],
            false,
        ),
        tool(
            "worst_case",
            "Move several inputs together (3-12, each inside its reasoned range from set_input_range) and find the lowest and highest value of an output, with where each input sits there. Deterministic, bounded search (all corners when few enough, points inside, coordinate refinement) on a copy of the model. oneAtATime shows what the single moves add up to: the gap is the inputs' interaction.",
            json!({"cell": s("Output cell"), "inputs": {"type": "array", "items": {"type": "string"}, "description": "Input cells (default: the marked inputs it depends on)"}, "maxEvals": n("Most model evaluations (default 3000, max 20000)")}),
            &["cell"],
            true,
        ),
        tool(
            "risk_matrix",
            "The inputs of an output on a confidence × impact matrix: how far each moves the output over its range, against your set_confidence level. Ranked by impact × (1 − confidence); \"check first\" = high impact, low confidence.",
            json!({"cell": s("Output cell")}),
            &["cell"],
            true,
        ),
        tool("get_layer", "The whole agent layer: anchors, questions, notes, inputs, private ranges, confidence, history.", json!({}), &[], true),
        tool(
            "run_command",
            "Change the workbook with any GridCraft command (cell.set, home.bold, …; see list_commands). Logged as yours with the reason and source. Reading commands are refused: use read_range.",
            json!({"command": s("Command id"), "params": {"type": "object"}, "reason": s("Why"), "source": s("What it is based on")}),
            &["command", "reason"],
            false,
        ),
        tool(
            "list_commands",
            "List GridCraft commands that change the workbook (id, label, params).",
            json!({"search": s("Substring to filter on")}),
            &[],
            true,
        ),
    ]
}

fn exec(b: &mut dyn Backend, command: &str, mut params: Value) -> Result<Value, String> {
    if let Value::Object(o) = &mut params {
        o.insert("actor".into(), json!("agent"));
        o.insert("masked".into(), json!(true));
    }
    b.call("engine.execute", json!({"command": command, "params": params}))
}

fn pick(args: &Value, keys: &[&str]) -> Value {
    let mut o = serde_json::Map::new();
    for k in keys {
        if let Some(v) = args.get(*k) {
            o.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(o)
}

pub fn call(b: &mut dyn Backend, name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "get_context" => exec(b, "agent.context", json!({})),
        "read_range" => exec(b, "agent.read", pick(args, &["range"])),
        "trace_number" => exec(b, "agent.trace", pick(args, &["cell", "depth"])),
        "sensitivity" => exec(b, "agent.sensitivity", pick(args, &["cell", "limit"])),
        "list_questions" => exec(b, "agent.questions", pick(args, &["status"])),
        "wait_for_questions" => {
            let secs = args.get("timeout_seconds").and_then(Value::as_f64).unwrap_or(25.0).clamp(0.0, 50.0);
            let mut polls = (secs * 2.0) as u32;
            loop {
                let v = exec(b, "agent.questions", json!({"status": "pending"}))?;
                if v.as_array().is_some_and(|a| !a.is_empty()) || polls == 0 {
                    return Ok(v);
                }
                polls -= 1;
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        "claim_question" => exec(b, "agent.claim", pick(args, &["id"])),
        "answer_question" => exec(b, "agent.answer", pick(args, &["id", "text", "links"])),
        "add_note" => exec(b, "agent.note", pick(args, &["range", "text", "title", "kind", "links"])),
        "set_input_range" => exec(b, "agent.input", pick(args, &["cell", "low", "high", "relative", "rationale"])),
        "set_confidence" => exec(b, "agent.confidence", pick(args, &["range", "level", "reason"])),
        "log_decision" => exec(b, "agent.decision", pick(args, &["text", "range", "source"])),
        "get_layer" => exec(b, "agent.layer", json!({})),
        "add_alternatives" => exec(b, "agent.variants", pick(args, &["range", "title", "options", "current", "outputs"])),
        "add_calculation_method" => exec(b, "agent.method", pick(args, &["cell", "exact", "simple", "threshold", "note", "title", "outputs"])),
        "compare_alternatives" => exec(b, "agent.compareVariants", pick(args, &["id"])),
        "worst_case" => exec(b, "agent.stress", pick(args, &["cell", "inputs", "maxEvals"])),
        "risk_matrix" => exec(b, "agent.risk", pick(args, &["cell"])),
        "use_alternative" => exec(b, "agent.useVariant", pick(args, &["id", "option"])),
        "run_command" => exec(b, "agent.exec", pick(args, &["command", "params", "reason", "source"])),
        "list_commands" => {
            let all = b.call("engine.commands", json!({}))?;
            let q = args.get("search").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let list: Vec<Value> = all
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| {
                            let id = c.get("id").and_then(Value::as_str).unwrap_or("");
                            let label = c.get("label").and_then(Value::as_str).unwrap_or("");
                            !id.starts_with("agent.")
                                && !id.starts_with("ui.")
                                && (q.is_empty() || id.to_lowercase().contains(&q) || label.to_lowercase().contains(&q))
                        })
                        .map(|c| pick(c, &["id", "label", "params"]))
                        .collect()
                })
                .unwrap_or_default();
            Ok(Value::Array(list))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

/// Handles one JSON-RPC message; `None` for notifications.
pub fn handle(b: &mut dyn Backend, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = id?;
    let ok = |r: Value| json!({"jsonrpc": "2.0", "id": id, "result": r});
    let err = |code: i64, m: String| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}});
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
            let version = if SUPPORTED.contains(&asked) { asked } else { PROTOCOL_VERSION };
            ok(json!({
                "protocolVersion": version,
                // A channel too: in "my session" mode new questions arrive as channel events.
                "capabilities": {"tools": {"listChanged": false}, "experimental": {"claude/channel": {}}},
                "serverInfo": {"name": "gridcraft-agent", "title": "GridCraft agent layer", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
            match call(b, name, &args) {
                Ok(v) => ok(json!({"content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default()}], "isError": false})),
                Err(e) => ok(json!({"content": [{"type": "text", "text": e}], "isError": true})),
            }
        }
        "resources/list" => ok(json!({"resources": []})),
        "prompts/list" => ok(json!({"prompts": []})),
        "logging/setLevel" => ok(json!({})),
        other => err(-32601, format!("method not found: {other}")),
    })
}
