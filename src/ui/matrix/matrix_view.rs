use gpui::*;

pub struct MatrixView;

impl MatrixView {
    pub fn new() -> Self {
        Self
    }
}

impl Render for MatrixView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
