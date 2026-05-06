use gpui::*;
use crate::ui::theme::*;

#[derive(IntoElement)]
pub struct FooterBar {
    pub sample_rate: u32,
    pub buffer_size: u32,
    pub engine_running: bool,
}

impl FooterBar {
    pub fn idle() -> Self {
        Self {
            sample_rate: 48_000,
            buffer_size: 512,
            engine_running: false,
        }
    }
}

impl RenderOnce for FooterBar {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let status = if self.engine_running { "Running" } else { "Idle" };
        let status_text = format!(
            "Status: {status}  |  Sample Rate: {} Hz  |  Buffer: {} frames",
            self.sample_rate, self.buffer_size
        );

        div()
            .flex()
            .items_center()
            .px_6()
            .py_2()
            .bg(rgb(SURFACE))
            .border_t_1()
            .border_color(rgb(SURFACE_2))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(TEXT_FAINT))
                    .child(status_text),
            )
    }
}
