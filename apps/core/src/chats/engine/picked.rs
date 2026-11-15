//! Prompts that carry an element picked in Chrome: the log keeps a note of it, and the agent
//! gets the page's content quoted after the person's words, with its screenshot.

use weacon_observability::{Fault, Field};
use weacon_protocol::{AgentKind, Pick, PickNote, RequestId, code};

use super::agent::{Agent, Order};
use super::{ChatKey, Engine, chat_about};
use crate::app::blocking;
use crate::picks::Picks;
use crate::picks::quote::{self, Shown};
use crate::runs::Queued;

/// A pick goes only from the machine's own client: the server never relays one.
pub(super) fn local_only() -> Fault {
    Fault::new(
        code!("chat.pick_local"),
        "Only this machine's own client can send a pick",
    )
}

fn missing() -> Fault {
    Fault::new(code!("pick.missing"), "The picked element isn't kept here")
}

impl<A: Agent> Engine<A> {
    /// The note the log keeps of the pick kept under `request`, if the prompt names one.
    pub(super) async fn pick_note(
        &self,
        request: Option<&RequestId>,
    ) -> Result<Option<PickNote>, Fault> {
        let Some(request) = request else {
            return Ok(None);
        };
        let pick = self.read_pick(request).await?.ok_or_else(missing)?;
        Ok(Some(quote::note(&pick)))
    }

    /// What starts `next`'s turn: its text, then its pick quoted, when the pick is still here.
    pub(super) async fn order_for(&self, key: &ChatKey, next: &Queued) -> Order {
        let text = next.text.clone();
        let read = match &next.pick {
            Some(request) => self.read_pick(request).await,
            None => Ok(None),
        };
        let pick = read.unwrap_or_else(|fault| {
            let [workspace, chat] = chat_about(key);
            let fields = [workspace, chat, ("err", Field::error(&fault))];
            self.log.warn("pick not read", &fields);
            None
        });
        let Some(pick) = pick else {
            return Order::Send { text, png: None };
        };
        let agent = self
            .runs
            .of(key)
            .map_or(AgentKind::Claude, |record| record.agent);
        if !weacon_agents::takes_images(agent) {
            return self.filed(key, &text, pick).await;
        }
        // Only a screenshot that is PNG in base64 goes, as the file an agent reads would be.
        let png = pick
            .screenshot_png
            .clone()
            .filter(|png| crate::picks::is_png(png));
        let shown = png.as_ref().map_or(Shown::Missing, |_| Shown::Attached);
        let text = quote::prompt(&text, &pick, shown);
        Order::Send { text, png }
    }

    // `text` and `pick` quoted, for an agent that reads the screenshot from a file beside it.
    async fn filed(&self, key: &ChatKey, text: &str, pick: Pick) -> Order {
        let (picks, kept) = (self.picks(), pick.clone());
        let file = blocking(move || picks.screenshot_file(&kept)).await;
        let path = file.unwrap_or_else(|error| {
            let [workspace, chat] = chat_about(key);
            let fields = [workspace, chat, ("err", Field::error(&unreadable(error)))];
            self.log.warn("screenshot not written", &fields);
            None
        });
        let path = path.map(|path| path.display().to_string());
        let shown = path.as_deref().map_or(Shown::Missing, Shown::File);
        Order::Send {
            text: quote::prompt(text, &pick, shown),
            png: None,
        }
    }

    async fn read_pick(&self, request: &RequestId) -> Result<Option<Pick>, Fault> {
        let (picks, request) = (self.picks(), request.clone());
        blocking(move || picks.read(&request))
            .await
            .map_err(unreadable)
    }

    fn picks(&self) -> Picks {
        Picks::new(self.workspaces.state_dir())
    }
}

fn unreadable(error: std::io::Error) -> Fault {
    Fault::new(code!("pick.unreadable"), "The picked element can't be read").because(error)
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Asker, ChatAction, ChatCommand, ChatEvent, ChatQueryKind};

    use super::*;
    use crate::chats::test_support::{
        Bench, Told, bench, client, prompt, prompt_naming, query, soon,
    };
    use crate::picks::quote::prompt as quoted;
    use crate::test_support::pick;

    fn picking(mut command: ChatCommand, pick: &str) -> ChatCommand {
        if let ChatAction::Prompt { pick: named, .. } = &mut command.action {
            *named = Some(pick.parse().unwrap());
        }
        command
    }

    fn kept(bench: &Bench, request: &str) -> Pick {
        let mut picked = pick(request, "<button>Save</button>");
        picked.url = "http://localhost:3000/cart?session=x#total".to_owned();
        Picks::new(bench.state_dir()).keep(&picked).unwrap();
        picked
    }

    async fn told(bench: &mut Bench) -> Vec<Told> {
        let mut started = soon(bench.started.recv()).await.unwrap();
        let mut told = vec![soon(started.told.recv()).await.unwrap()];
        while let Ok(more) = started.told.try_recv() {
            told.push(more);
        }
        told
    }

    async fn logged_pick(bench: &Bench) -> Option<PickNote> {
        let (client, _pushed) = client(8);
        let answer = bench
            .chats
            .query(query("c-1", ChatQueryKind::Snapshot), &client);
        let Some(weacon_protocol::ChatAnswerKind::Snapshot { events, .. }) = answer.await.unwrap()
        else {
            panic!("no snapshot");
        };
        events.into_iter().find_map(|entry| {
            let ChatEvent::Prompt { pick, .. } = entry.event else {
                return None;
            };
            pick
        })
    }

    #[tokio::test]
    async fn claude_gets_the_page_quoted_after_the_text_and_the_screenshot_as_an_image() {
        let mut bench = bench();
        let picked = kept(&bench, "p-1");

        bench
            .command(picking(prompt("r-1", "Make it blue"), "p-1"))
            .await
            .unwrap();

        let expected = quoted("Make it blue", &picked, Shown::Attached);
        assert_eq!(
            told(&mut bench).await,
            [Told::Send(expected), Told::Png("iVBORw0KGgo=".to_owned())]
        );
        let note = PickNote {
            url: "http://localhost:3000/cart".to_owned(),
            title: "Home".to_owned(),
            selector: "main > button".to_owned(),
        };
        assert_eq!(logged_pick(&bench).await, Some(note));
    }

    #[tokio::test]
    async fn codex_is_told_where_the_screenshot_is() {
        let mut bench = bench();
        let picked = kept(&bench, "p-1");
        let asked = prompt_naming("r-1", "Make it blue", AgentKind::Codex);

        bench.command(picking(asked, "p-1")).await.unwrap();

        let file = bench.state_dir().join("picks/pick-p-1.png");
        let path = file.display().to_string();
        let expected = quoted("Make it blue", &picked, Shown::File(&path));
        assert_eq!(told(&mut bench).await, [Told::Send(expected)]);
        assert!(file.is_file());
    }

    #[tokio::test]
    async fn a_pick_without_a_screenshot_is_quoted_alone() {
        let mut bench = bench();
        let mut picked = kept(&bench, "p-1");
        picked.request = "p-2".parse().unwrap();
        picked.screenshot_png = None;
        Picks::new(bench.state_dir()).keep(&picked).unwrap();
        let codex = prompt_naming("r-1", "Go", AgentKind::Codex);

        bench.command(picking(codex, "p-2")).await.unwrap();
        let expected = quoted("Go", &picked, Shown::Missing);
        assert_eq!(told(&mut bench).await, [Told::Send(expected.clone())]);

        let mut claude = picking(prompt("r-2", "Go"), "p-2");
        claude.chat = "c-2".parse().unwrap();
        bench.command(claude).await.unwrap();
        assert_eq!(told(&mut bench).await, [Told::Send(expected)]);
    }

    #[tokio::test]
    async fn a_pick_gone_before_its_turn_leaves_the_text_alone() {
        let mut bench = bench();
        kept(&bench, "p-1");
        let release = bench.hold_next_start();
        bench.command(prompt("r-1", "First")).await.unwrap();
        bench
            .command(picking(prompt("r-2", "Second"), "p-1"))
            .await
            .unwrap();
        std::fs::remove_file(bench.state_dir().join("picks/pick-p-1.json")).unwrap();
        drop(release);

        let mut started = soon(bench.started.recv()).await.unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("First".to_owned()))
        );
        started
            .say
            .send(weacon_protocol::RunEvent::Finished {
                outcome: weacon_protocol::Outcome::Completed,
            })
            .unwrap();
        assert_eq!(
            soon(started.told.recv()).await,
            Some(Told::Send("Second".to_owned()))
        );
    }

    #[tokio::test]
    async fn a_pick_unreadable_at_its_turn_leaves_the_text_alone_and_warns() {
        let mut bench = bench();
        kept(&bench, "p-1");
        let release = bench.hold_next_start();
        bench
            .command(picking(prompt("r-1", "Go"), "p-1"))
            .await
            .unwrap();
        let file = bench.state_dir().join("picks/pick-p-1.json");
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        drop(release);

        assert_eq!(told(&mut bench).await, [Told::Send("Go".to_owned())]);
        assert!(weacon_testing::messages(&bench.records).contains(&"pick not read"));
    }

    #[tokio::test]
    async fn a_screenshot_that_isnt_png_goes_to_no_agent() {
        let mut bench = bench();
        let mut picked = kept(&bench, "p-1");
        picked.request = "p-2".parse().unwrap();
        picked.screenshot_png = Some("Zm9v".to_owned());
        Picks::new(bench.state_dir()).keep(&picked).unwrap();

        bench
            .command(picking(prompt("r-1", "Go"), "p-2"))
            .await
            .unwrap();
        let expected = quoted("Go", &picked, Shown::Missing);
        assert_eq!(told(&mut bench).await, [Told::Send(expected.clone())]);

        let mut codex = picking(prompt_naming("r-2", "Go", AgentKind::Codex), "p-2");
        codex.chat = "c-2".parse().unwrap();
        bench.command(codex).await.unwrap();
        assert_eq!(told(&mut bench).await, [Told::Send(expected)]);
        assert!(!bench.state_dir().join("picks/pick-p-2.png").exists());
    }

    #[tokio::test]
    async fn a_relayed_or_missing_pick_is_refused_before_the_log() {
        let bench = bench();
        kept(&bench, "p-1");
        let mut relayed = picking(prompt("r-1", "Go"), "p-1");
        relayed.by = Some(Asker {
            account: "a-1".parse().unwrap(),
            member: "m-1".parse().unwrap(),
        });

        let refused = bench.command(relayed).await.unwrap_err();
        assert_eq!(refused.code().as_str(), "chat.pick_local");
        let missing = bench.command(picking(prompt("r-2", "Go"), "p-9")).await;
        assert_eq!(missing.unwrap_err().code().as_str(), "pick.missing");
        assert_eq!(logged_pick(&bench).await, None);
    }
}
