//! The destination-driven engine: [`SourceEngine`] runs in a source
//! endpoint's capture callback, [`BusEngine`] in a destination endpoint's
//! playback callback, and routes link them through SPSC rings with drift
//! correction. Everything here is platform-neutral; backends only convert
//! native buffers to/from interleaved `f32` blocks.
//!
//! Realtime contract: after warm-up, `process` never allocates, locks,
//! logs, or syscalls. Route ring halves, correctors, and scratch buffers are
//! allocated on the backend thread and handed over through bounded inbox
//! rings; retired halves travel back through outbox rings so reclamation
//! happens off the realtime thread. Topology changes are immutable plans
//! swapped through [`PlanSlot`] and reclaimed by generation: a retired plan
//! is only dropped once the callback reports completing a newer generation,
//! which proves no realtime-side reference to it survives.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use orion_dsp::{ChannelEq, DriftCorrector, DspError, EqCoefficients, ParameterSmoother};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::domain::{ChannelMode, RouteId};
use crate::realtime::controls::{ChannelControls, EndpointControls};
use crate::realtime::meter::RouteMeter;
use crate::realtime::stage::{
    apply_output_mode, apply_output_mode_crossfade, apply_sync_offset, capture_mode_frame,
    channel_gain, control_ramp_frames, corrector_target_frames, delay_line_capacity, refresh_eq,
    ring_capacity_frames, sanitize, MAX_BLOCK_FRAMES,
};

/// Largest number of routes feeding or reading one endpoint. Callback-side
/// collections are preallocated to this bound so they never grow in
/// realtime; the bound is far above any realistic patchbay.
pub const MAX_ROUTES_PER_ENDPOINT: usize = 32;
const INBOX_CAPACITY: usize = 64;
const OUTBOX_CAPACITY: usize = 64;

/// A plan that can be reclaimed by generation.
pub trait Planned {
    fn generation(&self) -> u64;
}

/// Shared plan slot between the backend thread (publisher) and one audio
/// callback (consumer). The callback reports the newest generation it has
/// *finished* with; the backend only reclaims retired plans at or below that
/// generation, so the last `Arc` drop can never land in the callback.
pub struct PlanSlot<P> {
    plan: ArcSwap<P>,
    completed_generation: AtomicU64,
}

impl<P> PlanSlot<P> {
    pub fn new(initial: P) -> Self {
        Self {
            plan: ArcSwap::from_pointee(initial),
            completed_generation: AtomicU64::new(0),
        }
    }

    /// Callback side: current plan.
    pub fn load(&self) -> Arc<P> {
        self.plan.load_full()
    }

    /// Callback side: report `generation` finished (call only after dropping
    /// the plan `Arc` it came from).
    pub fn complete(&self, generation: u64) {
        self.completed_generation
            .store(generation, Ordering::Release);
    }

    /// Backend side: publish the next plan, returning the retired one.
    pub fn publish(&self, next: P) -> Arc<P> {
        self.plan.swap(Arc::new(next))
    }

    /// Backend side: newest generation the callback has finished.
    pub fn completed(&self) -> u64 {
        self.completed_generation.load(Ordering::Acquire)
    }
}

/// Backend-side deferred reclamation for retired plans.
pub struct PlanReclaimer<P> {
    retired: Vec<Arc<P>>,
}

impl<P> Default for PlanReclaimer<P> {
    fn default() -> Self {
        Self {
            retired: Vec::new(),
        }
    }
}

impl<P: Planned> PlanReclaimer<P> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn retire(&mut self, plan: Arc<P>) {
        self.retired.push(plan);
    }

    /// Drop every retired plan the callback provably no longer references.
    pub fn collect(&mut self, slot: &PlanSlot<P>) {
        let completed = slot.completed();
        self.retired.retain(|plan| plan.generation() > completed);
    }
}

/// What a source must deliver for one route: the bus layout it maps into.
#[derive(Clone, Copy, Debug)]
pub struct RouteFeed {
    pub route_id: RouteId,
    pub bus_channels: usize,
}

/// Immutable source topology, swapped whole on connect/disconnect.
#[derive(Debug)]
pub struct SourcePlan {
    pub generation: u64,
    pub feeds: Vec<RouteFeed>,
}

impl Planned for SourcePlan {
    fn generation(&self) -> u64 {
        self.generation
    }
}

/// Immutable bus topology: the routes to pull and sum.
#[derive(Debug)]
pub struct BusPlan {
    pub generation: u64,
    pub routes: Vec<RouteId>,
}

impl Planned for BusPlan {
    fn generation(&self) -> u64 {
        self.generation
    }
}

/// Backend → callback handoff for the producing half of a route ring. The
/// smoothers are seeded on the backend thread so nothing is constructed in
/// the callback; `generation` ties the delivery to the plan that lists the
/// route, so out-of-order visibility resolves deterministically.
pub enum SourceInbox {
    Add {
        generation: u64,
        route_id: RouteId,
        producer: Producer<f32>,
        balance_left: ParameterSmoother,
        balance_right: ParameterSmoother,
    },
}

/// Backend → callback handoff for the consuming half of a route ring.
pub enum BusInbox {
    Add {
        generation: u64,
        route_id: RouteId,
        consumer: Consumer<f32>,
        corrector: DriftCorrector,
        scratch: Vec<f32>,
    },
}

/// Callback → backend retirement of ring halves; dropped on the backend
/// thread, never in realtime.
pub struct RetiredProducer {
    #[allow(dead_code)]
    pub route_id: RouteId,
    pub producer: Producer<f32>,
}

pub struct RetiredConsumer {
    #[allow(dead_code)]
    pub route_id: RouteId,
    pub consumer: Consumer<f32>,
    #[allow(dead_code)]
    pub corrector: DriftCorrector,
    pub scratch: Vec<f32>,
}

/// Backend-side bundle of everything a route needs at both ends. Created
/// (and allocated) entirely on the backend thread.
pub struct RouteLink {
    pub route_id: RouteId,
    pub bus_channels: usize,
    pub producer: Producer<f32>,
    pub consumer: Consumer<f32>,
    pub corrector: DriftCorrector,
    pub scratch: Vec<f32>,
}

impl RouteLink {
    pub fn new(
        route_id: RouteId,
        bus_channels: usize,
        quantum_frames: u32,
        target_quanta: u32,
        sample_rate: u32,
    ) -> Result<Self, DspError> {
        let capacity = ring_capacity_frames(quantum_frames).saturating_mul(bus_channels.max(1));
        let (producer, consumer) = RingBuffer::new(capacity);
        let corrector = DriftCorrector::new(
            bus_channels,
            corrector_target_frames(quantum_frames, target_quanta),
            sample_rate,
        )?;
        Ok(Self {
            route_id,
            bus_channels,
            producer,
            consumer,
            corrector,
            scratch: vec![0.0; MAX_BLOCK_FRAMES * bus_channels],
        })
    }

    /// Split into the two inbox deliveries. Each side's plan slot has its
    /// own generation counter, and each delivery is tagged with the
    /// generation of the plan that lists the route, so out-of-order
    /// visibility resolves deterministically. `balance` seeds the route's
    /// per-channel gain smoothers (the source's current balance).
    pub fn into_halves(
        self,
        source_generation: u64,
        bus_generation: u64,
        balance: (f32, f32),
    ) -> Result<(SourceInbox, BusInbox), DspError> {
        let source = SourceInbox::Add {
            generation: source_generation,
            route_id: self.route_id,
            producer: self.producer,
            balance_left: ParameterSmoother::new(balance.0)?,
            balance_right: ParameterSmoother::new(balance.1)?,
        };
        let bus = BusInbox::Add {
            generation: bus_generation,
            route_id: self.route_id,
            consumer: self.consumer,
            corrector: self.corrector,
            scratch: self.scratch,
        };
        Ok((source, bus))
    }
}

struct ActiveFeed {
    route_id: RouteId,
    bus_channels: usize,
    producer: Producer<f32>,
    balance_left: ParameterSmoother,
    balance_right: ParameterSmoother,
    /// Channel mode this feed currently maps with, plus an in-flight blend
    /// from the previous mode when the control changes (click-free switch).
    active_mode: ChannelMode,
    mode_fade: Option<(ChannelMode, ParameterSmoother)>,
}

struct PendingFeed {
    generation: u64,
    route_id: RouteId,
    producer: Producer<f32>,
    balance_left: ParameterSmoother,
    balance_right: ParameterSmoother,
}

/// Backend-side handle for a running source: inbox/outbox halves, the plan
/// slot, and the reclamation list. Owned by the platform adapter.
pub struct SourceHandle {
    pub inbox: Producer<SourceInbox>,
    pub retired: Consumer<RetiredProducer>,
    pub plan_slot: Arc<PlanSlot<SourcePlan>>,
    pub reclaimer: PlanReclaimer<SourcePlan>,
}

/// Backend-side plan management for one source endpoint, shared by all
/// platform adapters: tracks the published feed list and generations, keeps
/// plan-before-inbox ordering, and reclaims retired plans only after the
/// callback has completed them.
pub struct SourcePublisher {
    handle: SourceHandle,
    generation: u64,
    feeds: Vec<(RouteId, usize)>,
}

impl SourcePublisher {
    pub fn new(handle: SourceHandle) -> Self {
        Self {
            handle,
            generation: 0,
            feeds: Vec::new(),
        }
    }

    pub fn has_routes(&self) -> bool {
        !self.feeds.is_empty()
    }

    /// Generation the next published plan will carry (tags inbox deliveries).
    pub fn next_generation(&self) -> u64 {
        self.generation + 1
    }

    /// Publish a plan listing `route_id`, then deliver the ring half. On an
    /// inbox overflow the plan rolls back so the callback never waits for a
    /// half that will not arrive.
    pub fn add_feed(
        &mut self,
        route_id: RouteId,
        bus_channels: usize,
        item: SourceInbox,
    ) -> Result<(), rtrb::PushError<SourceInbox>> {
        self.feeds.push((route_id, bus_channels));
        self.publish();
        match self.handle.inbox.push(item) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.feeds.retain(|(id, _)| *id != route_id);
                self.publish();
                Err(error)
            }
        }
    }

    pub fn remove_feed(&mut self, route_id: RouteId) {
        self.feeds.retain(|(id, _)| *id != route_id);
        self.publish();
    }

    /// Drain retired ring halves and reclaim plans the callback finished
    /// with; drops happen here, on the backend thread.
    pub fn collect_garbage(&mut self) {
        while self.handle.retired.pop().is_ok() {}
        self.handle.reclaimer.collect(&self.handle.plan_slot);
    }

    fn publish(&mut self) {
        self.generation += 1;
        let old = self.handle.plan_slot.publish(SourcePlan {
            generation: self.generation,
            feeds: self
                .feeds
                .iter()
                .map(|(route_id, bus_channels)| RouteFeed {
                    route_id: *route_id,
                    bus_channels: *bus_channels,
                })
                .collect(),
        });
        self.handle.reclaimer.retire(old);
    }
}

/// Capture-side engine for one source endpoint: meter the raw input with
/// the strip's instantaneous controls, then gain/mute (smoothed), EQ, and
/// sync delay in the source layout, then fan out into every route ring with
/// the source's channel mode and balance applied in each bus's layout.
pub struct SourceEngine {
    channels: usize,
    ramp_frames: usize,
    controls: Arc<EndpointControls>,
    channel_controls: Arc<ChannelControls>,
    gain: ParameterSmoother,
    audible: ParameterSmoother,
    eqs: Vec<ChannelEq>,
    eq_current: Arc<EqCoefficients>,
    delay_line: Vec<f32>,
    delay_write: usize,
    scratch: Vec<f32>,
    meter: Arc<RouteMeter>,
    feeds: Vec<ActiveFeed>,
    pending: Vec<PendingFeed>,
    retiring: Vec<RetiredProducer>,
    inbox: Consumer<SourceInbox>,
    outbox: Producer<RetiredProducer>,
    plan: Arc<PlanSlot<SourcePlan>>,
    /// Channel mode currently reported by the endpoint control; compared
    /// per block to start feed crossfades.
    active_mode: ChannelMode,
}

impl SourceEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channels: usize,
        rate: u32,
        controls: Arc<EndpointControls>,
        channel_controls: Arc<ChannelControls>,
        meter: Arc<RouteMeter>,
        plan_slot: Arc<PlanSlot<SourcePlan>>,
    ) -> Result<(Self, SourceHandle), DspError> {
        if !(1..=orion_dsp::MAX_CHANNELS).contains(&channels) {
            return Err(DspError::InvalidChannelCount(channels));
        }
        let (inbox_tx, inbox_rx) = RingBuffer::new(INBOX_CAPACITY);
        let (outbox_tx, outbox_rx) = RingBuffer::new(OUTBOX_CAPACITY);
        let active_mode = ChannelMode::from_code(channel_controls.mode.load(Ordering::Relaxed));
        let engine = Self {
            channels,
            ramp_frames: control_ramp_frames(rate),
            gain: ParameterSmoother::new(controls.gain_target())?,
            audible: ParameterSmoother::new(if controls.is_muted() { 0.0 } else { 1.0 })?,
            controls,
            channel_controls,
            eqs: vec![ChannelEq::default(); channels],
            eq_current: Arc::new(EqCoefficients::default()),
            delay_line: vec![0.0; delay_line_capacity(rate, channels)],
            delay_write: 0,
            scratch: vec![0.0; MAX_BLOCK_FRAMES * channels],
            meter,
            feeds: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            pending: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            retiring: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            inbox: inbox_rx,
            outbox: outbox_tx,
            plan: plan_slot.clone(),
            active_mode,
        };
        let handle = SourceHandle {
            inbox: inbox_tx,
            retired: outbox_rx,
            plan_slot,
            reclaimer: PlanReclaimer::new(),
        };
        Ok((engine, handle))
    }

    /// Process one captured block (interleaved, `channels` wide). Chunks
    /// internally so any callback size works with bounded scratch.
    pub fn process(&mut self, input: &[f32]) {
        let plan = self.plan.load();
        let generation = plan.generation;
        self.drain_inbox(&plan);
        self.reconcile(&plan);
        self.flush_retiring();
        self.refresh_targets();

        let channels = self.channels;
        let reported_mode =
            ChannelMode::from_code(self.channel_controls.mode.load(Ordering::Relaxed));
        if reported_mode != self.active_mode {
            // Click-free mode switch: every feed blends old -> new mapping
            // over one control ramp instead of hard-switching at a frame.
            let previous = self.active_mode;
            self.active_mode = reported_mode;
            for feed in self.feeds.iter_mut() {
                let mut mix = ParameterSmoother::new(0.0).expect("zero is finite");
                let _ = mix.set_target(1.0, self.ramp_frames);
                feed.mode_fade = Some((previous, mix));
                feed.active_mode = reported_mode;
            }
        }
        let offset_samples = (self.channel_controls.delay_frames.load(Ordering::Relaxed) as usize)
            .saturating_mul(channels)
            .min(self.delay_line.len().saturating_sub(1));
        refresh_eq(&mut self.eqs, &mut self.eq_current, &self.channel_controls);
        let meter_gain = self.controls.gain_linear();
        let (meter_left, meter_right) = self.controls.balance_gains();

        let chunk_len = MAX_BLOCK_FRAMES * channels;
        for chunk in input.chunks(chunk_len) {
            let frames = chunk.len() / channels;
            let samples = frames * channels;
            // Defensive: never touch a partial trailing frame (adapters
            // deliver whole frames; a bug must not panic in realtime).
            let scratch = &mut self.scratch[..samples];
            scratch.copy_from_slice(&chunk[..samples]);
            for sample in scratch.iter_mut() {
                *sample = sanitize(*sample);
            }
            // Meter the source's own level with the instantaneous controls:
            // the meter tracks the fader, not the audio ramp. Peak per
            // sample; RMS and clip merged once per block.
            let mut sum_squares = [0.0_f64; orion_dsp::MAX_CHANNELS];
            let mut clipped = [false; orion_dsp::MAX_CHANNELS];
            for frame in 0..frames {
                for channel in 0..channels {
                    let sample = scratch[frame * channels + channel]
                        * meter_gain
                        * channel_gain(channel, meter_left, meter_right);
                    self.meter.observe(channel, sample);
                    sum_squares[channel] += f64::from(sample) * f64::from(sample);
                    clipped[channel] |= sample.abs() >= 1.0;
                }
            }
            for channel in 0..channels {
                self.meter.merge_block_stats(
                    channel,
                    sum_squares[channel],
                    frames as u64,
                    clipped[channel],
                );
            }
            // Gain and mute as cascaded ramps (click-free by construction).
            for frame in 0..frames {
                let gain = self.gain.next_value() * self.audible.next_value();
                for channel in 0..channels {
                    scratch[frame * channels + channel] *= gain;
                }
            }
            // Source EQ, then the source sync delay (both linear, so running
            // them pre-mapping in the source layout is equivalent).
            for (channel, eq) in self.eqs.iter_mut().enumerate() {
                for frame in 0..frames {
                    let index = frame * channels + channel;
                    scratch[index] = eq.process(scratch[index]);
                }
            }
            if offset_samples > 0 {
                for sample in scratch.iter_mut() {
                    *sample = apply_sync_offset(
                        *sample,
                        &mut self.delay_line,
                        &mut self.delay_write,
                        offset_samples,
                    );
                }
            }
            // Fan out: map into each bus's layout with the source's channel
            // mode (blending across a mode switch), then apply the source
            // balance in the bus layout.
            for feed in self.feeds.iter_mut() {
                let bus_channels = feed.bus_channels;
                let active_mode = feed.active_mode;
                for frame in 0..frames {
                    if feed.producer.slots() < bus_channels {
                        break;
                    }
                    let left = feed.balance_left.next_value();
                    let right = feed.balance_right.next_value();
                    let source_frame = &scratch[frame * channels..(frame + 1) * channels];
                    // During a crossfade both mappings are computed and
                    // blended; otherwise the fast path is a single mapping.
                    let fade = feed
                        .mode_fade
                        .as_mut()
                        .map(|(previous, mix)| (*previous, mix.next_value()));
                    for bus_channel in 0..bus_channels {
                        let mapped = match fade {
                            Some((previous, t)) => {
                                let old = capture_mode_frame(
                                    previous,
                                    source_frame,
                                    bus_channel,
                                    bus_channels,
                                );
                                let new = capture_mode_frame(
                                    active_mode,
                                    source_frame,
                                    bus_channel,
                                    bus_channels,
                                );
                                old + (new - old) * t
                            }
                            None => capture_mode_frame(
                                active_mode,
                                source_frame,
                                bus_channel,
                                bus_channels,
                            ),
                        };
                        let _ = feed
                            .producer
                            .push(mapped * channel_gain(bus_channel, left, right));
                    }
                }
                // Retire a completed fade so the fast path resumes.
                if feed
                    .mode_fade
                    .as_ref()
                    .is_some_and(|(_, mix)| !mix.is_smoothing())
                {
                    feed.mode_fade = None;
                }
            }
        }

        drop(plan);
        self.plan.complete(generation);
    }

    fn refresh_targets(&mut self) {
        let gain = self.controls.gain_target();
        if gain != self.gain.target() {
            let _ = self.gain.set_target(gain, self.ramp_frames);
        }
        let audible = if self.controls.is_muted() { 0.0 } else { 1.0 };
        if audible != self.audible.target() {
            let _ = self.audible.set_target(audible, self.ramp_frames);
        }
        let (left, right) = self.controls.balance_gains();
        for feed in self.feeds.iter_mut() {
            if left != feed.balance_left.target() {
                let _ = feed.balance_left.set_target(left, self.ramp_frames);
            }
            if right != feed.balance_right.target() {
                let _ = feed.balance_right.set_target(right, self.ramp_frames);
            }
        }
    }

    fn drain_inbox(&mut self, plan: &SourcePlan) {
        // `pending` is preallocated; leave surplus items queued in the inbox
        // for the next block rather than growing a collection in realtime.
        while self.pending.len() < MAX_ROUTES_PER_ENDPOINT {
            let Ok(SourceInbox::Add {
                generation,
                route_id,
                producer,
                balance_left,
                balance_right,
            }) = self.inbox.pop()
            else {
                break;
            };
            self.pending.push(PendingFeed {
                generation,
                route_id,
                producer,
                balance_left,
                balance_right,
            });
        }
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].generation > plan.generation {
                index += 1;
                continue;
            }
            let pending = self.pending.swap_remove(index);
            let bus_channels = plan
                .feeds
                .iter()
                .find(|feed| feed.route_id == pending.route_id)
                .map(|feed| feed.bus_channels);
            match bus_channels {
                Some(bus_channels) => self.activate_feed(pending, bus_channels),
                // The plan already moved past this route (fast disconnect):
                // retire the half without ever activating it.
                None => self.retire_producer(pending.route_id, pending.producer),
            }
        }
    }

    fn activate_feed(&mut self, pending: PendingFeed, bus_channels: usize) {
        if let Some(index) = self
            .feeds
            .iter()
            .position(|feed| feed.route_id == pending.route_id)
        {
            // Replacement (stream rebuild): retire the old half.
            let old = self.feeds.swap_remove(index);
            self.retire_producer(old.route_id, old.producer);
        }
        if self.feeds.len() < MAX_ROUTES_PER_ENDPOINT {
            self.feeds.push(ActiveFeed {
                route_id: pending.route_id,
                bus_channels,
                producer: pending.producer,
                balance_left: pending.balance_left,
                balance_right: pending.balance_right,
                // A newly linked route starts in the current mode; it never
                // fades in from a stale one.
                active_mode: self.active_mode,
                mode_fade: None,
            });
        } else {
            self.retire_producer(pending.route_id, pending.producer);
        }
    }

    /// Retire active feeds the plan no longer lists (disconnected routes).
    fn reconcile(&mut self, plan: &SourcePlan) {
        let mut index = 0;
        while index < self.feeds.len() {
            let active = plan
                .feeds
                .iter()
                .any(|feed| feed.route_id == self.feeds[index].route_id);
            if active {
                index += 1;
            } else {
                let feed = self.feeds.swap_remove(index);
                self.retire_producer(feed.route_id, feed.producer);
            }
        }
    }

    fn retire_producer(&mut self, route_id: RouteId, producer: Producer<f32>) {
        if self.retiring.len() < MAX_ROUTES_PER_ENDPOINT {
            self.retiring.push(RetiredProducer { route_id, producer });
        }
        // Full retirement queue is unreachable: it is bounded by the route
        // count and drained every block.
    }

    fn flush_retiring(&mut self) {
        let mut index = 0;
        while index < self.retiring.len() {
            let retired = self.retiring.swap_remove(index);
            match self.outbox.push(retired) {
                Ok(()) => {}
                // Outbox full (backend not draining): hold and retry next
                // block; the queue is preallocated so this never allocates.
                Err(rtrb::PushError::Full(retired)) => self.retiring.push(retired),
            }
            index += 1;
        }
    }
}

struct ActiveDraw {
    route_id: RouteId,
    consumer: Consumer<f32>,
    corrector: DriftCorrector,
    scratch: Vec<f32>,
}

struct PendingDraw {
    generation: u64,
    route_id: RouteId,
    consumer: Consumer<f32>,
    corrector: DriftCorrector,
    scratch: Vec<f32>,
}

/// Backend-side handle for a running bus.
pub struct BusHandle {
    pub inbox: Producer<BusInbox>,
    pub retired: Consumer<RetiredConsumer>,
    pub plan_slot: Arc<PlanSlot<BusPlan>>,
    pub reclaimer: PlanReclaimer<BusPlan>,
}

/// Backend-side plan management for one destination endpoint (the bus),
/// shared by all platform adapters. Same contract as [`SourcePublisher`].
pub struct BusPublisher {
    handle: BusHandle,
    generation: u64,
    routes: Vec<RouteId>,
}

impl BusPublisher {
    pub fn new(handle: BusHandle) -> Self {
        Self {
            handle,
            generation: 0,
            routes: Vec::new(),
        }
    }

    pub fn has_routes(&self) -> bool {
        !self.routes.is_empty()
    }

    pub fn next_generation(&self) -> u64 {
        self.generation + 1
    }

    /// The Err payload returns the undeliverable ring half to the caller —
    /// its size is the point.
    #[allow(clippy::result_large_err)]
    pub fn add_draw(
        &mut self,
        route_id: RouteId,
        item: BusInbox,
    ) -> Result<(), rtrb::PushError<BusInbox>> {
        self.routes.push(route_id);
        self.publish();
        match self.handle.inbox.push(item) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.routes.retain(|id| *id != route_id);
                self.publish();
                Err(error)
            }
        }
    }

    pub fn remove_draw(&mut self, route_id: RouteId) {
        self.routes.retain(|id| *id != route_id);
        self.publish();
    }

    pub fn collect_garbage(&mut self) {
        while self.handle.retired.pop().is_ok() {}
        self.handle.reclaimer.collect(&self.handle.plan_slot);
    }

    fn publish(&mut self) {
        self.generation += 1;
        let old = self.handle.plan_slot.publish(BusPlan {
            generation: self.generation,
            routes: self.routes.clone(),
        });
        self.handle.reclaimer.retire(old);
    }
}

/// Playback-side engine for one destination endpoint: pull every routed
/// source through its drift corrector, sum sample by sample, then apply the
/// bus strip (balance, gain/mute, sync delay, EQ, output mode) and meter
/// what actually plays.
pub struct BusEngine {
    channels: usize,
    ramp_frames: usize,
    controls: Arc<EndpointControls>,
    channel_controls: Arc<ChannelControls>,
    gain: ParameterSmoother,
    audible: ParameterSmoother,
    balance_left: ParameterSmoother,
    balance_right: ParameterSmoother,
    eqs: Vec<ChannelEq>,
    eq_current: Arc<EqCoefficients>,
    delay_line: Vec<f32>,
    delay_write: usize,
    accumulator: Vec<f32>,
    meter: Arc<RouteMeter>,
    draws: Vec<ActiveDraw>,
    pending: Vec<PendingDraw>,
    retiring: Vec<RetiredConsumer>,
    inbox: Consumer<BusInbox>,
    outbox: Producer<RetiredConsumer>,
    plan: Arc<PlanSlot<BusPlan>>,
    /// Channel mode currently applied to the output, plus an in-flight blend
    /// from the previous mode when the control changes (click-free switch).
    active_mode: ChannelMode,
    mode_fade: Option<(ChannelMode, ParameterSmoother)>,
}

impl BusEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        channels: usize,
        rate: u32,
        controls: Arc<EndpointControls>,
        channel_controls: Arc<ChannelControls>,
        meter: Arc<RouteMeter>,
        plan_slot: Arc<PlanSlot<BusPlan>>,
    ) -> Result<(Self, BusHandle), DspError> {
        if !(1..=orion_dsp::MAX_CHANNELS).contains(&channels) {
            return Err(DspError::InvalidChannelCount(channels));
        }
        let (inbox_tx, inbox_rx) = RingBuffer::new(INBOX_CAPACITY);
        let (outbox_tx, outbox_rx) = RingBuffer::new(OUTBOX_CAPACITY);
        let (balance_left, balance_right) = controls.balance_gains();
        let active_mode = ChannelMode::from_code(channel_controls.mode.load(Ordering::Relaxed));
        let engine = Self {
            channels,
            ramp_frames: control_ramp_frames(rate),
            gain: ParameterSmoother::new(controls.gain_target())?,
            audible: ParameterSmoother::new(if controls.is_muted() { 0.0 } else { 1.0 })?,
            balance_left: ParameterSmoother::new(balance_left)?,
            balance_right: ParameterSmoother::new(balance_right)?,
            controls,
            channel_controls,
            eqs: vec![ChannelEq::default(); channels],
            eq_current: Arc::new(EqCoefficients::default()),
            delay_line: vec![0.0; delay_line_capacity(rate, channels)],
            delay_write: 0,
            accumulator: vec![0.0; MAX_BLOCK_FRAMES * channels],
            meter,
            draws: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            pending: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            retiring: Vec::with_capacity(MAX_ROUTES_PER_ENDPOINT),
            inbox: inbox_rx,
            outbox: outbox_tx,
            plan: plan_slot.clone(),
            active_mode,
            mode_fade: None,
        };
        let handle = BusHandle {
            inbox: inbox_tx,
            retired: outbox_rx,
            plan_slot,
            reclaimer: PlanReclaimer::new(),
        };
        Ok((engine, handle))
    }

    /// Render one playback block into `output` (interleaved, `channels`
    /// wide). Chunks internally so any callback size works.
    pub fn process(&mut self, output: &mut [f32]) {
        let plan = self.plan.load();
        let generation = plan.generation;
        self.drain_inbox(&plan);
        self.reconcile(&plan);
        self.flush_retiring();
        self.refresh_targets();

        let channels = self.channels;
        let reported_mode =
            ChannelMode::from_code(self.channel_controls.mode.load(Ordering::Relaxed));
        if reported_mode != self.active_mode {
            // Mode reshaping only exists for stereo buses; start the blend.
            if channels == 2 {
                let mut mix = ParameterSmoother::new(0.0).expect("zero is finite");
                let _ = mix.set_target(1.0, self.ramp_frames);
                self.mode_fade = Some((self.active_mode, mix));
            }
            self.active_mode = reported_mode;
        }
        let offset_samples = (self.channel_controls.delay_frames.load(Ordering::Relaxed) as usize)
            .saturating_mul(channels)
            .min(self.delay_line.len().saturating_sub(1));
        refresh_eq(&mut self.eqs, &mut self.eq_current, &self.channel_controls);

        let chunk_len = MAX_BLOCK_FRAMES * channels;
        for chunk in output.chunks_mut(chunk_len) {
            let frames = chunk.len() / channels;
            let samples = frames * channels;
            // Defensive: never touch a partial trailing frame (adapters
            // deliver whole frames; a bug must not panic in realtime).
            let chunk = &mut chunk[..samples];
            self.accumulator[..samples].fill(0.0);
            // Sum every route: pull through the corrector at the bus clock,
            // then accumulate sample by sample.
            for draw in self.draws.iter_mut() {
                let occupancy = draw.consumer.slots() / channels;
                {
                    let consumer = &mut draw.consumer;
                    draw.corrector.process(
                        &mut draw.scratch[..samples],
                        channels,
                        occupancy,
                        &mut || consumer.pop().ok(),
                    );
                }
                for (mixed, routed) in self.accumulator[..samples]
                    .iter_mut()
                    .zip(draw.scratch[..samples].iter())
                {
                    *mixed += routed;
                }
            }
            // Bus strip: balance and gain/mute as cascaded smoothed ramps.
            for frame in 0..frames {
                let left = self.balance_left.next_value();
                let right = self.balance_right.next_value();
                let gain = self.gain.next_value() * self.audible.next_value();
                for channel in 0..channels {
                    self.accumulator[frame * channels + channel] *=
                        gain * channel_gain(channel, left, right);
                }
            }
            // Bus sync delay, then bus EQ (legacy playback order).
            if offset_samples > 0 {
                for sample in self.accumulator[..samples].iter_mut() {
                    *sample = apply_sync_offset(
                        *sample,
                        &mut self.delay_line,
                        &mut self.delay_write,
                        offset_samples,
                    );
                }
            }
            for (channel, eq) in self.eqs.iter_mut().enumerate() {
                for frame in 0..frames {
                    let index = frame * channels + channel;
                    self.accumulator[index] = eq.process(self.accumulator[index]);
                }
            }
            // The always-on saturator bounds the summed bus at ±1.0
            // (identity below -1 dBFS) — this is the post-sum protection
            // point, so it sees the mixed signal, not individual routes.
            // It also maps non-finite values to silence. The meter hears
            // what actually plays, but clipping is detected *before* the
            // saturator so driving the bus stays visible.
            let mut sum_squares = [0.0_f64; orion_dsp::MAX_CHANNELS];
            let mut clipped = [false; orion_dsp::MAX_CHANNELS];
            for frame in 0..frames {
                for channel in 0..channels {
                    let raw = self.accumulator[frame * channels + channel];
                    clipped[channel] |= raw.abs() >= 1.0;
                    let sample = orion_dsp::soft_clip(raw);
                    self.meter.observe(channel, sample);
                    sum_squares[channel] += f64::from(sample) * f64::from(sample);
                    chunk[frame * channels + channel] = sample;
                }
            }
            for channel in 0..channels {
                self.meter.merge_block_stats(
                    channel,
                    sum_squares[channel],
                    frames as u64,
                    clipped[channel],
                );
            }
            // Output channel mode: blend old -> new across a switch so the
            // reshape never clicks; retire the fade once the ramp lands.
            let mut fade_finished = false;
            if let Some((previous, mix)) = &mut self.mode_fade {
                apply_output_mode_crossfade(*previous, self.active_mode, mix, chunk);
                fade_finished = !mix.is_smoothing();
            } else {
                apply_output_mode(self.active_mode, chunk, channels);
            }
            if fade_finished {
                self.mode_fade = None;
            }
        }

        drop(plan);
        self.plan.complete(generation);
    }

    fn refresh_targets(&mut self) {
        let gain = self.controls.gain_target();
        if gain != self.gain.target() {
            let _ = self.gain.set_target(gain, self.ramp_frames);
        }
        let audible = if self.controls.is_muted() { 0.0 } else { 1.0 };
        if audible != self.audible.target() {
            let _ = self.audible.set_target(audible, self.ramp_frames);
        }
        let (left, right) = self.controls.balance_gains();
        if left != self.balance_left.target() {
            let _ = self.balance_left.set_target(left, self.ramp_frames);
        }
        if right != self.balance_right.target() {
            let _ = self.balance_right.set_target(right, self.ramp_frames);
        }
    }

    /// Test-only visibility into drift correction: (consumption ratio, ring
    /// occupancy in frames) per active route.
    #[cfg(test)]
    pub(crate) fn draw_debug(&self) -> Vec<(f64, usize)> {
        self.draws
            .iter()
            .map(|draw| {
                (
                    draw.corrector.ratio(),
                    draw.consumer.slots() / self.channels,
                )
            })
            .collect()
    }

    fn drain_inbox(&mut self, plan: &BusPlan) {
        // `pending` is preallocated; surplus items stay queued for the next
        // block rather than growing a collection in realtime.
        while self.pending.len() < MAX_ROUTES_PER_ENDPOINT {
            let Ok(BusInbox::Add {
                generation,
                route_id,
                consumer,
                corrector,
                scratch,
            }) = self.inbox.pop()
            else {
                break;
            };
            self.pending.push(PendingDraw {
                generation,
                route_id,
                consumer,
                corrector,
                scratch,
            });
        }
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].generation > plan.generation {
                index += 1;
                continue;
            }
            let pending = self.pending.swap_remove(index);
            if plan.routes.contains(&pending.route_id) {
                self.activate_draw(pending);
            } else {
                self.retire_draw(PendingDraw::into_retired(pending));
            }
        }
    }

    fn activate_draw(&mut self, pending: PendingDraw) {
        if let Some(index) = self
            .draws
            .iter()
            .position(|draw| draw.route_id == pending.route_id)
        {
            let old = self.draws.swap_remove(index);
            self.retire_draw(PendingDraw::from_active(old));
        }
        if self.draws.len() < MAX_ROUTES_PER_ENDPOINT {
            self.draws.push(ActiveDraw {
                route_id: pending.route_id,
                consumer: pending.consumer,
                corrector: pending.corrector,
                scratch: pending.scratch,
            });
        } else {
            self.retire_draw(PendingDraw::into_retired(pending));
        }
    }

    fn reconcile(&mut self, plan: &BusPlan) {
        let mut index = 0;
        while index < self.draws.len() {
            if plan.routes.contains(&self.draws[index].route_id) {
                index += 1;
            } else {
                let draw = self.draws.swap_remove(index);
                self.retire_draw(RetiredConsumer {
                    route_id: draw.route_id,
                    consumer: draw.consumer,
                    corrector: draw.corrector,
                    scratch: draw.scratch,
                });
            }
        }
    }

    fn retire_draw(&mut self, retired: RetiredConsumer) {
        if self.retiring.len() < MAX_ROUTES_PER_ENDPOINT {
            self.retiring.push(retired);
        }
    }

    fn flush_retiring(&mut self) {
        let mut index = 0;
        while index < self.retiring.len() {
            let retired = self.retiring.swap_remove(index);
            match self.outbox.push(retired) {
                Ok(()) => {}
                Err(rtrb::PushError::Full(retired)) => self.retiring.push(retired),
            }
            index += 1;
        }
    }
}

impl PendingDraw {
    fn into_retired(pending: PendingDraw) -> RetiredConsumer {
        RetiredConsumer {
            route_id: pending.route_id,
            consumer: pending.consumer,
            corrector: pending.corrector,
            scratch: pending.scratch,
        }
    }

    fn from_active(active: ActiveDraw) -> RetiredConsumer {
        RetiredConsumer {
            route_id: active.route_id,
            consumer: active.consumer,
            corrector: active.corrector,
            scratch: active.scratch,
        }
    }
}
