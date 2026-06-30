"""Graph projection tests: MonologRow -> RDF 1.2 N-Triples, and (if pyoxigraph
is installed) the bulk-load + cross-reader SPARQL round-trip.

The .nt projection is pure stdlib and always tested. The store round-trip needs
the optional `graph` extra, so it's skipped cleanly when pyoxigraph is absent —
the monolog hot path never depends on it.
"""

from __future__ import annotations

import json

import pytest

from weave.graph.project import (
    monolog_to_ntriples,
    reifier_iri,
    row_to_triples,
)
from weave.ingest import load
from weave.monolog import Monolog, MonologRow
from weave.readers import EmojikeyReader
from weave.readers.cadence import CadenceReader


# --- fixtures ----------------------------------------------------------------

def _transcript(tmp_path):
    lines = [
        {"type": "user", "uuid": "u1", "timestamp": "2026-06-24T21:00:00Z",
         "message": {"role": "user", "content": "how's it going? blockers?"}},
        {"type": "assistant", "uuid": "a1", "parentUuid": "u1",
         "timestamp": "2026-06-24T21:00:08Z",
         "message": {"role": "assistant", "content": [
             {"type": "text",
              "text": "shipping! [ME|🧠8]~[CONTENT|💻9]~[YOU|🎓8] green"}]}},
    ]
    p = tmp_path / "s.jsonl"
    p.write_text("\n".join(json.dumps(o) for o in lines) + "\n", encoding="utf-8")
    return p


def _build_monolog(transcript):
    recs = list(load(transcript))
    mono = Monolog.for_transcript(transcript)
    for reader in (CadenceReader(), EmojikeyReader()):
        for row in reader.read(recs):
            mono.append(row)
    mono.flush()
    return mono


# --- projection (pure stdlib, always runs) -----------------------------------

def test_row_emits_oa_prov_and_rdf12_triple_term():
    row = MonologRow(
        reader="cadence", event_type="rhythm/turn", ts="2026-06-24T21:00:00Z",
        anchor={"seq": 3, "src_uuid": "a2", "turn_id": "u2", "start": 0, "end": 40},
        evidence={"seqs": [2, 3]},
        signal={"words": 12, "voice": "assistant"},
    )
    nt = list(row_to_triples("sess", row))
    body = "\n".join(nt)
    # oa: anchoring
    assert "http://www.w3.org/ns/oa#Annotation" in body
    assert "http://www.w3.org/ns/oa#TextPositionSelector" in body
    assert "http://www.w3.org/ns/oa#start" in body
    # prov: provenance chain
    assert "http://www.w3.org/ns/prov#wasAttributedTo" in body
    assert "http://www.w3.org/ns/prov#generatedAtTime" in body
    assert "http://www.w3.org/ns/prov#used" in body  # evidence
    # RDF 1.2 triple term (PARENS — the 1.2 syntax, not RDF-star) + rdf:reifies
    assert "rdf-syntax-ns#reifies" in body
    assert "<<(" in body and ")>>" in body


def test_reifier_iri_is_deterministic():
    """Same row -> same reifier IRI -> idempotent re-import (no blank-node dup)."""
    row = MonologRow(reader="cadence", event_type="rhythm/turn",
                     anchor={"seq": 1}, signal={"words": 5})
    assert reifier_iri("sess", row) == reifier_iri("sess", row)
    other = MonologRow(reader="cadence", event_type="rhythm/turn",
                       anchor={"seq": 2}, signal={"words": 5})
    assert reifier_iri("sess", row) != reifier_iri("sess", other)


def test_null_signal_legs_are_omitted():
    """A None leg (e.g. latency_s on the first turn) emits NO triple — honest
    absence, not a faked value."""
    row = MonologRow(reader="cadence", event_type="rhythm/turn",
                     anchor={"seq": 0, "start": 0, "end": 10},
                     signal={"latency_s": None, "words": 3})
    body = "\n".join(row_to_triples("sess", row))
    assert "sig_words" in body
    assert "sig_latency_s" not in body


def test_monolog_to_ntriples_covers_both_readers(tmp_path):
    transcript = _transcript(tmp_path)
    _build_monolog(transcript)
    nt = list(monolog_to_ntriples(transcript))
    body = "\n".join(nt)
    assert "detector/cadence" in body
    assert "detector/emojikey" in body
    assert body.count("<<(") >= 2  # at least one reified claim per reader present


# --- store round-trip (needs the optional graph extra) -----------------------

pyoxigraph = pytest.importorskip("pyoxigraph",
                                 reason="graph extra (pyoxigraph) not installed")


def test_bulk_load_and_cross_reader_join(tmp_path):
    from weave.graph.store import load_monolog, query
    transcript = _transcript(tmp_path)
    _build_monolog(transcript)

    store = load_monolog(transcript)
    n1 = len(store)
    # idempotent: re-loading the SAME monolog adds nothing (deterministic IRIs)
    load_monolog(transcript, store=store)
    assert len(store) == n1

    # cross-reader join: seq 1 has BOTH a cadence row and an emojikey harvest
    q = """
    PREFIX oa:    <http://www.w3.org/ns/oa#>
    PREFIX prov:  <http://www.w3.org/ns/prov#>
    PREFIX weave: <https://weave.repolex.ai/ns#>
    SELECT ?seq ?emojiRaw WHERE {
      ?c prov:wasAttributedTo <https://weave.repolex.ai/ns#detector/cadence> ;
         oa:hasTarget ?ct . ?ct weave:seq ?seq .
      ?e prov:wasAttributedTo <https://weave.repolex.ai/ns#detector/emojikey> ;
         oa:hasTarget ?et ; oa:hasBody ?eb .
      ?et weave:seq ?seq . ?eb weave:sig_raw ?emojiRaw .
    }"""
    rows = list(query(store, q))
    assert len(rows) == 1
    assert rows[0]["seq"] == "1"
    assert "[ME|🧠8]" in rows[0]["emojiRaw"]


def test_rdf12_triple_term_claim_is_queryable(tmp_path):
    from weave.graph.store import load_monolog, query
    transcript = _transcript(tmp_path)
    _build_monolog(transcript)
    store = load_monolog(transcript)
    q = """
    PREFIX rdf:   <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
    PREFIX weave: <https://weave.repolex.ai/ns#>
    SELECT ?detector ?dtype WHERE {
      ?claim rdf:reifies ?tt ;
             weave:detector ?detector ;
             weave:detectionType ?dtype .
    }"""
    rows = list(query(store, q))
    detectors = {r["detector"] for r in rows}
    assert "cadence" in detectors
    assert "emojikey" in detectors
