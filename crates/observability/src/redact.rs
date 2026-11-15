use std::error::Error;

use serde_json::{Map, Value};

use crate::errors::{ErrorInfo, describe_error};

// Matched against a key's letters run together, so "apiKey" and "api_key" both match.
const SECRET_IN_KEY: [&str; 9] = [
    "password",
    "passwd",
    "passphrase",
    "passcode",
    "secret",
    "credential",
    "cookie",
    "authorisation",
    "authorization",
];
const SECRET_ENDS: [&str; 4] = ["key", "keys", "token", "tokens"];
const NOT_SECRET_KEY: [&str; 8] = [
    "publickey",
    "primarykey",
    "foreignkey",
    "cachekey",
    "sortkey",
    "idempotencykey",
    "partitionkey",
    "lookupkey",
];
const SECRET_WORDS: [&str; 15] = [
    "auth",
    "bearer",
    "creds",
    "csrf",
    "dsn",
    "jwt",
    "key",
    "otp",
    "pass",
    "pat",
    "pin",
    "pwd",
    "signature",
    "token",
    "xsrf",
];
const DESCRIBES_SECRET: [&str; 13] = [
    "count", "enabled", "expires", "expiry", "length", "method", "mode", "provider", "required",
    "size", "status", "ttl", "type",
];
const CONTENT_WORDS: [&str; 44] = [
    "args",
    "argument",
    "arguments",
    "argv",
    "body",
    "command",
    "completion",
    "content",
    "contents",
    "data",
    "delta",
    "description",
    "diff",
    "diffs",
    "fragment",
    "html",
    "input",
    "inputs",
    "markdown",
    "message",
    "messages",
    "msg",
    "notes",
    "output",
    "outputs",
    "patch",
    "payload",
    "prompt",
    "prompts",
    "query",
    "reasoning",
    "response",
    "result",
    "results",
    "snippet",
    "source",
    "stderr",
    "stdin",
    "stdout",
    "summary",
    "text",
    "thinking",
    "title",
    "transcript",
];
const DESCRIBES_CONTENT: [&str; 15] = [
    "bytes", "count", "format", "hash", "id", "ids", "index", "kind", "length", "lines", "size",
    "status", "tokens", "type", "version",
];

/// A log field's value: anything that converts into JSON, or an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Field(Kind);

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Value(Value),
    Error(ErrorInfo),
}

impl Field {
    /// An error without its message, unless it is a [`Fault`](crate::Fault).
    pub fn error<E: Error + 'static>(error: &E) -> Self {
        Self(Kind::Error(describe_error(error)))
    }
}

impl<T: Into<Value>> From<T> for Field {
    fn from(value: T) -> Self {
        Self(Kind::Value(value.into()))
    }
}

/// Copies log fields with those named for secrets or content replaced, at any depth.
pub(crate) fn redact_fields(fields: &[(&str, Field)]) -> Map<String, Value> {
    fields
        .iter()
        .map(|(key, Field(kind))| {
            let value = match kind {
                Kind::Value(value) => redact_entry(key, value),
                Kind::Error(info) => replacement(key).map_or_else(
                    || serde_json::to_value(info).unwrap_or(Value::Null),
                    Value::from,
                ),
            };
            ((*key).to_owned(), value)
        })
        .collect()
}

/// "[redacted]" or "[omitted]" when a field named `key` must not be kept.
fn replacement(key: &str) -> Option<&'static str> {
    let words = words(key);
    let joined = words.concat();
    let last = words.last().map_or("", String::as_str);
    let has = |list: &[&str]| words.iter().any(|word| list.contains(&word.as_str()));
    let named = SECRET_IN_KEY.iter().any(|word| joined.contains(word))
        || SECRET_ENDS.iter().any(|end| joined.ends_with(end));
    let secret = (named || has(&SECRET_WORDS))
        && !NOT_SECRET_KEY.iter().any(|word| joined.contains(word))
        && !DESCRIBES_SECRET.contains(&last);
    if secret {
        Some("[redacted]")
    } else if has(&CONTENT_WORDS) && !DESCRIBES_CONTENT.contains(&last) {
        Some("[omitted]")
    } else {
        None
    }
}

fn words(key: &str) -> Vec<String> {
    let mut spaced = String::with_capacity(key.len() + 4);
    let mut previous: Option<char> = None;
    for ch in key.chars() {
        if previous.is_some_and(|prev| prev.is_ascii_lowercase() || prev.is_ascii_digit())
            && ch.is_ascii_uppercase()
        {
            spaced.push(' ');
        }
        spaced.push(ch);
        previous = Some(ch);
    }
    spaced
        .to_ascii_lowercase()
        .split(|ch: char| !ch.is_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

// Numbers and flags are counts and switches, never secrets or content.
fn redact_entry(key: &str, value: &Value) -> Value {
    if value.is_number() || value.is_boolean() {
        return value.clone();
    }
    replacement(key).map_or_else(|| redact_value(value), Value::from)
}

fn redact_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => items.iter().map(redact_value).collect(),
        Value::Object(entries) => entries
            .iter()
            .map(|(key, value)| (key.clone(), redact_entry(key, value)))
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn redacted(fields: &[(&str, Field)]) -> Value {
        Value::Object(redact_fields(fields))
    }

    #[test]
    fn redacts_secrets_and_omits_user_content_at_any_depth_whatever_the_key_style() {
        let out = redacted(&[
            ("runId", "run_1".into()),
            ("apiKey", "MARKER".into()),
            ("x-api-key", "MARKER".into()),
            ("PGPASSWORD", "MARKER".into()),
            ("clientsecret", "MARKER".into()),
            ("sshKey", "MARKER".into()),
            ("apiToken2", "MARKER".into()),
            ("secrets", json!(["MARKER"]).into()),
            ("refreshTokens", json!(["MARKER"]).into()),
            ("encryptionKey", "MARKER".into()),
            ("headers", json!({ "Authorization": "MARKER", "set-cookie": "MARKER" }).into()),
            ("turns", json!([{ "systemPrompt": "MARKER", "accessToken": "MARKER", "count": 2 }]).into()),
            ("tool", json!({ "name": "edit", "file_content": "MARKER", "request-body": "MARKER", "command": "MARKER" }).into()),
            ("account", json!({ "password": "MARKER", "authHeader": "MARKER" }).into()),
            ("agent", json!({ "result": "MARKER", "thinking": "MARKER", "query": "MARKER", "stdin": "MARKER", "delta": "MARKER" }).into()),
            ("task", json!({ "title": "MARKER", "notes": "MARKER", "summary": "MARKER", "description": "MARKER" }).into()),
            ("messages", json!(["MARKER"]).into()),
            ("apiKeyValue", "MARKER".into()),
            ("api_key_header", "MARKER".into()),
            ("privateKeyPem", "MARKER".into()),
            ("GITHUB_PAT", "MARKER".into()),
            ("creds", "MARKER".into()),
            ("passcode", "MARKER".into()),
            ("otp", "MARKER".into()),
            ("csrf", "MARKER".into()),
            ("promptTemplate", "MARKER".into()),
            ("prompt_v2", "MARKER".into()),
            ("argv", json!(["MARKER"]).into()),
            ("event", json!({ "msg": "MARKER" }).into()),
        ]);

        assert!(!out.to_string().contains("MARKER"), "{out}");
        assert_eq!(out["runId"], "run_1");
        assert_eq!(out["PGPASSWORD"], "[redacted]");
        assert_eq!(out["secrets"], "[redacted]");
        assert_eq!(
            out["turns"],
            json!([{ "systemPrompt": "[omitted]", "accessToken": "[redacted]", "count": 2 }])
        );
        assert_eq!(out["messages"], "[omitted]");
    }

    #[test]
    fn keeps_counts_statuses_and_names_that_only_look_like_secrets_or_content() {
        let fields: Vec<(&str, Field)> = vec![
            ("tokens", 1200.into()),
            ("inputTokens", 800.into()),
            ("messages", 3.into()),
            ("hasPassword", true.into()),
            ("primaryKey", "id".into()),
            ("tokenCount", 3.into()),
            ("authStatus", "signed-in".into()),
            ("authMethod", "subscription".into()),
            ("contentLength", 52.into()),
            ("cacheKey", "chat_1".into()),
            ("subcommand", "status".into()),
            ("metadata", json!({ "kind": "note" }).into()),
            ("authorName", "Ada".into()),
            ("bypass", true.into()),
            ("sessionId", "session_1".into()),
            ("note", "Basic setup failed".into()),
        ];
        let Value::Object(expected) = json!({
            "tokens": 1200, "inputTokens": 800, "messages": 3, "hasPassword": true,
            "primaryKey": "id", "tokenCount": 3, "authStatus": "signed-in",
            "authMethod": "subscription", "contentLength": 52, "cacheKey": "chat_1",
            "subcommand": "status", "metadata": { "kind": "note" }, "authorName": "Ada",
            "bypass": true, "sessionId": "session_1", "note": "Basic setup failed",
        }) else {
            panic!("not an object")
        };

        assert_eq!(redact_fields(&fields), expected);
    }

    #[test]
    fn keeps_ids_and_counts_of_content() {
        let out = redacted(&[
            ("messageId", "msg_1".into()),
            ("contentType", "text/markdown".into()),
            ("promptStatus", "sent".into()),
            ("lookupKey", "chat_1".into()),
        ]);

        assert_eq!(
            out,
            json!({ "messageId": "msg_1", "contentType": "text/markdown", "promptStatus": "sent", "lookupKey": "chat_1" })
        );
    }

    #[test]
    fn keeps_numbers_and_errors_as_they_are() {
        let error = Field::error(&std::io::Error::other("MARKER"));
        let out = redacted(&[
            ("err", error),
            ("ratio", 0.5.into()),
            ("nothing", f64::NAN.into()),
        ]);

        assert!(!out.to_string().contains("MARKER"), "{out}");
        assert_eq!(out["err"]["name"], "std::io::error::Error");
        assert_eq!(out["ratio"], 0.5);
        assert_eq!(out["nothing"], Value::Null);
    }

    #[test]
    fn splits_keys_into_words_whatever_their_style() {
        assert_eq!(words("apiKey"), ["api", "key"]);
        assert_eq!(words("API_KEY"), ["api", "key"]);
        assert_eq!(words("x-api-key2Value"), ["x", "api", "key", "value"]);
        assert_eq!(words("ümlautKey"), ["mlaut", "key"]);
    }
}
