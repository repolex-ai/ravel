//! Ravel — transcript-graph engine (Rust core).
//!
//! Day-39 spike scope: prove the loop end-to-end on real data —
//!   transcript JSONL → project to RDF 1.2 → load into oxigraph →
//!   one SPARQL CONSTRUCT detector derives an annotation as a triple term →
//!   round-trip it back out.
//!
//! Engine/adapter line (locked w/ w4r3z): the ENGINE knows `Event`/`Turn` and a
//! generic `partition` string. It does NOT know what a "soul" is. The
//! claude-code ADAPTER (`adapter` mod) parses the dialect and supplies the
//! partition (`soul::TURN_PARTITION`, a resolvable https prefix carrying no
//! soul identity — see `soul.rs`). The engine is invariant under it.

pub mod adapter;
pub mod agy;
pub mod annotate;
pub mod client;
pub mod daemon;
pub mod graph;
pub mod project;
pub mod reader;
pub mod soul;
pub mod sync;

/// A generic transcript event — the engine's unit. Dialect-agnostic.
#[derive(Debug, Clone)]
pub struct Event {
    /// Stable id (the idempotency key — w4r3z: key on this, not file mtime).
    pub event_id: String,
    /// Parent event id (the reply-to spine). None at a root / compaction break.
    pub parent_id: Option<String>,
    /// "user" | "assistant" | other dialect role; the engine treats it opaquely.
    pub role: String,
    /// RFC-3339 timestamp (federation anchor lexical form — matches Pool).
    pub timestamp: Option<String>,
    /// The text content of this event, if any (concatenated text blocks).
    pub text: Option<String>,
    /// Provenance map over `text`: which byte range came from which kind of
    /// content block. Lossless-emit discipline — a reader that matches a span
    /// looks up its `SourceKind` here, so downstream can tell an AUTHORED
    /// emission from one merely QUOTED inside a tool result. Empty when unknown.
    pub text_provenance: Vec<TextSpan>,
    /// Model reasoning scratchpad, kept OUT of `text` so chat prose stays clean.
    ///
    /// Cross-dialect, measured not assumed: Claude Code files it as a
    /// `thinking` content block (750 of them across spaceGOAT's own 18
    /// mirrored sessions, 2026-08-26) and Gemini/Antigravity as a `thinking`
    /// sibling of `content` on `PLANNER_RESPONSE` (55 in one live
    /// conversation). Both adapters extract it.
    ///
    /// DEFERRED — nothing projects this yet. Whether it becomes a
    /// `ravel:thinking` property on Turn or a third `textOrigin` value is a
    /// modeling ruling that serves both dialects, standing before tr1p
    /// (see the extractor spec §2, 2026-08-26). It is carried here so the
    /// adapters are already lossless and the ruling costs one line in
    /// `project.rs` when it lands — absence as roadmap, not as ignorance.
    pub thinking: Option<String>,
}

/// Where a slice of an event's concatenated `text` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// First-class authored prose: the human's typed message, the assistant's
    /// text blocks. A signal found here is a LIVE emission.
    Authored,
    /// Text pasted back from a tool call (tool_result). A signal found here was
    /// QUOTED, not emitted — e.g. an emojikey inside a pasted DB dump.
    ToolResult,
}

impl SourceKind {
    /// Stable lowercase tag for RDF projection (`ravel:textOrigin "authored"`).
    pub fn tag(self) -> &'static str {
        match self {
            SourceKind::Authored => "authored",
            SourceKind::ToolResult => "tool_result",
        }
    }
}

/// A `[start, end)` byte range within an event's `text`, tagged with its origin.
#[derive(Debug, Clone, Copy)]
pub struct TextSpan {
    pub start: usize,
    pub end: usize,
    pub kind: SourceKind,
}

impl Event {
    /// The `SourceKind` covering a byte offset in `text`, if provenance is known.
    /// A match's `start` is used to classify it. Returns None when provenance is
    /// empty (unknown) or the offset falls outside every recorded span.
    pub fn source_kind_at(&self, offset: usize) -> Option<SourceKind> {
        self.text_provenance
            .iter()
            .find(|s| offset >= s.start && offset < s.end)
            .map(|s| s.kind)
    }
}

/// The engine namespace for the ravel ontology — VOCABULARY only (classes,
/// properties). Instance data never lives under this prefix.
pub const RAVEL_NS: &str = "https://repolex.ai/ontology/ravel/";

/// The base for ravel INSTANCE IRIs (turns, named graphs) — the product path,
/// mirroring git-lex's `…/git-lex/SpoEvent/<id>` / `…/git-lex/NamedGraph/<name>`
/// shape. Vocabulary and data must not share a prefix.
pub const RAVEL_BASE: &str = "https://repolex.ai/ravel/";
