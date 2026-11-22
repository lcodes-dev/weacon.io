//! A turn of each agent, with a stand-in that prints what the real program printed.

use agent_run::turn;
use core_link::{Agent, Deed, Event};

const PROGRAM: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/agent.sh");

/// A turn in a new folder: what it told, how it ended, and what the program was started with.
async fn ran(
    agent: Agent,
    session: Option<&str>,
    prompt: &str,
) -> (Vec<Event>, Result<String, String>, String) {
    let name = format!("agent-run-{}-{}", std::process::id(), prompt.len());
    let folder = std::env::temp_dir().join(name).join(agent.program());
    std::fs::create_dir_all(&folder).unwrap();
    let mut told = Vec::new();
    let tell = |event| told.push(event);
    let never = std::future::pending();
    let ended = turn(PROGRAM, agent, &folder, session, prompt, never, tell).await;
    let started = std::fs::read_to_string(folder.join("started")).unwrap_or_default();
    std::fs::remove_dir_all(&folder).ok();
    (told, ended, started)
}

fn did(deed: Deed, what: &str) -> Event {
    let what = what.to_owned();
    Event::Did { deed, what }
}

fn said(event: &Event) -> &str {
    match event {
        Event::Said { text } => text,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn claude_code_s_turn_is_told_in_common_words() {
    let (told, ended, started) = ran(Agent::Claude, None, "Add double").await;
    assert_eq!(ended.unwrap(), "6519d1c4-62a2-4f52-b380-8edfefe39125");
    assert!(started.contains("--permission-prompts none") && !started.contains("--resume"));
    assert!(said(&told[0]).starts_with("I'll start by checking the repo"));
    // What a sub-agent said is left out, and a file is named from the folder.
    let deeds: Vec<&Event> = told
        .iter()
        .filter(|event| matches!(event, Event::Did { .. }))
        .collect();
    assert!(
        matches!(deeds[0], Event::Did { deed: Deed::Command, what } if what.starts_with("ls -la"))
    );
    assert_eq!(
        deeds[1..],
        [
            &did(Deed::Read, "calc.py"),
            &did(Deed::Read, "test_calc.py"),
            &did(Deed::Edit, "calc.py"),
            &did(Deed::Edit, "test_calc.py"),
            &did(Deed::Command, "python3 test_calc.py; echo \"exit=$?\""),
            &did(
                Deed::Command,
                "curl -s -m 5 https://example.com | head -c 50"
            ),
            &did(Deed::Command, "touch ~/weacon-sandbox-probe"),
        ]
    );
    assert!(told.iter().all(|event| *event
        != Event::Said {
            text: "A sub-agent thinking aloud.".into()
        }));
    assert!(said(told.last().unwrap()).starts_with("I added `double` and its assert"));

    // A later turn goes on with the conversation.
    let (_, _, started) = ran(Agent::Claude, Some("the-session"), "And triple").await;
    assert!(started.ends_with("--resume the-session\n"));
}

#[tokio::test]
async fn codex_s_turn_is_told_in_the_same_words() {
    let (told, ended, started) = ran(Agent::Codex, None, "Add double").await;
    assert_eq!(ended.unwrap(), "01a20424-89de-7df1-89b6-12b022e12a8a");
    assert!(started.starts_with("exec --json") && started.contains("workspace-write"));
    assert!(said(&told[0]).starts_with("I’ll inspect the existing calculator"));
    let deeds: Vec<&Event> = told
        .iter()
        .filter(|event| matches!(event, Event::Did { .. }))
        .collect();
    assert!(
        matches!(deeds[0], Event::Did { deed: Deed::Command, what } if what.starts_with("pwd && rg --files"))
    );
    assert_eq!(
        deeds[1..],
        [
            &did(Deed::Edit, "calc.py"),
            &did(Deed::Edit, "test_calc.py"),
            &did(Deed::Command, "python3 test_calc.py"),
            &did(
                Deed::Command,
                "curl -s -m 5 https://example.com | head -c 30"
            ),
        ]
    );
    // A link to a file doesn't give the folder away either.
    let last = said(told.last().unwrap());
    assert!(last.contains("[calc.py](calc.py)"), "{last}");

    let (_, _, started) = ran(Agent::Codex, Some("the-session"), "And triple").await;
    assert!(started.starts_with("exec resume the-session --json"));
}

#[tokio::test]
async fn a_program_that_ends_badly_is_a_failed_turn() {
    for agent in Agent::ALL {
        let (told, ended, _) = ran(agent, None, "Please fail").await;
        assert!(told.is_empty());
        let why = ended.unwrap_err();
        assert!(why.ends_with("not signed in"), "{why}");
    }
    let folder = std::env::temp_dir();
    let never = std::future::pending();
    let missing = turn(
        "no-such-program",
        Agent::Claude,
        &folder,
        None,
        "",
        never,
        |_| {},
    )
    .await;
    assert!(
        missing
            .unwrap_err()
            .starts_with("no-such-program could not be started")
    );
}

#[tokio::test]
async fn a_stopped_turn_ends_its_program_and_what_it_was_running() {
    let folder = std::env::temp_dir().join(format!("agent-run-{}-stop", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let (mut told, started) = (Vec::new(), std::time::Instant::now());
    // Stopped once the program has said its first words, while its command still runs.
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut stop = Some(stop);
    let tell = |event| {
        told.push(event);
        stop.take();
    };
    let stopped = async { drop(stopped.await) };
    let ended = turn(
        PROGRAM,
        Agent::Claude,
        &folder,
        None,
        "Be slow",
        stopped,
        tell,
    )
    .await;
    assert_eq!(ended.unwrap_err(), "Stopped");
    assert_eq!(told.len(), 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    // The program wrote this when it was interrupted, after ending its command.
    assert!(folder.join("cleaned-up").exists());
    std::fs::remove_dir_all(&folder).ok();
}
