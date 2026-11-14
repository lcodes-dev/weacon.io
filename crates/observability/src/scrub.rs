use std::mem;

use regex::Regex;
use sentry_types::protocol::v7::{
    Breadcrumb, Context, DebugImage, Event, Frame, Map, Request, Stacktrace, Url, User, Value,
};

use crate::redact::{redact_in_place, redact_string, replacement, safe_url};

const NAVIGATION: [&str; 3] = ["url", "from", "to"];

/// Strips an error event: drops messages, exception values and source lines,
/// redacts the rest as the logger does, and writes `home_dir` as "~".
pub(crate) fn scrub_event(event: &mut Event<'_>, home_dir: &str) {
    let scrub = Scrubber {
        home: home_pattern(home_dir),
    };
    event.message = None;
    event.logentry = None;
    event.server_name = None;
    event.template = None;
    event.culprit = None;
    event.transaction = event.transaction.as_deref().map(safe_url);
    event.request = event.request.take().map(scrub_request);
    event.user = event.user.take().map(|user| User {
        id: user.id,
        ..User::default()
    });
    scrub.strings(&mut event.tags);
    scrub.object(&mut event.extra);
    event.contexts.retain(|_, context| scrub.context(context));
    event.breadcrumbs.values = mem::take(&mut event.breadcrumbs.values)
        .into_iter()
        .filter(|crumb| crumb.category.as_deref() != Some("console"))
        .map(|crumb| scrub.breadcrumb(crumb))
        .collect();
    for exception in &mut event.exception.values {
        exception.value = None;
        if !is_type_path(&exception.ty) {
            "Error".clone_into(&mut exception.ty);
        }
        if let Some(mechanism) = &mut exception.mechanism {
            mechanism.description = None;
            mechanism.help_link = None;
            scrub.object(&mut mechanism.data);
        }
        scrub.stack(exception.stacktrace.as_mut());
        scrub.stack(exception.raw_stacktrace.as_mut());
    }
    scrub.stack(event.stacktrace.as_mut());
    for thread in &mut event.threads.values {
        scrub.stack(thread.stacktrace.as_mut());
        scrub.stack(thread.raw_stacktrace.as_mut());
    }
    if !event.debug_meta.images.is_empty() {
        for image in &mut event.debug_meta.to_mut().images {
            scrub.image(image);
        }
    }
}

fn scrub_request(request: Request) -> Request {
    let url = request
        .url
        .and_then(|url| Url::parse(&safe_url(url.as_str())).ok());
    Request {
        url,
        method: request.method,
        ..Request::default()
    }
}

// The SDK takes an exception's type from the start of the error's `Debug` text, which can be a
// path or a message instead of a type.
fn is_type_path(ty: &str) -> bool {
    ty.split("::").all(|part| {
        let mut chars = part.chars();
        chars
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    })
}

struct Scrubber {
    home: Option<Regex>,
}

impl Scrubber {
    // `Context` is non-exhaustive; a kind this doesn't know can't be checked, so it is dropped.
    fn context(&self, context: &mut Context) -> bool {
        match context {
            Context::Device(device) => {
                device.name = None;
                self.object(&mut device.other);
            }
            Context::Os(os) => self.object(&mut os.other),
            Context::Runtime(runtime) => self.object(&mut runtime.other),
            Context::App(app) => self.object(&mut app.other),
            Context::Browser(browser) => self.object(&mut browser.other),
            Context::Gpu(gpu) => self.object(&mut gpu.other),
            Context::Trace(trace) => {
                trace.description = None;
                self.object(&mut trace.data);
            }
            Context::Otel(otel) => {
                self.object(&mut otel.attributes);
                self.object(&mut otel.resource);
                self.object(&mut otel.other);
            }
            Context::Response(response) => {
                response.cookies = None;
                response.data = None;
                self.strings(&mut response.headers);
            }
            Context::Other(fields) => self.object(fields),
            _ => return false,
        }
        true
    }

    fn breadcrumb(&self, mut crumb: Breadcrumb) -> Breadcrumb {
        crumb.message = None;
        self.object(&mut crumb.data);
        for key in NAVIGATION {
            if let Some(Value::String(url)) = crumb.data.get_mut(key) {
                *url = safe_url(url);
            }
        }
        crumb
    }

    fn object(&self, fields: &mut Map<String, Value>) {
        redact_in_place(fields);
        for value in fields.values_mut() {
            self.shorten_homes_in(value);
        }
    }

    fn strings(&self, fields: &mut Map<String, String>) {
        for (key, value) in fields.iter_mut() {
            *value = replacement(key)
                .map_or_else(|| self.shorten_home(&redact_string(value)), str::to_owned);
        }
    }

    fn shorten_homes_in(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.shorten_home(text),
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| self.shorten_homes_in(item)),
            Value::Object(entries) => entries
                .values_mut()
                .for_each(|item| self.shorten_homes_in(item)),
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    fn stack(&self, stack: Option<&mut Stacktrace>) {
        for frame in stack.into_iter().flat_map(|stack| &mut stack.frames) {
            self.frame(frame);
        }
    }

    fn frame(&self, frame: &mut Frame) {
        frame.vars.clear();
        frame.pre_context.clear();
        frame.context_line = None;
        frame.post_context.clear();
        for path in [&mut frame.filename, &mut frame.abs_path, &mut frame.package]
            .into_iter()
            .flatten()
        {
            *path = self.shorten_home(path);
        }
    }

    fn image(&self, image: &mut DebugImage) {
        match image {
            DebugImage::Apple(apple) => apple.name = self.shorten_home(&apple.name),
            DebugImage::Symbolic(symbolic) => {
                symbolic.name = self.shorten_home(&symbolic.name);
                symbolic.debug_file = symbolic
                    .debug_file
                    .as_deref()
                    .map(|path| self.shorten_home(path));
            }
            DebugImage::Wasm(wasm) => {
                wasm.code_file = self.shorten_home(&wasm.code_file);
                wasm.debug_file = wasm
                    .debug_file
                    .as_deref()
                    .map(|path| self.shorten_home(path));
            }
            DebugImage::Proguard(_) => {}
        }
    }

    fn shorten_home(&self, text: &str) -> String {
        self.home.as_ref().map_or_else(
            || text.to_owned(),
            |home| home.replace_all(text, "${1}~${2}").into_owned(),
        )
    }
}

// The home folder where a path starts: at the start of the text, after a file URL's "//", a
// Windows "\\?\" prefix, a space, a quote, "=" or "(". Either slash, in any case.
fn home_pattern(home_dir: &str) -> Option<Regex> {
    let home = home_dir.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return None;
    }
    let parts: Vec<String> = home.split(['/', '\\']).map(regex::escape).collect();
    Regex::new(&format!(
        r#"(?i)(^|//|\\\\\?\\|[\s"'=(]){}([/\\]|$)"#,
        parts.join(r"[/\\]")
    ))
    .ok()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use sentry_types::DebugId;
    use sentry_types::protocol::v7::{
        AppContext, AppleDebugImage, BrowserContext, DebugMeta, DeviceContext, Exception,
        GpuContext, LogEntry, Mechanism, OsContext, OtelContext, ResponseContext, RuntimeContext,
        SymbolicDebugImage, Thread, TraceContext, Uuid, Values, WasmDebugImage,
    };
    use serde_json::json;

    use super::*;

    fn frame(path: &str) -> Frame {
        Frame {
            function: Some("weacon_core::run".to_owned()),
            abs_path: Some(path.to_owned()),
            filename: Some(path.to_owned()),
            vars: [("prompt".to_owned(), json!("MARKER"))].into(),
            pre_context: vec!["MARKER".to_owned()],
            context_line: Some("let prompt = \"MARKER\";".to_owned()),
            ..Frame::default()
        }
    }

    fn stack(path: &str) -> Stacktrace {
        Stacktrace {
            frames: vec![frame(path)],
            ..Stacktrace::default()
        }
    }

    fn crumb(category: &str, message: &str, data: Value) -> Breadcrumb {
        Breadcrumb {
            category: Some(category.to_owned()),
            message: Some(message.to_owned()),
            data: serde_json::from_value(data).unwrap(),
            ..Breadcrumb::default()
        }
    }

    fn exceptions() -> Values<Exception> {
        Values {
            values: vec![
                Exception {
                    ty: "Fault".to_owned(),
                    value: Some("MARKER".to_owned()),
                    stacktrace: Some(stack("/Users/ada/dev/weacon/src/main.rs")),
                    raw_stacktrace: Some(stack("/Users/Ada/dev/weacon/src/raw.rs")),
                    mechanism: Some(Mechanism {
                        ty: "panic".to_owned(),
                        description: Some("MARKER".to_owned()),
                        help_link: Some(Url::parse("https://h.example/MARKER").unwrap()),
                        data: [("payload".to_owned(), json!("MARKER"))].into(),
                        ..Mechanism::default()
                    }),
                    ..Exception::default()
                },
                Exception {
                    ty: "/Users/Ada/clients/MARKER.md:".to_owned(),
                    ..Exception::default()
                },
            ],
        }
    }

    fn images() -> Vec<DebugImage> {
        vec![
            DebugImage::Wasm(WasmDebugImage {
                name: "weacon".to_owned(),
                debug_id: Uuid::nil(),
                debug_file: Some("file:///Users/Ada/dev/weacon.wasm".to_owned()),
                code_id: None,
                code_file: "/Users/Ada/dev/weacon.wasm".to_owned(),
            }),
            DebugImage::Apple(AppleDebugImage {
                name: "/Users/Ada/dev/Weacon.app/Contents/MacOS/weacon".to_owned(),
                arch: None,
                cpu_type: None,
                cpu_subtype: None,
                image_addr: 0.into(),
                image_size: 0,
                image_vmaddr: 0.into(),
                uuid: Uuid::nil(),
            }),
            DebugImage::Symbolic(SymbolicDebugImage {
                name: "/Users/Ada/dev/libweacon.so".to_owned(),
                arch: None,
                image_addr: 0.into(),
                image_size: 0,
                image_vmaddr: 0.into(),
                id: DebugId::nil(),
                code_id: None,
                debug_file: Some("/Users/Ada/dev/libweacon.debug".to_owned()),
            }),
        ]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a fixture that sets every field once"
    )]
    fn event() -> Event<'static> {
        Event {
            message: Some("MARKER".to_owned()),
            logentry: Some(LogEntry {
                message: "MARKER".to_owned(),
                params: vec![json!("MARKER")],
            }),
            server_name: Some("MARKER-laptop".into()),
            culprit: Some("MARKER".to_owned()),
            transaction: Some("GET /files/x?token=MARKER".to_owned()),
            request: Some(Request {
                url: Some(
                    Url::parse("https://ada:MARKER@weacon.example/chats/1?token=MARKER#MARKER")
                        .unwrap(),
                ),
                method: Some("POST".to_owned()),
                data: Some("MARKER".to_owned()),
                cookies: Some("MARKER".to_owned()),
                headers: [("authorization".to_owned(), "MARKER".to_owned())].into(),
                ..Request::default()
            }),
            user: Some(User {
                id: Some("user_1".to_owned()),
                email: Some("MARKER@example.com".to_owned()),
                username: Some("MARKER".to_owned()),
                ..User::default()
            }),
            tags: [
                ("route".to_owned(), "/chats/:id".to_owned()),
                ("cwd".to_owned(), "/Users/Ada/clients/acme".to_owned()),
                ("authToken".to_owned(), "MARKER".to_owned()),
                (
                    "callback".to_owned(),
                    "https://weacon.example/cb?code=MARKER".to_owned(),
                ),
            ]
            .into(),
            extra: [
                ("prompt".to_owned(), json!("MARKER")),
                ("attempts".to_owned(), json!(3)),
                (
                    "config".to_owned(),
                    json!({ "path": "/users/ada/weacon.toml" }),
                ),
            ]
            .into(),
            contexts: [
                (
                    "device".to_owned(),
                    Context::Device(Box::new(DeviceContext {
                        name: Some("MARKER-laptop".to_owned()),
                        arch: Some("arm64".to_owned()),
                        ..DeviceContext::default()
                    })),
                ),
                (
                    "chat".to_owned(),
                    Context::Other(
                        serde_json::from_value(json!({ "body": "MARKER", "id": "chat_1" }))
                            .unwrap(),
                    ),
                ),
            ]
            .into(),
            breadcrumbs: Values {
                values: vec![
                    crumb("console", "MARKER", json!({})),
                    crumb(
                        "navigation",
                        "MARKER",
                        json!({ "from": "/w/a?token=MARKER", "to": "/w/b#MARKER" }),
                    ),
                    crumb(
                        "http",
                        "MARKER",
                        json!({ "url": "https://h.example/x?key=MARKER", "status_code": 500 }),
                    ),
                ],
            },
            exception: exceptions(),
            stacktrace: Some(stack("/Users/Ada/dev/weacon/src/top.rs")),
            threads: Values {
                values: vec![Thread {
                    stacktrace: Some(stack("/Users/Ada/dev/weacon/src/thread.rs")),
                    raw_stacktrace: Some(stack("/Users/Ada/dev/weacon/src/thread_raw.rs")),
                    ..Thread::default()
                }],
            },
            debug_meta: Cow::Owned(DebugMeta {
                images: images(),
                ..DebugMeta::default()
            }),
            ..Event::default()
        }
    }

    #[test]
    fn leaves_no_free_text_secrets_or_content() {
        let mut event = event();

        scrub_event(&mut event, "/Users/Ada/");

        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("MARKER"), "{json}");
        assert!(!json.to_lowercase().contains("/users/ada"), "{json}");
    }

    #[test]
    fn keeps_what_a_report_needs() {
        let mut event = event();

        scrub_event(&mut event, "/Users/Ada");

        let request = event.request.unwrap();
        assert_eq!(
            request.url.unwrap().as_str(),
            "https://weacon.example/chats/1"
        );
        assert_eq!(request.method.as_deref(), Some("POST"));
        assert_eq!(event.user.unwrap().id.as_deref(), Some("user_1"));
        assert_eq!(event.tags["route"], "/chats/:id");
        assert_eq!(event.tags["authToken"], "[redacted]");
        assert_eq!(event.tags["callback"], "https://weacon.example/cb");
        assert_eq!(event.extra["attempts"], 3);
        assert_eq!(event.extra["prompt"], "[omitted]");
        assert_eq!(event.tags["cwd"], "~/clients/acme");
        assert_eq!(event.extra["config"]["path"], "~/weacon.toml");
        assert_eq!(event.transaction.as_deref(), Some("GET /files/x"));
        assert_eq!(event.exception.values[1].ty, "Error");
        let exception = &event.exception.values[0];
        assert_eq!(exception.ty, "Fault");
        let frame = &exception.stacktrace.as_ref().unwrap().frames[0];
        assert_eq!(frame.function.as_deref(), Some("weacon_core::run"));
        assert_eq!(frame.abs_path.as_deref(), Some("~/dev/weacon/src/main.rs"));
        let categories: Vec<_> = event
            .breadcrumbs
            .values
            .iter()
            .map(|crumb| crumb.category.as_deref())
            .collect();
        assert_eq!(categories, [Some("navigation"), Some("http")]);
        assert_eq!(event.breadcrumbs.values[0].data["from"], "/w/a");
        assert_eq!(event.breadcrumbs.values[1].data["status_code"], 500);
        let DebugImage::Wasm(image) = &event.debug_meta.images[0] else {
            panic!("not a wasm image")
        };
        assert_eq!(image.code_file, "~/dev/weacon.wasm");
        assert_eq!(
            image.debug_file.as_deref(),
            Some("file://~/dev/weacon.wasm")
        );
    }

    #[test]
    fn shortens_home_folders_in_any_case_and_with_either_slash() {
        let scrub = Scrubber {
            home: home_pattern(r"C:\Users\Ada\"),
        };

        for (path, shortened) in [
            (r"C:\Users\Ada\dev\main.rs", r"~\dev\main.rs"),
            ("c:/users/ada/dev/main.rs", "~/dev/main.rs"),
            (r"\\?\C:\Users\Ada\dev\main.rs", r"\\?\~\dev\main.rs"),
            (r#"opened "C:\Users\Ada\x" twice"#, r#"opened "~\x" twice"#),
            (r"C:\Users\Adam\main.rs", r"C:\Users\Adam\main.rs"),
            (r"D:\backup\C:\Users\Ada\x", r"D:\backup\C:\Users\Ada\x"),
        ] {
            assert_eq!(scrub.shorten_home(path), shortened, "{path}");
        }
        assert_eq!(
            Scrubber {
                home: home_pattern("")
            }
            .shorten_home("/srv/main.rs"),
            "/srv/main.rs"
        );
    }

    #[test]
    fn keeps_only_type_paths_as_exception_types() {
        for ty in ["Fault", "std::io::Error", "sqlx::Error", "_Inner"] {
            assert!(is_type_path(ty), "{ty}");
        }
        for ty in ["", "/Users/ada/x.md:", "error:", "Some(Error", "a::"] {
            assert!(!is_type_path(ty), "{ty}");
        }
    }

    fn contexts() -> [Context; 8] {
        let other = || -> Map<String, Value> {
            [
                ("prompt".to_owned(), json!("MARKER")),
                ("build".to_owned(), json!("1.2.3")),
            ]
            .into()
        };
        [
            Context::Os(Box::new(OsContext {
                other: other(),
                ..OsContext::default()
            })),
            Context::Runtime(Box::new(RuntimeContext {
                other: other(),
                ..RuntimeContext::default()
            })),
            Context::App(Box::new(AppContext {
                other: other(),
                ..AppContext::default()
            })),
            Context::Browser(Box::new(BrowserContext {
                other: other(),
                ..BrowserContext::default()
            })),
            Context::Gpu(Box::new(GpuContext {
                other: other(),
                ..GpuContext::default()
            })),
            Context::Trace(Box::new(TraceContext {
                description: Some("MARKER".to_owned()),
                data: other(),
                ..TraceContext::default()
            })),
            Context::Otel(Box::new(OtelContext {
                attributes: other(),
                resource: other(),
                other: other(),
            })),
            Context::Response(Box::new(ResponseContext {
                cookies: Some("MARKER".to_owned()),
                headers: [("set-cookie".to_owned(), "MARKER".to_owned())].into(),
                data: Some(json!("MARKER")),
                status_code: Some(500),
                ..ResponseContext::default()
            })),
        ]
    }

    #[test]
    fn scrubs_every_kind_of_context() {
        let mut event = Event {
            contexts: contexts()
                .into_iter()
                .enumerate()
                .map(|(index, context)| (index.to_string(), context))
                .collect(),
            ..Event::default()
        };

        scrub_event(&mut event, "/home/ada");

        let json = serde_json::to_string(&event.contexts).unwrap();
        assert!(!json.contains("MARKER"), "{json}");
        assert_eq!(event.contexts.len(), 8);
        assert_eq!(json.matches("1.2.3").count(), 9, "{json}");
    }
}
