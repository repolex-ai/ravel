# The Transcript Graph, in Full — the total shape of a session

A visual + textual reference for the **total type-space** of a Weave
`ClaudeCodeTranscript`: every kind of content one session can hold — text,
thinking, tool calls, tool results, images, attachments, compaction breaks — as
one tree. Built from `2026_06_29_CLAUDE_CODE_TRANSCRIPT_ONTOLOGY_DRAFT.ttl` +
live projected triples. Counts (`n≈`) are real, from the 125-session corpus
survey the ontology was carved against.

**Companion:** `2026_07_03_TRANSCRIPT_SHAPE.html` (the color-coded rendered view).

- **ns** `https://repolex.ai/ontology/weave#`
- **dialect** `weave:ClaudeCodeTranscript` (a sibling `weave:ClaudeDesktopExport` slots under the generic `weave:Transcript` core)
- **rdf** 1.2 (triple terms)

**Status legend:** `[wired]` = the Rust adapter emits this today · `[designed]` =
in the ontology, not yet wired (the adapter is still turns-only for the spine +
the emojikey reader).

## The mental model: four roles

Every node belongs to one of four roles. Hold these and the whole thing reads:

| Role | What it is |
|---|---|
| **Spine** | The conversation as it happened — Turns + content blocks, chained by reply-to. |
| **Structure** | Everything else the harness records — attachments, system events, compaction, titles. |
| **Reader output** | What a detector *found* — an `oa:Annotation` anchored to a span. Added, never in the raw log. |
| **Belief** | A finding as an *unasserted* RDF-1.2 triple term — "we believe X", not "X is true". |

---

## 01 — The spine: a Turn and its content blocks

A `Turn` is one message. Its `message.content[]` is an **ordered list of blocks**
(order is semantically meaningful: text → tool_use → text…). Each block is a
subtype of `ContentBlock` — this is where all the "kinds of content" live.

```
Transcript                                            [wired]  — one .jsonl session
├─ weave:sessionSlug  "820a266b-…"
│
└─ weave:hasEvent ─→ Turn  (one message · an Event subtype)   [wired]
      ├─ rdf:type          weave:Turn  (→ UserTurn | AssistantTurn)
      ├─ weave:role        "assistant"
      ├─ weave:timestamp   "2026-06-30T17:54:44Z"^^xsd:dateTime   ← TIME anchor (Pool join)
      ├─ weave:parentEvent ─→ …/Turn/<prev>                       ← reply-to spine
      ├─ weave:model       "claude-opus-4-8"  (AssistantTurn)     [designed]
      │
      └─ weave:hasContentBlock ─→ [ ordered blocks, blockOrder 0,1,2… ]   [designed]
            ├─◆ TextBlock       weave:text "…prose readers detect over…"   n≈8940
            ├─◆ ThinkingBlock   {thinking, signature}  extended-thinking    n≈75
            ├─◆ ToolUseBlock    {id, name, input, caller}  assistant call   n≈11320
            │        └─ pairs by tool_use_id ↓
            ├─◆ ToolResultBlock {tool_use_id, content, is_error}            n≈11320
            └─◆ ImageBlock      source{type, media_type, data}             n≈16
                     ↑ FEDERATION HOOK — pasted/produced images are the Pool-Moment join surface
```

**Pair:** `ToolUseBlock ⇄ ToolResultBlock` are two separate blocks linked by
`tool_use_id` — often in *different* turns (the call is the assistant's, the
result comes back on the next line). A reader that wants "what did this tool do"
walks that id, not the tree.

---

## 02 — Structure: the other Events a session records

A Turn is just *one* kind of Event. The harness writes many others as sibling
`weave:Event`s. The big one is `Attachment` — **not one envelope but a tag-union
of 20 subtypes**, carved three ways by whether the payload carries a graph edge
(the design pattern is the interesting part — see below).

```
Transcript weave:hasEvent ─→ Event  (siblings of Turn)
│
├─ Attachment   type=attachment · a 20-way tag union   n≈2289   [designed]
│   │
│   ├─ (A) EDGE-BEARING → typed subclass (a reader TRAVERSES the edge)
│   │   ├─◆ QueuedCommandAttachment  origin{kind,server} ← where a prompt came from; kind=channel → a subtext peer
│   │   ├─◆ HookContextAttachment    memory-recall payload → the Weave⋈Soul join surface
│   │   ├─◆ NestedMemoryAttachment   CLAUDE.md path → edge into the soul-repo graph
│   │   ├─◆ FileRefAttachment        {file, edited_text_file, compact_file_reference} → filesystem/Pool-blob node
│   │   ├─◆ InvokedSkillsAttachment  → edge to Skill objects (first-class in the kit)
│   │   └─◆ HookExecAttachment       {hook_success, hook_non_blocking_error} → a hook run
│   │
│   ├─ (B) VALUE-ONLY → ONE class, discriminator + payload (just READ)
│   │   └─◆ StateAttachment  attachmentKind ∈ {task_reminder, date_change, plan_mode,
│   │                          ultrathink_effort, command_permissions, skill_listing …}
│   │
│   └─ (C) REGISTRY DELTAS → ONE class, parametrized by registryName
│       └─◆ RegistryDelta  registryName ∈ {tools, mcp, agents}  + added/removedNames
│
├─ SystemEvent        subtype ∈ {turn_duration, compact_boundary, hook, api_error}   n≈1666
├─ FileHistorySnapshot editor undo state — not conversation                          n≈4035
├─ QueueOperation     prompt-queue mechanics                                         n≈2416
├─ PermissionModeEvent ∈ {normal, plan, acceptEdits, bypassPermissions}              n≈1407
├─ LastPromptEvent  n≈1330   ProgressEvent  n≈385   ModeEvent  n≈789
├─ AiTitleEvent  n≈699   PrLinkEvent  n≈94
└─ CustomTitleEvent  n≈14 · AgentNameEvent  n≈14 · SummaryEvent
```

**Compaction is a first-class `prov:Activity`**, not a mere event — and it's
*derivable*, not guessed: it `prov:used` the pre-compact tail and
`prov:generated` the continuation (linked by `logicalParentUuid`), marked by
`system.subtype=compact_boundary` (n=168 verified). `preTokens→postTokens` is a
load/biorhythm signal.

---

## 03 — Reader output & belief: the layer a detector adds

Nothing above 03 is in the raw log's *meaning*-layer — it's structure. A
**reader** walks the spine and mints an `oa:Annotation` (standard Web Annotation
vocab) plus an **unasserted** RDF-1.2 **triple-term claim**. This is the emojikey
reader, live today `[wired]`.

```
Turn (assistant, from the spine ↑)
└─◆ oa:Annotation  — one finding   [wired]
    ├─ prov:wasAttributedTo ─→ detector/emojikey   WHO found it
    ├─ prov:generatedAtTime "2026-06-30T17:54:44Z"^^xsd:dateTime
    ├─ weave:eventType      "emojikey/harvest"
    ├─ weave:sourceKind     "tool_result"   authored (live) vs quoted
    │
    ├─ oa:hasTarget ─→ ◆ target   WHERE in the source
    │     ├─ oa:hasSource   ─→ transcript/820a266b…  (the whole doc)
    │     ├─ oa:hasSelector ─→ ◆ TextPositionSelector
    │     │        ├─ oa:start 2228^^xsd:integer
    │     │        └─ oa:end   3277^^xsd:integer
    │     └─ weave:atEvent ─→ urn:soul:<sha>:Weave/Turn/572a…  the EXACT turn (soul anchor)
    │
    ├─ prov:used ─→ urn:soul:<sha>:Weave/Turn/572a…  the evidence it read
    │
    └─ oa:hasBody ─→ ◆ body   WHAT the signal says
          ├─ weave:sig_me       "📏🌊5∠90…"
          ├─ weave:sig_content  "🧠🎨…"
          ├─ weave:sig_you      "🤝…"
          └─ weave:sig_<k>      …numeric legs → xsd:integer/decimal  (FILTER-able)

claim/0470b23…  — the finding as a BELIEF, not a fact   [wired]
├─ rdf:reifies  <<( …Turn/572a…  weave:exhibits  "emojikey/harvest" )>>
│                the parens = UNASSERTED. "we believe this turn exhibits X", never "X is true".
├─ prov:wasDerivedFrom ─→ oa:Annotation   belief traces to its evidence
├─ weave:detector       "emojikey"
└─ weave:detectionType  "emojikey/harvest"
```

**Why belief:** detections are kept **structurally separate from facts**. A wrong
detector doesn't corrupt the graph — it just holds a belief you can filter out.
The honesty rail is in the data model itself, not a convention on top of it.

---

## The two design patterns worth stealing

1. **The tag-union carved by graph-shape, not schema-shape.** `Attachment`'s 20
   subtypes are split by ONE question — *does this payload carry an edge a reader
   will traverse?* Edge-bearing → its own subclass (so traversal is typed);
   value-only → one `StateAttachment` with a discriminator (don't mint 12 classes
   a reader treats identically); delta-shaped → one `RegistryDelta` parametrized
   by name. The carve axis is *how the data will be queried*, not *what the source
   calls it*. Twenty raw `attachment.type`s collapse to 8 classes with no loss.

2. **Detections as unasserted beliefs (RDF 1.2 triple terms).** A reader's output
   is `rdf:reifies <<( subject predicate object )>>` — a *quoted* proposition the
   graph does not assert. You can query every claim, rank them, filter by detector,
   or drop a bad reader's beliefs wholesale, all without a single wrong detection
   ever entering your fact set. Provenance (`prov:wasAttributedTo` /
   `prov:wasDerivedFrom`) makes each belief traceable to who formed it and on what
   evidence. This is the honesty rail as a data-model primitive, not a policy.
