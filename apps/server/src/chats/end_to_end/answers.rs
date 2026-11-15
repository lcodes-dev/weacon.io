//! What a core answers to a chat command it refuses, as the chat's feeds hear it.

use super::*;

// The envelope Ada's machine is sent next past the follows, and what it holds.
async fn command_sent(core: &mut Socket) -> u32 {
    loop {
        let envelope = next_envelope(core).await;
        if matches!(envelope.message, Message::ChatCommand(_)) {
            return envelope.id;
        }
    }
}

// The outcome of the next receipt `client` hears in the chat.
async fn receipt_heard(client: &mut Socket) -> (String, Outcome) {
    loop {
        if let ChatEvent::Receipt { receipt } = chat_event(client).await.event {
            return (receipt.request.as_str().to_owned(), receipt.outcome);
        }
    }
}

fn failed(code: weacon_protocol::Code) -> Outcome {
    Outcome::Failed { code }
}

// Ada's first prompt, sent to her machine, and her feed following its chat: the envelope id
// it went in, and the fixture, server, machine and feed it runs on.
async fn asked() -> (Fixture, Running, Socket, Socket, u32) {
    let fixture = Fixture::new().await;
    let server = fixture.run().await;
    let (machine, mut core, mut ada) = connected(&fixture, &server).await;
    let first = prompt("r-1", "c-1", "MARKER-PROMPT", Some(&machine));
    ask(&mut ada, 2, Message::ChatCommand(first)).await;
    let sent = command_sent(&mut core).await;
    let follow = chat_query("w1", ChatQueryKind::Subscribe { after: 0 });
    socket::send(
        &mut ada,
        Envelope {
            id: 3,
            message: follow,
        },
    )
    .await;
    (fixture, server, core, ada, sent)
}

#[tokio::test]
async fn a_command_its_core_refuses_ends_in_its_core_s_failed_receipt() {
    let (fixture, _server, mut core, mut ada, sent) = asked().await;
    let busy = Receipt {
        request: id("r-1"),
        outcome: failed(code!("chat.busy")),
    };

    let answer = Envelope {
        id: sent,
        message: Message::Receipt(busy),
    };
    socket::send(&mut core, answer).await;

    let heard = receipt_heard(&mut ada).await;
    assert_eq!(heard, ("r-1".to_owned(), failed(code!("chat.busy"))));
    let w1 = fixture.shared.db.workspace(&id("w1")).await.unwrap();
    assert!(w1.open_requests(&id("c-1")).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_command_its_core_cant_do_ends_in_a_failed_receipt_with_its_code() {
    let (_fixture, _server, mut core, mut ada, sent) = asked().await;
    let conflict = ErrorBody {
        code: code!("chat.repo_conflict"),
        retryable: false,
    };

    let answer = Envelope {
        id: sent,
        message: Message::Error(conflict),
    };
    socket::send(&mut core, answer).await;

    let heard = receipt_heard(&mut ada).await;
    assert_eq!(
        heard,
        ("r-1".to_owned(), failed(code!("chat.repo_conflict")))
    );
}

#[tokio::test]
async fn a_command_its_core_has_no_room_for_yet_stays_open_and_its_link_closes() {
    let (fixture, _server, mut core, _ada, sent) = asked().await;
    let for_now = ErrorBody {
        code: code!("chat.unavailable"),
        retryable: true,
    };

    let answer = Envelope {
        id: sent,
        message: Message::Error(for_now),
    };
    socket::send(&mut core, answer).await;

    // Closed before it reads the ping after the refusal, to be sent it again.
    assert!(!socket::pongs(&mut core).await);
    let w1 = fixture.shared.db.workspace(&id("w1")).await.unwrap();
    assert_eq!(w1.open_requests(&id("c-1")).await.unwrap().len(), 1);
}
