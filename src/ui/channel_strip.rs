use gpui::*;
use crate::ui::theme::*;

/// Placeholder channel strip — Phase 2 will add faders, meters, and mute/solo.
#[derive(IntoElement)]
pub struct ChannelStrip {
    pub label: SharedString,
}

impl ChannelStrip {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

impl RenderOnce for ChannelStrip {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .w(px(48.))
            .py_3()
            .gap_2()
            .bg(rgb(SURFACE_2))
            .rounded_md()
            .child(
                div()
                    .w(px(4.))
                    .h(px(80.))
                    .bg(rgb(TEXT_FAINT))
                    .rounded_sm(),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_MUTED))
                    .child(self.label),
            )
    }
}
