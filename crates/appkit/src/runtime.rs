use tokio::runtime::{Builder, Runtime};
use weacon_observability::Fault;
use weacon_protocol::code;

/// The async runtime `builder` describes, with I/O and timers on.
///
/// # Errors
/// `runtime.failed` when it can't start.
pub fn runtime(mut builder: Builder) -> Result<Runtime, Fault> {
    builder.enable_all().build().map_err(|error| {
        Fault::new(code!("runtime.failed"), "Weacon couldn't start its runtime").because(error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_runtime_runs_what_it_is_given() {
        let runtime = runtime(Builder::new_current_thread()).unwrap();

        assert_eq!(runtime.block_on(async { 7 }), 7);
    }
}
