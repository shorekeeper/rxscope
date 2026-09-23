//! Complex bandpass with independent edges, attached to a tuning point.
//!
//! ## Two things, not one
//!
//! The tuning point says what is being received: it is carried into the mixer
//! and is therefore the frequency that ends up at baseband nought. The edges
//! say how much of the neighbourhood comes with it, and they are measured from
//! the tuning point rather than from the edge of the digitized band.
//!
//! Splitting them is what makes a passband shift and a retune different
//! actions. Moving both edges together moves the mixer, and the detector moves
//! its reference by the same amount, so what survives changes while what
//! survives keeps its pitch: that is a passband control. Moving the tuning
//! point moves the mixer alone, so a given signal changes pitch: that is
//! tuning. With one number doing both jobs only the second behaviour exists,
//! and the filter becomes a swept oscillator that happens to pass noise.
//!
//! ## Why the edges are separate from each other
//!
//! They are moved for different reasons. The lower edge is raised to escape hum
//! and the rumble of a fading path; the upper edge is lowered to escape a
//! neighbouring station. Coupling them through a centre means every adjustment
//! of one disturbs the other, and an operator working a crowded band spends the
//! whole time correcting the correction.
//!
//! ## How the edges become a filter
//!
//! A complex signal is mixed down by the tuning point plus the midpoint of the
//! two edges, and then passed through a real lowpass whose cutoff is half their
//! separation. On a complex signal that lowpass is one sided: it keeps a band
//! from minus the cutoff to plus the cutoff around the new origin, which after
//! the mixing is exactly the band between the two stated edges. There is no
//! second image to remove, which is what makes an asymmetric passband
//! expressible at all.
//!
//! The mixer runs on a phase accumulator with the sine and cosine computed per
//! sample rather than on a rotating vector recurrence. A recurrence drifts in
//! amplitude and has to be renormalized, and at the rates in use the
//! transcendentals cost a fraction of a percent of a core.
//!
//! The coefficient table depends on the width alone, so a retune and a shift
//! cost one division each. Only a change of width replans, which is what keeps
//! a slider being dragged from rebuilding five hundred taps per frame.

use super::iq::Complex;

/// Longest lowpass the planner will build.
///
/// A filter this long already places its transition band inside a few hertz at
/// the decoder rate, and its group delay is a fortieth of a second, which is
/// where an operator starts to hear the delay between a key click and the
/// display.
const MAX_TAPS: usize = 511;

/// Shortest, so a wide filter still has a defined shape.
const MIN_TAPS: usize = 15;

/// Transition width, as a fraction of the passband half width.
///
/// A quarter keeps the skirt steep enough to reject an adjacent signal without
/// making the filter so long that the keying it passes is smeared. Below a
/// floor in hertz it stops shrinking, because a narrow filter would otherwise
/// demand a tap count that grows without bound.
const TRANSITION_FRACTION: f32 = 0.25;
const TRANSITION_FLOOR_HZ: f32 = 40.0;

/// Narrowest passband the planner accepts, in hertz.
const MIN_WIDTH_HZ: f32 = 20.0;

pub struct Bandpass {
    rate: f32,
    /// Where the receiver listens inside the captured span.
    tune_hz: f32,
    /// Edges, measured from the tuning point.
    low_hz: f32,
    high_hz: f32,
    /// Distance from the tuning point to the middle of the passband.
    rel_centre_hz: f32,
    /// Half width the coefficient table was planned for.
    half_width_hz: f32,

    taps: Vec<f32>,
    /// Delay line, complex, indexed by one ring position so the convolution
    /// reads both parts together.
    history: Vec<Complex>,
    pos: usize,

    /// Phase of the mixer, kept in double so a long run does not accumulate a
    /// visible error before the wrap.
    phase: f64,
    step: f64,
}

impl Bandpass {
    pub fn new(rate: u32) -> Bandpass {
        let mut filter = Bandpass {
            rate: rate.max(1) as f32,
            tune_hz: 0.0,
            low_hz: 0.0,
            high_hz: 0.0,
            rel_centre_hz: 0.0,
            half_width_hz: 0.0,
            taps: Vec::new(),
            history: Vec::new(),
            pos: 0,
            phase: 0.0,
            step: 0.0,
        };
        filter.set_band(0.0, 300.0, 2700.0);
        filter
    }

    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Distance from the tuning point to the middle of the passband.
    ///
    /// What the detector has to put back. The tuning point itself is not put
    /// back, and that is precisely the point: the audio a detector produces is
    /// the offset from the tuning point, not from the edge of the band.
    pub fn rel_centre_hz(&self) -> f32 {
        self.rel_centre_hz
    }

    /// Frequency the mixer removes, for a caller that needs the absolute one.
    pub fn mixer_hz(&self) -> f32 {
        self.tune_hz + self.rel_centre_hz
    }

    /// Group delay, in samples. A symmetric filter delays by half its length.
    pub fn delay(&self) -> usize {
        self.taps.len() / 2
    }

    /// Places the passband.
    ///
    /// The edges are relative to the tuning point. A retune or a shift changes
    /// the mixer alone and is applied at once; only a change of width replans
    /// the coefficients, and a change too small to alter the plan is ignored so
    /// a slider being dragged does not rebuild the table on every frame.
    ///
    /// The delay line is kept across a retune. It holds the previous band, so a
    /// retune releases a filter length of the old one into the new; that is a
    /// few tens of milliseconds and it sounds like the sweep of a real dial,
    /// whereas clearing it produces a click on every step of a tuning gesture.
    pub fn set_band(&mut self, tune_hz: f32, low_hz: f32, high_hz: f32) {
        let nyquist = self.rate * 0.5;
        let tune = tune_hz.clamp(-nyquist, nyquist);
        let low = low_hz.clamp(-2.0 * nyquist, 2.0 * nyquist);
        let high = high_hz.max(low + MIN_WIDTH_HZ);

        let rel_centre = (low + high) * 0.5;
        let half_width = ((high - low) * 0.5).clamp(MIN_WIDTH_HZ * 0.5, nyquist);

        // The mixer is clamped rather than the edges: an operator who pushed the
        // band past the Nyquist frequency asked for something the transform
        // cannot represent, and folding it back is more informative than
        // silently narrowing the passband they were setting.
        let mixer = (tune + rel_centre).clamp(-nyquist, nyquist);

        self.tune_hz = tune;
        self.low_hz = low;
        self.high_hz = high;
        self.rel_centre_hz = rel_centre;
        self.step = std::f64::consts::TAU * mixer as f64 / self.rate as f64;

        if (half_width - self.half_width_hz).abs() < 1.0 {
            return;
        }
        self.plan(half_width);
    }

    /// Edges as they were stated, relative to the tuning point.
    pub fn edges(&self) -> (f32, f32) {
        (self.low_hz, self.high_hz)
    }

    fn plan(&mut self, half_width: f32) {
        self.half_width_hz = half_width;
        let transition = (half_width * TRANSITION_FRACTION).max(TRANSITION_FLOOR_HZ);

        // A windowed sinc of length four over the normalized transition width
        // places the stopband of a Blackman window at about seventy five
        // decibels, which is below the noise of any receiver this will meet.
        let normalized = transition / self.rate;
        let mut n = (4.0 / normalized.max(1e-4)).ceil() as usize;
        if n % 2 == 0 {
            n += 1;
        }
        let n = n.clamp(MIN_TAPS, MAX_TAPS);

        self.taps.clear();
        self.taps.reserve(n);
        let cutoff = (half_width / self.rate) as f64;
        let centre = (n / 2) as f64;
        let mut sum = 0.0f64;
        for k in 0..n {
            let x = k as f64 - centre;
            let ideal = if x.abs() < 1e-9 {
                2.0 * cutoff
            } else {
                (std::f64::consts::TAU * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let t = k as f64 / (n - 1) as f64;
            let two_pi = std::f64::consts::TAU;
            let window = 0.42 - 0.5 * (two_pi * t).cos() + 0.08 * (2.0 * two_pi * t).cos();
            let v = ideal * window;
            sum += v;
            self.taps.push(v as f32);
        }
        // Unity gain at the centre of the passband, so changing the width does
        // not change the level and the gain control has nothing to chase.
        if sum.abs() > 1e-12 {
            let k = (1.0 / sum) as f32;
            for tap in self.taps.iter_mut() {
                *tap *= k;
            }
        }

        // The delay line is resized rather than merely cleared, and only here,
        // where its length genuinely changed.
        self.history.clear();
        self.history.resize(n, Complex::default());
        self.pos = 0;
    }

    pub fn reset(&mut self) {
        for v in self.history.iter_mut() {
            *v = Complex::default();
        }
        self.pos = 0;
        self.phase = 0.0;
    }

    /// Filters one sample and returns the baseband result.
    ///
    /// Centred on the middle of the passband rather than on the tuning point.
    /// Restoring the offset from the tuning point is the business of the
    /// detector, and keeping it there is what makes a passband shift silent.
    #[inline]
    pub fn sample(&mut self, z: Complex) -> Complex {
        // Mix down. The phase is advanced first and wrapped, so it never grows
        // large enough for the sine to lose precision.
        self.phase -= self.step;
        if self.phase < -std::f64::consts::TAU {
            self.phase += std::f64::consts::TAU;
        } else if self.phase > std::f64::consts::TAU {
            self.phase -= std::f64::consts::TAU;
        }
        let (s, c) = (self.phase.sin() as f32, self.phase.cos() as f32);
        let mixed = z.mul(Complex::new(c, s));

        self.history[self.pos] = mixed;
        self.pos = (self.pos + 1) % self.history.len();

        let n = self.history.len();
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for (k, &tap) in self.taps.iter().enumerate() {
            let at = (self.pos + n - 1 - k) % n;
            let h = self.history[at];
            re += tap * h.re;
            im += tap * h.im;
        }
        Complex { re, im }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::receiver::detector::DetectorBank;
    use crate::dsp::receiver::iq::IqFront;
    use crate::config::settings::Detector as Kind;

    /// Amplitude a tone reaches after the filter has settled.
    fn response(tune: f32, low: f32, high: f32, hz: f32) -> f32 {
        let rate = 12_000u32;
        let mut front = IqFront::new();
        front.configure(false, false, 0.0, 0.0);
        let mut filter = Bandpass::new(rate);
        filter.set_band(tune, low, high);

        let mut peak = 0.0f32;
        let settle = filter.delay() * 4 + 2048;
        for n in 0..(settle + 4096) {
            let t = std::f32::consts::TAU * hz * n as f32 / rate as f32;
            let z = front.sample(t.sin(), 0.0);
            let out = filter.sample(z);
            if n > settle {
                peak = peak.max(out.magnitude());
            }
        }
        peak
    }

    /// Pitch a tone comes out at, taken from the zero crossings of the
    /// demodulated audio. Coarse on purpose: the question is whether the pitch
    /// moved, not what it is to the hertz.
    fn pitch(tune: f32, low: f32, high: f32, hz: f32) -> f32 {
        let rate = 12_000u32;
        let mut front = IqFront::new();
        front.configure(false, false, 0.0, 0.0);
        let mut filter = Bandpass::new(rate);
        filter.set_band(tune, low, high);
        let mut detector = DetectorBank::new(rate);
        detector.configure(Kind::Usb, filter.rel_centre_hz());

        let settle = filter.delay() * 4 + 4096;
        let measure = 12_000usize;
        let mut previous = 0.0f32;
        let mut crossings = 0usize;

        for n in 0..(settle + measure) {
            let t = std::f32::consts::TAU * hz * n as f32 / rate as f32;
            let z = filter.sample(front.sample(t.sin(), 0.0));
            let audio = detector.sample(z);
            if n > settle {
                if previous <= 0.0 && audio > 0.0 {
                    crossings += 1;
                }
                previous = audio;
            }
        }
        crossings as f32 * rate as f32 / measure as f32
    }

    #[test]
    fn a_tone_inside_the_passband_survives() {
        let inside = response(0.0, 500.0, 2500.0, 1500.0);
        assert!(inside > 0.9, "passband loss, reached {:.3}", inside);
    }

    #[test]
    fn a_tone_outside_is_rejected() {
        let outside = response(0.0, 500.0, 2500.0, 3500.0);
        assert!(outside < 0.02, "stopband leakage {:.4}", outside);
    }

    /// The reason the edges are separate. A passband that is not symmetric
    /// about anything has to reject on one side while passing close by on the
    /// other, which a centre and a width cannot express.
    #[test]
    fn the_two_edges_move_independently() {
        assert!(response(0.0, 800.0, 1100.0, 950.0) > 0.9);
        assert!(response(0.0, 800.0, 1100.0, 600.0) < 0.05);
        assert!(response(0.0, 800.0, 1100.0, 1400.0) < 0.05);
    }

    /// The band follows the tuning point, which is the whole reason it exists.
    #[test]
    fn the_passband_follows_the_tuning_point() {
        // Relative 300 to 2700, tuned to three kilohertz: absolute 3300 to 5700.
        assert!(response(3000.0, 300.0, 2700.0, 4500.0) > 0.9);
        assert!(response(3000.0, 300.0, 2700.0, 1500.0) < 0.05);
    }

    /// A passband shift is not a retune. Moving both edges changes what
    /// survives and leaves the pitch of what survives alone; that is what an
    /// intermediate frequency shift does on a receiver, and it is the property
    /// a single absolute passband cannot have.
    #[test]
    fn shifting_the_passband_leaves_the_pitch_alone() {
        let narrow = pitch(1000.0, 300.0, 1500.0, 1900.0);
        let shifted = pitch(1000.0, 600.0, 1800.0, 1900.0);
        assert!(
            (narrow - shifted).abs() < 30.0,
            "the pitch moved with the passband: {:.0} then {:.0}",
            narrow,
            shifted
        );
        assert!((narrow - 900.0).abs() < 40.0, "pitch is not the offset: {:.0}", narrow);
    }

    /// Retuning is what changes the pitch, and by the amount it moved.
    #[test]
    fn retuning_changes_the_pitch() {
        let before = pitch(1000.0, 300.0, 2700.0, 2000.0);
        let after = pitch(1500.0, 300.0, 2700.0, 2000.0);
        assert!((before - 1000.0).abs() < 40.0, "{:.0}", before);
        assert!((after - 500.0).abs() < 40.0, "{:.0}", after);
    }
}