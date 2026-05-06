mod device;
mod engine;
mod model;
mod platform;
mod state;
mod ui;

use gpui::*;
use gpui_platform::application;

fn main() {
    env_logger::init();
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_window, cx| cx.new(|_| ui::RootView::new()),
        )
        .unwrap();
        cx.activate(true);
    });
}
