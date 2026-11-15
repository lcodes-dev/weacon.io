use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// A string that never reaches a log line: it travels as itself, and its `Debug` shows only
// that it is there; a doc comment would land in the schema of every field that holds one.
#[derive(PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_secret_travels_as_its_value_and_never_shows_it() {
        let secret = Secret::from("s3cret");

        assert_eq!(format!("{secret:?}"), "Secret(..)");
        assert_eq!(serde_json::to_value(&secret).unwrap(), json!("s3cret"));
        assert_eq!(
            serde_json::from_value::<Secret>(json!("s3cret")).unwrap(),
            secret
        );
        assert_eq!(secret.expose(), "s3cret");
        assert_eq!(Secret::from("s3cret".to_owned()).into_inner(), "s3cret");
    }
}
