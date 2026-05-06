use gpui::*;

#[derive(IntoElement)]
pub struct MatrixCell;

impl RenderOnce for MatrixCell {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
    }
}
