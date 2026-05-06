use gpui::*;
use crate::ui::theme::*;

#[derive(IntoElement)]
pub struct HeaderBar;

impl RenderOnce for HeaderBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .px_6()
            .py_3()
            .bg(rgb(SURFACE))
            .border_b_1()
            .border_color(rgb(ACCENT))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(ACCENT))
                            .child("Orion"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .child("Virtual Audio Mixer"),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(env!("CARGO_PKG_VERSION")),
            )
    }
}
