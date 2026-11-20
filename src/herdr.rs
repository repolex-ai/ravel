//! The sender line on a herdr message.
//!
//! `herdr agent prompt` types a peer's text into a pane, so the harness files
//! it as a turn written by the human. The squad's send command (th34,
//! 2026-11-18) puts one fixed line first:
//!
//! ```text
//! --herdr v=1 from=<name> pane=<pane-id> session=<harness-session-id> soul=<40 hex> user=<username>--
//! ```
//!
//! alone on the first line, the body unchanged after it. Everything on it is
//! what the sender SAYS about itself: typed text, signed by nothing. ravel
//! records it as claimed and never as who the turn is from.
//!
//! Read only at the very start of a turn that arrived as input, so a line
//! quoted further down, or inside a reply, is never taken for a sender.

/// What a herdr sender line claims. Every field is a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderClaim {
    /// The sender's herdr name. Absent when its pane had none (`from=`).
    pub from: Option<String>,
    pub pane: Option<String>,
    /// The sender's harness session id: what ravel files its transcript under.
    pub session: Option<String>,
    /// The sender's first commit, all 40 characters. Dropped unless it is
    /// exactly 40 hex characters: a shortened id names more than one soul.
    pub soul: Option<String>,
    /// The sender's permanent username (`soul.Soul.username`), added to the
    /// line on 2026-11-19. Dropped unless it fits the stack's slug rule,
    /// `^[a-z0-9]+(-[a-z0-9]+)*$`: it is the `{user}` part of an address.
    pub user: Option<String>,
}

/// The stack's slug rule for a username.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Claude Code wraps a long pasted or typed-in input in this pair of lines;
/// a herdr message of any length arrives inside it.
const PASTE_OPEN: &str = "<pasted_content id=\"";

/// The sender line of `text`, when its first line is one this version reads.
///
/// `None` for a turn with no such line, a version other than 1, or a line
/// that is not exactly `--herdr <key=value> ...--`. Unknown keys, and the
/// reserved `id`, `re` and `sig`, are skipped.
pub fn sender_claim(text: &str) -> Option<SenderClaim> {
    // The harness stores a wrapped input with blank lines before the wrapper.
    let mut lines = text.trim_start().lines();
    let mut first = lines.next()?;
    if first.starts_with(PASTE_OPEN) && first.ends_with("\">") {
        first = lines.next()?;
    }
    let inner = first.strip_prefix("--herdr ")?.strip_suffix("--")?;
    let mut keys = inner.split(' ').filter(|k| !k.is_empty());
    if keys.next()? != "v=1" {
        return None;
    }
    let mut c = SenderClaim {
        from: None,
        pane: None,
        session: None,
        soul: None,
        user: None,
    };
    for kv in keys {
        let (k, v) = kv.split_once('=')?;
        if v.is_empty() {
            continue;
        }
        let v = Some(v.to_string());
        match k {
            "from" => c.from = v,
            "pane" => c.pane = v,
            "session" => c.session = v,
            "soul" => {
                c.soul = v.filter(|s| s.len() == 40 && s.chars().all(|ch| ch.is_ascii_hexdigit()))
            }
            "user" => c.user = v.filter(|s| is_slug(s)),
            _ => {}
        }
    }
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_username_is_kept_only_when_it_is_a_slug() {
        let l = |u: &str| format!("--herdr v=1 from=m4xx soul={SOUL} user={u} id=ab--");
        assert_eq!(sender_claim(&l("m4xx")).unwrap().user.as_deref(), Some("m4xx"));
        assert_eq!(sender_claim(&l("tr1p-l3x")).unwrap().user.as_deref(), Some("tr1p-l3x"));
        for bad in ["spaceGOAT", "a--b", "-a", "a-", "a/b", "a.b"] {
            let c = sender_claim(&l(bad)).unwrap();
            assert_eq!(c.user, None, "{bad}");
            assert_eq!(c.from.as_deref(), Some("m4xx"), "the rest of the line still reads");
        }
    }

    const SOUL: &str = "9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b";

    fn line() -> String {
        format!("--herdr v=1 from=th34 pane=w9:p1 session=80992ee3-b99f soul={SOUL}--")
    }

    #[test]
    fn the_first_line_is_read_bare_and_inside_a_paste() {
        let want = SenderClaim {
            from: Some("th34".into()),
            pane: Some("w9:p1".into()),
            session: Some("80992ee3-b99f".into()),
            soul: Some(SOUL.into()),
            user: None,
        };
        assert_eq!(
            sender_claim(&format!("{}\nhello", line())),
            Some(want.clone())
        );
        let pasted = format!(
            "<pasted_content id=\"5463\">\n{}\nhello\n</pasted_content id=\"5463\">",
            line()
        );
        assert_eq!(sender_claim(&pasted), Some(want.clone()));
        // As Claude Code really stored th34's first message, 2026-11-18.
        assert_eq!(sender_claim(&format!("\n\n{pasted}")), Some(want));
    }

    #[test]
    fn a_line_anywhere_but_first_claims_nothing() {
        assert_eq!(sender_claim(&format!("look at this:\n{}", line())), None);
        assert_eq!(sender_claim(&format!("> {}", line())), None);
        assert_eq!(sender_claim(&format!("```\n{}\n```", line())), None);
        assert_eq!(sender_claim("no header here"), None);
        assert_eq!(sender_claim(""), None);
    }

    #[test]
    fn another_version_or_a_broken_line_claims_nothing() {
        assert_eq!(sender_claim("--herdr v=2 from=x--\nbody"), None);
        assert_eq!(sender_claim("--herdr from=x v=1--\nbody"), None);
        assert_eq!(sender_claim("--herdr v=1 from=x\nbody"), None);
        assert_eq!(sender_claim("--herdr v=1 from x--\nbody"), None);
    }

    #[test]
    fn an_empty_name_a_short_soul_and_reserved_keys_are_left_out() {
        let c = sender_claim(
            "--herdr v=1 from= pane=w1:p1 soul=9bdf2a id=00ff re=11ee sig=abc later=1--\nbody",
        )
        .unwrap();
        assert_eq!(
            c,
            SenderClaim {
                from: None,
                pane: Some("w1:p1".into()),
                session: None,
                soul: None,
                user: None,
            }
        );
    }
}
