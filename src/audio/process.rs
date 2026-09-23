//! Signal conditioning between the device and the decoders.
//!
//! Chain order: direct current removal, operator gain, level measurement,
//! then the rate conversion, then automatic gain control. Measurement sits
//! before the automatic gain so the meter reports the real line level rather
//! than the level after the loop has flattened it.

/// Corner of the offset blocker, in hertz.
///
/// Public because the display has to agree with it. A bin below this corner
/// has been emptied by the blocker and no longer describes the band, so the
/// spectrum has to cover the same range rather than draw the skirt of the
/// removal as a permanent line at the tuning point.
///
/// Twenty hertz is low enough to leave the slowest keying envelope intact and
/// high enough to settle in a few tens of milliseconds.
pub const DC_CORNER_HZ: f32 = 20.0;

/// Single pole high pass that removes the direct current offset a sound card
/// input carries. The transfer function is y = x - x1 + r * y1, which places a
/// zero at direct current and a pole just inside it.
pub struct DcBlocker {
    r: f32,
    x1: f32,
    y1: f32,
    /// Second channel. Separate state, because the two offsets are unrelated.
    x1b: f32,
    y1b: f32,
}

impl DcBlocker {
    pub fn new(sample_rate: u32) -> DcBlocker {
        let fs = sample_rate.max(1) as f32;
        let r = (1.0 - std::f32::consts::TAU * DC_CORNER_HZ / fs).clamp(0.5, 0.9999);
        DcBlocker { r, x1: 0.0, y1: 0.0, x1b: 0.0, y1b: 0.0 }
    }

    /// Removes the offset from both channels.
    ///
    /// Two independent states, because the two channels have two independent
    /// offsets: they come from two converter inputs and nothing ties them
    /// together.
    pub fn process_pairs(&mut self, buf: &mut [[f32; 2]]) {
        for pair in buf.iter_mut() {
            let x = pair[0];
            let y = x - self.x1 + self.r * self.y1;
            self.x1 = x;
            self.y1 = y;
            pair[0] = y;

            let x = pair[1];
            let y = x - self.x1b + self.r * self.y1b;
            self.x1b = x;
            self.y1b = y;
            pair[1] = y;
        }
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.y1 = 0.0;
        self.x1b = 0.0;
        self.y1b = 0.0;
    }
}

/// Peak and root mean square measurement over the blocks that pass through.
pub struct Meter {
    peak: f32,
    /// Decay applied to the held peak once per block, so a transient stays
    /// visible for a moment instead of flashing for one frame.
    decay: f32,
    sum_squares: f64,
    count: u64,
    last_rms: f32,
}

impl Meter {
    pub fn new() -> Meter {
        Meter { peak: 0.0, decay: 0.85, sum_squares: 0.0, count: 0, last_rms: 0.0 }
    }

    /// Measures the reduction of the pair.
    ///
    /// The reduction rather than one channel, because that is what the operator
    /// sees on the display and in the decoders: a meter reading one channel
    /// while the difference mode is in force would report a level nothing else
    /// in the application is working with.
    pub fn process_pairs(&mut self, buf: &[[f32; 2]], mode: crate::config::settings::ChannelMode) {
        if buf.is_empty() {
            return;
        }
        self.peak *= self.decay;
        let mut sum = 0.0f64;
        for &pair in buf {
            let s = super::convert::Converter::reduce(mode, pair);
            let a = s.abs();
            if a > self.peak {
                self.peak = a;
            }
            sum += (s as f64) * (s as f64);
        }
        self.sum_squares += sum;
        self.count += buf.len() as u64;

        // The averaging window is reset once it holds enough samples to be
        // meaningful, which keeps the reading responsive without a filter.
        if self.count >= 1024 {
            self.last_rms = (self.sum_squares / self.count as f64).sqrt() as f32;
            self.sum_squares = 0.0;
            self.count = 0;
        }
    }

    pub fn peak(&self) -> f32 {
        self.peak
    }

    pub fn rms(&self) -> f32 {
        self.last_rms
    }
}

impl Default for Meter {
    fn default() -> Meter {
        Meter::new()
    }
}

/// Automatic gain control settings.
///
/// The loop is not part of the capture chain, see the note on Pipeline. It is
/// kept for the monitor output, which feeds the operator headphones rather than
/// the decoders and therefore wants a constant listening level.
#[derive(Debug, Clone, Copy)]
pub struct AgcConfig {
    pub enabled: bool,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub target_db: f32,
}

/// Envelope following automatic gain control. Reserved for the monitor output.
///
/// The envelope rises with the attack coefficient and falls with the release
/// coefficient, and the gain is the ratio of the target to the envelope. A
/// separate ceiling stops the loop from amplifying pure noise during a pause.
#[allow(dead_code)]
pub struct Agc {
    enabled: bool,
    attack: f32,
    release: f32,
    target: f32,
    envelope: f32,
    gain: f32,
    max_gain: f32,
}

impl Agc {
    pub fn new(cfg: AgcConfig, sample_rate: u32) -> Agc {
        let fs = sample_rate.max(1) as f32;
        Agc {
            enabled: cfg.enabled,
            attack: time_constant(cfg.attack_ms, fs),
            release: time_constant(cfg.release_ms, fs),
            target: db_to_linear(cfg.target_db),
            envelope: 0.0,
            gain: 1.0,
            max_gain: db_to_linear(40.0),
        }
    }

    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.enabled {
            return;
        }
        for s in buf.iter_mut() {
            let level = s.abs();
            let coefficient = if level > self.envelope { self.attack } else { self.release };
            self.envelope += coefficient * (level - self.envelope);

            // Below the floor the loop holds its gain instead of chasing the
            // noise upwards.
            const FLOOR: f32 = 1e-5;
            if self.envelope > FLOOR {
                let wanted = (self.target / self.envelope).min(self.max_gain);
                // The gain itself is smoothed, otherwise a sample to sample
                // correction would modulate the signal it is measuring.
                self.gain += self.release * (wanted - self.gain);
            }
            *s *= self.gain;
        }
    }

    pub fn gain_db(&self) -> f32 {
        linear_to_db(self.gain)
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain = 1.0;
    }
}

/// Front section of the chain, applied at the device sample rate.
pub struct Frontend {
    dc: Option<DcBlocker>,
    gain: f32,
    pub meter: Meter,
}

impl Frontend {
    pub fn new(sample_rate: u32, dc_block: bool, gain_db: f32) -> Frontend {
        Frontend {
            dc: if dc_block { Some(DcBlocker::new(sample_rate)) } else { None },
            gain: db_to_linear(gain_db),
            meter: Meter::new(),
        }
    }

    /// Conditions both channels identically.
    ///
    /// Identically is the requirement, not a convenience. Image rejection in a
    /// quadrature receiver is decided by how well the two paths match, so a
    /// front end that treated them differently would introduce exactly the
    /// imbalance the correction downstream exists to remove, and the correction
    /// would then be fighting this stage rather than the hardware.
    ///
    /// The meter reads the reduction rather than one channel, because that is
    /// the signal the operator is looking at everywhere else.
    pub fn process(&mut self, buf: &mut [[f32; 2]], mode: crate::config::settings::ChannelMode) {
        if let Some(dc) = self.dc.as_mut() {
            dc.process_pairs(buf);
        }
        if (self.gain - 1.0).abs() > 1e-6 {
            for pair in buf.iter_mut() {
                pair[0] *= self.gain;
                pair[1] *= self.gain;
            }
        }
        self.meter.process_pairs(buf, mode);
    }
}

/// One pole coefficient for a given time constant in milliseconds.
fn time_constant(ms: f32, sample_rate: f32) -> f32 {
    let samples = (ms.max(0.01) * 0.001 * sample_rate).max(1.0);
    (1.0 - (-1.0 / samples).exp()).clamp(1e-6, 1.0)
}

pub fn db_to_linear(db: f32) -> f32 {
    10.0f32.powf(db / 20.0)
}

pub fn linear_to_db(v: f32) -> f32 {
    // The floor keeps the logarithm finite for a silent buffer and matches the
    // dynamic range of a sixteen bit input with room to spare.
    if v <= 1e-9 {
        -180.0
    } else {
        20.0 * v.log10()
    }
}