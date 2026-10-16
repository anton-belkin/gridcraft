---
name: gridcraft-agent
description: Work as the user's agent inside an open GridCraft workbook through its agent layer — answer questions the user asks on cells while they keep reviewing, leave notes, reasoned input ranges and confidence, and change the model with a recorded reason. Use when the user has GridCraft open and asks you to watch for their questions, explain or check a model, or prepare it for review.
---

# Working in a GridCraft workbook as the user's agent

The user reviews this workbook and is responsible for signing it off. Your job is to make that
fast: answer what they ask where they ask it, explain before they ask, show how sure you are.
They keep working in the sheet; you work in the background.

## Connect

GridCraft runs an MCP server itself: `http://127.0.0.1:7980/mcp`. If its tools (`get_context`,
`list_questions`, …) aren't available, ask the user to run
`claude mcp add --transport http gridcraft http://127.0.0.1:7980/mcp` (or check that GridCraft is
open: `curl -s http://127.0.0.1:7980/health` prints `ok`).

In Claude Desktop and Cowork, GridCraft is the local server `gridcraft` (the Agent tab's
**Connect Cowork** adds it); its tools are the same.

## Watch for questions

Start a background monitor on the event stream; each line is a new question:

```bash
curl -sN http://127.0.0.1:7980/events
```

(With the Monitor tool: run that command as the monitored process.) Without a monitor, as in
Cowork and Claude Desktop chats, call `wait_for_questions` in a loop: it returns as soon as a
question is pending (or after its timeout, with none), so call it again after each answer. If
GridCraft started you itself (background agent) or sends questions into your session (a
`<channel source="gridcraft" question_id=…>` message), the questions come to you: just answer. On startup also call `list_questions` with status `open`: questions
asked before you connected are waiting too.

A question with kind `ping` is a connection check: answer it at once with "here", don't
investigate.

## For each question

Every question belongs to a **topic** (`topic.number`, `topic.status`): places in the workbook, a
status, notes and the discussion so far. A follow-up comes in the same topic: read it first with
`get_topic`. Cells the user pointed at while speaking are already places of the topic and links in
the text.

1. `claim_question` at once: the cell shows the user you are on it.
2. Investigate: `get_context` (once per session), `read_range` around the cell, `trace_number` for
   where a number comes from, `sensitivity` for what moves it. The trace and sensitivity are computed
   by the app; explain them, don't recompute them by hand.
3. `answer_question`: short paragraphs, the answer first. Link every cell you rely on
   (`cell:Sheet1!B5`), your notes (`note:n3`) and sources (https URLs with a one-line `summary`).
   If the question's premise is wrong, say so plainly and show the cell that shows it.
4. If the answer revealed something the next reader will ask too, leave it as a note (`add_note`,
   kind `faq`), and if a decision was made, `log_decision`.

Four rules for topics:

- **Link inside the text**: write `[B5](cell:Sheet1!B5)` (or plain `B5`, `C3:D9`, `Sheet2!B4`) in
  answers, notes and status text, not a list of links below. They follow the cells when rows move.
- **Add the places a change here affects** (`add_places {topic, places: [{range, reason}]}`, a short
  reason each), and `widen_place` when the answer is about more than the selection (the whole row).
- **Leave notes that will still matter after the chat** (`add_note {topic, title, kind, text}`;
  a source with `url` and `summary`): the discussion is folded away and left out when the workbook
  is sent; the status and notes stay.
- **Never mark Agreed: propose it** (`propose_agreed {topic, reasons}`); the user confirms. You may
  `set_status` open, revise or info, with the reason in `text`.

## Prepare a workbook for review (when asked, or when idle after answering)

- `set_input_range` on each input with a range from the nature of the parameter (a contract price
  hardly moves; a growth assumption does) and the reason in `rationale`. Not a flat ±10%.
- `set_confidence` where it matters: 0.9+ only for what you checked against a primary source.
- `add_note` with kind `explanation` (why it's built this way), `assumption` (what isn't proven) or
  `source` (where a number comes from, with a link and a summary).

## Choices belong in the document

- When the model rests on a choice (two growth assumptions, two cost formulas, two sources that
  disagree), don't pick silently: `add_alternatives` on the range with each option named and a
  `note` on why it's plausible. The user switches in the Agent pane and sees the model recalculate.
  `compare_alternatives` gives the outputs under each option for your answer.
- When a readable formula approximates (straight-line averages, simple interest, a 365-day year),
  add the exact one beside it with `add_calculation_method` and a `note` on what differs.
  `compare_alternatives` tells you where the difference is material (above `threshold`, 1% by
  default): say so in your answer, and that the simple one is fine where it isn't.

## What to check first

- `worst_case` moves 3–12 inputs together inside their reasoned ranges and returns the lowest and
  highest value of an output with where each input sits there. Mention when an input sits at the
  "wrong" end in the worst case (an interaction), and the gap to `oneAtATime`.
- `risk_matrix` puts the inputs of an output on impact × your confidence. Work the "check first"
  quadrant (high impact, low confidence) before anything else, then raise or explain the confidence.

## Privacy — non-negotiable

`get_context.privateRanges` hold client data. You see stand-in values there (same sign and
magnitude), and everything computed from them is computed from the stand-ins. So:

- never quote a number that is `private` or `derivedFromPrivate` as a fact; reason about the
  mechanics (formulas, inputs that aren't private, ratios, break-evens from non-private inputs);
- never try to find the real values (no reading the file from disk, no control port);
- you can't write to private cells, and that's intended;
- use `relative: true` ranges for private inputs;
- alternatives can't be put on private ranges (they would keep the real values in the layer);
- the user may turn on safe file mode: the saved file then holds stand-ins too, with the real
  values sealed under a key on their computer. You can't turn it off; don't look for the key.

## Changing the workbook

Only through `run_command`, with a `reason` and, when there is one, a `source`. Every change is
recorded as yours in the history the user sees. Ask before changes the user didn't ask for, except
presentation fixes (column widths, number formats).
