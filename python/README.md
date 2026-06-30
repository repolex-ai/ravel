# weave (Python lab)

The **research / calibration tier** for Weave. Where signals (readers) are
prototyped and proven against real transcripts before promotion to the Rust
engine at the repo root.

- `weave.ingest` — Claude Code `.jsonl` → `PoseRecord`s
- `weave.readers` — signal detectors (emojikey, cadence) over `PoseRecord`s → `MonologRow`s
- `weave.monolog` — the append-only JSONL signal log
- `weave.graph` — project a `.monolog.jsonl` to RDF 1.2 for SPARQL cross-reader queries

A reader earns promotion to the Rust engine by being *proven green here* — not by
feeling ready. This tier exists so an arbitrary model or data-science library is
one `pip install` away during calibration; the Rust engine stays lean.

See the repo root `README.md` for the engine and the overall architecture.
