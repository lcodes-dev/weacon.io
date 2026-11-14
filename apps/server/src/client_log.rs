use weacon_observability::{Forwarded, KnownLogs, Logger};
use weacon_protocol::{ClientLog, Code, code};

use crate::auth::Member;
use crate::http::RequestTag;
use crate::shared::Shared;

/// What the server offers to clients that forward their logs on the feed.
pub(crate) const LOG_FORWARD: Code = code!("log.forward");

// The events and codes the web app logs, as its `log.ts` lists them.
static WEB: KnownLogs = KnownLogs {
    events: &["app started", "connection lost", "connection refused"],
    codes: &[code!("connection.lost"), code!("connection.refused")],
};

/// The logs one feed's web app forwards, written with the feed's ids.
pub(crate) struct ClientLogs {
    log: Logger,
    forwarded: Forwarded,
}

impl ClientLogs {
    pub(crate) fn new(shared: &Shared, member: &Member, request: &RequestTag) -> Self {
        let log = shared.log.child(&[
            ("client", "web".into()),
            ("workspace_id", member.workspace.id().as_str().into()),
            ("member_id", member.id.as_str().into()),
            ("request_id", request.0.as_str().into()),
        ]);
        Self {
            log,
            forwarded: Forwarded::new(&WEB, (shared.clock)()),
        }
    }

    pub(crate) fn take(&mut self, shared: &Shared, record: &ClientLog) {
        self.forwarded.take(&self.log, record, (shared.clock)());
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use weacon_protocol::{
        ClientLog, Envelope, Level, LogEvent, MachineId, MachineSettingsQuery,
        MachineSettingsQueryKind, Message, RequestId, RunId, code,
    };

    use crate::test_support::socket::{Socket, ends, next, open, say, send};
    use crate::test_support::{Fixture, member_id, workspace_id};

    // Something that gets an answer at once, so every frame before it has been read.
    async fn fence(client: &mut Socket) {
        let query = Message::MachineSettingsQuery(MachineSettingsQuery {
            request: RequestId::try_from("r-fence".to_owned()).unwrap(),
            machine: MachineId::try_from("mc-none".to_owned()).unwrap(),
            workspace: workspace_id("w1"),
            query: MachineSettingsQueryKind::Current,
            by: None,
        });
        send(
            client,
            Envelope {
                id: 9,
                message: query,
            },
        )
        .await;
        let answer = next(client).await;
        assert!(matches!(answer, Message::Error(_)), "{answer:?}");
    }

    // A known event and an unknown one, each with a token in its ids and a path in its code.
    async fn forward_markers(client: &mut Socket) {
        let token = "markertoken_sk_live_0123456789";
        for msg in ["app started", "markermessage"] {
            let log = ClientLog {
                level: Level::Warn,
                msg: LogEvent::try_from(msg.to_owned()).unwrap(),
                run_id: Some(RunId::try_from(token.to_owned()).unwrap()),
                request_id: Some(RequestId::try_from(token.to_owned()).unwrap()),
                code: Some(code!("home.markerpath.id_rsa")),
            };
            send(
                client,
                Envelope {
                    id: 0,
                    message: Message::Log(log),
                },
            )
            .await;
        }
    }

    async fn feed(fixture: &Fixture, offers: &[&str]) -> Vec<weacon_observability::Record> {
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = open(
            server.address,
            "/api/workspaces/w1/feed?since=1",
            &ada.token,
        )
        .await
        .unwrap();
        say(&mut client, offers).await;
        next(&mut client).await;
        forward_markers(&mut client).await;
        fence(&mut client).await;
        fixture.records.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn a_forwarded_log_keeps_only_its_known_event_and_the_feeds_ids() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();

        let records = feed(
            &fixture,
            &["workspace.feed", "machine.settings", "log.forward"],
        )
        .await;

        let written = serde_json::to_string(&records).unwrap();
        assert!(!written.contains("marker"), "{written}");
        let feed_request = records
            .iter()
            .find(|record| {
                record.fields.get("route") == Some(&json!("/api/workspaces/{workspace}/feed"))
            })
            .unwrap();
        let forwarded: Vec<_> = records
            .iter()
            .filter(|record| record.fields.get("client") == Some(&json!("web")))
            .map(|record| {
                (
                    record.level,
                    record.msg,
                    Value::Object(record.fields.clone()),
                )
            })
            .collect();
        let ids = json!({
            "client": "web",
            "workspace_id": "w1",
            "member_id": "m1",
            "request_id": feed_request.fields["request_id"],
        });
        assert_eq!(
            forwarded,
            [
                // The unknown code comes first, then its event without it.
                (Level::Warn, "client log unknown", ids.clone()),
                (Level::Warn, "app started", ids),
            ]
        );
    }

    #[tokio::test]
    async fn a_client_that_doesnt_offer_to_forward_logs_gets_nothing_written() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();

        let records = feed(&fixture, &["workspace.feed", "machine.settings"]).await;

        assert!(
            records
                .iter()
                .all(|record| !record.fields.contains_key("client")),
            "{records:?}"
        );
    }

    #[tokio::test]
    async fn a_member_removed_writes_nothing_more_and_loses_the_feed() {
        let mut fixture = Fixture::new().await;
        fixture.quiet();
        let (server, ada) = fixture.client("ada@example.com").await;
        let mut client = open(
            server.address,
            "/api/workspaces/w1/feed?since=1",
            &ada.token,
        )
        .await
        .unwrap();
        say(&mut client, &["workspace.feed", "log.forward"]).await;
        next(&mut client).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        w1.unlink(&member_id("m1")).await;

        let log = ClientLog {
            level: Level::Info,
            msg: LogEvent::try_from("app started".to_owned()).unwrap(),
            run_id: None,
            request_id: None,
            code: None,
        };
        send(
            &mut client,
            Envelope {
                id: 0,
                message: Message::Log(log),
            },
        )
        .await;

        assert!(ends(&mut client).await);
        let records = fixture.records.lock().unwrap().clone();
        assert!(
            records
                .iter()
                .all(|record| !record.fields.contains_key("client")),
            "{records:?}"
        );
    }
}
