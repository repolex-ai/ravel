# The ingestor / detector taxonomy

What signals can we pull out of a conversation corpus, what does each one need to
run, and how cheaply? This is the breadth-map. The reader-set
(`2026_06_13_READER_SET_AND_MONOLOG.md`) says *corpus-is-the-store, add a set of
readers*; the signal contract (`2026_06_13_SIGNAL_CONTRACT.md`) says *what one
reader emits*. This doc says *which readers are worth building, in what order, and
on what stack* — and it splits them by deployability so we know which can ship in
the soul kit by default and which need special setup.

Status: catalog + one shipped stub (the emojikey reader, `src/weave/readers/`).
pleeb's frame, Day 27. Research-grounded (SOTA survey saved in spaceGOAT soul,
`2026-06-15-sota-signal-extraction-survey`).

---

## Two layers: ingestor vs detector

These are different jobs and the taxonomy keeps them separate.

- **Ingestor** — turns a raw transcript into a clean prose stream. ONE per corpus
  source. It is the substrate every detector runs on. (Built: `src/weave/ingest`.)
- **Detector** (a.k.a. *reader*) — reads the prose stream (or a span of it) and
  emits a signal into the monolog. MANY per corpus; options we try and calibrate.

A detector often *needs* a specific ingest shape or a paired sub-detector. The
emojikey **detector** needs nothing but text; a *generative* emojikey detector
needs a small LLM; an emotion detector might need a lexicon (cheap) OR a
transformer (less cheap) OR a zero-shot LLM (flexible, pricey). Naming the pairing
is half the work — see the table's "needs" column.

### The ingestor is also a required FILTER (Sylkie's caveat)

The Raw mirror (`<soul>/Raw/ClaudeCodeSessionLog/*.jsonl`, written by git-lex's
raw-mirror adapter on every `git lex save`) is the WHOLE transcript: tool calls,
tool results, system reminders, file-history snapshots, permission flips,
queue-ops, ai-titles. Sylkie, learned feeding CoPIA's Pool:

> *run a role+text filter before your detectors or signal drowns in noise.*

Measured on a real 478-line spaceGOAT session: **478 raw lines → 106 prose blocks**
after the structural filter (keep user/assistant `text`, drop everything else).
Then a SECOND content filter (strip harness-injected `<channel>` peer messages,
`<command>`/`<system-reminder>` wrappers, `Caveat:` preambles) cut the apparent
human turns **28 → 12** — i.e. **57% of "human" turns in that session were not
the human speaking.** The ingestor owns both filters (`load(strip_wrappers=True)`);
no detector should ever parse raw JSONL itself. This is non-negotiable
preprocessing, not a nicety.

---

## The deployability split (pleeb's frame): GENERAL vs SPECIFIC

This is the axis that decides where a detector can live.

- **GENERAL** — simple text-processing: pure-Python / regex / lexicon / a pip
  install with no GPU. Hook-safe (runs in a `UserPromptSubmit`/`SessionEnd` hook
  in well under a second, fails soft). **Can be shipped in the soul kit by
  default**, the same way the memory-recall hook was — every soul gets it for free.
- **SPECIFIC** — needs an exotic model running, a GPU, a separate server, or a
  heavyweight dependency. Requires special handling / setup / a specific
  application already running. **NOT a kit default**; opt-in per soul/per repo.

A third, forward-looking option spans the split:

- **CENTRAL SERVER** — one process that does the heavy detection for all agents
  (a shared `mlx_vlm.server`-style endpoint, or a GLiNER2/emotion model loaded
  once and queried over HTTP). This turns a SPECIFIC detector into something
  *kit-default-able* for the squad — but **until that server actually exists and
  is reachable, anything depending on it stays in the SPECIFIC column.** Same
  pattern already used for Qwen in the subtexture observer (shared server
  serializes the GPU across consumers). Worth building once 2+ detectors want the
  same model.

### Effort + quality ratings

- **Effort to spin up:** LOW (pure text / pip, no GPU) · MED (small model,
  manageable) · HIGH (GPU server / heavy model / exotic setup).
- **Signal quality (theoretical):** how accurate/useful the extracted signal is.

---

## The catalog

Build order is roughly top-to-bottom: cheap-and-shippable first, exotic-and-deep
last. The wave engine (this repo's reason to exist) is its own densest reader and
is tracked separately (#140); it is listed here for completeness.

### GENERAL tier (kit-default candidates — pure text, hook-safe)

| detector | signal | needs (paired detector / backend) | effort | quality | status |
|---|---|---|---|---|---|
| **emojikey (harvest)** | inline `ME\|CONTENT\|YOU` keys already in the text | regex only | **LOW** | MED (depends on agent emitting them) | ✅ **shipped** (`readers/emojikey.py`) |
| **VADER sentiment** | polarity (pos/neu/neg + compound), social-tuned | `vaderSentiment` (pure-Python lexicon) | **LOW** | LOW-MED | catalog |
| **NRC EmoLex / NRC-VAD** | Plutchik-8 emotion + valence-arousal-dominance | word-lookup table | **LOW** | LOW-MED | catalog |
| **dialogue-act (4-class)** | Inform / Question / Directive / Commissive (ISO 24617-2 reduced) | regex + small lexicon, or embed+LR | **LOW** | MED | catalog |
| **structural / cadence** | turn lengths, question density, code-block ratio, latency | pure-Python over PoseRecords | **LOW** | MED (cheap context signal) | catalog |
| **embedding + logistic-reg** | any custom label you have ~200-500 examples for | MiniLM (CPU) + sklearn | **LOW-MED** | MED-HIGH (with labels) | catalog |

### SPECIFIC tier (opt-in — needs a model / GPU / server)

| detector | signal | needs (paired detector / backend) | effort | quality | status |
|---|---|---|---|---|---|
| **emotion (transformer)** | Ekman-6 / GoEmotions-27, contextual | `j-hartmann/emotion-english-distilroberta` (~82M, CPU-OK but model dep) | **MED** | MED-HIGH | catalog |
| **GLiNER2 (entities + class + schema-fill)** | zero-shot NER + classification + structured extraction in one model | GLiNER2 (DeBERTa-v3 encoder); CPU-capable; **Rust/ONNX path `gline-rs`** | **MED** | HIGH | catalog — *adell/lupov spiking the Rust port now* |
| **emojikey (generative)** | MINT a `ME\|CONTENT\|YOU` key for a span that has none | small LLM (Qwen3/Haiku) + constrained JSON | **MED-HIGH** | MED | catalog (pleeb's automated-emojikey-agent idea) |
| **small-LLM zero-shot tagger** | arbitrary tags via prompt→JSON (intent, custom signals) | local LLM + XGrammar, or hosted API | **LOW (API) – HIGH (self-host)** | MED (LOW-MED for nuanced emotion) | catalog |
| **episodic / visual scene** | time-anchored scene-ref (feeds CoPIA palette/LoRA) | Qwen3-VL via shared `mlx_vlm.server` | **HIGH** | HIGH | partly live (subtexture observer wires Qwen) |
| **AMR semantic graph** | full predicate-argument graph (who-did-what-to-whom) | `amrlib` BART-large, GPU for throughput | **HIGH** | HIGH (overkill for per-turn) | catalog — also the wave's AMR shape-arm (#144) |
| **persona-state / drift** | LLM persona-state, drift over a conversation | activation-space methods (Anthropic persona-vectors; Lu et al. 2026) — **white-box, needs model internals** | **HIGH** | HIGH (rigorous) but white-box only | research-only |
| **wave engine** | per-word `{returnStrength, novelty, turbulence}` + confidence | this repo + a ShapeProvider arm | **MED-HIGH** | (the project) | building (#140) |

---

## Bootstrap-then-distill (the architectural pattern)

The SOTA survey is clear on one thing: **fine-tuned dedicated classifiers beat
zero-shot LLMs once labels exist** — dramatically for nuanced signals (emotion,
stance: a zero-shot LLM scored ~0.15 F1 on anger where a small encoder scored
~0.89). So the path for a *new* signal type is:

1. Use a small LLM zero-shot (SPECIFIC, flexible) to bootstrap labels + prove the
   signal is real and worth keeping.
2. Distill into a cheap dedicated detector — GLiNER2 / DistilRoBERTa / embed+LR —
   which can then potentially **drop into the GENERAL tier or a central server.**

This is how a signal *migrates leftward* across the deployability split over time:
prove it expensive, ship it cheap.

---

## On "Claude moods" (Wet Claude / Stabby Claude)

Confirmed (research + Sylkie): the named-persona folksonomy is **community/Discord
vibes-vocabulary, not research** — "Wet Claude" is a documented meme, not an
Anthropic feature, no academic taxonomy classifies named LLM "moods." Use it as
**flavor in the UX, never as a detector foundation.**

The *rigorous* version of the same phenomenon does exist but is **white-box**
(needs model activations, not transcript text): Anthropic's persona-vectors /
persona-selection-model, and the persona-drift literature (Lu et al. 2026 —
measurable decline along an "Assistant Axis"; activation-velocity). A
transcript-only black-box "mood" detector is therefore **novel-but-unvalidated
work**, not established SOTA: it would be the small-LLM-zero-shot or
emotion-transformer detector pointed at a *custom persona schema*. Ground the
emotion signal in **dimensional VAD + Plutchik** (the legit foundations), and keep
the cute names for the surface.

---

## What ships in the kit, and when

- **Now-shippable (GENERAL):** emojikey-harvest is built and lifts clean into a
  hook the way memory-recall did. VADER / NRC / cadence are next — all pure-Python,
  all kit-default candidates. These give every soul a baseline signal layer for free.
- **Opt-in (SPECIFIC):** emotion-transformer, GLiNER2, generative-emojikey,
  episodic-scene — added per soul/repo that wants them and can run the backend.
- **The migration goal:** stand up a **central detection server** so the
  highest-value SPECIFIC detectors (GLiNER2 especially — adell/lupov's Rust port is
  the live path) become squad-wide defaults without every soul pinning a model.

The monolog is the same for all of them: one `{reader, anchor, signal}` row per
emission, one log per transcript, joinable back to byte-faithful source. A new
detector is a new `reader` value, never a new store.
