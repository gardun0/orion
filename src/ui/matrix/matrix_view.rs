use gpui::*;

use crate::ui::channel_strip::{channel_color, source_channel_color};
use crate::ui::root::RootView;
use crate::ui::theme::*;

impl RootView {
    pub(crate) fn render_routing(&self, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .overflow_hidden()
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child("ROUTING"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .child("Every active cell sends a source to that output."),
                    ),
            )
            .child(
                div()
                    .id("routing-matrix-scroll")
                    .mt_6()
                    .flex_1()
                    .overflow_scroll()
                    .child(
                        div()
                            .min_w(px(760.))
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .overflow_hidden()
                            .bg(rgb(SURFACE))
                            .child(
                                div()
                                    .h(px(76.))
                                    .flex()
                                    .items_center()
                                    .bg(rgb(BASE_RAISED))
                                    .border_b_1()
                                    .border_color(rgb(BORDER))
                                    .child(
                                        div()
                                            .w(px(240.))
                                            .flex_shrink_0()
                                            .px_4()
                                            .text_xs()
                                            .text_color(rgb(TEXT_FAINT))
                                            .child("SOURCE / DESTINATION"),
                                    )
                                    .children(self.state.outputs.iter().map(|output| {
                                        let color = channel_color(output);
                                        div()
                                            .w(px(130.))
                                            .flex_shrink_0()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .gap_1()
                                            .border_l_1()
                                            .border_color(rgb(BORDER))
                                            .child(
                                                div()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_sm()
                                                    .border_1()
                                                    .border_color(rgb(color))
                                                    .font_family(FONT_VALUES)
                                                    .text_xs()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(color))
                                                    .child(output.code.clone()),
                                            )
                                            .child(
                                                div()
                                                    .max_w(px(126.))
                                                    .px_1()
                                                    .overflow_hidden()
                                                    .text_xs()
                                                    .text_center()
                                                    .text_color(rgb(TEXT_MUTED))
                                                    .child(crate::ui::channel_strip::truncate_label(
                                                        &output.name,
                                                        14,
                                                    )),
                                            )
                                    })),
                            )
                            .children(self.state.sources.iter().enumerate().map(
                                |(source_index, source)| {
                                    div()
                                        .h(px(68.))
                                        .flex()
                                        .items_center()
                                        .border_b_1()
                                        .border_color(rgb(BORDER))
                                        .child(
                                            div()
                                                .w(px(240.))
                                                .flex_shrink_0()
                                                .px_4()
                                                .flex()
                                                .items_center()
                                                .gap_3()
                                                .child(
                                                    div()
                                                        .size(px(9.))
                                                        .rounded_full()
                                                        .bg(rgb(source_channel_color(source))),
                                                )
                                                .child(
                                                    div()
                                                        .min_w_0()
                                                        .flex_1()
                                                        .overflow_hidden()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .overflow_hidden()
                                                                .text_sm()
                                                                .font_weight(FontWeight::SEMIBOLD)
                                                                .text_color(rgb(TEXT))
                                                                .child(
                                                                    crate::ui::channel_strip::truncate_label(
                                                                        &source.name,
                                                                        22,
                                                                    ),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .overflow_hidden()
                                                                .text_xs()
                                                                .text_color(rgb(TEXT_FAINT))
                                                                .child(
                                                                    crate::ui::channel_strip::truncate_label(
                                                                        &source.detail,
                                                                        28,
                                                                    ),
                                                                ),
                                                        ),
                                                ),
                                        )
                                        .children(self.state.outputs.iter().enumerate().map(
                                            |(output_index, output)| {
                                                let active = source.routes[output_index];
                                                let failed = self
                                                    .state
                                                    .route_errors
                                                    .contains_key(&(source_index, output_index));
                                                let color = channel_color(output);
                                                div()
                                                    .w(px(130.))
                                                    .h_full()
                                                    .flex_shrink_0()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .border_l_1()
                                                    .border_color(rgb(BORDER))
                                                    .child(
                                                        div()
                                                            .id(format!(
                                                                "matrix-route-{source_index}-{output_index}"
                                                            ))
                                                            .size(px(32.))
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .rounded_md()
                                                            .border_1()
                                                            .border_color(if failed {
                                                                rgb(RED)
                                                            } else if active {
                                                                rgb(color)
                                                            } else {
                                                                rgb(BORDER)
                                                            })
                                                            .bg(if active {
                                                                rgb(SURFACE_RAISED)
                                                            } else {
                                                                rgb(ROUTE_OFF)
                                                            })
                                                            .text_sm()
                                                            .font_family(FONT_VALUES)
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(if active {
                                                                rgb(color)
                                                            } else {
                                                                rgb(TEXT_FAINT)
                                                            })
                                                            .cursor_pointer()
                                                            .hover(|style| {
                                                                style.bg(rgb(ROUTE_OFF_HOVER))
                                                            })
                                                            .on_click(cx.listener(
                                                                move |this, _, _, cx| {
                                                                    this.toggle_route(
                                                                        source_index,
                                                                        output_index,
                                                                        cx,
                                                                    );
                                                                },
                                                            ))
                                                            .child(if active { "ON" } else { "--" }),
                                                    )
                                            },
                                        ))
                                },
                            )),
                    ),
            )
    }
}
