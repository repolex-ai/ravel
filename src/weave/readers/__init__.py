"""Readers — the pluggable set that turns prose into signal.

A reader reads some span of the ingested prose stream, emits a signal in a fixed
greppable shape, and writes it to the monolog — never mutating the source. Readers
are OPTIONS we try out, the same plurality as the engine's shape-providers: you
run several and calibrate which signal is worth keeping. Adding a reader is
additive; it never touches the others or the corpus.

See docs/2026_06_13_READER_SET_AND_MONOLOG.md and the ingestor/detector taxonomy.

This package starts with the cheapest possible reader (emojikey, pure regex) to
prove the loop end-to-end: ingest -> read -> monolog. The wave engine is itself a
reader (the densest one); heavier model-backed readers (emotion, GLiNER2, ...)
are catalogued in the taxonomy and added as the calibration warrants.
"""

from __future__ import annotations

from .base import Reader
from .cadence import CadenceReader
from .emojikey import EmojikeyReader

__all__ = ["Reader", "CadenceReader", "EmojikeyReader"]
