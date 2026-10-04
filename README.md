# ravel

ravel keeps every conversation an agent has and turns it into a graph you can
query. It watches the transcript files that Claude Code and Gemini/Antigravity
write, imports Claude desktop (claude.ai) data exports, copies each into the
agent's own repository, and projects the copy into an RDF 1.2 graph beside it.
The bytes are canonical; the graph is a projection of them and can always be
rebuilt.

On top of the transcripts sits a memory index: one-line memories read out of
every conversation, summarized up a tree of time windows, so an agent can see
its whole history in about 96 lines and open any line down to the turns it
came from.

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
# raveld is RUNNING — pid 52756, up 0h 00m 10s, version 0.5.0, 20 on http://127.0.0.1:7881
#   memory index: on — $99.47 of $150.00 spent
#   9bdf2a  /Volumes/f00/repos/7R1PL3F0RC3/spaceGOAT
#       last pass 2026-10-04T16:11:50-07:00  mirrored 1 (unchanged 589), ingested 1 session(s) ...

# 2. Inside a soul repo, ask how its backup is doing. Exit 1 means look.
ravel health

# 3. What the graph holds
ravel stats
ravel query 'SELECT (COUNT(DISTINCT ?t) AS ?turns) WHERE { GRAPH ?g { ?t a <https://repolex.ai/ontology/ravel/Turn> } }'

# 4. Another soul, by its six-character id or a path
ravel health 700c5b
ravel stats ~/repos/SQUAD/lUX

# 5. The daemon itself
raveld status
raveld restart      # after editing the config; stops every raveld, starts one detached
raveld stop
```

`raveld` with no arguments runs in the foreground, for debugging. Day to day
nobody types it: `ravel` starts it detached, logging to
`~/.config/ravel/raveld.log`.

## Importing Claude desktop history

Download your data from claude.ai (Settings → Privacy → Export data), unzip
it, and import the folder into the soul it belongs to:

```sh
ravel import ~/Downloads/data-2026-04-06-10-54-07-batch-0000
# [ravel import] 9bdf2a data-2026-04-06-10-54-07-batch-0000: copied (4 file(s), 267818192 bytes)
# [ravel import] 9bdf2a claude.ai: 1 export(s) → ingested 1431 conversation(s), 31353 turns (0 already current)
```

The export is kept byte for byte in the soul. Importing the same one again
does nothing. A newer export months later adds what is new or has grown; an
older one imported after a newer one can never roll a conversation back.

## The memory index

```sh
ravel memory                  # the whole history in about 96 lines
ravel memory open w0-33830    # a summary opened into the lines under it
ravel memory open mec170751e01cedc   # a memory, with the turns it came from
ravel memory run              # read new turns and write summaries now
ravel memory run 50           # the same, at most 50 model calls
```

```
[ravel memory] 9bdf2a: 20075 memories, 4289 summaries — `ravel memory open <id>` opens any line
2023-11-10 → 2023-11-10  Agent corrected: LoRA is Low Rank Adaptation, not Long Range Arena. ...  [w0-33830]
2024-03-11 → 2024-03-11  Ran tag_images_by_wd14_tagger.py on /mnt/sda2/lora_source/...; cuDNN version mismatch ...  [w2-9187]
...
2026-10-04 14:33         Fixed ravel bug where 75 one-hour summaries failed due to 512-token output cap. ...  [m...]
```

Recent memories are shown word for word, older ones only through summaries.
A line in square brackets at the start ("not summarized yet", "stale") means
the summary for that stretch is still being written.

How it works: raveld reads each soul's transcripts every ten minutes, keeping
only what the human and the agent wrote (no tool output), each turn once. A
model writes one-line memories citing the turns they rest on, then a one-line
summary for every aligned window of 2^k hours that holds memories. History
imported later only re-summarizes the windows it lands in.

**It costs money, and is off until you turn it on.** It uses Claude Haiku 4.5.
Put an Anthropic API key in `~/.config/ravel/anthropic-api-key` (`chmod 600`;
never in your shell environment) and set `memory_budget_usd` in the config.
Every call is priced into `~/.config/ravel/memory-spend.tsv`; raveld stops at
the budget. For one squad of 20 souls, the first full read was $99 and still
running when this was written (2026-10-04), heading for about $120–130. After
that, only new turns are read.

## What lives where

```
<soul repo>/.ravel/_ignore/               machine-local, gitignored — never git history
├── transcripts/claude-code/*.jsonl       byte-for-byte copies of Claude Code sessions
├── transcripts/agy/<conversation>.jsonl  copies of Gemini/Antigravity conversations
├── transcripts/claude-ai/<export>/       claude.ai exports, as downloaded
├── ingest-manifest.tsv                   what has been ingested, by size (claude-code)
├── ingest-manifest-agy.tsv               what has been ingested, by content hash (agy)
├── ingest-manifest-claude-ai.tsv         each conversation's ingested version (claude.ai)
├── memory/                               the memory index: memories.jsonl, summaries.jsonl,
│                                         extracted.tsv — append-only, canonical
└── oxigraph/                             the graph: one named graph per transcript,
                                          plus memory-v1 for the memory index

~/.config/ravel/config.yml                the daemon: port, interval, souls, memory budget
~/.config/ravel/raveld.log                what a detached raveld would have said to a terminal
~/.config/ravel/anthropic-api-key         the memory index's key, read by ravel only
~/.config/ravel/memory-spend.tsv          one line per model call, with its price
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

Three dialects: Claude Code (`~/.claude/projects/<slug>/*.jsonl`, one file per
session), Gemini/Antigravity (`~/.gemini/antigravity-cli/`, one directory per
conversation, attributed to a soul through `history.jsonl`), and claude.ai
exports (imported, never watched). The turn spine is the same for all three;
a claude.ai `human` turn is projected as `user`, the value Claude Code uses.

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
properties the kit checker would otherwise call errors. Memory nodes are
`ravel:Memory`, linked to their turns by `ravel:fromTurn` and to the nodes they
summarize by `ravel:summarizes`.

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
