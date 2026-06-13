# The reader-set and the monolog

pleeb's frame (Day 26). The signal contract says *what one reader emits*. This
says *how many readers coexist* and *where their signals live*.

## The corpus is the store; you add readers

There is no separate database engine. The conversation corpus (the raw transcript
mirror) is the canonical record. On top of it sits a **pluggable set of readers**,
each one a small process that:

1. reads some span of the corpus,
2. emits a **signal** in a fixed, greppable shape,
3. writes that signal into a parallel log — never mutating the source.

Readers are **options we try out**, the same plurality as the engine's
shape-providers: you don't pick the one true reader, you run several and calibrate
which signal is worth keeping. Adding a reader is additive; it never touches the
others or the corpus.

The Weave wave engine is itself a reader — the densest one, emitting per-word.
Other readers are coarser (per-span, per-session).

## The monolog: one log per transcript

Signals do NOT go inline into the conversation text (that mutates the canonical
record and entangles readers' ordering). They go into a **monolog** — one
append-only log *per transcript*, parallel to the raw mirror, mapping back to the
source by message/line occurrence. The name is from octobody: it is literally the
*conversation's own monologue about itself*, one voice made of many readers.

One monolog per transcript (not one file per reader) is the chosen grain: you
reconstruct a single conversation's entire signal-history from one file, and a new
reader is a new **row type**, not a new file.

```jsonc
// <transcript-id>.monolog.jsonl  — one object per line, append-only
{ "reader": "wave",     "anchor": { "seq": 1402 },        "signal": { "returnStrength": 0.3, "novelty": 0.8, "turbulence": 0.6 } }
{ "reader": "emojikey", "anchor": { "span": [1380,1402] }, "signal": "[ME|🧠🎨8∠45]~[CONTENT|💻🧩9∠15]~[YOU|🎓🌱8∠35]" }
{ "reader": "episodic", "anchor": { "span": [1380,1402] }, "signal": { "scene_ref": "obs_000412" } }
```

- `reader` — which reader produced this row (the discriminator; new readers = new values).
- `anchor` — how it maps back to the raw transcript (a `seq`, a `span`, a line/message range).
- `signal` — the reader's payload, in that reader's own shape (the wave's named
  vector; the emojikey's `ME|CONTENT|YOU` string; an episodic scene-ref).

The source transcript stays byte-faithful. The monolog joins to it by anchor.
Still greppable, still no DB engine.

## The loop: text → signal → visual → text

pleeb's insight that closes the cycle: the readers don't just store — their
signals **feed the visual memory, which feeds back into the text stream
naturally.**

- The **emojikey** reader emits an affect signal → the visual surface (CoPIA)
  renders it as a *scene* (palette/LoRA from the affect).
- The **wave** reader emits the jiggle → a deform-widget reads it.
- A rendered scene's existence is itself a corpus event → the next reader sees it.

Text produces signal; signal conditions the visual; the visual re-enters the text
stream as an event the readers can read. A closed cycle where **each layer is just
another reader/writer on the one shared corpus.**

## Candidate readers (options, not commitments)

| reader | reads | emits | feeds |
|---|---|---|---|
| **recall** | curated docs vs prompt | injected context (ephemeral, not logged) | the live turn |
| **wave** (this engine) | word-by-word | `{returnStrength, novelty, turbulence}` + confidence | deform-widget |
| **emojikey** | a span | `[ME\|…]~[CONTENT\|…]~[YOU\|…]` | scene affect (palette/LoRA) |
| **AMR/SLG** | a span | logic-graph shape | the wave's shape-provider |
| **episodic** | session/span | time-anchored scene-ref | the grid, and back into text |

The signal contract (`2026_06_13_SIGNAL_CONTRACT.md`) is the **per-reader output
contract** — the wave is its first and densest instance. Other readers reuse its
spine: lossless emit, the engine/reader declares and the viz decides, anchor +
named-payload + provenance.

## Status

Frame only (Day 26). The recall reader already ships (in the soul kit, as a
UserPromptSubmit hook). The wave reader is this repo. The emojikey reader is the
likely next proof — small enough to validate the monolog the way recall validated
the corpus-is-the-store idea.
