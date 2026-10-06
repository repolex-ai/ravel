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

Commands default to the soul repository you are currently in. To query or inspect another soul from anywhere, pass its six-character genesis ID or repository path (`ravel <command> <soul>`).

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

# 5. List all registered souls and sync status
ravel souls

# 6. Trigger an immediate sync pass (raveld runs every 30 seconds anyway)
ravel sync

# 7. The daemon itself
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
# Inside the soul repo:
ravel import ~/Downloads/data-2026-04-06-10-54-07-batch-0000

# Or from anywhere, passing the soul ID or path first:
ravel import 9bdf2a ~/Downloads/data-2026-04-06-10-54-07-batch-0000
# [ravel import] 9bdf2a data-2026-04-06-10-54-07-batch-0000: copied (4 file(s), 267818192 bytes)
# [ravel import] 9bdf2a claude.ai: 1 export(s) → ingested 1431 conversation(s), 31353 turns (0 already current)
```

The export is copied byte for byte into `.ravel/_ignore/transcripts/claude-ai/`.
Importing the same export again is a no-op (`already imported, nothing copied`).
When you download an updated export later, ravel ingests the conversations that
are new or have grown and skips the rest (the count in `(N already current)`). An
older export imported after a newer one never rolls a conversation back.

## The memory index

The memory index gives an agent its full conversational history in about 96
lines: recent memories word for word, older ones rolled up into hierarchical
summaries.

```sh
ravel memory                  # the whole history in about 96 lines
ravel memory open w0-33830    # a window summary opened into the lines under it
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

### Reading and drilling down

- **Line tags**: Each line ends with an identifier in brackets:
  - `[m<hash>]` — an atomic level-0 memory extracted directly from conversation turns.
  - `[w<k>-<index>]` — a summary covering an aligned time window of $2^k$ hours (e.g. `w0` covers 1 hour, `w1` covers 2 hours, `w2` covers 4 hours).
- **Drill-down with `ravel memory open <id>`**:
  - Opening a window summary (`w0-33830`) expands it into the child memories or smaller window summaries under it.
  - Opening an atomic memory (`mec170751e01cedc`) prints the memory text followed by the exact transcript turns (speaker role, timestamp, turn ID, and dialogue excerpt) that produced it.
- **Status markers**: A bracketed prefix at the start of a line (`[not summarized yet — N memories]` or `[stale — N memories under it now]`) indicates that the window is waiting for a summarization pass. The view strictly adheres to its ~96 line budget rather than dumping unsummarized memories into the terminal.

### How it works

1. **Turn extraction**: Every ten minutes, `raveld` inspects new turns in the soul's transcripts. It extracts only authored prose from the human and the agent (skipping tool output and scratchpads), taking each turn once. A model writes concise, one-line past-tense facts (decisions, commands, errors, versions, results), each referencing the turn IDs it rests on (`git-lex:relatedToId`).
2. **Hierarchical rollup**: Memories are grouped into aligned time windows of $2^k$ hours counted from 2020-01-01T00:00:00Z. Level-0 windows (`w0`) summarize an hour's memories; higher levels summarize their two child windows (`git-lex:relatedToId`).
Because windows align to absolute time boundaries, backfilling historical transcripts only invalidates and re-summarizes the specific windows the imported turns land in, leaving the rest of the tree untouched.

### Cost controls and setup

The memory index is **completely disabled by default**; it calls no models and spends nothing until configured.

To enable it:

1. **Save your Anthropic API key** in `~/.config/ravel/anthropic-api-key` and restrict permissions:
   ```sh
   chmod 600 ~/.config/ravel/anthropic-api-key
   ```
   Ravel reads the key directly from disk; it is never read from environment variables to prevent accidental leaks to child processes.
2. **Set a dollar budget** in `~/.config/ravel/config.yml`:
   ```yaml
   memory_budget_usd: 50
   ```
3. **Restart the daemon**:
   ```sh
   raveld restart
   ```

Ravel uses Claude Haiku 4.5 ($1.00 input / $5.00 output per million tokens). Every call logs its token counts and price to `~/.config/ravel/memory-spend.tsv`. When the total spent reaches `memory_budget_usd`, `raveld` makes no new calls (calls already in flight finish, so the total can pass the budget by a few cents). For one squad of 20 souls, the first full read was $99 and still running on 2026-10-04, heading for about $120–130; once caught up, passes read only new turns.

## What lives where

```
<soul repo>/.ravel/_ignore/               machine-local, gitignored — never git history
├── transcripts/claude-code/*.jsonl       byte-for-byte copies of Claude Code sessions
├── transcripts/agy/<conversation>.jsonl  copies of Gemini/Antigravity conversations
├── transcripts/claude-ai/<export>/       claude.ai exports, as downloaded
├── ingest-manifest.tsv                   what has been ingested, by size (claude-code)
├── ingest-manifest-agy.tsv               what has been ingested, by content hash (agy)
├── ingest-manifest-claude-ai.tsv         each conversation's ingested version (claude.ai)
├── memory/                               the memory index logs: append-only, canonical
│   ├── memories.jsonl                    level-0 memories extracted from turns
│   ├── summaries.jsonl                   hierarchical window summaries
│   └── extracted.tsv                     ids of turns already read, one per line
└── oxigraph/                             the graph: one named graph per transcript,
                                          plus memory-v1 for the memory index

~/.config/ravel/config.yml                daemon config: port, interval, souls, memory budget
~/.config/ravel/raveld.log                log output from detached daemon
~/.config/ravel/anthropic-api-key         Anthropic API key for memory index (chmod 600)
~/.config/ravel/memory-spend.tsv          audit ledger: timestamp, soul, tokens, USD per call
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
properties the kit checker would otherwise call errors.

The store partitions data into named graphs under `https://repolex.ai/ravel/NamedGraph/`:
- **Transcripts**: Each transcript has its own named graph (`<…/NamedGraph/<transcript_id>>`) containing `ravel:Turn` nodes linked by `ravel:parentTurn`.
- **Memory index**: The memory index projects into `<https://repolex.ai/ravel/NamedGraph/memory-v1>`. Memory nodes are `ravel:Memory`, carrying `ravel:memoryId`, `ravel:memoryLevel`, `ravel:memoryText`, `ravel:windowStart`, and `ravel:windowEnd`. Every link is `git-lex:relatedToId`: a level-0 memory to each source turn, a window summary to each child node. What a link means is read off its target (a `ravel:Turn` is the source; a `ravel:Memory` one level down is a child).

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
