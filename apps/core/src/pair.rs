//! `weacon pair`: pairs this machine with the account of whoever confirms its code in the
//! web app.

use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use weacon_observability::Fault;
use weacon_protocol::{MachineName, PairClaim, PairClaimed, PairStart, PairStarted, code};

use crate::app::{App, state_unavailable};
use crate::machine::{self, Paired};
use crate::platform;
use crate::server::{Failure, Server};

/// How often the core asks whether the person has confirmed.
const CLAIM_EVERY: Duration = Duration::from_secs(2);
/// How many times it asks: a little past the ten minutes a pairing lasts.
const CLAIMS: u32 = 330;
/// How many failed claims in a row mean the server is gone.
const FAILED_CLAIMS: u32 = 15;

/// Pairs this machine with the server at `url`, named `name`; prints the code to confirm to
/// `out`.
///
/// # Errors
/// When the server can't be reached or refuses, nobody confirms in time, or the pairing can't
/// be kept.
pub(crate) fn run(
    app: &App,
    url: &str,
    name: Option<String>,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let asked = Asked {
        url,
        name: name.unwrap_or_else(|| platform::MACHINE_NAME.to_owned()),
    };
    pair(state_dir, &asked, CLAIM_EVERY, out)
}

/// What `weacon pair` was asked.
struct Asked<'a> {
    url: &'a str,
    name: String,
}

fn pair(
    state_dir: &Path,
    asked: &Asked<'_>,
    every: Duration,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let server = Server::new(asked.url)?;
    let start = PairStart {
        name: MachineName::try_from(asked.name.clone()).map_err(|_| bad_name())?,
        os: platform::MACHINE_OS,
    };
    let started: PairStarted = server.post("/api/machines/pair", &start).map_err(failed)?;
    writeln!(
        out,
        "Enter this code in Weacon to pair this machine: {}\nWaiting for you to confirm…",
        started.code.expose()
    )
    .map_err(|error| not_printed().because(error))?;
    let claim = PairClaim {
        pairing: started.pairing,
        secret: started.secret,
    };
    let paired = claimed(&server, asked.url, &claim, every)?;
    machine::write(state_dir, &paired)?;
    writeln!(
        out,
        "Paired. The core connects to Weacon while its daemon runs."
    )
    .map_err(|error| not_printed().because(error))
}

/// Claims the pairing with the server at `url` every `every` until the person confirms it, or
/// it ends.
fn claimed(
    server: &Server,
    url: &str,
    claim: &PairClaim,
    every: Duration,
) -> Result<Paired, Fault> {
    let mut failed_in_a_row = 0;
    for _ in 0..CLAIMS {
        match server.post("/api/machines/pair/claim", claim) {
            Ok(PairClaimed::Waiting) => failed_in_a_row = 0,
            Ok(PairClaimed::Paired {
                machine,
                owner,
                token,
            }) => {
                return Ok(Paired {
                    server: url.to_owned(),
                    machine,
                    owner,
                    token: token.into_inner(),
                });
            }
            Err(Failure::Unreachable(fault)) => {
                failed_in_a_row += 1;
                if failed_in_a_row >= FAILED_CLAIMS {
                    return Err(fault);
                }
            }
            Err(Failure::Refused(404, _)) => return Err(expired()),
            Err(Failure::Refused(..)) => return Err(refused()),
        }
        sleep(every);
    }
    Err(expired())
}

fn failed(failure: Failure) -> Fault {
    match failure {
        Failure::Unreachable(fault) => fault,
        Failure::Refused(..) => refused(),
    }
}

fn refused() -> Fault {
    Fault::new(
        code!("pairing.refused"),
        "The server refused to pair this machine",
    )
}

fn expired() -> Fault {
    Fault::new(
        code!("pairing.expired"),
        "Nobody confirmed the code in time; run weacon pair again",
    )
}

fn bad_name() -> Fault {
    Fault::new(
        code!("pairing.name_invalid"),
        "The name must be 1 to 64 characters on one line",
    )
}

fn not_printed() -> Fault {
    Fault::new(
        code!("pairing.output_failed"),
        "The output couldn't be written",
    )
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use tokio::sync::Notify;
    use weacon_protocol::{AccountId, ErrorBody, MachineId, PairingId};

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::local::{Local, answer, read_request};

    fn started() -> PairStarted {
        PairStarted {
            pairing: PairingId::try_from("p-1".to_owned()).unwrap(),
            code: "ABCD-EFGH".into(),
            secret: "s3cret".into(),
            expires_at: 1,
        }
    }

    fn paired() -> PairClaimed {
        PairClaimed::Paired {
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "t0ken".into(),
        }
    }

    fn json(body: &impl Serialize) -> String {
        serde_json::to_string(body).unwrap()
    }

    // Answers each request in turn with a status and body, and gives the requests' bodies.
    async fn serve(local: &Local, answers: Vec<(&str, String)>) -> Vec<String> {
        let mut bodies = Vec::new();
        for (status, body) in answers {
            let mut stream = local.accept().await;
            let (head, sent) = read_request(&mut stream).await;
            bodies.push(format!(
                "{} {sent}",
                head.lines().next().unwrap_or_default()
            ));
            answer(&mut stream, status, &body).await;
        }
        bodies
    }

    struct Outcome {
        done: Result<(), Fault>,
        requests: Vec<String>,
        printed: String,
    }

    // Pairs as `name` with a server that gives `answers`.
    async fn pair_with(
        dir: &Path,
        local: &Local,
        name: Option<&str>,
        answers: Vec<(&str, String)>,
    ) -> Outcome {
        let (dir, url) = (dir.to_path_buf(), local.url());
        let name = name.unwrap_or(platform::MACHINE_NAME).to_owned();
        let paired_up = tokio::task::spawn_blocking(move || {
            let asked = Asked { url: &url, name };
            let mut out = Vec::new();
            let done = pair(&dir, &asked, Duration::ZERO, &mut out);
            (done, out)
        });
        let finished = Notify::new();
        let client = async {
            let joined = paired_up.await;
            finished.notify_one();
            joined
        };
        let server = async {
            let requests = serve(local, answers).await;
            local.refuse_until(&finished).await;
            requests
        };
        let (joined, requests) = tokio::join!(client, server);
        let (done, out) = joined.unwrap();
        Outcome {
            done,
            requests,
            printed: String::from_utf8(out).unwrap(),
        }
    }

    #[tokio::test]
    async fn the_core_shows_its_code_and_keeps_the_pairing_once_confirmed() {
        let (dir, local) = (TempDir::new().unwrap(), Local::new().await);
        let answers = vec![
            ("201 Created", json(&started())),
            ("200 OK", json(&PairClaimed::Waiting)),
            ("200 OK", json(&paired())),
        ];

        let Outcome {
            done,
            requests,
            printed,
        } = pair_with(&dir, &local, Some("Studio"), answers).await;

        done.unwrap();
        let kept = machine::read(&dir).unwrap().unwrap();
        let start = PairStart {
            name: MachineName::try_from("Studio".to_owned()).unwrap(),
            os: platform::MACHINE_OS,
        };
        let claim = r#"{"pairing":"p-1","secret":"s3cret"}"#;
        assert_eq!(
            requests,
            [
                format!("POST /api/machines/pair HTTP/1.1 {}", json(&start)),
                format!("POST /api/machines/pair/claim HTTP/1.1 {claim}"),
                format!("POST /api/machines/pair/claim HTTP/1.1 {claim}"),
            ]
        );
        assert!(printed.contains("ABCD-EFGH"), "{printed}");
        assert_eq!(
            kept,
            Paired {
                server: local.url(),
                machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
                owner: AccountId::try_from("a-1".to_owned()).unwrap(),
                token: "t0ken".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn a_pairing_nobody_confirms_in_time_ends() {
        let (dir, local) = (TempDir::new().unwrap(), Local::new().await);
        let gone = ErrorBody {
            code: code!("pairing.not_found"),
            retryable: false,
        };
        let answers = vec![
            ("201 Created", json(&started())),
            ("404 Not Found", json(&gone)),
        ];

        let outcome = pair_with(&dir, &local, None, answers).await;

        assert_eq!(outcome.done.unwrap_err().code().as_str(), "pairing.expired");
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[tokio::test]
    async fn a_refused_start_or_a_bad_name_pairs_nothing() {
        let (dir, local) = (TempDir::new().unwrap(), Local::new().await);
        let busy = ErrorBody {
            code: code!("pairing.too_many"),
            retryable: true,
        };

        let refused = pair_with(
            &dir,
            &local,
            None,
            vec![("429 Too Many Requests", json(&busy))],
        )
        .await;
        let bad_name = pair_with(&dir, &local, Some(""), Vec::new()).await;

        assert_eq!(refused.done.unwrap_err().code().as_str(), "pairing.refused");
        assert_eq!(
            bad_name.done.unwrap_err().code().as_str(),
            "pairing.name_invalid"
        );
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[tokio::test]
    async fn a_server_that_stops_answering_ends_the_wait() {
        let (dir, local) = (TempDir::new().unwrap(), Local::new().await);
        let mut answers = vec![("201 Created", json(&started()))];
        answers.extend((0..FAILED_CLAIMS).map(|_| ("200 OK", "<html>".to_owned())));

        let outcome = pair_with(&dir, &local, None, answers).await;

        assert_eq!(
            outcome.done.unwrap_err().code().as_str(),
            "server.answer_invalid"
        );
        assert_eq!(outcome.requests.len(), 16);
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[test]
    fn this_systems_name_is_one_the_server_takes() {
        MachineName::try_from(platform::MACHINE_NAME.to_owned()).unwrap();
    }
}
