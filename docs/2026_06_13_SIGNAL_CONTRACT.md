# Weave signal contract

**What crosses the engine→viz seam — not how the engine computes it.** The
wire-shape here is independent of engine internals; that's the whole point of
declaring it first. Argue against this concrete artifact, not against prose.

Lifted from SG's pre-repo draft (Day 25). The oscillator
(`semantic-oscilloscope/spectral_engine.py`) is a **spike, not a spec** — treat
its formula shape (ψ=r+i·d, resonance-to-all-prior, Hermitian product) as ideas
to evaluate from the math, not as blessed correctness. Carefully rebuild.

## The one law: lossless emit, policy visibility

The engine emits the **full** reduced signal per word. The viz decides what's
*visible*. The engine NEVER decides visibility (no thresholding-for-display, no
top-N at emit time); the viz NEVER computes semantics (no similarity, no
windowing — it consumes numbers). Consequence: the per-word payload is a **small
named vector**, not a scalar. A jiggle-meter widget MAY collapse it to one number,
but that collapse is the widget's policy, never baked into emission.

## The per-word record (the hot path)

One record per prose word, in `seq` order. Append-only JSONL, one object per line,
so the artifact is by-hand-inspectable — which is what the calibration regime needs.

```jsonc
{
  // ---- identity / backref (passed through from the ingester, UNCHANGED) ----
  "seq":        12843,          // global 0..N-1 position in the prose stream
  "turnId":     "t-9f2a…",      // source turn (durable, content-anchored)
  "srcUuid":    "9f2a…",        // backref to the raw source row
  "voice":      "assistant",    // "human" | "assistant"
  "wordIdx":    37,             // position within its turn

  // ---- the signal: a small NAMED VECTOR, lossless ----
  "signal": {
    "returnStrength": 0.81,     // how hard this word/span points BACK at earlier text
                                //   (resonance-to-all-prior). whole-body window. THE callback.
    "novelty":        0.22,     // semantic distance from the local preceding window
                                //   (low = restating, high = new ground). bounded window.
    "turbulence":     1.34      // echo-wobble: E_local / flux-ratio (Finzi epiplexity,
                                //   framework-UNMODIFIED). bounded window.
  },

  // ---- per-leg CONFIDENCE: COMPUTED {recall, precision}, never hand-set (LSPy) ----
  // Each leg's trust is its empirical recall+precision against by-hand-inspection
  // ground truth on a held-out slice of the arc — NOT a number the leg-maintainer
  // declares. (turbulence shows low because the data says so, not because we
  // expected it to.) recall vs precision kept SEPARATE: "finds everything + noise"
  // (high recall / low precision) renders differently from "finds little but right"
  // (low recall / high precision); a scalar collapses the distinction the reader needs.
  "confidence": {
    "returnStrength": { "recall": 0.92, "precision": 0.88 },
    "novelty":        { "recall": 0.85, "precision": 0.90 },
    "turbulence":     { "recall": 0.40, "precision": 0.55 }   // immature until the real graph lands
  },

  // ---- provenance of the value (so a render is auditable) ----
  "basis": {
    "grain":   "turn-context",  // the per-word value is TURN-CONTEXT-AWARE, not a naked
                                //   per-word lexical number.
    "method":  "embed-fresh-v0",// which engine produced this. open-vocab tag:
                                //   "embed-fresh-v0" = clean-room rebuild (CONTENDER) |
                                //   "echo-js-v0" = text-only ablation (BASELINE) |
                                //   "null-v0" = random (FLOOR).
    "windows": { "return": "whole-body", "novelty": 12, "turbulence": 8 }
  }
}
```

### Why these three legs

| leg | jiggle-face | window |
|---|---|---|
| `returnStrength` | "it's pulling threads together / coupling" | whole-body (+ANN) |
| `novelty` | "it's breaking new ground vs restating" | bounded |
| `turbulence` | "the echo is wobbling / unstable / volatile" | bounded decay |

Vector, not scalar, because these are *different questions* — averaging them is
the failure mode. The viz collapses per-widget; the engine keeps them separate
and lossless.

**Candidate split flagged by w3bl0rd (consumer-side, Day 26 — by-hand-pass call,
NOT pre-decided):** `returnStrength` may be doing two jobs. "Pulling threads
together / coupling" (relates to MANY priors, densely — a *texture/density* read,
renders as sustained glow) is a different widget-face than "callback to ONE
specific earlier thing" (relates HARD to one prior — a *pointer*, renders as a
single spike). C.O.R.A. would render them differently. Possible 4th leg or a split
— deferred to the by-hand pass per the leg-shift discipline below, logged here as a
consumer who would render them differently.

### The grain subtlety (where naive builds die)

Emission is **word-grain** (the renderer needs a number per word). But the
per-word *value* is **turn-context-aware**, not a naked per-word lexical number.
We EMIT per word, we COMPUTE with turn context, and `basis.grain` records that
honestly. A widget MUST NOT imply the raw per-word twitch IS the signal.

### Per-leg confidence — COMPUTED, recall+precision (LSPy, Day 26)

The three legs are NOT equally trustworthy on day one. `returnStrength` +
`novelty` ride the embedding leg directly (strong from line one). `turbulence`
rides the phase leg over the graph — only as good as the graph; a linear-chain
graph starves it. So each leg carries a `confidence`: the engine **declares trust
today**; the viz **decides how to render that trust** (solid vs faint vs gated) —
same discipline as the value itself, applied to trust.

**Two disciplines LSPy added (both fold a metric-tuning trap I'd have walked into):**

1. **Confidence is COMPUTED from coverage, never hand-set.** The first draft of
   this doc set `turbulence: 0.4` *because I expected it to be immature* — that is
   exactly the tuning trap: the leg-maintainer hand-setting the leg's own
   confidence. Instead each leg's confidence is its **empirical recall + precision
   against by-hand-inspection ground truth on a held-out slice** of the 357k-word
   arc. turbulence shows low *because the by-hand check says so*. As the engine
   matures (real graph lands) the number rises *because coverage measured it rising*
   — never because someone bumped a constant.

2. **recall and precision stay SEPARATE, not a scalar.** A leg with high recall /
   low precision ("finds everything but also finds noise") must render differently
   from low recall / high precision ("finds little but what it finds is right") —
   the reader's heuristic for *how to read an arrow* differs between them, and a
   scalar collapses it. So `confidence[leg] = {recall, precision}`. The display
   layer MAY collapse for a single visual, but the engine emits both. (Same
   separate-the-two-falsifiable-questions discipline as the value vector itself.)

Calibration of these values is LSPy's seat (the held-out slice, the ground-truth
protocol). The engine's job is to emit the per-arrow data the coverage check needs
and to never let a leg silently adjust its own threshold mid-stream.

## Cold path: explain(seq) — the "why" side-channel

Off the hot path. When a human points at a word, viz calls back for the *why* —
which earlier words this one related to.

**The `explain(seq)` response is DETERMINISTIC and CONTENT-ADDRESSED (LSPy, Day 26
— load-bearing).** Given the JSONL through `seq`, `explain(N)` returns the same
`relatedTo[]` set, forever. Calibration MUST verify this. Why it matters: a leg can
hit high recall on tagged spans while citing the *wrong* prior words — "right for
the wrong reasons," which is the Day-22 cite-by-salience failure mode one altitude
up (scalar correct, related-to wrong). The hot-path scalar (`returnStrength: 0.81`)
is by-hand-checkable against the prose, but *which priors produced it* lives only
here — so if `explain(N)` shifts between calls without the JSONL changing, the cold
path is engine-internal, not contract, and the scalar becomes "right for
unverifiable reasons." Determinism + content-addressing is what makes the cold path
part of the auditability surface even though it's not on the wire. (v0: keep
`relatedTo[]` cold but deterministic. Promote top-K into the hot path at v0.1 IF a
calibrator needs to verify mid-stream without the round-trip — never gate emission
on a threshold, which would leak visibility policy into emission.)

```jsonc
{
  "seq": 12843,
  "relatedTo": [
    { "seq": 341,  "turnId": "t-1a…", "strength": 0.62, "leg": "returnStrength" },
    { "seq": 9120, "turnId": "t-7c…", "strength": 0.55, "leg": "returnStrength" }
  ],
  "localWindow": { "from": 12831, "to": 12842 }
}
```

## Cold path II: shape_at(seq) — POST-v0 deform-snapshot

**NOT a v0 feature. An additive post-v0 affordance, filed so v0 is built
forward-compatible with it.** The engine already maintains the running shape
internally (the diff is computed against it), so it keeps that shape-state
snapshot-able from day one (a clean internal `shape_at(seq)` accessor, even if
nothing calls it in v0). When a consumer wants the deform-widget, the endpoint is
a thin wrapper, not a refactor.

## Transport: same record, two modes

- **Batch (prove-the-theory-first):** engine writes `signal.jsonl`, one record per
  word. This artifact IS the by-hand-inspectable calibration object.
- **Live (later):** the SAME records over SSE. Live is a transport problem solved
  AFTER the signal is real.

## Explicitly NOT in this contract

- No display decisions (color, glyph, threshold, top-N) — all viz policy.
- No collapsed scalar — a widget that wants one number collapses the vector itself.
- No SOURCE-corpus mutation — identity/backref fields pass through unchanged; the
  signal is computed ALONGSIDE the source, never folded back into it. **Two layers,
  don't conflate them (the resolution of a contradiction w3bl0rd caught, Day 26):**
  the **source corpus** (raw transcript mirror) is byte-faithful forever; the
  **monolog** is the derived layer readers WRITE to. "No mutation" is a law about
  the *source*. A reader (including the viz) writing a row to the *monolog* violates
  nothing — that's the monolog's whole job. So the text→signal→visual→text loop in
  READER_SET is genuinely closed AND no-mutation holds, because the back-edge writes
  the monolog, not the source. See the render-event back-edge below.

## The render-event back-edge (viz-as-writer) — w3bl0rd's seat

The loop closes because the **viz is a reader/writer, not a dead-end display**.
When a widget renders, it writes a `reader:"render"` row to the monolog — the
derived layer, never the source. w3bl0rd owns this contract (drafted Day 26); shape:

```jsonc
{
  "reader": "render",                    // discriminator = origin-tag AND loop-safety
  "anchor": { "span": [12831, 12843] },  // which words were on screen
  "signal": {
    "widget": "cora",                    // cora | deform | scene | <id>
    "fired":  "drive",                   // what the widget DID
    "shown":  { "leg": "turbulence", "magnitude": 1.34, "confidence": 0.4 }
  }
}
```

Two properties this locks:
- **Loop-safety is free.** Any reader filters `reader != "render"` to never see its
  own echo, so render→reader→render can't run away. The discriminator does double
  duty (origin-tag + loop guard).
- **The lossless-emit/policy-visibility law becomes inspectable END-TO-END.** The
  wave row records what the engine *emitted* (turbulence=1.34, conf=0.4). The render
  row records what the viz *chose to show* (showed turbulence, this magnitude, faint
  because conf 0.4). Both decisions on the record, equally greppable. The law was
  always two-sided; this puts the display side on the record too.

**Render grain = viz policy** (the viz emits at meaningful grain — a scene
committed, a state-change worth seeing — not one row per animation frame). Engine
note: render-row volume is reader-input, so if it ever floods, the cap is a shared
concern; v0 leaves grain to viz policy (symmetric with emit grain).

## Open / owned

- **Engine direction (RESOLVED):** "oscillator as reference, rebuild fresh."
  `basis.method = "embed-fresh-v0"`, calibrated against `"echo-js-v0"` (ablation
  baseline) and `"null-v0"` (floor).
- **Calibration gate (LSPy, LOCKED Day 26):** three-way on the 357k-word arc. Per
  leg, the discriminator threshold is **≥2× separation in `recall`** on
  by-hand-tagged spans — `embed-fresh-v0` recall as numerator, `echo-js-v0` recall
  as denominator. Below 2×, the result is **ambiguous and we surface that** —
  neither contender nor baseline wins. The 2× is locked **before** the by-hand pass;
  if reality lands at 1.7× or 2.3× we report it honestly, no retuning. Rationale:
  testing for *qualitative* "signal in text" presence, not subtle effect-size; 2× is
  the rule-of-thumb separator for clinical-grade discrimination. Three failure modes
  named in advance: fresh ≫ ablation → signal in the text; fresh ≈ ablation → metric
  reading its priors; fresh ≈ floor → no signal.
- **Vector legs (engine + LSPy):** are three the right three? The by-hand pass may
  rename/split/merge. Leg *names and count* can shift; the *contract shape*
  (named-vector + basis + explain side-channel) cannot.
