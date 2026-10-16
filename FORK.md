# GridCraft fork: the agent layer

This fork adds a layer of interaction with the user's own agent (Claude Code, Claude Desktop, any
MCP client) inside the spreadsheet, so the person who has to sign off on a model can review it
faster: questions on cells answered in the background, the agent's notes where they apply, the
calculation tree of any number, sensitivity on hover, a privacy gate for client data, and a history
of who changed what.

The fork is built to be **rebased onto upstream often and cleanly**. Read "Keeping the fork alive"
before changing anything outside the agent crates.

## What is where

| Path | What | Upstream? |
| --- | --- | --- |
| `crates/agent/` (`gridcraft-agent`) | The layer: data model, anchors, storage in the `.xlsx`, `agent.*` commands, privacy masking, trace, sensitivity, alternatives and calculation methods (`variants.rs`), worst and best combinations and the risk matrix (`stress.rs`), safe file mode (`safe.rs`), Excel → slide links (`links.rs`), the in-app MCP server | fork only |
| `crates/agent-ui/` (`gridcraft-agent-ui`) | The Agent pane, cell badges, heat maps, hover card with the tornado chart, context-menu items; `examples/agent_snapshot.rs` renders it offscreen | fork only |
| `apps/gridcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`, `--goto`) | fork only |
| `apps/gridcraft-web/src/agent.rs` | Starts the layer in the web app (no server); opens a workbook embedded in an exported page, read-only | fork only |
| `crates/agent/src/export.rs`, `examples/export_html.rs` | `agent.exportHtml`: one HTML file with the web app and the workbook | fork only |
| `agent-layer/` | The agent's skill, the rebase script, `export-web.sh` (builds the web app, exports a page), `demo/build-demo.sh` | fork only |
| `crates/engine/src/ext.rs` | **Hook**: extension commands and command observers | new file in an upstream crate |
| `crates/ui-egui/src/ext.rs` | **Hook**: UI extensions (ribbon tabs, side panel, cell painting, context menu, per-frame) | new file in an upstream crate |
| lines marked `fork:agent` | The only edits to upstream files | patch |

### The patch to upstream files

Every edit to a file that exists upstream carries a `fork:agent` comment. `git grep -n fork:agent`
lists them; there are about twenty lines, almost all insertions:

- `crates/engine/src/lib.rs`: `pub mod ext;`, observers called before and after each command in
  `Session::execute`, extension commands listed in `Session::commands`.
- `crates/engine/src/cmd/mod.rs`: `find_command` also finds extension commands.
- `crates/ui-egui/src/ribbon.rs`: extension tabs added to the tab row, an extension's tab drawn by
  the extension, a match arm so the built-in fallback doesn't draw under it, and extension tabs'
  keytip badges (four lines).
- `crates/ui-egui/src/keytips.rs`: an extension tab's keytip opens it (one match arm).
- `crates/ui-egui/src/lib.rs`: `pub mod ext;`, `ext::frame` and `ext::side_panels` in `SheetApp::ui`,
  and hidden defined names kept out of the Name Box (a fix Excel agrees with).
- `crates/ui-egui/src/grid.rs`: `ext::paint_cells` after the built-in overlays, `ext::context_menu`
  at the end of the cell context menu.
- `apps/gridcraft/src/main.rs`, `apps/gridcraft/Cargo.toml`: wiring (five lines; the first line of
  `main` serves `--agent-stdio` before anything else starts).
- `apps/gridcraft-web/src/main.rs`, `src/web.rs`, `Cargo.toml`: wiring (four lines: the module,
  `agent::start` before the app is built, the two dependencies).
- `Cargo.toml`: the two workspace dependencies.
- `xtask/src/layers.rs`: the two crates in the layer table, and their order within L6.

The hooks know nothing about agents: they are generic extension points, written to be proposed
upstream. Each one accepted upstream removes its lines from the patch.

## Design decisions (and why)

- **Commands, not side channels.** Everything the layer does is an `agent.*` command registered
  through `gridcraft_engine::ext`, so the UI, the control channel, the CLI and MCP reach it like
  any other command, and `Session::execute` keeps catching panics.
- **The layer lives in a per-document store, not in the undo history.** Undoing a cell edit must
  never take away a question or an answer. The layer is written into the workbook only just before
  a save (an observer on `file.save*`).
- **In the file, as a custom XML part** (`urn:gridcraft:agent-layer:v1`, JSON in CDATA). GridCraft
  keeps custom XML parts on open and save (`xlsx/passthrough.rs`), and so does Excel, so the layer
  survives a round trip through Excel.
- **Anchors are hidden defined names** (`_GcAgent_<id>`). The engine and Excel move them with
  inserted and deleted rows and columns. The last known address is kept too: an undo that takes a
  name away gets it back.
- **Privacy is enforced where the agent reads.** The MCP surface has no tool that returns real
  values: every read goes through a copy of the workbook with private constants replaced by
  stand-ins (same sign and magnitude, stable per cell) and everything recalculated from them.
  Writes to private cells are refused. Only a person can lift a privacy mark. Not covered: the
  control port (7979) still reaches real data.
- **Safe file mode: privacy at rest.** With it on, every save (`file.save`, `file.saveAs`,
  `file.saveBytes`) writes the same masked workbook the agent sees (stand-ins, derived values
  recalculated from them) and seals the real values in the layer: AES-256-CTR + HMAC-SHA256
  (the workspace's `aes`/`sha2`, nothing new but `getrandom`) under a random key that never enters
  the file. The key lives in a key folder outside the workbook's (`~/Library/Application
  Support/GridCraft/agent-keys` on macOS, `GRIDCRAFT_AGENT_KEYS` to move it; the macOS keychain
  with `GRIDCRAFT_AGENT_KEYCHAIN=1`). Opening the file where the key is restores the real values;
  anywhere else it opens with the stand-ins, "locked". "Export for standard Excel" writes the real
  values with nothing sealed. The document in memory always keeps the real values: the observer
  swaps a safe copy in for the save only and puts the real workbook back after it (and before the
  next command, should the save have failed). Only a person turns the mode off; in the browser
  there is no key store, so a save there writes stand-ins only. Honest limit: an agent with a
  shell on the same computer as the key folder could read the key; the mode protects the file
  wherever it travels, not against a local agent that ignores its instructions.
- **The MCP server runs inside the app** (`http://127.0.0.1:7980/mcp`, Streamable HTTP, JSON
  responses) and reaches the document through the app's own loopback control channel. Connecting an
  agent is one line of config. `/events` streams new questions, one JSON line each, for an agent's
  background monitor (`curl -sN …/events`).
- **Export to a web page is the app itself, not a report.** `agent.exportHtml {path}` writes one
  HTML file: GridCraft for the web (wasm and its glue, gzipped, base64) and the workbook with its
  layer. A short loader inflates the app in the browser (`DecompressionStream`) and starts it; the
  web app finds the embedded workbook, opens it and makes the layer read-only (`set_read_only`:
  every changing `agent.*` command but `agent.seen` is refused, the pane hides Ask and Mark, the
  Connect tab explains). Prepared answers are kept and shown as new; open questions are dropped.
  No install, no server: it opens from a mail attachment. About 10 MB. The pane uses only
  glyphs the web build's fonts have (no system fonts there).
- **Choices live in the document.** "A or B?" is a set of alternatives on a range (values or
  formulas, as typed), kept in the layer. Switching writes the option with `range.setValues`, an
  ordinary undoable edit, so the model recalculates and undo works; the history records the switch,
  not the cell edit. Which option is active is read from the cells (an edit by hand shows as
  "edited"), never stored. A calculation method is the same thing with two options (simple,
  exact) and a threshold: the difference is shown on the furthest results downstream and flagged
  where it is material. Comparing evaluates every option on a scratch copy. Alternatives are
  refused on private ranges: they would keep real values in the layer, which the agent can read.
- **Excel → slide links** follow the contract DeckCraft keeps in `agent-layer/links.md` (the
  ArtCraft links contract v1). `link.copy` (context menu "Copy Link to Cells", ⌘⌥C, a button in
  the Cell tab) anchors the range with a hidden name `_GcLink_<id>` (moved with rows and columns
  like the agent's anchors, never cleaned up, put back if an undo takes it), gives the workbook a
  stable `workbookId` in the layer, and puts a plain table on the clipboard whose
  `data-artcraft-link` attribute carries the payload (values, displayed text, source). DeckCraft
  asks a running GridCraft for fresh values with `link.read` (control channel), reports uses with
  `link.addUses` and the registry file `ArtCraft/links.jsonl` (read when a workbook opens), and
  opens the source with `link.reveal` or `gridcraft <file> --goto <name>`. The uses are kept in the
  layer, so "▸ 2 slides" and Go to Slide travel with the workbook.
- **The app can bring the agent in itself**, in a mode the app sets (never the file: a workbook
  someone sends must not start an agent that spends the reader's money). Connect tab, or
  `--agent-mode connect|channel|background|off`.
  - *Send to my session* (Claude Code Channels, research preview): the server declares
    `experimental["claude/channel"]` and streams each new question on `GET /mcp` (SSE) as
    `notifications/claude/channel`. Claude Code spawns channel servers over stdio, so
    `gridcraft --agent-stdio` is a bridge: requests to the app's `POST /mcp`, events from its
    `GET /mcp` to stdout. `claude mcp add gridcraft-channel -- <gridcraft> --agent-stdio`, then
    `claude --dangerously-load-development-channels server:gridcraft-channel`. Interactive
    sessions only (`-p` ignores the flag).
  - *Background agent*: one `claude -p --input-format stream-json --output-format stream-json`
    process per document, in the workbook's folder (its session sits with the deal's files in
    Claude Desktop's Code tab), with `--strict-mcp-config` and only the GridCraft server (the
    user's own MCP servers and plugins stay out of its context), `--allowedTools
    "mcp__gridcraft__*"`, the skill as `--append-system-prompt-file`, `--max-turns`,
    `--max-budget-usd`. It is named with `/rename GridCraft · <file>`, gets each new question
    once as a stream-json user message, and its session id is kept in the layer: reopening the
    workbook resumes it (`--resume`); a resume that fails here, or "New session", starts a fresh
    one from a summary of the history. Stop is end of input, then SIGINT. Never `--bare` (it
    needs an API key). The command is configurable (`GRIDCRAFT_AGENT_COMMAND`). Desktop only.
- **The Agent tab on the ribbon holds what isn't about one cell**: the pane with what waits
  (answers ready, questions with the agent), client data (mark, safe file, export for Excel), the
  connection (server status, whether an agent is connected, the mode, New session, Copy MCP
  command, Connect Cowork, Open Claude), the heat maps, sharing (link for a slide, web page). The
  pane keeps only the content: the cell's questions and notes, the trace, sensitivity,
  alternatives, risk, history. Group names match WordCraft and DeckCraft (Agent, Client data,
  Connection, View, Share). Agent: one big button with the agent's mark and "1 ready" / "1 in
  work" / "2 waiting" / "idle". Connection holds three things only: the status, the mode and a
  Copy menu. The status answers "is anyone listening?": "Agent answering · checked 12 s ago"
  (green: a check answered), "Listener connected · not checked" (amber: an `/events` stream, a
  Claude Code channel stream, `wait_for_questions` polled in the last minute, the background
  agent, or recent tool calls), "Nobody is listening" (grey), "No answer to check" (red: a check
  unanswered for 60 s). **Check** sends `agent.ping`: a system question of kind `ping` ("Is
  anyone there?"), sent out on every path (list/wait_for_questions, `/events`, the channel, the
  background agent) but never shown as a question, badged, logged or saved; the agent answers
  "here". The mode is a list: I connect my agent (MCP), Send to my Claude Code session
  (channels), Background agent, Off (tools refuse). Copy holds one thing per item, by mode (MCP
  command, monitor command, Connect Cowork…; the two channel steps; for the background agent,
  open the session in Claude Desktop and New session), and Details… (URL, streams, session,
  last error). Keytip G (Alt, G; F10, G on macOS): A is Data, as in Excel. The floating Agent button shows only while the ribbon is collapsed.
- **Cowork and Claude Desktop** run local servers over stdio: `gridcraft --agent-stdio [--control
  127.0.0.1:7979]`, handled on the first line of `main` (no window, nothing but protocol on
  stdout), speaks MCP on stdin/stdout and exits when stdin closes. It reaches the app through the
  control channel, connected on the first tool call, so Claude can start first; while GridCraft is
  closed, `initialize` and `tools/list` work and tools say it isn't reachable. **Connect Cowork** writes `"gridcraft": {"command": <this app>, "args": ["--agent-stdio",
  "--control", "127.0.0.1:7979"]}` into `claude_desktop_config.json` (backup `.bak-gridcraft-<ms>`,
  temp file + rename, nothing else touched, a non-JSON file refused). Claude Desktop keeps that file
  in memory and writes it back when it quits, so while it runs GridCraft waits in the background
  for it to quit (`ps -axo comm=`) and writes 1.5 s after.
- **Analysis is deterministic.** The trace and the sensitivity are computed by the engine (the
  dependency graph, recalculation of a scratch copy). The agent explains them; it doesn't make them up.
- **Several inputs at once, bounded.** The worst and best combination moves 3–12 inputs inside
  their reasoned ranges: one at a time first (the no-interaction estimate), then every corner in
  Gray-code order (one input changes per step, so each recalculation is incremental) when they fit
  in the budget, else a fixed sample of corners; then fixed points inside the box, then a
  coordinate search from the two extremes. The work is capped by evaluations (3,000 by default,
  20,000 at most) and by evaluations × cells under the inputs, so a big model gets fewer.

## Running it

```bash
cargo run --release -p gridcraft
```

The agent server starts with the app (and the control channel on 7979 with it). Connect Claude Code:

```bash
claude mcp add --transport http gridcraft http://127.0.0.1:7980/mcp
```

and give it the skill in `agent-layer/skill/`. Export a workbook as a web page (builds the web app
into `target/agent-web` first; needs the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` at
the version in `Cargo.lock`):

```bash
agent-layer/export-web.sh book.xlsx book.html
```

In the app: Agent pane › Connect › Export web page (writes `book.html` beside the workbook), or
`agent.exportHtml {"path": "book.html"}` through the control channel or MCP. Offscreen render with the layer:

```bash
cargo run --release -p gridcraft-agent-ui --example agent_snapshot -- --in book.xlsx --pane cell --hover B23 out.png
```

## Keeping the fork alive

```bash
agent-layer/rebase.sh
```

The script fetches `upstream`, rebases the current branch onto `upstream/main`, and checks the
result: every `fork:agent` marker is still there, then fmt, clippy, the agent tests, the engine
and UI tests, layers and wasm. When it stops on a conflict, the conflict can only be in one of the
marked lines above. Resolve it by keeping upstream's version and putting the marked line back.

Rules that keep it this way:

1. New behaviour goes in `crates/agent*` or behind a hook, never into an upstream file.
2. Need a new hook? Make it generic (no agent vocabulary), put it in the crate's `ext.rs`, call it
   with one marked line, list it above.
3. Use public engine and UI APIs. When upstream renames one, the build breaks in `crates/agent*`
   with a clear error instead of a conflict.
4. Commits: hooks and wiring in their own commits, separate from the agent crates.

## Status

Prototype. Done for GridCraft: question queue with live badges, agent notes with links, privacy
gate, the calculation tree, sensitivity with reasoned ranges (tornado on hover, Impact heat map),
the confidence heat map, decision history and audit trail, in-app MCP and event stream, storage
in the `.xlsx`; export to a self-contained web page (WASM, prepared answers only, read-only); alternatives in the document ("A or B?", Variants tab, switch and recalculate) and
calculation methods side by side (simple vs exact, material differences flagged on the results);
multi-dimensional analysis (the worst and best combination of an output's inputs, against the
single moves added up); the risk matrix (each input's impact on an output against the agent's
confidence in it, Risk tab); safe file mode (client data saved as stand-ins, real values sealed
under a key kept on this computer; export for standard Excel); GridCraft's side of Excel → slide
links (copy as linked source, `link.read`, "used on slides" with Go to Slide, `--goto`); the app
starting the agent itself (into the user's own Claude Code session as channel events, or as a
background agent per workbook). `agent-layer/demo/build-demo.sh`
builds a demo workbook with all of it and renders each feature.

Next: WordCraft and DeckCraft are built the same way in their own forks (anton-belkin/wordcraft,
anton-belkin/deckcraft, branch agent-layer).
