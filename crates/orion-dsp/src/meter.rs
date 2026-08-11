#[derive(Debug, Clone)]
pub struct ChannelMeter {
    peak: f32,
    rms: f32,
    held_peak: f32,
    peak_hold_frames: usize,
    hold_remaining_frames: usize,
    clip_latched: bool,
    clip_count: u64,
}

impl ChannelMeter {
    pub const fn new(peak_hold_frames: usize) -> Self {
        Self {
            peak: 0.0,
            rms: 0.0,
            held_peak: 0.0,
            peak_hold_frames,
            hold_remaining_frames: 0,
            clip_latched: false,
            clip_count: 0,
        }
    }

    pub fn process(&mut self, samples: &[f32]) {
        let mut peak = 0.0_f32;
        let mut sum_squares = 0.0_f64;
        for &sample in samples {
            let sample = sanitize(sample);
            let magnitude = sample.abs();
            peak = peak.max(magnitude);
            sum_squares += f64::from(sample) * f64::from(sample);
            if magnitude >= 1.0 {
                self.clip_latched = true;
                self.clip_count = self.clip_count.saturating_add(1);
            }
        }

        self.peak = peak;
        self.rms = if samples.is_empty() {
            0.0
        } else {
            (sum_squares / samples.len() as f64).sqrt() as f32
        };

        if peak >= self.held_peak {
            self.held_peak = peak;
            self.hold_remaining_frames = self.peak_hold_frames;
        } else if samples.len() >= self.hold_remaining_frames {
            self.held_peak = peak;
            self.hold_remaining_frames = 0;
        } else {
            self.hold_remaining_frames -= samples.len();
        }
    }

    pub const fn peak(&self) -> f32 {
        self.peak
    }

    pub const fn rms(&self) -> f32 {
        self.rms
    }

    pub const fn held_peak(&self) -> f32 {
        self.held_peak
    }

    pub const fn hold_remaining_frames(&self) -> usize {
        self.hold_remaining_frames
    }

    pub const fn clip_latched(&self) -> bool {
        self.clip_latched
    }

    pub const fn clip_count(&self) -> u64 {
        self.clip_count
    }

    pub fn clear_clip_latch(&mut self) {
        self.clip_latched = false;
    }

    pub fn reset_clip_count(&mut self) {
        self.clip_count = 0;
    }

    pub fn reset(&mut self) {
        self.peak = 0.0;
        self.rms = 0.0;
        self.held_peak = 0.0;
        self.hold_remaining_frames = 0;
        self.clip_latched = false;
        self.clip_count = 0;
    }
}

fn sanitize(sample: f32) -> f32 {
    if sample.is_nan() {
        0.0
    } else if sample == f32::INFINITY {
        1.0
    } else if sample == f32::NEG_INFINITY {
        -1.0
    } else {
        sample
    }
}
