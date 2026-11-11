use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::MemberId;
use crate::token::token_id;

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
    /// An agent, named by the member it answers to.
    Agent {
        owner: MemberId,
    },
    /// An author this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn member() -> MemberId {
        MemberId::try_from("m-1".to_owned()).unwrap()
    }

    #[test]
    fn task_ids_are_plain_tokens_checked_on_arrival() {
        let task: TaskId = serde_json::from_value(json!("t_1")).unwrap();

        assert_eq!(task.as_str(), "t_1");
        assert_eq!(TaskId::try_from("a b".to_owned()), Err("a b".to_owned()));
        let bad = serde_json::from_value::<TaskId>(json!("../x")).unwrap_err();
        assert!(bad.to_string().contains("not a task id"), "{bad}");
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
        let agent = Author::Agent { owner: member() };
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
}
