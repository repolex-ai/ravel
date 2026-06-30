"""Load a projected monolog into pyoxigraph and run SPARQL over it.

pyoxigraph is a LAZY import here — `weave.graph.project` (the .nt emitter) is
pure stdlib and always importable; only load/query needs the optional `graph`
extra (`uv sync --extra graph`). Bulk import follows the chunk-and-flush
discipline that keeps RocksDB memtables from blowing up on large loads.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Iterator

from .project import monolog_to_ntriples


def _require_pyoxigraph():
    try:
        import pyoxigraph  # noqa: F401
        return pyoxigraph
    except ImportError as e:  # pragma: no cover - environment-dependent
        raise ImportError(
            "weave.graph.store needs pyoxigraph — install the graph extra:\n"
            "    uv sync --extra graph\n"
            "(the monolog hot path never needs it; the graph is a derived cache.)"
        ) from e


def load_monolog(transcript_path: str | Path, *, store=None):
    """Project a transcript's monolog to RDF 1.2 and bulk-load it.

    Returns an in-memory pyoxigraph Store (or loads into the one passed in).
    The .nt is built in memory and loaded in one shot — monologs are small
    (one transcript); the chunked path is for the cross-corpus aggregate.
    """
    ox = _require_pyoxigraph()
    if store is None:
        store = ox.Store()
    nt = "\n".join(monolog_to_ntriples(transcript_path)) + "\n"
    store.load(nt, format=ox.RdfFormat.N_TRIPLES)
    return store


def query(store, sparql: str) -> Iterator[dict[str, Any]]:
    """Run a SELECT and yield each solution as a plain {var: python-value} dict,
    so callers never touch pyoxigraph term objects.

    In pyoxigraph 0.5.x the variable list lives on the QuerySolutions result
    object (`.variables`), and each QuerySolution is indexed by Variable; a
    None means that variable was unbound in this solution and is omitted.
    """
    results = store.query(sparql)
    variables = list(results.variables)  # [Variable(...), ...]
    for solution in results:
        out: dict[str, Any] = {}
        for var in variables:
            term = solution[var]
            if term is not None:
                out[var.value] = _py(term)
        yield out


def _py(term) -> Any:
    """Best-effort term -> python. Literals carry a .value; IRIs a .value too."""
    v = getattr(term, "value", None)
    return v if v is not None else str(term)
