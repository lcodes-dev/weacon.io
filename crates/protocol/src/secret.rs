use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// A value that never reaches a log line: it travels as `T`, and its `Debug` shows only that
/// it is there.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    #[must_use]
    pub const fn expose(&self) -> &T {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Secret<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret<String> {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

// Its schema is `T`'s, so the schema doesn't change when a field becomes secret.
impl<T: JsonSchema> JsonSchema for Secret<T> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        generator.subschema_for::<T>()
    }
}

impl<T> fmt::Debug for Secret<T> {
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
            serde_json::from_value::<Secret<String>>(json!("s3cret")).unwrap(),
            secret
        );
        assert_eq!(secret.expose(), "s3cret");
        assert_eq!(secret.into_inner(), "s3cret");
        assert_eq!(Secret::<String>::schema_name(), String::schema_name());
    }
}
