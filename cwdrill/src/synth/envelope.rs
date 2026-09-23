//! Keying envelope.
//!
//! ## Why the shape is the first thing an operator hears
//!
//! A tone switched on instantaneously has a spectrum that reaches across the
//! whole band, and through a receiver that reads as a click either side of the
//! note. On a trainer the consequence is worse than untidy: the ear keys on the
//! click, because it is louder and sharper than the tone, and a student trained
//! that way cannot copy a properly shaped signal at all. They have learned to
//! hear the edge rather than the element.
//!
//! So the default is a shaped edge over a few milliseconds, which is what a
//! transmitter with a shaping network produces. The hard edge remains on offer
//! because recognizing one is itself worth practising: a badly adjusted
//! transmitter is a thing that happens, and a student who has only ever heard
//! shaped keying is surprised by it.
//!
//! ## Why three shapes rather than one
//!
//! They differ in how fast the spectrum falls away, and the difference is
//! audible as how much the note seems to spread.
//!
//! The raised cosine has a continuous first derivative and a discontinuous
//! second, so its spectrum falls at a fixed rate; it is what most equipment
//! produces and what a student should be trained against.
//!
//! The Gaussian form has no discontinuity at any order, because the envelope is
//! the integral of a Gaussian and every derivative vanishes at both ends. Its
//! spectrum falls faster than any polynomial, which is what makes it the
//! cleanest keying there is and also what makes it sound slightly softer than
//! the real thing.

use crate::config::settings::EnvelopeShape;

/// Half width of the Gaussian, in standard deviations.
///
/// At two and a half the integral is within four parts in ten thousand of its
/// limits, so the normalization below removes a negligible amount rather than a
/// visible step. Wider would flatten the middle of the ramp for nothing;
/// narrower would leave a step at the ends, which is the discontinuity the shape
/// exists to avoid.
const GAUSS_HALF_WIDTH: f32 = 2.5;

pub struct Envelope {
    shape: EnvelopeShape,
    rise: usize,
    fall: usize,
    /// Endpoints of the Gaussian integral, so the ramp is normalized without
    /// two calls to the approximation per sample.
    gauss_lo: f32,
    gauss_span: f32,
}

impl Envelope {
    pub fn new() -> Envelope {
        let mut env = Envelope {
            shape: EnvelopeShape::RaisedCosine,
            rise: 0,
            fall: 0,
            gauss_lo: 0.0,
            gauss_span: 1.0,
        };
        env.prime_gauss();
        env
    }

    fn prime_gauss(&mut self) {
        let lo = 0.5 * (1.0 + erf(-GAUSS_HALF_WIDTH));
        let hi = 0.5 * (1.0 + erf(GAUSS_HALF_WIDTH));
        self.gauss_lo = lo;
        self.gauss_span = (hi - lo).max(1e-6);
    }

    /// Applies the stated shape at a sample rate.
    ///
    /// A hard edge has no duration to state, which is what makes it hard, so the
    /// two edge lengths are forced to nought there rather than being ignored
    /// further down: a ramp of one sample is not a hard edge, it is a very short
    /// ramp with a spectrum of its own.
    pub fn configure(&mut self, shape: EnvelopeShape, rise_ms: f32, fall_ms: f32, rate: f32) {
        self.shape = shape;
        if shape == EnvelopeShape::Hard {
            self.rise = 0;
            self.fall = 0;
            return;
        }
        let per_ms = rate * 0.001;
        self.rise = (rise_ms.max(0.0) * per_ms).round() as usize;
        self.fall = (fall_ms.max(0.0) * per_ms).round() as usize;
    }

    /// Amplitude at a position inside an element of a stated length.
    ///
    /// The two edges are clamped to half the element each, so a rise longer than
    /// the element produces a triangle rather than an amplitude above one: at
    /// sixty words a minute a dot is twenty milliseconds, and a five millisecond
    /// pair of edges already occupies half of it.
    #[inline]
    pub fn amplitude(&self, position: usize, length: usize) -> f32 {
        if length == 0 {
            return 0.0;
        }
        let half = length / 2;
        let rise = self.rise.min(half);
        let fall = self.fall.min(half);

        if rise > 0 && position < rise {
            // The half sample offset centres the ramp on the samples it covers,
            // so the first sample is not silent and the last is not already at
            // full amplitude.
            return self.ramp((position as f32 + 0.5) / rise as f32);
        }
        if fall > 0 && position + fall >= length {
            let remaining = length - position;
            return self.ramp((remaining as f32 - 0.5) / fall as f32);
        }
        1.0
    }

    /// Samples the rising edge occupies.
    ///
    /// Read by the hand key gate, which has no element length to work from and
    /// therefore has to walk the edge itself.
    pub fn rise(&self) -> usize {
        self.rise
    }

    pub fn fall(&self) -> usize {
        self.fall
    }

    /// Amplitude at a position along an edge.
    ///
    /// Exposed for the same reason: a gate is an edge with no element behind it,
    /// and duplicating the shape would be a second place for it to be wrong.
    #[inline]
    pub fn shape(&self, phase: f32) -> f32 {
        self.ramp(phase)
    }

    /// Ramp value for a phase in the unit interval.
    #[inline]
    fn ramp(&self, u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        match self.shape {
            EnvelopeShape::Hard => 1.0,
            EnvelopeShape::RaisedCosine => 0.5 - 0.5 * (std::f32::consts::PI * u).cos(),
            EnvelopeShape::Gaussian => {
                let x = GAUSS_HALF_WIDTH * (2.0 * u - 1.0);
                let v = 0.5 * (1.0 + erf(x));
                ((v - self.gauss_lo) / self.gauss_span).clamp(0.0, 1.0)
            }
        }
    }

}

impl Default for Envelope {
    fn default() -> Envelope {
        Envelope::new()
    }
}

/// Error function, by the Abramowitz and Stegun rational approximation.
///
/// Five terms, with a stated worst case of one and a half parts in ten million,
/// which is four orders below what a sixteen bit output can represent. Written
/// out rather than reached for through a library because the whole of it is six
/// lines and the alternative is a dependency for one function.
fn erf(x: f32) -> f32 {
    const A1: f32 = 0.254_829_592;
    const A2: f32 = -0.284_496_736;
    const A3: f32 = 1.421_413_741;
    const A4: f32 = -1.453_152_027;
    const A5: f32 = 1.061_405_429;
    const P: f32 = 0.327_591_1;

    // The approximation is stated for a positive argument; the function is odd,
    // so the sign is taken out and put back.
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + P * x);
    let poly = t * (A1 + t * (A2 + t * (A3 + t * (A4 + t * A5))));
    sign * (1.0 - poly * (-x * x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_approximation_matches_the_known_values() {
        // Three points that pin the shape: the origin, the half way value and
        // the tail. A sign error or a transposed coefficient moves at least one.
        assert!((erf(0.0)).abs() < 1e-6);
        assert!((erf(0.476_936) - 0.5).abs() < 1e-4);
        assert!((erf(2.0) - 0.995_322).abs() < 1e-5);
        assert!((erf(-1.0) + erf(1.0)).abs() < 1e-6);
    }

    #[test]
    fn a_shaped_edge_starts_low_and_ends_high() {
        for shape in [EnvelopeShape::RaisedCosine, EnvelopeShape::Gaussian] {
            let mut env = Envelope::new();
            // Five milliseconds at forty eight thousand is two hundred and forty
            // samples, and the element is long enough that the two edges do not
            // meet.
            env.configure(shape, 5.0, 5.0, 48_000.0);
            let length = 2400;

            assert!(env.amplitude(0, length) < 0.05, "{:?} starts loud", shape);
            assert!(env.amplitude(length - 1, length) < 0.05, "{:?} ends loud", shape);
            assert!((env.amplitude(length / 2, length) - 1.0).abs() < 1e-6);

            // Monotonic through the rise. A ramp that dips would be an audible
            // ripple at the start of every element.
            let mut previous = 0.0f32;
            for i in 0..240 {
                let v = env.amplitude(i, length);
                assert!(v >= previous - 1e-6, "{:?} dips at {}", shape, i);
                previous = v;
            }
        }
    }

    #[test]
    fn a_hard_edge_is_full_amplitude_throughout() {
        let mut env = Envelope::new();
        // The stated edge lengths are refused rather than shortened: a one
        // sample ramp is not a hard edge, and full amplitude at the first sample
        // is what says the refusal happened.
        env.configure(EnvelopeShape::Hard, 5.0, 5.0, 48_000.0);
        for i in 0..100 {
            assert_eq!(env.amplitude(i, 100), 1.0);
        }
    }

    #[test]
    fn an_element_shorter_than_its_edges_stays_inside_unity() {
        // At sixty words a minute a dot is twenty milliseconds, so a five
        // millisecond pair of edges is half of it. Without the clamp the two
        // ramps would overlap and the middle would exceed full scale.
        let mut env = Envelope::new();
        env.configure(EnvelopeShape::RaisedCosine, 5.0, 5.0, 48_000.0);
        let length = 120;
        for i in 0..length {
            let v = env.amplitude(i, length);
            assert!((0.0..=1.0).contains(&v), "sample {} reached {}", i, v);
        }
    }
}