use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ChatId, RequestId, WorkspaceId};

/// Each answer is a [`DiffAnswer`] that echoes `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffQuery {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub query: DiffQueryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffQueryKind {
    /// Every file the chat's agents changed in its worktree.
    Changes,
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffAnswer {
    pub request: RequestId,
    pub workspace: WorkspaceId,
    pub chat: ChatId,
    pub answer: DiffAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffAnswerKind {
    /// One of the `files` changed files, which arrive one answer each, in order; `files` is at
    /// least 1 and the same in every answer to one query.
    File { files: u32, file: DiffFile },
    /// Nothing changed.
    Unchanged,
    /// An answer this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffFile {
    /// Where the file is now, or was when it was removed, relative to the worktree.
    pub path: String,
    pub change: FileChange,
    pub content: DiffContent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileChange {
    Added,
    Removed,
    Modified,
    Renamed {
        from: String,
    },
    /// A change this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiffContent {
    /// No hunks when only the file's mode changed.
    Text {
        hunks: Vec<Hunk>,
    },
    Binary,
    /// The file's changes wouldn't fit one frame.
    TooLarge,
    /// Content this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// A run of changed lines and their context. Lines count from 1; the start of a side with no
/// lines, as the old side of an added file, is 0.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

/// One line without its `\n`, so a `\r` before it stays in `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiffLine {
    pub op: LineOp,
    pub text: String,
    /// The file's last line, when it has no `\n`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_newline: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LineOp {
    Context,
    Added,
    Removed,
    /// An operation this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl DiffAnswerKind {
    pub(crate) fn is_newer(&self) -> bool {
        match self {
            Self::File { file, .. } => file.is_newer(),
            Self::Unchanged => false,
            Self::Newer => true,
        }
    }
}

impl DiffFile {
    fn is_newer(&self) -> bool {
        matches!(self.change, FileChange::Newer) || self.content.is_newer()
    }
}

impl DiffContent {
    fn is_newer(&self) -> bool {
        match self {
            Self::Text { hunks } => hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .any(|line| line.op == LineOp::Newer),
            Self::Binary | Self::TooLarge => false,
            Self::Newer => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::{Envelope, FrameError, Message};

    fn envelope(message: Message) -> Envelope {
        Envelope { id: 4, message }
    }

    fn request() -> RequestId {
        RequestId::try_from("r-1".to_owned()).unwrap()
    }

    fn workspace() -> WorkspaceId {
        WorkspaceId::try_from("w-1".to_owned()).unwrap()
    }

    fn chat() -> ChatId {
        ChatId::try_from("c-1".to_owned()).unwrap()
    }

    fn query(query: DiffQueryKind) -> Envelope {
        envelope(Message::DiffQuery(DiffQuery {
            request: request(),
            workspace: workspace(),
            chat: chat(),
            query,
        }))
    }

    fn answer(answer: DiffAnswerKind) -> Envelope {
        envelope(Message::DiffAnswer(DiffAnswer {
            request: request(),
            workspace: workspace(),
            chat: chat(),
            answer,
        }))
    }

    fn wire(message: &str, field: &str, body: Value) -> Value {
        let mut wire = json!({ "id": 4, "type": message, "request": "r-1",
                               "workspace": "w-1", "chat": "c-1" });
        wire[field] = body;
        wire
    }

    fn file_wire(change: &Value, content: &Value) -> Value {
        wire(
            "diff_answer",
            "answer",
            json!({ "kind": "file", "files": 2,
                    "file": { "path": "src/b.rs", "change": change, "content": content } }),
        )
    }

    fn travels(envelope: &Envelope, wire: Value) {
        assert_eq!(serde_json::to_value(envelope).unwrap(), wire);
        assert_eq!(
            Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
            *envelope
        );
        assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), *envelope);
    }

    fn file(change: FileChange, content: DiffContent) -> DiffAnswerKind {
        DiffAnswerKind::File {
            files: 2,
            file: DiffFile {
                path: "src/b.rs".to_owned(),
                change,
                content,
            },
        }
    }

    #[test]
    fn queries_have_a_tagged_shape() {
        travels(
            &query(DiffQueryKind::Changes),
            wire("diff_query", "query", json!({ "kind": "changes" })),
        );
    }

    #[test]
    fn a_text_answer_carries_its_hunks_and_lines() {
        let line = |op, text: &str| DiffLine {
            op,
            text: text.to_owned(),
            no_newline: false,
        };
        let hunks = vec![Hunk {
            old_start: 3,
            new_start: 4,
            lines: vec![
                line(LineOp::Context, "fn main() {"),
                line(LineOp::Removed, "    old();"),
                line(LineOp::Added, "    new();"),
                DiffLine {
                    no_newline: true,
                    ..line(LineOp::Added, "}")
                },
            ],
        }];
        travels(
            &answer(file(FileChange::Modified, DiffContent::Text { hunks })),
            file_wire(
                &json!({ "kind": "modified" }),
                &json!({ "kind": "text", "hunks": [{ "old_start": 3, "new_start": 4, "lines": [
                    { "op": "context", "text": "fn main() {" },
                    { "op": "removed", "text": "    old();" },
                    { "op": "added", "text": "    new();" },
                    { "op": "added", "text": "}", "no_newline": true },
                ] }] }),
            ),
        );
    }

    #[test]
    fn answers_tag_each_kind_of_change_and_content() {
        let cases = [
            (FileChange::Added, json!({ "kind": "added" })),
            (FileChange::Removed, json!({ "kind": "removed" })),
            (
                FileChange::Renamed {
                    from: "src/a.rs".to_owned(),
                },
                json!({ "kind": "renamed", "from": "src/a.rs" }),
            ),
        ];
        for (change, change_wire) in cases {
            travels(
                &answer(file(change, DiffContent::Binary)),
                file_wire(&change_wire, &json!({ "kind": "binary" })),
            );
        }
        travels(
            &answer(file(FileChange::Modified, DiffContent::TooLarge)),
            file_wire(
                &json!({ "kind": "modified" }),
                &json!({ "kind": "too_large" }),
            ),
        );
        travels(
            &answer(DiffAnswerKind::Unchanged),
            wire("diff_answer", "answer", json!({ "kind": "unchanged" })),
        );
    }

    #[test]
    fn a_newer_peers_queries_and_answers_parse_and_are_never_sent() {
        let text = |op: &str| {
            json!({ "kind": "text", "hunks": [{ "old_start": 1, "new_start": 1,
                                       "lines": [{ "op": op, "text": "x" }] }] })
        };
        let modified = json!({ "kind": "modified" });
        let newer = [
            wire("diff_query", "query", json!({ "kind": "since" })),
            wire("diff_answer", "answer", json!({ "kind": "summary" })),
            file_wire(&json!({ "kind": "copied" }), &json!({ "kind": "binary" })),
            file_wire(&modified, &json!({ "kind": "symlink" })),
            file_wire(&modified, &text("moved")),
        ];
        for wire in newer {
            let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{wire}");
        }

        let known: Envelope = serde_json::from_value(file_wire(&modified, &text("added"))).unwrap();
        known.to_json().unwrap();
    }
}
