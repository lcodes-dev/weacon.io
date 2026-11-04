use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::{Map, Value};

use crate::errors::{AsError, ErrorInfo, describe_error};

// A key is a secret when its letters, run together, contain one of these or end in a key or a
// token, or when one of its words is a short secret word; unless its last word only describes
// the secret, or it names a key that isn't one.
static SECRET_IN_KEY: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"password|passwd|passphrase|passcode|secret|credential|cookie|authori[sz]ation|(?:key|token)s?$",
    )
});
static NOT_SECRET_KEY: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?:public|primary|foreign|cache|sort|idempotency|partition|lookup)keys?")
});
// In free text: "password=…", "Authorization: …", "\"token\":\"…\"" and the like.
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"(?i)([a-z0-9_.-]*(?:pass(?:word|wd|phrase|code)?|pwd|secret|token|api[_-]?key|private[_-]?key|access[_-]?key|credential|authori[sz]ation|cookie)[a-z0-9_.-]*"?\s*[=:]\s*)(?:(?:bearer|basic|token)\s+[^\s"']+|"[^"]*"|'[^']*'|[^\s;&,"']+)"#,
    )
});
static CREDENTIAL_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9_.~+/=-]{8,}"));
static USERINFO_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)\b([a-z][a-z0-9+.-]*://)[^\s/@]+@"));
static QUERY_IN_PATH: LazyLock<Regex> = LazyLock::new(|| pattern(r"(^|\s)(/[^\s?#]*)[?#]\S*"));
static SCHEME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^[a-z][a-z0-9+.-]*://"));
static URL_IN_TEXT: LazyLock<Regex> =
    LazyLock::new(|| pattern(r#"(?i)[a-z][a-z0-9+.-]*://[^\s"'<>]+"#));

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
const MAX_STRING: usize = 1000;
const MAX_KEY: usize = 100;
const MAX_ITEMS: usize = 100;
const MAX_DEPTH: usize = 8;

/// A log field's value: anything that converts into JSON, or an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Field(Kind);

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Value(Value),
    Error(ErrorInfo),
}

impl Field {
    /// Describes an error by its type, code, I/O error kind and causes. Only a
    /// [`Fault`](crate::Fault) keeps its message.
    pub fn error<E: AsError + ?Sized>(error: &E) -> Self {
        Self(Kind::Error(describe_error(error)))
    }
}

impl<T: Into<Value>> From<T> for Field {
    fn from(value: T) -> Self {
        Self(Kind::Value(value.into()))
    }
}

/// Copies log fields: fields named like secrets become "[redacted]", fields named like user
/// content become "[omitted]", credentials, queries and fragments are cut from URLs, and long
/// strings and keys, big collections and deep nesting are cut.
pub(crate) fn redact_fields(fields: &[(&str, Field)]) -> Map<String, Value> {
    let kept = fields.iter().take(MAX_ITEMS).map(|(key, Field(kind))| {
        let value = match kind {
            Kind::Value(value) => copy_entry(key, value, 0),
            Kind::Error(info) => replacement(key, false).map_or_else(
                || serde_json::to_value(info).unwrap_or(Value::Null),
                Value::from,
            ),
        };
        (truncate(key, MAX_KEY), value)
    });
    kept.chain(more_entries(fields.len())).collect()
}

/// [`redact_fields`] for a JSON object, such as a report's extra data.
pub(crate) fn redact_in_place(fields: &mut BTreeMap<String, Value>) {
    *fields = copy_entries(fields.iter(), 0).collect();
}

/// A URL without its credentials, query or fragment; relative URLs work too.
pub(crate) fn safe_url(url: &str) -> String {
    let prefix_len = SCHEME.find(url).map_or(0, |found| found.end());
    let (prefix, rest) = url.split_at_checked(prefix_len).unwrap_or(("", url));
    let address = if prefix.is_empty() {
        rest
    } else {
        rest.get(credentials_end(rest)..).unwrap_or(rest)
    };
    let path = address.split(['?', '#']).next().unwrap_or_default();
    format!("{prefix}{path}")
}

/// Redacts one string: secrets assigned to secret-looking names, credentials after "Bearer",
/// "Basic" or "token", and credentials, queries and fragments of URLs anywhere in it. It is cut
/// to length only afterwards, so a cut can't hide a secret from the patterns.
pub(crate) fn copy_string(value: &str) -> String {
    let text = SECRET_ASSIGNMENT.replace_all(value, "${1}[redacted]");
    let text = CREDENTIAL_IN_TEXT.replace_all(&text, "${1} [redacted]");
    let text = USERINFO_IN_TEXT.replace_all(&text, "${1}");
    let text = URL_IN_TEXT.replace_all(&text, |found: &Captures<'_>| safe_url(&found[0]));
    let text = QUERY_IN_PATH.replace_all(&text, "${1}${2}");
    truncate(&text, MAX_STRING)
}

/// "[redacted]" or "[omitted]" when a field named `key` must not be kept.
pub(crate) fn replacement(key: &str, count_or_flag: bool) -> Option<&'static str> {
    if count_or_flag {
        return None;
    }
    let words = words(key);
    let joined = words.concat();
    let last = words.last().map_or("", String::as_str);
    let has = |list: &[&str]| words.iter().any(|word| list.contains(&word.as_str()));
    let secret = (SECRET_IN_KEY.is_match(&joined) || has(&SECRET_WORDS))
        && !NOT_SECRET_KEY.is_match(&joined)
        && !DESCRIBES_SECRET.contains(&last);
    if secret {
        Some("[redacted]")
    } else if has(&CONTENT_WORDS) && !DESCRIBES_CONTENT.contains(&last) {
        Some("[omitted]")
    } else {
        None
    }
}

pub(crate) fn truncate(text: &str, max: usize) -> String {
    let length = text.chars().count();
    if length <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}… [{} more]", length - max)
}

// Credentials end at the authority's last "@". A colon in the authority is either a port or the
// start of a password that holds a "/", and the two can't be told apart; when an "@" follows
// before the query, it is taken to be a password.
fn credentials_end(rest: &str) -> usize {
    let authority = rest.split('/').next().unwrap_or_default();
    if let Some(at) = authority.rfind('@') {
        return at + 1;
    }
    if !authority.contains(':') {
        return 0;
    }
    let before_query = rest.split(['?', '#']).next().unwrap_or_default();
    before_query.rfind('@').map_or(0, |at| at + 1)
}

// "apiKey", "api_key" and "API-KEY" all give ["api", "key"].
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

fn copy_value(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) => copy_string(text).into(),
        Value::Array(_) | Value::Object(_) if depth >= MAX_DEPTH => "[too deep]".into(),
        Value::Array(items) => {
            let kept = items
                .iter()
                .take(MAX_ITEMS)
                .map(|item| copy_value(item, depth + 1));
            let more = excess(items.len()).map(|more| format!("[{more} more]").into());
            Value::Array(kept.chain(more).collect())
        }
        Value::Object(entries) => Value::Object(copy_entries(entries.iter(), depth).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

fn copy_entries<'a>(
    entries: impl ExactSizeIterator<Item = (&'a String, &'a Value)>,
    depth: usize,
) -> impl Iterator<Item = (String, Value)> {
    let more = more_entries(entries.len());
    entries
        .take(MAX_ITEMS)
        .map(move |(key, value)| {
            (
                truncate(&copy_string(key), MAX_KEY),
                copy_entry(key, value, depth),
            )
        })
        .chain(more)
}

// Numbers and flags are counts and switches, never secrets or content.
fn copy_entry(key: &str, value: &Value, depth: usize) -> Value {
    replacement(key, value.is_number() || value.is_boolean())
        .map_or_else(|| copy_value(value, depth + 1), Value::from)
}

fn more_entries(count: usize) -> Option<(String, Value)> {
    excess(count).map(|more| ("[more]".to_owned(), more.into()))
}

fn excess(count: usize) -> Option<usize> {
    count.checked_sub(MAX_ITEMS).filter(|more| *more > 0)
}

#[expect(
    clippy::expect_used,
    reason = "the patterns are constants, and the tests compile each one"
)]
fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("a valid pattern")
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
    fn cuts_credentials_from_values_wherever_a_url_sits_in_the_text() {
        let out = redacted(&[
            ("rawHeaders", json!(["Authorization", "Bearer abcdefgh1234MARKER"]).into()),
            ("shell", r#"curl -H "Authorization: Bearer abcdefgh1234MARKER" https://h.example/x?key=MARKER"#.into()),
            ("databaseUrl", "postgres://app:MARKER@db.internal:5432/weacon".into()),
            ("note", "see https://weacon.example/cb?code=MARKER#state=MARKER for details".into()),
        ]);

        assert_eq!(
            out,
            json!({
                "rawHeaders": ["Authorization", "Bearer [redacted]"],
                "shell": r#"curl -H "Authorization: [redacted]" https://h.example/x"#,
                "databaseUrl": "postgres://db.internal:5432/weacon",
                "note": "see https://weacon.example/cb for details",
            })
        );
    }

    #[test]
    fn redacts_secrets_in_free_text_and_keys_even_past_the_length_limit() {
        let texts = [
            "postgres://app:se'cretMARKER@db/weacon",
            "postgres://app:a<MARKER@db/weacon",
            "curl -H 'Authorization: token ghp_MARKER1234'",
            "host=db user=app password=MARKER",
            "Server=db;Password=MARKER;",
            "GITHUB_TOKEN=MARKER",
            r#"{"authorization":"MARKER"}"#,
            "token ghp_MARKER1234",
            "GET /auth/callback?code=MARKER&state=1",
        ];
        let long_url = format!("{} postgres://app:MARKERpw@db/weacon", "x".repeat(976));
        let long_bearer = format!("{} Authorization: Bearer MARKER123456789", "x".repeat(970));
        let out = redacted(&[
            ("texts", json!(texts).into()),
            ("long", json!([long_url, long_bearer]).into()),
            (
                "remotes",
                json!({ "https://x-access-token:ghs_MARKER@github.com/o/r.git": "origin" }).into(),
            ),
        ]);

        assert!(!out.to_string().contains("MARKER"), "{out}");
        assert_eq!(out["texts"][3], "host=db user=app password=[redacted]");
        assert_eq!(out["texts"][8], "GET /auth/callback");
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
    fn cuts_long_strings_long_keys_big_collections_and_deep_nesting() {
        let mut deep = json!({ "leaf": true });
        for _ in 0..10 {
            deep = json!({ "next": deep });
        }
        let long_key = format!("k{}", "e".repeat(150));
        let wide: Map<String, Value> = (0..105)
            .map(|index| (format!("k{index}"), index.into()))
            .collect();
        let out = redacted(&[
            ("long", "y".repeat(1200).into()),
            (&long_key, 1.into()),
            ("list", Value::from((0..103).collect::<Vec<i32>>()).into()),
            ("wide", Value::Object(wide).into()),
            ("deep", deep.into()),
        ]);

        assert_eq!(out["long"], format!("{}… [200 more]", "y".repeat(1000)));
        assert!(out.get(format!("k{}… [51 more]", "e".repeat(99))).is_some());
        let mut list: Vec<Value> = (0..100).map(Value::from).collect();
        list.push("[3 more]".into());
        assert_eq!(out["list"], Value::Array(list));
        assert_eq!(out["wide"].as_object().unwrap().len(), 101);
        assert_eq!(out["wide"]["[more]"], 5);
        assert!(out["deep"].to_string().contains("[too deep]"));
    }

    #[test]
    fn notes_how_many_fields_were_cut() {
        let fields: Vec<(String, Field)> = (0..102)
            .map(|index| (format!("k{index}"), index.into()))
            .collect();
        let borrowed: Vec<(&str, Field)> = fields
            .iter()
            .map(|(key, field)| (key.as_str(), field.clone()))
            .collect();

        assert_eq!(redact_fields(&borrowed)["[more]"], 2);
    }

    #[test]
    fn safe_url_drops_credentials_queries_and_fragments_even_from_odd_and_relative_urls() {
        let cases = [
            (
                "https://user:pass@weacon.example:8443/w/a?x=1#y",
                "https://weacon.example:8443/w/a",
            ),
            ("https://u:p@ss@host/x", "https://host/x"),
            ("postgres://u:p#ss@h/db", "postgres://h/db"),
            ("https://u:pa/ss@host/db", "https://host/db"),
            ("/invite?code=secret#frag", "/invite"),
            (
                "https://unpkg.com/react@18.3.1/index.js",
                "https://unpkg.com/react@18.3.1/index.js",
            ),
            ("https://host:8443/@user/post?x=1", "https://user/post"),
            (
                "postgres://app:2024/MARKER@db/weacon",
                "postgres://db/weacon",
            ),
            ("HTTPS://Host/Path?q", "HTTPS://Host/Path"),
        ];

        for (url, expected) in cases {
            assert_eq!(safe_url(url), expected, "{url}");
        }
    }

    #[test]
    fn splits_keys_into_words_whatever_their_style() {
        assert_eq!(words("apiKey"), ["api", "key"]);
        assert_eq!(words("API_KEY"), ["api", "key"]);
        assert_eq!(words("x-api-key2Value"), ["x", "api", "key", "value"]);
        assert_eq!(words("ümlautKey"), ["mlaut", "key"]);
    }

    #[test]
    fn truncates_by_characters_not_bytes() {
        assert_eq!(truncate("héllo", 2), "hé… [3 more]");
        assert_eq!(truncate("héllo", 5), "héllo");
    }

    #[test]
    fn compiles_every_pattern() {
        for pattern in [
            &SECRET_IN_KEY,
            &NOT_SECRET_KEY,
            &SECRET_ASSIGNMENT,
            &CREDENTIAL_IN_TEXT,
            &USERINFO_IN_TEXT,
            &QUERY_IN_PATH,
            &SCHEME,
            &URL_IN_TEXT,
        ] {
            LazyLock::force(pattern);
        }
    }
}
