//! Claude desktop / claude.ai dialect adapter: a data export → generic `Event`s.
//!
//! Sibling of `adapter` (Claude Code) and `agy` (Antigravity). Same contract:
//! it knows one dialect's shape, hands the engine generic Events, and the
//! engine never learns which substrate they came from.
//!
//! The source is the account export from claude.ai (Settings → Privacy →
//! Export data): a directory holding `conversations.json`, `projects.json`,
//! `memories.json` and `users.json`. Measured on the 2026-04-06 export
//! (1,463 conversations, 31,353 messages, 2023-11 → 2026-04):
//!
//! 1. **Every message has a `uuid`, unique across the export.** It is the
//!    idempotency key, exactly as a Claude Code record's `uuid` is, so a turn
//!    imported twice — from two exports months apart — lands on one IRI.
//! 2. **The export carries no parent pointer.** Messages are listed in order
//!    (zero out-of-order `created_at` pairs across the whole export), so the
//!    spine is derived from list order: each message's parent is the one before
//!    it. Edited and regenerated branches are not in the export at all.
//! 3. **`sender` is `human` or `assistant`.** `human` is projected as `user`,
//!    the value Claude Code uses for the same speaker, so a query for "what did
//!    the human say" does not need to know which client it was typed into.
//!
//! Text comes from the content blocks: `text` and `voice_note` are authored
//! prose; `tool_result` and pasted attachments (`extracted_content`) are quoted
//! material; `thinking` goes to `Event::thinking`; `tool_use` and
//! `token_budget` carry no prose.

use crate::{Event, SourceKind, TextSpan};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// The files that make a directory a claude.ai export. `conversations.json`
/// holds the transcripts; `users.json` is the cheap proof it is an export and
/// not some other JSON that happens to share a name.
pub const EXPORT_FILES: &[&str] = &["conversations.json", "users.json"];

/// True when `dir` looks like a claude.ai export.
pub fn is_export_dir(dir: &Path) -> bool {
    EXPORT_FILES.iter().all(|f| dir.join(f).is_file())
}

#[derive(Debug, Deserialize)]
pub struct Conversation {
    pub uuid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub chat_messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub uuid: String,
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub created_at: String,
    /// The flattened text the export also carries. Used only when a message
    /// has no content blocks at all (older exports).
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub content: Vec<serde_json::Value>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

#[derive(Debug, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub file_name: String,
    #[serde(default)]
    pub extracted_content: String,
}

/// Read `conversations.json` from an export directory.
pub fn read_conversations(dir: &Path) -> Result<Vec<Conversation>> {
    let p = dir.join("conversations.json");
    let f = std::fs::File::open(&p).with_context(|| format!("open {}", p.display()))?;
    serde_json::from_reader(std::io::BufReader::new(f))
        .with_context(|| format!("parse {} as a claude.ai export", p.display()))
}

/// One conversation's messages as Events, spine from list order.
pub fn conversation_events(c: &Conversation) -> Vec<Event> {
    let mut events = Vec::with_capacity(c.chat_messages.len());
    let mut prev: Option<String> = None;
    for m in &c.chat_messages {
        let (text, text_provenance) = message_text(m);
        events.push(Event {
            event_id: m.uuid.clone(),
            parent_id: prev.clone(),
            role: if m.sender == "human" {
                "user".to_string()
            } else {
                m.sender.clone()
            },
            timestamp: (!m.created_at.is_empty()).then(|| m.created_at.clone()),
            text,
            text_provenance,
            thinking: message_thinking(m),
        });
        prev = Some(m.uuid.clone());
    }
    events
}

/// The version key of a conversation: changes whenever the conversation does.
/// Two exports holding the same conversation are compared by it; the larger
/// wins, so a newer export can only add to what an older one held.
pub fn version_key(c: &Conversation) -> String {
    format!("{}|{:06}", c.updated_at, c.chat_messages.len())
}

fn message_text(m: &Message) -> (Option<String>, Vec<TextSpan>) {
    let mut out = String::new();
    let mut spans = Vec::new();
    let mut push = |piece: &str, kind: SourceKind, out: &mut String| {
        if piece.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let start = out.len();
        out.push_str(piece);
        spans.push(TextSpan {
            start,
            end: out.len(),
            kind,
        });
    };
    if m.content.is_empty() {
        push(&m.text, SourceKind::Authored, &mut out);
    }
    for b in &m.content {
        match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" | "voice_note" => {
                if let Some(s) = b.get("text").and_then(|t| t.as_str()) {
                    push(s, SourceKind::Authored, &mut out);
                }
            }
            "tool_result" => {
                if let Some(s) = tool_result_text(b) {
                    push(&s, SourceKind::ToolResult, &mut out);
                }
            }
            _ => {}
        }
    }
    for a in &m.attachments {
        if !a.extracted_content.is_empty() {
            let piece = format!("[attachment: {}]\n{}", a.file_name, a.extracted_content);
            push(&piece, SourceKind::ToolResult, &mut out);
        }
    }
    if out.is_empty() {
        (None, Vec::new())
    } else {
        (Some(out), spans)
    }
}

fn tool_result_text(b: &serde_json::Value) -> Option<String> {
    let content = b.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    let parts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|it| it.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|it| it.get("text").and_then(|t| t.as_str()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn message_thinking(m: &Message) -> Option<String> {
    let parts: Vec<&str> = m
        .content
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"))
        .filter_map(|b| b.get("thinking").and_then(|t| t.as_str()))
        .filter(|s| !s.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A two-conversation export in the real shape, small enough to read.
    pub(crate) fn fixture_json(second_reply: bool) -> String {
        let extra = if second_reply {
            r#",{"uuid":"m4","sender":"assistant","created_at":"2024-01-02T00:00:03Z","text":"","content":[{"type":"text","text":"and more"}],"attachments":[],"files":[]}"#
        } else {
            ""
        };
        let updated = if second_reply {
            "2024-01-02T00:00:03Z"
        } else {
            "2024-01-02T00:00:02Z"
        };
        format!(
            r#"[
 {{"uuid":"c1","name":"first","summary":"","created_at":"2024-01-01T00:00:00Z","updated_at":"2024-01-01T00:00:01Z","account":{{"uuid":"a"}},
  "chat_messages":[
   {{"uuid":"m1","sender":"human","created_at":"2024-01-01T00:00:00Z","text":"","content":[{{"type":"text","text":"hello"}}],
     "attachments":[{{"file_name":"paste.txt","file_size":3,"file_type":"txt","extracted_content":"pasted"}}],"files":[]}},
   {{"uuid":"m2","sender":"assistant","created_at":"2024-01-01T00:00:01Z","text":"","content":[
     {{"type":"thinking","thinking":"hm"}},
     {{"type":"text","text":"hi"}},
     {{"type":"tool_use","name":"x","input":{{}}}},
     {{"type":"tool_result","content":[{{"type":"text","text":"tool says"}}]}}],"attachments":[],"files":[]}}
  ]}},
 {{"uuid":"c2","name":"second","summary":"","created_at":"2024-01-02T00:00:00Z","updated_at":"{updated}","account":{{"uuid":"a"}},
  "chat_messages":[
   {{"uuid":"m3","sender":"human","created_at":"2024-01-02T00:00:02Z","text":"old flat text","content":[],"attachments":[],"files":[]}}{extra}
  ]}},
 {{"uuid":"c3","name":"empty","summary":"","created_at":"2024-01-03T00:00:00Z","updated_at":"2024-01-03T00:00:00Z","account":{{"uuid":"a"}},"chat_messages":[]}}
]"#
        )
    }

    pub(crate) fn write_fixture(dir: &Path, second_reply: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("conversations.json"), fixture_json(second_reply)).unwrap();
        std::fs::write(dir.join("users.json"), r#"[{"uuid":"a"}]"#).unwrap();
    }

    fn convs() -> Vec<Conversation> {
        serde_json::from_str(&fixture_json(false)).unwrap()
    }

    #[test]
    fn spine_follows_list_order_and_human_is_user() {
        let ev = conversation_events(&convs()[0]);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].event_id, "m1");
        assert_eq!(ev[0].parent_id, None);
        assert_eq!(ev[0].role, "user");
        assert_eq!(ev[1].parent_id.as_deref(), Some("m1"));
        assert_eq!(ev[1].role, "assistant");
        assert_eq!(ev[1].timestamp.as_deref(), Some("2024-01-01T00:00:01Z"));
    }

    #[test]
    fn prose_quoted_and_thinking_are_kept_apart() {
        let ev = conversation_events(&convs()[0]);
        // human: typed text authored, the pasted attachment quoted
        let t0 = ev[0].text.as_deref().unwrap();
        assert!(t0.starts_with("hello\n[attachment: paste.txt]\npasted"));
        assert_eq!(ev[0].source_kind_at(0), Some(SourceKind::Authored));
        assert_eq!(
            ev[0].source_kind_at(t0.find("pasted").unwrap()),
            Some(SourceKind::ToolResult)
        );
        // assistant: prose authored, tool output quoted, thinking out of text
        let t1 = ev[1].text.as_deref().unwrap();
        assert_eq!(t1, "hi\ntool says");
        assert_eq!(
            ev[1].source_kind_at(t1.find("tool").unwrap()),
            Some(SourceKind::ToolResult)
        );
        assert_eq!(ev[1].thinking.as_deref(), Some("hm"));
    }

    #[test]
    fn a_message_without_blocks_falls_back_to_its_flat_text() {
        let ev = conversation_events(&convs()[1]);
        assert_eq!(ev[0].text.as_deref(), Some("old flat text"));
    }

    #[test]
    fn version_key_grows_with_the_conversation() {
        let a: Vec<Conversation> = serde_json::from_str(&fixture_json(false)).unwrap();
        let b: Vec<Conversation> = serde_json::from_str(&fixture_json(true)).unwrap();
        assert!(version_key(&b[1]) > version_key(&a[1]));
        assert_eq!(version_key(&b[0]), version_key(&a[0]));
    }
}
