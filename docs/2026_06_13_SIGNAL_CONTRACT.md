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

  // ---- per-leg CONFIDENCE: lossless-emit/visibility-policy applied to TRUST ----
  "confidence": {
    "returnStrength": 1.0,      // strong leg — rides the embedding resonance directly
    "novelty":        1.0,      // strong leg — rides embedding distance directly
    "turbulence":     0.4       // immature leg — rides the phase, which needs a real
                                //   graph (not linear-chain). faint until the graph lands.
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

### The grain subtlety (where naive builds die)

Emission is **word-grain** (the renderer needs a number per word). But the
per-word *value* is **turn-context-aware**, not a naked per-word lexical number.
We EMIT per word, we COMPUTE with turn context, and `basis.grain` records that
honestly. A widget MUST NOT imply the raw per-word twitch IS the signal.

### Per-leg confidence

The three legs are NOT equally trustworthy on day one. `returnStrength` +
`novelty` ride the embedding leg directly (strong from line one). `turbulence`
rides the phase leg over the graph — only as good as the graph; a linear-chain
graph starves it. So each leg carries a `confidence` in [0,1]: the engine
**declares trust today**; the viz **decides how to render that trust** (solid vs
faint vs gated). Same discipline as the value itself, applied to trust. The number
rises as the engine matures (turbulence → 1.0 once the real graph lands) with no
widget code change.

## Cold path: explain(seq) — the "why" side-channel

Off the hot path. When a human points at a word, viz calls back for the *why* —
which earlier words this one related to.

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
- No corpus mutation — identity/backref fields pass through unchanged; the signal
  is computed ALONGSIDE the corpus, never folded back into it.

## Open / owned

- **Engine direction (RESOLVED):** "oscillator as reference, rebuild fresh."
  `basis.method = "embed-fresh-v0"`, calibrated against `"echo-js-v0"` (ablation
  baseline) and `"null-v0"` (floor).
- **Calibration gate (LSPy):** three-way on the 357k-word arc. If the fresh
  embedding engine beats the ablation baseline, the signal was in the text.
- **Vector legs (engine + LSPy):** are three the right three? The by-hand pass may
  rename/split/merge. Leg *names and count* can shift; the *contract shape*
  (named-vector + basis + explain side-channel) cannot.
