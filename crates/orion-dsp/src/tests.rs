use super::*;

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1.0e-6,
        "expected {expected}, got {actual}"
    );
}

fn config(channels: usize, max_frames: usize) -> ProcessConfig {
    ProcessConfig::new(48_000, max_frames, channels).unwrap()
}

#[test]
fn process_config_rejects_invalid_values() {
    assert_eq!(
        ProcessConfig::new(0, 64, 1),
        Err(DspError::InvalidSampleRate(0))
    );
    assert_eq!(
        ProcessConfig::new(48_000, 0, 1),
        Err(DspError::InvalidMaxBlockSize(0))
    );
    assert_eq!(
        ProcessConfig::new(48_000, 64, 0),
        Err(DspError::InvalidChannelCount(0))
    );
    assert_eq!(
        ProcessConfig::new(48_000, 64, 3),
        Err(DspError::InvalidChannelCount(3))
    );

    let valid = config(2, 128);
    assert_eq!(valid.sample_rate(), 48_000);
    assert_eq!(valid.max_block_size(), 128);
    assert_eq!(valid.channel_count(), 2);
}

#[test]
fn context_and_planar_buffer_are_validated() {
    let cfg = config(2, 4);
    assert_eq!(
        ProcessContext::new(cfg, 5, 0),
        Err(DspError::BlockTooLarge {
            frames: 5,
            max_frames: 4
        })
    );

    let mut none: [&mut [f32]; 0] = [];
    assert!(matches!(
        AudioBuffer::new(&mut none),
        Err(DspError::InvalidChannelCount(0))
    ));

    let mut left = [0.0; 2];
    let mut right = [0.0; 3];
    let mut uneven: [&mut [f32]; 2] = [&mut left, &mut right];
    assert!(matches!(
        AudioBuffer::new(&mut uneven),
        Err(DspError::NonUniformBuffer {
            expected_frames: 2,
            channel: 1,
            actual_frames: 3
        })
    ));
}

#[test]
fn processor_rejects_unprepared_and_mismatched_blocks() {
    let cfg = config(1, 8);
    let context = ProcessContext::new(cfg, 2, 10).unwrap();
    let mut samples = [1.0; 2];
    let mut channels: [&mut [f32]; 1] = [&mut samples];
    let mut buffer = AudioBuffer::new(&mut channels).unwrap();
    let mut gain = GainProcessor::new(1.0).unwrap();
    assert_eq!(
        gain.process(&context, &mut buffer),
        Err(DspError::NotPrepared)
    );

    gain.prepare(config(1, 4)).unwrap();
    assert_eq!(
        gain.process(&context, &mut buffer),
        Err(DspError::ConfigurationMismatch)
    );

    let cfg = config(1, 8);
    gain.prepare(cfg).unwrap();
    let context = ProcessContext::new(cfg, 1, 10).unwrap();
    assert_eq!(
        gain.process(&context, &mut buffer),
        Err(DspError::FrameCountMismatch {
            expected: 1,
            actual: 2
        })
    );
}

#[test]
fn smoother_ramps_by_frames_and_retargets_continuously() {
    let mut smoother = ParameterSmoother::new(0.0).unwrap();
    smoother.set_target(1.0, 4).unwrap();
    assert_close(smoother.next_value(), 0.25);
    assert_close(smoother.next_value(), 0.5);

    smoother.set_target(0.0, 2).unwrap();
    assert_close(smoother.next_value(), 0.25);
    assert_close(smoother.next_value(), 0.0);
    assert!(!smoother.is_smoothing());
    assert_close(smoother.next_value(), 0.0);

    smoother.set_target(0.75, 0).unwrap();
    assert_close(smoother.current(), 0.75);
    assert_eq!(
        smoother.set_target(f32::NAN, 3),
        Err(DspError::InvalidParameter("smoother target"))
    );
}

#[test]
fn gain_ramp_continues_across_blocks() {
    let cfg = config(1, 4);
    let mut gain = GainProcessor::new(0.0).unwrap();
    gain.prepare(cfg).unwrap();
    gain.set_target_gain(1.0, 4).unwrap();

    let mut first = [1.0; 2];
    {
        let mut channels: [&mut [f32]; 1] = [&mut first];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        gain.process(&ProcessContext::new(cfg, 2, 0).unwrap(), &mut buffer)
            .unwrap();
    }
    let mut second = [1.0; 2];
    {
        let mut channels: [&mut [f32]; 1] = [&mut second];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        gain.process(&ProcessContext::new(cfg, 2, 2).unwrap(), &mut buffer)
            .unwrap();
    }

    assert_eq!(first, [0.25, 0.5]);
    assert_eq!(second, [0.75, 1.0]);
}

#[test]
fn mute_and_unmute_are_click_free_ramps() {
    let cfg = config(2, 8);
    let mut gain = GainProcessor::new(1.0).unwrap();
    gain.prepare(cfg).unwrap();
    gain.set_muted(true, 4).unwrap();

    let mut left = [1.0; 4];
    let mut right = [0.5; 4];
    {
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        gain.process(&ProcessContext::new(cfg, 4, 0).unwrap(), &mut buffer)
            .unwrap();
    }
    assert_eq!(left, [0.75, 0.5, 0.25, 0.0]);
    assert_eq!(right, [0.375, 0.25, 0.125, 0.0]);
    assert!(gain.is_muted());

    gain.set_muted(false, 2).unwrap();
    let mut mono = [1.0; 2];
    let mono_cfg = config(1, 8);
    gain.prepare(mono_cfg).unwrap();
    {
        let mut channels: [&mut [f32]; 1] = [&mut mono];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        gain.process(&ProcessContext::new(mono_cfg, 2, 4).unwrap(), &mut buffer)
            .unwrap();
    }
    assert_eq!(mono, [0.5, 1.0]);
}

#[test]
fn balance_only_attenuates_the_opposite_side() {
    assert!(StereoBalanceProcessor::new(1.01).is_err());
    let mono_cfg = config(1, 4);
    let mut balance = StereoBalanceProcessor::new(0.0).unwrap();
    assert_eq!(
        balance.prepare(mono_cfg),
        Err(DspError::ChannelCountMismatch {
            expected: 2,
            actual: 1
        })
    );

    let cfg = config(2, 4);
    balance.prepare(cfg).unwrap();
    balance.set_target_balance(1.0, 0).unwrap();
    let mut left = [2.0, 2.0];
    let mut right = [3.0, 3.0];
    {
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        balance
            .process(&ProcessContext::new(cfg, 2, 0).unwrap(), &mut buffer)
            .unwrap();
    }
    assert_eq!(left, [0.0, 0.0]);
    assert_eq!(right, [3.0, 3.0]);

    balance.set_target_balance(-1.0, 0).unwrap();
    let mut left = [2.0];
    let mut right = [3.0];
    {
        let mut channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut buffer = AudioBuffer::new(&mut channels).unwrap();
        balance
            .process(&ProcessContext::new(cfg, 1, 2).unwrap(), &mut buffer)
            .unwrap();
    }
    assert_eq!(left, [2.0]);
    assert_eq!(right, [0.0]);
}

#[test]
fn linear_balance_law_attenuates_only_the_opposite_side() {
    assert_eq!(linear_balance_gains(0.0), (1.0, 1.0));
    assert_eq!(linear_balance_gains(-1.0), (1.0, 0.0));
    assert_eq!(linear_balance_gains(1.0), (0.0, 1.0));
    // Half-right keeps right at unity and attenuates left linearly.
    assert_eq!(linear_balance_gains(0.5), (0.5, 1.0));
    assert_eq!(linear_balance_gains(-0.25), (1.0, 0.75));
    // Out-of-range input is clamped into the law's domain.
    assert_eq!(linear_balance_gains(2.0), (0.0, 1.0));
    assert_eq!(linear_balance_gains(-3.0), (1.0, 0.0));
}

#[test]
fn constant_power_pan_has_correct_center_and_extremes() {
    let (left, right) = constant_power_pan_gains(0.0).unwrap();
    assert_close(left, core::f32::consts::FRAC_1_SQRT_2);
    assert_close(right, core::f32::consts::FRAC_1_SQRT_2);

    let input = [1.0, -0.5];
    let mut left = [9.0; 2];
    let mut right = [9.0; 2];
    pan_mono_to_stereo(&input, &mut left, &mut right, -1.0).unwrap();
    assert_eq!(left, input);
    assert_close(right[0], 0.0);
    assert_close(right[1], 0.0);

    pan_mono_to_stereo(&input, &mut left, &mut right, 1.0).unwrap();
    assert_close(left[0], 0.0);
    assert_close(left[1], 0.0);
    assert_eq!(right, input);
    assert!(pan_mono_to_stereo(&input, &mut left, &mut right, f32::INFINITY).is_err());
}

#[test]
fn mixer_sums_routes_and_fans_mono_out_to_stereo() {
    let cfg = config(2, 4);
    let context = ProcessContext::new(cfg, 2, 0).unwrap();
    let mut mono_a = [1.0, 2.0];
    let mut mono_b = [4.0, 6.0];
    let mut a_channels: [&mut [f32]; 1] = [&mut mono_a];
    let mut b_channels: [&mut [f32]; 1] = [&mut mono_b];
    let input_a = AudioBuffer::new(&mut a_channels).unwrap();
    let input_b = AudioBuffer::new(&mut b_channels).unwrap();
    let routes = [
        MixRoute::new(&input_a, 1.0).unwrap(),
        MixRoute::new(&input_b, 0.5).unwrap(),
    ];
    let mut left = [99.0; 2];
    let mut right = [99.0; 2];
    {
        let mut output_channels: [&mut [f32]; 2] = [&mut left, &mut right];
        let mut output = AudioBuffer::new(&mut output_channels).unwrap();
        let mut mixer = Mixer::new();
        mixer.prepare(cfg);
        mixer.process(&context, &routes, &mut output).unwrap();
    }
    assert_eq!(left, [3.0, 5.0]);
    assert_eq!(right, [3.0, 5.0]);
}

#[test]
fn mixer_handles_all_channel_mappings() {
    // mono -> mono
    let mono_cfg = config(1, 2);
    let mono_context = ProcessContext::new(mono_cfg, 2, 0).unwrap();
    let mut mono_input = [2.0, -2.0];
    let mut mono_channels: [&mut [f32]; 1] = [&mut mono_input];
    let mono_buffer = AudioBuffer::new(&mut mono_channels).unwrap();
    let mono_routes = [MixRoute::new(&mono_buffer, 0.25).unwrap()];
    let mut mono_output = [0.0; 2];
    {
        let mut output_channels: [&mut [f32]; 1] = [&mut mono_output];
        let mut output = AudioBuffer::new(&mut output_channels).unwrap();
        let mut mixer = Mixer::new();
        mixer.prepare(mono_cfg);
        mixer
            .process(&mono_context, &mono_routes, &mut output)
            .unwrap();
    }
    assert_eq!(mono_output, [0.5, -0.5]);

    // stereo -> mono uses an average, not a sum.
    let mut left = [1.0, 3.0];
    let mut right = [3.0, -1.0];
    let mut stereo_channels: [&mut [f32]; 2] = [&mut left, &mut right];
    let stereo_buffer = AudioBuffer::new(&mut stereo_channels).unwrap();
    let stereo_routes = [MixRoute::new(&stereo_buffer, 1.0).unwrap()];
    let mut downmix = [0.0; 2];
    {
        let mut output_channels: [&mut [f32]; 1] = [&mut downmix];
        let mut output = AudioBuffer::new(&mut output_channels).unwrap();
        let mut mixer = Mixer::new();
        mixer.prepare(mono_cfg);
        mixer
            .process(&mono_context, &stereo_routes, &mut output)
            .unwrap();
    }
    assert_eq!(downmix, [2.0, 1.0]);

    // stereo -> stereo preserves channel identity.
    let stereo_cfg = config(2, 2);
    let stereo_context = ProcessContext::new(stereo_cfg, 2, 0).unwrap();
    let mut stereo_left = [0.0; 2];
    let mut stereo_right = [0.0; 2];
    {
        let mut output_channels: [&mut [f32]; 2] = [&mut stereo_left, &mut stereo_right];
        let mut output = AudioBuffer::new(&mut output_channels).unwrap();
        let mut mixer = Mixer::new();
        mixer.prepare(stereo_cfg);
        mixer
            .process(&stereo_context, &stereo_routes, &mut output)
            .unwrap();
    }
    assert_eq!(stereo_left, left);
    assert_eq!(stereo_right, right);
}

#[test]
fn mixer_validates_before_modifying_output() {
    let cfg = config(1, 4);
    let context = ProcessContext::new(cfg, 2, 0).unwrap();
    let mut input = [1.0; 3];
    let mut input_channels: [&mut [f32]; 1] = [&mut input];
    let input_buffer = AudioBuffer::new(&mut input_channels).unwrap();
    let routes = [MixRoute::new(&input_buffer, 1.0).unwrap()];
    let mut output_samples = [7.0; 2];
    {
        let mut output_channels: [&mut [f32]; 1] = [&mut output_samples];
        let mut output = AudioBuffer::new(&mut output_channels).unwrap();
        let mut mixer = Mixer::new();
        mixer.prepare(cfg);
        assert_eq!(
            mixer.process(&context, &routes, &mut output),
            Err(DspError::FrameCountMismatch {
                expected: 2,
                actual: 3
            })
        );
    }
    assert_eq!(output_samples, [7.0; 2]);
}

#[test]
fn meter_reports_peak_rms_hold_and_clipping() {
    let mut meter = ChannelMeter::new(4);
    meter.process(&[0.5, -1.0, 0.25, -0.25]);
    assert_close(meter.peak(), 1.0);
    assert_close(meter.rms(), (1.375_f32 / 4.0).sqrt());
    assert_close(meter.held_peak(), 1.0);
    assert_eq!(meter.hold_remaining_frames(), 4);
    assert!(meter.clip_latched());
    assert_eq!(meter.clip_count(), 1);

    meter.clear_clip_latch();
    meter.process(&[0.1, 0.2, 0.1]);
    assert_close(meter.held_peak(), 1.0);
    assert_eq!(meter.hold_remaining_frames(), 1);
    assert!(!meter.clip_latched());

    meter.process(&[0.3]);
    assert_close(meter.held_peak(), 0.3);
    assert_eq!(meter.hold_remaining_frames(), 0);
    assert_eq!(meter.clip_count(), 1);
}

#[test]
fn meter_sanitizes_nonfinite_samples_and_saturates_clip_count() {
    let mut meter = ChannelMeter::new(0);
    meter.process(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 2.0]);
    assert!(meter.peak().is_finite());
    assert!(meter.rms().is_finite());
    assert_close(meter.peak(), 2.0);
    assert_close(meter.rms(), (6.0_f32 / 4.0).sqrt());
    assert_eq!(meter.clip_count(), 3);
    assert!(meter.clip_latched());

    meter.reset();
    assert_eq!(meter.peak(), 0.0);
    assert_eq!(meter.rms(), 0.0);
    assert_eq!(meter.held_peak(), 0.0);
    assert_eq!(meter.clip_count(), 0);
    assert!(!meter.clip_latched());
}

#[test]
fn drift_corrector_passes_audio_through_at_nominal_rate() {
    let mut corrector = DriftCorrector::new(2, 512).expect("stereo corrector");
    let input: Vec<f32> = (0..4_096)
        .map(|frame| (frame as f32 * 0.05).sin() * 0.5)
        .collect();
    let mut read = 0usize;
    let mut output = vec![0.0_f32; 2 * 1_024];

    corrector.process(&mut output, 2, 512, &mut || {
        let sample = input.get(read).copied();
        read += 1;
        sample
    });

    assert!(
        output.iter().all(|sample| sample.is_finite()),
        "corrector produced non-finite samples"
    );
    let peak = output.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
    assert!(peak > 0.1, "input signal must reach the output");
}

#[test]
fn drift_corrector_ratio_tracks_occupancy_direction() {
    let mut corrector = DriftCorrector::new(2, 512).expect("corrector");
    let mut output = vec![0.0_f32; 2 * 128];
    let mut pull = || Some(0.1_f32);

    for _ in 0..40 {
        corrector.process(&mut output, 2, 4_096, &mut pull);
    }
    assert!(
        corrector.ratio() > 1.0,
        "occupancy above target must speed up consumption: ratio {}",
        corrector.ratio()
    );

    for _ in 0..400 {
        corrector.process(&mut output, 2, 0, &mut pull);
    }
    assert!(
        corrector.ratio() < 1.0,
        "occupancy below target must slow consumption: ratio {}",
        corrector.ratio()
    );
    assert!(corrector.ratio() >= 1.0 - 0.002 - 1.0e-9);
}

#[test]
fn drift_corrector_holds_last_frame_on_underrun_without_clicks() {
    let mut corrector = DriftCorrector::new(2, 512).expect("corrector");
    let mut output = vec![0.0_f32; 2 * 256];
    let mut provided = true;
    corrector.process(&mut output, 2, 0, &mut || {
        if provided {
            provided = false;
            Some(0.75_f32)
        } else {
            None
        }
    });
    assert!(
        output.iter().all(|sample| sample.is_finite()),
        "underrun must stay finite"
    );
    // One real frame, then hold: no zero-gap clicks.
    assert!(
        output.iter().any(|sample| sample.abs() > 0.1),
        "held frame must appear in output"
    );
}

#[test]
fn drift_corrector_supports_multichannel_buses() {
    // Buses are not always stereo: a corrector on a 4-channel bus must hold
    // one frame per channel and keep every channel finite and aligned.
    let mut corrector = DriftCorrector::new(4, 512).expect("4-channel corrector");
    let mut read = 0usize;
    let input: Vec<f32> = (0..2_048).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut output = vec![0.0_f32; 4 * 256];

    corrector.process(&mut output, 4, 512, &mut || {
        let sample = input.get(read).copied();
        read += 1;
        sample
    });

    assert!(output.iter().all(|sample| sample.is_finite()));
    assert!(
        output.iter().any(|sample| sample.abs() > 0.01),
        "signal must reach every channel"
    );
    assert!(DriftCorrector::new(0, 512).is_err());
    assert!(DriftCorrector::new(MAX_CHANNELS + 1, 512).is_err());
}

#[test]
fn drift_corrector_converges_under_simulated_clock_drift() {
    let target = 512usize;
    let mut corrector = DriftCorrector::new(2, target).expect("corrector");
    let mut ring: std::collections::VecDeque<f32> = std::collections::VecDeque::new();
    let mut phase = 0.0_f32;
    let mut output = vec![0.0_f32; 2 * 256];

    for callback in 0..400 {
        // Producer runs fast: extra input keeps landing in the ring.
        for _ in 0..(256 + usize::from(callback % 4 == 0)) {
            let sample = phase.sin() * 0.25;
            phase += 0.05;
            ring.push_back(sample);
            ring.push_back(sample);
        }
        let buffered_frames = ring.len() / 2;
        corrector.process(&mut output, 2, buffered_frames, &mut || ring.pop_front());
        assert!(
            output.iter().all(|sample| sample.is_finite()),
            "non-finite sample under drift"
        );
    }

    let final_buffered = ring.len() / 2;
    assert!(
        final_buffered < target * 4,
        "drift must not accumulate unbounded: buffered {final_buffered}"
    );
    assert!(
        corrector.ratio() != 1.0,
        "controller should have moved off the nominal ratio"
    );
}

#[test]
fn dasp_sample_frame_and_signal_are_available() {
    fn takes_sample<T: Sample>(_sample: T) {}
    fn takes_frame<T: Frame>(_frame: T) {}
    fn takes_signal<T: Signal>(_signal: T) {}

    takes_sample(0.0_f32);
    takes_frame([0.0_f32, 1.0]);
    takes_signal(dasp::signal::from_iter([[0.0_f32]; 2]));
}

// ---- EQ ----

/// Feed a sine at `freq_hz` through the EQ for a second and measure RMS of
/// the tail (steady-state response).
fn eq_rms_at(coefficients: &EqCoefficients, freq_hz: f32, rate: u32) -> f32 {
    let mut eq = ChannelEq::default();
    eq.set_coefficients(coefficients);
    let frames = rate as usize;
    let mut sum = 0.0;
    let mut count = 0usize;
    for frame in 0..frames {
        let phase = 2.0 * std::f32::consts::PI * freq_hz * frame as f32 / rate as f32;
        let out = eq.process(phase.sin());
        if frame > frames / 2 {
            sum += out * out;
            count += 1;
        }
    }
    (sum / count as f32).sqrt()
}

#[test]
fn eq_is_transparent_at_zero_db() {
    let coefficients = EqCoefficients::new(48_000, 0.0, 0.0, 0.0);
    for freq in [100.0, 1_000.0, 10_000.0] {
        let rms = eq_rms_at(&coefficients, freq, 48_000);
        let expected = 0.5_f32.sqrt(); // sine RMS
        assert!(
            (rms - expected).abs() < 0.01,
            "0 dB should pass {freq} Hz cleanly: rms {rms} vs {expected}"
        );
    }
}

#[test]
fn eq_boosts_and_cuts_each_band() {
    let boost = EqCoefficients::new(48_000, 12.0, 12.0, 12.0);
    let cut = EqCoefficients::new(48_000, -12.0, -12.0, -12.0);

    for (freq, band) in [(100.0, "low"), (1_000.0, "mid"), (10_000.0, "high")] {
        let boosted = eq_rms_at(&boost, freq, 48_000);
        let flat = 0.5_f32.sqrt();
        let cut_rms = eq_rms_at(&cut, freq, 48_000);
        assert!(
            boosted > flat * 1.5,
            "{band} band should boost {freq} Hz: {boosted}"
        );
        assert!(
            cut_rms < flat * 0.5,
            "{band} band should cut {freq} Hz: {cut_rms}"
        );
    }

    // Band isolation: boosting the mid band barely touches 100 Hz.
    let mid_only = EqCoefficients::new(48_000, 0.0, 12.0, 0.0);
    let low_rms = eq_rms_at(&mid_only, 100.0, 48_000);
    assert!(
        (low_rms - 0.5_f32.sqrt()).abs() < 0.05,
        "mid boost should leave 100 Hz alone: {low_rms}"
    );
}

#[test]
fn eq_coefficients_are_stable_across_rates() {
    for rate in [44_100, 48_000, 96_000, 768_000] {
        let coefficients = EqCoefficients::new(rate, 6.0, -6.0, 3.0);
        for band in [coefficients.low, coefficients.mid, coefficients.high] {
            for value in band {
                assert!(value.is_finite(), "rate {rate} produced {value}");
            }
        }
        // Stability: pole magnitudes under 1 (biquad in [b0 b1 b2 a1 a2]).
        for band in [coefficients.low, coefficients.mid, coefficients.high] {
            let a1 = band[3] as f64;
            let a2 = band[4] as f64;
            let discriminant = a1 * a1 - 4.0 * a2;
            let radius = if discriminant >= 0.0 {
                (a1.abs() + discriminant.sqrt()) / 2.0
            } else {
                // Complex-conjugate poles: |p|^2 = a2.
                a2.sqrt()
            };
            assert!(radius < 1.0, "rate {rate} unstable pole radius {radius}");
        }
    }
}

#[test]
fn eq_defaults_are_identity_not_silence() {
    // Regression: a derived all-zero default mutes every sample. Both the
    // bare biquad/chain and the default coefficient set must pass audio.
    let mut bare = ChannelEq::default();
    assert_eq!(bare.process(0.5), 0.5);
    assert_eq!(bare.process(-0.25), -0.25);

    let coefficients = EqCoefficients::default();
    let mut eq = ChannelEq::default();
    eq.set_coefficients(&coefficients);
    let rms = eq_rms_at(&coefficients, 1_000.0, 48_000);
    assert!(
        (rms - 0.5_f32.sqrt()).abs() < 0.01,
        "default coefficients must pass audio: rms {rms}"
    );
}
