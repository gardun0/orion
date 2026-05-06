#[derive(Clone, Debug)]
pub struct DeviceDescriptor {
    pub name: String,
    pub is_input: bool,
    pub is_output: bool,
    pub default_sample_rate: u32,
    pub channel_count: u16,
}
