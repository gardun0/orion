use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::assets::{ICON_MUTE, ICON_SOUND};
use crate::state::DeviceMonitorStatus;
use crate::ui::root::RootView;
use crate::ui::theme::*;

impl RootView {
    pub(crate) fn render_footer(&self, cx: &mut Context<Self>) -> Div {
        let muted = self.state.master_muted;
        let (backend_label, backend_color) = match self.state.device_monitor_status {
            DeviceMonitorStatus::Connecting => ("CONNECTING", AMBER),
            DeviceMonitorStatus::Connected => ("ONLINE", GREEN),
            DeviceMonitorStatus::Error(_) => ("OFFLINE", RED),
        };

        // let scene_name = self.state.scenes[self.state.selected_scene].name.clone();
        let connections = self.state.active_routes.len();
        let message = &self.state.status_message;
        let show_message = !message.starts_with("Ready ·");

        div()
            .h(px(62.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_5()
            .bg(rgb(BASE_RAISED))
            .border_t_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_6()
                    .text_xs()
                    .child(
                        div()
                            .id("engine-restart")
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(
                                if matches!(
                                    self.state.device_monitor_status,
                                    DeviceMonitorStatus::Error(_)
                                ) {
                                    rgb(RED)
                                } else {
                                    rgba(0x00000000)
                                },
                            )
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|style| {
                                style.border_color(rgb(BORDER_STRONG)).bg(rgb(SURFACE_2))
                            })
                            .tooltip(crate::ui::channel_strip::text_tooltip(
                                "Restart the audio engine",
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.restart_engine(cx);
                            }))
                            .child(div().size(px(10.)).rounded_full().bg(rgb(backend_color)))
                            .child(div().text_color(rgb(TEXT_FAINT)).child("ENGINE"))
                            .child(
                                div()
                                    .font_family(FONT_VALUES)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(backend_color))
                                    .child(backend_label),
                            ),
                    )
                    .child(div().w(px(1.)).h(px(28.)).bg(rgb(BORDER)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(rgb(TEXT_MUTED))
                            .child(
                                div()
                                    .font_family(FONT_VALUES)
                                    .text_color(rgb(TEXT_FAINT))
                                    .child(format!("{connections} active connections")),
                            ),
                    )
                    .when(show_message, |row| {
                        row.child(div().w(px(1.)).h(px(28.)).bg(rgb(BORDER))).child(
                            div()
                                .max_w(px(360.))
                                .min_w_0()
                                .overflow_hidden()
                                .text_color(rgb(TEXT_MUTED))
                                .child(crate::ui::channel_strip::truncate_label(message, 56)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_6()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child("CPU")
                            .child(
                                div()
                                    .font_family(FONT_VALUES)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(format!("{:.0}%", self.cpu_load)),
                            ),
                    )
                    .child(div().w(px(1.)).h(px(28.)).bg(rgb(BORDER)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child("BUFFER")
                            .child(
                                div()
                                    .font_family(FONT_VALUES)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT_MUTED))
                                    .child(format!("{} frames", self.state.buffer_size)),
                            ),
                    )
                    .child(
                        div()
                            .id("mute-all")
                            .w(px(200.))
                            .h(px(42.))
                            .ml_2()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(RED))
                            .bg(if muted { rgb(RED) } else { rgb(SURFACE_RAISED) })
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .cursor_pointer()
                            .hover(|style| style.bg(rgb(DANGER).opacity(0.28)))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_master_mute(cx);
                            }))
                            .child(
                                svg()
                                    .path(if muted { ICON_SOUND } else { ICON_MUTE })
                                    .size(px(18.))
                                    .text_color(rgb(TEXT)),
                            )
                            .child(if muted { "UNMUTE ALL" } else { "MUTE ALL" }),
                    ),
            )
    }
}
