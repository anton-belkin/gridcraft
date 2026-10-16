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
| `crates/agent/` (`gridcraft-agent`) | The layer: data model, anchors, storage in the `.xlsx`, `agent.*` commands, privacy masking, trace, sensitivity, alternatives and calculation methods (`variants.rs`), worst and best combinations (`stress.rs`), the in-app MCP server | fork only |
| `crates/agent-ui/` (`gridcraft-agent-ui`) | The Agent pane, cell badges, heat maps, hover card with the tornado chart, context-menu items; `examples/agent_snapshot.rs` renders it offscreen | fork only |
| `apps/gridcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`) | fork only |
| `agent-layer/` | The agent's skill, the rebase script | fork only |
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
- **Choices live in the document.** "A or B?" is a set of alternatives on a range (values or
  formulas, as typed), kept in the layer. Switching writes the option with `range.setValues`, an
  ordinary undoable edit, so the model recalculates and undo works; the history records the switch,
  not the cell edit. Which option is active is read from the cells (an edit by hand shows as
  "edited"), never stored. A calculation method is the same thing with two options (simple,
  exact) and a threshold: the difference is shown on the furthest results downstream and flagged
  where it is material. Comparing evaluates every option on a scratch copy. Alternatives are
  refused on private ranges: they would keep real values in the layer, which the agent can read.
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

and give it the skill in `agent-layer/skill/`. Offscreen render with the layer:

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
in the `.xlsx`; alternatives in the document ("A or B?", Variants tab, switch and recalculate) and
calculation methods side by side (simple vs exact, material differences flagged on the results);
multi-dimensional analysis (the worst and best combination of an output's inputs, against the
single moves added up); the risk matrix (each input's impact on an output against the agent's
confidence in it, Risk tab).

Next: export to a self-contained web page (WASM, prepared answers only), a safe file mode for privacy, then WordCraft and DeckCraft (Excel → slide links
with a back-reference in the workbook), built the same way.
