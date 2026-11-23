//! Project generic `Event`s into an oxigraph Store as RDF 1.2.
//!
//! Subject URNs are partition-scoped: `<partition><event_id>` where the adapter
//! supplies `partition` (for the soul adapter, `https://repolex.ai/ravel/Turn/`,
//! carrying no soul identity — soul scoping is the store). The engine
//! treats `partition` as an opaque prefix — it does not know it means "soul".

use crate::{Event, Wrote, RAVEL_NS};
use anyhow::Result;
use oxigraph::store::Store;

/// Build the subject IRI for an event under a partition.
pub fn event_iri(partition: &str, event_id: &str) -> String {
    format!("{partition}{event_id}")
}

/// Validate an event_id before it's embedded in an IRI. VALIDATE, don't mangle:
/// a `safe()`-style character mangle on an IDENTITY key can collide two
/// distinct ids ("a b" and "a_b" → same IRI) — identity-loss hidden as
/// robustness. The claude-code adapter sources event_id from JSONL uuids (hex +
/// hyphens); the agy adapter synthesizes `<conversation-uuid>:<step_index>`.
///
/// `:` was added to the allowlist on 2026-08-26 when the agy adapter — the
/// first non-UUID dialect, which this gate was written to catch — hit it. It is
/// a deliberate WIDENING, not a mangle: `:` is a legal `pchar` in an IRI path
/// segment (RFC 3987), and since neither a UUID nor a decimal index can contain
/// one, no two distinct agy ids can collide through it and no agy id can
/// collide with a claude uuid.
pub fn validate_event_id(event_id: &str) -> Result<()> {
    if event_id.trim().is_empty() {
        anyhow::bail!("empty event_id — refusing to derive an IRI for a non-event");
    }
    if !event_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ':')
    {
        anyhow::bail!(
            "malformed event_id {event_id:?} — contains characters unsafe to embed in an IRI; fix the adapter, don't mangle the id"
        );
    }
    Ok(())
}

/// The two names an author can be built from: the soul whose transcripts
/// these are, and the person at this machine. Either may be unknown, and
/// then no author is written for the turns it would have named.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Who {
    pub soul: Option<String>,
    pub person: Option<String>,
}

impl Who {
    pub fn of(names: &crate::names::Names) -> Self {
        Self {
            soul: names
                .username()
                .filter(|u| crate::names::plain_username(u))
                .map(str::to_string),
            person: crate::names::machine_owner(),
        }
    }
}

/// Who wrote a turn, as `ravel:author` states it (ruled by goodlux,
/// 2026-11-22; subtexture.ttl 0.7.0): `{username}-on-{model}` for a reply
/// whose record names its model, the bare username when it names none, the
/// bare username for a person always. A herdr sender line names its own
/// user, bare: it is that sender's word, like every `claimedSender*`. None
/// when the record names no one; a model is never guessed.
fn author(e: &Event, who: &Who, claim: Option<&crate::herdr::SenderClaim>) -> Option<String> {
    if let Some(c) = claim {
        return c
            .user
            .clone()
            .filter(|u| crate::names::plain_username(u));
    }
    match e.wrote {
        Wrote::Soul => {
            let u = who.soul.as_deref()?;
            Some(match &e.model {
                Some(m) => format!("{u}-on-{m}"),
                None => u.to_string(),
            })
        }
        // A peer's message with no sender line names no one we can state.
        Wrote::Person if e.text.as_deref().is_some_and(crate::herdr::unsigned_peer_line) => None,
        Wrote::Person => who.person.clone(),
        Wrote::Nobody => None,
    }
}

/// Load events into a fresh in-memory Store, emitting the spine + scalar props.
/// Returns the Store. Idempotent by construction: subject IRIs are deterministic
/// (partition + event_id), so re-projecting the same events overwrites, never
/// duplicates — the w4r3z idempotency discipline, at the triple level.
pub fn project(events: &[Event], partition: &str) -> Result<Store> {
    let store = Store::new()?;
    store.load_from_reader(
        oxigraph::io::RdfFormat::NTriples,
        project_nt(events, partition, &Who::default())?.as_bytes(),
    )?;
    Ok(store)
}

/// Project events to N-Triples (the Turn nodes: type, role, spine, timestamp,
/// text). Exposed alongside `annotate::annotation_nt` so the persistent-graph
/// ingest can load the Turn NODES into the same named graph as the annotations
/// that reference them — without this, an annotation's soul-prefixed
/// `ravel:atTurn` / `prov:used` anchor points at a Turn that was never derived,
/// and the federation TIME anchor (`ravel:timestamp` xsd:dateTime) is absent, so
/// a soul+time cross-store join has soul but not time. The Turn node is what
/// carries the time half of the anchor contract.
pub fn project_nt(events: &[Event], partition: &str, who: &Who) -> Result<String> {
    let mut nt = String::new();

    let turn_class = format!("{RAVEL_NS}Turn");
    let p_turn_id = format!("{RAVEL_NS}turnId");
    let p_parent = format!("{RAVEL_NS}parentTurn");
    let p_ts = format!("{RAVEL_NS}timestamp");
    let p_text = format!("{RAVEL_NS}text");
    let p_role = format!("{RAVEL_NS}role");
    let p_author = format!("{RAVEL_NS}author");
    let p_setting = format!("{RAVEL_NS}modelSetting");
    let xsd_dt = "http://www.w3.org/2001/XMLSchema#dateTime";

    for e in events {
        validate_event_id(&e.event_id)?;
        let s = event_iri(partition, &e.event_id);
        // rdf:type ravel:Turn
        nt.push_str(&format!(
            "<{s}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{turn_class}> .\n"
        ));
        // per-class id (kit-ontology law: every class declares <class>Id)
        nt.push_str(&format!("<{s}> <{p_turn_id}> {} .\n", lit(&e.event_id)));
        // role (plain literal)
        nt.push_str(&format!("<{s}> <{p_role}> {} .\n", lit(&e.role)));
        // spine
        if let Some(pid) = &e.parent_id {
            let p = event_iri(partition, pid);
            nt.push_str(&format!("<{s}> <{p_parent}> <{p}> .\n"));
        }
        // timestamp as xsd:dateTime (federation anchor lexical form)
        if let Some(ts) = &e.timestamp {
            nt.push_str(&format!(
                "<{s}> <{p_ts}> \"{}\"^^<{xsd_dt}> .\n",
                escape(ts)
            ));
        }
        // text (plain literal)
        if let Some(tx) = &e.text {
            nt.push_str(&format!("<{s}> <{p_text}> {} .\n", lit(tx)));
        }
        if let Some(m) = &e.model_setting {
            nt.push_str(&format!("<{s}> <{p_setting}> {} .\n", lit(m)));
        }
        // A herdr sender line, on input only: what the sender says of itself.
        // Input only: Claude Code files it as `user`, Antigravity as
        // `USER_EXPLICIT`. The same line in a reply is the soul quoting it.
        let claim = (e.role == "user" || e.role == "USER_EXPLICIT")
            .then(|| e.text.as_deref().and_then(crate::herdr::sender_claim))
            .flatten();
        if let Some(a) = author(e, who, claim.as_ref()) {
            nt.push_str(&format!("<{s}> <{p_author}> {} .\n", lit(&a)));
        }
        {
            if let Some(c) = claim {
                for (name, v) in [
                    ("claimedSender", c.from),
                    ("claimedSenderPane", c.pane),
                    ("claimedSenderSession", c.session),
                    ("claimedSenderSoul", c.soul),
                    ("claimedSenderUser", c.user),
                ] {
                    if let Some(v) = v {
                        nt.push_str(&format!("<{s}> <{RAVEL_NS}{name}> {} .\n", lit(&v)));
                    }
                }
            }
        }
    }

    Ok(nt)
}

/// N-Triples-escape a string and wrap as a plain literal.
fn lit(s: &str) -> String {
    format!("\"{}\"", escape(s))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ontology_declares_every_turn_term_the_projection_emits() {
        let ttl = include_str!("../ontology/ravel/ravel.ttl");
        let store = Store::new().unwrap();
        store
            .load_from_reader(oxigraph::io::RdfFormat::Turtle, ttl.as_bytes())
            .expect("ravel.ttl parses");
        let e = Event {
            event_id: "e1".into(),
            parent_id: Some("e0".into()),
            role: "assistant".into(),
            timestamp: Some("2026-10-14T12:00:00Z".into()),
            text: Some("hello".into()),
            text_provenance: vec![],
            thinking: None,
            model: Some("claude-opus-5-5".into()),
            model_setting: Some("Gemini 4 Argon (Medium)".into()),
            wrote: crate::Wrote::Soul,
            actions: Vec::new(),
            outcomes: Vec::new(),
        };
        let said = Event {
            event_id: "e2".into(),
            role: "user".into(),
            text: Some(
                "--herdr v=1 from=th34 pane=w9:p1 session=80992ee3 soul=9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b--\nhello"
                    .into(),
            ),
            ..e.clone()
        };
        // The same line in a reply is the soul quoting it, not a sender.
        let quoted = Event {
            event_id: "e3".into(),
            role: "assistant".into(),
            ..said.clone()
        };
        let who = Who { soul: Some("spacegoat".into()), person: Some("goodlux".into()) };
        let nt = project_nt(&[quoted], "urn:test:", &who).unwrap();
        assert!(!nt.contains("claimedSender"));
        let nt = project_nt(&[e, said], "urn:test:", &who).unwrap();
        assert!(nt.contains("claimedSender> \"th34\""));
        assert!(nt.contains("claimedSenderSoul> \"9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b\""));
        let mut terms: Vec<String> = nt
            .split_whitespace()
            .filter_map(|w| w.strip_prefix(&format!("<{RAVEL_NS}")))
            .map(|w| w.trim_end_matches('>').to_string())
            .collect();
        terms.sort();
        terms.dedup();
        assert!(
            terms.contains(&"author".to_string()) && terms.contains(&"modelSetting".to_string())
        );
        assert!(!terms.contains(&"model".to_string()), "ravel:model is retired");
        for t in terms {
            let q = format!("ASK {{ <{RAVEL_NS}{t}> a ?k }}");
            let ok = matches!(
                oxigraph::sparql::SparqlEvaluator::new()
                    .parse_query(&q)
                    .unwrap()
                    .on_store(&store)
                    .execute()
                    .unwrap(),
                oxigraph::sparql::QueryResults::Boolean(true)
            );
            assert!(ok, "ravel:{t} is emitted but not declared in ravel.ttl");
        }
    }

    fn turn(id: &str, role: &str, text: &str, model: Option<&str>, wrote: Wrote) -> Event {
        Event {
            event_id: id.into(),
            parent_id: None,
            role: role.into(),
            timestamp: None,
            text: Some(text.into()),
            text_provenance: vec![],
            thinking: None,
            model: model.map(str::to_string),
            model_setting: None,
            wrote,
            actions: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    fn authors(nt: &str, id: &str) -> Vec<String> {
        nt.lines()
            .filter(|l| l.starts_with(&format!("<urn:test:{id}> <{RAVEL_NS}author> ")))
            .map(|l| l.split('"').nth(1).unwrap().to_string())
            .collect()
    }

    #[test]
    fn an_author_names_the_soul_and_its_model_or_the_person_and_guesses_nothing() {
        let who = Who { soul: Some("s3lk13".into()), person: Some("goodlux".into()) };
        let events = [
            turn("a", "assistant", "hi", Some("claude-fable-5-2"), Wrote::Soul),
            turn("b", "assistant", "hi", None, Wrote::Soul),
            turn("c", "user", "fix it", None, Wrote::Person),
            turn("d", "user", "tool output", None, Wrote::Nobody),
            // A herdr line names its own user, whatever else the turn is.
            turn("e", "user", "--herdr v=1 from=th34 pane=w9:p1 user=4m41th34--\nhello", None, Wrote::Person),
            // A sender line with no user names no one.
            turn("f", "user", "--herdr v=1 from=th34 pane=w9:p1--\nhello", None, Wrote::Person),
        ];
        let nt = project_nt(&events, "urn:test:", &who).unwrap();
        assert_eq!(authors(&nt, "a"), ["s3lk13-on-claude-fable-5-2"]);
        assert_eq!(authors(&nt, "b"), ["s3lk13"]);
        assert_eq!(authors(&nt, "c"), ["goodlux"]);
        assert!(authors(&nt, "d").is_empty());
        assert_eq!(authors(&nt, "e"), ["4m41th34"]);
        assert!(authors(&nt, "f").is_empty());
        let peer = [turn("g", "user", "[kira] GREEN LIGHT", None, Wrote::Person)];
        assert!(!project_nt(&peer, "urn:test:", &who).unwrap().contains("author>"));
        assert!(!nt.contains(&format!("<{RAVEL_NS}model>")));

        // A store with no username, on a machine with no recorded owner.
        let nt = project_nt(&events, "urn:test:", &Who::default()).unwrap();
        assert_eq!(nt.matches("author>").count(), 1, "only the herdr line still names someone");
    }

    /// Real transcripts, read-only: how many turns each author gets, and a
    /// sample of the person's. `RAVEL_TRIAL_FILE` is a Claude Code jsonl,
    /// `RAVEL_TRIAL_AGY` an Antigravity one (either may be unset).
    #[test]
    #[ignore]
    fn real_authors() {
        let who = Who { soul: Some("soul".into()), person: Some("person".into()) };
        let mut sets: Vec<(String, Vec<Event>)> = Vec::new();
        if let Ok(f) = std::env::var("RAVEL_TRIAL_FILE") {
            let e = crate::adapter::parse_transcript(&std::fs::read_to_string(&f).unwrap()).unwrap();
            sets.push((f, e));
        }
        if let Ok(f) = std::env::var("RAVEL_TRIAL_AGY") {
            let e = crate::agy::parse_transcript(&std::fs::read_to_string(&f).unwrap(), "c").unwrap().events;
            sets.push((f, e));
        }
        for (f, events) in sets {
            let mut n: std::collections::BTreeMap<String, usize> = Default::default();
            let mut shown = 0;
            for e in &events {
                let claim = (e.role == "user" || e.role == "USER_EXPLICIT").then(|| e.text.as_deref().and_then(crate::herdr::sender_claim)).flatten();
                let a = author(e, &who, claim.as_ref()).unwrap_or_else(|| format!("(none, {})", e.role));
                if a == "person" && shown < 12 {
                    shown += 1;
                    let t: String = e.text.as_deref().unwrap_or("").chars().take(90).collect();
                    println!("  PERSON: {}", t.replace('\n', " "));
                }
                *n.entry(a).or_default() += 1;
            }
            println!("{f}\n  {n:?}");
        }
    }
}
