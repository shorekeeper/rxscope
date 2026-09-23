//! Gain control with hang, and the squelch.
//!
//! ## Why hang is not an optional refinement
//!
//! A loop with only an attack and a release raises its gain during every pause,
//! because a pause looks exactly like a signal that has faded. On a keyed
//! transmission the pauses are the majority of the time, so the gain spends most
//! of it climbing towards whatever the noise reaches, and each new element
//! arrives at full amplification and is clipped before the attack can act.
//!
//! Hang holds the gain where the signal left it for a fixed period. Inside that
//! period a pause costs nothing; past it the release takes over and the loop
//! recovers for a station that really has gone. The period therefore has to
//! exceed the longest pause worth ignoring, which for speech is a fraction of a
//! second and for keying is the gap between words.
//!
//! ## Why this loop is not in the decoder path
//!
//! Everything above describes exactly the case a keying detector must not have.
//! At twenty words per minute a dash is a hundred and eighty milliseconds and a
//! gap sixty, which is the range of any usable attack and release pair, so the
//! loop flattens the very envelope the detector measures. The decoder path has
//! no gain control at all and this one lives after the detector, where the only
//! consumer is an ear.

/// Ceiling on the gain.
///
/// Without one the loop amplifies silence until the noise reaches the target,
/// which is audible as a rush that swells whenever the band goes quiet. Sixty
/// decibels covers the range between a strong local station and one at the noise
/// floor.
const MAX_GAIN_DB: f32 = 60.0;

/// Level below which the loop stops tracking altogether.
///
/// Distinct from the ceiling above. The ceiling bounds how far the gain may
/// climb; this decides when there is nothing worth climbing towards, and holds
/// the loop rather than letting it wind up against the limit.
const FLOOR: f32 = 1e-6;

pub struct Agc {
    enabled: bool,
    target: f32,
    attack: f32,
    release: f32,
    /// Samples the gain is held after the envelope stops rising.
    hang_samples: u32,
    hang_left: u32,

    envelope: f32,
    gain: f32,
    max_gain: f32,
}

impl Agc {
    pub fn new(rate: u32) -> Agc {
        let mut agc = Agc {
            enabled: true,
            target: 0.25,
            attack: 1.0,
            release: 1.0,
            hang_samples: 0,
            hang_left: 0,
            envelope: 0.0,
            gain: 1.0,
            max_gain: 10.0f32.powf(MAX_GAIN_DB / 20.0),
        };
        agc.configure(rate, true, 5.0, 300.0, 500.0, -12.0);
        agc
    }

    pub fn configure(
        &mut self,
        rate: u32,
        enabled: bool,
        attack_ms: f32,
        hang_ms: f32,
        release_ms: f32,
        target_db: f32,
    ) {
        let fs = rate.max(1) as f32;
        self.enabled = enabled;
        self.attack = coefficient(attack_ms, fs);
        self.release = coefficient(release_ms, fs);
        self.hang_samples = (hang_ms.max(0.0) * 0.001 * fs) as u32;
        self.target = 10.0f32.powf(target_db.clamp(-60.0, 0.0) / 20.0);
    }

    /// Gain currently applied, in decibels. Reads as how much of the range the
    /// loop is using, which is what says whether the input level is sensible.
    pub fn gain_db(&self) -> f32 {
        20.0 * self.gain.max(1e-9).log10()
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain = 1.0;
        self.hang_left = 0;
    }

    #[inline]
    pub fn sample(&mut self, x: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let level = x.abs();

        if level > self.envelope {
            // Rising: the attack follows and the hang timer is reloaded, so a
            // pause is measured from the last thing that was actually there.
            self.envelope += self.attack * (level - self.envelope);
            self.hang_left = self.hang_samples;
        } else if self.hang_left > 0 {
            self.hang_left -= 1;
        } else {
            self.envelope += self.release * (level - self.envelope);
        }

        if self.envelope > FLOOR {
            let wanted = (self.target / self.envelope).min(self.max_gain);
            // The gain itself is smoothed with the release coefficient. Applying
            // the ratio directly would modulate the signal at the sample rate,
            // which is distortion rather than gain control.
            self.gain += self.release * (wanted - self.gain);
        }

        // Clipped rather than wrapped: an overload has to sound like an
        // overload, and a wrap sounds like noise arriving from nowhere.
        (x * self.gain).clamp(-1.0, 1.0)
    }
}

/// Squelch.
///
/// Judged on the level before the gain control, because after it every signal
/// reads at the target and there is nothing left to threshold. Opening and
/// closing at different levels is what stops a signal sitting on the threshold
/// from chattering, and the release is slower than the attack so a gap inside a
/// transmission does not close the gate.
pub struct Squelch {
    enabled: bool,
    threshold: f32,
    hysteresis: f32,
    open: bool,
    envelope: f32,
    attack: f32,
    release: f32,
    /// Ramp applied while the gate moves, so it does not click.
    ramp: f32,
    step: f32,
}

impl Squelch {
    pub fn new(rate: u32) -> Squelch {
        let fs = rate.max(1) as f32;
        Squelch {
            enabled: false,
            threshold: 1e-4,
            // Three decibels between the two thresholds. Enough to cover the
            // variation of a steady signal, small enough that a weak one is not
            // held shut once it has been heard.
            hysteresis: 10.0f32.powf(-3.0 / 20.0),
            open: false,
            envelope: 0.0,
            attack: coefficient(2.0, fs),
            release: coefficient(150.0, fs),
            ramp: 0.0,
            // Five milliseconds from shut to open. Below that the transition is
            // a click; above it the start of a transmission is clipped.
            step: 1.0 / (0.005 * fs).max(1.0),
        }
    }

    pub fn configure(&mut self, enabled: bool, threshold_db: f32) {
        self.enabled = enabled;
        self.threshold = 10.0f32.powf(threshold_db.clamp(-140.0, 0.0) / 20.0);
    }

    pub fn is_open(&self) -> bool {
        !self.enabled || self.open
    }

    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.open = false;
        self.ramp = 0.0;
    }

    /// Measures the level and returns the gate position, nought to one.
    #[inline]
    pub fn sample(&mut self, x: f32) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let level = x.abs();
        let c = if level > self.envelope { self.attack } else { self.release };
        self.envelope += c * (level - self.envelope);

        if self.open {
            if self.envelope < self.threshold * self.hysteresis {
                self.open = false;
            }
        } else if self.envelope > self.threshold {
            self.open = true;
        }

        let wanted = if self.open { 1.0 } else { 0.0 };
        if self.ramp < wanted {
            self.ramp = (self.ramp + self.step).min(wanted);
        } else if self.ramp > wanted {
            self.ramp = (self.ramp - self.step).max(wanted);
        }
        self.ramp
    }
}

/// One pole coefficient for a time constant in milliseconds.
fn coefficient(ms: f32, rate: f32) -> f32 {
    let samples = (ms.max(0.01) * 0.001 * rate).max(1.0);
    (1.0 - (-1.0 / samples).exp()).clamp(1e-6, 1.0)
}