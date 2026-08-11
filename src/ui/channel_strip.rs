use gpui::*;

use crate::assets::{ICON_ARROW_DOWN, ICON_MUTE, ICON_SOUND};
use crate::state::{
    EndpointPickerTarget, FaderTarget, KnobParam, KnobTarget, OutputBus, SourceStrip, MAX_DELAY_MS,
};
use crate::ui::root::RootView;
use crate::ui::theme::*;
use orion_dsp;

/// Simple text bubble for truncated labels.
struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .border_1()
            .border_color(rgb(BORDER_STRONG))
            .bg(rgb(SURFACE_RAISED))
            .text_xs()
            .text_color(rgb(TEXT))
            .child(self.0.clone())
    }
}

pub(crate) fn text_tooltip(
    text: impl Into<String>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| TextTooltip(text.clone().into())).into()
}

impl RootView {
    pub(crate) fn render_source_strip(
        &self,
        index: usize,
        strip_height: f32,
        meter_height: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let source = &self.state.sources[index];
        let color = source.color.value();
        let muted = source.muted || self.state.master_muted;

        div()
            .w(px(126.))
            .h(px(strip_height))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h(px(62.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .pt_2()
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().size(px(7.)).rounded_full().bg(if source.online {
                                rgb(GREEN)
                            } else if source.endpoint_id.is_some() {
                                rgb(WARNING)
                            } else {
                                rgb(TEXT_FAINT)
                            }))
                            .child(
                                div()
                                    .id(format!("source-rename-{index}"))
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(TEXT))
                                    .whitespace_nowrap()
                                    .cursor_pointer()
                                    .hover(|style| style.text_color(rgb(color)))
                                    .tooltip(text_tooltip(source.name.clone()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_rename_modal(true, index, window, cx);
                                    }))
                                    .child(truncate_label(&source.name.to_uppercase(), 14)),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("source-selector-{index}"))
                            .w_full()
                            .h(px(26.))
                            .px_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(BASE_RAISED))
                            .text_size(px(9.))
                            .text_color(if source.online {
                                rgb(TEXT_MUTED)
                            } else {
                                rgb(TEXT_FAINT)
                            })
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(BORDER_STRONG)))
                            .tooltip(text_tooltip(source.detail.clone()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_endpoint_picker(EndpointPickerTarget::Source(index), cx);
                            }))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(truncate_label(&source.detail, 13)),
                            )
                            .child(
                                svg()
                                    .path(ICON_ARROW_DOWN)
                                    .size(px(10.))
                                    .flex_shrink_0()
                                    .text_color(rgb(TEXT_FAINT)),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(meter_height))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(BASE_RAISED))
                    .child(render_meter(source.meter_l, source.meter_r, muted)),
            )
            .child(render_knob_section(
                FaderTarget::Source(index),
                source.delay_ms,
                &source.eq,
                source.mode,
                color,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h(px(172.))
                    .flex_shrink_0()
                    .flex()
                    .pt_1()
                    .px_2()
                    .gap_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(self.render_fader(FaderTarget::Source(index), source.gain_db, color, cx))
                    .child(
                        div()
                            .h_full()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_start()
                            .items_center()
                            .gap_1()
                            .pt(px(5.))
                            .overflow_hidden()
                            .children(self.state.outputs.iter().enumerate().map(
                                |(output_index, output)| {
                                    let wanted = source.routes[output_index];
                                    let live = self.state.route_connected(index, output_index);
                                    let route_error = self
                                        .state
                                        .route_errors
                                        .get(&(index, output_index))
                                        .cloned();
                                    let bus_color = output.color.value();
                                    // Wanted but not live yet: pending style
                                    // (device offline or route connecting).
                                    let pending = wanted && !live;
                                    div()
                                        .id(format!("route-{index}-{output_index}"))
                                        .w_full()
                                        .h(px(24.))
                                        .flex_shrink_0()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(if route_error.is_some() {
                                            rgb(RED)
                                        } else if live || pending {
                                            rgb(bus_color)
                                        } else {
                                            rgb(BORDER)
                                        })
                                        .bg(if live {
                                            rgb(SURFACE_RAISED)
                                        } else {
                                            rgb(ROUTE_OFF)
                                        })
                                        .text_size(px(9.))
                                        .font_family(FONT_VALUES)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(if route_error.is_some() {
                                            rgb(RED)
                                        } else if live {
                                            rgb(bus_color)
                                        } else if pending {
                                            rgb(bus_color).opacity(0.55)
                                        } else {
                                            rgb(TEXT_FAINT)
                                        })
                                        .cursor_pointer()
                                        .hover(|style| style.bg(rgb(ROUTE_OFF_HOVER)))
                                        .tooltip(text_tooltip(route_error.unwrap_or_else(|| {
                                            format!("{} → {}", source.name, output.name)
                                        })))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_route(index, output_index, cx);
                                        }))
                                        .child(output.code.clone())
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .py_2()
                    .flex()
                    .justify_center()
                    .font_family(FONT_VALUES)
                    .text_sm()
                    .text_color(rgb(TEXT_MUTED))
                    .child(format_gain(source.gain_db)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .id(format!("source-mute-{index}"))
                            .h(px(32.))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(if source.muted { rgb(RED) } else { rgb(BORDER) })
                            .bg(if source.muted {
                                rgb(DANGER).opacity(0.22)
                            } else {
                                rgb(SURFACE_2)
                            })
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if source.muted {
                                rgb(TEXT)
                            } else {
                                rgb(TEXT_MUTED)
                            })
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(BORDER_STRONG)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_source_mute(index);
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path(if source.muted { ICON_SOUND } else { ICON_MUTE })
                                    .size(px(13.))
                                    .text_color(if source.muted {
                                        rgb(TEXT)
                                    } else {
                                        rgb(TEXT_MUTED)
                                    }),
                            )
                            .child(if source.muted { "MUTED" } else { "MUTE" }),
                    ),
            )
    }

    pub(crate) fn render_output_strip(
        &self,
        index: usize,
        strip_height: f32,
        meter_height: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let output = &self.state.outputs[index];
        let color = output.color.value();
        let muted = output.muted || self.state.master_muted;

        div()
            .w(px(108.))
            .h(px(strip_height))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .h(px(62.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .pt_2()
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .px_1()
                                    .flex_shrink_0()
                                    .rounded_sm()
                                    .text_size(px(9.))
                                    .font_family(FONT_VALUES)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(color))
                                    .child(output.code.clone()),
                            )
                            .child(div().size(px(7.)).rounded_full().bg(if output.online {
                                rgb(GREEN)
                            } else if output.endpoint_id.is_some() {
                                rgb(WARNING)
                            } else {
                                rgb(TEXT_FAINT)
                            }))
                            .child(
                                div()
                                    .id(format!("output-rename-{index}"))
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(TEXT))
                                    .whitespace_nowrap()
                                    .cursor_pointer()
                                    .hover(|style| style.text_color(rgb(color)))
                                    .tooltip(text_tooltip(output.name.clone()))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_rename_modal(false, index, window, cx);
                                    }))
                                    .child(truncate_label(&output.name.to_uppercase(), 12)),
                            ),
                    )
                    .child(
                        div()
                            .id(format!("output-selector-{index}"))
                            .w_full()
                            .h(px(26.))
                            .px_1()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(BASE_RAISED))
                            .text_size(px(9.))
                            .text_color(if output.online {
                                rgb(TEXT_MUTED)
                            } else {
                                rgb(TEXT_FAINT)
                            })
                            .cursor_pointer()
                            .hover(|style| style.border_color(rgb(BORDER_STRONG)))
                            .tooltip(text_tooltip(output.detail.clone()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_endpoint_picker(EndpointPickerTarget::Output(index), cx);
                            }))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .child(truncate_label(&output.detail, 11)),
                            )
                            .child(
                                svg()
                                    .path(ICON_ARROW_DOWN)
                                    .size(px(10.))
                                    .flex_shrink_0()
                                    .text_color(rgb(TEXT_FAINT)),
                            ),
                    ),
            )
            .child(
                div()
                    .h(px(meter_height))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(BASE_RAISED))
                    .child(render_meter(output.meter_l, output.meter_r, muted)),
            )
            .child(render_knob_section(
                FaderTarget::Output(index),
                output.delay_ms,
                &output.eq,
                output.mode,
                color,
                cx,
            ))
            .child(
                div()
                    .flex_1()
                    .min_h(px(172.))
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt_1()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(self.render_fader(
                        FaderTarget::Output(index),
                        output.gain_db,
                        color,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .py_2()
                    .flex()
                    .justify_center()
                    .font_family(FONT_VALUES)
                    .text_sm()
                    .text_color(rgb(TEXT_MUTED))
                    .child(format_gain(output.gain_db)),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_end()
                    .px_2()
                    .py_2()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .id(format!("output-mute-{index}"))
                            .h(px(32.))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .rounded_sm()
                            .border_1()
                            .border_color(if output.muted { rgb(RED) } else { rgb(BORDER) })
                            .bg(if output.muted {
                                rgb(DANGER).opacity(0.22)
                            } else {
                                rgb(SURFACE_2)
                            })
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(if output.muted {
                                rgb(TEXT)
                            } else {
                                rgb(TEXT_MUTED)
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.toggle_output_mute(index);
                                cx.notify();
                            }))
                            .child(
                                svg()
                                    .path(if output.muted { ICON_SOUND } else { ICON_MUTE })
                                    .size(px(13.))
                                    .text_color(if output.muted {
                                        rgb(TEXT)
                                    } else {
                                        rgb(TEXT_MUTED)
                                    }),
                            )
                            .child(if output.muted { "MUTED" } else { "MUTE" }),
                    ),
            )
    }

    fn render_fader(
        &self,
        target: FaderTarget,
        gain_db: f32,
        color: u32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let normalized = ((gain_db + 60.0) / 70.0).clamp(0.0, 1.0);
        div()
            .id(match target {
                FaderTarget::Source(index) => format!("source-fader-{index}"),
                FaderTarget::Output(index) => format!("output-fader-{index}"),
            })
            .w(px(34.))
            .h_full()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.start_fader_drag(target, event, cx);
                }),
            )
            .child(
                canvas(
                    move |bounds, _, _| (bounds, normalized, color),
                    |_, state, window, _| {
                        let (bounds, normalized, color) = state;
                        paint_fader(bounds, normalized, color, window);
                    },
                )
                .size_full(),
            )
    }
}

/// Paint the fader from its actual bounds so strips can stretch: track,
/// colored fill up to the value, then the knob cap on top.
fn paint_fader(bounds: Bounds<Pixels>, normalized: f32, color: u32, window: &mut Window) {
    let height: f32 = bounds.size.height.into();
    let origin_x: f32 = bounds.origin.x.into();
    let origin_y: f32 = bounds.origin.y.into();
    let travel = (height - 10.0 - 18.0).max(0.0);

    // Track.
    window.paint_quad(fill(
        Bounds {
            origin: point(px(origin_x + 14.0), px(origin_y + 5.0)),
            size: size(px(6.0), px(height - 10.0)),
        },
        rgb(BASE),
    ));
    // Value fill.
    let fill_h = normalized * (height - 10.0);
    if fill_h > 0.0 {
        window.paint_quad(fill(
            Bounds {
                origin: point(px(origin_x + 15.0), px(origin_y + height - 5.0 - fill_h)),
                size: size(px(4.0), px(fill_h)),
            },
            rgb(color),
        ));
    }
    // Knob cap with grip line.
    let knob_y = origin_y + 5.0 + (1.0 - normalized) * travel;
    window.paint_quad(quad(
        Bounds {
            origin: point(px(origin_x + 2.0), px(knob_y)),
            size: size(px(30.0), px(18.0)),
        },
        px(3.),
        rgb(TEXT),
        px(1.),
        rgb(TEXT_MUTED),
        BorderStyle::Solid,
    ));
    window.paint_quad(fill(
        Bounds {
            origin: point(px(origin_x + 6.0), px(knob_y + 8.0)),
            size: size(px(22.0), px(2.0)),
        },
        rgb(TEXT_MUTED),
    ));
}

/// Rotary knob for a strip parameter. Drag vertically to adjust (Shift for
/// fine steps), double-click resets to zero. Bipolar params (EQ dB) show the
/// zero detent at 12 o'clock; the delay starts at 7:30.
fn render_param_knob(
    target: KnobTarget,
    value: f32,
    color: u32,
    size: f32,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let (min, max) = match target.param {
        KnobParam::Delay => (0.0, MAX_DELAY_MS),
        _ => (orion_dsp::EQ_MIN_DB, orion_dsp::EQ_MAX_DB),
    };
    let fraction = ((value - min) / (max - min)).clamp(0.0, 1.0);
    let label = target.param.label();
    let knob_id = match (target.strip, target.param) {
        (FaderTarget::Source(i), p) => format!("knob-source-{i}-{}", p.label()),
        (FaderTarget::Output(i), p) => format!("knob-output-{i}-{}", p.label()),
    };
    let (value_text, tooltip) = match target.param {
        KnobParam::Delay => (
            format!("{value:.1} ms"),
            format!("Sync delay {value:.1} ms — drag to adjust, double-click to reset"),
        ),
        _ => (
            format!("{value:+.1}"),
            format!("{label} {value:+.1} dB — drag to adjust, double-click to reset"),
        ),
    };
    let active = match target.param {
        KnobParam::Delay => value.abs() > f32::EPSILON,
        _ => value.abs() > 0.05,
    };

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(2.))
        .child(
            div()
                .text_size(px(7.))
                .text_color(rgb(TEXT_FAINT))
                .child(label),
        )
        .child(
            div()
                .id(knob_id)
                .size(px(size))
                .cursor_pointer()
                .tooltip(text_tooltip(tooltip))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.start_knob_drag(target, event, cx);
                    }),
                )
                .child(
                    canvas(
                        move |bounds, _, _| (bounds, fraction, color, active),
                        |_, state, window, _| {
                            let (bounds, fraction, color, active) = state;
                            paint_knob(bounds, fraction, color, active, window);
                        },
                    )
                    .size_full(),
                ),
        )
        .child(
            div()
                .font_family(FONT_VALUES)
                .text_size(px(8.))
                .text_color(if active {
                    rgb(TEXT_MUTED)
                } else {
                    rgb(TEXT_FAINT)
                })
                .child(value_text),
        )
}

/// Knob section above the fader: 3-band EQ row, the sync delay knob, and the
/// channel-mode cycle pill.
fn render_knob_section(
    target: FaderTarget,
    delay_ms: f32,
    eq: &crate::state::EqBands,
    mode: orion::domain::ChannelMode,
    color: u32,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let mut knob = |param: KnobParam, value: f32, size: f32| {
        render_param_knob(
            KnobTarget {
                strip: target,
                param,
            },
            value,
            color,
            size,
            cx,
        )
    };
    div()
        .flex_shrink_0()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .px_2()
        .py_2()
        .border_t_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(knob(KnobParam::EqHigh, eq.high_db, 26.))
                .child(knob(KnobParam::EqMid, eq.mid_db, 26.))
                .child(knob(KnobParam::EqLow, eq.low_db, 26.)),
        )
        .child(knob(KnobParam::Delay, delay_ms, 36.))
        .child(
            // Channel mode cycle button, sized like the strip's MUTE button.
            div()
                .id(match target {
                    FaderTarget::Source(index) => format!("mode-source-{index}"),
                    FaderTarget::Output(index) => format!("mode-output-{index}"),
                })
                .w_full()
                .h(px(32.))
                .px_2()
                .flex()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded_sm()
                .border_1()
                .border_color(if mode == orion::domain::ChannelMode::Auto {
                    rgb(BORDER)
                } else {
                    rgb(color)
                })
                .text_size(px(8.))
                .font_family(FONT_VALUES)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if mode == orion::domain::ChannelMode::Auto {
                    rgb(TEXT_FAINT)
                } else {
                    rgb(color)
                })
                .cursor_pointer()
                .tooltip(text_tooltip(format!(
                    "Channel mode: {} — click to cycle (Auto, Stereo, Mono, Left, Right, Swap)",
                    mode.label()
                )))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.cycle_strip_mode(target, cx);
                }))
                .child(mode.label()),
        )
}

/// Knob dial geometry: zero sits at 135 degrees (7:30) and sweeps 270
/// degrees clockwise to 4:30 (screen coordinates: y-axis points down).
const KNOB_ZERO_DEG: f32 = 135.0;
const KNOB_SWEEP_DEG: f32 = 270.0;

fn knob_dial_point(center: Point<Pixels>, degrees: f32, radius: f32) -> Point<Pixels> {
    let radians = degrees.to_radians();
    point(
        center.x + px(radius * radians.cos()),
        center.y + px(radius * radians.sin()),
    )
}

fn knob_arc(
    center: Point<Pixels>,
    radius: f32,
    from_deg: f32,
    to_deg: f32,
    stroke: f32,
) -> PathBuilder {
    let mut builder = PathBuilder::stroke(px(stroke));
    builder.move_to(knob_dial_point(center, from_deg, radius));
    let mut angle = from_deg;
    // Lyon draws at most 180-degree arcs reliably; split longer sweeps.
    while (to_deg - angle) > 170.0 {
        angle += 170.0;
        builder.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            knob_dial_point(center, angle, radius),
        );
    }
    builder.arc_to(
        point(px(radius), px(radius)),
        px(0.),
        false,
        true,
        knob_dial_point(center, to_deg, radius),
    );
    builder
}

fn knob_circle(center: Point<Pixels>, radius: f32, builder: &mut PathBuilder) {
    builder.move_to(knob_dial_point(center, 0.0, radius));
    for to in [180.0, 360.0] {
        builder.arc_to(
            point(px(radius), px(radius)),
            px(0.),
            false,
            true,
            knob_dial_point(center, to, radius),
        );
    }
    builder.close();
}

/// Paint the knob: ticked track ring, value arc in the bus color, raised
/// body and a pointer needle. Runs on every frame for the knob's bounds.
fn paint_knob(
    bounds: Bounds<Pixels>,
    fraction: f32,
    color: u32,
    active: bool,
    window: &mut Window,
) {
    let size = bounds.size.width.min(bounds.size.height);
    let center = point(bounds.origin.x + size / 2.0, bounds.origin.y + size / 2.0);
    let ring_r = f32::from(size) / 2.0 - 3.5;
    let value_deg = KNOB_ZERO_DEG + fraction * KNOB_SWEEP_DEG;

    // Tick ring: 9 marks, active ones follow the bus color.
    for tick in 0..9 {
        let degrees = KNOB_ZERO_DEG + tick as f32 * (KNOB_SWEEP_DEG / 8.0);
        let tick_active = active && degrees <= value_deg + 0.5;
        let mut builder = PathBuilder::stroke(px(1.4));
        builder.move_to(knob_dial_point(center, degrees, ring_r - 4.0));
        builder.line_to(knob_dial_point(center, degrees, ring_r - 0.5));
        if let Ok(path) = builder.build() {
            window.paint_path(path, rgb(if tick_active { color } else { BORDER_STRONG }));
        }
    }

    // Value arc over the track.
    if fraction > 0.0 {
        if let Ok(path) = knob_arc(center, ring_r - 2.2, KNOB_ZERO_DEG, value_deg, 2.8).build() {
            window.paint_path(path, rgb(if active { color } else { TEXT_FAINT }));
        }
    }

    // Raised knob body with a subtle rim.
    let body_r = ring_r - 8.0;
    if let Ok(path) = {
        let mut builder = PathBuilder::fill();
        knob_circle(center, body_r, &mut builder);
        builder.build()
    } {
        window.paint_path(path, rgb(SURFACE_RAISED));
    }
    if let Ok(path) = {
        let mut builder = PathBuilder::stroke(px(1.));
        knob_circle(center, body_r, &mut builder);
        builder.build()
    } {
        window.paint_path(path, rgb(BORDER_STRONG));
    }

    // Pointer needle from the center toward the value.
    let mut needle = PathBuilder::stroke(px(2.4));
    needle.move_to(knob_dial_point(center, value_deg, 4.0));
    needle.line_to(knob_dial_point(center, value_deg, body_r - 2.5));
    if let Ok(path) = needle.build() {
        window.paint_path(path, rgb(if active { color } else { TEXT_MUTED }));
    }
}

fn render_meter(left: f32, right: f32, muted: bool) -> Div {
    let left = if muted { 0.0 } else { left };
    let right = if muted { 0.0 } else { right };
    // Column width and gap must mirror paint_meter so labels, bars and
    // readouts line up exactly.
    let column = |text: String, color: u32| {
        div()
            .w(px(30.))
            .flex()
            .justify_center()
            .text_size(px(8.))
            .font_family(FONT_VALUES)
            .text_color(rgb(color))
            .child(text)
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .py_1()
        .child(
            div()
                .flex()
                .gap(px(8.))
                .child(column("L".into(), TEXT_FAINT))
                .child(column("R".into(), TEXT_FAINT)),
        )
        .child(
            div().flex_1().min_h_0().w_full().pt_1().child(
                canvas(
                    move |bounds, _, _| (bounds, left, right),
                    |_, state, window, _| {
                        let (bounds, left, right) = state;
                        paint_meter(bounds, left, right, window);
                    },
                )
                .size_full(),
            ),
        )
        .child(
            div()
                .flex()
                .gap(px(8.))
                .pt_1()
                .child(column(format_level_db(amplitude_to_db(left)), TEXT_MUTED))
                .child(column(format_level_db(amplitude_to_db(right)), TEXT_MUTED)),
        )
}

/// Elastic segmented L/R meter painted from its real bounds: the segment
/// count follows the track height.
fn paint_meter(bounds: Bounds<Pixels>, left: f32, right: f32, window: &mut Window) {
    let height: f32 = bounds.size.height.into();
    let origin_x: f32 = bounds.origin.x.into();
    let origin_y: f32 = bounds.origin.y.into();
    let width: f32 = bounds.size.width.into();
    let segments = meter_segments_for_height(height);
    let slot = height / segments as f32;
    let bar_w = 30.0_f32.min((width - 8.0) / 2.0);
    let left_x = origin_x + (width - (bar_w * 2.0 + 8.0)) / 2.0;

    for (channel_x, level) in [(left_x, left), (left_x + bar_w + 8.0, right)] {
        let level_db = amplitude_to_db(level);
        for segment in 0..segments {
            let segment_db = segment_threshold_db(segment, segments);
            let active = level_db >= segment_db;
            let color = if segment >= segments - 2 {
                RED
            } else if segment >= segments * 3 / 4 {
                YELLOW
            } else {
                GREEN
            };
            let y = origin_y + height - (segment as f32 + 1.0) * slot;
            window.paint_quad(fill(
                Bounds {
                    origin: point(px(channel_x), px(y + 0.5)),
                    size: size(px(bar_w), px((slot - 1.0).max(1.0))),
                },
                rgb(if active { color } else { SURFACE_RAISED }),
            ));
        }
    }
}

/// Segment count for a track height: 3px bars with 1px gaps.
fn meter_segments_for_height(height: f32) -> usize {
    (((height + 1.0) / 4.0) as usize).clamp(8, 48)
}
pub(crate) fn truncate_label(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_owned()
    } else if max_chars <= 1 {
        "…".into()
    } else {
        let keep = max_chars.saturating_sub(1);
        let mut truncated: String = text.chars().take(keep).collect();
        truncated.push('…');
        truncated
    }
}

fn amplitude_to_db(amplitude: f32) -> f32 {
    if !amplitude.is_finite() || amplitude <= 0.0 {
        METER_FLOOR_DB
    } else {
        (20.0 * amplitude.max(1.0e-8).log10()).clamp(METER_FLOOR_DB, 0.0)
    }
}

/// Meters read -90..0 dB; the segment count follows the track height, so the
/// scale is deep enough to catch very quiet sources at any strip size.
const METER_FLOOR_DB: f32 = -90.0;

fn segment_threshold_db(segment: usize, segments: usize) -> f32 {
    METER_FLOOR_DB + (segment as f32 + 1.0) * (90.0 / segments as f32)
}

fn format_level_db(level_db: f32) -> String {
    if level_db <= METER_FLOOR_DB + 0.5 {
        " -∞".into()
    } else {
        format!("{level_db:>4.0}")
    }
}

pub(crate) fn channel_color(channel: &OutputBus) -> u32 {
    channel.color.value()
}

pub(crate) fn source_channel_color(channel: &SourceStrip) -> u32 {
    channel.color.value()
}

fn format_gain(gain_db: f32) -> String {
    if gain_db <= -59.9 {
        "-inf dB".into()
    } else {
        format!("{gain_db:.1} dB")
    }
}
