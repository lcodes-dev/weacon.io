//! `weacon pair`: pairs this machine with the account of whoever confirms its code in the
//! web app.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use hyper::StatusCode;
use weacon_observability::Fault;
use weacon_protocol::{
    AccountId, MachineId, MachineName, PairClaim, PairClaimed, PairStart, PairStarted, code,
};

use crate::app::{App, state_unavailable};
use crate::daemon::runtime;
use crate::machine::{self, Key, Paired};
use crate::platform;
use crate::server::{Failure, Server};

/// How often the core asks whether the person has confirmed.
const CLAIM_EVERY: Duration = Duration::from_secs(2);

/// What `weacon pair` was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Pairing {
    pub(crate) server: String,
    pub(crate) name: Option<String>,
    /// The certificates to trust for the server in place of the public roots, as PEM.
    pub(crate) pinned: Option<String>,
}

/// Reads the PEM file `pin`, if one was given.
///
/// # Errors
/// When it can't be read.
pub(crate) fn pinned(pin: Option<&Path>) -> Result<Option<String>, Fault> {
    pin.map(|path| std::fs::read_to_string(path).map_err(|error| bad_pin().because(error)))
        .transpose()
}

/// Pairs this machine, printing the code to confirm and the key's fingerprint to `out`.
///
/// # Errors
/// When the server can't be reached or refuses, the person doesn't confirm in time, or the
/// pairing can't be kept.
pub(crate) fn run(app: &App, pairing: &Pairing, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let key = Key::from_random(random_key()?);
    runtime()?.block_on(pair(state_dir, pairing, key, CLAIM_EVERY, out))
}

async fn pair(
    state_dir: &Path,
    pairing: &Pairing,
    key: Key,
    every: Duration,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let pinned = pairing.pinned.clone();
    let server = Server::new(&pairing.server, pinned.as_deref().map(str::as_bytes))?;
    let name = pairing
        .name
        .clone()
        .unwrap_or_else(|| platform::MACHINE_NAME.to_owned());
    let start = PairStart {
        name: MachineName::try_from(name).map_err(|_| bad_name())?,
        os: platform::MACHINE_OS,
        key: key.public()?,
    };
    let started: PairStarted = server
        .post("/api/machines/pair", &start)
        .await
        .map_err(failed)?;
    let fingerprint = key.fingerprint()?;
    writeln!(
        out,
        "Enter this code in Weacon to pair this machine: {}\nCheck that Weacon shows this key: {fingerprint}\nWaiting for you to confirm…",
        started.code
    )
    .map_err(|error| not_printed().because(error))?;
    let claim = PairClaim {
        pairing: started.pairing,
        secret: started.secret,
    };
    let (machine, owner, token) = claimed(&server, &claim, every).await?;
    let paired = Paired {
        server: pairing.server.clone(),
        pinned,
        machine,
        owner,
        token,
        key,
    };
    machine::write(state_dir, &paired)?;
    writeln!(
        out,
        "Paired. The core connects to Weacon while its daemon runs."
    )
    .map_err(|error| not_printed().because(error))
}

/// Claims the pairing every `every` until the person confirms it, or it ends.
async fn claimed(
    server: &Server,
    claim: &PairClaim,
    every: Duration,
) -> Result<(MachineId, AccountId, String), Fault> {
    loop {
        match server.post("/api/machines/pair/claim", claim).await {
            Ok(PairClaimed::Paired {
                machine,
                owner,
                token,
            }) => return Ok((machine, owner, token)),
            Ok(PairClaimed::Waiting) | Err(Failure::Unreachable(_)) => {}
            Ok(PairClaimed::Newer) => return Err(failed(Failure::Refused(StatusCode::OK))),
            Err(Failure::Refused(StatusCode::NOT_FOUND)) => return Err(expired()),
            Err(refused) => return Err(failed(refused)),
        }
        tokio::time::sleep(every).await;
    }
}

fn random_key() -> Result<[u8; 32], Fault> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes)
        .map_err(|_| Fault::new(code!("random.failed"), "The core couldn't get random bytes"))?;
    Ok(bytes)
}

fn failed(failure: Failure) -> Fault {
    match failure {
        Failure::Unreachable(fault) => fault,
        Failure::Refused(..) => Fault::new(
            code!("pairing.refused"),
            "The server refused to pair this machine",
        ),
    }
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

fn bad_pin() -> Fault {
    Fault::new(
        code!("server.pin_unreadable"),
        "The pinned certificate file can't be read",
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
    use std::fs;
    use weacon_protocol::{ErrorBody, MachineOs, PairingId};

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::tls::{Tls, answer, read_request};

    fn started() -> PairStarted {
        PairStarted {
            pairing: PairingId::try_from("p-1".to_owned()).unwrap(),
            code: "ABCD-EFGH".to_owned(),
            secret: "s3cret".to_owned(),
            expires_at: 1,
        }
    }

    fn paired() -> PairClaimed {
        PairClaimed::Paired {
            machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
            token: "t0ken".to_owned(),
        }
    }

    fn json(body: &impl Serialize) -> String {
        serde_json::to_string(body).unwrap()
    }

    // Answers each request in turn with a status and body, and gives the requests' bodies.
    async fn serve(tls: &Tls, answers: Vec<(&str, String)>) -> Vec<String> {
        let mut bodies = Vec::new();
        for (status, body) in answers {
            let mut stream = tls.accept().await.unwrap();
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

    // Pairs as `name` with a server that gives `answers`, pinning its certificate.
    async fn pair_with(
        dir: &Path,
        tls: &Tls,
        name: Option<&str>,
        answers: Vec<(&str, String)>,
    ) -> Outcome {
        let pairing = Pairing {
            server: tls.url(),
            name: name.map(str::to_owned),
            pinned: Some(tls.pem.clone()),
        };
        let mut out = Vec::new();
        let key = Key::from_random([7; 32]);
        let paired_up = pair(dir, &pairing, key, Duration::ZERO, &mut out);
        let (done, requests) = tokio::join!(paired_up, serve(tls, answers));
        Outcome {
            done,
            requests,
            printed: String::from_utf8(out).unwrap(),
        }
    }

    #[tokio::test]
    async fn the_core_shows_its_code_and_key_and_keeps_the_pairing_once_confirmed() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let answers = vec![
            ("201 Created", json(&started())),
            ("200 OK", json(&PairClaimed::Waiting)),
            ("200 OK", json(&paired())),
        ];

        let Outcome {
            done,
            requests,
            printed,
        } = pair_with(&dir, &tls, Some("Studio"), answers).await;

        done.unwrap();
        let kept = machine::read(&dir).unwrap().unwrap();
        let key = Key::from_random([7; 32]);
        let start = PairStart {
            name: MachineName::try_from("Studio".to_owned()).unwrap(),
            os: platform::MACHINE_OS,
            key: key.public().unwrap(),
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
        assert!(printed.contains(&key.fingerprint().unwrap()), "{printed}");
        assert_eq!(
            kept,
            Paired {
                server: tls.url(),
                pinned: Some(tls.pem.clone()),
                machine: MachineId::try_from("mc-1".to_owned()).unwrap(),
                owner: AccountId::try_from("a-1".to_owned()).unwrap(),
                token: "t0ken".to_owned(),
                key,
            }
        );
    }

    #[tokio::test]
    async fn a_pairing_nobody_confirms_in_time_ends() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let gone = ErrorBody {
            code: code!("pairing.not_found"),
            retryable: false,
        };
        let answers = vec![
            ("201 Created", json(&started())),
            ("404 Not Found", json(&gone)),
        ];

        let outcome = pair_with(&dir, &tls, None, answers).await;

        assert_eq!(outcome.done.unwrap_err().code().as_str(), "pairing.expired");
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[tokio::test]
    async fn a_refused_start_or_a_bad_name_pairs_nothing() {
        let (dir, tls) = (TempDir::new().unwrap(), Tls::new().await);
        let busy = ErrorBody {
            code: code!("pairing.too_many"),
            retryable: true,
        };

        let refused = pair_with(
            &dir,
            &tls,
            None,
            vec![("429 Too Many Requests", json(&busy))],
        )
        .await;
        let bad_name = pair_with(&dir, &tls, Some(""), Vec::new()).await;

        assert_eq!(refused.done.unwrap_err().code().as_str(), "pairing.refused");
        assert_eq!(
            bad_name.done.unwrap_err().code().as_str(),
            "pairing.name_invalid"
        );
        assert_eq!(machine::read(&dir).unwrap(), None);
    }

    #[test]
    fn a_pin_is_read_from_its_file() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("server.pem"), "PEM").unwrap();

        assert_eq!(pinned(None).unwrap(), None);
        assert_eq!(
            pinned(Some(&dir.join("server.pem"))).unwrap().as_deref(),
            Some("PEM")
        );
        assert_eq!(
            pinned(Some(&dir.join("missing.pem")))
                .unwrap_err()
                .code()
                .as_str(),
            "server.pin_unreadable"
        );
    }

    #[test]
    fn this_system_is_one_the_server_knows() {
        assert_ne!(platform::MACHINE_OS, MachineOs::Newer);
        MachineName::try_from(platform::MACHINE_NAME.to_owned()).unwrap();
    }
}
