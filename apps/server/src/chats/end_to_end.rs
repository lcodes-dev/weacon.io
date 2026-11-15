//! The whole loop over the real server: a member's prompt reaches their machine, and what it
//! says of the chat is stored and read by the workspace only.

use weacon_protocol::{
    ChatAnswerKind, ChatEntry, ChatEvent, ChatQuery, ChatQueryKind, ChatUpdate, Envelope,
    ErrorBody, MachineId, Outcome, Receipt,
};

use crate::db::test_support::{prompt, said};
use futures_util::SinkExt as _;
use tokio_tungstenite::tungstenite::Message as Wire;

use crate::test_support::socket::{self, Socket, next, open, past_pings, say, upgrade_status};
use crate::test_support::{
    Fixture, Running, email, fresh_account, member_id, pair_machine, sign_in, workspace_id,
};

use weacon_protocol::Message;

use crate::db::test_support::id;

const OFFERS: [&str; 3] = [
    "workspace.feed",
    "workspace.chats",
    "workspace.chat_commands",
];

async fn token(fixture: &Fixture, server: &Running, address: &str) -> String {
    let (_, body) = sign_in(fixture, server.address, address).await;
    body["token"].as_str().unwrap().to_owned()
}

async fn feed(server: &Running, workspace: &str, token: &str) -> Socket {
    let path = format!("/api/workspaces/{workspace}/feed");
    let mut client = open(server.address, &path, token).await.unwrap();
    say(&mut client, &OFFERS).await;
    assert!(matches!(next(&mut client).await, Message::Welcome(_)));
    client
}

async fn ask(client: &mut Socket, id: u32, message: Message) -> Message {
    socket::send(client, Envelope { id, message }).await;
    heard(client).await
}

// The next message past the workspace's own changes.
async fn heard(client: &mut Socket) -> Message {
    loop {
        let message = next(client).await;
        if !matches!(message, Message::Workspace(_)) {
            return message;
        }
    }
}

fn chat_query(workspace: &str, query: ChatQueryKind) -> Message {
    Message::ChatQuery(ChatQuery {
        workspace: id(workspace),
        chat: id("c-1"),
        query,
    })
}

fn refusal(message: &Message) -> &str {
    let Message::Error(ErrorBody { code, .. }) = message else {
        panic!("not refused: {message:?}");
    };
    code.as_str()
}

fn uploaded(seq: u64, event: ChatEvent) -> Envelope {
    let update = ChatUpdate {
        workspace: id("w1"),
        chat: id("c-1"),
        entry: ChatEntry { seq, event },
    };
    Envelope {
        id: 0,
        message: Message::Chat(update),
    }
}

// Bo, a member of w1 besides Ada, and Cy, a member of w2 only.
async fn others(fixture: &Fixture) {
    let db = &fixture.shared.db;
    let one = db.workspace(&id("w1")).await.unwrap();
    let bo = email("bo@example.com");
    one.add_member(&member_id("m3"), &fresh_account(), &bo)
        .await
        .unwrap();
    let two = db
        .create_workspace(&workspace_id("w2"), "Two")
        .await
        .unwrap();
    let cy = email("cy@example.com");
    two.add_member(&member_id("m4"), &fresh_account(), &cy)
        .await
        .unwrap();
}

// Ada's machine, connected and serving chats, and her feed of w1.
async fn connected(fixture: &Fixture, server: &Running) -> (MachineId, Socket, Socket) {
    let paired = pair_machine(fixture, server.address).await;
    let mut core = open(server.address, "/api/machines/connect", &paired.token)
        .await
        .unwrap();
    say(&mut core, &["machine.connect", "chat.serve"]).await;
    assert!(matches!(next(&mut core).await, Message::Welcome(_)));
    let ada = feed(server, "w1", &paired.session).await;
    (paired.machine_id(), core, ada)
}

// Ada's first prompt reaches her machine, and what it says of the chat reaches her feed.
async fn ada_chats(machine: &MachineId, core: &mut Socket, ada: &mut Socket) {
    let first = prompt("r-1", "c-1", "MARKER-PROMPT", Some(machine));
    let answer = ask(ada, 2, Message::ChatCommand(first)).await;
    let receipt = Receipt {
        request: id("r-1"),
        outcome: Outcome::Accepted,
    };
    assert_eq!(answer, Message::Receipt(receipt));
    let relayed = next(core).await;
    let Message::ChatCommand(relayed) = relayed else {
        panic!("the machine wasn't asked: {relayed:?}");
    };
    assert_eq!(relayed.by.map(|by| by.member), Some(member_id("m1")));
    assert!(matches!(next(core).await, Message::ChatQuery(_)));
    let follow = chat_query("w1", ChatQueryKind::Subscribe { after: 0 });
    socket::send(
        ada,
        Envelope {
            id: 3,
            message: follow,
        },
    )
    .await;
    let typed = ChatEvent::OutsidePrompt {
        text: "MARKER-PROMPT".to_owned(),
    };
    socket::send(core, uploaded(1, typed)).await;
    socket::send(core, uploaded(2, said("MARKER-REPLY"))).await;
    let heard = [chat_event(ada).await, chat_event(ada).await];
    assert_eq!(heard.map(|entry| entry.seq), [1, 2]);
}

async fn chat_event(client: &mut Socket) -> ChatEntry {
    loop {
        if let Message::Chat(update) = heard(client).await {
            return update.entry;
        }
    }
}

#[tokio::test]
async fn a_prompt_runs_on_its_owners_machine_and_only_the_workspace_reads_the_chat() {
    let fixture = Fixture::new().await;
    let server = fixture.run().await;
    others(&fixture).await;
    let (machine, mut core, mut ada) = connected(&fixture, &server).await;

    ada_chats(&machine, &mut core, &mut ada).await;

    let bo_token = token(&fixture, &server, "bo@example.com").await;
    let mut bo = feed(&server, "w1", &bo_token).await;
    let read = ask(&mut bo, 2, chat_query("w1", ChatQueryKind::Snapshot)).await;
    let snapshot = matches!(&read, Message::ChatAnswer(read)
        if matches!(read.answer, ChatAnswerKind::Snapshot { seq: 2, .. }));
    assert!(snapshot, "{read:?}");
    let asked = prompt("r-2", "c-1", "MARKER-PROMPT", None);
    let refused = ask(&mut bo, 3, Message::ChatCommand(asked)).await;
    assert_eq!(refusal(&refused), "chat.not_owner");
    let cy_token = token(&fixture, &server, "cy@example.com").await;
    let w1 = open(server.address, "/api/workspaces/w1/feed", &cy_token).await;
    assert_eq!(upgrade_status(w1), 401);
    let mut cy = feed(&server, "w2", &cy_token).await;
    let elsewhere = ask(&mut cy, 2, chat_query("w1", ChatQueryKind::Snapshot)).await;
    assert_eq!(refusal(&elsewhere), "chat.not_found");
    let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
    assert!(!logged.contains("MARKER"), "{logged}");
}

// The answer to `message`, which must come before the pong to a ping sent after it.
async fn answered_at_once(client: &mut Socket, message: Message) -> Message {
    socket::send(client, Envelope { id: 2, message }).await;
    client.send(Wire::Ping(Vec::new().into())).await.unwrap();
    let mut answer = None;
    loop {
        match past_pings(client).await {
            Some(Ok(Wire::Pong(_))) => return answer.expect("answered only after the ping"),
            Some(Ok(Wire::Text(text))) => {
                let message = Envelope::from_json(text.as_bytes()).unwrap().message;
                if !matches!(message, Message::Workspace(_)) {
                    answer = answer.or(Some(message));
                }
            }
            other => panic!("no answer: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_feed_answers_a_chat_query_before_what_its_client_sends_next() {
    let fixture = Fixture::new().await;
    let server = fixture.run().await;
    let ada = token(&fixture, &server, "ada@example.com").await;
    let mut client = feed(&server, "w1", &ada).await;

    let answer = answered_at_once(&mut client, chat_query("w1", ChatQueryKind::Snapshot)).await;

    assert_eq!(refusal(&answer), "chat.not_found");
}
