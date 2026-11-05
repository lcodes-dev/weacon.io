//! `weacon-desktop`: the desktop app, a client of the core.

mod app;
mod feed;
mod platform;
mod report;
#[cfg(test)]
mod test_support;
mod transcript;
mod ui;

use std::io;
use std::process::ExitCode;
use std::sync::Arc;

use gpui_kit::component::Root;
use gpui_kit::{AppContext as _, Bounds, WindowBounds, WindowOptions, px, size};
use weacon_client::socket_path;
use weacon_observability::json_lines;

use crate::app::{App, Setup};
use crate::ui::Main;

fn main() -> ExitCode {
    let env = |key: &str| std::env::var_os(key);
    let app = App::start(
        weacon_version::version!(),
        Setup {
            env: &env,
            stderr: json_lines(io::stderr()),
            deliver: report::http(),
        },
    );
    std::panic::set_hook(app.panic_hook());
    let Some(state_dir) = app.state_dir.clone() else {
        app.fail(&app::state_unavailable());
        return ExitCode::FAILURE;
    };
    let feed = feed::start(socket_path(&state_dir), app::build(&app.version));
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(960.), px(720.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..WindowOptions::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let main = cx.new(|cx| {
                Main::listen(feed, cx);
                Main::new(app.version, Arc::clone(&app.reporter), app.log.clone(), cx)
            });
            cx.new(|cx| Root::new(main, window, cx))
        });
        if let Err(error) = opened {
            app.fail(&app::window_failed(error));
            cx.quit();
        }
    });
    ExitCode::SUCCESS
}
