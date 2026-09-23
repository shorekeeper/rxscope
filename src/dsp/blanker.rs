//! Impulse blanker for the display and the decoders.
//!
//! ## Why one lives here as well as in the receiver
//!
//! The receiver chain has two blankers and neither of them helps here. That
//! chain runs on the monitor thread and serves the ear; the samples the display
//! and the keying detectors see never pass through it. So an ignition impulse
//! reaches the transform, spreads across every bin, and paints a full width
//! stripe across the waterfall on the frame it arrived.
//!
//! Which is exactly why it has to be removed before the transform. Afterwards
//! it is no longer an impulse in any useful sense: it is a line across the whole
//! spectrum, and nothing downstream can tell it from a hundred stations coming
//! on at once.
//!
//! ## Why this does not eat the keying it is supposed to preserve
//!
//! The one thing the skimmer path must not lose is the keying envelope, and a
//! blanker is a device for removing sudden rises. A keying edge is shaped by the
//! transmitter and rises over milliseconds to the level of the signal; the
//! threshold here is a multiple of the running level of the whole passband, so at
//! the default it sits some fifteen decibels above anything a keying edge
//! reaches. An impulse overshoots by far more, because it carries the energy of
//! the whole spectrum into a handful of samples.
//!
//! The trade is in the threshold and it is stated as a setting for that reason.
//! Lowered far enough this will start clipping keying edges, and the symptom is
//! a decoder that reports elements shorter than they are.
//!
//! ## Why a delay line
//!
//! An impulse is recognized from the sample that crosses the threshold, and by
//! then its leading edge has already gone past. Blanking forward from the
//! detection leaves the front of the click in the signal, which is most of what
//! makes a click visible on a spectrum. The signal is therefore delayed by half
//! the window and the window is centred on the detection.
//!
//! The hole is filled with the last value before the event rather than with
//! nought. Zeroing a stretch of a continuous signal substitutes one
//! discontinuity for another, and the transform cannot tell the two apart.

use crate::audio::convert::Converter;
use crate::config::settings::ChannelMode;

/// Longest window the blanker will hold, in samples.
const MAX_WINDOW: usize = 64;

/// Duration of the blanking window, in milliseconds.
///
/// Under a millisecond, because that is the duration of the event: an impulse
/// that has not met a filter yet occupies a handful of samples. Longer would
/// start removing signal either side of it for no reason.
const WINDOW_MS: f32 = 0.6;

/// Time constant of the level the threshold is measured against, in seconds.
///
/// A tenth of a second is two orders above the length of a click and two below
/// the length of a fade, which is the whole requirement: the level must not be
/// lifted by the impulse and must not be left behind by propagation.
const BASELINE_S: f32 = 0.1;

pub struct Blanker {
    enabled: bool,
    /// Multiple of the running level a sample must exceed.
    threshold: f32,
    /// How the pair is reduced to the one signal that is measured.
    mode: ChannelMode,
    rate: u32,

    window: usize,
    baseline: f32,
    alpha: f32,

    /// Delayed signal, so the window can be centred on the detection.
    delay: Vec<[f32; 2]>,
    pos: usize,
    /// Samples still to be replaced.
    remaining: usize,
    /// Value the hole is filled with.
    held: [f32; 2],

    /// Impulses acted on, for a readout that answers whether the setting is
    /// doing anything at all.
    events: u64,
}

impl Blanker {
    pub fn new(rate: u32) -> Blanker {
        let mut blanker = Blanker {
            enabled: false,
            threshold: 6.0,
            mode: ChannelMode::Left,
            rate: 0,
            window: 3,
            baseline: 0.0,
            alpha: 1.0,
            delay: Vec::new(),
            pos: 0,
            remaining: 0,
            held: [0.0, 0.0],
            events: 0,
        };
        blanker.set_rate(rate);
        blanker
    }

    /// Replans the window for a new sample rate.
    ///
    /// The delay line is discarded, which costs the window itself: a handful of
    /// samples at any rate this reaches.
    pub fn set_rate(&mut self, rate: u32) {
        let rate = rate.max(1);
        if rate == self.rate {
            return;
        }
        self.rate = rate;

        let fs = rate as f32;
        let mut window = ((WINDOW_MS * 0.001 * fs).round() as usize).clamp(3, MAX_WINDOW);
        // Odd, so the delay is a whole number of samples and the window centres
        // exactly on the detection rather than half a sample off.
        if window % 2 == 0 {
            window += 1;
        }
        self.window = window;
        self.alpha = (1.0 - (-1.0 / (BASELINE_S * fs)).exp()).clamp(1e-6, 1.0);

        self.delay.clear();
        self.delay.resize(window, [0.0, 0.0]);
        self.pos = 0;
        self.remaining = 0;
        self.baseline = 0.0;
    }

    pub fn configure(&mut self, enabled: bool, threshold: f32, mode: ChannelMode) {
        self.enabled = enabled;
        self.threshold = threshold.clamp(1.5, 40.0);
        self.mode = mode;
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    /// Delay the stage introduces, in samples.
    ///
    /// Nought while it is off, because a stage that does nothing must not cost
    /// latency either.
    pub fn delay(&self) -> usize {
        if self.enabled {
            self.window / 2
        } else {
            0
        }
    }

    pub fn reset(&mut self) {
        for slot in self.delay.iter_mut() {
            *slot = [0.0, 0.0];
        }
        self.pos = 0;
        self.remaining = 0;
        self.baseline = 0.0;
        self.held = [0.0, 0.0];
    }

    /// Removes impulses from one block, in place.
    ///
    /// Both channels are blanked whichever one the detection came from. On a
    /// quadrature pair an impulse is on both by construction, and on a real pair
    /// the second channel is not read at all, so blanking it costs nothing and
    /// keeps the two in step for anything that reads them together later.
    pub fn process(&mut self, frames: &mut [[f32; 2]]) {
        if !self.enabled || self.delay.is_empty() {
            return;
        }
        let window = self.window;

        for frame in frames.iter_mut() {
            // Measured on the reduction rather than on either channel, because
            // the reduction is the signal everything downstream actually reads.
            let level = Converter::reduce(self.mode, *frame).abs();
            let hit = self.baseline > 1e-9 && level > self.baseline * self.threshold;

            // The level is only updated on a sample that is not part of an
            // event. Letting an impulse into the average is what makes a blanker
            // stop working under heavy interference: the level rises to meet the
            // clicks and the threshold is never crossed again.
            if !hit {
                self.baseline += self.alpha * (level - self.baseline);
            }

            let out = self.delay[self.pos];
            self.delay[self.pos] = *frame;
            self.pos = (self.pos + 1) % window;

            if hit {
                if self.remaining == 0 {
                    self.events += 1;
                    // The value held is the one about to leave the delay line,
                    // which is the last sample before the event reached the
                    // input.
                    self.held = out;
                }
                self.remaining = window;
            }

            if self.remaining > 0 {
                self.remaining -= 1;
                *frame = self.held;
            } else {
                *frame = out;
            }
        }
    }
}