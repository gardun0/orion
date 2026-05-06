use crate::model::MatrixSnapshot;

/// Mix input buffers into output buffers according to `snapshot`.
/// This function is called on the real-time thread — no allocation, no locking.
///
/// `inputs[i]`  = interleaved f32 samples for input channel i
/// `outputs[j]` = interleaved f32 samples for output channel j (pre-zeroed by caller)
pub fn mix(snapshot: &MatrixSnapshot, inputs: &[&[f32]], outputs: &mut Vec<Vec<f32>>) {
    for route in snapshot {
        if !route.enabled {
            continue;
        }
        // Phase 1: stub — real index lookup from ChannelId happens in Phase 2
        // when AudioEngine maps ChannelId → ring buffer index.
        let _ = inputs;
        let _ = outputs;
        let _ = route.gain_linear;
    }
}
