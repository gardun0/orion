use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ChannelId {
    Physical(String),
    Virtual(String),
}

impl ChannelId {
    pub fn label(&self) -> &str {
        match self {
            ChannelId::Physical(name) => name.as_str(),
            ChannelId::Virtual(name) => name.as_str(),
        }
    }

    pub fn is_virtual(&self) -> bool {
        matches!(self, ChannelId::Virtual(_))
    }
}

impl fmt::Display for ChannelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}
