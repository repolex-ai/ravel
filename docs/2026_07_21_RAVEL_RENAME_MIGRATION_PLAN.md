# weave → ravel: rename migration plan

**Status:** DUCKS-IN-A-ROW (2026-07-21). Rob greenlit moving the rename forward
("ravel is growing on me"). This doc is the coordinated-migration plan the Day-42
tabling asked for: full surface inventory, availability pass (per the process note
in `2026_07_03_POSSIBLE_NAMES.md`), and the sequencing that avoids renaming twice.

**The name:** *ravel* — a contronym meaning both tangle AND untangle ("unravel a
conversation" = the read direction). Short, characterful, from the loom-process
vein of the naming field. Rob's late favorite in the possible-names doc, now the pick.

---

## 1. Availability pass (run 2026-07-21 — the process note is satisfied)

| Namespace | `ravel` | Verdict |
|---|---|---|
| GitHub `repolex-ai/ravel` | **FREE** (404) | ✅ our actual home |
| crates.io `ravel` | **TAKEN** — experimental Rust UI crate, updated 2024-06 | ⚠️ see below |
| crates.io `ravel-lex` | FREE | ✅ publish name if ever needed |
| npm `ravel` | taken (JS framework, dormant since 2022) | – irrelevant to us |
| PyPI `ravel` | taken ("Python's first meta-framework") | – lab tier can prefix |
| Prior art | Maurice Ravel (composer); Ravel Law (legal analytics, acquired by **LexisNexis** — amusing next to "git-lex"); Ravelry (fiber-arts community — actually on-brand for the textile vein) | no trademark conflict in our domain |

**The one collision that lives in our ecosystem:** crates.io `ravel` (Rust UI crate).
**Why it does not block:** weave/ravel is not published to crates.io and installs from
git (the stack's pattern). A local crate named `ravel` conflicts with nothing unless we
publish; if we ever publish, `ravel-lex` is free and matches the git-lex family. This is
strictly less crowded than "weave" was (Weaveworks, W&B Weave, Weave social — active,
famous, same-space projects).

## 2. Rename surface inventory (swept 2026-07-21, all verified by grep — not memory)

### In the weave repo (the bulk — one commit)
- **Cargo.toml:** package `weave`, lib `weave`, bins `weave-spike` / `weave-emojikey` /
  `weave-ingest` / `weave-stats` → `ravel`, `ravel-*`.
- **`WEAVE_NS`** (`src/lib.rs:83`): `https://repolex.ai/ontology/weave#` →
  `https://repolex.ai/ontology/ravel#`. This also retires the Day-40 NS-drift flag
  (Rust `ontology/weave#` vs Python lab `weave.repolex.ai/ns#`) — the lab tier adopts
  the same `ontology/ravel#` NS when it next moves.
- **`weave` strings across src/** (~112 hits: soul.rs 14, graph.rs 18, annotate.rs 31,
  project.rs 9, lib.rs 4, bins 36) — mostly doc-comments, log lines, test fixtures.
  Mechanical, single sweep.
- **Ontology draft** `docs/2026_06_29_CLAUDE_CODE_TRANSCRIPT_ONTOLOGY_DRAFT.ttl` +
  `docs/2026_07_03_TRANSCRIPT_SHAPE.{md,html}` reference the weave NS. The .ttl gets
  the NS swap; dated prose docs stay as historical record.
- **GitHub repo:** `repolex-ai/weave` → `repolex-ai/ravel` (GitHub auto-redirects old
  URLs/remotes). Local checkout dir `~/repos/repolex-ai/weave` → `ravel`; remotes
  re-pointed.

### The federation anchor segment (the coordinated part)
- `urn:soul:<sha>:Weave/Turn/<event_id>` → `urn:soul:<sha>:Ravel/Turn/<event_id>`.
- **ONE live site constructs this prefix** (`src/soul.rs:65`, now
  `ravel_partition_prefix()`) — verified Day 44; every other hit is
  doc-comment/test-fixture/read-side FILTER (`stats.rs`).
- **OPEN QUESTION FOR ROB (raised by trip.lex 2026-07-21, pre-federation window):**
  the `urn:` scheme itself. git-lex was swept this month from opaque identifiers to
  resolvable https IRIs; the Day-38 anchor contract predates that ruling. If Rob
  re-rules the anchor as an https IRI, that's the SAME one-line change + re-ingest —
  but it should be decided BEFORE the store cut (step 3) so the store is re-derived
  exactly once. If `urn:soul:` stands as deliberate design, nothing changes here.
- **No stored-data migration needed:** JSONL is canonical, RDF is a projection.
  The `.weave/oxigraph` store (~10.7k turns) is wiped and re-ingested with the new
  prefix. Idempotency is turn-keyed, so a clean re-ingest is the designed path.
- **Pool side:** ZERO code coupling — one doc-comment (`pool/src/layout.rs:6`).
  The Day-38 contract federates at query time; Pool never stores our prefix.
- **trip.lex federation vocab: NOT YET AUTHORED** (verified — no `urn:soul`/`Weave/Turn`
  in TR1P.L3X ttl/md). **This is the window.** Rename lands first → vocab is authored with
  `Ravel/Turn/` on day one → nobody renames twice. This composes with the Day-44
  graph-track sequence (enforce → derive → swap → federate): the segment rename rides
  the same one-line swap weave already owes (hand-mirror → derived anchor).

### Per-soul dirs + neighbors
- **`.weave/` → `.ravel/`** in each soul repo (spaceGOAT: 27M store — re-ingest, don't
  copy) + `.gitignore` line (spaceGOAT `.gitignore:14-18`).
- **souls.toml:** clean (zero weave refs). **Kit repo:** clean. **Pool:** one comment.
- **subtexture/docs/:** 10 dated docs mention weave — historical record, do NOT sweep.
  Future docs (and nug3's structure work → `subtexture/docs/ravel/`) use ravel.
- **Blog:** the build-in-public post shipped under "weave" — stands as history; the
  rename is a natural follow-up post, not an edit.

## 3. Sequencing (so we rename exactly once)

1. **Heads-up to trip.lex** (sent 2026-07-21): federation vocab, when it is authored, uses
   `Ravel/Turn/` + `ontology/ravel#`. He's on HOLD for the git2 cluster anyway — zero
   rework, pure pre-emption.
2. **The repo cut (one session, mostly one commit):** Cargo.toml + NS constant +
   src sweep + .ttl NS + tests green → rename GitHub repo → move local dir → re-point
   remote.
3. **The store cut (per soul, cheap):** stop ingest → `.weave/` retired → re-ingest
   JSONL into `.ravel/` with `Ravel/Turn/` prefix → `.gitignore` updated → verify
   turn-count parity with old store (the frozen-file idempotency check, reused).
4. **The federation swap stays sequenced as agreed Day 44:** enforce (w4r3z SHACL
   link-guard) → derive (trip.lex vocab, now Ravel-native) → ravel swaps hand-mirror
   for derived anchor (still one line) → distributor federates.
5. **Coordination notes:** w4r3z's one-graph/temporal-model build and Pan/Syrinx may
   reference weave by name in fresh docs — nug3 pulse requested; anything in flight
   gets the new name at write time, not a retro-sweep.

## 4. What this plan deliberately does NOT do
- No sweep of dated/historical docs (they are records, not live surface).
- No stored-IRI migration (re-ingest is the designed path; the store is a projection).
- No crates.io publish-name decision forced (repo-local `ravel` now; `ravel-lex`
  reserved-by-freeness if publishing ever matters).
