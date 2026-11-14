use std::error::Error;
use std::fmt;
use std::io;

use serde::Serialize;
use weacon_protocol::Code;

const MAX_CAUSES: usize = 3;

/// An error Weacon raises: a code and a fixed message, both safe to log.
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
    pub const fn code(&self) -> &Code {
        &self.code
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

/// What a log line or an error report may say about an error.
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

impl ErrorInfo {
    pub(crate) fn type_name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub(crate) fn cause(&self) -> Option<&Self> {
        self.cause.as_deref()
    }
}

/// Describes an error without its message, which can hold paths and input; only a [`Fault`]
/// keeps its own.
pub(crate) fn describe_error<E: Error + 'static>(error: &E) -> ErrorInfo {
    ErrorInfo {
        name: Some(type_path::<E>()),
        ..describe(error, 0)
    }
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
        code: fault.map(|fault| fault.code().clone()),
        message: fault.map(|fault| fault.message),
        kind: io_kind.map(|kind| format!("{kind:?}")),
        cause,
    }
}

// Generic arguments can name user types in odd places and add nothing to a report.
fn type_path<T>() -> String {
    let name = std::any::type_name::<T>();
    name.split('<').next().unwrap_or(name).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const STORE_FULL: Code = weacon_protocol::code!("store.full");

    #[derive(Debug)]
    struct Leaky<T>(T);

    impl<T: fmt::Debug> fmt::Display for Leaky<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "could not open /home/ada/MARKER {:?}", self.0)
        }
    }

    impl<T: fmt::Debug> Error for Leaky<T> {}

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
