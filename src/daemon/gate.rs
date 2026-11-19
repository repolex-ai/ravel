//! Who is asking raveld, and whether they may. Every request passes here
//! before its handler, so the whole rule is in one file.
//!
//! The checking itself is tessera's (the stack's shared crate): the token,
//! the key list in `~/.lex/identity/jwks.json`, service grants, the machine
//! owner. ravel only says which route needs what.
//!
//! | Route | Needs |
//! |---|---|
//! | `GET /health`, `GET /souls` | nothing: gitlexd, pand and Syrinx leave the same two open, and Syrinx finds stores through `/souls` |
//! | every read of a soul | a token is not required YET; one that is sent must be good, and a service's grant must say `ravel:read` |
//! | `POST /souls/{id}/sync`, `/import` | that soul, or the machine owner; a service also needs `ravel:write` |
//! | `POST /sync`, `POST /shutdown` | the machine owner |
//!
//! "Not required yet" is the stack's Phase 4 state (goodlux, 2026-11-19):
//! reads identify the caller and refuse a bad token, and the day raveld,
//! pand, gitlexd and Syrinx all refuse a missing one is one switch, flipped
//! together. `READS_NEED_A_TOKEN` is that switch here.
//!
//! A soul is matched by its first commit, not by its address: the address
//! in a token changes when a soul takes a username or a squad, the first
//! commit never does.

use super::registry::Soul;
use super::Daemon;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;
use tessera::{Caller, Owner};

/// Phase 5 turns this on, in every daemon at once.
pub const READS_NEED_A_TOKEN: bool = false;

pub const READ: &str = "ravel:read";
pub const WRITE: &str = "ravel:write";

/// What a route asks of its caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Needs<'a> {
    Nothing,
    Read,
    /// Write to the soul with this route id (six characters).
    WriteSoul(&'a str),
    MachineOwner,
}

/// The one table of routes. A path not listed here is a read.
pub fn needs<'a>(method: &Method, path: &'a str) -> Needs<'a> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, parts.as_slice()) {
        (&Method::GET, ["health"]) | (&Method::GET, ["souls"]) => Needs::Nothing,
        (&Method::POST, ["sync"]) | (&Method::POST, ["shutdown"]) => Needs::MachineOwner,
        (&Method::POST, ["souls", id, "sync"]) | (&Method::POST, ["souls", id, "import"]) => {
            Needs::WriteSoul(id)
        }
        _ => Needs::Read,
    }
}

/// A refusal: the status, what to tell the caller, and the RFC 6750 header.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    pub reason: String,
    pub www_authenticate: Option<String>,
}

fn no_token(what: &str) -> Refusal {
    Refusal {
        status: 401,
        reason: format!(
            "{what} needs to know who is asking. Send Authorization: Bearer <token>; \
             `git lex identity token` prints one, after `git lex identity bind` in the soul's folder. \
             The `ravel` command sends it by itself."
        ),
        www_authenticate: Some(format!("Bearer realm=\"{}\"", tessera::AUDIENCE)),
    }
}

fn forbidden(reason: String) -> Refusal {
    Refusal { status: 403, reason, www_authenticate: None }
}

fn scope(caller: &Caller, word: &str) -> Result<(), Refusal> {
    // Only a service acting under a grant is held to a scope; a soul's own
    // token speaks for the soul.
    if caller.actor.is_none() {
        return Ok(());
    }
    tessera::require_scope(caller, word).map_err(|d| Refusal {
        status: d.http_status(),
        reason: d.reason().to_string(),
        www_authenticate: d.www_authenticate(),
    })
}

/// The decision, with no HTTP in it. `soul` is the soul a `WriteSoul` route
/// names, when raveld serves one by that id.
pub fn decide(
    needs: &Needs<'_>,
    caller: Option<&Caller>,
    soul: Option<&Soul>,
    owner: Option<&Owner>,
    reads_need_a_token: bool,
) -> Result<(), Refusal> {
    let is_owner = |c: &Caller| owner.is_some_and(|o| o.matches(c));
    match needs {
        Needs::Nothing => Ok(()),
        Needs::Read => match caller {
            None if reads_need_a_token => Err(no_token("Reading a soul's store")),
            None => Ok(()),
            Some(c) => scope(c, READ),
        },
        Needs::WriteSoul(id) => {
            let Some(c) = caller else {
                return Err(no_token("Writing to a soul's store"));
            };
            scope(c, WRITE)?;
            // No such soul: the handler answers 404, which says more.
            let Some(soul) = soul else { return Ok(()) };
            if is_owner(c) || c.genesis == soul.genesis {
                Ok(())
            } else {
                Err(forbidden(format!(
                    "soul {id} may be written by itself or by this machine's owner; \
                     this token speaks for {}",
                    c.subject
                )))
            }
        }
        Needs::MachineOwner => {
            let Some(c) = caller else {
                return Err(no_token("This route"));
            };
            if owner.is_none() {
                return Err(forbidden(
                    "this route is for the machine's owner, and no owner is recorded \
                     (`git lex identity owner`)"
                        .into(),
                ));
            }
            // A service never stops raveld or starts a pass over every soul.
            if c.actor.is_none() && is_owner(c) {
                Ok(())
            } else {
                Err(forbidden(format!(
                    "this route is for the machine's owner; this token speaks for {}",
                    c.subject
                )))
            }
        }
    }
}

fn respond(r: Refusal) -> Response {
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::FORBIDDEN);
    let mut resp = (status, Json(json!({ "error": r.reason }))).into_response();
    if let Some(v) = r.www_authenticate.and_then(|v| v.parse().ok()) {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    resp
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The layer every route sits behind.
pub async fn check(State(d): State<Arc<Daemon>>, req: Request, next: Next) -> Response {
    let needs = needs(req.method(), req.uri().path());
    if needs == Needs::Nothing {
        return next.run(req).await;
    }
    let authorization = req
        .headers()
        .get(header::AUTHORIZATION)
        .map(|v| v.to_str().unwrap_or("\u{0}"));
    // A header that is sent and is not a good token is refused on every
    // route, reads included: a caller who thinks it is identified must not
    // be served as nobody.
    let caller = match d.keys.identify(authorization, now()) {
        Ok(c) => c,
        Err(why) => {
            return respond(Refusal {
                status: 401,
                www_authenticate: Some(tessera::www_authenticate_invalid_token(&why.0)),
                reason: why.0,
            })
        }
    };
    let owner = match d.keys.owner() {
        Ok(o) => o,
        Err(why) => {
            return respond(Refusal {
                status: 500,
                reason: format!("the machine owner record cannot be read: {}", why.0),
                www_authenticate: None,
            })
        }
    };
    let soul = match &needs {
        Needs::WriteSoul(id) => d.soul(id).ok(),
        _ => None,
    };
    match decide(&needs, caller.as_ref(), soul, owner.as_ref(), READS_NEED_A_TOKEN) {
        Ok(()) => next.run(req).await,
        Err(r) => respond(r),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "9bdf2afa2a49bfac1b6d6ff7f5f3ab34f04ed44b";
    const OTHER: &str = "b5f080a1bd3c1c143c7397f8835157363e990d7b";
    const BOSS: &str = "4d2437304347fdb68873e076f8909a035e01c4c5";

    fn caller(genesis: &str) -> Caller {
        Caller {
            subject: format!("https://repolex.ai/soul/Soul/{genesis}"),
            genesis: genesis.into(),
            username: None,
            squad: None,
            actor: None,
            scope: None,
            expires: u64::MAX,
        }
    }

    fn service(genesis: &str, scopes: &[&str]) -> Caller {
        Caller {
            actor: Some("subtexture-ui".into()),
            scope: Some(scopes.iter().map(|s| s.to_string()).collect()),
            ..caller(genesis)
        }
    }

    fn soul() -> Soul {
        Soul { id: ME[..6].into(), genesis: ME.into(), path: "/x".into() }
    }

    fn owner() -> Owner {
        Owner { owner: format!("https://repolex.ai/soul/Soul/{BOSS}"), genesis: BOSS.into(), since: 0 }
    }

    fn go(n: Needs<'_>, c: Option<Caller>) -> Result<(), u16> {
        decide(&n, c.as_ref(), Some(&soul()), Some(&owner()), false).map_err(|r| r.status)
    }

    /// This machine's real key list against a real session token:
    /// `RAVEL_TOKEN=$(git lex identity token) cargo test real_token -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_token_is_identified() {
        let keys = tessera::HostKeys::new(crate::daemon::identity_dir().join("jwks.json"));
        let header = format!("Bearer {}", std::env::var("RAVEL_TOKEN").expect("RAVEL_TOKEN"));
        let c = keys.identify(Some(&header), now()).unwrap().expect("a caller");
        let owner = keys.owner().unwrap();
        println!("genesis {} username {:?} squad {:?} actor {:?}", c.genesis, c.username, c.squad, c.actor);
        println!("machine owner recorded: {}, caller is owner: {}", owner.is_some(), owner.as_ref().is_some_and(|o| o.matches(&c)));
        let me = Soul { id: c.genesis[..6].into(), genesis: c.genesis.clone(), path: "/x".into() };
        assert!(decide(&Needs::WriteSoul(&me.id), Some(&c), Some(&me), owner.as_ref(), false).is_ok());
        assert_eq!(decide(&Needs::MachineOwner, Some(&c), None, owner.as_ref(), false).unwrap_err().status, 403);
    }

    #[test]
    fn the_route_table() {
        assert_eq!(needs(&Method::GET, "/health"), Needs::Nothing);
        assert_eq!(needs(&Method::GET, "/souls"), Needs::Nothing);
        assert_eq!(needs(&Method::POST, "/sync"), Needs::MachineOwner);
        assert_eq!(needs(&Method::POST, "/shutdown"), Needs::MachineOwner);
        assert_eq!(needs(&Method::POST, "/souls/9bdf2a/sync"), Needs::WriteSoul("9bdf2a"));
        assert_eq!(needs(&Method::POST, "/souls/9bdf2a/import"), Needs::WriteSoul("9bdf2a"));
        assert_eq!(needs(&Method::POST, "/souls/9bdf2a/query"), Needs::Read);
        assert_eq!(needs(&Method::POST, "/stores/9bdf2a/sparql"), Needs::Read);
        assert_eq!(needs(&Method::GET, "/souls/9bdf2a/memory"), Needs::Read);
        // A route added later and not listed is held as a read, never open.
        assert_eq!(needs(&Method::GET, "/souls/9bdf2a/anything-new"), Needs::Read);
        // GET on a write path is not a write, and is not open either.
        assert_eq!(needs(&Method::GET, "/shutdown"), Needs::Read);
    }

    #[test]
    fn a_read_with_no_token_passes_until_the_switch() {
        assert_eq!(go(Needs::Read, None), Ok(()));
        let r = decide(&Needs::Read, None, None, Some(&owner()), true).unwrap_err();
        assert_eq!(r.status, 401);
        assert!(r.www_authenticate.is_some());
    }

    #[test]
    fn a_service_reads_only_with_the_read_scope() {
        assert_eq!(go(Needs::Read, Some(service(OTHER, &[READ]))), Ok(()));
        assert_eq!(go(Needs::Read, Some(service(OTHER, &["pan:read"]))), Err(403));
    }

    #[test]
    fn a_soul_writes_to_itself_and_to_no_other() {
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), None), Err(401));
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(caller(ME))), Ok(()));
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(caller(OTHER))), Err(403));
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(caller(BOSS))), Ok(()));
    }

    #[test]
    fn a_soul_is_matched_by_first_commit_whatever_its_address() {
        let mut c = caller(ME);
        c.subject = "https://repolex.ai/7r1pl3f0rc3/spacegoat/soul/Soul/spacegoat".into();
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(c)), Ok(()));
    }

    #[test]
    fn a_service_writes_only_with_the_write_scope() {
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(service(ME, &[WRITE]))), Ok(()));
        assert_eq!(go(Needs::WriteSoul("9bdf2a"), Some(service(ME, &[READ]))), Err(403));
    }

    #[test]
    fn only_the_machine_owner_stops_raveld() {
        assert_eq!(go(Needs::MachineOwner, None), Err(401));
        assert_eq!(go(Needs::MachineOwner, Some(caller(ME))), Err(403));
        assert_eq!(go(Needs::MachineOwner, Some(caller(BOSS))), Ok(()));
        assert_eq!(go(Needs::MachineOwner, Some(service(BOSS, &[WRITE]))), Err(403));
        let r = decide(&Needs::MachineOwner, Some(&caller(BOSS)), None, None, false);
        assert_eq!(r.unwrap_err().status, 403);
    }
}
