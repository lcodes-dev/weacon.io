//! The MCP's one write tool: a run's agent proposes a change to a doc or task of its own
//! workspace, which the server holds until a person accepts it.

use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{ProposalAction, ProposeArgs, code};

use super::grants::Grant;
use crate::proposals::{Proposals, proposal_id};

pub(crate) const PROPOSED: &str = "Proposed. It changes nothing until a person accepts it.\n";
const NO_SERVER: &str = "Not proposed: this machine isn't connected to a server.\n";
const UNANSWERED: &str =
    "The server didn't answer in time, so it may hold the proposal yet. Don't propose it again.\n";

/// What to answer the agent: an error when the proposal didn't reach the server.
pub(crate) async fn propose(
    proposals: Option<&Proposals>,
    grant: &Grant,
    args: ProposeArgs,
    log: &Logger,
) -> Result<String, String> {
    let proposals = proposals.ok_or_else(|| NO_SERVER.to_owned())?;
    sent(proposals, grant, args).await.map_err(|fault| {
        log.warn("mcp proposal refused", &[("err", Field::error(&fault))]);
        if *fault.code() == code!("proposal.unanswered") {
            return UNANSWERED.to_owned();
        }
        format!("Not proposed: {}.\n", fault.code().as_str())
    })?;
    Ok(PROPOSED.to_owned())
}

async fn sent(proposals: &Proposals, grant: &Grant, args: ProposeArgs) -> Result<(), Fault> {
    let proposal = proposal_id(&grant.workspace, grant.run.as_str(), &args.change)?;
    let action = ProposalAction::Propose {
        proposal,
        agent: grant.agent,
        change: args.change,
    };
    proposals.command(&grant.workspace, action).await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use weacon_protocol::{AgentKind, ProposalChange};
    use weacon_testing::{messages, recording_logger};

    use super::*;
    use crate::proposals::fake::Fake;

    fn grant() -> Grant {
        let workspace = "w-1".parse().unwrap();
        Grant::new(
            workspace,
            "run-1".parse().unwrap(),
            AgentKind::Codex,
            PathBuf::from("/w"),
        )
    }

    fn args(title: String) -> ProposeArgs {
        ProposeArgs {
            change: ProposalChange::NewTask { title },
        }
    }

    #[tokio::test]
    async fn an_agents_proposal_reaches_the_server_as_its_runs_agent() {
        let fake = Fake::default();
        let proposals = fake.connect();
        let (log, records) = recording_logger();

        let answer = propose(Some(&proposals), &grant(), args("Ship".to_owned()), &log).await;

        assert_eq!(answer, Ok(PROPOSED.to_owned()));
        let [command] = fake.commands().try_into().unwrap();
        let change = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        let proposal = proposal_id(&grant().workspace, "run-1", &change).unwrap();
        assert_eq!(
            command.action,
            ProposalAction::Propose {
                proposal,
                agent: AgentKind::Codex,
                change,
            }
        );
        assert_eq!(command.workspace.as_str(), "w-1");
        assert!(records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refusal_or_no_server_says_why_and_a_large_change_never_leaves() {
        let fake = Fake::default();
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let proposals = fake.connect();
        let (log, records) = recording_logger();

        let refused = propose(Some(&proposals), &grant(), args("Ship".to_owned()), &log).await;
        let large = args("x".repeat(weacon_protocol::MAX_PROPOSAL));
        let too_large = propose(Some(&proposals), &grant(), large, &log).await;
        let alone = propose(None, &grant(), args("Ship".to_owned()), &log).await;

        assert_eq!(
            refused,
            Err("Not proposed: proposal.too_many.\n".to_owned())
        );
        assert_eq!(
            too_large,
            Err("Not proposed: proposal.too_large.\n".to_owned())
        );
        assert_eq!(alone, Err(NO_SERVER.to_owned()));
        assert_eq!(fake.commands().len(), 1);
        assert_eq!(messages(&records), ["mcp proposal refused"; 2]);
    }

    #[tokio::test]
    async fn a_proposal_the_server_never_answered_may_still_be_held() {
        let (log, _) = recording_logger();
        let (mut proposals, _asks) = crate::proposals::channel();
        proposals.set_patience(std::time::Duration::from_millis(10));

        let answer = propose(Some(&proposals), &grant(), args("Ship".to_owned()), &log).await;

        assert_eq!(answer, Err(UNANSWERED.to_owned()));
    }
}
