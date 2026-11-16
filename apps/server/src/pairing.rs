//! Pairing a machine with an account: its core starts and claims, and a signed-in person
//! confirms the code the core shows.

use axum::Json;
use axum::Router;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use weacon_observability::Fault;
use weacon_protocol::{
    MachineId, MachineList, MachineOs, MachineSummary, PairClaim, PairClaimed, PairConfirm,
    PairLookup, PairStart, PairStarted, code, to_hex,
};

use crate::app::{fresh, new_id};
use crate::auth::Session;
use crate::auth::token_hash;
use crate::codes::{new_code, normalised, shown};
use crate::db::{Claim, Claiming, Confirmation, NewPairing};
use crate::http::{Body, error, failure, not_found};
use crate::secret::random_token;
use crate::shared::Shared;
use crate::uplink;

/// How long a person has to confirm a pairing, in seconds.
const PAIRING_LIFETIME: i64 = 600;
/// How long a core has to claim its machine once the person confirms, in seconds.
const CLAIM_WITHIN: i64 = 120;

pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/machines", get(machines))
        .route("/api/machines/{machine}", delete(unpair))
        .route("/api/machines/pair", post(start))
        .route("/api/machines/pair/lookup", post(lookup))
        .route("/api/machines/pair/confirm", post(confirm))
        .route("/api/machines/pair/claim", post(claim))
}

/// `POST /api/machines/pair`: records a machine waiting to be paired, and answers with the
/// code for its person and the secret its core claims it with.
pub(crate) async fn start(State(shared): State<Shared>, Body(body): Body<PairStart>) -> Response {
    if body.os == MachineOs::Newer {
        return error(StatusCode::BAD_REQUEST, code!("pairing.invalid"), false);
    }
    // Answered like a name the protocol refuses, since only a new name can be held to this.
    if body.name.as_str().trim() != body.name.as_str() {
        return error(StatusCode::BAD_REQUEST, code!("request.invalid"), false);
    }
    match started(&shared, &body).await {
        Ok(started) => (StatusCode::CREATED, Json(started)).into_response(),
        Err(fault) => failure(fault),
    }
}

async fn started(shared: &Shared, body: &PairStart) -> Result<PairStarted, Fault> {
    let (code, secret) = (new_code()?, random_token()?);
    let pairing = fresh(&new_id)?;
    let now = shared.now();
    let expires_at = now.saturating_add(PAIRING_LIFETIME);
    let new = NewPairing {
        id: &pairing,
        code_hash: &shared.secret.pairing_code_hash(&code),
        secret_hash: &token_hash(&secret),
        name: &body.name,
        os: body.os,
        expires_at,
    };
    shared.db.start_pairing(&new, now).await?;
    Ok(PairStarted {
        pairing,
        code: shown(&code).into(),
        secret: secret.into(),
        expires_at,
    })
}

/// `POST /api/machines/pair/lookup`: the machine waiting under a code, for the signed-in
/// person to check.
pub(crate) async fn lookup(
    State(shared): State<Shared>,
    _: Session,
    Body(body): Body<PairLookup>,
) -> Response {
    let code_hash = shared
        .secret
        .pairing_code_hash(&normalised(body.code.expose()));
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
    session: Session,
    Body(body): Body<PairConfirm>,
) -> Response {
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
    let now = shared.now();
    let confirmation = Confirmation {
        account: &session.account,
        pairing: &body.pairing,
        code_hash: &shared
            .secret
            .pairing_code_hash(&normalised(body.code.expose())),
        machine: &fresh(&new_id)?,
        claim_by: now.saturating_add(CLAIM_WITHIN),
    };
    shared.db.confirm_pairing(&confirmation, now).await
}

/// `POST /api/machines/pair/claim`: whether the person has confirmed the pairing yet, and
/// once they have, the machine's token, the same for every claim until the pairing expires or
/// the machine is unpaired.
pub(crate) async fn claim(State(shared): State<Shared>, Body(body): Body<PairClaim>) -> Response {
    match claimed(&shared, &body).await {
        Ok(Some(claimed)) => Json(claimed).into_response(),
        Ok(None) => not_found(code!("pairing.not_found")),
        Err(fault) => failure(fault),
    }
}

async fn claimed(shared: &Shared, body: &PairClaim) -> Result<Option<PairClaimed>, Fault> {
    let token = to_hex(&shared.secret.machine_token(body.secret.expose()));
    let claiming = Claiming {
        pairing: &body.pairing,
        secret_hash: &token_hash(body.secret.expose()),
        token_hash: &token_hash(&token),
    };
    let claim = shared.db.claim_pairing(&claiming, shared.now()).await?;
    Ok(match claim {
        Claim::Unknown => None,
        Claim::Waiting => Some(PairClaimed::Waiting),
        Claim::Paired { machine, owner } => Some(PairClaimed::Paired {
            machine,
            owner,
            token: token.into(),
        }),
    })
}

/// `GET /api/machines`: the machines paired with the signed-in account.
pub(crate) async fn machines(State(shared): State<Shared>, session: Session) -> Response {
    match shared
        .db
        .machines_of(&session.account, &uplink::offers())
        .await
    {
        Ok(mut machines) => {
            for machine in &mut machines {
                machine.connected = shared.links.is_open(&machine.id);
            }
            Json(MachineList { machines }).into_response()
        }
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
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => not_found(code!("machine.not_found")),
        Err(fault) => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{
        Fixture, ask, bearer, claim_body, confirm_body, lookup_body, pair_machine, sign_in,
        start_pairing,
    };

    async fn post(address: SocketAddr, path: &str, headers: &str, body: &str) -> (String, Value) {
        ask(address, &format!("POST {path}"), headers, body).await
    }

    #[tokio::test]
    async fn a_person_pairs_the_machine_whose_code_they_enter() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let ada_proof = bearer(&ada);
        let (started, pairing) = start_pairing(server.address, MachineOs::Macos).await;
        let code = &pairing.code;
        let claim = claim_body(&pairing);
        let (_, waiting) = post(server.address, "/api/machines/pair/claim", "", &claim).await;
        let typed_code = code.expose().to_lowercase().replace('-', " ");
        let lookup = lookup_body(&typed_code);
        let (looked, pending) = post(
            server.address,
            "/api/machines/pair/lookup",
            &ada_proof,
            &lookup,
        )
        .await;
        let confirm = confirm_body(&pairing);
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
        assert_eq!(code.expose().len(), 9);
        assert_eq!(code.expose().as_bytes().get(4), Some(&b'-'));
        assert_eq!(pairing.expires_at, fixture.shared.now() + PAIRING_LIFETIME);
        assert_eq!(pairing.secret.expose().len(), 64);
        assert_eq!(waiting, json!({ "state": "waiting" }));
        assert_eq!(looked, "http/1.1 200 ok");
        assert_eq!(
            pending,
            json!({ "pairing": pairing.pairing, "name": "Studio", "os": "macos" })
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
    async fn the_list_says_whether_a_machines_core_is_connected_now() {
        use crate::test_support::socket::{next, open, say};

        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let list = async || {
            ask(server.address, "GET /api/machines", &paired.proof, "")
                .await
                .1
        };
        let offline = list().await;
        let mut core = open(server.address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, &["machine.connect", "symbols.search"]).await;
        // Welcomed only once its connection is open.
        next(&mut core).await;

        assert_eq!(offline["machines"][0].get("connected"), None);
        assert_eq!(list().await["machines"][0]["connected"], json!(true));
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
        let (_, pairing) = start_pairing(server.address, MachineOs::Linux).await;
        let lookup = lookup_body(pairing.code.expose());
        let wrong = json!({ "pairing": pairing.pairing, "code": "0000-0000" }).to_string();
        let right = confirm_body(&pairing);

        let (anonymous, _) = post(server.address, "/api/machines/pair/lookup", "", &lookup).await;
        let (stranger, _) = post(server.address, "/api/machines/pair/confirm", "", &right).await;
        let (_, wrong) = post(
            server.address,
            "/api/machines/pair/confirm",
            &bearer(&ada),
            &wrong,
        )
        .await;
        let bad_secret =
            json!({ "pairing": pairing.pairing, "secret": "ab".repeat(32) }).to_string();
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
    async fn a_confirmed_pairing_is_claimed_only_until_its_deadline() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let (_, ada) = sign_in(&fixture, server.address, "ada@example.com").await;
        let mut claims = Vec::new();
        for _ in 0..2 {
            let (_, pairing) = start_pairing(server.address, MachineOs::Linux).await;
            let confirm = confirm_body(&pairing);
            post(
                server.address,
                "/api/machines/pair/confirm",
                &bearer(&ada),
                &confirm,
            )
            .await;
            claims.push(claim_body(&pairing));
        }

        fixture.advance(CLAIM_WITHIN - 1);
        let (_, in_time) = post(server.address, "/api/machines/pair/claim", "", &claims[0]).await;
        fixture.advance(1);
        let (_, late) = post(server.address, "/api/machines/pair/claim", "", &claims[1]).await;

        assert_eq!(in_time["state"], "paired");
        assert_eq!(late["code"], "pairing.not_found");
    }

    #[tokio::test]
    async fn a_system_this_server_doesnt_know_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;

        let start = json!({ "name": "Studio", "os": "plan9" }).to_string();
        let (status, refusal) = post(server.address, "/api/machines/pair", "", &start).await;

        assert_eq!(status, "http/1.1 400 bad request");
        assert_eq!(refusal["code"], "pairing.invalid");
    }

    #[tokio::test]
    async fn a_blank_or_untrimmed_name_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;

        for name in ["   ", " Studio", "Studio\u{a0}", "Studio\n"] {
            let start = json!({ "name": name, "os": "macos" }).to_string();
            let (status, refusal) = post(server.address, "/api/machines/pair", "", &start).await;

            assert_eq!(status, "http/1.1 400 bad request", "{name:?}");
            assert_eq!(refusal["code"], "request.invalid", "{name:?}");
        }
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
