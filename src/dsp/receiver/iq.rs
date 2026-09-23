//! Complex signal construction.
//!
//! Everything downstream works on a complex baseband, because that is the only
//! representation in which the two halves of a spectrum are distinguishable. A
//! real signal is not: its spectrum is symmetric about nought, so a tone at plus
//! one kilohertz and one at minus one kilohertz are the same samples and no
//! filter can separate them.
//!
//! Two inputs produce that representation and they are not equivalent.
//!
//! A receiver that delivers two channels is already complex: the second channel
//! is the quadrature component and the spectrum genuinely has two halves. The
//! width the display can show doubles, and a signal on the wrong side is
//! suppressed rather than folded onto its mirror.
//!
//! A single channel carries no such information, so the quadrature component is
//! manufactured by a Hilbert transform. That yields the analytic signal, whose
//! spectrum is the positive half of the original with nothing below nought. It
//! is a valid complex signal and it is not the same thing: nothing was
//! separated, because there was nothing to separate. What it buys is a uniform
//! representation for the chain that follows.
//!
//! ## Why imbalance correction is not optional
//!
//! Image rejection is decided entirely by how well the two paths match, and no
//! hardware matches them exactly. A gain difference of one tenth of a decibel
//! with a phase error of one degree already limits rejection to around forty
//! decibels, which on a busy band puts a readable ghost of every strong station
//! on the opposite side. The correction costs two multiplies per sample and is
//! the difference between a two sided spectrum and a two sided spectrum with a
//! false copy of everything in it.
//!
//! The correction is applied to the quadrature channel alone. Correcting both
//! would be one degree of freedom too many: only the ratio between the paths is
//! observable, and an absolute scale belongs to the gain control.

/// Taps in the Hilbert transformer.
///
/// Odd, so the group delay is a whole number of samples and the in phase path
/// can be delayed by an integer rather than filtered. Sixty three taps at the
/// decoder rate give a passband from roughly two hundred hertz to the Nyquist
/// less the same, which covers everything a receiver passes; a shorter filter
/// eats into the low end, where a keyed signal at a low pitch lives.
const HILBERT_TAPS: usize = 63;

/// Complex sample. A pair rather than a named type, because nothing here needs
/// arithmetic operators and a plain tuple would lose the field names.
#[derive(Debug, Clone, Copy, Default)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    pub fn new(re: f32, im: f32) -> Complex {
        Complex { re, im }
    }

    pub fn magnitude(self) -> f32 {
        (self.re * self.re + self.im * self.im).sqrt()
    }

    pub fn power(self) -> f32 {
        self.re * self.re + self.im * self.im
    }

    /// Product with another complex value.
    pub fn mul(self, other: Complex) -> Complex {
        Complex {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }

    /// Product with the conjugate of another value, which is the operation a
    /// phase comparison is built from.
    pub fn mul_conj(self, other: Complex) -> Complex {
        Complex {
            re: self.re * other.re + self.im * other.im,
            im: self.im * other.re - self.re * other.im,
        }
    }

    pub fn scale(self, k: f32) -> Complex {
        Complex { re: self.re * k, im: self.im * k }
    }
}

/// Correction applied between the two channels.
#[derive(Debug, Clone, Copy)]
pub struct Imbalance {
    /// Amplitude ratio the quadrature channel is divided by.
    gain: f32,
    /// Sine and cosine of the phase error, precomputed because they are used
    /// once per sample and the error moves only when the operator turns a knob.
    sin_phi: f32,
    cos_phi: f32,
}

impl Imbalance {
    pub fn new(gain_db: f32, phase_deg: f32) -> Imbalance {
        let gain = 10.0f32.powf(gain_db.clamp(-12.0, 12.0) / 20.0);
        let phi = phase_deg.clamp(-45.0, 45.0).to_radians();
        Imbalance { gain, sin_phi: phi.sin(), cos_phi: phi.cos().max(1e-3) }
    }

    /// Applies the correction to one sample pair.
    ///
    /// The in phase channel is the reference and is left alone. The quadrature
    /// channel is scaled and then had its projection onto the reference removed,
    /// which is the inverse of the usual model where the two paths differ by a
    /// gain and a phase.
    #[inline]
    fn apply(&self, i: f32, q: f32) -> Complex {
        let q = q / self.gain;
        Complex { re: i, im: (q - i * self.sin_phi) / self.cos_phi }
    }
}

impl Default for Imbalance {
    fn default() -> Imbalance {
        Imbalance { gain: 1.0, sin_phi: 0.0, cos_phi: 1.0 }
    }
}

/// Turns the capture stream into a complex baseband.
pub struct IqFront {
    /// True when the input carries two channels.
    complex_input: bool,
    swap: bool,
    imbalance: Imbalance,

    /// Hilbert coefficients, only the odd taps are nonzero.
    taps: Vec<f32>,
    /// Delay line for the single channel path.
    history: Vec<f32>,
    /// Write position in the ring.
    pos: usize,
}

impl IqFront {
    pub fn new() -> IqFront {
        let mut taps = vec![0.0f32; HILBERT_TAPS];
        let centre = (HILBERT_TAPS / 2) as isize;

        // The ideal Hilbert response is two over pi n for odd n and nought for
        // even n. Truncating it outright would ripple by nine percent across the
        // passband; a Blackman window brings that under a tenth of a decibel at
        // the cost of a slightly narrower usable range.
        for k in 0..HILBERT_TAPS {
            let n = k as isize - centre;
            if n == 0 || n % 2 == 0 {
                continue;
            }
            let ideal = 2.0 / (std::f64::consts::PI * n as f64);
            let t = k as f64 / (HILBERT_TAPS - 1) as f64;
            let two_pi = std::f64::consts::TAU;
            let window = 0.42 - 0.5 * (two_pi * t).cos() + 0.08 * (2.0 * two_pi * t).cos();
            taps[k] = (ideal * window) as f32;
        }

        IqFront {
            complex_input: false,
            swap: false,
            imbalance: Imbalance::default(),
            taps,
            history: vec![0.0; HILBERT_TAPS],
            pos: 0,
        }
    }

    /// Group delay of the single channel path, in samples.
    ///
    /// Reported because everything measured against the input has to account for
    /// it: a blanker that detects an impulse before this stage and blanks after
    /// it would blank the wrong place by half the filter length.
    pub fn delay(&self) -> usize {
        if self.complex_input {
            0
        } else {
            HILBERT_TAPS / 2
        }
    }

    pub fn configure(&mut self, complex_input: bool, swap: bool, gain_db: f32, phase_deg: f32) {
        if complex_input != self.complex_input {
            self.complex_input = complex_input;
            self.reset();
        }
        self.swap = swap;
        self.imbalance = Imbalance::new(gain_db, phase_deg);
    }

    pub fn is_complex(&self) -> bool {
        self.complex_input
    }

    pub fn reset(&mut self) {
        for v in self.history.iter_mut() {
            *v = 0.0;
        }
        self.pos = 0;
    }

    /// Converts one sample pair. The second value is ignored on a single
    /// channel input, which is what lets the caller hand over the same buffer
    /// whichever mode is in force.
    #[inline]
    pub fn sample(&mut self, i: f32, q: f32) -> Complex {
        if self.complex_input {
            let (i, q) = if self.swap { (q, i) } else { (i, q) };
            return self.imbalance.apply(i, q);
        }

        self.history[self.pos] = i;
        self.pos = (self.pos + 1) % HILBERT_TAPS;

        // The quadrature component is the filtered signal; the in phase one is
        // the input delayed to match. Only the taps at an odd distance from the
        // centre carry weight, so the loop steps by two and the cost is half the
        // tap count.
        //
        // Which parity of index that is follows from where the centre sits, and
        // it is not the parity of the distance. The centre is at thirty one for
        // this length, so an odd distance is an even index; starting at one
        // visits exactly the taps that hold nothing, and the transformer then
        // returns a quadrature component of nought for every input.
        //
        // Derived rather than written out, because the coefficients and the
        // evaluation are computed in two places and have to agree: a change of
        // length reverses which parity is meant.
        let mut im = 0.0f32;
        let mut k = (HILBERT_TAPS / 2 + 1) % 2;
        while k < HILBERT_TAPS {
            let at = (self.pos + HILBERT_TAPS - 1 - k) % HILBERT_TAPS;
            im += self.taps[k] * self.history[at];
            k += 2;
        }
        let centre = HILBERT_TAPS / 2;
        let at = (self.pos + HILBERT_TAPS - 1 - centre) % HILBERT_TAPS;
        Complex { re: self.history[at], im }
    }
}

impl Default for IqFront {
    fn default() -> IqFront {
        IqFront::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The analytic signal of a real tone has constant magnitude, which is the
    /// one property that says the transformer works: a Hilbert pair in
    /// quadrature traces a circle, an imperfect one traces an ellipse.
    #[test]
    fn a_real_tone_becomes_a_constant_envelope() {
        let mut front = IqFront::new();
        front.configure(false, false, 0.0, 0.0);

        let rate = 12_000.0f32;
        let hz = 1000.0f32;
        let mut worst = 0.0f32;
        let mut quadrature = 0.0f32;
        // The first samples run on an empty delay line and mean nothing.
        for n in 0..4096 {
            let t = n as f32 / rate;
            let z = front.sample((std::f32::consts::TAU * hz * t).sin(), 0.0);
            if n > 512 {
                worst = worst.max((z.magnitude() - 1.0).abs());
                quadrature = quadrature.max(z.im.abs());
            }
        }
        // Tested before the ripple, because it is the one failure here that is
        // not a matter of degree. A quadrature component that never leaves
        // nought means the evaluation and the coefficients disagree about which
        // taps carry weight, and the magnitude then follows the input rather
        // than its envelope. The ripple reads as exactly one in that case, which
        // is a number that says nothing about the cause.
        assert!(quadrature > 0.5, "the quadrature component is dead");
        assert!(worst < 0.02, "envelope ripple {:.4}", worst);
    }

    #[test]
    fn the_correction_undoes_a_stated_imbalance() {
        // A quadrature channel a decibel low and three degrees out is what the
        // correction exists for; after it the envelope has to be flat again.
        let mut front = IqFront::new();
        front.configure(true, false, 1.0, 3.0);

        let rate = 12_000.0f32;
        let hz = 800.0f32;
        let gain = 10.0f32.powf(1.0 / 20.0);
        let phi = 3.0f32.to_radians();

        let mut worst = 0.0f32;
        for n in 0..2048 {
            let t = std::f32::consts::TAU * hz * n as f32 / rate;
            // The impairment the correction models: the quadrature path is
            // scaled and rotated relative to the reference.
            let i = t.cos();
            let q = gain * (t + phi).sin();
            let z = front.sample(i, q);
            worst = worst.max((z.magnitude() - 1.0).abs());
        }
        assert!(worst < 1e-3, "residual imbalance {:.5}", worst);
    }
}