"""Transcript ingest — turn a Claude Code session JSONL into a clean prose stream.

This is the FIRST engine build (#142). It is the thing every reader runs on: the
byte-faithful Raw mirror goes in, a filtered stream of prose-bearing turns comes
out. No reader should ever parse raw JSONL itself — they consume `PoseRecord`s.

## Why a dedicated filter layer

The Raw mirror (`<soul>/Raw/ClaudeCodeSessionLog/*.jsonl`, written by git-lex's
raw-mirror adapter on every save) is the WHOLE transcript: tool calls, tool
results, system reminders, file-history snapshots, permission-mode flips,
queue-operations, ai-titles — all of it. Sylkie's caveat, learned feeding CoPIA's
Pool: *run a role+text filter before your detectors or signal drowns in noise.*

Measured on a real 478-line spaceGOAT session: 9 distinct line-types, but only
`user`/`assistant` carry prose, and inside those only `text` blocks are prose
(`tool_use` / `tool_result` are tool spam). 478 lines -> ~106 prose blocks, a >4x
noise cut. So the filter is not a nicety; it is the ingester's primary job.

There is a SECOND noise layer inside `user` text: the harness injects
`<channel .../>` peer messages, `<command .../>` / `<local-command .../>`
wrappers, `<system-reminder>` blocks, and `Caveat:` preambles that the human never
typed. `strip_harness_wrappers` removes these as an OPTIONAL, separately-auditable
pass (off by default in `load` so the structural filter and the content filter are
never silently conflated — you opt into the content scrub explicitly).

## Two modes

- `load(path)`            — full-load: read the whole file, yield every prose turn.
- `load(path, after=uuid)`— append-incoming: yield only turns after a known uuid,
                            for tailing a growing transcript without re-reading it.

## What a reader gets

`PoseRecord` carries the signal-contract anchor fields so a reader's output can
join straight back to the source: `seq` (prose-stream position), `src_uuid` (the
transcript line's uuid), `voice` (human|assistant), `turn_id`, `ts`, `text`.
See docs/2026_06_13_SIGNAL_CONTRACT.md and docs/2026_06_13_READER_SET_AND_MONOLOG.md.
"""

from __future__ import annotations

from .reader import (
    PoseRecord,
    load,
    iter_raw_lines,
    extract_prose_blocks,
    strip_harness_wrappers,
)

__all__ = [
    "PoseRecord",
    "load",
    "iter_raw_lines",
    "extract_prose_blocks",
    "strip_harness_wrappers",
]
