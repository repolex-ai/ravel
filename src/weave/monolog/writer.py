"""Monolog read/write: append-only JSONL, one file per transcript.

Deliberately tiny and dependency-free. The monolog is greppable JSONL, never a
DB engine (corpus-is-the-store). Writes are append-only and never touch the
source transcript. A reader filters `reader != "render"` to avoid reading the
viz back-edge as if it were prose-derived signal (loop-safety; see the
render-event back-edge in the signal contract).
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterator, Optional


def monolog_path(transcript_path: str | Path) -> Path:
    """`<transcript>.jsonl` -> `<transcript>.monolog.jsonl`, alongside the source.

    Parallel to the raw mirror, joining by anchor. Naming is derived from the
    transcript so one transcript has exactly one monolog.
    """
    p = Path(transcript_path)
    # strip a trailing .jsonl if present so we don't get .jsonl.monolog.jsonl
    stem = p.name[:-6] if p.name.endswith(".jsonl") else p.name
    return p.with_name(f"{stem}.monolog.jsonl")


@dataclass(frozen=True, slots=True)
class MonologRow:
    """One signal emission. `anchor`/`signal` are reader-defined shapes."""

    reader: str
    anchor: dict[str, Any]
    signal: Any

    def to_json(self) -> str:
        # compact, stable key order so rows diff cleanly and grep predictably
        return json.dumps(
            {"reader": self.reader, "anchor": self.anchor, "signal": self.signal},
            ensure_ascii=False, separators=(",", ":"), sort_keys=False,
        )

    @classmethod
    def from_obj(cls, obj: dict) -> "MonologRow":
        return cls(reader=obj["reader"], anchor=obj.get("anchor", {}),
                   signal=obj.get("signal"))


@dataclass
class Monolog:
    """Append-only handle to one transcript's monolog."""

    path: Path
    _rows: list[MonologRow] = field(default_factory=list, repr=False)

    @classmethod
    def for_transcript(cls, transcript_path: str | Path) -> "Monolog":
        return cls(path=monolog_path(transcript_path))

    def append(self, row: MonologRow) -> None:
        """Buffer a row. Call `flush()` to persist (one open per batch)."""
        self._rows.append(row)

    def emit(self, reader: str, anchor: dict[str, Any], signal: Any) -> None:
        """Convenience: build + buffer a row in one call."""
        self.append(MonologRow(reader=reader, anchor=anchor, signal=signal))

    def flush(self) -> int:
        """Append all buffered rows to the file. Returns count written."""
        if not self._rows:
            return 0
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self.path.open("a", encoding="utf-8") as f:
            for row in self._rows:
                f.write(row.to_json() + "\n")
        n = len(self._rows)
        self._rows.clear()
        return n

    def read(self, *, reader: Optional[str] = None,
             exclude_render: bool = True) -> Iterator[MonologRow]:
        """Iterate persisted rows.

        - `reader`: keep only rows from this reader.
        - `exclude_render`: drop the viz back-edge (`reader == "render"`) so a
          reader never consumes its own rendered echo (default on — loop-safety).
        """
        if not self.path.exists():
            return
        with self.path.open("r", encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if exclude_render and obj.get("reader") == "render":
                    continue
                if reader is not None and obj.get("reader") != reader:
                    continue
                yield MonologRow.from_obj(obj)
