//! Runs the built `fake-core` binary.

#[cfg(test)]
mod tests {
    use std::process::Command;

    fn fake_core(args: &[&str]) -> Option<i32> {
        Command::new(env!("CARGO_BIN_EXE_fake-core"))
            .args(args)
            .status()
            .unwrap()
            .code()
    }

    #[test]
    fn without_a_state_folder_it_says_how_to_run_it() {
        assert_eq!(fake_core(&[]), Some(2));
    }

    #[test]
    fn a_state_folder_it_cant_listen_in_is_a_failure() {
        assert_eq!(fake_core(&["/nonexistent/weacon-state"]), Some(1));
    }
}
