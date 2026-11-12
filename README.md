# ravel

ravel keeps every conversation an agent has and turns it into a graph you can
query. It watches the transcript files that Claude Code and Gemini/Antigravity
write, imports Claude desktop (claude.ai) data exports, copies each into the
agent's own repository, and projects the copy into an RDF 1.2 graph beside it.
The bytes are canonical; the graph is a projection of them and can always be
rebuilt.

On top of the transcripts sits the agent's biography: its life so far. Its
first section is optmem, the story the agent wrote for itself: ravel's adaptation of
[OptMem](https://github.com/VictorTaelin/OptMem), the memory architecture by
Victor Taelin, kept as a graph. One-line notes of what the agent learned,
in the order it learned them, merged pairwise up OptMem's binary tree, so the
agent wakes with its whole history in at most 96 lines and can open any line
down to the conversation turns it rests on.

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
#   memory: written by claude-code (claude-haiku-5-5) on the subscription, every 10 min
#   9bdf2a  /Volumes/f00/repos/7R1PL3F0RC3/spaceGOAT
#       last pass 2026-10-04T16:11:50-07:00  mirrored 1 (unchanged 589), ingested 1 session(s) ...
#       memory 2026-10-04 16:10: 9 notes and 7 merges from 63 turns

# 2. Inside a soul repo: its backup's health, its store, its memory. Exit 1 means look.
ravel status

# 3. What the graph holds: `ravel query help` shows every class and property,
#    where things are stored (every pattern goes inside GRAPH), and three examples
ravel query help
ravel query 'SELECT (COUNT(DISTINCT ?t) AS ?turns) WHERE { GRAPH ?g { ?t a <https://repolex.ai/ontology/ravel/Turn> } }'

# 4. Another soul, by its six-character id or a path
ravel status 700c5b
ravel status ~/repos/SQUAD/lUX

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
`~/.config/ravel/raveld.log`. The log caps itself: past 100 MB, raveld
keeps the newest 50 MB.

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

## The biography, and optmem

`ravel biography` is a soul's life so far, read at wake. It has one section
per memory architecture ravel runs. Today that is optmem alone, the story the
soul wrote for itself; other architectures, or a statistics section, can
follow.

optmem is ravel's adaptation of [OptMem](https://github.com/VictorTaelin/OptMem),
the memory architecture by Victor Taelin, kept as a graph. It follows OptMem as
closely as ravel can, but it is an adaptation, not OptMem itself. The full guide is [docs/2026_10_13_MEMORY.md](docs/2026_10_13_MEMORY.md).

- **Notes.** One line each, at most 280 bytes. The soul notes something
  whenever it learns something new, or something worth keeping happens. That
  covers a task worth real effort, a fact or insight its human teaches it,
  anything it learns about their life, and any event of lasting effect. It
  does not note what it already holds. Each note's position in the log is its
  identity: `#0, #1, …`.
- **Merges.** OptMem's binary tree over the log, by position: `#0-1`, `#2-3`,
  then `#0-3`, and so on. Neighbours are merged, never grouped by topic or
  time. A block of up to 16 notes is merged from the notes themselves; a bigger
  one from its two halves. Every merge is done the moment it comes due, with
  OptMem's prompt word for word: "Keep what has lasting effect, drop what does
  not. Invent nothing."
- **Wake.** OptMem's cover: the whole log in at most 96 lines. Recent notes are
  shown word for word, older ones through their merges.
- **The graph.** Every note and every merge is a `ravel:Memory`. A note links
  to each conversation turn it rests on, and a merge links to its two halves,
  all with `git-lex:relatedToId`.

```sh
ravel biography          # the whole story, read at wake: optmem's index today
ravel optmem                 # what optmem is, where this soul's stands; short
ravel optmem wake            # optmem's index alone, to get your bearings again
ravel optmem open 16-31      # a merged block, opened into its two halves
ravel optmem open '#12'      # a note, with the turns it rests on
ravel optmem recall kohya    # where a word appears across the soul's life
```

```
# 4RX (d5af17): biography

## optmem: ravel's adaptation of OptMem, the memory architecture by Victor Taelin (https://github.com/VictorTaelin/OptMem)
42 notes, 39 merges. Open a line by its label: `ravel optmem open <label>`.
#0-31 Kira: parser; me: forx+storage. IRI repolex.ai/r/{org}/{repo}/{phase}/{sha}. Repolex core never public-runner; …
#32-39 …
#40 2026-09-07 actix-web -> bytes 1,138 call edges, actix-web -> http 426, rlex -> actix-web 16, …
#41 2026-09-07 My repo and release counts drifted across reports (19, 29, 24, 26). Quote quad totals, …
```

The lines are OptMem's own format: `#i date text` for a note and `#lo-hi
text` for a merge. `tests/optmem_conformance.rs` runs OptMem's `memo` beside
ravel and requires the same bytes for every merge prompt and every wake view.

### Keeping it up to date

raveld writes every soul's memory itself, on the subscription you already
have. Turn it on with one block in `config.yml`:

```yaml
memory:
  writer: claude-code        # the harness whose command line writes the memory
  model: claude-haiku-5-5    # optional; this is the default
```

Without `memory:`, raveld calls no model and only shows the memory that is
there.

Every ten minutes, for each soul whose transcripts gained turns, raveld:

1. reads the turns its memory has not read yet, as one stream ordered by time;
2. leaves anything younger than 30 minutes for later, so a conversation still
   going on is not noted halfway, and waits until the rest fills a stretch of
   about 24,000 characters, or until its oldest turn is six hours old;
3. relives that stretch as the soul. It wakes as OptMem wakes, notes what the
   soul would have noted, and does every merge that came due, with OptMem's
   prompt plus one paragraph of ravel's (see "Merges read as a story" below). The writer is given the soul's name and SOUL.md, because in OptMem
   the agent writes and merges its own memory;
4. rebuilds any merge the checks refuse, with every merge above it.

**Merges read as a story.** OptMem's merge prompt turned a year of a soul's
work into a list of rules that named no project. ravel adds one paragraph,
`STORY` in `src/memory_log.rs`, asking for what happened: the work, the
projects the notes name, who did each thing, and what was left unrun or
unverified. It was chosen over three rounds of blind audits against OptMem's
own wording. Every merge records which wording wrote it. With `reword:` in
the config, raveld rewrites merges left in an older wording, up to 40 per
soul per pass and only after any new turns:

```yaml
memory:
  writer: claude-code
  reword: [h4nk, w0z]   # or on, or off (the default)
```

A first pass catches up the whole past, so a new soul's memory is built the
same way. At most 60 model calls go to one soul per pass, so one long
catch-up never holds the other souls back.

**On the subscription.** The writer is the harness's own command line, run
with no tools, no settings and no project, in an empty directory. For Claude
Code that is `claude -p`; `ANTHROPIC_API_KEY` is removed from its environment,
and a call that used an API key anyway is refused. When the subscription
reports its usage limit, every soul waits 30 minutes, and `ravel` says so.

**Writers.** ravel does not need Claude. Any harness whose command line can
answer one prompt with structured output, on the user's own login, with no
tools, and without saving its session where ravel reads transcripts, can be a
writer. Claude Code is built. Codex, Gemini CLI, Antigravity and Hermes are
named in the config and refuse with what each one still needs
(`src/writer.rs`).

**What the reader is shown:**

- Only the soul's own life: what the soul said, what its human said
  (labelled by name, from `human:` in config.yml), and messages from other
  agents (labelled by sender).
- What the soul did is shown beside what it said: each tool call as one
  line, with how it came out, for example
  `[ran: Count banned words (rg -c face */test.json) → 0 matches]`. A read
  or an edit says nothing of the file unless it failed. Never the output
  itself (#27: without these lines, a check the soul ran looked like one it
  never did).
- Thinking and compaction summaries are left out.
- Claude Code subagent and workflow transcripts are left out, and so are
  Antigravity subagents. In OptMem, a subagent never writes memory.
- Harness records that arrive as user input are left out: notifications,
  slash commands, system reminders and the like.
- A message another soul typed in through herdr is recognised from the
  sender's side: the sender's own transcript holds the `herdr agent prompt`
  that sent it. The senders are the other souls in `config.yml`.

**`ravel optmem check`** tests a soul's optmem end to end, and exits 1 on any
failure:

- every line passes OptMem's limits;
- every merge that is due is built;
- every note cites turns of the soul's own stream;
- the wake is OptMem's cover;
- the graph projection loads;
- every cited turn is a Turn in the soul's store.

## What lives where

```
<soul repo>/.ravel/_ignore/               machine-local, gitignored — never git history
├── transcripts/claude-code/*.jsonl       byte-for-byte copies of Claude Code sessions
├── transcripts/agy/<conversation>.jsonl  copies of Gemini/Antigravity conversations
├── transcripts/claude-ai/<export>/       claude.ai exports, as downloaded
├── ingest-manifest.tsv                   what has been ingested, by size (claude-code)
├── ingest-manifest-agy.tsv               what has been ingested, by content hash (agy)
├── ingest-manifest-claude-ai.tsv         each conversation's ingested version (claude.ai)
├── optmem/                               the memory: append-only, canonical
│   ├── memories.jsonl                    OptMem's log: one note per line, in order noted
│   ├── summaries.jsonl                   OptMem's tree: one merged block per line (`16-31`)
│   ├── extracted.tsv                     ids of turns already read, one per line
│   ├── dropped.tsv                       lines the checks refused, and stretches left blank
│   ├── stream.json                       how many turns the reader's stream held at the last pass
│   └── usage.tsv                         tokens per model call
└── oxigraph/                             the graph: one named graph per transcript,
                                          plus memory-v1 for the memory index

~/.config/ravel/config.yml                daemon config: port, interval, souls, the human's name
~/.config/ravel/raveld.log                log output from detached daemon (capped at 100 MB)
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
Queries filter; ingest never drops. That is why `ravel status` counts
"unasserted claims" separately from everything else.

## The ontology

`ontology/ravel/ravel.ttl`. The copy shipped in
[git-lex-kit-ravel](https://github.com/repolex-ai/git-lex-kit-ravel) must be
byte-identical; CI checks it on every push, along with the six graph-only
properties the kit checker would otherwise call errors.

The store partitions data into named graphs under `https://repolex.ai/ravel/NamedGraph/`:
- **Transcripts**: Each transcript has its own named graph (`<…/NamedGraph/<transcript_id>>`) containing `ravel:Turn` nodes linked by `ravel:parentTurn`.
- **Memory**: projected into `<https://repolex.ai/ravel/NamedGraph/memory-v1>`.
  - Every node is a `ravel:Memory`, carrying `ravel:memoryId`,
    `ravel:memoryLevel` (0 for a note, k for a block of 2^k notes),
    `ravel:logStart` and `ravel:logEnd` (its positions in OptMem's log),
    `ravel:memoryText`, `ravel:windowStart` and `ravel:windowEnd` (the times of
    its first and last note), and `ravel:memoryModel`.
  - Every link is `git-lex:relatedToId`: a note to each turn it rests on, a
    block to its two halves.

## Health

`ravel status <soul>` is the one diagnostic, and it is read-only. It reports the
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
