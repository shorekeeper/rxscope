//! Impulse blankers.
//!
//! Two of them, on two time scales, and the reason is that an impulse changes
//! shape as it travels through the chain.
//!
//! Ahead of the filter an impulse is still an impulse: a few samples reaching
//! every part of the spectrum at once, far above the surrounding level. It is
//! cheapest to remove there, because a short window covers the whole of it.
//!
//! What survives the filter is the impulse response of that filter, which is as
//! long as the filter is and no longer looks like an impulse at all. A detector
//! tuned for the first case sees nothing; one tuned for the second would blank
//! a substantial part of every real signal if it were applied first. Hence two,
//! with different windows and different baselines.
//!
//! ## Why a delay line
//!
//! An impulse is recognized from the sample that exceeds the threshold, but by
//! then its leading edge is already past. Blanking from the detection point
//! forward leaves the front of the click audible, which is most of what makes a
//! click objectionable. The signal is therefore delayed by half the blanking
//! window and the window is centred on the detection, so the whole event is
//! covered.
//!
//! ## Why the hole is filled rather than zeroed
//!
//! Zeroing a stretch of a continuous signal substitutes one discontinuity for
//! another, and on a narrow filter the substitute rings just as the original
//! did. The blanked samples are replaced by the last value before the event
//! instead, which for a window of a few samples is a good enough continuation
//! and introduces no new edge.

use super::iq::Complex;

/// Longest window either blanker will hold, in samples.
const MAX_WINDOW: usize = 64;

/// Rate the baseline follows the signal, per sample, at the decoder rate.
///
/// The baseline has to sit above the noise and below an impulse, so it must be
/// slow enough that a click does not lift it and fast enough that a fade does
/// not leave it stranded. A time constant of a tenth of a second is two orders
/// of magnitude above a click and two below a fade.
const BASELINE_TAU_S: f32 = 0.1;

pub struct Blanker {
    enabled: bool,
    /// Multiple of the baseline a sample must exceed.
    threshold: f32,
    /// Samples blanked around a detection.
    window: usize,

    baseline: f32,
    alpha: f32,

    /// Delayed signal, so the window can be centred on the event.
    delay: Vec<Complex>,
    pos: usize,
    /// Samples still to be replaced.
    remaining: usize,
    /// Value the hole is filled with.
    held: Complex,

    /// Events acted on, for a readout that answers whether the setting is doing
    /// anything at all.
    events: u64,
}

impl Blanker {
    /// Builds a blanker with a window stated in milliseconds.
    ///
    /// The wide one wants a window of a fraction of a millisecond, which is the
    /// duration of the impulse itself. The narrow one wants a window of a few
    /// milliseconds, which is the duration of what the filter turned it into.
    pub fn new(rate: u32, window_ms: f32) -> Blanker {
        let fs = rate.max(1) as f32;
        let window = ((window_ms * 0.001 * fs).round() as usize).clamp(3, MAX_WINDOW);
        // Odd, so the delay is a whole number of samples and the window centres
        // exactly on the detection rather than half a sample off.
        let window = if window % 2 == 0 { window + 1 } else { window };

        Blanker {
            enabled: false,
            threshold: 8.0,
            window,
            baseline: 0.0,
            alpha: (1.0 - (-1.0 / (BASELINE_TAU_S * fs)).exp()).clamp(1e-6, 1.0),
            delay: vec![Complex::default(); window],
            pos: 0,
            remaining: 0,
            held: Complex::default(),
            events: 0,
        }
    }

    pub fn configure(&mut self, enabled: bool, threshold: f32) {
        self.enabled = enabled;
        self.threshold = threshold.clamp(1.5, 40.0);
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    /// Delay the stage introduces, in samples. Nought while it is off, because
    /// a stage that does nothing must not cost latency either.
    pub fn delay(&self) -> usize {
        if self.enabled {
            self.window / 2
        } else {
            0
        }
    }

    pub fn reset(&mut self) {
        for v in self.delay.iter_mut() {
            *v = Complex::default();
        }
        self.pos = 0;
        self.remaining = 0;
        self.baseline = 0.0;
        self.held = Complex::default();
    }

    #[inline]
    pub fn sample(&mut self, z: Complex) -> Complex {
        if !self.enabled {
            return z;
        }

        let level = z.magnitude();
        let hit = self.baseline > 1e-9 && level > self.baseline * self.threshold;

        // The baseline is only updated on a sample that is not part of an event.
        // Letting an impulse into the average is what makes a blanker stop
        // working under heavy interference: the baseline rises to meet the
        // clicks and the threshold is never crossed again.
        if !hit {
            self.baseline += self.alpha * (level - self.baseline);
        }

        let out = self.delay[self.pos];
        self.delay[self.pos] = z;
        self.pos = (self.pos + 1) % self.window;

        if hit {
            if self.remaining == 0 {
                self.events += 1;
                // The value held is the one about to leave the delay line, which
                // is the last sample before the event reached the input.
                self.held = out;
            }
            self.remaining = self.window;
        }

        if self.remaining > 0 {
            self.remaining -= 1;
            return self.held;
        }
        out
    }
}