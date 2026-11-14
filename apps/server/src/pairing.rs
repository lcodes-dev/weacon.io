//! Pairing a machine with an account: its core starts and claims, and a signed-in person
//! confirms the code the core shows.

use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use tokio::time::Instant;
use weacon_observability::Fault;
use weacon_protocol::{
    MachineId, MachineList, MachineOs, MachineSummary, PairClaim, PairClaimed, PairConfirm,
    PairLookup, PairStart, PairStarted, code, to_hex,
};

use crate::app::{fresh, new_id};
use crate::auth::Session;
use crate::auth::token_hash;
use crate::client::Client;
use crate::db::{Claim, Confirmation, NewPairing};
use crate::http::{Body, error, failure, not_found, too_many};
use crate::limit::{HOUR, Limit};
use crate::secret::{random_token, random_u64};
use crate::shared::Shared;

/// How long a person has to confirm a pairing, in seconds.
const PAIRING_LIFETIME: i64 = 600;
/// How many pairings may be under way at once, server-wide, as anyone may start one.
const MOST_UNDER_WAY: i64 = 1000;
/// Crockford's base 32: no I, L, O or U, which read like other letters.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// How many pairings each client may start.
pub(crate) fn starts() -> Limit {
    Limit::new(5, HOUR)
}

/// How many codes each client may look up or confirm.
pub(crate) fn tries() -> Limit {
    Limit::new(60, HOUR)
}

/// How many claims each client may make; a core claims every few seconds while it waits.
pub(crate) fn claims() -> Limit {
    Limit::new(1200, HOUR)
}

/// How often each client may ask each pairing step.
pub(crate) struct Limits {
    pub(crate) starts: Limit,
    pub(crate) tries: Limit,
    pub(crate) claims: Limit,
}

pub(crate) fn limits() -> Limits {
    Limits {
        starts: starts(),
        tries: tries(),
        claims: claims(),
    }
}

/// `POST /api/machines/pair`: records a machine waiting to be paired, and answers with the
/// code for its person and the secret its core claims it with.
pub(crate) async fn start(
    State(shared): State<Shared>,
    Client(from): Client,
    Body(body): Body<PairStart>,
) -> Response {
    if !shared.pairing.starts.allow(from, Instant::now()) {
        return too_many(code!("pairing.too_many"));
    }
    if body.os == MachineOs::Newer {
        return error(StatusCode::BAD_REQUEST, code!("pairing.invalid"), false);
    }
    match started(&shared, &body).await {
        Ok(Some(started)) => (StatusCode::CREATED, Json(started)).into_response(),
        Ok(None) => too_many(code!("pairing.too_many")),
        Err(fault) => failure(fault),
    }
}

async fn started(shared: &Shared, body: &PairStart) -> Result<Option<PairStarted>, Fault> {
    let (code, secret) = (code_text(random_u64()?), random_token()?);
    let pairing = fresh(&new_id)?;
    let now = shared.now();
    let expires_at = now.saturating_add(PAIRING_LIFETIME);
    let new = NewPairing {
        id: &pairing,
        code_hash: &shared.secret.pairing_code_hash(&code),
        secret_hash: &token_hash(&secret),
        name: &body.name,
        os: body.os,
        key: &body.key,
        expires_at,
    };
    let recorded = shared.db.start_pairing(&new, now, MOST_UNDER_WAY).await?;
    Ok(recorded.then(|| PairStarted {
        pairing,
        code: shown(&code),
        secret,
        expires_at,
    }))
}

/// `POST /api/machines/pair/lookup`: the machine waiting under a code, for the signed-in
/// person to check.
pub(crate) async fn lookup(
    State(shared): State<Shared>,
    Client(from): Client,
    _: Session,
    Body(body): Body<PairLookup>,
) -> Response {
    if !shared.pairing.tries.allow(from, Instant::now()) {
        return too_many(code!("pairing.too_many"));
    }
    let code_hash = shared.secret.pairing_code_hash(&normalised(&body.code));
    match shared.db.pending_pairing(&code_hash, shared.now()).await {
        Ok(Some(pending)) => Json(pending).into_response(),
        Ok(None) => not_found(code!("pairing.not_found")),
        Err(fault) => failure(fault),
    }
}

/// `POST /api/machines/pair/confirm`: pairs the machine waiting under a code with the
/// signed-in account.
pub(crate) async fn confirm(
    State(shared): State<Shared>,
    Client(from): Client,
    session: Session,
    Body(body): Body<PairConfirm>,
) -> Response {
    if !shared.pairing.tries.allow(from, Instant::now()) {
        return too_many(code!("pairing.too_many"));
    }
    match confirmed(&shared, &session, &body).await {
        Ok(Some(machine)) => (StatusCode::CREATED, Json(machine)).into_response(),
        Ok(None) => not_found(code!("pairing.not_found")),
        Err(fault) => failure(fault),
    }
}

async fn confirmed(
    shared: &Shared,
    session: &Session,
    body: &PairConfirm,
) -> Result<Option<MachineSummary>, Fault> {
    let confirmation = Confirmation {
        account: &session.account,
        pairing: &body.pairing,
        code_hash: &shared.secret.pairing_code_hash(&normalised(&body.code)),
        machine: &fresh(&new_id)?,
    };
    shared.db.confirm_pairing(&confirmation, shared.now()).await
}

/// `POST /api/machines/pair/claim`: whether the person has confirmed the pairing yet, and
/// once they have, the machine's token, only this once.
pub(crate) async fn claim(
    State(shared): State<Shared>,
    Client(from): Client,
    Body(body): Body<PairClaim>,
) -> Response {
    if !shared.pairing.claims.allow(from, Instant::now()) {
        return too_many(code!("pairing.too_many"));
    }
    match claimed(&shared, &body).await {
        Ok(Some(claimed)) => Json(claimed).into_response(),
        Ok(None) => not_found(code!("pairing.not_found")),
        Err(fault) => failure(fault),
    }
}

async fn claimed(shared: &Shared, body: &PairClaim) -> Result<Option<PairClaimed>, Fault> {
    let token = to_hex(&shared.secret.machine_token(&body.secret));
    let claim = shared
        .db
        .claim_pairing(
            &body.pairing,
            &token_hash(&body.secret),
            &token_hash(&token),
            shared.now(),
        )
        .await?;
    Ok(match claim {
        Claim::Unknown => None,
        Claim::Waiting => Some(PairClaimed::Waiting),
        Claim::Paired { machine, owner } => Some(PairClaimed::Paired {
            machine,
            owner,
            token,
        }),
    })
}

/// `GET /api/machines`: the machines paired with the signed-in account.
pub(crate) async fn machines(State(shared): State<Shared>, session: Session) -> Response {
    match shared.db.machines_of(&session.account).await {
        Ok(machines) => Json(MachineList { machines }).into_response(),
        Err(fault) => failure(fault),
    }
}

/// `DELETE /api/machines/{machine}`: unpairs one of the signed-in account's machines.
pub(crate) async fn unpair(
    State(shared): State<Shared>,
    session: Session,
    machine: Result<Path<MachineId>, PathRejection>,
) -> Response {
    // An id that isn't one names no machine either.
    let Ok(Path(machine)) = machine else {
        return not_found(code!("machine.not_found"));
    };
    match shared.db.unpair(&session.account, &machine).await {
        Ok(true) => {
            // Nobody listening is fine: a connection also checks at each ping.
            drop(shared.unpaired.send(machine));
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found(code!("machine.not_found")),
        Err(fault) => failure(fault),
    }
}

/// A code of eight letters from [`ALPHABET`]: 40 random bits.
fn code_text(word: u64) -> String {
    (0..8)
        .filter_map(|place| {
            let index = usize::try_from((word >> (5 * place)) & 31).ok()?;
            ALPHABET.get(index).copied().map(char::from)
        })
        .collect()
}

/// A code as its person reads it: `ABCD-EFGH`.
fn shown(code: &str) -> String {
    let (first, second) = code.split_at_checked(4).unwrap_or((code, ""));
    format!("{first}-{second}")
}

/// A code as a person typed it, read as its letters: any case, with or without spaces and
/// dashes, and the letters that look like digits taken for them.
fn normalised(code: &str) -> String {
    code.chars()
        .filter(|letter| !letter.is_whitespace() && *letter != '-')
        .map(|letter| match letter.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::{Value, json};

    use std::sync::Arc;

    use super::*;
    use crate::test_support::{Fixture, ask, bearer, pair_machine, pairing_start, sign_in};

    async fn start_pairing(address: SocketAddr, os: &str) -> (String, Value) {
        ask(address, "POST /api/machines/pair", "", &pairing_start(os)).await
    }

    async fn post(address: SocketAddr, path: &str, headers: &str, body: &Value) -> (String, Value) {
        ask(address, &format!("POST {path}"), headers, &body.to_string()).await
    }

    #[test]
    fn codes_are_eight_letters_read_as_typed() {
        assert_eq!(code_text(0), "00000000");
        assert_eq!(code_text(u64::MAX), "ZZZZZZZZ");
        assert_eq!(code_text(u64::from_le_bytes([1; 8])), "1802G040");
        assert_eq!(shown("ABCDEFGH"), "ABCD-EFGH");
        assert_eq!(normalised(" abcd-efgh "), "ABCDEFGH");
        assert_eq!(normalised("oO iI lL"), "001111");
    }

    #[tokio::test]
    async fn a_person_pairs_the_machine_whose_code_they_enter() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let ada_proof = bearer(&ada);
        let (started, pairing) = start_pairing(server.address, "macos").await;
        let code = pairing["code"].as_str().unwrap().to_owned();
        let claim = json!({ "pairing": pairing["pairing"], "secret": pairing["secret"] });
        let (_, waiting) = post(server.address, "/api/machines/pair/claim", "", &claim).await;
        let typed_code = code.to_lowercase().replace('-', " ");
        let lookup = json!({ "code": typed_code });
        let (looked, pending) = post(
            server.address,
            "/api/machines/pair/lookup",
            &ada_proof,
            &lookup,
        )
        .await;
        let confirm = json!({ "pairing": pairing["pairing"], "code": code });
        let (confirmed, machine) = post(
            server.address,
            "/api/machines/pair/confirm",
            &ada_proof,
            &confirm,
        )
        .await;
        let (_, paired) = post(server.address, "/api/machines/pair/claim", "", &claim).await;
        let (_, again) = post(server.address, "/api/machines/pair/claim", "", &claim).await;
        let (_, list) = ask(server.address, "GET /api/machines", &ada_proof, "").await;

        assert_eq!(started, "http/1.1 201 created");
        assert_eq!(code.len(), 9);
        assert_eq!(code.as_bytes().get(4), Some(&b'-'));
        assert_eq!(
            pairing["expires_at"],
            fixture.shared.now() + PAIRING_LIFETIME
        );
        assert_eq!(pairing["secret"].as_str().unwrap().len(), 64);
        assert_eq!(waiting, json!({ "state": "waiting" }));
        assert_eq!(looked, "http/1.1 200 ok");
        assert_eq!(
            pending,
            json!({ "pairing": pairing["pairing"], "name": "Studio", "os": "macos", "key": "ab".repeat(32) })
        );
        assert_eq!(confirmed, "http/1.1 201 created");
        assert_eq!(machine["name"], "Studio");
        assert_eq!(paired["state"], "paired");
        assert_eq!(paired["machine"], machine["id"]);
        assert_eq!(paired["owner"], ada["account"]);
        assert_eq!(paired["token"].as_str().unwrap().len(), 64);
        assert_eq!(again, paired);
        assert_eq!(list, json!({ "machines": [machine] }));
    }

    #[tokio::test]
    async fn a_person_unpairs_their_machine() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        assert_eq!((paired.session.len(), paired.token.len()), (64, 64));
        let (ada_proof, machine) = (paired.proof, paired.machine);
        let path = format!("DELETE /api/machines/{}", machine["id"].as_str().unwrap());
        let (unpaired, _) = ask(server.address, &path, &ada_proof, "").await;
        let (twice, refusal) = ask(server.address, &path, &ada_proof, "").await;
        let (_, list) = ask(server.address, "GET /api/machines", &ada_proof, "").await;

        assert_eq!(unpaired, "http/1.1 204 no content");
        assert_eq!(
            (twice.as_str(), &refusal["code"]),
            ("http/1.1 404 not found", &json!("machine.not_found"))
        );
        assert_eq!(list, json!({ "machines": [] }));
    }

    #[tokio::test]
    async fn only_a_signed_in_person_with_the_right_code_confirms() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let (_, pairing) = start_pairing(server.address, "linux").await;
        let code = pairing["code"].clone();
        let lookup = json!({ "code": code });
        let wrong = json!({ "pairing": pairing["pairing"], "code": "0000-0000" });
        let right = json!({ "pairing": pairing["pairing"], "code": code });

        let (anonymous, _) = post(server.address, "/api/machines/pair/lookup", "", &lookup).await;
        let (stranger, _) = post(server.address, "/api/machines/pair/confirm", "", &right).await;
        let (_, wrong) = post(
            server.address,
            "/api/machines/pair/confirm",
            &bearer(&ada),
            &wrong,
        )
        .await;
        let bad_secret = json!({ "pairing": pairing["pairing"], "secret": "ab".repeat(32) });
        let (_, guessed) = post(server.address, "/api/machines/pair/claim", "", &bad_secret).await;
        fixture.advance(PAIRING_LIFETIME);
        let (_, late) = post(
            server.address,
            "/api/machines/pair/lookup",
            &bearer(&ada),
            &lookup,
        )
        .await;
        let (_, too_late) = post(
            server.address,
            "/api/machines/pair/confirm",
            &bearer(&ada),
            &right,
        )
        .await;

        assert_eq!(anonymous, "http/1.1 401 unauthorized");
        assert_eq!(stranger, "http/1.1 401 unauthorized");
        for refusal in [wrong, guessed, late, too_late] {
            assert_eq!(refusal["code"], "pairing.not_found");
        }
    }

    #[tokio::test]
    async fn a_system_this_server_doesnt_know_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;

        let (status, refusal) = start_pairing(server.address, "plan9").await;

        assert_eq!(status, "http/1.1 400 bad request");
        assert_eq!(refusal["code"], "pairing.invalid");
    }

    #[tokio::test]
    async fn each_client_starts_only_so_many_pairings() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        for _ in 0..5 {
            let (status, _) = start_pairing(server.address, "linux").await;
            assert_eq!(status, "http/1.1 201 created");
        }

        let (status, refusal) = start_pairing(server.address, "linux").await;

        assert_eq!(status, "http/1.1 429 too many requests");
        assert_eq!(refusal["code"], "pairing.too_many");
    }

    #[tokio::test]
    async fn each_client_tries_and_claims_only_so_many_times() {
        let mut fixture = Fixture::new().await;
        fixture.shared.pairing = Arc::new(Limits {
            starts: starts(),
            tries: Limit::new(2, HOUR),
            claims: Limit::new(1, HOUR),
        });
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let (_, pairing) = start_pairing(server.address, "linux").await;
        let lookup = json!({ "code": pairing["code"] });
        let confirm = json!({ "pairing": pairing["pairing"], "code": pairing["code"] });
        let claim = json!({ "pairing": pairing["pairing"], "secret": pairing["secret"] });
        let mut answers = Vec::new();
        for (path, body) in [
            ("/api/machines/pair/lookup", &lookup),
            ("/api/machines/pair/confirm", &confirm),
            ("/api/machines/pair/lookup", &lookup),
            ("/api/machines/pair/claim", &claim),
            ("/api/machines/pair/claim", &claim),
        ] {
            answers.push(post(server.address, path, &bearer(&ada), body).await);
        }
        let statuses: Vec<_> = answers.iter().map(|(status, _)| status.as_str()).collect();

        assert_eq!(
            statuses,
            [
                "http/1.1 200 ok",
                "http/1.1 201 created",
                "http/1.1 429 too many requests",
                "http/1.1 200 ok",
                "http/1.1 429 too many requests",
            ]
        );
        assert_eq!(answers[2].1["code"], "pairing.too_many");
        assert_eq!(answers[4].1["code"], "pairing.too_many");
    }

    #[tokio::test]
    async fn a_malformed_body_or_id_is_answered_as_an_error() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let path = format!("DELETE /api/machines/{}", "a".repeat(65));

        let (bad_body, refusal) = ask(server.address, "POST /api/machines/pair", "", "{").await;
        let (bad_id, missing) = ask(server.address, &path, &bearer(&ada), "").await;

        assert_eq!(bad_body, "http/1.1 400 bad request");
        assert_eq!(refusal["code"], "request.invalid");
        assert_eq!(bad_id, "http/1.1 404 not found");
        assert_eq!(missing["code"], "machine.not_found");
    }
}
