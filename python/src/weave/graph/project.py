"""MonologRow -> RDF 1.2 N-Triples.

Pure stdlib: emitting `.nt` is a streaming string map over the JSONL, no triple
store needed (N-Triples is itself a one-triple-per-line append-only format, so
the projection is as cheap as the monolog it reads). pyoxigraph is only needed
to LOAD and QUERY the result — that lives in `store.py` and is a lazy import.

The RDF 1.2 syntax here is verified against pyoxigraph 0.5.6:
  - raw triple term  `<<( s p o )>>`  (PARENS; object-position only)
  - reification predicate  `rdf:reifies`
  - the base triple is NOT separately asserted (the detection is an unasserted
    CLAIM carrying a confidence, which is exactly the belief semantics we want).

The reified proposition is the WORLD-claim — `<<( event weave:exhibits type )>>`
with the EVENT (the thing the detection is about) as subject, never the
annotation. Reifying a statement about the annotation would restate metadata
the annotation already asserts, and the belief semantics would protect nothing
(2026-07-02 fix, synchronized with the Rust engine's annotate.rs). The claim
joins to its evidence wrapper via `prov:wasDerivedFrom`.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any, Iterator

from ..monolog import MonologRow, monolog_path

# Namespaces. WEAVE is ours; the rest are the W3C standards each owning one link.
WEAVE = "https://weave.repolex.ai/ns#"
OA = "http://www.w3.org/ns/oa#"
PROV = "http://www.w3.org/ns/prov#"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
XSD = "http://www.w3.org/2001/XMLSchema#"
DCT = "http://purl.org/dc/terms/"


# --- term builders (N-Triples is fully-expanded IRIs + literals) -------------

def _iri(s: str) -> str:
    return f"<{s}>"


def _lit(value: Any) -> str:
    """A typed N-Triples literal. bool/int/float get xsd types; everything else
    is an escaped string. Mirrors how oxigraph round-trips these so a projected
    value compares equal to a hand-written query constant."""
    if isinstance(value, bool):
        return f'"{str(value).lower()}"^^<{XSD}boolean>'
    if isinstance(value, int):
        return f'"{value}"^^<{XSD}integer>'
    if isinstance(value, float):
        return f'"{value}"^^<{XSD}decimal>'
    return _str_lit(str(value))


def _str_lit(s: str) -> str:
    esc = (s.replace("\\", "\\\\").replace('"', '\\"')
            .replace("\n", "\\n").replace("\r", "\\r").replace("\t", "\\t"))
    return f'"{esc}"'


def _triple(s: str, p: str, o: str) -> str:
    return f"{s} {p} {o} ."


# --- deterministic identity (the idempotent-reimport requirement) ------------

def _hash(*parts: Any) -> str:
    """Stable short hash over a row's identity. Re-importing the same JSONL
    yields the same IRIs, so a bulk re-load is idempotent (no blank-node
    duplication — the footgun proven on pyoxigraph 0.5.6)."""
    blob = json.dumps(parts, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha1(blob.encode("utf-8")).hexdigest()[:16]


def annotation_iri(transcript_id: str, row: MonologRow) -> str:
    h = _hash(transcript_id, row.reader, row.event_type, row.anchor, row.signal)
    return f"{WEAVE}ann/{h}"


def reifier_iri(transcript_id: str, row: MonologRow) -> str:
    h = _hash(transcript_id, row.reader, row.event_type, row.anchor, row.signal)
    return f"{WEAVE}claim/{h}"


def _detector_iri(reader: str) -> str:
    return f"{WEAVE}detector/{reader}"


def _transcript_iri(transcript_id: str) -> str:
    return f"{WEAVE}transcript/{transcript_id}"


def _event_subject_iri(transcript_id: str, row: MonologRow) -> tuple[str, str | None]:
    """The world-side subject of the claim: the most durable identity the anchor
    carries for the thing the detection is ABOUT (not the annotation node).
    Priority: source-event uuid > turn id > seq position > whole transcript.
    (Containers are identity, positions are addresses — identity survives
    operations that renumber positions, so turn_id outranks seq.)

    Returns (iri, resolved_by_note). The note is non-None when an identity key
    was PRESENT BUT EMPTY and resolution fell through to a positional address —
    that fallthrough must be loud (a `weave:resolvedBy` tag on the claim), never
    silent: missing-identity hidden as data-completeness is identity-loss."""
    base = _transcript_iri(transcript_id)
    degraded = [k for k in ("src_uuid", "turn_id")
                if k in row.anchor and not str(row.anchor[k] or "").strip()]
    if row.anchor.get("src_uuid"):
        return f"{base}/event/{row.anchor['src_uuid']}", None
    if row.anchor.get("turn_id"):
        note = f"turn_id ({degraded[0]} was empty)" if degraded else None
        return f"{base}/event/{row.anchor['turn_id']}", note
    if "seq" in row.anchor:
        note = f"seq ({' and '.join(degraded)} was empty)" if degraded else None
        return f"{base}/seq/{row.anchor['seq']}", note
    note = f"transcript ({' and '.join(degraded)} was empty)" if degraded else None
    return base, note


# --- the per-row projection --------------------------------------------------

def row_to_triples(transcript_id: str, row: MonologRow) -> Iterator[str]:
    """One MonologRow -> a handful of N-Triples lines.

    Layout:
      ann  a oa:Annotation ; oa:hasTarget [ oa:hasSource <transcript> ;
             oa:hasSelector [ a oa:TextPositionSelector ; oa:start ; oa:end ] ] ;
           oa:hasBody [ <signal fields> ] ;
           prov:wasAttributedTo <detector> ; prov:generatedAtTime ts ;
           prov:used <evidence seqs...>
      claim rdf:reifies <<( event weave:exhibits event_type )>> ;
            prov:wasDerivedFrom ann ;
            weave:detector ; weave:confidence ; ...        (UNASSERTED belief)
    """
    ann = _iri(annotation_iri(transcript_id, row))
    claim = _iri(reifier_iri(transcript_id, row))
    detector = _iri(_detector_iri(row.reader))
    transcript = _iri(_transcript_iri(transcript_id))
    a = _iri(RDF + "type")

    # --- the detector as a prov:SoftwareAgent ---
    yield _triple(detector, a, _iri(PROV + "SoftwareAgent"))
    yield _triple(detector, _iri(WEAVE + "readerName"), _str_lit(row.reader))

    # --- the annotation (oa:) anchored to a span of the transcript ---
    yield _triple(ann, a, _iri(OA + "Annotation"))
    yield _triple(ann, _iri(PROV + "wasAttributedTo"), detector)
    if row.event_type:
        yield _triple(ann, _iri(WEAVE + "eventType"), _str_lit(row.event_type))
    if row.ts:
        yield _triple(ann, _iri(PROV + "generatedAtTime"),
                      f'{_str_lit(row.ts)}^^<{XSD}dateTime>')

    # target: a blank-node-free deterministic selector node, hung off the ann IRI
    target = _iri(f"{annotation_iri(transcript_id, row)}/target")
    selector = _iri(f"{annotation_iri(transcript_id, row)}/selector")
    yield _triple(ann, _iri(OA + "hasTarget"), target)
    yield _triple(target, _iri(OA + "hasSource"), transcript)
    start = row.anchor.get("start")
    end = row.anchor.get("end")
    if start is not None and end is not None:
        yield _triple(target, _iri(OA + "hasSelector"), selector)
        yield _triple(selector, a, _iri(OA + "TextPositionSelector"))
        yield _triple(selector, _iri(OA + "start"), _lit(int(start)))
        yield _triple(selector, _iri(OA + "end"), _lit(int(end)))
    # carry the durable join keys too (address-by-identity, not just offsets)
    if "seq" in row.anchor:
        yield _triple(target, _iri(WEAVE + "seq"), _lit(int(row.anchor["seq"])))
    if row.anchor.get("src_uuid"):
        yield _triple(target, _iri(WEAVE + "srcUuid"), _str_lit(str(row.anchor["src_uuid"])))
    if row.anchor.get("turn_id"):
        yield _triple(target, _iri(WEAVE + "turnId"), _str_lit(str(row.anchor["turn_id"])))

    # --- evidence (prov:used): what the detector looked at ---
    for ev_seq in _evidence_seqs(row.evidence):
        ev = _iri(f"{_transcript_iri(transcript_id)}/seq/{ev_seq}")
        yield _triple(ann, _iri(PROV + "used"), ev)

    # --- body (oa:hasBody): the signal payload, flattened onto a body node ---
    body = _iri(f"{annotation_iri(transcript_id, row)}/body")
    yield _triple(ann, _iri(OA + "hasBody"), body)
    if isinstance(row.signal, dict):
        for k, v in row.signal.items():
            if v is None:
                continue  # honest absence: don't emit a triple for a null leg
            yield _triple(body, _iri(WEAVE + "sig_" + _safe(k)), _lit(v))
    else:
        yield _triple(body, _iri(WEAVE + "sigValue"), _lit(row.signal))

    # --- the CLAIM: an RDF 1.2 triple term, UNASSERTED, carrying detector meta ---
    # <<( event weave:exhibits event_type )>> — the world-claim, with the EVENT
    # as subject (see module docstring). claim → ann via prov:wasDerivedFrom.
    et = _str_lit(row.event_type or "signal")
    event_subject, resolved_by = _event_subject_iri(transcript_id, row)
    event = _iri(event_subject)
    triple_term = f"<<( {event} {_iri(WEAVE + 'exhibits')} {et} )>>"
    yield _triple(claim, _iri(RDF + "reifies"), triple_term)
    yield _triple(claim, _iri(PROV + "wasDerivedFrom"), ann)
    if resolved_by:
        # loud fallthrough: identity key present-but-empty, resolution degraded
        yield _triple(claim, _iri(WEAVE + "resolvedBy"), _str_lit(resolved_by))
    yield _triple(claim, _iri(WEAVE + "detector"), _str_lit(row.reader))
    # detector_version / detection_type / confidence: read from signal if the
    # reader put them there; always emit detection_type from event_type.
    if row.event_type:
        yield _triple(claim, _iri(WEAVE + "detectionType"), _str_lit(row.event_type))
    if isinstance(row.signal, dict):
        if "confidence" in row.signal and row.signal["confidence"] is not None:
            yield _triple(claim, _iri(WEAVE + "confidence"), _lit(float(row.signal["confidence"])))
        if "version" in row.signal:
            yield _triple(claim, _iri(WEAVE + "detectorVersion"), _str_lit(str(row.signal["version"])))


def _evidence_seqs(evidence: Any) -> list[int]:
    """Normalize the readers' two evidence shapes ({seq:..} | {seqs:[..]}) to a
    list of ints. Unknown shapes contribute nothing (fail-soft)."""
    if not isinstance(evidence, dict):
        return []
    if "seqs" in evidence and isinstance(evidence["seqs"], list):
        return [int(s) for s in evidence["seqs"]]
    if "seq" in evidence:
        return [int(evidence["seq"])]
    return []


def _safe(k: str) -> str:
    return "".join(c if c.isalnum() else "_" for c in k)


# --- whole-monolog projection ------------------------------------------------

def _transcript_id_from_path(transcript_path: str | Path) -> str:
    """Stable id for the transcript: its filename stem (sans .jsonl/.monolog).
    Address-by-identity — the same transcript always yields the same IRIs."""
    name = Path(transcript_path).name
    for suffix in (".monolog.jsonl", ".jsonl"):
        if name.endswith(suffix):
            return name[: -len(suffix)]
    return name


def monolog_to_ntriples(transcript_path: str | Path,
                        *, exclude_render: bool = True) -> Iterator[str]:
    """Stream the .monolog.jsonl for a transcript as N-Triples lines.

    Resolves the monolog sidecar from the transcript path the same way the
    writer does, reads it row-by-row, and yields N-Triples. Deduplicates the
    shared detector/transcript triples so the .nt isn't bloated with repeats.
    """
    mono = monolog_path(transcript_path)
    transcript_id = _transcript_id_from_path(transcript_path)
    if not Path(mono).exists():
        return
    seen: set[str] = set()
    with open(mono, "r", encoding="utf-8") as f:
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
            row = MonologRow.from_obj(obj)
            for t in row_to_triples(transcript_id, row):
                # dedupe the repeated detector/transcript-level triples; keep
                # per-annotation triples (their subjects are unique hashes).
                if t in seen:
                    continue
                seen.add(t)
                yield t
