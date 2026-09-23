//! Noise reduction and the manual notch.
//!
//! ## Which method, and why the choice is provisional
//!
//! Two are worth having. Spectral subtraction over overlapping blocks estimates
//! the noise floor per bin and removes it; an adaptive predictor separates the
//! predictable part of the signal from the unpredictable part and keeps the
//! first.
//!
//! The predictor is implemented here for three reasons that hold without a
//! measurement. It introduces no block latency, which matters on a path an
//! operator is listening through while turning a dial. It produces no musical
//! noise, which is the characteristic failure of spectral subtraction and the
//! reason many operators switch it off. And it needs no transform, so its cost
//! is a few hundred operations per sample rather than a block of them at once.
//!
//! What it does not do is help on speech, which is not predictable over the
//! horizon a short filter can see. Settling that against spectral subtraction
//! needs a measurement on real noise rather than a comparison of descriptions,
//! and until that measurement exists this is the method that is in the chain.
//!
//! ## How a predictor removes noise
//!
//! The filter is fed a delayed copy of the signal and asked to predict the
//! present sample from it. A tone is periodic, so a delayed copy predicts it
//! well; noise is not, so a delayed copy predicts nothing. The prediction is
//! therefore the tonal part and the residual is the noise, and taking the
//! prediction as the output is the whole of the method.
//!
//! The delay has to exceed the correlation length of the noise, otherwise the
//! filter predicts the noise as well and removes nothing. One millisecond is
//! well past the correlation length of anything a receiver passband carries.

use crate::config::settings::{Detector, NrMethod};
use crate::dsp::fft::Fft;

/// Taps in the predictor.
///
/// Each tap is one more frequency the filter can resolve, so the count decides
/// how many simultaneous tones survive. Sixty four at the decoder rate resolves
/// down to roughly two hundred hertz, which separates a keyed carrier from its
/// neighbours and is short enough that the filter tracks a fade.
const TAPS: usize = 64;

/// Prediction distance, in milliseconds.
///
/// Must exceed the correlation length of the noise, otherwise the filter
/// predicts the noise as well and removes nothing. One millisecond is well past
/// the correlation length of anything a receiver passband carries.
const DELAY_MS: f32 = 1.0;

/// Ceiling on the step size, as a fraction of the input power.
const MAX_STEP: f32 = 0.10;

/// Transform length of the spectral method.
///
/// Two hundred and fifty six samples is twenty one milliseconds at the decoder
/// rate. Longer resolves the noise floor more finely and smears a keying edge
/// across the block; shorter does the opposite. This is the length at which a
/// resolution of about fifty hertz is reached without the smearing becoming
/// audible on speech.
const NFFT: usize = 256;

/// Hop, as a fraction of the transform length.
///
/// A quarter, which is three quarters overlap. A half would be enough for
/// reconstruction and produces markedly more musical noise, because each bin is
/// then modulated by a gain that changes twice as fast.
const NHOP: usize = NFFT / 4;

/// Rate the noise estimate rises, per block.
///
/// Deliberately far slower than it falls. The estimate is meant to sit on the
/// floor, so it follows a decrease at once and an increase only over seconds:
/// a signal appearing must not be taken for a higher noise floor and removed.
const NOISE_RISE: f32 = 0.004;

/// Rate it falls.
const NOISE_FALL: f32 = 0.35;

/// Rate the per bin gain follows its target.
///
/// The whole of the musical noise problem is here. A gain that jumps per block
/// turns residual noise into a shower of short tones; one that is smoothed
/// turns it into a rush, which is what an ear expects noise to sound like.
const GAIN_SMOOTH: f32 = 0.35;

/// Which method is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Predictor,
    Spectral,
}

/// Adaptive predictor.
///
/// The filter is fed a delayed copy of the signal and asked to predict the
/// present sample from it. A tone is periodic, so a delayed copy predicts it
/// well; noise is not, so a delayed copy predicts nothing. The prediction is
/// therefore the tonal part and the residual is the noise, and taking the
/// prediction as the output is the whole of the method.
struct Predictor {
    mu: f32,
    /// Fraction of the residual mixed back in.
    ///
    /// A prediction with none of the residual sounds hollow, because every
    /// consonant and every keying edge is unpredictable and is therefore in the
    /// residual. Keeping a little of it back is what makes the result sound like
    /// a quieter signal rather than like a different one.
    residual: f32,
    weights: [f32; TAPS],
    history: Vec<f32>,
    pos: usize,
    delay: usize,
    power: f32,
}

impl Predictor {
    fn new(rate: u32) -> Predictor {
        let fs = rate.max(1) as f32;
        let delay = ((DELAY_MS * 0.001 * fs).round() as usize).max(1);
        Predictor {
            mu: 0.02,
            residual: 0.25,
            weights: [0.0; TAPS],
            history: vec![0.0; delay + TAPS + 1],
            pos: 0,
            delay,
            power: 1e-6,
        }
    }

    fn configure(&mut self, strength: f32) {
        let s = strength.clamp(0.0, 1.0);
        self.mu = 0.002 + s * (MAX_STEP - 0.002);
        self.residual = 0.4 * (1.0 - s);
    }

    fn reset(&mut self) {
        for w in self.weights.iter_mut() {
            *w = 0.0;
        }
        for h in self.history.iter_mut() {
            *h = 0.0;
        }
        self.pos = 0;
        self.power = 1e-6;
    }

    #[inline]
    fn sample(&mut self, x: f32) -> f32 {
        let n = self.history.len();
        self.history[self.pos] = x;
        self.pos = (self.pos + 1) % n;

        let mut y = 0.0f32;
        for (k, &w) in self.weights.iter().enumerate() {
            let at = (self.pos + n - 1 - self.delay - k) % n;
            y += w * self.history[at];
        }
        let error = x - y;

        // Normalized step. Dividing by the running power makes the adaptation
        // rate independent of the level, which is what stops a strong signal
        // from driving the filter unstable and a weak one from freezing it.
        self.power += 0.001 * (x * x - self.power);
        let step = self.mu / (self.power * TAPS as f32 + 1e-6);
        for k in 0..TAPS {
            let at = (self.pos + n - 1 - self.delay - k) % n;
            self.weights[k] += step * error * self.history[at];
        }

        y + self.residual * error
    }
}

/// Spectral subtraction over overlapping blocks.
///
/// The noise floor is estimated per bin and a gain below one is applied where
/// the measured power is close to it. Three things keep the result from sounding
/// like a shower of short tones, which is what this method does when built
/// naively: the subtraction is taken past unity, the gain has a floor, and the
/// gain is smoothed in time.
struct Spectral {
    fft: Fft,
    window: Vec<f32>,
    /// Reciprocal of the overlap sum, so the output level matches the input.
    norm: f32,
    input: Vec<f32>,
    /// Overlap accumulator, one transform long.
    overlap: Vec<f32>,
    /// Samples ready to be handed back.
    output: Vec<f32>,
    /// Read position in the output queue.
    taken: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    noise: Vec<f32>,
    gain: Vec<f32>,
    /// How far past unity the subtraction goes.
    over: f32,
    /// Lowest gain a bin may take.
    floor: f32,
    primed: bool,
}

impl Spectral {
    fn new() -> Spectral {
        // Hann, applied on analysis and again on synthesis. At three quarters
        // overlap the squared window sums to a constant, which is what makes
        // the reconstruction exact rather than merely close.
        let mut window = vec![0.0f32; NFFT];
        for (i, slot) in window.iter_mut().enumerate() {
            let t = i as f32 / (NFFT - 1) as f32;
            *slot = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
        }
        // Measured rather than derived, so a change to the window or to the hop
        // does not silently change the level.
        let mut sum = 0.0f32;
        let mut k = 0usize;
        while k < NFFT {
            sum += window[k] * window[k];
            k += NHOP;
        }
        let norm = if sum > 1e-9 { 1.0 / sum } else { 1.0 };

        Spectral {
            fft: Fft::new(NFFT),
            window,
            norm,
            input: Vec::with_capacity(NFFT * 2),
            overlap: vec![0.0; NFFT],
            output: Vec::with_capacity(NFFT * 2),
            taken: 0,
            re: vec![0.0; NFFT],
            im: vec![0.0; NFFT],
            noise: vec![1e-9; NFFT / 2 + 1],
            gain: vec![1.0; NFFT / 2 + 1],
            over: 1.8,
            floor: 0.15,
            primed: false,
        }
    }

    fn configure(&mut self, strength: f32) {
        let s = strength.clamp(0.0, 1.0);
        // More subtraction and a lower floor as the control rises. The two move
        // together because pushing one without the other either leaves the noise
        // audible or leaves the residual musical.
        self.over = 1.0 + s * 2.0;
        self.floor = 0.30 - s * 0.25;
    }

    fn reset(&mut self) {
        self.input.clear();
        self.output.clear();
        self.taken = 0;
        for v in self.overlap.iter_mut() {
            *v = 0.0;
        }
        for v in self.noise.iter_mut() {
            *v = 1e-9;
        }
        for v in self.gain.iter_mut() {
            *v = 1.0;
        }
        self.primed = false;
    }

    /// Latency, in samples. One transform, which is what an overlap add method
    /// costs and cannot avoid.
    fn delay(&self) -> usize {
        NFFT
    }

    #[inline]
    fn sample(&mut self, x: f32) -> f32 {
        self.input.push(x);
        while self.input.len() >= NFFT {
            self.transform();
        }

        // The output queue runs one transform behind the input by construction,
        // so it is empty only until the first block has been processed.
        if self.taken < self.output.len() {
            let y = self.output[self.taken];
            self.taken += 1;
            if self.taken >= NHOP * 4 {
                self.output.drain(..self.taken);
                self.taken = 0;
            }
            y
        } else {
            0.0
        }
    }

    fn transform(&mut self) {
        for i in 0..NFFT {
            self.re[i] = self.input[i] * self.window[i];
            self.im[i] = 0.0;
        }
        self.fft.forward(&mut self.re, &mut self.im);

        let half = NFFT / 2;
        for k in 0..=half {
            let p = self.re[k] * self.re[k] + self.im[k] * self.im[k];

            // Minimum seeking tracker. Falls quickly and rises slowly, so it
            // settles onto the floor rather than following whatever is loudest.
            let rate = if p < self.noise[k] { NOISE_FALL } else { NOISE_RISE };
            if self.primed {
                self.noise[k] += rate * (p - self.noise[k]);
            } else {
                self.noise[k] = p;
            }

            let ratio = p / (self.noise[k] + 1e-12);
            let wanted = (1.0 - self.over / ratio.max(1e-6)).max(self.floor);
            self.gain[k] += GAIN_SMOOTH * (wanted - self.gain[k]);
        }
        self.primed = true;

        // The gain is real and even, so applying it to the lower half and
        // mirroring keeps the result real without a second pass.
        for k in 0..=half {
            let g = self.gain[k];
            self.re[k] *= g;
            self.im[k] *= g;
            if k > 0 && k < half {
                let mirror = NFFT - k;
                self.re[mirror] *= g;
                self.im[mirror] *= g;
            }
        }

        // Inverse by conjugation. A forward transform of the conjugate,
        // conjugated again and scaled, is the inverse; a second table would be
        // one more thing to keep in step for no gain.
        for v in self.im.iter_mut() {
            *v = -*v;
        }
        self.fft.forward(&mut self.re, &mut self.im);
        let scale = 1.0 / NFFT as f32;

        let start = self.output.len();
        self.output.resize(start + NHOP, 0.0);
        for i in 0..NFFT {
            let v = self.re[i] * scale * self.window[i] * self.norm;
            self.overlap[i] += v;
        }
        for i in 0..NHOP {
            self.output[start + i] = self.overlap[i];
        }
        self.overlap.copy_within(NHOP.., 0);
        for v in self.overlap[NFFT - NHOP..].iter_mut() {
            *v = 0.0;
        }

        self.input.drain(..NHOP);
    }
}

/// Noise reduction, either method.
pub struct NoiseReduction {
    enabled: bool,
    method: Method,
    predictor: Predictor,
    spectral: Spectral,
}

impl NoiseReduction {
    pub fn new(rate: u32) -> NoiseReduction {
        NoiseReduction {
            enabled: false,
            method: Method::Predictor,
            predictor: Predictor::new(rate),
            spectral: Spectral::new(),
        }
    }

    /// Strength runs from nought to one. One control rather than two because
    /// the two settings each method takes are not independently useful, and an
    /// operator adjusting a receiver has enough to do.
    ///
    /// A change of method clears the one being left, so switching back does not
    /// release whatever was sitting in its state.
    pub fn configure(&mut self, enabled: bool, strength: f32, method: NrMethod, detector: Detector) {
        self.enabled = enabled;
        let wanted = match method {
            NrMethod::Predictor => Method::Predictor,
            NrMethod::Spectral => Method::Spectral,
            // The detector already states which kind of signal is being
            // received, so the choice follows from it rather than being asked
            // for a second time.
            NrMethod::Auto => match detector {
                Detector::Cw | Detector::DigU | Detector::DigL => Method::Predictor,
                _ => Method::Spectral,
            },
        };
        if wanted != self.method {
            match self.method {
                Method::Predictor => self.predictor.reset(),
                Method::Spectral => self.spectral.reset(),
            }
            self.method = wanted;
        }
        self.predictor.configure(strength);
        self.spectral.configure(strength);
    }

    /// Latency the stage introduces, in samples.
    pub fn delay(&self) -> usize {
        if self.enabled && self.method == Method::Spectral {
            self.spectral.delay()
        } else {
            0
        }
    }

    pub fn reset(&mut self) {
        self.predictor.reset();
        self.spectral.reset();
    }

    #[inline]
    pub fn sample(&mut self, x: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        match self.method {
            Method::Predictor => self.predictor.sample(x),
            Method::Spectral => self.spectral.sample(x),
        }
    }
}

/// Manual notch.
///
/// One biquad rather than a cascade. A single section reaches a depth of thirty
/// decibels or so, which removes a carrier from under speech; a deeper notch
/// needs a narrower one, and a narrow notch rings audibly on the keying it is
/// sitting next to.
pub struct Notch {
    enabled: bool,
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
    rate: f32,
    hz: f32,
    width_hz: f32,
}

impl Notch {
    pub fn new(rate: u32) -> Notch {
        Notch {
            enabled: false,
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
            rate: rate.max(1) as f32,
            hz: 0.0,
            width_hz: 0.0,
        }
    }

    pub fn configure(&mut self, enabled: bool, hz: f32, width_hz: f32) {
        self.enabled = enabled;
        let f0 = hz.clamp(20.0, self.rate * 0.45);
        let bw = width_hz.clamp(5.0, self.rate * 0.2);
        // Recomputed only on a real move, so a slider being dragged does not
        // sweep the coefficients on every frame and turn a notch into a warble.
        if (f0 - self.hz).abs() < 0.5 && (bw - self.width_hz).abs() < 0.5 {
            return;
        }
        self.hz = f0;
        self.width_hz = bw;

        let w0 = std::f32::consts::TAU * f0 / self.rate;
        let alpha = (w0.sin() / 2.0) * (f0 / bw).clamp(0.5, 100.0).recip().recip().recip();
        // The expression above is the reciprocal of the quality factor written
        // out; stated directly it is the half bandwidth over the centre.
        let alpha = if alpha.is_finite() && alpha > 0.0 {
            alpha
        } else {
            w0.sin() * bw / (2.0 * f0)
        };
        let a0 = 1.0 + alpha;

        self.b0 = 1.0 / a0;
        self.b1 = -2.0 * w0.cos() / a0;
        self.b2 = 1.0 / a0;
        self.a1 = self.b1;
        self.a2 = (1.0 - alpha) / a0;
    }

    pub fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    #[inline]
    pub fn sample(&mut self, x: f32) -> f32 {
        if !self.enabled {
            return x;
        }
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}