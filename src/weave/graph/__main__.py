"""`python -m weave.graph` — project a monolog to RDF and query it.

Usage:
    python -m weave.graph nt    <transcript>          # emit N-Triples to stdout
    python -m weave.graph query <transcript> [sparql] # load + run a SPARQL SELECT

With no SPARQL given, `query` runs a built-in CROSS-READER demo: turns that BOTH
have a cadence signal AND an emojikey harvested — the join that JSONL grep can't
do but the graph does in one query. This is the proof the projection is real.
"""

from __future__ import annotations

import sys

from .project import monolog_to_ntriples

# The demo cross-reader query: find transcript sequence positions (turns) that
# carry BOTH a cadence annotation and an emojikey annotation — i.e. structurally
# located turns where an emojikey was also harvested. One SPARQL join across two
# independent readers, anchored on the shared seq.
_DEMO_SPARQL = """
PREFIX oa:    <http://www.w3.org/ns/oa#>
PREFIX prov:  <http://www.w3.org/ns/prov#>
PREFIX weave: <https://weave.repolex.ai/ns#>
SELECT ?seq ?cadenceType ?emojiRaw WHERE {
  ?cadAnn   prov:wasAttributedTo <https://weave.repolex.ai/ns#detector/cadence> ;
            weave:eventType ?cadenceType ;
            oa:hasTarget ?cadTgt .
  ?cadTgt   weave:seq ?seq .
  ?emojiAnn prov:wasAttributedTo <https://weave.repolex.ai/ns#detector/emojikey> ;
            oa:hasTarget ?emojiTgt ;
            oa:hasBody   ?emojiBody .
  ?emojiTgt weave:seq ?seq .
  ?emojiBody weave:sig_raw ?emojiRaw .
}
ORDER BY ?seq
"""


def _cmd_nt(transcript: str) -> int:
    for line in monolog_to_ntriples(transcript):
        print(line)
    return 0


def _cmd_query(transcript: str, sparql: str | None) -> int:
    from .store import load_monolog, query  # lazy: needs the graph extra
    store = load_monolog(transcript)
    q = sparql or _DEMO_SPARQL
    rows = list(query(store, q))
    if not rows:
        print("(no solutions)")
        return 0
    for r in rows:
        print(r)
    return 0


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__)
        return 2
    cmd, transcript = argv[0], argv[1]
    if cmd == "nt":
        return _cmd_nt(transcript)
    if cmd == "query":
        return _cmd_query(transcript, argv[2] if len(argv) > 2 else None)
    print(f"unknown command: {cmd}\n{__doc__}")
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
