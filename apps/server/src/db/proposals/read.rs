use weacon_observability::Fault;
use weacon_protocol::{ProposalAbout, ProposalId, ProposalRecord, ProposalState};

use super::super::{Workspace, failed, parsed, stored, stored_author, stored_version, unreadable};
use super::{
    ACCEPTED, DOC_EDIT, NEW_DOC, NEW_TASK, OPEN, PAGE, REJECTED, SUPERSEDED, TASK_EDIT, WITHDRAWN,
    not_found,
};

type RecordRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    i64,
);

const COLUMNS: &str = "proposal_id, kind, doc_id, task_id, new_id, change, author_member, agent,
    state, decided_by, version, at";

impl Workspace {
    /// The workspace's newest proposals, newest first.
    pub(crate) async fn proposals(&self) -> Result<Vec<ProposalRecord>, Fault> {
        let sql = format!(
            "SELECT {COLUMNS} FROM proposals WHERE workspace_id = ?
             ORDER BY at DESC, rowid DESC LIMIT ?"
        );
        sqlx::query_as::<_, RecordRow>(sqlx::AssertSqlSafe(sql))
            .bind(self.id.as_str())
            .bind(PAGE)
            .fetch_all(&self.db.readers)
            .await
            .map_err(failed)?
            .into_iter()
            .map(record)
            .collect()
    }

    /// The proposal `id`; `proposal.not_found` when there's none.
    pub(crate) async fn proposal(&self, id: &ProposalId) -> Result<ProposalRecord, Fault> {
        let sql =
            format!("SELECT {COLUMNS} FROM proposals WHERE workspace_id = ? AND proposal_id = ?");
        let row = sqlx::query_as::<_, RecordRow>(sqlx::AssertSqlSafe(sql))
            .bind(self.id.as_str())
            .bind(id.as_str())
            .fetch_optional(&self.db.readers)
            .await
            .map_err(failed)?
            .ok_or_else(not_found)?;
        record(row)
    }
}

fn record(row: RecordRow) -> Result<ProposalRecord, Fault> {
    let (id, kind, doc, task, new_id, change, author, agent, state, by, version, at) = row;
    let about = match (kind.as_str(), doc, task, new_id) {
        (DOC_EDIT, Some(doc), None, None) => ProposalAbout::Doc { doc: stored(doc)? },
        (TASK_EDIT, None, Some(task), None) => ProposalAbout::Task {
            task: stored(task)?,
        },
        (NEW_DOC, None, None, Some(doc)) => ProposalAbout::NewDoc { doc: stored(doc)? },
        (NEW_TASK, None, None, Some(task)) => ProposalAbout::NewTask {
            task: stored(task)?,
        },
        _ => return Err(unreadable()),
    };
    let state = match (state.as_str(), by, version) {
        (OPEN, None, None) => ProposalState::Open,
        (ACCEPTED, Some(by), Some(version)) => ProposalState::Accepted {
            version: stored_version(version)?.get(),
            by: stored(by)?,
        },
        (REJECTED, Some(by), None) => ProposalState::Rejected { by: stored(by)? },
        (SUPERSEDED, None, None) => ProposalState::Superseded,
        (WITHDRAWN, None, None) => ProposalState::Withdrawn,
        _ => return Err(unreadable()),
    };
    Ok(ProposalRecord {
        id: stored(id)?,
        about,
        author: stored_author("agent", author, Some(&agent))?,
        state,
        at,
        change: change.as_deref().map(parsed).transpose()?,
    })
}
