//! What a person sees of the sessions they ran outside Weacon: a list, and pages of events.

use std::cmp::Reverse;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use weacon_protocol::{
    AgentKind, AgentSession, ChatEntry, ChatEvent, MAX_FRAME, OutsideSession, RunId, RunUpdate,
    SessionsAnswerKind, SessionsQueryKind,
};

use crate::sessions::{Files, SessionFile, find};
use crate::transcript::{Step, Transcript};

/// How much of a file, 64 KiB, is read for the folder it names.
const HEAD: u64 = 65_536;
/// How much of a file, 64 MiB, is read to open it; a longer session shows its start.
const WHOLE: u64 = 67_108_864;

/// How many sessions a list holds at most; with their folders, they still fit one frame.
const LISTED: usize = 200;
/// What an answer's envelope adds to its events.
const ENVELOPE: usize = 4096;

/// The answer to `query` about the sessions in each agent's config folder in `dirs`; `None`
/// for a query this build doesn't know. A file that can't be read counts as missing.
pub fn answer(
    files: &impl Files,
    dirs: &[(AgentKind, &Path)],
    query: &SessionsQueryKind,
) -> Option<SessionsAnswerKind> {
    Some(match query {
        SessionsQueryKind::List => SessionsAnswerKind::List {
            sessions: list(files, dirs, LISTED),
        },
        SessionsQueryKind::Open {
            agent,
            session,
            after,
        } => {
            let dir = dirs.iter().find(|(kind, _)| kind == agent);
            let budget = MAX_FRAME - ENVELOPE;
            match dir.and_then(|&place| open(files, place, session, *after, budget)) {
                Some(Ok((events, more))) => SessionsAnswerKind::Opened { events, more },
                Some(Err(_)) | None => SessionsAnswerKind::Missing,
            }
        }
        SessionsQueryKind::Newer => return None,
    })
}

/// The sessions in each agent's config folder, newest first, at most `limit`.
pub fn list(files: &impl Files, dirs: &[(AgentKind, &Path)], limit: usize) -> Vec<OutsideSession> {
    let mut found: Vec<SessionFile> = dirs
        .iter()
        .flat_map(|(agent, dir)| find(files, *agent, dir))
        .collect();
    found.sort_by_key(|file| Reverse(file.modified));
    found
        .iter()
        .take(limit)
        .map(|file| OutsideSession {
            agent: file.agent,
            session: file.session.clone(),
            folder: folder(files, file),
            modified_at: seconds(file.modified),
        })
        .collect()
}

/// The events of `agent`'s `session` in `dir` after `after`, as many as fit `budget` bytes of
/// JSON, and whether more follow; `None` when there's no such session. An event too large
/// for `budget` on its own is left out.
///
/// # Errors
/// When the session's file can't be read.
pub fn open(
    files: &impl Files,
    (agent, dir): (AgentKind, &Path),
    session: &AgentSession,
    after: u64,
    budget: usize,
) -> Option<io::Result<(Vec<ChatEntry>, bool)>> {
    let file = find(files, agent, dir)
        .into_iter()
        .find(|file| file.session == *session)?;
    let run = RunId::try_from(session.as_str().to_owned()).ok()?;
    Some(file.read(files, 0, WHOLE).map(|bytes| {
        let steps = Transcript::read(agent, &bytes).map(|read| read.steps);
        page(&run, steps.unwrap_or_default(), after, budget)
    }))
}

fn page(run: &RunId, steps: Vec<Step>, after: u64, budget: usize) -> (Vec<ChatEntry>, bool) {
    let mut events = Vec::new();
    let mut used = 0;
    let later = (1..).zip(steps).skip_while(|(seq, _)| *seq <= after);
    for (seq, step) in later {
        let entry = ChatEntry {
            seq,
            event: event(run, step),
        };
        let size = serde_json::to_vec(&entry).map_or(usize::MAX, |json| json.len());
        if size > budget {
            continue;
        }
        if used + size > budget {
            return (events, true);
        }
        used += size;
        events.push(entry);
    }
    (events, false)
}

fn event(run: &RunId, step: Step) -> ChatEvent {
    match step {
        Step::Prompt(text) => ChatEvent::OutsidePrompt { text },
        Step::Event(event) => ChatEvent::Run {
            update: RunUpdate {
                run: run.clone(),
                event,
            },
        },
    }
}

fn folder(files: &impl Files, file: &SessionFile) -> Option<String> {
    let head = file.read(files, 0, HEAD).ok()?;
    let cwd = Transcript::read(file.agent, &head)?.cwd?;
    cwd.to_str().map(str::to_owned)
}

fn seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use serde_json::json;
    use weacon_protocol::{Envelope, MachineId, Message, RequestId, RunEvent, SessionsAnswer};
    use weacon_testing::TempDir;

    use super::*;
    use crate::sessions::Disk;
    use crate::test_support::jsonl;

    const ONE: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
    const TWO: &str = "1c8d7b63-4a2f-4e3b-8d9c-6f5a4b3c2d1e";

    fn write(file: &Path, text: &str, at: u64) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
        let modified = UNIX_EPOCH + Duration::from_secs(at);
        let opened = fs::File::options().write(true).open(file).unwrap();
        opened.set_modified(modified).unwrap();
    }

    fn claude(dir: &Path, session: &str) -> std::path::PathBuf {
        dir.join("projects")
            .join("-work")
            .join(format!("{session}.jsonl"))
    }

    fn session(id: &str) -> AgentSession {
        AgentSession::try_from(id.to_owned()).unwrap()
    }

    fn prompt(text: &str) -> String {
        jsonl(&[json!({ "type": "user", "cwd": "/work/app", "message": { "content": text } })])
    }

    #[test]
    fn sessions_of_both_agents_are_listed_newest_first_with_their_folders() {
        let dir = TempDir::new().unwrap();
        let (claude_dir, codex_dir) = (dir.join("claude"), dir.join("codex"));
        write(&claude(&claude_dir, ONE), &prompt("Hi"), 100);
        let day = codex_dir
            .join("sessions")
            .join("2026")
            .join("11")
            .join("13");
        let rollout = day.join(format!("rollout-2026-11-13T10-00-00-{TWO}.jsonl"));
        write(&rollout, "", 200);
        write(&claude(&claude_dir, TWO), "", 50);
        let dirs = [
            (AgentKind::Claude, &*claude_dir),
            (AgentKind::Codex, &*codex_dir),
        ];

        let listed = list(&Disk, &dirs, 2);

        assert_eq!(
            listed,
            [
                OutsideSession {
                    agent: AgentKind::Codex,
                    session: session(TWO),
                    folder: None,
                    modified_at: 200,
                },
                OutsideSession {
                    agent: AgentKind::Claude,
                    session: session(ONE),
                    folder: Some("/work/app".to_owned()),
                    modified_at: 100,
                },
            ]
        );
    }

    fn file_of(lines: usize) -> String {
        (0..lines).map(|at| prompt(&format!("p{at}"))).collect()
    }

    fn opened(dir: &Path, after: u64, budget: usize) -> (Vec<ChatEntry>, bool) {
        open(
            &Disk,
            (AgentKind::Claude, dir),
            &session(ONE),
            after,
            budget,
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn a_session_opens_as_prompts_and_runs_named_after_it() {
        let dir = TempDir::new().unwrap();
        let assistant = json!({ "type": "assistant", "message": { "content": [
            { "type": "text", "text": "Done" },
        ] } });
        write(
            &claude(&dir, ONE),
            &(prompt("Fix it") + &jsonl(&[assistant])),
            0,
        );

        let (events, more) = opened(&dir, 0, usize::MAX);

        let run = RunId::try_from(ONE.to_owned()).unwrap();
        assert_eq!(
            events,
            [
                ChatEntry {
                    seq: 1,
                    event: ChatEvent::OutsidePrompt {
                        text: "Fix it".to_owned()
                    },
                },
                ChatEntry {
                    seq: 2,
                    event: ChatEvent::Run {
                        update: RunUpdate {
                            run,
                            event: RunEvent::Text {
                                text: "Done".to_owned()
                            },
                        },
                    },
                },
            ]
        );
        assert!(!more);
    }

    #[test]
    fn events_come_in_pages_that_fit_the_budget() {
        let dir = TempDir::new().unwrap();
        write(&claude(&dir, ONE), &file_of(5), 0);
        let one = ChatEntry {
            seq: 1,
            event: ChatEvent::OutsidePrompt {
                text: "p0".to_owned(),
            },
        };
        let size = serde_json::to_vec(&one).unwrap().len();

        let (first, more) = opened(&dir, 0, 2 * size);
        assert_eq!(
            first.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(more);
        let (rest, more) = opened(&dir, 3, 2 * size);
        assert_eq!(
            rest.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [4, 5]
        );
        assert!(!more);
        let (exact, more) = opened(&dir, 4, size);
        assert_eq!(exact.len(), 1);
        assert!(!more);
    }

    #[test]
    fn an_event_too_large_on_its_own_is_left_out() {
        let dir = TempDir::new().unwrap();
        let long = "x".repeat(500);
        write(&claude(&dir, ONE), &(prompt(&long) + &prompt("short")), 0);

        let (events, more) = opened(&dir, 0, 100);

        assert_eq!(
            events.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            [2]
        );
        assert!(!more);
    }

    #[test]
    fn a_session_that_isnt_there_or_cant_be_read_says_so() {
        let dir = TempDir::new().unwrap();
        let secret = dir.join(".credentials.json");
        write(&secret, "secret", 0);
        let file = claude(&dir, TWO);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::hard_link(&secret, &file).unwrap();
        let place = (AgentKind::Claude, &*dir);

        assert!(open(&Disk, place, &session(ONE), 0, 100).is_none());
        open(&Disk, place, &session(TWO), 0, 100)
            .unwrap()
            .unwrap_err();
        assert_eq!(list(&Disk, &[place], 10).first().unwrap().folder, None);
    }

    #[test]
    fn queries_are_answered_from_the_agents_own_folders() {
        let dir = TempDir::new().unwrap();
        write(&claude(&dir, ONE), &prompt("Hi"), 9);
        let secret = dir.join(".credentials.json");
        write(&secret, "secret", 0);
        fs::hard_link(&secret, claude(&dir, TWO)).unwrap();
        let dirs = [(AgentKind::Claude, &*dir)];
        let open = |agent, id| SessionsQueryKind::Open {
            agent,
            session: session(id),
            after: 0,
        };

        let listed = answer(&Disk, &dirs, &SessionsQueryKind::List).unwrap();
        let SessionsAnswerKind::List { sessions } = listed else {
            panic!("{listed:?}");
        };
        assert_eq!(sessions.len(), 2);
        let opened = answer(&Disk, &dirs, &open(AgentKind::Claude, ONE)).unwrap();
        assert!(
            matches!(opened, SessionsAnswerKind::Opened { ref events, more: false } if events.len() == 1)
        );
        for missing in [open(AgentKind::Claude, TWO), open(AgentKind::Codex, ONE)] {
            assert_eq!(
                answer(&Disk, &dirs, &missing),
                Some(SessionsAnswerKind::Missing)
            );
        }
        assert_eq!(answer(&Disk, &dirs, &SessionsQueryKind::Newer), None);
    }

    #[test]
    fn a_long_session_opens_a_frame_at_a_time() {
        let dir = TempDir::new().unwrap();
        let line = prompt(&"x".repeat(1000));
        write(&claude(&dir, ONE), &line.repeat(2000), 0);
        let query = SessionsQueryKind::Open {
            agent: AgentKind::Claude,
            session: session(ONE),
            after: 0,
        };

        let opened = answer(&Disk, &[(AgentKind::Claude, &*dir)], &query).unwrap();

        assert!(
            matches!(&opened, SessionsAnswerKind::Opened { events, more: true } if events.len() > 900)
        );
        let envelope = Envelope {
            id: u32::MAX,
            message: Message::SessionsAnswer(SessionsAnswer {
                request: RequestId::try_from("r".repeat(64)).unwrap(),
                machine: MachineId::try_from("m".repeat(64)).unwrap(),
                answer: opened,
            }),
        };
        envelope.to_json().unwrap();
    }

    #[test]
    fn a_time_before_the_epoch_counts_as_its_start() {
        assert_eq!(seconds(UNIX_EPOCH - Duration::from_secs(1)), 0);
        assert_eq!(seconds(UNIX_EPOCH + Duration::from_secs(7)), 7);
    }
}
