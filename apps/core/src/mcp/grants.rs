//! The tokens that let a run reach the core's MCP: one per run, kept only in memory.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use weacon_agents::AgentRun;
use weacon_observability::Fault;
use weacon_protocol::{PermissionAnswer, RunEvent, RunId, WorkspaceId, code, to_hex};

use crate::chats::engine::agent::Agent;

/// How many of a run's tool calls may be answered at once, so a looping agent can't hold the
/// daemon's threads.
pub(crate) const CALLS_AT_ONCE: usize = 2;

/// A run's token, which it sends as `Authorization: Bearer <token>`.
pub(crate) struct Token(String);

impl Token {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(..)")
    }
}

/// What a token lets its run read: the code of its own folder, in its own workspace.
#[derive(Debug)]
pub(crate) struct Grant {
    pub(crate) workspace: WorkspaceId,
    pub(crate) run: RunId,
    pub(crate) folder: PathBuf,
    pub(crate) calls: Semaphore,
    revoked: AtomicBool,
}

impl Grant {
    pub(crate) fn new(workspace: WorkspaceId, run: RunId, folder: PathBuf) -> Self {
        Self {
            workspace,
            run,
            folder,
            calls: Semaphore::new(CALLS_AT_ONCE),
            revoked: AtomicBool::new(false),
        }
    }

    /// Whether its token still works.
    pub(crate) fn is_live(&self) -> bool {
        !self.revoked.load(Ordering::Relaxed)
    }
}

type Held = Vec<([u8; 32], Arc<Grant>)>;

/// The tokens of the runs alive now, by their hash.
#[derive(Clone, Debug, Default)]
pub(crate) struct Grants(Arc<Mutex<Held>>);

impl Grants {
    /// A new token for `grant`, which works until the [`Revoke`] is dropped.
    pub(crate) fn issue(&self, grant: Grant) -> Result<(Token, Revoke), Fault> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| {
            Fault::new(code!("random.failed"), "The core couldn't get random bytes")
        })?;
        let token = Token(to_hex(&bytes));
        let hash = hash(token.as_str());
        self.held().push((hash, Arc::new(grant)));
        let revoke = Revoke {
            grants: self.clone(),
            hash,
        };
        Ok((token, revoke))
    }

    /// The grant of a live run's token. Every held hash is compared in constant time.
    pub(crate) fn find(&self, token: &str) -> Option<Arc<Grant>> {
        let hash = hash(token);
        let held = self.held();
        // Every hash is compared, so the time taken says nothing about which one matched.
        held.iter().fold(None, |found, (held, grant)| {
            if bool::from(held.ct_eq(&hash)) {
                Some(Arc::clone(grant))
            } else {
                found
            }
        })
    }

    /// How many tokens work now.
    #[cfg(test)]
    pub(crate) fn live(&self) -> usize {
        self.held().len()
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Ends a token when dropped.
#[derive(Debug)]
pub(crate) struct Revoke {
    grants: Grants,
    hash: [u8; 32],
}

impl Drop for Revoke {
    fn drop(&mut self) {
        self.grants.held().retain(|(hash, grant)| {
            let ends = *hash == self.hash;
            if ends {
                grant.revoked.store(true, Ordering::Relaxed);
            }
            !ends
        });
    }
}

/// An agent whose token ends with it.
pub(crate) struct Granted<A = AgentRun> {
    pub(crate) agent: A,
    pub(crate) _revoke: Option<Revoke>,
}

impl<A: Agent> Agent for Granted<A> {
    async fn send(&mut self, prompt: &str) -> Result<(), Fault> {
        self.agent.send(prompt).await
    }

    async fn answer(&mut self, answer: &PermissionAnswer) -> Result<(), Fault> {
        self.agent.answer(answer).await
    }

    async fn interrupt(&mut self) -> Result<(), Fault> {
        self.agent.interrupt().await
    }

    async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
        self.agent.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fails every order with the name of the call, and says something.
    struct Echo;

    fn refused(call: &'static str) -> Fault {
        Fault::new(code!("test.refused"), call)
    }

    impl Agent for Echo {
        async fn send(&mut self, _: &str) -> Result<(), Fault> {
            Err(refused("send"))
        }

        async fn answer(&mut self, _: &PermissionAnswer) -> Result<(), Fault> {
            Err(refused("answer"))
        }

        async fn interrupt(&mut self) -> Result<(), Fault> {
            Err(refused("interrupt"))
        }

        async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
            Ok(Some(RunEvent::Text { text: "hi".into() }))
        }
    }

    #[tokio::test]
    async fn a_granted_agent_passes_every_call_on() {
        let mut agent = Granted {
            agent: Echo,
            _revoke: None,
        };
        let answer = PermissionAnswer {
            run: RunId::try_from("run-1".to_owned()).unwrap(),
            prompt: 1,
            allow: true,
        };

        let refusals = [
            agent.send("hi").await.unwrap_err(),
            agent.answer(&answer).await.unwrap_err(),
            agent.interrupt().await.unwrap_err(),
        ];

        let said: Vec<_> = refusals.iter().map(ToString::to_string).collect();
        assert_eq!(said, ["send", "answer", "interrupt"]);
        assert!(matches!(
            agent.next().await,
            Ok(Some(RunEvent::Text { .. }))
        ));
    }

    fn grant(workspace: &str, run: &str) -> Grant {
        Grant::new(
            WorkspaceId::try_from(workspace.to_owned()).unwrap(),
            RunId::try_from(run.to_owned()).unwrap(),
            PathBuf::from("/work"),
        )
    }

    #[test]
    fn a_token_finds_its_own_grant_until_it_is_revoked() {
        let grants = Grants::default();
        let (first, revoke_first) = grants.issue(grant("w-1", "run-1")).unwrap();
        let (second, _revoke_second) = grants.issue(grant("w-2", "run-2")).unwrap();

        let found = |token: &str| {
            grants
                .find(token)
                .map(|grant| grant.run.as_str().to_owned())
        };
        assert_eq!(found(first.as_str()).as_deref(), Some("run-1"));
        assert_eq!(found(second.as_str()).as_deref(), Some("run-2"));
        assert_eq!(found(""), None);
        assert_eq!(found(&first.as_str().to_uppercase()), None);

        assert_eq!(grants.live(), 2);
        let held = grants.find(first.as_str()).unwrap();
        assert!(held.is_live());
        drop(revoke_first);
        assert!(!held.is_live());
        assert!(grants.find(second.as_str()).unwrap().is_live());
        assert_eq!(grants.live(), 1);
        assert_eq!(found(first.as_str()), None);
        assert_eq!(found(second.as_str()).as_deref(), Some("run-2"));
    }

    #[test]
    fn tokens_are_long_random_hex_and_their_debug_hides_them() {
        let grants = Grants::default();
        let (one, _a) = grants.issue(grant("w-1", "run-1")).unwrap();
        let (two, _b) = grants.issue(grant("w-1", "run-1")).unwrap();

        assert_eq!(one.as_str().len(), 64);
        assert!(one.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(one.as_str(), two.as_str());
        assert_eq!(format!("{one:?}"), "Token(..)");
    }
}
