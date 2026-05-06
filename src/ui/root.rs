use gpui::*;
use crate::ui::footer::FooterBar;
use crate::ui::header::HeaderBar;
use crate::ui::theme::*;

pub struct RootView;

impl RootView {
    pub fn new() -> Self {
        Self
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BASE))
            .child(HeaderBar)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .child("— content goes here —"),
                    ),
            )
            .child(FooterBar::idle())
    }
}
