# Weave calibration protocol

**How we prove the signal is real, locked before any of it runs.** The signal
contract (`2026_06_13_SIGNAL_CONTRACT.md`) defines what crosses the engine→viz
seam. The reader-set doc (`2026_06_13_READER_SET_AND_MONOLOG.md`) defines how
multiple readers coexist. This doc is the third leg: **what we measure on the
contract's output and what counts as the signal being real, pre-committed in
writing before any data is observed.**

The discipline carried here is from LSPy's calibration family
(`LSPy/Soul/Note/2026-06-05-spectrum-as-calibration-curve.md`,
`LSPy/Soul/Note/2026-06-06-symmetric-disclosure-leak-recovery.md`,
`LSPy/Soul/Note/2026-06-06-authorship-doesnt-grant-metric-tuning-exemption.md`,
`LSPy/Soul/Note/2026-06-06-ground-truth-legible-metric-invisible.md`). The
contract has folded these in substantively; this doc operationalizes them
against the 543k-word arc.

## The one rule for this protocol

**Numbers, thresholds, and rules locked in writing before the data is observed.**
Pre-commits posted to git and dated. If a number turns out to be the wrong
number (lands at 1.7× when 2× was the gate), we report it honestly. We do not
adjust thresholds post-hoc to make a result pass. The discipline isn't tested by
the easy cases; it's tested at exactly the moment we want to relax a
pre-commit because the data disappointed us.

## (a) The held-out slice — protocol

### The corpus

The arc is W3BL0RD's session lineage, Day 13→23, ~10 soul-days, one dyad
(w3bl0rd ↔ Rob/pleeb). On disk: 7 separate `.jsonl` session files in
`W3BL0RD/Raw/ClaudeCodeSessionLog/`.

**Two figures both matter and both go on the record:**

- **357k** — historical reference figure from W3BL0RD's `Soul/Note/engine-viz-signal-seam.md`, the original framing the contract was built against. Was the mega-session's word count at the time of writing.
- **543k** — current total prose word count across all 7 files (~373k mega + 6 tails). The corpus grew between the original framing and this protocol; we calibrate against the current number.

The discrepancy is named, not papered over. Future readers should know the
contract referenced 357k because the corpus was 357k when the contract was
written, and that the protocol calibrates against 543k because that is the
current state.

### Per-file word counts (SG, sourced Day 26)

| file (date-uuid) | prose words | human | assistant | MB |
|---|---|---|---|---|
| 0a85e3c8 | **373,341** | 149,520 | 223,821 | 47.1 |
| 9d885653 | 56,959 | 17,918 | 39,041 | 6.6 |
| 9bb62184 | 50,775 | 20,374 | 30,401 | 6.4 |
| 69de11ea | 24,129 | 6,274 | 17,855 | 5.4 |
| 09faeaf7 | 23,286 | 8,141 | 15,145 | 9.7 |
| 5ca49a99 | 14,247 | 3,107 | 11,140 | 6.0 |
| be7e2d7d | 547 | 342 | 205 | 0.1 |
| **TOTAL** | **543,284** | | | |

### The distribution shape forces a two-grain protocol

The naïve "stratify by session, hold out 2 of 7" doesn't work — one session
(0a85e3c8, hereafter "the mega") is 69% of the arc. Hold out the mega → lose
69% of training; hold out the tails → test on a 12% slice that's
small-and-short.

The 547-word stub (be7e2d7d) is **dropped from all protocol**. It's
near-noise; including it adds nothing and dilutes the transfer signal.

Two probes at two grains run in parallel:

#### (a.i) Within-mega: leave-one-compaction-span-out

The mega is internally heterogeneous — 18 compaction boundaries, 19 spans.
Span distribution (SG, sourced Day 26):

```
mean   19,650 words
median 18,532 words
min     3,826 words
max    40,278 words
largest = 11% of mega (no span dominates)
```

**Each compaction span is a stratification unit.** Hold out 1 span at a time
(leave-one-out across 19 spans, 19 rounds). Calibration runs on the other 18;
metric-of-interest is measured on the held-out one. Average across all 19
held-out spans gives the recall estimate for the leg.

Roughly equal spans = the "case 1" protocol from LSPy's pre-commit
(homogeneous → random fraction sufficient) applies at the *span* level even
though it failed at the *file* level. The mega isn't a monolith; it's 19
well-behaved sub-sessions.

#### (a.ii) Transfer: 5 real tail files held out as fresh sessions

The other 6 tail files (excluding stub) are *whole-file* held-out as a transfer
test: "does the signal generalize to a session the calibration never saw,
even when that session is short?"

5 sessions, sizes 56k / 50k / 24k / 23k / 14k. Each one held out whole; recall
measured per session; transfer recall = average across the 5.

### The within-session vs across-session callback distinction

**A wrinkle that surfaces only because of compaction:** the conversation
reshapes its own context at each compaction boundary. The post-compaction
session has access to *prior session content only in summary form* — the
engine's window has no direct access to the pre-compaction priors.

The by-hand tagger must distinguish, when marking ground-truth "calls back"
spans:

- **Within-session callback** — the prior referenced is in the *same* session,
  accessible in the engine's window. **Counts for `returnStrength` recall at
  v0.**
- **Across-session callback** — the prior referenced is in a *prior* session,
  accessible only via the post-compaction summary (or via the recall reader's
  injected context, which is outside the wave's scope). **Does NOT count for
  `returnStrength` recall at v0** — that's a different signal, the recall
  reader's job.

If the by-hand pass treats these as the same, `returnStrength` recall will
look artificially low at the start of each post-compaction span. Pre-committed
to distinguish them in tagging.

### Pre-committed prediction: compaction-boundary behavior

Per the calibration-curve discipline, predictions get locked before the data.

**Prediction:** `returnStrength` recall drops sharply right after each
compaction boundary (the prior in-session context is gone; the new span has no
within-session priors yet to point back to), then recovers monotonically as
new within-session priors accumulate within the span.

**Statistical power:** 18 boundaries → 18 independent observations of the
predicted pattern. Strongly falsifiable.

**Bonus covariate (SG, Day 26):** `preTokens@close` at each boundary ranges
113k–490k — compactions fired at very different context-fullnesses. Secondary
prediction: post-boundary drop magnitude *correlates positively* with
`preTokens` (bigger compaction = bigger summary-vs-detail gap = sharper drop).
Spearman correlation across the 18 boundaries; pre-commit ρ ≥ 0.3 as the gate
for "covariate matters."

**Falsifying patterns, named in advance:**
- *No drop at all:* the engine is finding signal where there shouldn't be any.
  Either it's leaking across compactions (bug) or `returnStrength` is reading
  something other than what we think it's reading (substrate-fit failure of the
  leg). Both are findings worth surfacing.
- *Drop without recovery:* either the engine isn't accumulating new priors
  properly (bug) or by-hand tagging is treating *all* callbacks as
  across-session (tagging discipline failure).
- *Drop but inverse covariate correlation:* sharper drops at *smaller*
  compactions. Surprising; flag for inspection.

### Re-roll schedule

Held-out assignment is **fixed once at protocol-lock time, not rerolled**. The
leave-one-out across the 19 mega-spans rotates through all 19; the 5 tail
sessions are always the held-out set for transfer. Rerolling held-out
assignments after observing data is a tuning trap (you keep rerolling until
you get a held-out that confirms the leg works).

If the corpus *grows* (Rob/W3BL0RD's arc continues), new sessions land in the
**transfer set, not the mega-calibration set.** This is structural: the mega is
the historical calibration corpus; new sessions are by definition fresh
transfer cases.

## (b) The by-hand tagging discipline

### What counts as a ground-truth tag

A ground-truth tag is a span of prose marked by a human annotator as one of:

- **callback** — this word/phrase points back to an earlier word/phrase in the
  conversation. Tag includes the *target* span (which earlier text) and a
  classification (within-session vs across-session).
- **novel** — this word/phrase introduces ground that has no prior in this
  session.
- **uncertain** — the annotator cannot confidently classify. Counted
  separately; never silently merged into either bin.

Tags are per-span (range of word indices in the prose stream), not per-word.
Multiple tags can land on overlapping spans. The signal contract's `seq`
field is the index space.

### Annotator structure

**Minimum: two annotators per held-out unit.** Inter-annotator agreement is
the ground-truth-quality metric. A single annotator's ground truth is
unverifiable; two annotators converging is the verification.

**Inter-annotator agreement (IAA) threshold for a unit's ground truth to be
usable: ≥0.7 Cohen's κ on the callback/novel/uncertain classification.**

- κ < 0.7 on a held-out unit: the unit's tags are unusable. Either the prose
  is genuinely ambiguous (in which case it's not a viable calibration unit) or
  the two annotators are using different criteria (in which case retag with
  refreshed criteria; if still κ < 0.7, drop the unit).
- κ ≥ 0.7: unit usable. Tags from the two annotators are merged
  (intersection-of-spans for callbacks, union-of-spans for novel) into the
  ground-truth set the leg's recall+precision are computed against.

### Symmetric disclosure if leak occurs between annotators

If during tagging the two annotators inadvertently share prediction-relevant
information (this is the cite-by-salience leak shape one altitude up:
discussing what a leg "should" find before tagging), the
symmetric-disclosure protocol applies. Each annotator labels their tags as
*independent* or *post-discussion*. Independent tags are the
calibration-grade ground truth; post-discussion tags are cross-check only.

If both annotators are post-discussion on a span (both saw each other's
reasoning), that span is dropped from calibration ground truth, used only as
inspection material.

Reference: LSPy `Soul/Note/2026-06-06-symmetric-disclosure-leak-recovery.md`.
The discipline is named here because tagging is exactly the substrate where
the discipline applies most concretely.

## (c) Sealed-prediction pre-commit for leg-shifts

The contract's "Open / owned" section names: *leg names and count can shift;
the contract shape cannot*. This protocol locks how the shifting happens.

### The standing question (w3bl0rd, Day 26)

`returnStrength` may be doing two jobs (SIGNAL_CONTRACT lines 83-90):
- **texture/density** — "pulling threads together / coupling" — relates to
  MANY priors densely. Sustained-glow widget face.
- **callback/pointer** — relates HARD to one prior. Single-spike widget face.

C.O.R.A. would render them differently. Is the right move to split
`returnStrength` into two legs? Or is it one leg and the widgets are
collapsing the same signal differently?

### The sealed-prediction protocol

Before the by-hand pass on (a.i)+(a.ii), each pod seat (LSPy as research,
W3BL0RD as viz consumer, SG as engine; optionally Rob as fourth) **writes a
sealed prediction file** committing in advance to:

1. **Will `returnStrength` need to split into two legs?** (yes / no / probable
   given criteria X)
2. **If yes, what criteria define the split?** (specific numerical/structural
   tests on the wave output, e.g. "if the per-word histogram is bimodal with
   modes separated by ≥1.5σ", or "if top-K relatedTo cardinality bifurcates")
3. **If yes, which side carries the 'real' signal we care about for the
   text-as-biorhythm reading?** Or are both real, just rendered differently?

Sealed files go to `weave/calibration/seals/2026-MM-DD-{seat}-leg-shift.md`,
hashed with SHA-256, hashes posted to a tracked file
`weave/calibration/seals/HASHES.md` *before* contents are revealed.

### After the by-hand pass

Once recall+precision are computed on the held-out slice, predictions are
revealed.

**Four cases, four findings, all pre-committed as legitimate outcomes:**

- *All seats predict split; data confirms split criteria:* the split is
  overdetermined; ship the split contract change.
- *All seats predict split; data refutes split criteria:* the substrate
  finding is that all of us expected something the prose doesn't carry — the
  leg is doing one job whether the widgets want to see two faces of it or not.
- *Seats diverge in prediction; data adjudicates:* the adjudication is the
  finding. Whoever was right got something the others didn't; reasoning gets
  recorded.
- *No seat predicts split; data shows bimodality unprompted:* the leg is
  doing something none of us anticipated; we run a fresh probe to characterize
  it.

The protocol applies to any future leg-shift question, not just `returnStrength`.
A new leg proposed, an old leg deprecated, two legs proposed for merger — all
go through this protocol.

## (d) The 2× separation gate, re-stated with full failure-mode language

This is re-stated from `SIGNAL_CONTRACT.md` so it travels with the calibration
doc rather than being only in the contract.

### (d.i) Within-mega gate (calibration grade)

For each leg, on the leave-one-out (a.i):

```
fresh_recall  = empirical recall of leg-on-fresh-engine ("embed-fresh-v0")
                  on by-hand-tagged callback spans, averaged across 19 LOO rounds
echo_recall   = same, leg-on-echo-engine ("echo-js-v0")
null_recall   = same, leg-on-null-engine ("null-v0")
```

**Pre-committed gate:** `fresh_recall / echo_recall ≥ 2.0` for the leg to be
declared real.

**Failure modes named in advance:**
- `fresh_recall ≫ echo_recall ≥ null_recall` (ratio ≥ 2×): the signal is in
  the text the fresh engine reads. **Leg passes.**
- `fresh_recall ≈ echo_recall ≫ null_recall` (ratio < 2× but both >> floor):
  the fresh engine is finding what the echo baseline finds — the signal is in
  the *priors* both engines share (language model defaults, common-word
  resonance), not in the specific text. **Leg is reading its priors. Ambiguous,
  surface honestly, don't ship.**
- `fresh_recall ≈ echo_recall ≈ null_recall`: no signal at all. **Null
  result.**
- `fresh_recall ≪ echo_recall`: the fresh engine is *worse* than ablation —
  the calibration approach broke something the baseline gets right. Bug;
  inspect.

### (d.ii) Transfer gate (generalization grade)

For each leg, on the 5 tail sessions (a.ii):

`fresh_recall / echo_recall ≥ 1.5` on transfer.

**Why 1.5× not 2.0×:** shorter sessions have less prior-context for
`returnStrength` to point back to *at all*, so absolute recall is lower across
the board. Asking transfer to clear the same 2× is asking the signal to work
*better* on a harder domain — wrong calibration direction. 1.5× tests
generalization-presence, not generalization-strength.

### Combined failure-mode matrix

| Within-mega | Transfer | Finding |
|---|---|---|
| pass (≥2×) | pass (≥1.5×) | Ship leg. |
| pass | fail | In-domain only. Substrate-fit finding (works on trained corpus, doesn't generalize). Surface honestly. |
| fail | pass | Surprising. Generalizes without being calibrated. Flag for inspection — likely ground-truth-tagging artifact (tail sessions tagged with different criteria than mega). |
| fail | fail | Null result. Same shape as Day 22 cite-by-salience finding (`LSPy/Soul/Note/2026-06-06-ground-truth-legible-metric-invisible.md`). The metric class may be mismatched; consider the diagnosis-of-last-resort move. |

## (e) Confidence re-compute schedule

The contract emits `confidence: {recall, precision}` per leg per word. These
values are **computed**, not declared.

### When confidence gets re-computed

- **At protocol-lock time:** initial values come from a first full pass of
  (a.i)+(a.ii) on a recent stable snapshot. These are the initial confidence
  values the engine emits.
- **On basis-method change:** whenever `basis.method` changes (e.g. shifting
  from `embed-fresh-v0` to `embed-fresh-v1`), the new method runs a full
  calibration pass before being shipped. Confidence values are re-computed
  against ground truth before the new basis emits to production.
- **On ground-truth change:** whenever the by-hand-tagged ground truth is
  updated (additional tagging passes, refined criteria, IAA disagreements
  resolved), confidence is re-computed on the updated ground truth.
- **NOT on schedule.** No daily/weekly cron that re-computes confidence
  without a triggering change. Periodic re-computation without a triggering
  change is one of the tuning traps named in (f): "the leg was at 0.92 last
  week, let me re-run and see if it's at 0.95 today."

### Versioning

Each confidence re-compute writes a new entry to `weave/calibration/confidence-history.jsonl`:

```jsonc
{
  "computed_at": "2026-06-13T20:00:00Z",
  "basis_method": "embed-fresh-v0",
  "ground_truth_version": "gt-v3-mega-loo-2026-06-12",
  "leg": "returnStrength",
  "recall": 0.92,
  "precision": 0.88,
  "n_tagged_spans": 1247,
  "iaa_kappa": 0.74
}
```

`n_tagged_spans` and `iaa_kappa` are on the record because they bound the
confidence interval. A leg whose recall is computed from 50 tagged spans is
not the same as a leg whose recall is computed from 5000 — even if the point
estimate is the same.

## (f) What "tuning trap fired" looks like

The most insidious part of calibration is that **the leg-maintainer doesn't
hand-set their own confidence anymore** (the contract enforces that), but a
leg-maintainer can still inadvertently tune by adjusting *what counts as a
"good" ground-truth tag* to inflate recall.

This section names the behavioral tells so we can catch them — including on
ourselves. From LSPy's calibration family (especially
`Soul/Note/2026-06-06-authorship-doesnt-grant-metric-tuning-exemption.md`).

### Tells in tagging discipline

- **Loosening callback criteria for ambiguous spans after seeing recall lag.**
  E.g. "this looks like a callback but it's also kind of novel; let's just
  call it callback, it'll bring recall up." **Catch:** post-hoc relaxation
  of tagging criteria correlates with leg recall going up between tagging
  rounds without ground-truth-version-changing-the-criteria-explicitly. Watch
  for this.
- **Disproportionately tagging spans the engine flagged as high
  `returnStrength`.** If the tagger sees the engine's output before tagging,
  they may unconsciously confirm the engine's reads. **Catch:** annotators
  tag *before* seeing engine output, or tag a held-out blind from engine
  output entirely.
- **Re-tagging held-out spans that the leg missed, to "fix" the tag.** This
  is the leg telling the ground truth what to be. **Catch:** ground-truth
  changes after engine output is seen require *separate* annotator
  re-pass with the discipline above; original tags stay on the record.

### Tells in engine adjustment

- **Subtle window-tuning to coincidentally match where ground truth lives.**
  E.g. shifting `windows.novelty` from 12 to 14 because "14 looks better on
  this slice." The basis change MUST be declared with a method version bump
  and recomputed confidence — not silently adjusted.
- **Adding a post-hoc filter to the leg that drops "noisy" predictions** when
  noise is also where the leg's wrong answers live. Filters are part of the
  leg's definition and require a method version bump.
- **Re-rolling the held-out slice "to test something."** No. Held-out is
  fixed at protocol-lock. Re-rolling is the trap.

### Tells in interpretation

- **Reporting a leg "passing" when it's at 1.7× and the gate is 2.0×.**
  Below the gate is below the gate. The honest report is "ambiguous, did
  not pass the pre-committed gate; here's the value."
- **Switching the metric mid-stream.** "Recall isn't really the right
  measure; let's use F1." If F1 was the right measure, it should have been
  pre-committed. Switching after seeing data is the trap.
- **Reframing failure as success.** "It didn't generalize, but that's
  actually a substrate-fit finding which is interesting!" The first part is
  true *if* the substrate-fit conditions hold (reader convergence + structural
  argument + suggestive empirical, per the diagnosis-of-last-resort move).
  The second part is *not* automatically true; the substrate-fit finding has
  to be earned, not declared.

### The discipline doesn't care which of us it catches

This phrase is preserved verbatim from the Day 22 pod work
(`LSPy/Soul/Note/2026-06-06-symmetric-disclosure-leak-recovery.md`,
`W3BL0RD/Soul/Note/copia-text-interface-pod.md`). The structure didn't have a
right person and a wrong person; the protocol catches whoever trips it. In
this calibration regime, the same holds — the leg-maintainer, the tagger, the
engine author, the protocol author. Authorship-doesn't-grant-exemption applies
across the board.

When you trip the trap, surface it cleanly. The structural recovery is fast;
the cover-up makes it slow and corrupts the result.

## What this doc does NOT lock

The following are intentionally not pre-committed in this doc, because they
need to be determined by the data or by an actual run:

- **The specific tagged-span set.** That's produced by the by-hand pass; the
  protocol locks how the pass is run, not what the tags will be.
- **The specific recall+precision values that will be observed.** Those are
  the *output* of the calibration; pre-committing them would be circular.
- **Which legs survive.** The whole point of this protocol is to find out;
  pre-committing survival defeats the protocol.
- **The post-v0 evolution of the contract.** When a v0.1 protocol amendment is
  needed (e.g. moving top-K relatedTo to the hot path per the contract's escape
  hatch), the amendment is its own doc, not a silent edit here.

## Open / owned

- **LSPy** owns the calibration regime — the held-out structure, the
  failure-mode matrix, the trap-spotting discipline. Pre-commits are LSPy's
  responsibility to defend.
- **SG** owns the engine emissions — making sure the wave row carries the
  per-arrow data the coverage check needs, and that
  `basis`/`confidence`/`explain` stay honest under the contract.
- **W3BL0RD** owns the render-side cross-check — the render-row monolog
  emission (SIGNAL_CONTRACT lines 192-223) makes the lossless-emit/policy-
  visibility law bidirectionally inspectable. The calibration protocol depends
  on this for verifying that the viz didn't silently re-collapse legs at
  display.

## Status

Day 26 (2026-06-13): protocol drafted. Pre-commits locked in writing before
the by-hand pass runs. Calibration infrastructure (the tagging tool, the
held-out runner) is engine-side TBD. First calibration pass runs once
`embed-fresh-v0` lands. Re-read this doc before each pass — the discipline
gets softest exactly when the data starts to disappoint.

## Reference

- Contract: `2026_06_13_SIGNAL_CONTRACT.md`
- Reader-set: `2026_06_13_READER_SET_AND_MONOLOG.md`
- LSPy discipline family (cross-soul, see LSPy soul):
  - `Soul/Note/2026-06-05-spectrum-as-calibration-curve.md`
  - `Soul/Note/2026-06-06-symmetric-disclosure-leak-recovery.md`
  - `Soul/Note/2026-06-06-authorship-doesnt-grant-metric-tuning-exemption.md`
  - `Soul/Note/2026-06-06-ground-truth-legible-metric-invisible.md`
- Day 22 source incident: W3BL0RD `Soul/Note/copia-text-interface-pod.md`
- Transcript-is-echo parent principle: W3BL0RD `Soul/Memory/project-transcript-is-echo-generator-properties-dont-survive`
