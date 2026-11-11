use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What a workspace's agents may do on one machine, chosen by the machine's owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    /// No agent runs.
    Off,
    ReadOnly,
    /// Agents ask a person before each change.
    AskFirst,
    /// Agents change things without asking; destructive operations still ask.
    Full,
    /// A level this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl AccessLevel {
    /// The level to act on: one this build doesn't know allows nothing.
    #[must_use]
    pub const fn effective(self) -> Self {
        match self {
            Self::Newer | Self::Off => Self::Off,
            Self::ReadOnly | Self::AskFirst | Self::Full => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn levels_travel_as_snake_case_names() {
        let levels = [
            (AccessLevel::Off, "off"),
            (AccessLevel::ReadOnly, "read_only"),
            (AccessLevel::AskFirst, "ask_first"),
            (AccessLevel::Full, "full"),
        ];
        for (level, name) in levels {
            assert_eq!(serde_json::to_value(level).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<AccessLevel>(json!(name)).unwrap(),
                level
            );
            assert_eq!(level.effective(), level);
        }
    }

    #[test]
    fn a_newer_level_parses_and_allows_nothing() {
        let newer: AccessLevel = serde_json::from_value(json!("sandboxed")).unwrap();

        assert_eq!(newer, AccessLevel::Newer);
        assert_eq!(newer.effective(), AccessLevel::Off);
    }
}
