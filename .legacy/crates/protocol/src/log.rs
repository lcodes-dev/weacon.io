use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Very noisy.
    Trace,
    /// Developer detail.
    Debug,
    /// Lifecycle.
    #[default]
    Info,
    /// Failed and handled.
    Warn,
    /// A person needs to look.
    Error,
    /// About to exit.
    Fatal,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn levels_are_ordered_and_lowercase() {
        assert!(
            Level::Trace < Level::Debug
                && Level::Warn < Level::Error
                && Level::Error < Level::Fatal
        );
        assert_eq!(Level::default(), Level::Info);
        assert_eq!(serde_json::to_value(Level::Fatal).unwrap(), json!("fatal"));
    }
}
