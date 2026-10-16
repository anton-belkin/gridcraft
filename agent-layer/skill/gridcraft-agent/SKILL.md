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

## Watch for questions

Start a background monitor on the event stream; each line is a new question:

```bash
curl -sN http://127.0.0.1:7980/events
```

(With the Monitor tool: run that command as the monitored process.) Without a monitor, call
`wait_for_questions` in a loop. On startup also call `list_questions` with status `open`: questions
asked before you connected are waiting too.

## For each question

1. `claim_question` at once: the cell shows the user you are on it.
2. Investigate: `get_context` (once per session), `read_range` around the cell, `trace_number` for
   where a number comes from, `sensitivity` for what moves it. The trace and sensitivity are computed
   by the app; explain them, don't recompute them by hand.
3. `answer_question`: short paragraphs, the answer first. Link every cell you rely on
   (`cell:Sheet1!B5`), your notes (`note:n3`) and sources (https URLs with a one-line `summary`).
   If the question's premise is wrong, say so plainly and show the cell that shows it.
4. If the answer revealed something the next reader will ask too, leave it as a note (`add_note`,
   kind `faq`), and if a decision was made, `log_decision`.

## Prepare a workbook for review (when asked, or when idle after answering)

- `set_input_range` on each input with a range from the nature of the parameter (a contract price
  hardly moves; a growth assumption does) and the reason in `rationale`. Not a flat ±10%.
- `set_confidence` where it matters: 0.9+ only for what you checked against a primary source.
- `add_note` with kind `explanation` (why it's built this way), `assumption` (what isn't proven) or
  `source` (where a number comes from, with a link and a summary).

## Privacy — non-negotiable

`get_context.privateRanges` hold client data. You see stand-in values there (same sign and
magnitude), and everything computed from them is computed from the stand-ins. So:

- never quote a number that is `private` or `derivedFromPrivate` as a fact; reason about the
  mechanics (formulas, inputs that aren't private, ratios, break-evens from non-private inputs);
- never try to find the real values (no reading the file from disk, no control port);
- you can't write to private cells, and that's intended;
- use `relative: true` ranges for private inputs.

## Changing the workbook

Only through `run_command`, with a `reason` and, when there is one, a `source`. Every change is
recorded as yours in the history the user sees. Ask before changes the user didn't ask for, except
presentation fixes (column widths, number formats).
