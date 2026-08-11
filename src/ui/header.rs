use gpui::prelude::FluentBuilder;
use gpui::*;

use crate::assets::{ICON_ARROW_DOWN, ICON_CAT};
use crate::state::format_sample_rate;
use crate::ui::root::RootView;
use crate::ui::theme::*;

impl RootView {
    pub(crate) fn render_header(&self, cx: &mut Context<Self>) -> Div {
        let scene_name = self.state.scenes[self.state.selected_scene].name.clone();
        let single_scene = self.state.scenes.len() <= 1;

        div()
            .h(px(60.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .px_5()
            .bg(rgb(BASE_RAISED))
            .border_b_1()
            .border_color(rgb(BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(svg().path(ICON_CAT).size(px(28.)).text_color(rgb(PRIMARY)))
                            .child(
                                div()
                                    .text_size(px(23.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .child("ORION"),
                            ),
                    )
                    .child(div().w(px(1.)).h(px(28.)).bg(rgb(BORDER)))
                    .child(
                        div()
                            .id("scene-selector")
                            .w(px(180.))
                            .h(px(36.))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(SURFACE))
                            .text_sm()
                            .text_color(rgb(TEXT_MUTED))
                            .when(!single_scene, |selector| {
                                selector
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.state.scene_dropdown_open =
                                            !this.state.scene_dropdown_open;
                                        cx.notify();
                                    }))
                            })
                            .child(scene_name)
                            .child(svg().path(ICON_ARROW_DOWN).size(px(12.)).text_color(
                                if single_scene {
                                    rgb(BORDER)
                                } else {
                                    rgb(TEXT_FAINT)
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .font_family(FONT_VALUES)
                            .text_xs()
                            .text_color(rgb(TEXT_FAINT))
                            .child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
                            .child(format_sample_rate(self.state.sample_rate)),
                    ),
            )
    }
}
