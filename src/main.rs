mod assets;
mod process_stats;
mod state;
mod ui;

use std::{borrow::Cow, sync::Arc};

use gpui::*;
use gpui_platform::application;

fn main() {
    env_logger::init();
    application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            cx.set_app_identity("io.github.gardun0.orion", "Orion");
            cx.text_system()
                .add_fonts(vec![
                    Cow::Borrowed(
                        include_bytes!("../assets/fonts/Inter-Variable.ttf") as &'static [u8]
                    ),
                    Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Variable.ttf")
                        as &'static [u8]),
                ])
                .expect("failed to load Orion fonts");
            let app_icon = Arc::new(
                image::load_from_memory(include_bytes!(
                    "../assets/app-icon/io.github.gardun0.orion-512.png"
                ))
                .expect("failed to load Orion app icon")
                .into_rgba8(),
            );
            let bounds = Bounds::centered(None, size(px(1440.), px(860.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Orion".into()),
                        ..Default::default()
                    }),
                    app_id: Some("io.github.gardun0.orion".into()),
                    icon: Some(app_icon),
                    window_min_size: Some(size(px(1100.), px(680.))),
                    ..Default::default()
                },
                |_window, cx| cx.new(ui::RootView::new),
            )
            .unwrap();
            cx.activate(true);
        });
}
