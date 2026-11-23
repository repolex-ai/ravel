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
//! partition (`names::Names::turn_partition`, a resolvable https prefix carrying no
//! soul identity — see `soul.rs`). The engine is invariant under it.

pub mod adapter;
pub mod agy;
pub mod annotate;
pub mod claude_ai;
pub mod client;
pub mod daemon;
pub mod graph;
pub mod herdr;
pub mod memory;
pub mod memory_extract;
pub mod memory_log;
pub mod memory_relive;
pub mod names;
pub mod outside;
pub mod peers;
pub mod project;
pub mod readdress;
pub mod reader;
pub mod soul;
pub mod sync;
pub mod writer;

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
    /// The model that answered this turn, as the record itself reports it.
    /// Claude Code writes the API's own `message.model` on every reply, so this
    /// is the model that ran, not the one that was chosen. None when the record
    /// does not say: a harness-written `<synthetic>` reply, a human turn, the
    /// Claude desktop export (which names no model anywhere).
    pub model: Option<String>,
    /// The model setting in force for this turn, as the harness last announced
    /// it in this conversation. Antigravity writes no model on its replies; it
    /// announces each change of its `Model Selection` setting inside the next
    /// user input, and almost always on a conversation's first input. A change
    /// made outside the conversation is not seen, so this is the setting, never
    /// a report of what ran.
    pub model_setting: Option<String>,
    /// Who wrote this event, as far as the record itself shows.
    pub wrote: Wrote,
    /// The tool calls this event made: what the speaker DID, as distinct from
    /// what it said. Kept so a memory reader can see a check the soul ran,
    /// not only its words about it (#27: Selkie's reruns were invisible, so
    /// the writer recorded them as never run). Not projected to the graph.
    pub actions: Vec<Action>,
    /// The results this event carries back for earlier tool calls, by call id.
    pub outcomes: Vec<Outcome>,
}

/// Who wrote an event. Each adapter decides from the record alone; the
/// projector turns it into a name (`ravel:author`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrote {
    /// The soul this transcript belongs to: a model's reply.
    Soul,
    /// The person at the keyboard: their own typed words.
    Person,
    /// No one the record names: tool output, harness notices, a subagent's
    /// prompt, a reply the harness made without a model.
    #[default]
    Nobody,
}

/// One tool call, as a reader needs it: what kind of thing was done, and to
/// what. `id` pairs it with its [`Outcome`].
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub id: String,
    pub kind: ActionKind,
    /// The command, path, pattern or request: one line, clipped.
    pub target: String,
}

/// What a tool call did, across harnesses' differing tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Ran,
    Read,
    Edited,
    Searched,
    Listed,
    Subagent,
    Fetched,
    /// Anything else, under the tool's own name.
    Other,
}

/// A tool call's result: its first line, how many lines followed, and
/// whether the harness reported it failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub id: String,
    pub first: String,
    pub lines: usize,
    pub failed: bool,
}

/// One line, at most `n` characters, for a reader: newlines become spaces.
pub fn one_line(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        format!("{}…", flat.chars().take(n).collect::<String>())
    }
}

/// An [`Outcome`] from a tool's output text.
pub fn outcome_of(id: &str, output: &str, failed: bool) -> Outcome {
    let mut it = output.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = it.next().map(|l| one_line(l, 100)).unwrap_or_default();
    Outcome {
        id: id.to_string(),
        first,
        lines: it.count(),
        failed,
    }
}

/// What kind of thing a tool call did, from its name in either harness
/// (Claude Code: `Bash`, `Read`, `Edit`…; Antigravity: `run_command`,
/// `view_file`, `replace_file_content`…).
pub fn action_kind(tool: &str) -> ActionKind {
    let t = tool.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
    if has(&["bash", "command", "shell", "exec"]) {
        ActionKind::Ran
    } else if t == "agent" || t == "task" || t.contains("subagent") {
        ActionKind::Subagent
    } else if has(&["edit", "write", "replace", "notebook"]) {
        ActionKind::Edited
    } else if has(&["read", "view"]) {
        ActionKind::Read
    } else if has(&["grep", "search"]) {
        ActionKind::Searched
    } else if has(&["glob", "list"]) {
        ActionKind::Listed
    } else if has(&["fetch", "url"]) {
        ActionKind::Fetched
    } else {
        ActionKind::Other
    }
}

/// How much of a shell command a reader is shown. At 100 a check's own
/// arguments were cut off (the word list a soul grepped for, the files it ran
/// over), and the memory writer could not tell the soul's check from a claim
/// that one was run (#27, lUX's 2026-11-13 note).
pub const COMMAND_CHARS: usize = 400;

/// The argument that says what a tool call did, from its JSON input: the
/// command, the path, the pattern, the request. Harness-neutral by key name.
pub fn action_target(input: &serde_json::Value) -> String {
    // A shell command: the harness's own one-line description of it (Claude
    // Code's `description`) first, then the command itself, without the
    // `cd <dir> &&` that opens most of them and would fill the line.
    let cmd = ["command", "CommandLine"]
        .iter()
        .find_map(|k| input.get(k).and_then(serde_json::Value::as_str));
    if let Some(c) = cmd {
        let c = strip_cd(c);
        return match input.get("description").and_then(serde_json::Value::as_str) {
            Some(d) if !d.trim().is_empty() => {
                format!("{} ({})", one_line(d, 80), one_line(c, COMMAND_CHARS))
            }
            _ => one_line(c, COMMAND_CHARS),
        };
    }
    const KEYS: &[&str] = &[
        "command",
        "CommandLine",
        "file_path",
        "AbsolutePath",
        "TargetFile",
        "path",
        "DirectoryPath",
        "pattern",
        "Query",
        "query",
        "url",
        "Url",
        "description",
        "prompt",
        "skill",
    ];
    for k in KEYS {
        if let Some(v) = input.get(k).and_then(serde_json::Value::as_str) {
            if !v.trim().is_empty() {
                return one_line(v, 160);
            }
        }
    }
    input
        .as_object()
        .and_then(|o| {
            o.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "toolAction" | "toolSummary"))
                .find_map(|(_, v)| v.as_str().filter(|s| !s.trim().is_empty()))
        })
        .map(|s| one_line(s, 160))
        .unwrap_or_default()
}

/// A command without its leading `cd <dir> &&` or `cd <dir>;` steps.
fn strip_cd(c: &str) -> &str {
    let mut c = c.trim_start();
    while let Some(rest) = c.strip_prefix("cd ") {
        match rest.find(['&', ';', '\n']) {
            Some(i) => c = rest[i..].trim_start_matches(['&', ';', '\n', ' ']),
            None => break,
        }
    }
    c
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

// Instance addresses (turns, named graphs, memories) are per soul and come
// from [`names::Names`]. Vocabulary and data never share a prefix.

#[cfg(test)]
mod action_tests {
    use super::*;

    /// A check's arguments are the evidence that it was a check: a command
    /// under the limit is shown whole, with or without a description.
    #[test]
    fn a_command_keeps_its_arguments_up_to_the_limit() {
        let words = (0..40)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join("|");
        let cmd = format!("cd /x/films && rg -c '{words}' */test.json");
        assert!(cmd.len() > 250 && cmd.len() < COMMAND_CHARS);
        let described = action_target(
            &serde_json::json!({ "command": cmd, "description": "Count banned words" }),
        );
        assert!(described.starts_with("Count banned words (rg -c"));
        assert!(described.contains("word39") && described.ends_with("*/test.json)"));
        let bare = action_target(&serde_json::json!({ "command": cmd }));
        assert!(bare.ends_with("*/test.json"));
        let long = action_target(&serde_json::json!({ "command": "x ".repeat(600) }));
        assert_eq!(
            long.chars().count(),
            COMMAND_CHARS + 1,
            "clipped, with the mark that says so"
        );
    }
}
