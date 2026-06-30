"""The monolog — one append-only signal log per transcript.

The conversation's own monologue about itself: one voice made of many readers.
See docs/2026_06_13_READER_SET_AND_MONOLOG.md.

Every reader writes rows of the SAME shape into ONE log per transcript:

    {"reader": "<name>", "anchor": {...}, "signal": <reader's own payload>}

- `reader` is the discriminator (a new reader = a new value, not a new file).
- `anchor` joins back to the source (a `seq`, a `span`, a line/message range).
- `signal` is the reader's payload in its own shape.

The source transcript stays byte-faithful forever; the monolog is the DERIVED
layer readers WRITE to. That two-layer split is what lets the loop be closed
(readers write signals) while the "no source mutation" law still holds (the
source is never the thing being written). One log per transcript means you
reconstruct a conversation's whole signal-history from one file, and readers are
ROWS not files.
"""

from __future__ import annotations

from .writer import Monolog, MonologRow, monolog_path

__all__ = ["Monolog", "MonologRow", "monolog_path"]
