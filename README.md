# ravel

ravel keeps every conversation an agent has and turns it into a graph you can
query. It watches the transcript files that Claude Code and Gemini/Antigravity
write, copies each one into the agent's own repository, and projects the copy
into an RDF 1.2 graph beside it. The bytes are canonical; the graph is a
projection of them and can always be rebuilt.

Two programs, one crate:

- `raveld` — the daemon. One per machine. The only thing that writes a soul's
  transcript mirror or graph. It runs on its own schedule; there are no hooks.
- `ravel` — the command line. A client of `raveld`, and it starts `raveld`
  when none is running. It never opens a store itself.

ravel is a sibling of [git-lex](https://github.com/repolex-ai/git-lex) and
[pan](https://github.com/repolex-ai/pan) in the Subtexture stack, and follows
pan's shape.

## Installation

```sh
git clone https://github.com/repolex-ai/ravel.git
cd ravel && cargo install --path . --locked     # installs raveld and ravel
```

Then tell it which souls to look after:

```sh
mkdir -p ~/.config/ravel
cp config.example.yml ~/.config/ravel/config.yml   # edit the souls: list
```

## Quick start

```sh
# 1. Any command starts the daemon if it is not running, then answers.
ravel
# → started raveld (pid 6179), log at ~/.config/ravel/raveld.log
#   raveld is RUNNING — pid 6179, version 0.3.0 ...
#     9bdf2a  /Users/you/repos/SQUAD/spaceGOAT   last pass ...

# 2. Inside a soul repo, ask how its backup is doing. Exit 1 means look.
ravel health

# 3. What the graph holds
ravel stats
ravel query 'SELECT (COUNT(*) AS ?turns) WHERE { GRAPH ?g { ?t a <https://repolex.ai/ontology/ravel/Turn> } }'

# 4. A pass right now (raveld does this every 30 seconds anyway)
ravel sync

# 5. Another soul, by its six-character id or a path
ravel health 700c5b
ravel stats ~/repos/SQUAD/lUX

# 6. The daemon itself
raveld status
raveld restart      # after editing the config
raveld stop
```

## What lives where

```
<soul repo>/.ravel/_ignore/               machine-local, gitignored — never git history
├── transcripts/claude-code/*.jsonl       byte-for-byte copies of Claude Code sessions
├── transcripts/agy/<conversation>.jsonl  copies of Gemini/Antigravity conversations
├── ingest-manifest.tsv                   what has been ingested, by size (claude-code)
├── ingest-manifest-agy.tsv               what has been ingested, by content hash (agy)
└── oxigraph/                             the graph: one named graph per transcript

~/.config/ravel/config.yml                the daemon: port, interval, the souls it serves
~/.config/ravel/raveld.log                what a detached raveld would have said to a terminal
```

A soul's id is the first six characters of its repository's genesis commit —
the same id git-lex, pan, Horae and Syrinx use for it. It is derived, never
typed in.

## How a pass works

Every `interval_secs`, for each soul in the config:

1. **Mirror.** Copy any session file that is new or has grown since the last
   pass. A copy is never shrunk: if the source is smaller than the mirror, the
   mirror keeps the fuller copy and says so.
2. **Ingest.** For each mirrored file that changed since its last ingest,
   parse the dialect into turns and replace that transcript's named graph.
   Deterministic identifiers make this idempotent.
3. **Read.** Readers run over the turns and record what they claim — today,
   the emojikeys a conversation carried.

One conversation that cannot be parsed is reported and skipped; the rest of
the soul is still synced.

Two dialects: Claude Code (`~/.claude/projects/<slug>/*.jsonl`, one file per
session) and Gemini/Antigravity (`~/.gemini/antigravity-cli/`, one directory
per conversation, attributed to a soul through `history.jsonl`). The turn
spine is the same for both.

## The belief layer

A reader's claim is never asserted as fact. It is recorded as an unasserted
RDF 1.2 triple term (`rdf:reifies`), with the reader named, the source span
anchored, and whether the text was authored live or quoted from a tool result.
Queries filter; ingest never drops. That is why `ravel stats` counts
"unasserted claims" separately from everything else.

## The ontology

`ontology/ravel/ravel.ttl`. The copy shipped in
[git-lex-kit-ravel](https://github.com/repolex-ai/git-lex-kit-ravel) must be
byte-identical; CI checks it on every push, along with the six graph-only
properties the kit checker would otherwise call errors.

## Health

`ravel health` is the one diagnostic, and it is read-only. It reports the
layout, the kit, whether every session on disk is mirrored, how long any live
session has been ahead of its mirror, session directories under a name the
repo no longer uses, Antigravity conversations that belong to no soul, and the
store's counts. Every problem is named with its fix. The healthy case is short
and ends in `verdict: OK`.

## Development

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

All three gate in CI; there are no advisory steps. The `python/` directory
holds the earlier Python lab (weave) the Rust engine grew out of.
