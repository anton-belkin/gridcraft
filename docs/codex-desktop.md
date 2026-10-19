# Codex Desktop: experimental agent context

This adds an opt-in **Codex** context to the agent-layer fork. Questions are delivered by an
external bridge to the actual loaded Desktop chat on a shared Codex App Server. Codex reads and
proposes changes through the existing AItoCraft MCP tools; the reader still applies or rejects
the proposal in GridCraft.

The companion launcher/bridge, URL receiver, skill and installation instructions live in the
[AItoCraft Codex Desktop example](https://github.com/alexey-belkin/aitocraft/tree/codex/desktop-chat-links/examples/codex-desktop).
This GridCraft PR targets **agent-layer**, not the fork's default main branch.

## Enable one application instance

The companion launcher sets these environment variables only for its child processes:

| Variable | Purpose |
| --- | --- |
| `GRIDCRAFT_AGENT_PROVIDER=codex` | Use Codex instead of reading legacy Claude setup; respect an explicit Off context |
| `GRIDCRAFT_CODEX_BRIDGE=/absolute/path/codex_bridge.py` | Worker launched by Connect Codex, using Python 3 and the GUI's instance ID |
| `GRIDCRAFT_NO_PREFS=1` | Isolate this experimental application's preferences |
| `AITOCRAFT_REGISTRY_DIR=/absolute/path/registry` | Use an isolated registry shared with the AItoCraft instances |

Without `GRIDCRAFT_AGENT_PROVIDER=codex`, existing context choices keep their behavior. The Codex
instance skips Claude setup checks and does not persist its context into global agent defaults.
No global Claude configuration needs to change.

## Saved origin and live route are separate

`AgentContext` has two optional, default-empty fields, `codex_thread_id` and
`codex_thread_title`. They record the chat that created a workbook, inside its existing agent
custom XML part. A new workbook does not inherit another workbook's origin through app defaults.
Existing workbooks without these fields still load. An older application that does not recognize
the new `codex` context kind is not a supported reader of the new context metadata.

The working chat selected by a link is maintained by the companion bridge. It has priority over
origin but does not rewrite provenance or workbook cells. If the selected chat is unavailable,
the bridge can create a new actual Desktop chat on the first question; the panel shows the
mismatch. A copy/Save As retains origin; the bridge's delivery journal and link binding handle
the working route across save, reopen and worker restart.

## Native commands

All commands target the selected document; use `agent.onDocument` from the control channel to
avoid changing whichever workbook happens to be frontmost.

| Command | Parameters / result |
| --- | --- |
| `agent.codexOrigin` | `{}` reads `{threadId,title}`; `{threadId,title}` changes the saved origin as an undoable person action. Empty ID clears it. Accepts a technical UUID only. |
| `agent.codexRoute` | Runtime bridge update `{status,threadId?,title?,detail?}`; `{}` reads it. No workbook mutation. Calls marked as agent are refused. |
| `agent.codexConnect` | Starts the configured external worker for this GUI instance; no CLI model session is started. |
| `agent.codexOpenChat` | Opens `codex://threads/<technical-id>` for the live route, falling back to origin; a person action. |

Route states are `matched`, `linked`, `fresh`, `fallback`, `missing`, `linkMissing`, `unbound`,
`disconnected`, `queued`, and `attention`. Route information expires after 15 seconds without a
bridge refresh and is not serialized. The Codex Ready lamp requires a recent **Codex desktop
bridge** listener, rather than an old answered connection check. This label/presence convention
is operational status, not authentication.

## Runtime

Keep Desktop and its shared App Server running, along with GridCraft and the question worker.
The worker polls AItoCraft without an idle LLM turn; questions go through `turn/steer` when a
Desktop turn is active and `turn/start` when the selected chat is idle. It never substitutes
`codex exec/resume` for a Desktop conversation. HTTP links and the native URL receiver are
optional entry points; they do not need to process answers.

The native change is Rust only. The multi-runtime experimental example is maintained separately
in AItoCraft. The application uses the existing proposal commands: `agent.applyProposal` and
`agent.rejectProposal` remain person actions.

## Validation

The local implementation was built on macOS arm64 with Rust 1.99.1. Agent-layer tests cover
origin round-trip, no origin inheritance, runtime-only routes, UUID validation and refusal of
agent-authored route updates. Agent UI tests cover requiring a live bridge listener.

In the companion live test, five unique questions reached the same real Desktop chat, including
a rejection comment and a question after reopen. Accepting 60 → 70 changed profit 40 → 30;
rejecting 70 → 90 preserved 70/30. Formula, decisions, answers and origin survived save/reopen.
The final pass used the app's supported person-action API commands, the same commands as the
Apply/Reject buttons; it did not claim fresh UI clicks while Computer Use was unavailable.
