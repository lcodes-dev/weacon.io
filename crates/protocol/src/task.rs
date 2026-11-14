use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::token::token_id;
use crate::{AgentRef, MemberId};

token_id!(
    /// A task in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
    TaskId,
    "not a task id"
);

/// How far along a task is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
    /// A status this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// Who made a change, such as one to a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    Person {
        member: MemberId,
    },
    /// An agent, named by the member it answers to, and by which agent it is when known.
    Agent {
        owner: MemberId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "AgentRef")]
        agent: Option<AgentRef>,
    },
    /// An author this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A task as it travels: `order` is its order key and `version` rises with each change. The
/// server sends it to clients that have the capability `workspace.tasks`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskRecord {
    pub id: TaskId,
    pub title: String,
    pub status: TaskStatus,
    pub order: String,
    pub version: u64,
}

/// A workspace's tasks in their order, from the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskList {
    pub tasks: Vec<TaskRecord>,
}

/// Asks the server for a new task, placed in front of `before`, or last without one. The
/// answer is its [`TaskRecord`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskCreate {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "TaskId")]
    pub before: Option<TaskId>,
}

/// Asks the server to change a task that the client read at `version`; the server refuses
/// when the task has changed since. The answer is its [`TaskRecord`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TaskChange {
    pub version: u64,
    pub edit: TaskEditBody,
}

/// One edit to a task, inside a [`TaskChange`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskEditBody {
    Title {
        title: String,
    },
    Status {
        status: TaskStatus,
    },
    /// Places the task in front of `before`, or last without one.
    Move {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "TaskId")]
        before: Option<TaskId>,
    },
    /// An edit this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{AgentKind, AgentOwner, MachineName};

    fn member() -> MemberId {
        "m-1".parse().unwrap()
    }

    #[test]
    fn statuses_are_snake_case_and_a_newer_one_still_parses() {
        for (status, wire) in [
            (TaskStatus::Todo, "todo"),
            (TaskStatus::InProgress, "in_progress"),
            (TaskStatus::Done, "done"),
        ] {
            assert_eq!(serde_json::to_value(status).unwrap(), json!(wire));
            assert_eq!(
                serde_json::from_value::<TaskStatus>(json!(wire)).unwrap(),
                status
            );
        }
        assert_eq!(
            serde_json::from_value::<TaskStatus>(json!("blocked")).unwrap(),
            TaskStatus::Newer
        );
    }

    #[test]
    fn authors_are_tagged_and_a_newer_one_still_parses() {
        let person = Author::Person { member: member() };
        let agent = Author::Agent {
            owner: member(),
            agent: None,
        };
        let person_wire = json!({ "kind": "person", "member": "m-1" });
        let agent_wire = json!({ "kind": "agent", "owner": "m-1" });

        assert_eq!(serde_json::to_value(&person).unwrap(), person_wire);
        assert_eq!(serde_json::to_value(&agent).unwrap(), agent_wire);
        assert_eq!(
            serde_json::from_value::<Author>(person_wire).unwrap(),
            person
        );
        assert_eq!(serde_json::from_value::<Author>(agent_wire).unwrap(), agent);
        assert_eq!(
            serde_json::from_value::<Author>(json!({ "kind": "robot", "x": 1 })).unwrap(),
            Author::Newer
        );
        serde_json::from_value::<Author>(json!({ "kind": "person" })).unwrap_err();
    }

    #[test]
    fn an_agent_author_names_which_agent_when_known() {
        let named = Author::Agent {
            owner: member(),
            agent: Some(AgentRef {
                kind: AgentKind::Codex,
                owner: AgentOwner::Member { member: member() },
                machine: Some(MachineName::try_from("Ada's laptop".to_owned()).unwrap()),
            }),
        };
        let wire = json!({ "kind": "agent", "owner": "m-1", "agent": { "kind": "codex",
            "owner": { "kind": "member", "member": "m-1" }, "machine": "Ada's laptop" } });

        assert_eq!(serde_json::to_value(&named).unwrap(), wire);
        assert_eq!(serde_json::from_value::<Author>(wire).unwrap(), named);
    }

    fn record() -> TaskRecord {
        TaskRecord {
            id: "t-1".parse().unwrap(),
            title: "Write the docs".to_owned(),
            status: TaskStatus::InProgress,
            order: "V".to_owned(),
            version: 3,
        }
    }

    #[test]
    fn a_task_and_a_list_travel_as_plain_json() {
        let wire = json!({
            "id": "t-1", "title": "Write the docs", "status": "in_progress", "order": "V",
            "version": 3,
        });
        let list = TaskList {
            tasks: vec![record()],
        };
        let list_wire = json!({ "tasks": [wire] });

        assert_eq!(serde_json::to_value(record()).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<TaskRecord>(wire).unwrap(),
            record()
        );
        let newer = json!({
            "id": "t-1", "title": "Write the docs", "status": "in_progress", "order": "V",
            "version": 3, "due": "soon",
        });
        assert_eq!(
            serde_json::from_value::<TaskRecord>(newer).unwrap(),
            record()
        );
        assert_eq!(serde_json::to_value(&list).unwrap(), list_wire);
        assert_eq!(serde_json::from_value::<TaskList>(list_wire).unwrap(), list);
    }

    #[test]
    fn a_new_task_names_the_one_it_goes_in_front_of_or_none() {
        let before: TaskId = "t-2".parse().unwrap();
        let create = |before| TaskCreate {
            title: "Ship".to_owned(),
            before,
        };
        let last = json!({ "title": "Ship" });
        let placed = json!({ "title": "Ship", "before": "t-2" });

        assert_eq!(serde_json::to_value(create(None)).unwrap(), last);
        assert_eq!(
            serde_json::from_value::<TaskCreate>(last).unwrap(),
            create(None)
        );
        assert_eq!(
            serde_json::to_value(create(Some(before.clone()))).unwrap(),
            placed
        );
        assert_eq!(
            serde_json::from_value::<TaskCreate>(placed).unwrap(),
            create(Some(before))
        );
        serde_json::from_value::<TaskCreate>(json!({ "title": "Ship", "before": "a b" }))
            .unwrap_err();
    }

    #[test]
    fn a_change_names_the_version_read_and_one_tagged_edit() {
        let before: TaskId = "t-2".parse().unwrap();
        for (edit, wire) in [
            (
                TaskEditBody::Title {
                    title: "Ship".to_owned(),
                },
                json!({ "kind": "title", "title": "Ship" }),
            ),
            (
                TaskEditBody::Status {
                    status: TaskStatus::Done,
                },
                json!({ "kind": "status", "status": "done" }),
            ),
            (
                TaskEditBody::Move {
                    before: Some(before),
                },
                json!({ "kind": "move", "before": "t-2" }),
            ),
            (
                TaskEditBody::Move { before: None },
                json!({ "kind": "move" }),
            ),
        ] {
            let change = TaskChange { version: 3, edit };
            let wire = json!({ "version": 3, "edit": wire });

            assert_eq!(serde_json::to_value(&change).unwrap(), wire);
            assert_eq!(serde_json::from_value::<TaskChange>(wire).unwrap(), change);
        }
        assert_eq!(
            serde_json::from_value::<TaskEditBody>(json!({ "kind": "archive" })).unwrap(),
            TaskEditBody::Newer
        );
        serde_json::from_value::<TaskChange>(json!({ "edit": { "kind": "move" } })).unwrap_err();
    }
}
