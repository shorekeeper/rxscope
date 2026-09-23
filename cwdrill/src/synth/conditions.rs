//! Band conditions.
//!
//! ## What separates a trainer from the air
//!
//! A clean tone in silence is a signal nobody receives. Every real signal
//! arrives inside noise, fades, drifts, and shares its frequency with somebody
//! else, and a student who has only copied the clean one hears real traffic as a
//! different alphabet: the ear has learned to key on a tone against nothing
//! rather than on a tone against a background.
//!
//! ## Why the noise is filtered
//!
//! White noise sounds nothing like a band. What a receiver delivers is noise that
//! has been through its filter, so it is concentrated around the tone and has the
//! character of a rush rather than a hiss. Filtering it here is what makes the
//! stated ratio mean what an operator means by it: the tone against the noise in
//! the bandwidth the tone is being listened to in.
//!
//! The level is corrected by measurement rather than by arithmetic. A bandpass
//! has a gain that depends on its geometry, so a stated coefficient would deliver
//! the requested ratio only at one bandwidth; a slow average of the output and a
//! scale factor against it deliver it at every bandwidth and go on delivering it
//! when the geometry changes.

use crate::core::Rng;

/// Bandwidth of the noise filter, in hertz.
///
/// Comparable to a narrow receiver filter, which is what a student listening to
/// keying has in circuit. Wider would be a wider filter, which is a different
/// exercise and not a harder one: the noise would be louder but no closer to the
/// tone.
const NOISE_BANDWIDTH_HZ: f32 = 400.0;

/// Time constant of the level correction, in seconds.
///
/// Long enough that the keying does not modulate it and short enough that a
/// change of bandwidth settles inside a group.
const LEVEL_TAU_S: f32 = 0.25;

/// Duration of one impulse, in milliseconds.
///
/// What a distant discharge sounds like through a narrow filter: the filter is
/// what stretches an instantaneous event into something audible, so this is the
/// filter rather than the event.
const IMPULSE_MS: f32 = 3.0;

/// Bound on the drift, in hertz.
///
/// Past this a transmitter would be retuned, which is what the reversal
/// represents: the drift is a triangle rather than a walk, because a walk
/// eventually leaves the passband and stays there.
const DRIFT_BOUND_HZ: f32 = 90.0;

/// Everything the conditions read, taken from the shared block once per buffer.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConditionSnapshot {
    pub noise: bool,
    pub snr_db: f32,
    pub qsb: bool,
    pub qsb_rate_hz: f32,
    pub qsb_depth_db: f32,
    pub qrm: bool,
    pub qrm_offset_hz: f32,
    pub qrm_level_db: f32,
    pub qrn: bool,
    pub qrn_per_minute: f32,
    pub drift_hz_per_min: f32,
}

/// Two pole bandpass in the state variable form.
///
/// Chosen over a biquad because the two coefficients follow directly from the
/// centre and the bandwidth, so a change of either needs no factorization: at
/// the rate the operator moves a control that matters less than the fact that
/// the form is stable at every setting a slider can reach.
struct Bandpass {
    f: f32,
    q: f32,
    low: f32,
    band: f32,
}

impl Bandpass {
    fn new() -> Bandpass {
        Bandpass { f: 0.1, q: 0.2, low: 0.0, band: 0.0 }
    }

    fn configure(&mut self, centre_hz: f32, bandwidth_hz: f32, rate: f32) {
        // The frequency coefficient is bounded well below the stability limit of
        // the form, which is two: a tone near the Nyquist frequency would
        // otherwise make the filter oscillate rather than resonate.
        self.f = (2.0 * std::f32::consts::PI * centre_hz / rate).clamp(0.001, 1.0);
        self.q = (bandwidth_hz / centre_hz.max(1.0)).clamp(0.01, 2.0);
    }

    #[inline]
    fn tick(&mut self, input: f32) -> f32 {
        self.low += self.f * self.band;
        let high = input - self.low - self.q * self.band;
        self.band += self.f * high;
        self.band
    }

    fn reset(&mut self) {
        self.low = 0.0;
        self.band = 0.0;
    }
}

pub struct Conditions {
    rate: f32,
    snap: ConditionSnapshot,

    rng: Rng,
    filter: Bandpass,
    /// Running mean square of the filter output, for the level correction.
    level: f32,
    level_alpha: f32,
    /// Amplitude the noise is scaled to.
    noise_gain: f32,

    qsb_phase: f64,
    qsb_step: f64,

    impulse_left: usize,
    impulse_length: usize,
    impulse_gain: f32,
    /// Probability that an impulse begins on any one sample.
    impulse_chance: f32,

    drift_hz: f32,
    drift_rising: bool,
}

impl Conditions {
    pub fn new(rate: u32) -> Conditions {
        let rate = rate.max(1) as f32;
        Conditions {
            rate,
            snap: ConditionSnapshot::default(),
            rng: Rng::from_clock(),
            filter: Bandpass::new(),
            level: 0.0,
            level_alpha: (1.0 / (LEVEL_TAU_S * rate)).clamp(1e-6, 1.0),
            noise_gain: 0.0,
            qsb_phase: 0.0,
            qsb_step: 0.0,
            impulse_left: 0,
            impulse_length: ((IMPULSE_MS * 0.001 * rate) as usize).max(1),
            impulse_gain: 0.0,
            impulse_chance: 0.0,
            drift_hz: 0.0,
            drift_rising: true,
        }
    }

    /// Applies the settings for one buffer.
    ///
    /// The signal amplitude is passed in because the noise level is stated
    /// against it: a ratio has to be a ratio of something, and the something is
    /// whatever the level control is delivering.
    pub fn configure(&mut self, snap: ConditionSnapshot, tone_hz: f32, signal: f32) {
        let changed = snap.snr_db != self.snap.snr_db;
        self.snap = snap;

        self.filter.configure(tone_hz, NOISE_BANDWIDTH_HZ, self.rate);
        // Amplitude rather than power: the correction below measures a mean
        // square and takes its root, so the two are in the same units.
        self.noise_gain = signal * 10.0f32.powf(-snap.snr_db / 20.0);
        if changed {
            // The correction is left to converge rather than reset. A reset
            // would put a step in the noise at the moment the operator moved the
            // control, which is louder than the change they asked for.
        }

        self.qsb_step = std::f64::consts::TAU * snap.qsb_rate_hz.max(0.001) as f64
            / self.rate as f64;
        self.impulse_chance = if snap.qrn {
            (snap.qrn_per_minute / 60.0 / self.rate).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }

    /// Advances the drift by one buffer.
    ///
    /// Per buffer rather than per sample: at a hundred hertz a minute the change
    /// inside twenty milliseconds is three hundredths of a hertz, and a per
    /// sample update would cost a multiply to deliver it.
    pub fn advance(&mut self, seconds: f32) {
        let rate = self.snap.drift_hz_per_min;
        if rate == 0.0 {
            self.drift_hz = 0.0;
            return;
        }
        let step = rate.abs() * seconds / 60.0;
        if self.drift_rising {
            self.drift_hz += step;
            if self.drift_hz >= DRIFT_BOUND_HZ {
                self.drift_hz = DRIFT_BOUND_HZ;
                self.drift_rising = false;
            }
        } else {
            self.drift_hz -= step;
            if self.drift_hz <= -DRIFT_BOUND_HZ {
                self.drift_hz = -DRIFT_BOUND_HZ;
                self.drift_rising = true;
            }
        }
    }

    /// Frequency the tone is offset by.
    pub fn drift_hz(&self) -> f32 {
        if self.snap.drift_hz_per_min == 0.0 {
            0.0
        } else {
            self.drift_hz
        }
    }

    /// Amplitude factor the signal is multiplied by.
    ///
    /// Fading applies to the signal and not to the noise, which is what fading
    /// is: the path attenuates, and the noise of the receiver does not travel
    /// down it.
    #[inline]
    pub fn signal_gain(&mut self) -> f32 {
        if !self.snap.qsb {
            return 1.0;
        }
        self.qsb_phase += self.qsb_step;
        if self.qsb_phase > std::f64::consts::TAU {
            self.qsb_phase -= std::f64::consts::TAU;
        }
        // Sinusoidal in decibels rather than in amplitude, because a fade of
        // twelve decibels means twelve decibels at the bottom rather than a
        // twelfth of the amplitude.
        let cycle = (self.qsb_phase.sin() as f32 * 0.5) - 0.5;
        10.0f32.powf(self.snap.qsb_depth_db * cycle / 20.0)
    }

    /// Everything that is added to the signal rather than multiplied by it.
    #[inline]
    pub fn additive(&mut self) -> f32 {
        let mut out = 0.0f32;

        if self.snap.noise {
            let white = self.rng.symmetric();
            let filtered = self.filter.tick(white);
            // The correction, see the note at the head of the file: a slow mean
            // square of the output and a scale against its root deliver the
            // stated ratio whatever the filter geometry turns out to be.
            self.level += self.level_alpha * (filtered * filtered - self.level);
            let rms = self.level.max(1e-12).sqrt();
            out += filtered / rms * self.noise_gain;
        }

        if self.impulse_left > 0 {
            self.impulse_left -= 1;
            // Decaying rather than square, because a square burst is a click at
            // both ends and only one of them is the discharge.
            let phase = self.impulse_left as f32 / self.impulse_length as f32;
            out += self.rng.symmetric() * self.impulse_gain * phase;
        } else if self.impulse_chance > 0.0 && self.rng.chance(self.impulse_chance) {
            self.impulse_left = self.impulse_length;
            // Loud, because that is what an impulse is: a discharge through a
            // narrow filter arrives well above the noise or it is not heard at
            // all.
            self.impulse_gain = self.noise_gain.max(0.02) * 12.0;
        }

        out
    }

    /// Amplitude the interfering station is scaled to.
    pub fn qrm_gain(&self, signal: f32) -> f32 {
        if !self.snap.qrm {
            return 0.0;
        }
        signal * 10.0f32.powf(self.snap.qrm_level_db / 20.0)
    }

    pub fn qrm_offset_hz(&self) -> f32 {
        if self.snap.qrm {
            self.snap.qrm_offset_hz
        } else {
            0.0
        }
    }

    pub fn qrm_wanted(&self) -> bool {
        self.snap.qrm
    }

    pub fn reset(&mut self) {
        self.filter.reset();
        self.level = 0.0;
        self.impulse_left = 0;
        self.qsb_phase = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> ConditionSnapshot {
        ConditionSnapshot {
            noise: true,
            snr_db: 12.0,
            ..ConditionSnapshot::default()
        }
    }

    #[test]
    fn the_noise_reaches_the_stated_ratio() {
        // The whole reason the level is corrected by measurement. A stated
        // coefficient would deliver the requested ratio at one bandwidth and
        // something else at every other, and the operator would be setting a
        // number that means nothing.
        let mut c = Conditions::new(48_000);
        c.configure(snapshot(), 600.0, 0.25);

        // Settled first: the correction is a slow average and its first second
        // is the convergence rather than the answer.
        for _ in 0..48_000 {
            c.additive();
        }
        let mut sum = 0.0f64;
        for _ in 0..48_000 {
            let v = c.additive();
            sum += (v * v) as f64;
        }
        let rms = (sum / 48_000.0).sqrt() as f32;
        let wanted = 0.25 * 10.0f32.powf(-12.0 / 20.0);
        assert!(
            (rms / wanted - 1.0).abs() < 0.15,
            "noise reached {:.5} against {:.5}",
            rms,
            wanted
        );
    }

    #[test]
    fn fading_reaches_its_depth_and_no_further() {
        let mut c = Conditions::new(48_000);
        let mut snap = ConditionSnapshot::default();
        snap.qsb = true;
        snap.qsb_rate_hz = 1.0;
        snap.qsb_depth_db = 20.0;
        c.configure(snap, 600.0, 0.25);

        let mut lowest = 1.0f32;
        let mut highest = 0.0f32;
        for _ in 0..96_000 {
            let g = c.signal_gain();
            lowest = lowest.min(g);
            highest = highest.max(g);
        }
        // The top of the cycle is unity: fading attenuates rather than
        // amplifying, or the level control would mean two different things.
        assert!((highest - 1.0).abs() < 0.01, "peaked at {}", highest);
        let depth = -20.0 * lowest.log10();
        assert!((depth - 20.0).abs() < 0.5, "faded by {:.1} dB", depth);
    }

    #[test]
    fn the_drift_turns_back_rather_than_walking_away() {
        let mut c = Conditions::new(48_000);
        let mut snap = ConditionSnapshot::default();
        snap.drift_hz_per_min = 600.0;
        c.configure(snap, 600.0, 0.25);

        let mut reached = 0.0f32;
        for _ in 0..600 {
            c.advance(1.0);
            reached = reached.max(c.drift_hz().abs());
            assert!(
                c.drift_hz().abs() <= DRIFT_BOUND_HZ + 1.0,
                "drifted to {}",
                c.drift_hz()
            );
        }
        // And it really did travel: a bound that was never reached would make
        // the test pass on a drift that does nothing.
        assert!(reached > DRIFT_BOUND_HZ * 0.9);
    }
}