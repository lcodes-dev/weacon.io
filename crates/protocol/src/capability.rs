use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Code;

/// A feature a peer supports, such as `chat.send`, named like a [`Code`]. A peer keeps the ones
/// it doesn't know, so a newer peer's list still parses.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Capability(Code);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travels_as_a_dotted_name_checked_on_arrival() {
        let known: Capability = serde_json::from_str(r#""chat.send""#).unwrap();
        let unknown: Capability = serde_json::from_str(r#""future.thing_v2""#).unwrap();

        assert_eq!(serde_json::to_string(&known).unwrap(), r#""chat.send""#);
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            r#""future.thing_v2""#
        );
        serde_json::from_str::<Capability>(r#""Chat Send""#).unwrap_err();
    }
}
