//! Who sent a message that arrived as user input: matched from the sender's
//! side.
//!
//! herdr's `agent prompt` types a peer's message straight into a soul's
//! session. Claude Code records it as a user turn; Antigravity as a
//! USER_INPUT (source USER_EXPLICIT). Neither marks it, so on the receiving
//! side a peer's message looks exactly like the human's. On 4RX that made a
//! reader take Kira's automated messages for Rob's and then doubt Rob's real
//! go-aheads (2026-10-13).
//!
//! The sender's own transcript holds the send: a Bash or run_command call
//! `herdr agent prompt <target> "<text>"`, its text byte for byte (4RX:
//! 311 of 469 of Kira's sends exact, 114 more by prefix), or a file the
//! sender wrote just before and sends as `"$(cat <file>)"`. Ravel mirrors
//! every soul's transcripts, so a user turn whose text the sender sent
//! shortly before is that peer's message. Anything unmatched stays the
//! human's.

use std::collections::HashMap;
use std::path::Path;

/// One `herdr agent prompt` call: when, by which soul, and the sent text
/// flattened (see [`flat`]). Empty `flat` when the text could not be read.
#[derive(Debug, Clone)]
pub struct Send {
    pub ts: i64,
    pub sender: String,
    pub flat: String,
    /// A subagent's opening prompt (Antigravity `invoke_subagent`), not a
    /// message to a peer.
    pub spawn: bool,
}

/// Text with whitespace, quotes, backticks and backslashes removed: what
/// survives shell quoting on the way from a command line to a pane.
pub fn flat(t: &str) -> String {
    t.chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | '\\'))
        .collect()
}

fn unix(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp())
}

/// The file a command sends with `$(cat <file>)`, if it does.
fn cat_path(cmd: &str) -> Option<String> {
    let i = cmd.find("$(cat ")?;
    let rest = &cmd[i + 6..];
    let end = rest.find(')')?;
    Some(
        rest[..end]
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string(),
    )
}

/// Files a sender wrote by heredoc (`cat > <file> <<'EOF' … EOF`), as
/// (path, body).
fn heredocs(cmd: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = cmd;
    while let Some(i) = rest.find("cat > ") {
        let after = &rest[i + 6..];
        let Some(nl) = after.find('\n') else { break };
        let head = &after[..nl];
        let path = head
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        let marker = head.split("<<").nth(1).map(|m| {
            m.trim()
                .trim_matches(|c| c == '\'' || c == '"' || c == '-')
                .to_string()
        });
        let body = &after[nl + 1..];
        match marker {
            Some(m) if !m.is_empty() => {
                let end = body.find(&format!("\n{m}")).unwrap_or(body.len());
                out.push((path, body[..end].to_string()));
                rest = &body[end..];
            }
            _ => rest = after,
        }
    }
    out
}

/// The message part of a `herdr agent prompt <target> <message…>` command:
/// everything after the target.
fn body_of(cmd: &str) -> &str {
    let Some(i) = cmd.find("herdr agent prompt") else {
        return cmd;
    };
    let rest = cmd[i + "herdr agent prompt".len()..].trim_start();
    let rest = rest.trim_start_matches(|c: char| !c.is_whitespace());
    rest.trim_start()
}

/// Every `herdr agent prompt` call in one soul's mirrored transcripts.
pub fn sends(repo: &Path, sender: &str) -> Vec<Send> {
    // (ts, command) for every shell command the soul ran; files it wrote.
    let mut cmds: Vec<(i64, String)> = Vec::new();
    let mut written: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    let mut spawns: Vec<(i64, String)> = Vec::new();
    let mut scan = |p: &Path| {
        let Ok(s) = std::fs::read_to_string(p) else {
            return;
        };
        for line in s.lines() {
            let cheap = line.contains("herdr agent prompt")
                || line.contains("\"Write\"")
                || line.contains("write_to_file")
                || line.contains("invoke_subagent")
                || line.contains("cat > ");
            if !cheap {
                continue;
            }
            let Ok(r) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            // Claude Code: assistant tool_use blocks.
            if let (Some(ts), Some(blocks)) = (
                r["timestamp"].as_str().and_then(unix),
                r["message"]["content"].as_array(),
            ) {
                for b in blocks {
                    if b["type"] != "tool_use" {
                        continue;
                    }
                    let inp = &b["input"];
                    match b["name"].as_str() {
                        Some("Bash") => {
                            if let Some(c) = inp["command"].as_str() {
                                cmds.push((ts, c.to_string()));
                            }
                        }
                        Some("Write") => {
                            if let (Some(f), Some(c)) =
                                (inp["file_path"].as_str(), inp["content"].as_str())
                            {
                                written
                                    .entry(f.to_string())
                                    .or_default()
                                    .push((ts, c.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Antigravity: tool_calls with a CommandLine, or a file write.
            if let (Some(ts), Some(calls)) = (
                r["created_at"].as_str().and_then(unix),
                r["tool_calls"].as_array(),
            ) {
                for c in calls {
                    let a = &c["args"];
                    if c["name"] == "invoke_subagent" {
                        for sub in a["Subagents"].as_array().into_iter().flatten() {
                            if let Some(p) = sub["Prompt"].as_str() {
                                spawns.push((ts, p.to_string()));
                            }
                        }
                    }
                    if let Some(cl) = a["CommandLine"].as_str() {
                        cmds.push((ts, cl.to_string()));
                    }
                    if let (Some(f), Some(body)) =
                        (a["TargetFile"].as_str(), a["CodeContent"].as_str())
                    {
                        written
                            .entry(f.to_string())
                            .or_default()
                            .push((ts, body.to_string()));
                    }
                }
            }
        }
    };
    for p in crate::sync::walk_jsonl(&repo.join(crate::sync::TRANSCRIPTS_SUBDIR)) {
        scan(&p);
    }
    let agy = repo.join(crate::sync::AGY_TRANSCRIPTS_SUBDIR);
    if let Ok(rd) = std::fs::read_dir(&agy) {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("jsonl") {
                scan(&e.path());
            }
        }
    }
    for (ts, c) in &cmds {
        for (f, body) in heredocs(c) {
            written.entry(f).or_default().push((*ts, body));
        }
    }
    let mut out = Vec::new();
    for (ts, c) in cmds {
        if !c.contains("herdr agent prompt") {
            continue;
        }
        // The text: from the file it cats, written at or before the send;
        // else the command line itself (the quoted text is inside it).
        let text = match cat_path(&c) {
            Some(f) => written
                .get(&f)
                .and_then(|v| v.iter().filter(|(t, _)| *t <= ts).max_by_key(|(t, _)| *t))
                .map(|(_, b)| b.clone())
                .unwrap_or_default(),
            None => body_of(&c).to_string(),
        };
        out.push(Send {
            ts,
            sender: sender.to_string(),
            flat: flat(&text),
            spawn: false,
        });
    }
    for (ts, p) in spawns {
        out.push(Send {
            ts,
            sender: sender.to_string(),
            flat: flat(&p),
            spawn: true,
        });
    }
    out
}

/// A user turn's peer origin.
#[derive(Debug, Clone, PartialEq)]
pub enum Sent {
    /// The whole turn is the peer's message.
    Whole(String),
    /// The human's words, then the peer's message typed in after them.
    Embedded(String),
    /// The peer's message, then more words that arrived with it
    /// (Antigravity joins input that queues while the agent works).
    Leading(String),
}

/// Every send by every soul, ordered by time.
#[derive(Debug, Default)]
pub struct PeerIndex {
    sends: Vec<Send>,
}

/// How long before a turn its send may have been made, and how far after
/// (clock skew between transcripts): seen 2 s to 11 min on 4RX.
const BEFORE: i64 = 30 * 60;
const AFTER: i64 = 60;
/// The flattened prefix of a turn that must appear in the send.
const KEY: usize = 120;
const MIN_KEY: usize = 20;

impl PeerIndex {
    /// The index over souls given as (repo, name).
    pub fn build(souls: &[(std::path::PathBuf, String)]) -> Self {
        let mut sends: Vec<Send> = souls
            .iter()
            .flat_map(|(repo, name)| sends(repo, name))
            .collect();
        sends.sort_by_key(|s| s.ts);
        PeerIndex { sends }
    }

    /// Whether `text`, the first input of a conversation of `me`'s, is the
    /// prompt `me` opened a subagent with just before: the conversation is
    /// that subagent's, not the soul's own (Kira, 2026-10-13: 870ad044).
    pub fn spawned(&self, me: &str, ts: &str, text: &str) -> bool {
        let Some(t) = unix(ts) else { return false };
        let key: String = flat(text).chars().take(KEY).collect();
        if key.chars().count() < MIN_KEY {
            return false;
        }
        let lo = self.sends.partition_point(|s| s.ts < t - 10 * 60);
        let hi = self.sends.partition_point(|s| s.ts <= t + AFTER);
        self.sends[lo..hi]
            .iter()
            .any(|s| s.spawn && s.sender == me && s.flat.contains(&key))
    }

    pub fn len(&self) -> usize {
        self.sends.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sends.is_empty()
    }

    /// Who sent `text` to `receiver` shortly before `ts`: the whole turn, or
    /// a peer message embedded after the human's own words (herdr typed it
    /// into the pane while the human was typing: W4R3Z, 2026-08-26).
    pub fn sender_of(&self, receiver: &str, ts: &str, text: &str) -> Option<Sent> {
        let t = unix(ts)?;
        let all = flat(text);
        let key: String = all.chars().take(KEY).collect();
        if key.chars().count() < MIN_KEY {
            return None;
        }
        let lo = self.sends.partition_point(|s| s.ts < t - BEFORE);
        let hi = self.sends.partition_point(|s| s.ts <= t + AFTER);
        let near = || {
            self.sends[lo..hi]
                .iter()
                .rev()
                .filter(|s| !s.spawn && s.sender != receiver)
        };
        if let Some(s) = near().find(|s| s.flat.contains(&key)) {
            // The whole turn, unless it runs on past what was sent.
            let whole = s.flat.contains(&all);
            return Some(if whole {
                Sent::Whole(s.sender.clone())
            } else {
                Sent::Leading(s.sender.clone())
            });
        }
        // The sent message inside the turn: where it starts says who spoke
        // first.
        near().find_map(|s| {
            let head: String = s.flat.chars().take(KEY).collect();
            if head.chars().count() < 2 * MIN_KEY {
                return None;
            }
            let at = all.find(&head)?;
            Some(if at > 0 {
                Sent::Embedded(s.sender.clone())
            } else if s.flat.contains(&all) {
                Sent::Whole(s.sender.clone())
            } else {
                Sent::Leading(s.sender.clone())
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_heredoc_body_and_its_cat_are_found() {
        let cmd = "cat > /tmp/m.txt <<'EOF'\nHey 4rx! Wave 1 \"done\".\nEOF\nherdr agent prompt forx \"$(cat /tmp/m.txt)\"";
        assert_eq!(cat_path(cmd).as_deref(), Some("/tmp/m.txt"));
        let h = heredocs(cmd);
        assert_eq!(
            h,
            vec![(
                "/tmp/m.txt".to_string(),
                "Hey 4rx! Wave 1 \"done\".".to_string()
            )]
        );
    }

    #[test]
    fn a_turn_is_matched_to_the_send_that_carried_it() {
        let msg = "Hey 4rx! Wave 175 surge, verified & ingested. It's \"done\".";
        let idx = PeerIndex {
            sends: vec![Send {
                ts: unix("2026-09-19T15:01:14Z").unwrap(),
                sender: "kira".into(),
                spawn: false,
                flat: flat(body_of(&format!(
                    "herdr agent prompt forx \"{}\"",
                    msg.replace('"', "\\\"")
                ))),
            }],
        };
        assert_eq!(
            idx.sender_of("4RX", "2026-09-19T15:01:20Z", msg),
            Some(Sent::Whole("kira".into()))
        );
        // Typed in while the human was typing: the human's words come first.
        let both = format!("hey what is going on? Please ping kira{msg}");
        assert_eq!(
            idx.sender_of("4RX", "2026-09-19T15:01:20Z", &both),
            Some(Sent::Embedded("kira".into()))
        );
        // The human typing the same thing an hour later is the human.
        assert_eq!(idx.sender_of("4RX", "2026-09-19T16:31:20Z", msg), None);
        // The peer's message, then the human's words queued after it.
        let more = format!("{msg} also: Rob here, please hold the next wave");
        assert_eq!(
            idx.sender_of("4RX", "2026-09-19T15:01:20Z", &more),
            Some(Sent::Leading("kira".into()))
        );
        // A soul never sends to itself.
        assert_eq!(idx.sender_of("kira", "2026-09-19T15:01:20Z", msg), None);
        // Too short to tell.
        assert_eq!(idx.sender_of("4RX", "2026-09-19T15:01:20Z", "ok"), None);
    }
}
