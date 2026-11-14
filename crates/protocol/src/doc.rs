use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, de};

use crate::token::token_id;
use crate::{Author, MemberId};

token_id!(
    /// A doc in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
    DocId,
    "not a doc id"
);

const PIN_MAX: usize = 512;

// The schema's forms of `is_pin` and `is_commit`; tests keep each pair in step.
const PIN_PATTERN: &str = r"^(sym|flow|area):[!-~]+#[0-9a-f]{16}$";
const COMMIT_PATTERN: &str = r"^([0-9a-f]{40}|[0-9a-f]{64})$";

/// An anchor a doc links to, with the fingerprint the code had at the revision's commit:
/// `sym:weacon_protocol::frame::encode_line#0123456789abcdef`, in printable ASCII. The core
/// computes both; the server only checks their shape.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct DocPin(#[schemars(regex(pattern = PIN_PATTERN), length(max = PIN_MAX))] String);

impl DocPin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The pin without its fingerprint, as a doc links to it.
    #[must_use]
    pub fn anchor(&self) -> &str {
        self.0
            .rsplit_once('#')
            .map_or(&self.0, |(anchor, _)| anchor)
    }
}

/// Whether a link's target is a code anchor: `sym:`, `flow:` or `area:`, then anything.
#[must_use]
pub fn links_to_code(target: &str) -> bool {
    target
        .split_once(':')
        .is_some_and(|(kind, _)| matches!(kind, "sym" | "flow" | "area"))
}

impl TryFrom<String> for DocPin {
    type Error = String;

    fn try_from(pin: String) -> Result<Self, Self::Error> {
        if is_pin(&pin) {
            Ok(Self(pin))
        } else {
            Err(pin)
        }
    }
}

impl<'de> Deserialize<'de> for DocPin {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not an anchor pin"))
    }
}

fn is_pin(pin: &str) -> bool {
    let Some((anchor, fingerprint)) = pin.rsplit_once('#') else {
        return false;
    };
    let Some((_, name)) = anchor.split_once(':') else {
        return false;
    };
    pin.len() <= PIN_MAX
        && links_to_code(anchor)
        && !name.is_empty()
        && name.bytes().all(|byte| byte.is_ascii_graphic())
        && fingerprint.len() == 16
        && is_lower_hex(fingerprint)
}

/// A git commit, as 40 or 64 lowercase hex digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub struct CommitId(#[schemars(regex(pattern = COMMIT_PATTERN))] String);

impl CommitId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CommitId {
    type Error = String;

    fn try_from(commit: String) -> Result<Self, Self::Error> {
        if is_commit(&commit) {
            Ok(Self(commit))
        } else {
            Err(commit)
        }
    }
}

impl<'de> Deserialize<'de> for CommitId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(String::deserialize(deserializer)?)
            .map_err(|_| de::Error::custom("not a commit id"))
    }
}

fn is_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && is_lower_hex(commit)
}

fn is_lower_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

/// What one revision of a doc says: its title, its Markdown, and the anchors it links to,
/// unless it links to none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocContent {
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "DocAnchors")]
    pub anchors: Option<DocAnchors>,
}

/// A pin for each anchor a doc links to, all computed at `commit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocAnchors {
    pub commit: CommitId,
    pub pins: Vec<DocPin>,
}

/// A doc at one revision; `version` rises with each revision. The server sends it to clients
/// that have the capability `workspace.docs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocRecord {
    pub id: DocId,
    pub version: u64,
    pub content: DocContent,
}

/// A doc in a [`DocList`], without its body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocSummary {
    pub id: DocId,
    pub title: String,
    pub version: u64,
}

/// A workspace's docs, from the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocList {
    pub docs: Vec<DocSummary>,
}

/// Asks the server for a new revision of a doc that the client read at `version`; the server
/// refuses when the doc has changed since. The answer is its [`DocRecord`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocRevise {
    pub version: u64,
    pub content: DocContent,
}

/// Who made one revision of a doc, who accepted it, and when, in seconds since the Unix
/// epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocRevision {
    pub version: u64,
    pub author: Author,
    pub accepted_by: MemberId,
    pub at: i64,
}

/// A doc's revisions, newest first, from the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocHistory {
    pub revisions: Vec<DocRevision>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const PIN: &str = "sym:weacon_protocol::frame::encode_line#0123456789abcdef";
    const COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn pin(text: &str) -> DocPin {
        DocPin::try_from(text.to_owned()).unwrap()
    }

    #[test]
    fn pins_name_an_anchor_and_a_fingerprint_and_match_the_schema() {
        let pattern = regex::Regex::new(PIN_PATTERN).unwrap();
        let pin_of = |name_len| format!("area:{}#0123456789abcdef", "a".repeat(name_len));
        let longest = pin_of(PIN_MAX - 22);
        for good in [
            PIN,
            "flow:a::b->c::d#ffffffffffffffff",
            "area:crates/protocol/src/lib#0000000000000000",
            "area:a#b#0000000000000000",
            longest.as_str(),
        ] {
            assert!(is_pin(good), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        let too_long = pin_of(PIN_MAX - 21);
        for bad in [
            "",
            "sym:a",
            "sym:#0123456789abcdef",
            "sym#0123456789abcdef",
            "file:a#0123456789abcdef",
            "sym:a b#0123456789abcdef",
            "sym:a\u{1b}#0123456789abcdef",
            "sym:a\u{feff}#0123456789abcdef",
            "area:é#0123456789abcdef",
            "sym:a#0123456789ABCDEF",
            "sym:a#0123456789abcde",
            "sym:a#0123456789abcdef0",
            "sym:a#0123456789abcdeg",
            "Sym:a#0123456789abcdef",
            too_long.as_str(),
        ] {
            assert!(!is_pin(bad), "{bad}");
            assert!(bad.len() > PIN_MAX || !pattern.is_match(bad), "{bad}");
        }
    }

    #[test]
    fn a_pin_travels_as_a_string_checked_on_arrival_and_names_its_anchor() {
        assert_eq!(serde_json::to_value(pin(PIN)).unwrap(), json!(PIN));
        assert_eq!(
            serde_json::from_value::<DocPin>(json!(PIN)).unwrap(),
            pin(PIN)
        );
        assert_eq!(pin(PIN).as_str(), PIN);
        assert_eq!(pin(PIN).anchor(), "sym:weacon_protocol::frame::encode_line");
        assert_eq!(pin("area:a#b#0000000000000000").anchor(), "area:a#b");
        let bad = serde_json::from_value::<DocPin>(json!("sym:a")).unwrap_err();
        assert!(bad.to_string().contains("not an anchor pin"), "{bad}");
    }

    #[test]
    fn code_links_are_named_by_their_kind() {
        for code in ["sym:a", "flow:a->b", "area:x", "sym:"] {
            assert!(links_to_code(code), "{code}");
        }
        for other in [
            "https://example.com",
            "#section",
            "symbol:a",
            "sym",
            "",
            "area",
        ] {
            assert!(!links_to_code(other), "{other}");
        }
    }

    #[test]
    fn commits_are_sha1_or_sha256_in_lowercase_hex_and_match_the_schema() {
        let pattern = regex::Regex::new(COMMIT_PATTERN).unwrap();
        let sha256 = "0".repeat(64);
        for good in [COMMIT, sha256.as_str()] {
            assert!(CommitId::try_from(good.to_owned()).is_ok(), "{good}");
            assert!(pattern.is_match(good), "{good}");
        }
        let short = "a".repeat(39);
        let between = "a".repeat(41);
        let long = "a".repeat(65);
        let upper = COMMIT.to_uppercase();
        for bad in [
            "",
            &short,
            &between,
            &long,
            &upper,
            &COMMIT.replace('a', "g"),
        ] {
            assert_eq!(CommitId::try_from(bad.to_owned()), Err(bad.to_owned()));
            assert!(!pattern.is_match(bad), "{bad}");
        }
        let commit: CommitId = serde_json::from_value(json!(COMMIT)).unwrap();
        assert_eq!(commit.as_str(), COMMIT);
        assert_eq!(serde_json::to_value(&commit).unwrap(), json!(COMMIT));
        let bad = serde_json::from_value::<CommitId>(json!("HEAD")).unwrap_err();
        assert!(bad.to_string().contains("not a commit id"), "{bad}");
    }

    fn content() -> DocContent {
        DocContent {
            title: "Framing".to_owned(),
            body: "See [encode](sym:weacon_protocol::frame::encode_line).".to_owned(),
            anchors: Some(DocAnchors {
                commit: CommitId::try_from(COMMIT.to_owned()).unwrap(),
                pins: vec![pin(PIN)],
            }),
        }
    }

    fn content_wire() -> serde_json::Value {
        json!({
            "title": "Framing",
            "body": "See [encode](sym:weacon_protocol::frame::encode_line).",
            "anchors": { "commit": COMMIT, "pins": [PIN] },
        })
    }

    #[test]
    fn content_without_anchors_leaves_them_out_and_pins_need_a_commit() {
        let bare = DocContent {
            anchors: None,
            ..content()
        };
        let wire = json!({ "title": "Framing", "body": content().body });

        assert_eq!(serde_json::to_value(content()).unwrap(), content_wire());
        assert_eq!(
            serde_json::from_value::<DocContent>(content_wire()).unwrap(),
            content()
        );
        assert_eq!(serde_json::to_value(&bare).unwrap(), wire);
        assert_eq!(serde_json::from_value::<DocContent>(wire).unwrap(), bare);
        serde_json::from_value::<DocContent>(json!({ "title": "Framing" })).unwrap_err();
        let uncommitted = json!({ "title": "Framing", "body": "", "anchors": { "pins": [PIN] } });
        serde_json::from_value::<DocContent>(uncommitted).unwrap_err();
    }

    #[test]
    fn records_lists_and_revisions_travel_as_plain_json() {
        let record = DocRecord {
            id: "d-1".parse().unwrap(),
            version: 2,
            content: content(),
        };
        let record_wire = json!({ "id": "d-1", "version": 2, "content": content_wire() });
        let list = DocList {
            docs: vec![DocSummary {
                id: record.id.clone(),
                title: "Framing".to_owned(),
                version: 2,
            }],
        };
        let list_wire = json!({ "docs": [{ "id": "d-1", "title": "Framing", "version": 2 }] });
        let revise = DocRevise {
            version: 2,
            content: content(),
        };
        let revise_wire = json!({ "version": 2, "content": content_wire() });

        assert_eq!(serde_json::to_value(&record).unwrap(), record_wire);
        assert_eq!(
            serde_json::from_value::<DocRecord>(record_wire).unwrap(),
            record
        );
        assert_eq!(serde_json::to_value(&list).unwrap(), list_wire);
        assert_eq!(serde_json::from_value::<DocList>(list_wire).unwrap(), list);
        assert_eq!(serde_json::to_value(&revise).unwrap(), revise_wire);
        assert_eq!(
            serde_json::from_value::<DocRevise>(revise_wire).unwrap(),
            revise
        );
        serde_json::from_value::<DocRevise>(json!({ "content": content_wire() })).unwrap_err();
        serde_json::from_value::<DocRecord>(
            json!({ "id": "a b", "version": 1, "content": content_wire() }),
        )
        .unwrap_err();
    }

    #[test]
    fn a_history_lists_who_made_and_accepted_each_revision() {
        let member: MemberId = "m-1".parse().unwrap();
        let history = DocHistory {
            revisions: vec![DocRevision {
                version: 1,
                author: Author::Agent {
                    owner: member.clone(),
                    agent: None,
                },
                accepted_by: member,
                at: 1_700_000_000,
            }],
        };
        let wire = json!({ "revisions": [{
            "version": 1,
            "author": { "kind": "agent", "owner": "m-1" },
            "accepted_by": "m-1",
            "at": 1_700_000_000,
        }] });

        assert_eq!(serde_json::to_value(&history).unwrap(), wire);
        assert_eq!(serde_json::from_value::<DocHistory>(wire).unwrap(), history);
    }
}
