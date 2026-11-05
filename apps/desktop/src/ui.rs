mod frames;
mod settings;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::message_scroller::{MessageScroller, MessageScrollerState};
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Window, div, px,
};
use tokio::sync::mpsc::UnboundedReceiver;
use weacon_observability::{Field, Logger};
use weacon_protocol::Outcome;
use weacon_version::Version;

use crate::feed::Feed;
use crate::report::Reporter;
use crate::transcript::{Change, Row, ToolState, Transcript};
use crate::ui::frames::Frames;

/// The window's one view: the run's transcript, or Settings.
pub(crate) struct Main {
    settings: bool,
    status: Status,
    transcript: Transcript,
    texts: HashMap<usize, Entity<TextViewState>>,
    scroller: Entity<MessageScrollerState>,
    frames: Frames,
    version: Version,
    reporter: Arc<Reporter>,
    log: Logger,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Connecting,
    Live,
    Ended,
    Failed(SharedString),
}

impl Main {
    pub(crate) fn new(
        version: Version,
        reporter: Arc<Reporter>,
        log: Logger,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        Self {
            settings: false,
            status: Status::Connecting,
            transcript: Transcript::default(),
            texts: HashMap::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            frames: Frames::default(),
            version,
            reporter,
            log,
        }
    }

    /// Applies what `feed` brings until it closes.
    pub(crate) fn listen(mut feed: UnboundedReceiver<Feed>, cx: &Context<'_, Self>) {
        cx.spawn(async move |this, cx| {
            while let Some(item) = feed.recv().await
                && this.update(cx, |main, cx| main.receive(item, cx)).is_ok()
            {}
        })
        .detach();
    }

    pub(crate) fn receive(&mut self, feed: Feed, cx: &mut Context<'_, Self>) {
        match feed {
            Feed::Connected(core) => {
                self.log.info(
                    "core connected",
                    &[
                        ("core", core.version.into()),
                        ("core_commit", core.commit.into()),
                    ],
                );
                self.status = Status::Live;
            }
            Feed::Update(update) => {
                for change in self.transcript.apply(&update.event) {
                    self.change(change, cx);
                }
            }
            Feed::Ended => {
                self.frames.report(&self.log);
                self.status = Status::Ended;
            }
            Feed::Failed(fault) => {
                self.log
                    .error("core feed lost", &[("err", Field::error(&fault))]);
                self.reporter.capture_error(&fault, fault.code(), &self.log);
                self.status = Status::Failed(fault.to_string().into());
            }
        }
        cx.notify();
    }

    fn change(&mut self, change: Change<'_>, cx: &mut Context<'_, Self>) {
        match change {
            Change::Added(index) => {
                if matches!(self.transcript.rows().get(index), Some(Row::Text)) {
                    let text = cx.new(|cx| TextViewState::markdown("", cx));
                    self.texts.insert(index, text);
                }
                self.scroller.update(cx, |state, cx| state.append(1, cx));
            }
            Change::Appended(index, text) => {
                if let Some(state) = self.texts.get(&index) {
                    state.update(cx, |state, cx| state.push_str(text, cx));
                }
            }
            Change::Replaced(index, text) => {
                if let Some(state) = self.texts.get(&index) {
                    state.update(cx, |state, cx| state.set_text(text, cx));
                }
            }
            Change::Updated(index) => {
                self.scroller
                    .update(cx, |state, cx| state.remeasure_items(index..index + 1, cx));
            }
        }
    }

    const fn header_label(&self) -> &'static str {
        if self.settings { "Back" } else { "Settings" }
    }

    fn header(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        h_flex()
            .justify_between()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().text_sm().child(self.status_text()))
            .child(
                Button::new("toggle-settings")
                    .ghost()
                    .label(self.header_label())
                    .on_click(cx.listener(|main, _, _, cx| {
                        main.settings = !main.settings;
                        cx.notify();
                    })),
            )
    }

    fn status_text(&self) -> SharedString {
        match &self.status {
            Status::Connecting => "Connecting to the core".into(),
            Status::Live => "Connected".into(),
            Status::Ended => "The core closed the connection".into(),
            Status::Failed(message) => message.clone(),
        }
    }

    fn transcript(&self) -> impl IntoElement {
        let rows: Vec<Row> = self.transcript.rows().to_vec();
        let texts = self.texts.clone();
        MessageScroller::new("transcript", self.scroller.clone(), move |index, _, cx| {
            row(rows.get(index), texts.get(&index), cx)
        })
        .size_full()
    }
}

fn row(row: Option<&Row>, text: Option<&Entity<TextViewState>>, cx: &gpui_kit::App) -> AnyElement {
    let body: AnyElement = match (row, text) {
        (Some(Row::Text), Some(text)) => TextView::new(text).selectable(true).into_any_element(),
        (Some(Row::Tool { tool, state }), _) => {
            let state = match state {
                ToolState::Running => "running",
                ToolState::Done => "done",
                ToolState::Failed => "failed",
            };
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(format!("{tool}: {state}"))
                .into_any_element()
        }
        (Some(Row::Finished(outcome)), _) => div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(finished(outcome))
            .into_any_element(),
        _ => div().into_any_element(),
    };
    div().px_4().py_1().child(body).into_any_element()
}

fn finished(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "Turn finished",
        Outcome::Interrupted => "Turn interrupted",
        Outcome::Failed { .. } => "Turn failed",
        Outcome::Accepted | Outcome::Unknown | Outcome::Newer => "Turn ended",
    }
}

impl Render for Main {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if self.status == Status::Live {
            self.frames.tick(Instant::now());
        }
        let body = if self.settings {
            settings::page(self.version, &self.reporter, cx).into_any_element()
        } else {
            self.transcript().into_any_element()
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.header(cx))
            .child(div().flex_1().min_h(px(0.)).child(body))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::base::TextSelection;
    use gpui_kit::{
        Modifiers, MouseButton, TestAppContext, VisualContext as _, VisualTestContext, point, size,
    };
    use weacon_observability::Fault;
    use weacon_protocol::{App as Build, code};

    use super::*;
    use crate::report::Reporter;
    use crate::test_support::{Records, VERSION, messages, recording_logger, updates};

    // How each text message of the recorded session starts.
    const OPENINGS: [&str; 6] = [
        "I'll start by reading",
        "The store keeps each list",
        "There's already a converter",
        "The converter now takes",
        "One test fails",
        "All 48 list tests pass",
    ];

    fn open(cx: &mut TestAppContext) -> (Entity<Main>, Records, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let (log, records) = recording_logger();
        let (root, cx) = cx.add_window_view(|window, cx| {
            let reporter = Arc::new(Reporter::new(None, Box::new(|_| Ok(()))));
            let main = cx.new(|cx| Main::new(VERSION, reporter, log, cx));
            gpui_kit::component::Root::new(main, window, cx)
        });
        let main = root.read_with(cx, |root, _| {
            root.view().clone().downcast::<Main>().unwrap()
        });
        (main, records, cx)
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| drop(window.draw(cx)));
    }

    fn feed(
        main: &Entity<Main>,
        items: impl IntoIterator<Item = Feed>,
        cx: &mut VisualTestContext,
    ) {
        main.update(cx, |main, cx| {
            items.into_iter().for_each(|item| main.receive(item, cx));
        });
        draw(cx);
    }

    fn core() -> Build {
        Build {
            name: "weacon-core".to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    // Tall enough that nothing scrolls: rows that scroll out of the list lose their selection.
    #[gpui_kit::test]
    fn text_stays_selectable_across_many_messages_while_the_run_streams(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        let window = cx.window_handle();
        cx.simulate_window_resize(window, size(px(1600.), px(60_000.)));
        let updates = updates(6_000);
        let (first, rest) = updates.split_at(updates.len() / 2);
        let connected = std::iter::once(Feed::Connected(core()));
        feed(
            &main,
            connected.chain(first.iter().cloned().map(Feed::Update)),
            cx,
        );

        let (messages, start, end) = main.read_with(cx, |main, cx| {
            let bounds = |index| main.texts.get(&index).map(|text| text.read(cx).bounds());
            let last = main.texts.keys().max().copied().unwrap_or_default();
            (main.texts.len(), bounds(0).unwrap(), bounds(last).unwrap())
        });
        let from = start.origin + point(px(1.), px(1.));
        let to = end.bottom_right() - point(px(1.), px(1.));
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        draw(cx);
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        draw(cx);
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        draw(cx);
        feed(&main, rest.iter().cloned().map(Feed::Update), cx);
        let selected = cx.update(TextSelection::selected_text);
        let copied: usize = OPENINGS
            .iter()
            .map(|opening| selected.matches(opening).count())
            .sum();

        assert!(messages >= 30, "only {messages} messages had streamed");
        assert_eq!(copied, messages, "{selected:.300}");
    }

    #[gpui_kit::test]
    fn the_status_follows_the_connection_and_a_failure_is_logged(cx: &mut TestAppContext) {
        let (main, records, cx) = open(cx);
        let status = |cx: &mut VisualTestContext| main.read_with(cx, |main, _| main.status_text());

        assert_eq!(status(cx), "Connecting to the core");
        feed(&main, [Feed::Connected(core())], cx);
        assert_eq!(status(cx), "Connected");
        feed(&main, [Feed::Ended], cx);
        assert_eq!(status(cx), "The core closed the connection");
        let fault = Fault::new(code!("daemon.not_running"), "The daemon isn't running");
        feed(&main, [Feed::Failed(fault)], cx);
        assert_eq!(status(cx), "The daemon isn't running");
        assert!(messages(&records).contains(&"core feed lost"));
    }

    #[gpui_kit::test]
    fn settings_end_with_about_which_names_the_build(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        main.update(cx, |main, cx| {
            main.settings = true;
            cx.notify();
        });
        draw(cx);

        assert_eq!(main.read_with(cx, |main, _| main.header_label()), "Back");
        assert_eq!(
            settings::about(VERSION),
            [("Version", "0.1.0"), ("Commit", "1a2b3c4")]
        );
    }
}
