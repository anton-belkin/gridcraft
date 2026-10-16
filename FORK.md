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
| `crates/agent-ui/` (`gridcraft-agent-ui`) | The Agent pane, cell badges, heat maps, hover card with the tornado chart, context-menu items; voice questions (`voice.rs`, native only); `examples/agent_snapshot.rs` renders it offscreen | fork only |
| [`anton-belkin/craft-voice`](https://github.com/anton-belkin/craft-voice) (git dependency of `gridcraft-agent-ui`) | Microphone capture, local Whisper (candle), model download, the splice of pointing into the transcript; shared with WordCraft and DeckCraft | separate repo |
| `apps/gridcraft/src/agent.rs` | Starts the layer in the desktop app (`--agent-port`, `--no-agent`, `--goto`) | fork only |
| `apps/gridcraft-web/src/agent.rs` | Starts the layer in the web app (no server); opens a workbook embedded in an exported page, read-only | fork only |
| `crates/agent/src/export.rs`, `examples/export_html.rs` | `agent.exportHtml`: one HTML file with the web app and the workbook | fork only |
| `agent-layer/` | The agent's skill, the rebase script, `export-web.sh` (builds the web app, exports a page), `demo/build-demo.sh` | fork only |
| `crates/engine/src/ext.rs` | **Hook**: extension commands, command observers, custom undo steps | new file in an upstream crate |
| `crates/ui-egui/src/ext.rs` | **Hook**: UI extensions (ribbon tabs, side panel, cell painting, context menu, per-frame) | new file in an upstream crate |
| lines marked `fork:agent` | The only edits to upstream files | patch |

### The patch to upstream files

Every edit to a file that exists upstream carries a `fork:agent` comment. `git grep -n fork:agent`
lists them; there are about twenty lines, almost all insertions:

- `crates/engine/src/lib.rs`: `pub mod ext;`, observers called before and after each command in
  `Session::execute`, extension commands listed in `Session::commands`.
- `crates/engine/src/cmd/mod.rs`: `find_command` also finds extension commands.
- `crates/engine/src/lib.rs` (`HistoryEntry::step`) and `crates/engine/src/cmd/edit.rs` (undo and
  redo): an extension's own undo steps, interleaved with the workbook snapshots
  (`ext::push_undo_step`); a step entry keeps the workbook as it is and runs the step.
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
- `packaging/macos/Info.plist.in`: `NSMicrophoneUsageDescription` (one line); `packaging/macos/entitlements.plist`:
  `com.apple.security.device.audio-input`, which the hardened runtime needs for the microphone (one line).
- `Cargo.lock`: an entry for `zip 8.6.0` (see "Voice questions"); `rebase.sh` puts it back after
  taking upstream's lock.

The hooks know nothing about agents: they are generic extension points, written to be proposed
upstream. Each one accepted upstream removes its lines from the patch.

## Design decisions (and why)

- **Commands, not side channels.** Everything the layer does is an `agent.*` command registered
  through `gridcraft_engine::ext`, so the UI, the control channel, the CLI and MCP reach it like
  any other command, and `Session::execute` keeps catching panics.
- **The layer lives in a per-document store, not in the document's snapshots.** Undoing a cell
  edit must never take away a question or an answer. The layer is written into the workbook only
  just before a save (an observer on `file.save*`). What a *person* does with the layer (ask, set a
  status, mark client data, remove a note, reject a place, safe file, input ranges) is still on
  their ⌘Z: a custom undo step (`ext::push_undo_step`, the engine hook below) sits in the same
  history as their edits and restores only its part of the layer. The agent's own answers, notes
  and places are not on the person's stack; each has a Remove or Reject that is.
- **In the file, as a custom XML part** (`urn:gridcraft:agent-layer:v1`, JSON in CDATA, `version` 2 since Topics; a version 1 layer
  becomes topics on load). GridCraft
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
- **The app can bring the agent in itself**, in the mode the document's Context says (below), or
  `--agent-mode connect|channel|background|off`. A workbook someone sends must not start an
  agent that spends the reader's money: the background client starts for a document only after
  the person asked a question in it, chose its context or started it in this run
  (`runner::arm`), whatever context the file carries.
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
  context (what the agent knows), the heat maps, sharing (link for a slide, web page). Group names
  match WordCraft and DeckCraft (Agent, Client data, Context, View, Share). Agent: one big button
  with the agent's mark and the sign-off count. Keytip G (Alt, G; F10, G on macOS): A is Data, as
  in Excel. The floating Agent button shows only while the ribbon is collapsed.
- **Context: what the agent knows** (the brief shared with WordCraft and DeckCraft, Anton
  2026-10-16; `context.rs` in both crates). A large button shows the mode, with the Ready dot on
  its sign; its menu has three rows, each a name, one line and chips: **No context** ("Only this
  document. Runs in the background." [Document]: the background client in its neutral folder),
  **Claude Cowork** ("Your Cowork chat, its project and memory." [Document][Your chat]: AItoCraft
  and the `wait_for_questions` loop), **Claude Code** ("Your Claude Code session or a project
  folder." [Document][Project folder]). Beside it, by mode: No context has **☐ Web** (the
  background client is also allowed WebSearch and WebFetch), the Ready lamp, and **Get ready ▾**
  only while `claude` isn't logged in (one step: Log in to Claude, a terminal with `claude auth
  login`). Cowork: the lamp, Get ready with Install AItoCraft (`aitocraft --install-desktop`) and
  Ask Cowork to collaborate (copies the phrase). Claude Code: **My session | Project folder**, the
  lamp, Get ready: My session installs AItoCraft (`claude mcp add --scope user aitocraft -- <hub>`,
  done when `claude mcp get aitocraft` succeeds) and copies the command that starts claude with
  the channel; Project folder chooses a folder (shown beside it; the background client runs
  there and sees its CLAUDE.md, memory and files). Get ready's main click runs the first step not
  done; its menu ticks the done ones.
  - *The lamp*: green Ready (a listener attached to this document recently or a check answered;
    or the background client is usable: `claude` logged in, the folder there), amber Waiting
    (after a Get ready step until a listener attaches, or while something listens that hasn't
    answered), grey Not ready. The app checks by itself: `claude auth status`, `claude mcp get`
    and Claude Desktop's configuration in the background (once a minute, every 5 s while
    waiting), and when something listens without having answered it sends `agent.ping` quietly
    (at most every 2 minutes). The ping stays an internal mechanism: no button, no wording.
  - *Per document*: `agent.agentContext {kind, via, web, folder}` keeps the choice in the layer
    (`context`), logged and undoable as a person's action (`Part::Context`); a document without
    one follows the last choice made in the app (`<settings>/agent/context.json`, without a
    folder). `context::sync` turns the active document's choice into the runner's mode, folder
    and web permission each frame (and after every change or undo); a changed folder or web
    permission restarts that document's client. `agent.agentMode` maps onto it. The old status
    line, Check, Mode list and Copy menu are gone; the direct stdio connection to Claude Desktop
    stays as the `agent.connectCowork` command.
- **Cowork and Claude Desktop** run local servers over stdio: `gridcraft --agent-stdio [--control
  127.0.0.1:7979]`, handled on the first line of `main` (no window, nothing but protocol on
  stdout), speaks MCP on stdin/stdout and exits when stdin closes. It reaches the app through the
  control channel, connected on the first tool call, so Claude can start first; while GridCraft is
  closed, `initialize` and `tools/list` work and tools say it isn't reachable. **Connect Cowork** writes `"gridcraft": {"command": <this app>, "args": ["--agent-stdio",
  "--control", "127.0.0.1:7979"]}` into `claude_desktop_config.json` (backup `.bak-gridcraft-<ms>`,
  temp file + rename, nothing else touched, a non-JSON file refused). Claude Desktop keeps that file
  in memory and writes it back when it quits, so while it runs GridCraft waits in the background
  for it to quit (`ps -axo comm=`) and writes 1.5 s after.
- **AItoCraft: one connection for every document of every Craft app** (contract:
  `~/aitocraft/PROTOCOL.md`, v1). GridCraft announces itself in
  `ArtCraft/agents/gridcraft-<pid>.json` (`registry.rs`: written atomically on changes, at most once
  a second, a 10 s heartbeat, removed when the window closes; stale entries of crashed instances
  swept; titles and paths only, no content). Each document carries `focusedAt` (when its window last had focus with it active; the hub
  picks "the open file" by it). Documents have ids `gridcraft:<pid>:<doc uid>`. Every
  MCP tool takes `document`: `agent.onDocument` runs the command with that document active for the
  length of the command and puts the active one back (control-channel commands run between frames,
  so the switch is never drawn; no engine hook needed). `list_documents`, `attach {label}` →
  listener, `detach`; question tools take `listener` (refreshed on every call, dropped after 90 s);
  questions carry `document` and `documentTitle`; `/events?document=` (and `&listener=`) streams
  one document. The Connection group shows the active document's listener ("Listening: Cowork ·
  Acme · 20 s ago"), the check's result with who answered, or "Nobody is listening for this
  document". Modes: Claude Desktop / Cowork (AItoCraft), My Claude Code session (AItoCraft
  channel), Background agent, Off; the hub's items (phrase for Claude, Install AItoCraft in Claude
  Desktop, Claude Code: add AItoCraft), the per-app `--agent-stdio` kept under Details. The
  background agent runs in the neutral `ArtCraft/agent-sessions/` unless "use the document's folder
  as its project" is on.
- **Shared details with WordCraft and DeckCraft**: ping ids are `ping<ms>`, and an unanswered
  ping leaves `agent.questions` after 60 s (the status still says "No answer to check");
  `wait_for_questions` polls with `waiting: true` (a listener for 60 s), streams and the runner
  with `poll: true`, and neither counts as a tool call; the background agent is started with
  `--name "GridCraft · <file>"`, and a new session gets the history seed only when the log has
  something; Off refuses every agent-side command ("the agent connection is off in GridCraft
  (Agent tab ▸ Connection ▸ Mode)") while `initialize` and `tools/list` still answer; the mode is
  remembered in `<settings>/agent/mode`, and `--agent-mode` overrides it.
- **Topics (v1, the shared model in `aitocraft/TOPICS.md`).** The unit is a topic: places
  (anchors with the agent's reasons), a status (Open, Revise, Agreed, For information, with text,
  who and when), notes, and the discussion (its questions and answers). Questions and notes stay in
  their own lists with a `topic` id, so the existing question flow (claim, answer, events, voice)
  didn't change shape. Topics are numbered like footnotes, by their first place in sheet order, and
  places within a topic (`3.1`, `3.2`); numbers are computed, never stored. Only a person sets
  Agreed: the agent proposes it with reasons and the person accepts with one click
  (`agent.proposeAgreed`, `agent.acceptProposal`); every status change is in the log with who made
  it. MCP: `list_topics`, `get_topic`, `add_places`, `widen_place`, `set_status`, `propose_agreed`;
  `add_note` takes a topic. The web export drops the discussion unless "Include Discussion" is on.
  - *Links inside text* (`textlinks.rs`). Answers, notes, status text, reasons and questions are
    stored with `[label](anchor:<id>)` links; a `[B1](cell:Sheet1!B1)` the agent writes, and a
    plain unambiguous `C3:D9`, `Sheet2!B4` or a lone `B1` that holds something, becomes one when
    the text is stored. A label that was the address is stored as `@` and shown as the current
    address, so an answer that said C4 says C5 after a row insert, with nothing marking it.
  - *Whereabouts of deleted places.* An observer before `home.deleteRows`, `deleteColumns`,
    `deleteCells` and `deleteSheet` anchors the neighbours (the row above and below, the column left
    and right) of every anchor the delete takes; the place then reads "removed (was C10, between C9
    and C10)" with live links, struck through in its topic. Undo of the delete brings the anchor
    back. Cut and paste move a place (the engine moves defined names); copy and fill don't
    duplicate it (they don't copy names).
  - *Undo of a question*: not yet taken, it goes with its topic; being worked on, it's withdrawn
    (the agent gets a `withdrawn` event and `answer_question` says to drop it); answered, it's
    hidden with its answer. Redo sends it again, or shows it as it was.
  - *Markers*: a numbered pill per place in the top-right cell's top-right corner, in the status
    colour, blue and pulsing while the agent works, a dot for an unread answer; several stack
    leftwards. They never cover the value: measured with the cell's font, the stack goes left of a
    right-aligned value when the corner is taken, and a cell with no room left gets a corner flag
    (the numbers are on hover and in the pane). Clicking one opens the topic.
  - *The pane*: Selection (the cell, the topics touching it as cards, and folded below them where
    the number comes from, sensitivity, worst and best case, alternatives, risk; the question box at
    the bottom, adding to the selection's topic or starting a new one), Overview (sign-off "Agreed
    3 of 5 · 1 revise · 1 open", filters, every topic; also every set of alternatives), History.
    The ribbon's Agent button shows the sign-off count.
  - Deferred (TOPICS.md §6): nested places.
- **Proposals instead of direct edits** (`proposals.rs`; brief part B, the same MCP in all three
  apps). In a topic the agent doesn't change the workbook: `run_command {topic, option?}` runs the
  command on a draft (a copy of the accepted workbook in a scratch session, recalculated),
  refuses it if it doesn't run, and records it in the topic's proposal (`Topic.edits`, saved as
  `proposal`; options A–H, up to 200 commands each). `describe_option` names an option and says
  why; `discard_proposal` drops one or all. Reads of the accepted workbook mark cells a pending
  proposal changes (`pending: [{topic, option, summary}]`), `read_range {view: "proposal"}` reads
  a draft, topics show `proposal: {options: [{option, title, why, changes}], state}`, and pending
  and working questions carry `pendingNearby`. The person sees the option on the sheet (old → new
  on edited cells, lighter on results that recalculate), switches options on the card, and
  applies one: the commands are replayed on the workbook with their own history entries set
  aside, and one custom undo step holds the workbook before and after plus the layer (the
  proposal applied, a decision note "chose B: …", overlapping pending proposals out of date). A
  step that doesn't fit restores the workbook and marks the proposal out of date ("out of date,
  ask the agent to redo"). Reject takes an optional comment, which also goes to the agent as a
  follow-up question in the topic. Events `proposal_applied`, `proposal_rejected` (with the
  comment) and `proposal_outdated` on `/events` and the channel. Drafts are cached per workbook
  version; proposals stay home when the workbook is sent, unless the discussion goes too.
- **Pane and topics** (brief part E).
  - *The active cell inside a place lights up its topic*: every place of the topic outlined in its
    status colour, its markers ringed, its card outlined on the Selection tab (no click needed).
  - *The pane's text is one point larger* than the app's.
  - *The file's own context* (`filecontext.rs`, `Layer.file_context`, saved as `fileContext`):
    entries with a title, a kind (about, audience, house rules, background, fact), text with links
    inside, who added each and when. MCP `get_file_context`, `add_file_context` → id,
    `update_file_context`, `remove_file_context`; `get_context` and every question carry a short
    summary (`fileContext`). Kept in the file and when it is sent. A **File context** button beside
    the pane's tabs shows it; the person adds, edits and removes entries there, each an undo step
    (`Part::FileContext`); the agent's aren't on the person's stack.
  - *Hovering a link in the pane highlights its target* on the sheet, without moving the
    selection or the view: a cell or range (outlined), a topic's places (`topic:`), a place
    (`place:`); a removed place outlines the neighbours it was between. A target off screen or on
    another sheet gets an edge hint ("↓ C40", "→ Sheet2!B4").
- **Many cells in one call** (`setranges.rs`): `set_ranges {ranges: [{range, values?, formulas?,
  format?}], reason, source?, topic?, option?}` is the command `agent.setRanges`. Values and
  formulas are 2-D arrays matching each range (one value fills it; a value that starts with `=` is
  text); `format` maps onto `home.*` (bold, italic, underline, strikethrough, wrap, numberFormat,
  fill, fontColor, fontSize, fontName, align, valign). The whole call is checked first (ranges,
  shapes, formats, at most 200 ranges and 100,000 cells, and the privacy gate, which refuses the
  whole call and names the client-data cells), then runs the `range.setValues` / `home.*`
  commands quietly with their history entries set aside: one undo step "(agent): set N ranges",
  one history entry with the reason and source. With `topic` it is one step of the proposal.
  Commands that write their own entry (`agent.setRanges`, `agent.applyProposal`) aren't logged
  again by the observer.
- **Latency of the agent path.** A command from the agent reaches the UI thread at once (the
  control server wakes egui); with the window visible it waits at most one frame (≈17 ms at 60 Hz),
  hidden or idle under a millisecond. Two things made it slow: a draft of a proposal was built
  from a fresh `DocState`, which recalculates the whole workbook (0.4–1.3 s per `run_command
  {topic}` on a 5,000-row model, and the pane rebuilt it after every edit), and the registry's
  heartbeat came from UI frames, so an idle or hidden window went stale after 30 s and the hub
  stopped listing GridCraft. Drafts now start from the accepted document's calculated state (only
  the steps recalculate: 2 ms per call, 20 ms per rebuild), and the heartbeat has its own thread.
- **Analysis is deterministic.** The trace and the sensitivity are computed by the engine (the
  dependency graph, recalculation of a scratch copy). The agent explains them; it doesn't make them up.
- **Several inputs at once, bounded.** The worst and best combination moves 3–12 inputs inside
  their reasoned ranges: one at a time first (the no-interaction estimate), then every corner in
  Gray-code order (one input changes per step, so each recalculation is incremental) when they fit
  in the budget, else a fixed sample of corners; then fixed points inside the box, then a
  coordinate search from the two extremes. The work is capped by evaluations (3,000 by default,
  20,000 at most) and by evaluations × cells under the inputs, so a big model gets fewer.

- **Voice questions: the reader's rhythm, by voice.** Hold ⌥ Option (Alt elsewhere) for a
  moment, or click the microphone on the Agent tab, speak, and click the cells you mean while
  speaking; release (or click again) and it is asked. No preview, no confirmation, nothing to wait
  for: the question goes on the cells at once with a "transcribing" flag (status `transcribing`,
  anchors on the selection and on every cell pointed at, so they move with edits), Whisper
  transcribes it on a worker thread, and `agent.spoken` puts the text in (status `pending`): from
  then on it is an ordinary question (`/events`, the channel, the background agent). A failed
  transcription is a red flag with Retry and Dismiss, never a dialog. Esc cancels a recording.
  - *Pointing is not interpreted.* Every click and selection change while recording is a pointer
    at the moment it began; `craft_voice::splice` puts `⟦n⟧` into the transcript where the reader
    was in their speech (Whisper's segment timestamps, interpolated over the segment's characters,
    rounded to a word boundary; a click in silence goes to the nearer end of the speech). No
    matching of "this"/"это": the agent resolves it. The question keeps `refs: [{n, anchor}]`
    beside its main `anchor` (old files load as before). Agent-facing reads expand the markers:
    `⟦1: Sheet1!C16 · Revenue · FY2028 = 1,234⟧`, values masked like every other read; the raw
    text is `spokenText`. The skill has one line about it. A click's place is read a frame after
    the button comes up (the extension hook runs before the grid handles the frame's input, so
    reading it at once gave the cell selected before), a drag counts once with its final range,
    and the same place pointed at again within a second adds no marker. Each transcription is
    logged with its segments' times and the pointers' times (text only), so a marker that lands
    in the wrong place can be explained afterwards.
  - *Drafts never reach the file or the agent.* `transcribing` and `failed` questions are kept out
    of `agent.questions` (and so out of MCP, `/events`, the channel and the background agent) and
    out of the saved layer.
  - *Audio stays in memory*, in the UI crate only (the engine never sees it): from key-down until
    the text is in, or the reader dismisses it; then it is dropped. Nothing is written to disk and
    nothing leaves the machine but the one-time model download.
  - *The model is the reader's choice on the Agent tab*, off by default: Whisper small (q5_1,
    190 MB) or large-v3-turbo (q5_0, 574 MB, labelled slower). An app setting (`voice.json` beside
    `agent-keys`; `GRIDCRAFT_VOICE_SETTINGS` moves it), never the workbook's. Picking a model that
    isn't downloaded downloads it now, with progress on the ribbon; a downloaded one loads when the
    reader starts speaking (load and warm-up take 1–3 s, less than the question), and is let go
    after 15 minutes without a question. `agent.voiceModel {model}` and `agent.voiceStatus` do the
    same through the control channel and MCP. `GRIDCRAFT_VOICE_LANGUAGES=ru,en` limits language
    detection.
  - *The models* come from Hugging Face, [`ggerganov/whisper.cpp`](https://huggingface.co/ggerganov/whisper.cpp)
    at revision `5359861c`, `ggml-small-q5_1.bin` and `ggml-large-v3-turbo-q5_0.bin`: OpenAI's
    Whisper weights (MIT) as converted by whisper.cpp (MIT). Checked against their SHA-256,
    converted once into GGUF, kept in `~/Library/Application Support/ArtCraft/voice-models` (shared
    by the ArtCraft apps; `CRAFT_VOICE_MODELS` moves it). Not assets of this repository: nothing is
    committed, so they have no ATTRIBUTION.md row.
  - *Microphone permission.* The bundled app asks with `NSMicrophoneUsageDescription`, and the
    hardened runtime needs the `audio-input` entitlement (both in `packaging/macos`). A binary run
    with `cargo run` has no Info.plist: macOS asks on behalf of the app that started it (Terminal,
    iTerm, VS Code…), which needs microphone access under Privacy & Security; a denied microphone
    delivers silence, which GridCraft reports as "is microphone access allowed?".
  - *Dependency note.* Every published candle depends on `zip` 7.x/8.x and crates.io has yanked
    every `zip` before 9.0.3. Cargo uses a yanked version only when `Cargo.lock` already names it,
    so the lock carries `zip 8.6.0` (used by candle only to read `.npz`, which nothing here does).
    On Linux, `cpal` needs ALSA headers (`libasound2-dev`).
  - *Speed* on a MacBook Pro M1 Pro (candle on Metal, median of 3, after warm-up): small 1.1–1.5 s
    for 5 s of speech, 2.8–3.4 s for 15 s; turbo 2.1–2.2 s and 3.0–3.5 s. Memory while loaded:
    0.65 GB and 1.3 GB.

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

Prototype. Done for GridCraft: topics (TOPICS.md v1: places with whereabouts, statuses with
sign-off, links inside text that follow the cells, numbered markers, Selection / Overview pane,
person actions on ⌘Z, layer format v2), the Context group (what the agent knows, per document,
with the Ready lamp and Get ready), proposals instead of direct edits (options, preview, apply as
one step), the file's own context, link hover, question queue with live markers, agent notes with links, privacy
gate, the calculation tree, sensitivity with reasoned ranges (tornado on hover, Impact heat map),
the confidence heat map, decision history and audit trail, in-app MCP and event stream, storage
in the `.xlsx`; export to a self-contained web page (WASM, prepared answers only, read-only); alternatives in the document ("A or B?", switch and recalculate) and
calculation methods side by side (simple vs exact, material differences flagged on the results);
multi-dimensional analysis (the worst and best combination of an output's inputs, against the
single moves added up); the risk matrix (each input's impact on an output against the agent's
confidence in it); safe file mode (client data saved as stand-ins, real values sealed
under a key kept on this computer; export for standard Excel); GridCraft's side of Excel → slide
links (copy as linked source, `link.read`, "used on slides" with Go to Slide, `--goto`); the app
starting the agent itself (into the user's own Claude Code session as channel events, or as a
background agent per workbook); voice questions with pointing (hold ⌥, speak, click the cells;
local Whisper through `craft-voice`). `agent-layer/demo/build-demo.sh`
builds a demo workbook with all of it and renders each feature.

Next: WordCraft and DeckCraft are built the same way in their own forks (anton-belkin/wordcraft,
anton-belkin/deckcraft, branch agent-layer).
