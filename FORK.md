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
| `crates/agent/` (`gridcraft-agent`) | The layer: data model, anchors, storage in the `.xlsx`, `agent.*` commands, privacy masking, trace, sensitivity, the in-app MCP server | fork only |
| `crates/agent-ui/` (`gridcraft-agent-ui`) | The Agent pane, cell badges, heat maps, hover card with the tornado chart, context-menu items; `examples/agent_snapshot.rs` renders it offscreen | fork only |
| `apps/gridcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`) | fork only |
| `apps/gridcraft-web/src/agent.rs` | Starts the layer in the web app (no server); opens a workbook embedded in an exported page, read-only | fork only |
| `crates/agent/src/export.rs`, `examples/export_html.rs` | `agent.exportHtml`: one HTML file with the web app and the workbook | fork only |
| `agent-layer/` | The agent's skill, the rebase script, `export-web.sh` (builds the web app, exports a page) | fork only |
| `crates/engine/src/ext.rs` | **Hook**: extension commands and command observers | new file in an upstream crate |
| `crates/ui-egui/src/ext.rs` | **Hook**: UI extensions (side panel, cell painting, context menu, per-frame) | new file in an upstream crate |
| lines marked `fork:agent` | The only edits to upstream files | patch |

### The patch to upstream files

Every edit to a file that exists upstream carries a `fork:agent` comment. `git grep -n fork:agent`
lists them; there are about twenty lines, almost all insertions:

- `crates/engine/src/lib.rs`: `pub mod ext;`, observers called before and after each command in
  `Session::execute`, extension commands listed in `Session::commands`.
- `crates/engine/src/cmd/mod.rs`: `find_command` also finds extension commands.
- `crates/ui-egui/src/lib.rs`: `pub mod ext;`, `ext::frame` and `ext::side_panels` in `SheetApp::ui`,
  and hidden defined names kept out of the Name Box (a fix Excel agrees with).
- `crates/ui-egui/src/grid.rs`: `ext::paint_cells` after the built-in overlays, `ext::context_menu`
  at the end of the cell context menu.
- `apps/gridcraft/src/main.rs`, `apps/gridcraft/Cargo.toml`: wiring (four lines).
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
  Writes to private cells are refused. Only a person can lift a privacy mark. Not yet covered:
  the control port (7979) and the `.xlsx` on disk still hold real data. The "safe file" mode in
  the concept is the next step.
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
- **Analysis is deterministic.** The trace and the sensitivity are computed by the engine (the
  dependency graph, recalculation of a scratch copy). The agent explains them; it doesn't make them up.

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
in the `.xlsx`, export to a self-contained web page (WASM, prepared answers only, read-only).

Next: alternative formulas
side by side, multi-dimensional sensitivity, a safe file mode for privacy, then WordCraft and
DeckCraft (Excel → slide links with a back-reference in the workbook), built the same way.
