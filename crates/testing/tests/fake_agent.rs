//! Runs the built `fake-agent` binary.

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::process::{Command, Output, Stdio};

    fn fake_agent(args: &[&str], input: &str) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fake-agent"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    #[test]
    fn it_answers_the_sign_in_status_and_refuses_other_calls() {
        let status = fake_agent(&["auth", "status", "--json"], "");
        let login = fake_agent(&["auth", "login"], "");

        assert_eq!(status.status.code(), Some(0));
        assert_eq!(status.stdout, b"{\"authMethod\":\"claude.ai\"}\n");
        assert_eq!(login.status.code(), Some(2));
        assert!(login.stdout.is_empty());
    }

    #[test]
    fn it_plays_a_turn_and_says_why_it_failed_when_the_answer_isnt_the_recorded_one() {
        let prompt = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"Hi\"}}\n";

        let asked = fake_agent(&["--print"], prompt);
        let strayed = fake_agent(&["--print"], &format!("{prompt}{{}}\n"));

        assert_eq!(asked.status.code(), Some(0));
        assert_eq!(String::from_utf8(asked.stdout).unwrap().lines().count(), 3);
        assert_eq!(strayed.status.code(), Some(1));
        let why = String::from_utf8(strayed.stderr).unwrap();
        assert!(why.starts_with("fake-agent: expected "));
        assert!(why.ends_with("but got {}\n"));
    }
}
