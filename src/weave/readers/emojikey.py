"""The emojikey reader — the cheapest reader, pure regex, GENERAL tier.

pleeb's emojikey idea (Day 26): the conversation already carries emojikeys inline
(an agent emits them in its own text). The standard shape `ME|CONTENT|YOU` is
greppable, so you don't need an outside store — you FIND them in the text. This
reader is exactly that: scan each prose record for emojikey blocks and emit one
monolog row per key found.

It is the proof-of-loop reader: ingest -> read -> monolog, no model, no GPU,
hook-safe. It validates the monolog the way the recall hook validated
corpus-is-the-store. A heavier *generative* emojikey reader (a small LLM that
MINTS a key from a span that has none) is a SPECIFIC-tier reader catalogued in the
taxonomy; this one only HARVESTS keys already present.

## The shape it matches

An emojikey is three segments, each `LABEL|payload`, joined by `~`, each wrapped
in square brackets. Labels are ME / CONTENT / YOU (case-insensitive). Payload is
freeform (emoji, digits, angle notation — the reader does not interpret it, it
captures it verbatim so the viz/downstream decides meaning). Examples that match:

    [ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35]
    [ME|🐐]~[CONTENT|⚙️🌊]~[YOU|🤝]

The reader is permissive on payload, strict on structure (the 3-label spine), so
it harvests real keys and ignores ordinary bracketed prose.
"""

from __future__ import annotations

import re
from typing import Iterable, Iterator

from ..ingest import PoseRecord
from ..monolog import MonologRow

#: A full key is three `[LABEL|payload]` segments in order (ME, CONTENT, YOU),
#: joined by `~` (optional surrounding whitespace). The fixed label ordering
#: enforces the spine while payloads stay freeform (`[^\]]*?` — anything but a
#: closing bracket, non-greedy).
_KEY = re.compile(
    r"\[\s*ME\s*\|\s*(?P<me>[^\]]*?)\s*\]"
    r"\s*~\s*"
    r"\[\s*CONTENT\s*\|\s*(?P<content>[^\]]*?)\s*\]"
    r"\s*~\s*"
    r"\[\s*YOU\s*\|\s*(?P<you>[^\]]*?)\s*\]",
    re.IGNORECASE,
)


class EmojikeyReader:
    """Harvest inline `ME|CONTENT|YOU` emojikeys into the monolog.

    Emits one row per key found, anchored to the record's `seq`, with the raw key
    string AND its parsed segments as the signal (downstream gets both the
    verbatim text and a structured handle without re-parsing).
    """

    name = "emojikey"

    def read(self, records: Iterable[PoseRecord]) -> Iterator[MonologRow]:
        for rec in records:
            for m in _KEY.finditer(rec.text):
                raw = m.group(0)
                yield MonologRow(
                    reader=self.name,
                    anchor={"seq": rec.seq, "src_uuid": rec.src_uuid},
                    signal={
                        "raw": raw,
                        "me": m.group("me"),
                        "content": m.group("content"),
                        "you": m.group("you"),
                        "voice": rec.voice,  # whose key it is (usually assistant)
                    },
                )
