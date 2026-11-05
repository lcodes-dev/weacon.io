use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Code;

/// A feature a peer supports, such as `chat.send`, named like a [`Code`]. A peer keeps the ones
/// it doesn't know, so a newer peer's list still parses.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Capability(Code);

impl Capability {
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travels_as_a_dotted_name_checked_on_arrival() {
        let known: Capability = serde_json::from_str(r#""chat.send""#).unwrap();
        let unknown: Capability = serde_json::from_str(r#""future.thing_v2""#).unwrap();

        assert_eq!(known.as_str(), "chat.send");
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            r#""future.thing_v2""#
        );
        serde_json::from_str::<Capability>(r#""Chat Send""#).unwrap_err();
    }
}
