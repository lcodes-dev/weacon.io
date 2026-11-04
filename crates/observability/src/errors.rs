use std::error::Error;
use std::fmt;
use std::io;

use serde::Serialize;

const MAX_CAUSES: usize = 3;

/// A dotted error code such as `store.full`: lowercase words joined by dots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Code(&'static str);

/// A [`Code`], checked when the program is compiled: `code!("store.full")`.
///
/// ```compile_fail
/// let code = weacon_observability::code!("Store.full");
/// ```
#[macro_export]
macro_rules! code {
    ($code:literal) => {
        const { $crate::Code::new($code) }
    };
}

impl Code {
    /// Use [`code!`], which checks the code when the program is compiled.
    ///
    /// # Panics
    /// Unless `code` is dotted lowercase words.
    #[doc(hidden)]
    #[must_use]
    pub const fn new(code: &'static str) -> Self {
        assert!(
            is_code(code),
            "error codes are dotted lowercase words, like store.full"
        );
        Self(code)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

const fn is_code(code: &str) -> bool {
    let mut rest = code.as_bytes();
    let mut words = 1;
    let mut word_start = true;
    while let [byte, tail @ ..] = rest {
        let byte = *byte;
        if word_start {
            if !byte.is_ascii_lowercase() {
                return false;
            }
            word_start = false;
        } else if byte == b'.' {
            words += 1;
            word_start = true;
        } else if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
        rest = tail;
    }
    !word_start && words > 1
}

/// An error Weacon raises: a code and a fixed message with no user content, so both may be
/// logged and reported.
#[derive(Debug)]
pub struct Fault {
    code: Code,
    message: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl Fault {
    #[must_use]
    pub const fn new(code: Code, message: &'static str) -> Self {
        Self {
            code,
            message,
            source: None,
        }
    }

    #[must_use]
    pub fn because(mut self, source: impl Into<Box<dyn Error + Send + Sync>>) -> Self {
        self.source = Some(source.into());
        self
    }

    #[must_use]
    pub const fn code(&self) -> Code {
        self.code
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl Error for Fault {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_deref()?)
    }
}

/// What can safely be said about an error in a log line or an error report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ErrorInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<Code>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cause: Option<Box<Self>>,
}

/// An error that can be logged: any error type, or a `dyn Error` such as a boxed one.
pub trait AsError {
    fn as_error(&self) -> &(dyn Error + 'static);

    /// The error's type, when it is known.
    fn type_path(&self) -> Option<String>;
}

impl<E: Error + 'static> AsError for E {
    fn as_error(&self) -> &(dyn Error + 'static) {
        self
    }

    fn type_path(&self) -> Option<String> {
        Some(type_path::<E>())
    }
}

impl AsError for dyn Error + 'static {
    fn as_error(&self) -> &(dyn Error + 'static) {
        self
    }

    fn type_path(&self) -> Option<String> {
        None
    }
}

impl AsError for dyn Error + Send + Sync + 'static {
    fn as_error(&self) -> &(dyn Error + 'static) {
        self
    }

    fn type_path(&self) -> Option<String> {
        None
    }
}

/// Describes an error by its type, code, I/O error kind and the first causes. Only a [`Fault`]
/// keeps its message: other code writes paths, input and URLs into messages.
pub(crate) fn describe_error<E: AsError + ?Sized>(error: &E) -> ErrorInfo {
    let mut info = describe(error.as_error(), 0);
    if let Some(name) = error.type_path() {
        info.name = Some(name);
    }
    info
}

fn describe(error: &(dyn Error + 'static), depth: usize) -> ErrorInfo {
    let fault = error.downcast_ref::<Fault>();
    let io_kind = error.downcast_ref::<io::Error>().map(io::Error::kind);
    let cause = error
        .source()
        .filter(|_| depth < MAX_CAUSES)
        .map(|source| Box::new(describe(source, depth + 1)));
    ErrorInfo {
        name: fault
            .map(|_| type_path::<Fault>())
            .or_else(|| io_kind.map(|_| type_path::<io::Error>())),
        code: fault.map(Fault::code),
        message: fault.map(|fault| fault.message),
        kind: io_kind.map(|kind| format!("{kind:?}")),
        cause,
    }
}

// Generic arguments can name user types in odd places and add nothing to a report.
fn type_path<T: ?Sized>() -> String {
    let name = std::any::type_name::<T>();
    name.split('<').next().unwrap_or(name).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_FULL: Code = crate::code!("store.full");

    #[derive(Debug)]
    struct Leaky<T>(T);

    impl<T: fmt::Debug> fmt::Display for Leaky<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "could not open /home/ada/MARKER {:?}", self.0)
        }
    }

    impl<T: fmt::Debug> Error for Leaky<T> {}

    #[test]
    fn accepts_dotted_lowercase_codes_only() {
        for good in [
            "store.full",
            "driver.timeout",
            "ssh.host_key_changed",
            "agent_sdk.timeout",
            "a1.b2.c_3",
        ] {
            assert!(is_code(good), "{good}");
        }
        for bad in [
            "",
            "store",
            "Store.full",
            "store.",
            ".full",
            "store..full",
            "_store.full",
            "store.Full",
            "store.2x",
            "störe.full",
            "store full",
        ] {
            assert!(!is_code(bad), "{bad}");
        }
    }

    #[test]
    fn keeps_a_faults_code_and_message() {
        let info = describe_error(&Fault::new(STORE_FULL, "The store is full"));

        assert_eq!(
            info.name.as_deref(),
            Some("weacon_observability::errors::Fault")
        );
        assert_eq!(info.code, Some(STORE_FULL));
        assert_eq!(info.message, Some("The store is full"));
    }

    #[test]
    fn drops_other_messages_and_generic_arguments() {
        let info = describe_error(&Leaky("MARKER"));
        let json = serde_json::to_string(&info).unwrap();

        assert!(!json.contains("MARKER"), "{json}");
        assert_eq!(
            info.name.as_deref(),
            Some("weacon_observability::errors::tests::Leaky")
        );
        assert_eq!(info.message, None);
    }

    #[test]
    fn names_io_errors_by_kind() {
        let error = io::Error::new(io::ErrorKind::NotFound, "/home/ada/MARKER");
        let info = describe_error(&Fault::new(STORE_FULL, "The store is full").because(error));
        let cause = info.cause.unwrap();

        assert_eq!(cause.name.as_deref(), Some("std::io::error::Error"));
        assert_eq!(cause.kind.as_deref(), Some("NotFound"));
        assert_eq!(cause.message, None);
    }

    #[test]
    fn describes_boxed_errors_by_what_they_hold() {
        let boxed: Box<dyn Error + Send + Sync> =
            Box::new(Fault::new(STORE_FULL, "The store is full"));
        let info = describe_error(&*boxed);

        assert_eq!(
            info.name.as_deref(),
            Some("weacon_observability::errors::Fault")
        );
        assert_eq!(info.message, Some("The store is full"));
    }

    #[test]
    fn follows_at_most_three_causes() {
        let mut error = Fault::new(STORE_FULL, "level 0");
        for _ in 0..5 {
            error = Fault::new(STORE_FULL, "outer").because(error);
        }
        let mut depth = 0;
        let mut info = describe_error(&error);
        while let Some(cause) = info.cause {
            depth += 1;
            info = *cause;
        }

        assert_eq!(depth, MAX_CAUSES);
    }
}
