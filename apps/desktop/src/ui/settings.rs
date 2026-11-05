use std::sync::Arc;

use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::{Context, IntoElement, ParentElement as _, Styled as _, div};
use weacon_version::Version;

use crate::report::Reporter;
use crate::ui::Main;

/// Settings, ending with About: which build this is.
pub(super) fn page(
    version: Version,
    reporter: &Arc<Reporter>,
    cx: &Context<'_, Main>,
) -> impl IntoElement {
    let muted = cx.theme().muted_foreground;
    let reports = reporter.is_available().then(|| {
        let reporter = Arc::clone(reporter);
        Switch::new("error-reports")
            .label("Send error reports")
            .checked(reporter.agreed())
            .on_click(cx.listener(move |_, agreed: &bool, _, cx| {
                reporter.set_agreed(*agreed);
                cx.notify();
            }))
    });
    v_flex()
        .gap_4()
        .p_4()
        .child(div().text_lg().child("Settings"))
        .children(reports)
        .child(
            v_flex()
                .gap_1()
                .child(div().text_base().child("About"))
                .children(about(version).map(|(label, value)| line(label, value, muted))),
        )
}

pub(super) const fn about(version: Version) -> [(&'static str, &'static str); 2] {
    [("Version", version.version), ("Commit", version.commit)]
}

fn line(label: &'static str, value: &'static str, muted: gpui_kit::Hsla) -> impl IntoElement {
    h_flex()
        .gap_2()
        .child(div().text_color(muted).child(label))
        .child(div().child(value))
}
