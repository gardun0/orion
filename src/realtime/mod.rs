//! Platform-neutral, destination-driven block engine.
//!
//! The engine owns all realtime audio state and processing. Platform
//! backends (PipeWire today; WASAPI/CoreAudio later) are thin adapters:
//! they discover endpoints, open native streams, convert their native
//! buffers to/from interleaved `f32` blocks at the engine's nominal rate,
//! and call [`SourceEngine::process`] / [`BusEngine::process`] from their
//! audio callbacks. Nominal sample-rate and format conversion belong to the
//! platform (PipeWire's adapter, the WASAPI audio engine, AUHAL); the engine
//! itself only performs small clock-drift correction between independently
//! clocked endpoints via [`orion_dsp::DriftCorrector`].
//!
//! Topology: one [`SourceEngine`] per source endpoint (capture callback),
//! one [`BusEngine`] per destination endpoint (playback callback). A route
//! is an SPSC ring plus a drift corrector linking the two; a bus sums every
//! route feeding its destination sample by sample. Structural changes
//! (connect/disconnect) travel as immutable plans swapped through
//! [`PlanSlot`]; retired plans are reclaimed on the backend thread once the
//! callback's completed generation proves no realtime reference survives.
//!
//! Realtime contract: after warm-up, `process` performs no allocations, no
//! locks, no logging, and no syscalls. All growing storage is created on the
//! backend thread and handed over through the inbox/outbox rings.

mod controls;
mod engine;
mod meter;
mod stage;

pub use controls::{ChannelControls, ControlHub, EndpointControls, MAX_DELAY_MS};
pub use engine::{
    BusEngine, BusHandle, BusInbox, BusPlan, PlanReclaimer, PlanSlot, Planned, RetiredConsumer,
    RetiredProducer, RouteFeed, RouteLink, SourceEngine, SourceHandle, SourceInbox, SourcePlan,
    MAX_ROUTES_PER_ENDPOINT,
};
pub use meter::RouteMeter;
pub use stage::{
    control_ramp_frames, corrector_target_frames, delay_line_capacity, ms_to_delay_frames,
    ring_capacity_frames, MAX_BLOCK_FRAMES, MAX_TARGET_QUANTA, TARGET_QUANTA,
};

#[cfg(test)]
mod tests;
