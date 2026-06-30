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
    """One signal emission — a structured-log envelope around a reader's finding.

    The shape is deliberately the OpenTelemetry-log / Elastic-Common-Schema
    posture: a small stable envelope + an open per-reader payload. Each field
    maps cleanly onto a W3C vocab when the monolog is projected to RDF (see
    docs + the graph/ projector):

      - `reader`     the detector's discriminator      -> prov:SoftwareAgent
      - `event_type` what specifically fired (ECS       -> the asserted finding's
                     event.action, e.g. "rhythm/burst")    predicate/type
      - `ts`         ISO-8601 emit time                 -> prov:generatedAtTime
      - `anchor`     WHERE in the source (the locator)  -> oa:hasTarget +
                     {seq, src_uuid, turn_id, start, end}   oa:TextPositionSelector
      - `evidence`   WHAT was looked at to decide       -> prov:used
      - `signal`     the reader's payload (the finding) -> oa:hasBody

    Only `reader`, `anchor`, `signal` are required — `event_type`, `ts`, and
    `evidence` default empty and are OMITTED from the JSON when unset, so the
    artifact stays minimal and OLD rows / OLD readers round-trip unchanged
    (lossless-emit discipline: a reader emits the facts it has, nothing faked).
    """

    reader: str
    anchor: dict[str, Any]
    signal: Any
    event_type: str = ""
    ts: str = ""
    evidence: Any = None

    def to_json(self) -> str:
        # compact, stable key order so rows diff cleanly and grep predictably.
        # Envelope fields are emitted only when set, so the row never carries
        # empty "event_type":"" noise and pre-envelope rows stay byte-identical.
        obj: dict[str, Any] = {"reader": self.reader}
        if self.event_type:
            obj["event_type"] = self.event_type
        if self.ts:
            obj["ts"] = self.ts
        obj["anchor"] = self.anchor
        if self.evidence is not None:
            obj["evidence"] = self.evidence
        obj["signal"] = self.signal
        return json.dumps(
            obj, ensure_ascii=False, separators=(",", ":"), sort_keys=False,
        )

    @classmethod
    def from_obj(cls, obj: dict) -> "MonologRow":
        return cls(reader=obj["reader"], anchor=obj.get("anchor", {}),
                   signal=obj.get("signal"),
                   event_type=obj.get("event_type", ""),
                   ts=obj.get("ts", ""),
                   evidence=obj.get("evidence"))


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

    def emit(self, reader: str, anchor: dict[str, Any], signal: Any,
             *, event_type: str = "", ts: str = "", evidence: Any = None) -> None:
        """Convenience: build + buffer a row in one call.

        Envelope fields (`event_type`/`ts`/`evidence`) are keyword-only and
        optional so the 3-arg call site stays valid; a reader fills them in when
        it has them.
        """
        self.append(MonologRow(reader=reader, anchor=anchor, signal=signal,
                               event_type=event_type, ts=ts, evidence=evidence))

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
