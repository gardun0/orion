use crate::model::MatrixSnapshot;

pub enum EngineCommand {
    UpdateMatrix(MatrixSnapshot),
    Shutdown,
}

pub enum EngineEvent {
    Started { sample_rate: u32, buffer_size: u32 },
    DeviceError(String),
}
