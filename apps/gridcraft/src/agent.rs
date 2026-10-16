//! fork:agent — starts the agent layer: the `agent.*` commands, the Agent pane and the in-app MCP
//! server (`http://127.0.0.1:7980/mcp`), which reaches the document through the control channel.
//!
//! `--agent-port <port>` (or `GRIDCRAFT_AGENT_PORT`) picks the port, `--no-agent` turns it off,
//! `--goto <name | Sheet!A1>` selects a place once the workbook is open.
//! With the agent on, the control channel starts too (on 7979 unless `--control` says otherwise).

/// Default port of the agent server.
const AGENT_PORT: u16 = 7980;
/// Control port used for the agent when none was asked for.
const CONTROL_PORT: u16 = 7979;

/// Takes the agent's own arguments out of the command line; returns the rest and the port.
pub fn take_args(args: Vec<String>) -> (Vec<String>, Option<u16>) {
    let mut port = std::env::var("GRIDCRAFT_AGENT_PORT").ok().and_then(|p| p.parse().ok()).or(Some(AGENT_PORT));
    let mut rest = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agent-port" => port = it.next().and_then(|p| p.parse().ok()),
            "--no-agent" => port = None,
            // Open the workbook at a defined name or a place (a slide's "Open source").
            "--goto" => {
                if let Some(t) = it.next() {
                    gridcraft_agent_ui::set_goto(t);
                }
            }
            _ => rest.push(a),
        }
    }
    (rest, port)
}

/// The control port: the agent server needs one.
pub fn control_port(agent: Option<u16>, control: Option<u16>) -> Option<u16> {
    if agent.is_some() { control.or(Some(CONTROL_PORT)) } else { control }
}

/// Registers the agent layer and starts its server (on the UI thread, before the first frame).
pub fn start(agent: Option<u16>, control: Option<u16>) {
    let (Some(port), Some(control)) = (agent, control) else { return };
    gridcraft_agent_ui::install();
    match gridcraft_agent::server::start(port, format!("127.0.0.1:{control}")) {
        Ok(port) => gridcraft_agent_ui::set_server_url(format!("http://127.0.0.1:{port}/mcp")),
        Err(e) => log::error!("agent server on port {port}: {e}"),
    }
}
