# Weave

The **semantic-wave engine** behind CoPIA's alive-text interface.

A document is not a bag of particle-tokens — it's a **meaning waveform**. Each new
word is a **diff** that deforms the running shape, and *the deformation is the
signal* (pleeb: "the things that make the AI's brain jiggle — epiplexity,
cognitive load, hallucination"). Diff-as-spine is incremental by construction:
O(N), built to survive a 357k-word transcript arc.

This repo is the **engine** (Python — for the ML tooling). The *interface* that
renders the engine's signal lives separately in `copia-studio/weave/` (w3bl0rd's
alive-text renderer). They meet at one declared seam: the **signal contract**.

## The trio

| seat | who | owns |
|---|---|---|
| **engine** | spaceGOAT | ingest, the wave math, shape-providers, signal emission |
| **viz** | w3bl0rd | the alive-text renderer; consumes the signal, decides display |
| **research / test-design** | LSPy | the calibration regime; proves the signal is real |

## The two load-bearing ideas

1. **Shape-provider seam.** The wave op — *"how much did this token deform the
   running structure"* — is the same over any shape. So the engine rides a
   pluggable shape interface with three arms, none blocking on the others:
   - **embedding-trajectory** — no external dependency, the baseline shape.
   - **AMR-graph** — public Python parser (amrlib/SPRING), the "simplify-then-wave" arm.
   - **SLG-graph** — drop-in later if/when a `text → logic-graph` transform exists.

   *Which shape makes the meaning-wave most legible* is an empirical calibration
   arm, not an assumption.

2. **Emission is lossless; visibility is policy.** The engine emits the full
   per-word named vector (+ per-leg confidence + provenance). The viz decides
   what's shown. The engine never thresholds-for-display; the viz never computes
   semantics. See `docs/`.

## Build order (pleeb, Day 25)

1. **Ingest** — full-load the transcript + append incoming messages, in Python.
   (w3bl0rd's Node ingester is read as a *guide* to the JSONL format and the hard
   parts — harness adapter, content-anchored ids, cold-load-then-tail — not ported
   assumption-for-assumption.)
2. **Wave-over-shape** — the diff math against the shape-provider interface.
3. **Shape arms** — embedding-trajectory first (ships the engine), then AMR.

The oscillator (`semantic-oscilloscope/spectral_engine.py`) is a **two-day spike,
not a spec** — a prior attempt to interrogate with suspicion, not a blessed
reference. Carefully rebuild from the math + the corpus. Do not assume the spike
was correct.

## Status

Day 26 (2026-06-13): repo created, structure + signal contract landed. Engine
code is skeleton-only. `docs/` carries the contract to argue against before
anyone builds an imagined version of it.

## Layout

```
src/weave/          engine code (skeleton)
  shape/            the shape-provider interface + arms
docs/               the signal contract + reader-set ("monolog") architecture
```
