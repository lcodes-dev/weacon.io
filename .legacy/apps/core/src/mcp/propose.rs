//! The MCP's one write tool: a run's agent proposes a change to a doc or task of its own
//! workspace, which the server holds until a person accepts it.

use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{ProposalAction, ProposalChange, ProposeArgs, RepoId, code};

use super::grants::Grant;
use crate::proposals::{Proposals, proposal_id};

pub(crate) const PROPOSED: &str = "Proposed. It changes nothing until a person accepts it.\n";
const NO_SERVER: &str = "Not proposed: this machine isn't connected to a server.\n";
const UNANSWERED: &str =
    "The server didn't answer in time, so it may hold the proposal yet. Don't propose it again.\n";

/// What to answer the agent: an error when the proposal didn't reach the server. Pins that
/// don't say their repository are said to be of `repo`, the one the run's folder is in.
pub(crate) async fn propose(
    proposals: Option<&Proposals>,
    grant: &Grant,
    args: ProposeArgs,
    repo: Option<&RepoId>,
    log: &Logger,
) -> Result<String, String> {
    let proposals = proposals.ok_or_else(|| NO_SERVER.to_owned())?;
    let change = of_repo(args.change, repo);
    sent(proposals, grant, change).await.map_err(|fault| {
        log.warn("mcp proposal refused", &[("err", Field::error(&fault))]);
        if *fault.code() == code!("proposal.unanswered") {
            return UNANSWERED.to_owned();
        }
        format!("Not proposed: {}.\n", fault.code().as_str())
    })?;
    Ok(PROPOSED.to_owned())
}

async fn sent(proposals: &Proposals, grant: &Grant, change: ProposalChange) -> Result<(), Fault> {
    let proposal = proposal_id(&grant.workspace, grant.run.as_str(), &change)?;
    let action = ProposalAction::Propose {
        proposal,
        agent: grant.agent,
        change,
    };
    proposals.command(&grant.workspace, action).await
}

/// `change`, its doc's pins said to be of `repo` unless they say whose they are.
fn of_repo(mut change: ProposalChange, repo: Option<&RepoId>) -> ProposalChange {
    if let ProposalChange::DocEdit { content, .. } | ProposalChange::NewDoc { content } =
        &mut change
        && let Some(anchors) = &mut content.anchors
        && anchors.repo.is_none()
    {
        anchors.repo = repo.cloned();
    }
    change
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

        let answer = propose(
            Some(&proposals),
            &grant(),
            args("Ship".to_owned()),
            None,
            &log,
        )
        .await;

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
        *fake.refuse.lock().unwrap() = Some(code!("proposal.invalid"));
        let proposals = fake.connect();
        let (log, records) = recording_logger();

        let refused = propose(
            Some(&proposals),
            &grant(),
            args("Ship".to_owned()),
            None,
            &log,
        )
        .await;
        let large = args("x".repeat(weacon_protocol::MAX_PROPOSAL));
        let too_large = propose(Some(&proposals), &grant(), large, None, &log).await;
        let alone = propose(None, &grant(), args("Ship".to_owned()), None, &log).await;

        assert_eq!(refused, Err("Not proposed: proposal.invalid.\n".to_owned()));
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

        let answer = propose(
            Some(&proposals),
            &grant(),
            args("Ship".to_owned()),
            None,
            &log,
        )
        .await;

        assert_eq!(answer, Err(UNANSWERED.to_owned()));
    }
    #[test]
    fn a_docs_pins_are_of_the_runs_repository_unless_they_say_whose_they_are() {
        let site: RepoId = "site".parse().unwrap();
        let anchors = |repo: Option<&str>| weacon_protocol::DocAnchors {
            commit: "a".repeat(40).parse().unwrap(),
            pins: vec!["sym:a::go#0123456789abcdef".parse().unwrap()],
            repo: repo.map(|repo| repo.parse().unwrap()),
        };
        let content = |repo: Option<&str>| weacon_protocol::DocContent {
            title: "Plan".to_owned(),
            body: "[go](sym:a::go)\n".to_owned(),
            anchors: Some(anchors(repo)),
        };
        let edit = |repo| ProposalChange::DocEdit {
            doc: "d-1".parse().unwrap(),
            base: 1,
            content: content(repo),
        };
        let new = |repo| ProposalChange::NewDoc {
            content: content(repo),
        };
        let task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };

        assert_eq!(of_repo(edit(None), Some(&site)), edit(Some("site")));
        assert_eq!(of_repo(new(None), Some(&site)), new(Some("site")));
        assert_eq!(of_repo(new(Some("other")), Some(&site)), new(Some("other")));
        assert_eq!(of_repo(new(None), None), new(None));
        assert_eq!(of_repo(task.clone(), Some(&site)), task);
    }
}
