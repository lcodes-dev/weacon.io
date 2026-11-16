//! What was said in a chat since a member last prompted their agent there.

use futures_util::TryStreamExt as _;
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, ChatEvent, ChatId, MachineName, MemberId, RunEvent, RunUpdate};

use super::legs::stored_agent;
use crate::db::{Workspace, failed, stored, unreadable};

/// Something a person or an agent said in a chat.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Said {
    /// Who wrote it, or whose agent did.
    pub(crate) member: MemberId,
    /// The agent that wrote it and its machine's name, when one did.
    pub(crate) agent: Option<(AgentKind, MachineName)>,
    pub(crate) text: String,
}

type SaidRow = (String, Option<String>, Option<String>, Option<String>);

impl Workspace {
    /// What `chat`'s people and the agents of its other members said after `member`'s last
    /// prompt there, the newest first, until `enough` says so of what it was handed.
    pub(crate) async fn said_since(
        &self,
        chat: &ChatId,
        member: &MemberId,
        mut enough: impl FnMut(&Said) -> bool,
    ) -> Result<Vec<Said>, Fault> {
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        let mut rows = sqlx::query_as::<_, SaidRow>(
            "SELECT e.body, e.member_id, l.agent, l.machine_name FROM chat_events AS e
             LEFT JOIN chat_legs AS l ON l.workspace_id = e.workspace_id
                 AND l.chat_id = e.chat_id AND l.member_id = e.member_id
             WHERE e.workspace_id = ?1 AND e.chat_id = ?2 AND e.member_id IS NOT ?3
                 AND e.seq > (SELECT COALESCE(MAX(p.seq), 0) FROM chat_events AS p
                     WHERE p.workspace_id = ?1 AND p.chat_id = ?2 AND p.member_id = ?3
                         AND json_extract(p.body, '$.kind') = 'prompt')
             ORDER BY e.seq DESC",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(member.as_str())
        .fetch(&mut *reader);
        let mut said = Vec::new();
        while let Some(row) = rows.try_next().await.map_err(failed)? {
            let Some(one) = said_in(row)? else {
                continue;
            };
            let last = enough(&one);
            said.push(one);
            if last {
                break;
            }
        }
        Ok(said)
    }
}

// What the stored event `row` holds says, if it is a message: a person's, or an agent's text.
fn said_in((body, from, agent, machine): SaidRow) -> Result<Option<Said>, Fault> {
    let event = serde_json::from_str(&body).map_err(|_| unreadable())?;
    let from = from.map(stored::<MemberId>).transpose()?;
    Ok(match (event, from) {
        (ChatEvent::Message { author, text }, _) => Some(Said {
            member: author,
            agent: None,
            text,
        }),
        (ChatEvent::Prompt { text, .. }, Some(member)) => Some(Said {
            member,
            agent: None,
            text,
        }),
        (
            ChatEvent::Run {
                update:
                    RunUpdate {
                        event: RunEvent::Text { text },
                        ..
                    },
            },
            Some(member),
        ) => {
            let agent = stored_agent(&agent.ok_or_else(unreadable)?)?;
            let machine = stored(machine.ok_or_else(unreadable)?)?;
            Some(Said {
                member,
                agent: Some((agent, machine)),
                text,
            })
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::Incoming;
    use crate::db::test_support::{NOW, append, id, make, said};
    use crate::test_support::{member_id, two_teams};

    fn message(author: &str, text: &str) -> ChatEvent {
        ChatEvent::Message {
            author: member_id(author),
            text: text.to_owned(),
        }
    }

    fn prompt(text: &str) -> ChatEvent {
        ChatEvent::Prompt {
            request: id("r-1"),
            text: text.to_owned(),
            author: None,
            repo: None,
            agent: None,
            task: None,
        }
    }

    async fn says(workspace: &Workspace, event: ChatEvent) {
        let incoming = Incoming {
            core_seq: None,
            event: &event,
            at: NOW,
        };
        let (chat, events) = (id("c-1"), [incoming]);
        workspace.append_events(&chat, &events).await.unwrap();
    }

    fn texts(said: &[Said]) -> Vec<&str> {
        said.iter().map(|one| one.text.as_str()).collect()
    }

    #[tokio::test]
    async fn what_was_said_since_a_members_last_prompt_is_read_newest_first_in_its_workspace_only()
    {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        make(&one, "c-1", "m1", NOW).await.unwrap();
        make(&two, "c-1", "m2", NOW).await.unwrap();
        let (chat, ada, bo) = (id::<ChatId>("c-1"), member_id("m1"), member_id("m3"));
        says(&one, message("m3", "Before")).await;
        let turn = [(Some(1), prompt("Fix it")), (Some(2), said("Fixed."))];
        append(&one, "c-1", &turn).await.unwrap();
        says(&one, message("m3", "Thanks")).await;
        says(&one, message("m1", "Sure")).await;

        let for_ada = one.said_since(&chat, &ada, |_| false).await.unwrap();
        let for_bo = one.said_since(&chat, &bo, |_| false).await.unwrap();
        let newest = one.said_since(&chat, &bo, |_| true).await.unwrap();
        let elsewhere = two.said_since(&chat, &member_id("m2"), |_| false).await;

        // Her agent has her prompt and its own answer; her own message it hasn't.
        assert_eq!(texts(&for_ada), ["Sure", "Thanks"]);
        assert_eq!(
            texts(&for_bo),
            ["Sure", "Thanks", "Fixed.", "Fix it", "Before"]
        );
        let by_agent = for_bo.iter().find(|one| one.text == "Fixed.").unwrap();
        let agent = (AgentKind::Claude, id::<MachineName>("Ada's laptop"));
        assert_eq!((&by_agent.member, &by_agent.agent), (&ada, &Some(agent)));
        let asked = for_bo.iter().find(|one| one.text == "Fix it").unwrap();
        assert_eq!((&asked.member, &asked.agent), (&ada, &None));
        assert_eq!(texts(&newest), ["Sure"]);
        assert!(elsewhere.unwrap().is_empty());
    }
}
