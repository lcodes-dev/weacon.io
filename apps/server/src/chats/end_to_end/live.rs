//! A run's live updates over the real server: who hears them, and that none is kept.

use super::*;

// A live update of `chat` from its machine, after the second event of its log.
fn live(chat: &str, event: RunEvent) -> ChatLive {
    ChatLive {
        workspace: id("w1"),
        chat: id(chat),
        update: RunUpdate {
            run: id("run-1"),
            event,
        },
        after: Some(2),
    }
}

// Whether `client` hears nothing but the workspace's own changes before the pong to a ping.
async fn quiet(client: &mut Socket) -> bool {
    client.send(Wire::Ping(Vec::new().into())).await.unwrap();
    loop {
        match past_pings(client).await {
            Some(Ok(Wire::Pong(_))) => return true,
            Some(Ok(Wire::Text(text))) => {
                let message = Envelope::from_json(text.as_bytes()).unwrap().message;
                if !matches!(message, Message::Workspace(_)) {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

// The next live update `client` hears, past the workspace's own changes.
async fn live_heard(client: &mut Socket) -> ChatLive {
    loop {
        if let Message::ChatLive(live) = heard(client).await {
            return live;
        }
    }
}

#[tokio::test]
async fn a_runs_text_and_its_prompts_tool_reach_the_chats_followers_live_and_are_never_kept() {
    let fixture = Fixture::new().await;
    let server = fixture.run().await;
    others(&fixture).await;
    let (machine, mut core, mut ada) = connected(&fixture, &server).await;
    ada_chats(&machine, &mut core, &mut ada).await;
    let bo_token = token(&fixture, &server, "bo@example.com").await;
    let mut bo = feed(&server, "w1", &bo_token).await;
    let writing = RunEvent::TextDelta {
        text: "MARKER-DELTA".to_owned(),
    };
    let asked = RunEvent::PermissionAsked {
        run: id("run-1"),
        prompt: 1,
        tool: "Bash".to_owned(),
        input: serde_json::json!({ "command": "MARKER-INPUT" }),
    };
    let up = |live| Envelope {
        id: 0,
        message: Message::ChatLive(live),
    };

    // Not this machine's chat, and what a viewer never needs live, go nowhere.
    socket::send(&mut core, up(live("c-9", writing.clone()))).await;
    let whole = RunEvent::Text {
        text: "MARKER-REPLY".to_owned(),
    };
    socket::send(&mut core, up(live("c-1", whole))).await;
    // Text from before the reply the chat stored already.
    let stale = ChatLive {
        after: Some(1),
        ..live("c-1", writing.clone())
    };
    socket::send(&mut core, up(stale)).await;
    socket::send(&mut core, up(live("c-1", writing.clone()))).await;
    socket::send(&mut core, up(live("c-1", asked))).await;

    assert_eq!(live_heard(&mut ada).await, live("c-1", writing));
    let named = RunEvent::PermissionAsked {
        run: id("run-1"),
        prompt: 1,
        tool: "Bash".to_owned(),
        input: serde_json::Value::Null,
    };
    assert_eq!(live_heard(&mut ada).await, live("c-1", named));
    // Bo follows no chat, so hears none of it.
    assert!(quiet(&mut bo).await);
    let read = ask(&mut ada, 4, chat_query("w1", ChatQueryKind::Snapshot)).await;
    let kept = matches!(&read, Message::ChatAnswer(read)
        if matches!(&read.answer, ChatAnswerKind::Snapshot { seq: 2, events, .. } if events.len() == 2));
    assert!(kept, "{read:?}");
    let logged = serde_json::to_string(&*fixture.records.lock().unwrap()).unwrap();
    assert!(!logged.contains("MARKER"), "{logged}");
}

#[tokio::test]
async fn live_updates_go_only_to_a_client_that_takes_them_and_a_member_no_more_gets_none() {
    let fixture = Fixture::new().await;
    let server = fixture.run().await;
    others(&fixture).await;
    let (machine, mut core, mut ada) = connected(&fixture, &server).await;
    ada_chats(&machine, &mut core, &mut ada).await;
    let bo_token = token(&fixture, &server, "bo@example.com").await;
    let mut bo = feed(&server, "w1", &bo_token).await;
    // Followed from before the reply, whose coming back says the follow is in place.
    let follow = chat_query("w1", ChatQueryKind::Subscribe { after: 1 });
    socket::send(
        &mut bo,
        Envelope {
            id: 2,
            message: follow,
        },
    )
    .await;
    assert_eq!(chat_event(&mut bo).await.seq, 2);
    let token = token(&fixture, &server, "ada@example.com").await;
    let mut older = open(server.address, "/api/workspaces/w1/feed", &token)
        .await
        .unwrap();
    say(&mut older, &OFFERS[..3]).await;
    assert!(matches!(next(&mut older).await, Message::Welcome(_)));
    let follow = chat_query("w1", ChatQueryKind::Subscribe { after: 1 });
    socket::send(
        &mut older,
        Envelope {
            id: 2,
            message: follow,
        },
    )
    .await;
    assert_eq!(chat_event(&mut older).await.seq, 2);
    let writing = || RunEvent::TextDelta {
        text: "Wor".to_owned(),
    };
    let up = |live| Envelope {
        id: 0,
        message: Message::ChatLive(live),
    };

    socket::send(&mut core, up(live("c-1", writing()))).await;

    assert_eq!(live_heard(&mut ada).await, live("c-1", writing()));
    assert_eq!(live_heard(&mut bo).await, live("c-1", writing()));
    assert!(quiet(&mut older).await);
    let w1 = fixture.shared.db.workspace(&id("w1")).await.unwrap();
    w1.leave(&member_id("m3")).await.unwrap();
    socket::send(&mut core, up(live("c-1", writing()))).await;
    assert_eq!(live_heard(&mut ada).await, live("c-1", writing()));
    assert!(socket::ends(&mut bo).await);
}
