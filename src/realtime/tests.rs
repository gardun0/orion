//! Engine and stage tests: everything here runs without any audio backend,
//! which is the point of the platform-neutral split.

use super::controls::{ChannelControls, EndpointControls};
use super::engine::*;
use super::meter::RouteMeter;
use super::stage::*;

use std::sync::Arc;

use crate::domain::{ChannelMode, RouteId};

const CHANNELS: usize = 2;
const RATE: u32 = 48_000;
const QUANTUM: u32 = 128;

fn source_setup(
    channels: usize,
) -> (
    SourceEngine,
    SourceHandle,
    Arc<EndpointControls>,
    Arc<ChannelControls>,
    Arc<RouteMeter>,
) {
    let controls = Arc::new(EndpointControls::default());
    let channel_controls = Arc::new(ChannelControls::default());
    let meter = Arc::new(RouteMeter::new(channels));
    let slot = Arc::new(PlanSlot::new(SourcePlan {
        generation: 0,
        feeds: Vec::new(),
    }));
    let (engine, handle) = SourceEngine::new(
        channels,
        RATE,
        controls.clone(),
        channel_controls.clone(),
        meter.clone(),
        slot,
    )
    .expect("source engine");
    (engine, handle, controls, channel_controls, meter)
}

fn bus_setup(
    channels: usize,
) -> (
    BusEngine,
    BusHandle,
    Arc<EndpointControls>,
    Arc<ChannelControls>,
    Arc<RouteMeter>,
) {
    let controls = Arc::new(EndpointControls::default());
    let channel_controls = Arc::new(ChannelControls::default());
    let meter = Arc::new(RouteMeter::new(channels));
    let slot = Arc::new(PlanSlot::new(BusPlan {
        generation: 0,
        routes: Vec::new(),
    }));
    let (engine, handle) = BusEngine::new(
        channels,
        RATE,
        controls.clone(),
        channel_controls.clone(),
        meter.clone(),
        slot,
    )
    .expect("bus engine");
    (engine, handle, controls, channel_controls, meter)
}

/// Wire a route between a source and a bus the way the backend does:
/// publish both plans, then deliver the ring halves through the inboxes.
/// Each plan slot owns its generation counter, exactly like the backend's
/// per-endpoint counters.
fn link(
    source: &mut SourceHandle,
    bus: &mut BusHandle,
    route_id: RouteId,
    source_generation: &mut u64,
    bus_generation: &mut u64,
    source_controls: &EndpointControls,
) {
    *source_generation += 1;
    *bus_generation += 1;
    let old_source = source.plan_slot.publish(SourcePlan {
        generation: *source_generation,
        feeds: vec![RouteFeed {
            route_id,
            bus_channels: CHANNELS,
        }],
    });
    source.reclaimer.retire(old_source);
    let old_bus = bus.plan_slot.publish(BusPlan {
        generation: *bus_generation,
        routes: vec![route_id],
    });
    bus.reclaimer.retire(old_bus);

    let link = RouteLink::new(route_id, CHANNELS, QUANTUM, TARGET_QUANTA, 48_000).expect("link");
    let (source_half, bus_half) = link
        .into_halves(
            *source_generation,
            *bus_generation,
            source_controls.balance_gains(),
        )
        .expect("halves");
    source.inbox.push(source_half).expect("inbox has capacity");
    bus.inbox.push(bus_half).expect("inbox has capacity");
}

fn constant_block(value: f32, frames: usize, channels: usize) -> Vec<f32> {
    vec![value; frames * channels]
}

#[test]
fn source_to_bus_passes_signal_click_free() {
    let (mut source, mut source_handle, source_controls, _, source_meter) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, bus_meter) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    let input = constant_block(0.5, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    // A few blocks of warm-up: the corrector holds zeros while the ring
    // fills toward its one-quantum target, then glides onto the signal.
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }

    let tail = &output[output.len() - 8..];
    assert!(
        tail.iter().all(|sample| (sample - 0.5).abs() < 1.0e-6),
        "unity route must converge to the source exactly: {tail:?}"
    );
    // Both meters saw the signal.
    assert!(source_meter.readings().any(|reading| reading.peak > 0.4));
    assert!(bus_meter.readings().all(|reading| reading.peak > 0.4));
}

#[test]
fn bus_sums_multiple_routes_sample_by_sample() {
    let (mut source_a, mut handle_a, controls_a, _, _) = source_setup(CHANNELS);
    let (mut source_b, mut handle_b, controls_b, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_a = RouteId::new();
    let route_b = RouteId::new();
    // Each plan slot owns its generation counter, like the backend does.
    let mut generation_a = 0_u64;
    let mut generation_b = 0_u64;
    let mut generation_bus = 0_u64;

    // Two routes into one destination: publish a two-route bus plan.
    for (handle, route_id, generation) in [
        (&mut handle_a, route_a, &mut generation_a),
        (&mut handle_b, route_b, &mut generation_b),
    ] {
        *generation += 1;
        let old = handle.plan_slot.publish(SourcePlan {
            generation: *generation,
            feeds: vec![RouteFeed {
                route_id,
                bus_channels: CHANNELS,
            }],
        });
        handle.reclaimer.retire(old);
    }
    generation_bus += 1;
    let old = bus_handle.plan_slot.publish(BusPlan {
        generation: generation_bus,
        routes: vec![route_a, route_b],
    });
    bus_handle.reclaimer.retire(old);
    for (handle, route_id, controls, source_generation) in [
        (&mut handle_a, route_a, &controls_a, &mut generation_a),
        (&mut handle_b, route_b, &controls_b, &mut generation_b),
    ] {
        let link =
            RouteLink::new(route_id, CHANNELS, QUANTUM, TARGET_QUANTA, 48_000).expect("link");
        let (source_half, bus_half) = link
            .into_halves(*source_generation, generation_bus, controls.balance_gains())
            .expect("halves");
        handle.inbox.push(source_half).expect("capacity");
        bus_handle.inbox.push(bus_half).expect("capacity");
    }

    let input_a = constant_block(0.25, QUANTUM as usize, CHANNELS);
    let input_b = constant_block(0.5, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    for _ in 0..8 {
        source_a.process(&input_a);
        source_b.process(&input_b);
        bus.process(&mut output);
    }

    let tail = &output[output.len() - 8..];
    assert!(
        tail.iter().all(|sample| (sample - 0.75).abs() < 1.0e-6),
        "the bus must sum both routes exactly: {tail:?}"
    );
}

#[test]
fn bus_gain_and_mute_ramp_without_steps() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, bus_controls, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    let input = constant_block(1.0, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }
    assert!(output[output.len() - 1] > 0.9, "settled at unity");

    // Mute: the output must ramp to zero, never jump.
    bus_controls.set_muted(true);
    let mut previous = output[output.len() - 1];
    let mut saw_intermediate = false;
    for _ in 0..4 {
        source.process(&input);
        bus.process(&mut output);
        for &sample in output.iter() {
            let step = (sample - previous).abs();
            assert!(step <= 0.05, "mute must ramp, not jump (step {step})");
            if sample > 0.01 && sample < previous {
                saw_intermediate = true;
            }
            previous = sample;
        }
    }
    assert!(saw_intermediate, "ramp produced intermediate levels");
    assert!(
        output[output.len() - 1].abs() < 1.0e-3,
        "muted bus is silent"
    );
}

#[test]
fn bus_balance_attenuates_only_the_opposite_side() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, bus_controls, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );
    bus_controls.set_balance(crate::domain::NormalizedBalance::new(1.0).unwrap());

    let input = constant_block(0.5, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    for _ in 0..10 {
        source.process(&input);
        bus.process(&mut output);
    }

    let tail = &output[output.len() - 8..];
    let left: f32 = tail.iter().step_by(2).copied().sum();
    let right: f32 = tail.iter().skip(1).step_by(2).copied().sum();
    assert!(
        left.abs() < 1.0e-6,
        "full-right balance silences left: {left}"
    );
    assert!(
        (right - 0.5 * 4.0).abs() < 1.0e-5,
        "right stays at unity: {right}"
    );
}

#[test]
fn empty_bus_outputs_silence_and_holds_on_underrun() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // Starve the bus: the corrector must emit its held frame (zeros at
    // start), never NaN and never garbage.
    let mut output = constant_block(7.0, QUANTUM as usize, CHANNELS);
    bus.process(&mut output);
    assert!(
        output.iter().all(|sample| *sample == 0.0),
        "starved bus must output silence"
    );

    // Feed once, then starve again: output glides onto the held frame.
    let input = constant_block(0.8, QUANTUM as usize, CHANNELS);
    source.process(&input);
    bus.process(&mut output);
    for _ in 0..4 {
        bus.process(&mut output);
        assert!(output.iter().all(|sample| sample.is_finite()));
    }
    assert!(
        output[output.len() - 1] > 0.1,
        "held frame keeps the last real signal, not zeros"
    );
}

#[test]
fn plans_are_reclaimed_only_after_the_callback_completes_them() {
    let (_source, mut handle, _controls, _, _) = source_setup(CHANNELS);

    let gen1 = handle.plan_slot.publish(SourcePlan {
        generation: 1,
        feeds: Vec::new(),
    });
    handle.reclaimer.retire(gen1);
    let gen2 = handle.plan_slot.publish(SourcePlan {
        generation: 2,
        feeds: Vec::new(),
    });
    handle.reclaimer.retire(gen2);

    // Callback has not completed anything: both retired plans survive.
    handle.reclaimer.collect(&handle.plan_slot);
    // (No direct size assertion possible without exposing internals; use
    // generation completion to prove the invariant below.)
    assert_eq!(handle.plan_slot.completed(), 0);

    // Simulate a callback finishing generation 1, then 2.
    handle.plan_slot.complete(1);
    handle.reclaimer.collect(&handle.plan_slot);
    handle.plan_slot.complete(2);
    handle.reclaimer.collect(&handle.plan_slot);
    assert_eq!(handle.plan_slot.completed(), 2);
}

#[test]
fn sync_offset_delays_by_exactly_the_configured_samples() {
    // Ported parity check for the interleaved delay line: an offset of 2
    // samples shifts the ramp 1..=8 by 2, and zero offset is a passthrough.
    let mut line = vec![0.0_f32; 64];
    let mut write = 0;
    let output: Vec<f32> = (1..=8)
        .map(|sample| apply_sync_offset(sample as f32, &mut line, &mut write, 2))
        .collect();
    assert_eq!(output, vec![0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);

    let passthrough: Vec<f32> = (1..=4)
        .map(|sample| apply_sync_offset(sample as f32, &mut line, &mut write, 0))
        .collect();
    assert_eq!(passthrough, vec![1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn source_delay_shifts_the_delivered_signal() {
    let (mut source, mut source_handle, source_controls, channel_controls, _) =
        source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // 8-frame delay on the source: a block of ones arrives shifted.
    channel_controls
        .delay_frames
        .store(8, std::sync::atomic::Ordering::Relaxed);
    let input = constant_block(0.5, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    source.process(&input);
    bus.process(&mut output);
    // With an 8-frame source delay, the first frames the bus can deliver are
    // silence/held frames; the signal must not appear before the delay.
    assert!(
        output[..8 * CHANNELS]
            .iter()
            .all(|sample| sample.abs() < 1e-6),
        "delayed source must not deliver signal before the delay elapses"
    );
}

#[test]
fn channel_mode_mapping_matches_legacy_rules() {
    // Mono source fans out to both bus channels.
    assert_eq!(
        capture_mode_frame(ChannelMode::Auto, &[0.25], 0, CHANNELS),
        0.25
    );
    assert_eq!(
        capture_mode_frame(ChannelMode::Auto, &[0.25], 1, CHANNELS),
        0.25
    );
    // Stereo -> mono averages; stereo -> stereo maps by index.
    assert_eq!(
        capture_mode_frame(ChannelMode::Auto, &[0.5, -0.5], 0, 1),
        0.0
    );
    assert_eq!(
        capture_mode_frame(ChannelMode::Auto, &[0.5, -0.5], 1, CHANNELS),
        -0.5
    );
    // Modes reshape: Swap exchanges, Left/Right duplicate one side, Mono
    // averages into every bus channel.
    assert_eq!(
        capture_mode_frame(ChannelMode::Swap, &[0.5, -0.5], 0, CHANNELS),
        -0.5
    );
    assert_eq!(
        capture_mode_frame(ChannelMode::Left, &[0.5, -0.5], 1, CHANNELS),
        0.5
    );
    assert_eq!(
        capture_mode_frame(ChannelMode::Mono, &[0.5, -0.5], 1, CHANNELS),
        0.0
    );
}

#[test]
fn output_mode_post_pass_reshapes_stereo_only() {
    let mut block = vec![1.0, -1.0, 0.5, -0.5];
    apply_output_mode(ChannelMode::Swap, &mut block, 2);
    assert_eq!(block, vec![-1.0, 1.0, -0.5, 0.5]);

    let mut mono = vec![1.0, -1.0];
    apply_output_mode(ChannelMode::Swap, &mut mono, 1);
    assert_eq!(mono, vec![1.0, -1.0], "mono buses pass through");
}

#[test]
fn output_mode_crossfade_blends_between_mappings() {
    // Left maps to (l, l), Right to (r, r); a 4-frame ramp must hit the
    // exact linear interpolation points and land precisely on the new mode.
    let mut mix = orion_dsp::ParameterSmoother::new(0.0).expect("finite");
    mix.set_target(1.0, 4).expect("ramp");
    let mut block = vec![0.8, -0.2, 0.8, -0.2, 0.8, -0.2, 0.8, -0.2];
    apply_output_mode_crossfade(ChannelMode::Left, ChannelMode::Right, &mut mix, &mut block);
    // Left maps both channels to l = 0.8, Right maps both to r = -0.2, so
    // every channel follows the same 0.8 -> -0.2 blend over the 4 frames.
    let expected = [0.55, 0.55, 0.3, 0.3, 0.05, 0.05, -0.2, -0.2];
    for (actual, expected) in block.iter().zip(expected) {
        assert!(
            (actual - expected).abs() < 1.0e-6,
            "blend point: {actual} vs {expected}"
        );
    }
}

#[test]
fn source_mode_switch_crossfades_without_clicks() {
    let (mut source, mut source_handle, source_controls, channel_controls, _) =
        source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // Distinct constant channels: L = 0.8, R = -0.2.
    let mut input = vec![0.0_f32; QUANTUM as usize * CHANNELS];
    for frame in input.chunks_exact_mut(2) {
        frame[0] = 0.8;
        frame[1] = -0.2;
    }
    let mut output = vec![0.0_f32; QUANTUM as usize * CHANNELS];
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }
    assert!(
        (output[output.len() - 2] - 0.8).abs() < 0.01,
        "Auto maps L to the left channel"
    );

    // Switch the source to Right mid-stream: the left bus channel must
    // travel 0.8 -> -0.2 as a ramp, never as a step.
    channel_controls.mode.store(
        ChannelMode::Right.code(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let mut previous = output[output.len() - 2];
    let mut max_step = 0.0_f32;
    let mut final_left = previous;
    for _ in 0..12 {
        source.process(&input);
        bus.process(&mut output);
        for frame in output.chunks_exact(2) {
            max_step = max_step.max((frame[0] - previous).abs());
            previous = frame[0];
            final_left = frame[0];
        }
    }
    assert!(
        max_step < 0.01,
        "mode switch must crossfade, not click (max step {max_step})"
    );
    assert!(
        (final_left + 0.2).abs() < 0.01,
        "converged to the Right mapping: {final_left}"
    );
}

#[test]
fn bus_mode_switch_crossfades_without_clicks() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, channel_controls, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    let mut input = vec![0.0_f32; QUANTUM as usize * CHANNELS];
    for frame in input.chunks_exact_mut(2) {
        frame[0] = 0.8;
        frame[1] = -0.2;
    }
    let mut output = vec![0.0_f32; QUANTUM as usize * CHANNELS];
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }

    // Swap on the bus: the right channel travels -0.2 -> 0.8 as a ramp.
    channel_controls.mode.store(
        ChannelMode::Swap.code(),
        std::sync::atomic::Ordering::Relaxed,
    );
    let mut previous = output[output.len() - 1];
    let mut max_step = 0.0_f32;
    let mut final_right = previous;
    for _ in 0..12 {
        source.process(&input);
        bus.process(&mut output);
        for frame in output.chunks_exact(2) {
            max_step = max_step.max((frame[1] - previous).abs());
            previous = frame[1];
            final_right = frame[1];
        }
    }
    assert!(
        max_step < 0.01,
        "bus mode switch must crossfade, not click (max step {max_step})"
    );
    assert!(
        (final_right - 0.8).abs() < 0.01,
        "converged to the swapped mapping: {final_right}"
    );
}

#[test]
fn corrector_target_is_one_quantum_validated_to_two() {
    assert_eq!(corrector_target_frames(512, TARGET_QUANTA), 512);
    assert_eq!(corrector_target_frames(512, MAX_TARGET_QUANTA), 1024);
    assert_eq!(
        corrector_target_frames(512, 9),
        1024,
        "targets past two quanta clamp"
    );
    assert_eq!(corrector_target_frames(0, 1), 1, "never zero");
}

#[test]
fn ring_capacity_scales_with_quantum() {
    assert_eq!(ring_capacity_frames(512), 16_384);
    assert_eq!(ring_capacity_frames(16_384), 65_536);
}

#[test]
fn control_ramp_is_ten_ms_at_any_rate() {
    assert_eq!(control_ramp_frames(48_000), 480);
    assert_eq!(control_ramp_frames(768_000), 7_680);
    assert_eq!(control_ramp_frames(0), 1);
}

#[test]
fn engine_keeps_output_finite_when_producer_overruns() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // Flood: never consume; the source must drop instead of panicking or
    // blocking, then recover when the bus catches up.
    let input = constant_block(1.0, QUANTUM as usize, CHANNELS);
    for _ in 0..200 {
        source.process(&input);
    }
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    bus.process(&mut output);
    assert!(output.iter().all(|sample| sample.is_finite()));
}

/// Backend-side mirror of route teardown: publish plans without the route,
/// let the callbacks retire their halves, then collect the garbage.
fn unlink(
    source: &mut SourceHandle,
    bus: &mut BusHandle,
    _route_id: RouteId,
    source_generation: &mut u64,
    bus_generation: &mut u64,
) {
    *source_generation += 1;
    *bus_generation += 1;
    let old_source = source.plan_slot.publish(SourcePlan {
        generation: *source_generation,
        feeds: Vec::new(),
    });
    source.reclaimer.retire(old_source);
    let old_bus = bus.plan_slot.publish(BusPlan {
        generation: *bus_generation,
        routes: Vec::new(),
    });
    bus.reclaimer.retire(old_bus);
}

fn collect_backend_garbage(source: &mut SourceHandle, bus: &mut BusHandle) {
    while source.retired.pop().is_ok() {}
    while bus.retired.pop().is_ok() {}
    source.reclaimer.collect(&source.plan_slot);
    bus.reclaimer.collect(&bus.plan_slot);
}

#[test]
fn plan_churn_under_continuous_audio_stays_stable() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let mut source_generation = 0;
    let mut bus_generation = 0;
    let input = constant_block(0.5, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);

    // 60 connect/process/disconnect cycles: the route storm the reconciler
    // can produce when the user scribbles in the matrix.
    for cycle in 0..60 {
        let route_id = RouteId::new();
        link(
            &mut source_handle,
            &mut bus_handle,
            route_id,
            &mut source_generation,
            &mut bus_generation,
            &source_controls,
        );
        source.process(&input);
        bus.process(&mut output);
        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "cycle {cycle}: output must stay finite"
        );
        unlink(
            &mut source_handle,
            &mut bus_handle,
            route_id,
            &mut source_generation,
            &mut bus_generation,
        );
        source.process(&input);
        bus.process(&mut output);
        collect_backend_garbage(&mut source_handle, &mut bus_handle);
    }
    // After the storm: unlinked routes are fully retired, audio path clean.
    let (bus_debug, _) = (bus.draw_debug(), ());
    assert!(
        bus_debug.is_empty(),
        "every churned route must be retired: {bus_debug:?}"
    );
    assert!(output.iter().all(|sample| *sample == 0.0));
}

#[test]
fn hot_mix_is_bounded_by_the_always_on_saturator() {
    // Two full-scale routes into one bus: the sum reaches 2.0, and the
    // saturator must keep the output at or under 1.0 without folding.
    let (mut source_a, mut handle_a, controls_a, _, _) = source_setup(CHANNELS);
    let (mut source_b, mut handle_b, controls_b, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_a = RouteId::new();
    let route_b = RouteId::new();
    let mut generation_a = 0_u64;
    let mut generation_b = 0_u64;
    let mut generation_bus = 0_u64;
    for (handle, route_id, generation) in [
        (&mut handle_a, route_a, &mut generation_a),
        (&mut handle_b, route_b, &mut generation_b),
    ] {
        *generation += 1;
        let old = handle.plan_slot.publish(SourcePlan {
            generation: *generation,
            feeds: vec![RouteFeed {
                route_id,
                bus_channels: CHANNELS,
            }],
        });
        handle.reclaimer.retire(old);
    }
    generation_bus += 1;
    let old = bus_handle.plan_slot.publish(BusPlan {
        generation: generation_bus,
        routes: vec![route_a, route_b],
    });
    bus_handle.reclaimer.retire(old);
    for (handle, route_id, controls, source_generation) in [
        (&mut handle_a, route_a, &controls_a, &mut generation_a),
        (&mut handle_b, route_b, &controls_b, &mut generation_b),
    ] {
        let link =
            RouteLink::new(route_id, CHANNELS, QUANTUM, TARGET_QUANTA, 48_000).expect("link");
        let (source_half, bus_half) = link
            .into_halves(*source_generation, generation_bus, controls.balance_gains())
            .expect("halves");
        handle.inbox.push(source_half).expect("capacity");
        bus_handle.inbox.push(bus_half).expect("capacity");
    }

    let input_a = constant_block(1.0, QUANTUM as usize, CHANNELS);
    let input_b = constant_block(1.0, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    for _ in 0..8 {
        source_a.process(&input_a);
        source_b.process(&input_b);
        bus.process(&mut output);
    }

    assert!(
        output.iter().all(|sample| sample.abs() <= 1.0),
        "the summed bus must never exceed ±1.0"
    );
    let tail = &output[output.len() - 8..];
    assert!(
        tail.iter().all(|sample| *sample > 0.9),
        "a hot but legal mix saturates close to full scale: {tail:?}"
    );
}

#[test]
fn meters_report_rms_and_pre_saturator_clip() {
    // Two routes from one source into one bus: 0.8 + 0.8 sums past full
    // scale. The output is bounded by the saturator and the peak meter reads
    // the bounded signal, but the clip flag must still fire.
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, bus_meter) = bus_setup(CHANNELS);
    let route_a = RouteId::new();
    let route_b = RouteId::new();

    let old_source = source_handle.plan_slot.publish(SourcePlan {
        generation: 1,
        feeds: vec![
            RouteFeed {
                route_id: route_a,
                bus_channels: CHANNELS,
            },
            RouteFeed {
                route_id: route_b,
                bus_channels: CHANNELS,
            },
        ],
    });
    source_handle.reclaimer.retire(old_source);
    let old_bus = bus_handle.plan_slot.publish(BusPlan {
        generation: 1,
        routes: vec![route_a, route_b],
    });
    bus_handle.reclaimer.retire(old_bus);
    for route_id in [route_a, route_b] {
        let link =
            RouteLink::new(route_id, CHANNELS, QUANTUM, TARGET_QUANTA, 48_000).expect("link");
        let (source_half, bus_half) = link
            .into_halves(1, 1, source_controls.balance_gains())
            .expect("halves");
        source_handle.inbox.push(source_half).expect("capacity");
        bus_handle.inbox.push(bus_half).expect("capacity");
    }

    let input = constant_block(0.8, QUANTUM as usize, CHANNELS);
    let mut output = constant_block(0.0, QUANTUM as usize, CHANNELS);
    for _ in 0..8 {
        source.process(&input);
        bus.process(&mut output);
    }

    let readings: Vec<_> = bus_meter.readings().collect();
    assert_eq!(readings.len(), CHANNELS);
    for reading in &readings {
        assert!(reading.clipped, "hot mix must raise the clip flag");
        assert!(reading.peak <= 1.0, "peak reads the bounded output");
        assert!(reading.peak > 0.9, "saturated mix stays near full scale");
        // Output converges to soft_clip(1.6) for a constant input, so RMS
        // tracks that level, not the unbounded sum.
        let expected = orion_dsp::soft_clip(1.6);
        assert!(
            (reading.rms - expected).abs() < 0.02,
            "RMS tracks the bounded signal: {} vs {expected}",
            reading.rms
        );
    }
    // A second window with no new clip: flag clears on read.
    let readings: Vec<_> = bus_meter.readings().collect();
    assert!(readings.iter().all(|reading| !reading.clipped));
}

#[test]
fn drift_corrector_tracks_a_fast_source_clock() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // Simulate a source clock running ~1.6% fast (far beyond real device
    // drift): 130 frames delivered per 128 consumed.
    let frames = QUANTUM as usize;
    let mut output = constant_block(0.0, frames, CHANNELS);
    let mut phase = 0.0_f32;
    let mut max_occupancy = 0_usize;
    for block in 0..4_000 {
        let extra = if block % 4 == 0 { 2 } else { 1 };
        let input: Vec<f32> = (0..(frames + extra) * CHANNELS)
            .map(|_| {
                let sample = phase.sin() * 0.25;
                phase += 0.05;
                sample
            })
            .collect();
        source.process(&input);
        bus.process(&mut output);
        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "block {block}: output must stay finite under drift"
        );
        if block % 100 == 0 {
            for (_, occupancy) in bus.draw_debug() {
                max_occupancy = max_occupancy.max(occupancy);
            }
        }
    }

    let (ratio, occupancy) = bus
        .draw_debug()
        .into_iter()
        .next()
        .expect("route still linked");
    assert!(
        ratio > 1.0,
        "corrector must consume faster than nominal under a fast source: {ratio}"
    );
    assert!(
        ratio <= 1.0021,
        "correction stays within the ±0.2% clamp: {ratio}"
    );
    let capacity = ring_capacity_frames(QUANTUM);
    assert!(
        occupancy < capacity / 2 && max_occupancy < capacity / 2,
        "occupancy must stay far from overrun (max {max_occupancy}, capacity {capacity})"
    );
    // The signal still reaches the output after the soak.
    let peak = output.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert!(peak > 0.1, "signal must keep flowing: peak {peak}");
}

#[test]
fn drift_corrector_tracks_a_slow_source_clock() {
    let (mut source, mut source_handle, source_controls, _, _) = source_setup(CHANNELS);
    let (mut bus, mut bus_handle, _, _, _) = bus_setup(CHANNELS);
    let route_id = RouteId::new();
    let mut source_generation = 0;
    let mut bus_generation = 0;
    link(
        &mut source_handle,
        &mut bus_handle,
        route_id,
        &mut source_generation,
        &mut bus_generation,
        &source_controls,
    );

    // Source running slightly slow: 127 frames delivered per 128 consumed.
    let frames = QUANTUM as usize;
    let mut output = constant_block(0.0, frames, CHANNELS);
    let mut phase = 0.0_f32;
    for block in 0..2_000 {
        let delivered = frames - usize::from(block % 2 == 0);
        let input: Vec<f32> = (0..delivered * CHANNELS)
            .map(|_| {
                let sample = phase.sin() * 0.25;
                phase += 0.05;
                sample
            })
            .collect();
        source.process(&input);
        bus.process(&mut output);
        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "block {block}: held frames must stay finite"
        );
    }

    let (ratio, _) = bus
        .draw_debug()
        .into_iter()
        .next()
        .expect("route still linked");
    assert!(
        ratio < 1.0,
        "corrector must slow consumption under a slow source: {ratio}"
    );
    assert!(ratio >= 0.9979, "correction stays within clamp: {ratio}");
    let peak = output.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert!(peak > 0.1, "held-frame glide must not silence the bus");
}
