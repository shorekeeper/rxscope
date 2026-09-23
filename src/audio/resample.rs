//! Arbitrary ratio resampler.
//!
//! The device runs at 44100 or 48000 and the decoders want something like
//! 12000, so a rate conversion is unavoidable. The method is a windowed sinc
//! kernel sampled at a fixed number of fractional phases, with linear
//! interpolation between the two nearest phases. That is accurate enough that
//! the residual error sits far below the noise floor of any receiver, and it
//! handles ratios that are not simple integers, such as 44100 to 12000.
//!
//! The kernel length scales with the inverse of the cutoff, so a heavy
//! decimation gets a correspondingly steeper filter. Aliasing matters here:
//! energy folded down from above the new Nyquist would land inside the decoder
//! passband and no later filter could remove it.

/// Fractional phases stored in the table. Between two phases the coefficients
/// are interpolated, so the effective resolution is much finer than this.
const PHASES: usize = 64;

/// Zero crossings on each side of the kernel centre before the cutoff scaling.
const ZERO_CROSSINGS: usize = 16;

/// Guard factor on the cutoff. Nine tenths of the new Nyquist leaves room for
/// the transition band without eating into the audio range that matters.
const CUTOFF_GUARD: f64 = 0.9;

pub struct Resampler {
    /// Input samples consumed per output sample.
    step: f64,
    /// Step the rates alone imply. Held apart from the working step so a drift
    /// correction is expressed against the nominal ratio rather than compounding
    /// on top of the previous correction.
    base_step: f64,
    /// Fractional read position inside the working buffer.
    pos: f64,
    buf: Vec<f32>,
    taps: usize,
    half: usize,
    /// Row major, PHASES plus one rows of taps coefficients. The extra row is
    /// the phase at exactly one, needed by the interpolation.
    coef: Vec<f32>,
    passthrough: bool,
    in_rate: u32,
    out_rate: u32,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Resampler {
        if in_rate == 0 || out_rate == 0 || in_rate == out_rate {
            return Resampler {
                step: 1.0,
                base_step: 1.0,
                pos: 0.0,
                buf: Vec::new(),
                taps: 0,
                half: 0,
                coef: Vec::new(),
                passthrough: true,
                in_rate,
                out_rate,
            };
        }

        let ratio = out_rate as f64 / in_rate as f64;
        // Cutoff is expressed as a fraction of the input Nyquist. Upsampling
        // needs no extra band limiting beyond the input itself.
        let cutoff = (CUTOFF_GUARD * ratio.min(1.0)).clamp(0.01, 0.95);

        let half = ((ZERO_CROSSINGS as f64 / cutoff).ceil() as usize).clamp(4, 192);
        let taps = half * 2;

        let mut coef = vec![0.0f32; (PHASES + 1) * taps];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = p * taps;
            let mut sum = 0.0f64;
            for k in 0..taps {
                // Distance from the kernel centre in input samples for tap k
                // when the read position sits at frac past an input sample.
                let t = (k as f64 - half as f64 + 1.0) - frac;
                let w = blackman(k, taps);
                let value = w * cutoff * sinc(cutoff * t);
                coef[row + k] = value as f32;
                sum += value;
            }
            // Each phase is normalized to unity gain at direct current, which
            // removes the ripple a truncated kernel would otherwise leave in
            // the passband amplitude.
            if sum.abs() > 1e-12 {
                let scale = (1.0 / sum) as f32;
                for k in 0..taps {
                    coef[row + k] *= scale;
                }
            }
        }

        crate::log_info!(
            "audio",
            "resampler {} -> {} Hz, {} taps, cutoff {:.3}",
            in_rate,
            out_rate,
            taps,
            cutoff
        );

        Resampler {
            step: in_rate as f64 / out_rate as f64,
            base_step: in_rate as f64 / out_rate as f64,
            // The first output needs half minus one samples of history, so the
            // read position starts there instead of at zero.
            pos: (half - 1) as f64,
            buf: Vec::with_capacity(taps * 4),
            taps,
            half,
            coef,
            passthrough: false,
            in_rate,
            out_rate,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.passthrough
    }

    pub fn rates(&self) -> (u32, u32) {
        (self.in_rate, self.out_rate)
    }

    /// Consumes input and appends whatever output it produced. Samples that do
    /// not yet have enough context are held until the next call.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        if input.is_empty() && self.buf.is_empty() {
            return;
        }
        self.buf.extend_from_slice(input);

        let taps = self.taps;
        let half = self.half;

        loop {
            let base = self.pos.floor();
            let bi = base as usize;
            // The kernel spans bi + 1 - half up to bi + half inclusive.
            if bi + half + 1 > self.buf.len() {
                break;
            }
            let frac = self.pos - base;
            let phase = frac * PHASES as f64;
            let p0 = phase.floor() as usize;
            let alpha = (phase - p0 as f64) as f32;
            let r0 = p0 * taps;
            let r1 = r0 + taps;
            let start = bi + 1 - half;

            let mut acc = 0.0f32;
            for k in 0..taps {
                let c = self.coef[r0 + k] * (1.0 - alpha) + self.coef[r1 + k] * alpha;
                acc += self.buf[start + k] * c;
            }
            out.push(acc);
            self.pos += self.step;
        }

        // Drop everything the kernel can no longer reach and rebase the
        // position, which keeps the working buffer bounded regardless of how
        // long the stream runs.
        let bi = self.pos.floor() as usize;
        let keep = (bi + 1).saturating_sub(half);
        if keep > 0 {
            self.buf.drain(..keep);
            self.pos -= keep as f64;
        }
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.pos = if self.passthrough { 0.0 } else { (self.half - 1) as f64 };
    }

    /// Trims the conversion ratio by a few parts per million.
    ///
    /// Two devices run on two crystals, and nothing keeps them in step. A
    /// hundred parts per million is ordinary, which is six samples per second
    /// at forty eight kilohertz: a buffer between them either fills or empties
    /// within minutes, and the resulting overrun is an audible click every time.
    ///
    /// Correcting the ratio removes the accumulation instead of periodically
    /// discarding it. The bound is well below what the ear resolves as a pitch
    /// change, and far above any real crystal error, so a correction that
    /// reaches it means the two rates were never what they claimed to be.
    pub fn set_drift(&mut self, ppm: f32) {
        if self.passthrough {
            return;
        }
        let trim = ppm.clamp(-1000.0, 1000.0) as f64 * 1e-6;
        self.step = self.base_step * (1.0 + trim);
    }
}

/// Normalized sinc, sin of pi x over pi x, with the removable singularity at
/// zero filled in.
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        return 1.0;
    }
    let pix = std::f64::consts::PI * x;
    pix.sin() / pix
}

/// Blackman window. Its sidelobes fall off fast enough that the stopband of
/// the resampling filter stays below roughly ninety decibels.
fn blackman(k: usize, n: usize) -> f64 {
    if n < 2 {
        return 1.0;
    }
    let t = k as f64 / (n - 1) as f64;
    let two_pi = std::f64::consts::TAU;
    0.42 - 0.5 * (two_pi * t).cos() + 0.08 * (2.0 * two_pi * t).cos()
}