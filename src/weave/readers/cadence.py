"""The cadence reader — turn-shape and rhythm, pure-Python, GENERAL tier.

The cheapest *structural* reader: where emojikey HARVESTS a token already in the
text, cadence COMPUTES over the shape of each turn — length, question density,
code-block ratio — plus the rhythm BETWEEN turns (latency, who-spoke-switch). No
model, no GPU, hook-safe; it runs on PoseRecords the same way emojikey does.

This is the catalog's "structural / cadence" detector (LOW effort, MED quality —
a cheap context signal). It is the second reader in the set, and the first that
reads STRUCTURE rather than a literal token: it proves the reader-set pattern
generalizes past pure harvest.

## What it emits

One monolog row per prose record, `reader="cadence"`, anchored to the record's
`seq` (joins straight back to source). The signal is a small named dict — the
same lossless-emit discipline as the wave contract: emit the full set of cheap
structural facts, let the viz/downstream decide what's worth showing. Nothing is
thresholded or dropped at emit time.

    signal = {
      "voice":            "human" | "assistant",
      "words":            int,    # whitespace-token count of the prose
      "chars":            int,    # character length of the prose
      "question_density": float,  # question marks / sentence-ish units, 0..~1
      "code_block_ratio": float,  # fraction of chars inside code (fenced + inline)
      "latency_s":        float | None,  # seconds since the PREVIOUS record's ts
      "voice_switch":     bool,   # did the speaker change from the previous record
    }

`latency_s` and `voice_switch` are RELATIONAL — they need the previous record.
`latency_s` is None for the first record and whenever either timestamp is missing
or unparseable (no faked zero; absence is reported honestly, not as 0.0).

## Why these facts

| fact | rhythm-face | grain |
|---|---|---|
| words / chars | "how much was said" | per-record |
| question_density | "asking vs telling" | per-record |
| code_block_ratio | "coding vs talking" | per-record |
| latency_s | "fast back-and-forth vs slow deliberation" | relational |
| voice_switch | "monolog run vs alternation" | relational |

They are kept SEPARATE (a named dict, never averaged into one "cadence score") for
the same reason the wave contract keeps its legs separate: they answer different
questions, and a downstream widget collapses them per its own policy, not here.
"""

from __future__ import annotations

import re
from datetime import datetime
from typing import Iterable, Iterator, Optional

from ..ingest import PoseRecord
from ..monolog import MonologRow

#: Fenced code blocks: ```...``` (greedy across lines), and inline `code` spans.
#: Both contribute to code_block_ratio so "talking about code in backticks" and
#: "pasting a code block" both register, weighted by how much text they occupy.
_FENCED = re.compile(r"```.*?```", re.DOTALL)
_INLINE = re.compile(r"`[^`\n]+`")

#: Sentence-ish terminators, to give question_density a denominator that isn't just
#: "characters". A turn of "why? how? when?" should read as denser than one long
#: paragraph ending in a single "?". We count terminal punctuation runs.
_SENTENCE_END = re.compile(r"[.!?]+")
_QUESTION = re.compile(r"\?")


def _parse_ts(ts: Optional[str]) -> Optional[datetime]:
    """Parse an ISO timestamp to datetime, or None if absent/unparseable.

    The transcript stamps are ISO-8601, usually UTC with a trailing `Z`.
    `fromisoformat` doesn't accept `Z` before Python 3.11, so normalize it to
    `+00:00`. Any parse failure returns None — a bad stamp must degrade latency to
    "unknown", never to a fake 0.0 that would read as instant reply.
    """
    if not ts:
        return None
    try:
        return datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except (ValueError, TypeError):
        return None


def _code_block_ratio(text: str) -> float:
    """Fraction of characters that live inside fenced or inline code, 0..1.

    Fenced blocks are measured first; inline spans are measured on the
    fenced-removed remainder so a backtick pair *inside* a fenced block isn't
    double-counted. Clamped to 1.0 defensively.
    """
    if not text:
        return 0.0
    total = len(text)
    fenced_chars = sum(len(m.group(0)) for m in _FENCED.finditer(text))
    remainder = _FENCED.sub("", text)
    inline_chars = sum(len(m.group(0)) for m in _INLINE.finditer(remainder))
    return min(1.0, (fenced_chars + inline_chars) / total)


def _question_density(text: str) -> float:
    """Question marks per sentence-ish unit, 0..~1 (can exceed 1 only if every
    sentence is a multi-`?` — clamped to 1.0).

    Denominator is the count of sentence terminators (`.`/`!`/`?` runs), floored
    at 1 so a single un-terminated fragment still has a denominator. A turn with
    no `?` is 0.0; a turn that is all questions trends to 1.0.
    """
    if not text:
        return 0.0
    questions = len(_QUESTION.findall(text))
    sentences = max(1, len(_SENTENCE_END.findall(text)))
    return min(1.0, questions / sentences)


class CadenceReader:
    """Emit per-turn structural + rhythm facts into the monolog.

    Stateful across the stream only for the RELATIONAL facts: it remembers the
    previous record's timestamp and voice to compute latency and voice-switch.
    Everything else is computed from the current record alone. One row per record,
    in `seq` order — the reader never reorders or drops records.
    """

    name = "cadence"

    def read(self, records: Iterable[PoseRecord]) -> Iterator[MonologRow]:
        prev_ts: Optional[datetime] = None
        prev_voice: Optional[str] = None
        prev_seq: Optional[int] = None

        for rec in records:
            cur_ts = _parse_ts(rec.ts)

            # relational facts (need the previous record)
            if prev_ts is not None and cur_ts is not None:
                latency_s: Optional[float] = (cur_ts - prev_ts).total_seconds()
            else:
                latency_s = None  # honest absence, not a faked 0.0
            voice_switch = prev_voice is not None and rec.voice != prev_voice

            text = rec.text
            # what was looked at (prov:used): this record always; the PREVIOUS
            # record too whenever a relational fact was computed from it — so the
            # provenance honestly reflects the relational legs, not just the turn.
            evidence_seqs = [rec.seq]
            if prev_seq is not None and (latency_s is not None or prev_voice is not None):
                evidence_seqs.insert(0, prev_seq)

            yield MonologRow(
                reader=self.name,
                event_type="rhythm/turn",
                ts=rec.ts or "",
                # whole-turn anchor: cadence reads the entire record, so the span
                # is the full prose (0..len) — the oa:TextPositionSelector covers
                # the turn, not a substring.
                anchor={
                    "seq": rec.seq,
                    "src_uuid": rec.src_uuid,
                    "turn_id": rec.turn_id,
                    "start": 0,
                    "end": len(text),
                },
                evidence={"seqs": evidence_seqs},
                signal={
                    "voice": rec.voice,
                    "words": len(text.split()),
                    "chars": len(text),
                    "question_density": round(_question_density(text), 4),
                    "code_block_ratio": round(_code_block_ratio(text), 4),
                    "latency_s": latency_s,
                    "voice_switch": voice_switch,
                },
            )

            # advance relational state. Carry the LAST KNOWN ts forward when the
            # current record has none, so a single missing stamp doesn't poison
            # the next latency too — but voice always advances (it's never absent).
            if cur_ts is not None:
                prev_ts = cur_ts
            prev_voice = rec.voice
            prev_seq = rec.seq
