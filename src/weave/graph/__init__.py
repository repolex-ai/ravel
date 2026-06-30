"""The graph projection — a .monolog.jsonl rendered as RDF 1.2, on demand.

The monolog is canonical as append-only JSONL (greppable, dependency-free, the
hot path). The graph is a DERIVED projection: parse the JSONL into RDF 1.2
N-Triples, bulk-load into pyoxigraph, and you can run SPARQL cross-reader
queries ("turns where cadence burst AND emojikey present"). The .nt and the
store are caches — `rm` them and rebuild from the JSONL any time. Same
discipline as repolex: source isn't stored AS triples, it's parsed INTO them.

Vocab mapping (each W3C standard owns one link of the chain), verified against
the RDF 1.2 / PROV-O / Web-Annotation specs and probed on pyoxigraph 0.5.6:

    reader      -> prov:SoftwareAgent          (the detector)
    a run       -> prov:Activity (used/atTime)  (one detector pass)
    anchor      -> oa:Annotation + oa:hasTarget + oa:TextPositionSelector
    evidence    -> prov:used                    (what was looked at)
    signal      -> oa:hasBody                    (the finding payload)
    {detector,version,detection_type,confidence}
                -> an RDF 1.2 triple term via rdf:reifies (UNASSERTED — a
                   detector's claim-with-confidence, not a materialized fact)

Reifier and annotation nodes get DETERMINISTIC IRIs (hash of the row's identity)
so re-importing the same JSONL is idempotent — no blank-node duplication. (The
blank-node footgun was proven on 0.5.6: Turtle auto-blank-nodes would duplicate
every detection on re-load.)
"""

from __future__ import annotations

from .project import (
    WEAVE,
    monolog_to_ntriples,
    row_to_triples,
    reifier_iri,
)

__all__ = ["WEAVE", "monolog_to_ntriples", "row_to_triples", "reifier_iri"]
