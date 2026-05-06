use crate::model::channel::ChannelId;

#[derive(Clone, Debug)]
pub struct Route {
    pub src: ChannelId,
    pub dst: ChannelId,
    pub gain_linear: f32,
    pub enabled: bool,
}

impl Route {
    pub fn new(src: ChannelId, dst: ChannelId) -> Self {
        Self {
            src,
            dst,
            gain_linear: 1.0,
            enabled: true,
        }
    }

    pub fn gain_db(&self) -> f32 {
        20.0 * self.gain_linear.max(1e-10).log10()
    }
}
