"""The Reader protocol — the contract every reader implements.

A reader consumes the ingested prose stream (PoseRecords) and produces monolog
rows. The protocol is deliberately minimal so a reader can be anything from a
regex (emojikey) to a model-backed classifier (emotion) to the wave engine itself
— the monolog doesn't care which, it only sees `{reader, anchor, signal}` rows.
"""

from __future__ import annotations

from typing import Iterable, Iterator, Protocol, runtime_checkable

from ..ingest import PoseRecord
from ..monolog import MonologRow


@runtime_checkable
class Reader(Protocol):
    """Reads prose, emits monolog rows.

    `name` is the reader's discriminator in the monolog (its `reader` field).
    `read` is a generator so a reader can be per-record (emojikey: one row per
    record that has a key) or per-span (emotion-over-a-window) without changing
    the contract — it just yields however many rows it finds.
    """

    name: str

    def read(self, records: Iterable[PoseRecord]) -> Iterator[MonologRow]:
        """Consume prose records, yield monolog rows (zero or more)."""
        ...
