use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::Code;

/// Something a core does only on some machines, such as `worktree.open_terminal`, named like a
/// [`Code`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Capability(Code);

impl Capability {
    #[must_use]
    pub const fn new(code: Code) -> Self {
        Self(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travels_as_a_dotted_name_checked_on_arrival() {
        let known: Capability = serde_json::from_str(r#""worktree.open_terminal""#).unwrap();

        assert_eq!(
            serde_json::to_string(&known).unwrap(),
            r#""worktree.open_terminal""#
        );
        assert_eq!(
            Capability::new(crate::code!("worktree.open_terminal")),
            known
        );
        serde_json::from_str::<Capability>(r#""Chat Send""#).unwrap_err();
    }
}
