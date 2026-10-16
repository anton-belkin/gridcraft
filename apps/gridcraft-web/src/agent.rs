//! fork:agent — the agent layer in the browser: the `agent.*` commands, badges, heat maps and the
//! Agent pane (no MCP server: an agent can't reach a page).
//!
//! A page exported with `agent.exportHtml` carries its workbook in
//! `<script id="gridcraft_workbook" data-name="…">` (base64). It opens on start with the layer
//! read-only and the Agent pane showing the prepared answers.
#![cfg(target_arch = "wasm32")]

use gridcraft_engine::Session;
use serde_json::json;

/// Registers the agent layer and opens the workbook embedded in the page, if any.
pub fn start(session: &mut Session) {
    gridcraft_agent_ui::install();
    let Some(el) = web_sys::window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(gridcraft_agent::export::WORKBOOK_ELEMENT)) else {
        return;
    };
    let name = el.get_attribute("data-name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "Workbook.xlsx".to_string());
    let base64 = el.text_content().unwrap_or_default();
    // The page keeps no second copy of the workbook.
    el.remove();
    gridcraft_agent::set_read_only(true);
    if let Err(e) = session.execute("file.open", json!({"name": name, "base64": base64.trim()})) {
        log::error!("couldn't open the workbook in this page: {e}");
        return;
    }
    if let Some(d) = session.active_mut() {
        // An exported page is a copy to read: nothing to save on leaving.
        d.saved = d.wb.clone();
    }
    let answers = gridcraft_agent::active_layer(session).is_some_and(|l| !l.questions.is_empty());
    gridcraft_agent_ui::request_pane(true, if answers { "questions" } else { "notes" }, "off");
}
