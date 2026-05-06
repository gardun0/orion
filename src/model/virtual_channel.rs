#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

#[derive(Clone, Debug)]
pub struct VirtualChannel {
    pub name: String,
    pub direction: Direction,
    pub channel_count: u16,
}

impl VirtualChannel {
    pub fn input(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            direction: Direction::Input,
            channel_count: 2,
        }
    }

    pub fn output(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            direction: Direction::Output,
            channel_count: 2,
        }
    }
}

pub struct VirtualIoManager {
    pub channels: Vec<VirtualChannel>,
}

impl VirtualIoManager {
    pub fn with_defaults() -> Self {
        Self {
            channels: vec![
                VirtualChannel::input("Virtual Input 1"),
                VirtualChannel::input("Virtual Input 2"),
                VirtualChannel::output("Virtual Output 1"),
                VirtualChannel::output("Virtual Output 2"),
            ],
        }
    }

    pub fn inputs(&self) -> impl Iterator<Item = &VirtualChannel> {
        self.channels.iter().filter(|c| c.direction == Direction::Input)
    }

    pub fn outputs(&self) -> impl Iterator<Item = &VirtualChannel> {
        self.channels.iter().filter(|c| c.direction == Direction::Output)
    }

    pub fn add(&mut self, direction: Direction, name: &str) {
        self.channels.push(VirtualChannel {
            name: name.to_string(),
            direction,
            channel_count: 2,
        });
    }

    pub fn remove(&mut self, idx: usize) {
        if idx < self.channels.len() {
            self.channels.remove(idx);
        }
    }
}
