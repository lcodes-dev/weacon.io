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
use tokio::sync::mpsc::Receiver;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{Outcome, RunId, RunUpdate, code};
use weacon_version::Version;

use crate::feed::Feed;
use crate::report::Reporter;
use crate::transcript::{Change, Row, ToolState, Transcript};
use crate::ui::frames::Frames;

// The most updates applied before the window may draw again.
const BATCH: usize = 256;

/// The window's one view: the run's transcript, or Settings.
pub(crate) struct Main {
    settings: bool,
    status: Status,
    /// The run shown: the first one the core streamed.
    run: Option<RunId>,
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
            run: None,
            transcript: Transcript::default(),
            texts: HashMap::new(),
            scroller: cx.new(|cx| MessageScrollerState::new(0, cx)),
            frames: Frames::default(),
            version,
            reporter,
            log,
        }
    }

    /// Applies what `feed` brings until it closes, a batch at a time.
    pub(crate) fn listen(mut feed: Receiver<Feed>, cx: &Context<'_, Self>) {
        cx.spawn(async move |this, cx| {
            while let Some(first) = feed.recv().await
                && this
                    .update(cx, |main, cx| main.receive_batch(first, &mut feed, cx))
                    .is_ok()
            {}
        })
        .detach();
    }

    // `first` and what else has queued, up to a batch, so the window can draw in between.
    fn receive_batch(
        &mut self,
        first: Feed,
        feed: &mut Receiver<Feed>,
        cx: &mut Context<'_, Self>,
    ) {
        let rest = std::iter::from_fn(|| feed.try_recv().ok()).take(BATCH - 1);
        std::iter::once(first)
            .chain(rest)
            .for_each(|item| self.receive(item, cx));
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
            Feed::Update(update) => self.update(&update, cx),
            Feed::Skipped(code) => {
                self.log
                    .warn("core frame dropped", &[("code", code.as_str().into())]);
            }
            Feed::Ended => {
                self.frames.report(&self.log);
                self.status = Status::Ended;
            }
            Feed::Failed(fault) => {
                self.status = Status::Failed(fault.to_string().into());
                self.failed(fault, cx);
            }
        }
        cx.notify();
    }

    fn update(&mut self, update: &RunUpdate, cx: &mut Context<'_, Self>) {
        if *self.run.get_or_insert_with(|| update.run.clone()) != update.run {
            return;
        }
        for change in self.transcript.apply(&update.event) {
            self.change(change, cx);
        }
    }

    // A core that isn't running or is too old is expected; anything else is reported, off the
    // window's thread because a report blocks while it uploads.
    fn failed(&self, fault: Fault, cx: &Context<'_, Self>) {
        let expected = [code!("daemon.not_running"), code!("core.no_run_stream")];
        if expected.contains(fault.code()) {
            return self
                .log
                .warn("core unavailable", &[("err", Field::error(&fault))]);
        }
        self.log
            .error("core feed lost", &[("err", Field::error(&fault))]);
        let (reporter, log) = (Arc::clone(&self.reporter), self.log.clone());
        cx.background_spawn(async move { reporter.capture_error(&fault, fault.code(), &log) })
            .detach();
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
                    .on_click(cx.listener(|main, _, _, cx| main.toggle_settings(cx))),
            )
    }

    fn toggle_settings(&mut self, cx: &mut Context<'_, Self>) {
        self.settings = !self.settings;
        cx.notify();
    }

    fn status_text(&self) -> SharedString {
        match &self.status {
            Status::Connecting => "Connecting to the core".into(),
            Status::Live => "Connected".into(),
            Status::Ended => "The core closed the connection".into(),
            Status::Failed(message) => message.clone(),
        }
    }

    // Rows render after this view does, so they read it rather than a copy of every row.
    fn transcript(&self, cx: &Context<'_, Self>) -> impl IntoElement {
        let this = cx.weak_entity();
        MessageScroller::new("transcript", self.scroller.clone(), move |index, _, cx| {
            let Some(main) = this.upgrade() else {
                return div().into_any_element();
            };
            let main = main.read(cx);
            row(
                main.transcript.rows().get(index),
                main.texts.get(&index),
                cx,
            )
        })
        .size_full()
    }
}

fn row(row: Option<&Row>, text: Option<&Entity<TextViewState>>, cx: &gpui_kit::App) -> AnyElement {
    let body = match (row, text) {
        (Some(Row::Text), Some(text)) => TextView::new(text).selectable(true).into_any_element(),
        _ => div()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .children(row.and_then(label))
            .into_any_element(),
    };
    div().px_4().py_1().child(body).into_any_element()
}

/// What a row other than text says.
fn label(row: &Row) -> Option<String> {
    let text = match row {
        Row::Text => return None,
        Row::Tool { tool, state } => {
            let state = match state {
                ToolState::Running => "running",
                ToolState::Done => "done",
                ToolState::Failed => "failed",
            };
            return Some(format!("{tool}: {state}"));
        }
        Row::Finished(Outcome::Completed) => "Turn finished",
        Row::Finished(Outcome::Interrupted) => "Turn interrupted",
        Row::Finished(Outcome::Failed { .. }) => "Turn failed",
        Row::Finished(Outcome::Accepted | Outcome::Unknown | Outcome::Newer) => "Turn ended",
    };
    Some(text.to_owned())
}

impl Render for Main {
    fn render(&mut self, _: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        if self.status == Status::Live {
            self.frames.tick(Instant::now());
        }
        let body = if self.settings {
            settings::page(self.version, &self.reporter, cx).into_any_element()
        } else {
            self.transcript(cx).into_any_element()
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
    use weacon_protocol::{App as Build, RunEvent};

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
        open_with(Reporter::new(None, Box::new(|_| Ok(()))), cx)
    }

    fn open_with(
        reporter: Reporter,
        cx: &mut TestAppContext,
    ) -> (Entity<Main>, Records, &mut VisualTestContext) {
        cx.update(gpui_kit::init);
        let (log, records) = recording_logger();
        let reporter = Arc::new(reporter);
        let (root, cx) = cx.add_window_view(|window, cx| {
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
    fn the_status_follows_the_connection(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        let status = |cx: &mut VisualTestContext| main.read_with(cx, |main, _| main.status_text());

        assert_eq!(status(cx), "Connecting to the core");
        feed(&main, [Feed::Connected(core())], cx);
        assert_eq!(status(cx), "Connected");
        feed(&main, [Feed::Ended], cx);
        assert_eq!(status(cx), "The core closed the connection");
        let fault = Fault::new(code!("daemon.not_running"), "The daemon isn't running");
        feed(&main, [Feed::Failed(fault)], cx);
        assert_eq!(status(cx), "The daemon isn't running");
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

    fn run_update(run: &str, text: &str) -> Feed {
        Feed::Update(RunUpdate {
            run: RunId::try_from(run.to_owned()).unwrap(),
            event: RunEvent::Text {
                text: text.to_owned(),
            },
        })
    }

    fn rows(main: &Entity<Main>, cx: &VisualTestContext) -> usize {
        main.read_with(cx, |main, _| main.transcript.rows().len())
    }

    #[gpui_kit::test]
    fn the_feed_reaches_the_view_until_it_closes(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        let (sender, receiver) = tokio::sync::mpsc::channel(BATCH * 3);
        main.update(cx, |_, cx| Main::listen(receiver, cx));
        sender.try_send(Feed::Connected(core())).unwrap();
        for index in 0..BATCH * 2 {
            sender
                .try_send(run_update("r-1", &format!("Message {index}")))
                .unwrap();
        }
        drop(sender);
        draw(cx);

        assert_eq!(
            main.read_with(cx, |main, _| main.status_text()),
            "Connected"
        );
        assert_eq!(rows(&main, cx), BATCH * 2);
    }

    #[gpui_kit::test]
    fn only_the_first_run_is_shown(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        feed(
            &main,
            [
                run_update("r-1", "One"),
                run_update("r-2", "Other"),
                run_update("r-1", "Two"),
            ],
            cx,
        );

        assert_eq!(rows(&main, cx), 2);
    }

    #[gpui_kit::test]
    fn frames_are_measured_only_while_connected(cx: &mut TestAppContext) {
        let (idle, idle_records, cx) = open(cx);
        draw(cx);
        draw(cx);
        feed(&idle, [Feed::Ended], cx);

        assert!(!messages(&idle_records).contains(&"stream measured"));
    }

    #[gpui_kit::test]
    fn a_stream_reports_its_frames_when_it_ends(cx: &mut TestAppContext) {
        let (main, records, cx) = open(cx);
        feed(&main, [Feed::Connected(core())], cx);
        feed(&main, [run_update("r-1", "Hello")], cx);
        feed(&main, [Feed::Ended], cx);

        assert!(messages(&records).contains(&"stream measured"));
    }

    #[gpui_kit::test]
    fn a_dropped_frame_is_logged(cx: &mut TestAppContext) {
        let (main, records, cx) = open(cx);
        feed(&main, [Feed::Skipped(code!("frame.invalid"))], cx);

        assert_eq!(messages(&records), ["core frame dropped"]);
    }

    fn reporting(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Main>,
        Records,
        Arc<std::sync::Mutex<usize>>,
        &mut VisualTestContext,
    ) {
        let reports = weacon_observability::Reports::new(
            "https://key@sentry.example.com/1",
            "weacon-desktop/0.1.0",
            &VERSION.release(),
            "/home/ada",
        )
        .ok();
        let sent = Arc::new(std::sync::Mutex::new(0));
        let counted = Arc::clone(&sent);
        let reporter = Reporter::new(
            reports,
            Box::new(move |_| {
                *counted.lock().unwrap() += 1;
                Ok(())
            }),
        );
        reporter.set_agreed(true);
        let (main, records, cx) = open_with(reporter, cx);
        (main, records, sent, cx)
    }

    #[gpui_kit::test]
    fn an_unexpected_failure_is_reported_and_a_missing_core_only_warned(cx: &mut TestAppContext) {
        let (main, records, sent, cx) = reporting(cx);
        let missing = Fault::new(code!("daemon.not_running"), "The daemon isn't running");
        let old = Fault::new(code!("core.no_run_stream"), "The core can't stream runs");
        feed(&main, [Feed::Failed(missing), Feed::Failed(old)], cx);
        let after_expected = *sent.lock().unwrap();
        let lost = Fault::new(code!("daemon.no_answer"), "The daemon didn't answer");
        feed(&main, [Feed::Failed(lost)], cx);

        assert_eq!(after_expected, 0);
        assert_eq!(*sent.lock().unwrap(), 1);
        assert_eq!(
            messages(&records),
            ["core unavailable", "core unavailable", "core feed lost"]
        );
    }

    #[gpui_kit::test]
    fn the_settings_button_toggles_between_settings_and_back(cx: &mut TestAppContext) {
        let (main, _, cx) = open(cx);
        let label = |cx: &mut VisualTestContext| main.read_with(cx, |main, _| main.header_label());
        main.update(cx, Main::toggle_settings);
        let opened = label(cx);
        main.update(cx, Main::toggle_settings);

        assert_eq!(opened, "Back");
        assert_eq!(label(cx), "Settings");
    }

    #[test]
    fn rows_other_than_text_say_what_happened() {
        let tool = |state| Row::Tool {
            tool: "Read".to_owned(),
            state,
        };
        let labels: Vec<_> = [
            Row::Text,
            tool(ToolState::Running),
            tool(ToolState::Done),
            tool(ToolState::Failed),
            Row::Finished(Outcome::Completed),
            Row::Finished(Outcome::Interrupted),
            Row::Finished(Outcome::Failed {
                code: code!("agent.failed"),
            }),
            Row::Finished(Outcome::Unknown),
        ]
        .iter()
        .map(label)
        .collect();

        assert_eq!(
            labels,
            [
                None,
                Some("Read: running".to_owned()),
                Some("Read: done".to_owned()),
                Some("Read: failed".to_owned()),
                Some("Turn finished".to_owned()),
                Some("Turn interrupted".to_owned()),
                Some("Turn failed".to_owned()),
                Some("Turn ended".to_owned()),
            ]
        );
    }
}
