//! Short time spectrum.
//!
//! Samples are appended to a queue and consumed one hop at a time. Each frame
//! is windowed, transformed and folded into a running power average, and the
//! result is exposed in decibels relative to full scale.
//!
//! The consumer drives the loop:
//!     spectrum.feed(&samples);
//!     while spectrum.next_frame() { use spectrum.bins(); }
//! Splitting feed from next_frame keeps every waterfall line visible to the
//! caller instead of only the last one of a block.
//!
//! ## Why the quadrature correction is applied here as well
//!
//! The receiver chain corrects the two channels before it does anything else,
//! and the display has to make the same correction or it is not showing the
//! signal that reaches the ear. The channel swap is the case that matters:
//! swapping conjugates the spectrum, so a display that ignores it draws the
//! upper sideband where the lower one is and the operator hears the opposite
//! of what the picture claims. The gain and phase terms are applied for the
//! same reason at a smaller scale: without them the display keeps the image
//! the receiver has already suppressed.

use crate::audio::process::DC_CORNER_HZ;
use crate::config::settings::WindowFn;

use super::fft::Fft;
use super::window;

/// Floor reported for a bin with no energy. Far below anything a sound card
/// can deliver, so it never clips a real reading.
const SILENCE_DB: f32 = -200.0;

/// Time constant of the long power average, in seconds.
///
/// Four seconds averages the noise down by about an order of magnitude in power
/// while a steady carrier stays exactly where it is, which is the whole point: a
/// tone below the noise of one frame is above the noise of a hundred.
///
/// Longer would find a weaker carrier and would take longer to notice that it
/// stopped, and the second cost is the one an operator feels: a trace that still
/// shows a station a minute after it left is a trace that lies.
const SLOW_SECONDS: f32 = 4.0;

pub struct Spectrum {
    fft: Fft,
    size: usize,
    hop: usize,
    window: Vec<f32>,
    /// Turns a windowed bin magnitude into an amplitude ratio.
    norm: f32,
    /// Input queue, both channels.
    fifo: Vec<[f32; 2]>,
    re: Vec<f32>,
    im: Vec<f32>,
    /// Smoothed linear power per bin, in display order.
    power: Vec<f32>,
    db: Vec<f32>,
    peak_db: Vec<f32>,
    alpha: f32,
    /// Long power average, and the same in decibels.
    ///
    /// A second accumulator rather than a longer time constant on the first.
    /// The first is what the display and the decoders read and has to follow the
    /// signal; this one exists precisely because it does not.
    slow: Vec<f32>,
    slow_db: Vec<f32>,
    slow_alpha: f32,
    sample_rate: u32,
    /// True when the second channel is the quadrature component.
    ///
    /// A real signal has a spectrum symmetric about nought, so the negative
    /// half carries nothing that is not already in the positive one and keeping
    /// it would draw every signal twice. A complex signal genuinely has two
    /// halves and discarding one would throw away half the band.
    complex: bool,
    /// Quadrature correction, mirroring what the receiver front end applies.
    swap: bool,
    iq_gain: f32,
    iq_sin: f32,
    iq_cos: f32,
    primed: bool,
    frames: u64,
}

impl Spectrum {
    pub fn new(
        size: usize,
        hop: usize,
        kind: WindowFn,
        beta: f32,
        average_frames: u32,
        sample_rate: u32,
    ) -> Spectrum {
        let size = size.max(64).next_power_of_two();
        let hop = hop.clamp(1, size);
        let w = window::build(kind, size, beta);

        // A full scale sine produces a peak bin of amplitude times the window
        // sum divided by two, so dividing by half the sum reads zero decibels.
        let s = window::sum(&w);
        let norm = if s > 1e-9 { 2.0 / s } else { 1.0 };

        let bins = size / 2;
        crate::log_info!(
            "dsp",
            "spectrum {} bins, hop {}, {:.2} lines per second at {} Hz",
            bins,
            hop,
            sample_rate as f32 / hop as f32,
            sample_rate
        );

        Spectrum {
            fft: Fft::new(size),
            size,
            hop,
            window: w,
            norm,
            fifo: Vec::with_capacity(size * 2),
            re: vec![0.0; size],
            im: vec![0.0; size],
            power: vec![0.0; bins],
            db: vec![SILENCE_DB; bins],
            peak_db: vec![SILENCE_DB; bins],
            alpha: 1.0 / average_frames.max(1) as f32,
            slow: vec![0.0; bins],
            slow_db: vec![SILENCE_DB; bins],
            // Stated in seconds and converted here, so the trace behaves the
            // same whatever the transform geometry: a coefficient per frame
            // would average over four times as long at a quarter of the rate.
            slow_alpha: {
                let frame_seconds = hop as f32 / sample_rate.max(1) as f32;
                (1.0 - (-frame_seconds / SLOW_SECONDS).exp()).clamp(1e-5, 1.0)
            },
            sample_rate,
            complex: false,
            swap: false,
            iq_gain: 1.0,
            iq_sin: 0.0,
            iq_cos: 1.0,
            primed: false,
            frames: 0,
        }
    }

    /// Bins in ascending frequency order.
    ///
    /// For a complex input the negative half comes first, so the index of a bin
    /// is its position on the display and nothing downstream has to know about
    /// the transform ordering.
    pub fn bins(&self) -> &[f32] {
        &self.db
    }

    pub fn peaks(&self) -> &[f32] {
        &self.peak_db
    }

    /// Power inside a band and over the whole span, in linear units.
    ///
    /// Both figures rather than the first alone, because the useful answer is the
    /// ratio: a band measured against the whole passband is calibration free,
    /// however the transform happens to be normalized, so a narrow reading can be
    /// applied to a figure measured somewhere else entirely without either of
    /// them having to know about the other.
    ///
    /// Linear rather than decibels, and taken from the accumulator rather than
    /// from the published values. Converting a few thousand bins out of the
    /// logarithm every frame would cost more than the transform that produced
    /// them, and the accumulator holds exactly what is wanted already.
    pub fn band_power(&self, low_hz: f32, high_hz: f32) -> (f32, f32) {
        let bin = self.bin_hz();
        if bin <= 0.0 || self.power.is_empty() {
            return (0.0, 0.0);
        }
        let base = self.low_hz();
        let (lo, hi) = if low_hz <= high_hz {
            (low_hz, high_hz)
        } else {
            (high_hz, low_hz)
        };
        let last = self.power.len() - 1;
        let first = (((lo - base) / bin).floor().max(0.0) as usize).min(last);
        let final_bin = (((hi - base) / bin).ceil().max(0.0) as usize).min(last);

        let mut band = 0.0f32;
        let mut total = 0.0f32;
        for (k, &p) in self.power.iter().enumerate() {
            total += p;
            if k >= first && k <= final_bin {
                band += p;
            }
        }
        (band, total)
    }

    /// Long power average, in decibels and in display order.
    ///
    /// Read by the trace and by the automatic notch search, which want the same
    /// surface for the same reason: both are looking for what stays.
    pub fn average(&self) -> &[f32] {
        &self.slow_db
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Width of one bin in hertz.
    pub fn bin_hz(&self) -> f32 {
        self.sample_rate as f32 / self.size as f32
    }

    /// Highest frequency the transform can represent.
    pub fn nyquist_hz(&self) -> f32 {
        self.sample_rate as f32 * 0.5
    }

    pub fn is_complex(&self) -> bool {
        self.complex
    }

    /// Lowest frequency the display covers.
    pub fn low_hz(&self) -> f32 {
        if self.complex {
            -self.nyquist_hz()
        } else {
            0.0
        }
    }

    /// Highest frequency the display covers.
    pub fn high_hz(&self) -> f32 {
        self.nyquist_hz()
    }

    /// Frequency of a bin, by its position in the returned slice.
    pub fn hz_of_bin(&self, index: usize) -> f32 {
        self.low_hz() + index as f32 * self.bin_hz()
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Switches between the one sided and the two sided arrangement.
    ///
    /// The bin count changes with it, so every accumulator is resized and
    /// cleared: an average carried across the change would hold values from
    /// frequencies the new arrangement puts elsewhere.
    pub fn set_complex(&mut self, complex: bool) {
        if complex == self.complex {
            return;
        }
        self.complex = complex;
        let bins = if complex { self.size } else { self.size / 2 };
        self.power.clear();
        self.power.resize(bins, 0.0);
        self.db.clear();
        self.db.resize(bins, SILENCE_DB);
        self.peak_db.clear();
        self.peak_db.resize(bins, SILENCE_DB);
        self.slow.clear();
        self.slow.resize(bins, 0.0);
        self.slow_db.clear();
        self.slow_db.resize(bins, SILENCE_DB);
        self.primed = false;
        crate::log_info!(
            "dsp",
            "spectrum {} sided, {} bins",
            if complex { "two" } else { "one" },
            bins
        );
    }

    /// Sets the quadrature correction, in the same terms the receiver takes.
    ///
    /// Applied to the quadrature channel alone, because only the ratio between
    /// the two paths is observable and an absolute scale belongs to the gain
    /// control. The correction is the inverse of the usual model where the two
    /// paths differ by a gain and a phase, which is exactly what the receiver
    /// front end computes, so the two cannot drift apart in shape.
    pub fn set_iq_correction(&mut self, swap: bool, gain_db: f32, phase_deg: f32) {
        self.swap = swap;
        self.iq_gain = 10.0f32.powf(gain_db.clamp(-12.0, 12.0) / 20.0);
        let phi = phase_deg.clamp(-45.0, 45.0).to_radians();
        self.iq_sin = phi.sin();
        // Guarded rather than clamped at the input, so a phase near ninety
        // degrees produces a large correction instead of a division by nought.
        self.iq_cos = phi.cos().max(1e-3);
    }

    pub fn feed(&mut self, input: &[[f32; 2]]) {
        if input.is_empty() {
            return;
        }
        self.fifo.extend_from_slice(input);

        // A backlog can only build up when the interface stalls. Dropping the
        // oldest samples keeps the display in real time; the alternative would
        // be a waterfall that slowly falls behind the receiver.
        let limit = self.size + self.hop * 64;
        if self.fifo.len() > limit {
            let excess = self.fifo.len() - limit;
            self.fifo.drain(..excess);
        }
    }

    /// Consumes one hop and refreshes the bins. Returns false when there is
    /// not enough data yet.
    pub fn next_frame(&mut self) -> bool {
        if self.fifo.len() < self.size {
            return false;
        }

        for i in 0..self.size {
            let w = self.window[i];
            if self.complex {
                let frame = self.fifo[i];
                let (in_phase, raw_q) = if self.swap {
                    (frame[1], frame[0])
                } else {
                    (frame[0], frame[1])
                };
                let scaled = raw_q / self.iq_gain;
                let quadrature = (scaled - in_phase * self.iq_sin) / self.iq_cos;
                self.re[i] = in_phase * w;
                self.im[i] = quadrature * w;
            } else {
                self.re[i] = self.fifo[i][0] * w;
                self.im[i] = 0.0;
            }
        }
        self.fft.forward(&mut self.re, &mut self.im);

        let bins = self.db.len();
        for k in 0..bins {
            // Display order to transform order. A two sided display starts at
            // minus Nyquist, which the transform places in its upper half.
            let src = if self.complex {
                (k + self.size / 2) % self.size
            } else {
                k
            };
            let mag = (self.re[src] * self.re[src] + self.im[src] * self.im[src]).sqrt() * self.norm;
            let p = mag * mag;

            // The first frame seeds the average so the display does not have
            // to climb out of silence after a restart.
            if self.primed {
                self.power[k] += self.alpha * (p - self.power[k]);
            } else {
                self.power[k] = p;
            }

            self.db[k] = if self.power[k] <= 1e-20 {
                SILENCE_DB
            } else {
                10.0 * self.power[k].log10()
            };

            // Averaged in power rather than in decibels. Averaging logarithms
            // drags an intermittent signal towards the noise in proportion to
            // its duty cycle and does it geometrically, which is neither what a
            // long average means nor what the notch search compares against.
            if self.primed {
                self.slow[k] += self.slow_alpha * (p - self.slow[k]);
            } else {
                self.slow[k] = p;
            }
            self.slow_db[k] = if self.slow[k] <= 1e-20 {
                SILENCE_DB
            } else {
                10.0 * self.slow[k].log10()
            };
        }

        self.suppress_dc();

        // The peak hold reads the cleaned values, otherwise the artifact would
        // be held forever in the very surface that exists to keep a transient
        // visible.
        for k in 0..bins {
            let db = self.db[k];
            if db > self.peak_db[k] {
                self.peak_db[k] = db;
            } else {
                self.peak_db[k] -= 0.4;
            }
        }

        self.primed = true;
        self.frames += 1;
        self.fifo.drain(..self.hop);
        true
    }

    /// Covers the bins the capture high pass has already emptied.
    ///
    /// The offset blocker at the capture stage removes the direct current of
    /// the sound card and, on a quadrature input, the leakage of the local
    /// oscillator. It removes them exactly at nought and leaves the skirt of
    /// that removal in the bins beside it, which on a two sided display sit in
    /// the middle of the picture. What appears there is a permanent vertical
    /// line at the tuning point that an operator reads as a station and that
    /// no amount of tuning moves.
    ///
    /// Those bins carry nothing about the band, because the content they would
    /// describe was removed before the transform ever saw it. They are filled
    /// with a straight line in decibels between the nearest bins outside the
    /// cut, which reads as the surrounding floor. The cut follows the corner of
    /// the blocker rather than a constant of its own, so the display and the
    /// audio cannot disagree about which range has been emptied.
    fn suppress_dc(&mut self) {
        let bin_hz = self.bin_hz();
        let n = self.db.len();
        if bin_hz <= 0.0 || n < 8 {
            return;
        }

        let radius = ((DC_CORNER_HZ / bin_hz).ceil() as usize).max(1);
        // Nought sits in the middle of a two sided display and at the left edge
        // of a one sided one.
        let centre = if self.complex { n / 2 } else { 0 };
        let lo = centre.saturating_sub(radius);
        let hi = (centre + radius).min(n - 1);
        if hi + 1 >= n && lo == 0 {
            // The cut would swallow the whole display, which means the
            // transform is far too short to be showing anything at all.
            return;
        }

        // Endpoints just outside the cut. At an edge the nearer one stands in
        // for both, which is the flattest reading available and is what the one
        // sided arrangement always needs.
        let left = if lo > 0 {
            self.db[lo - 1]
        } else {
            self.db[(hi + 1).min(n - 1)]
        };
        let right = if hi + 1 < n { self.db[hi + 1] } else { left };

        let span = (hi - lo + 1) as f32;
        for k in lo..=hi {
            let t = (k - lo + 1) as f32 / (span + 1.0);
            self.db[k] = left + (right - left) * t;
        }
    }

    /// Slides the accumulated surfaces by a whole number of bins.
    ///
    /// Called when the dial moved and the display is anchored to the band. The
    /// peak hold and the running average are statements about audio positions,
    /// and a retune moves every station to a different one; leaving them alone
    /// would leave a peak marking a frequency nothing is on, which is worse
    /// than no peak at all.
    ///
    /// Shifting rather than clearing keeps the accumulation of everything that
    /// stayed in view, which after a retune of a few kilohertz is most of it.
    /// Bins vacated at the edge are set to silence, because nothing has been
    /// measured there.
    pub fn shift(&mut self, bins: i32) {
        if bins == 0 {
            return;
        }
        let n = self.db.len();
        if n == 0 {
            return;
        }
        let step = bins.unsigned_abs() as usize;
        if step >= n {
            // Everything held refers to frequencies no longer represented.
            for v in self.power.iter_mut() {
                *v = 0.0;
            }
            for v in self.slow.iter_mut() {
                *v = 0.0;
            }
            for v in self.peak_db.iter_mut() {
                *v = SILENCE_DB;
            }
            for v in self.slow_db.iter_mut() {
                *v = SILENCE_DB;
            }
            self.primed = false;
            return;
        }

        if bins > 0 {
            self.power.copy_within(..n - step, step);
            self.slow.copy_within(..n - step, step);
            self.peak_db.copy_within(..n - step, step);
            self.slow_db.copy_within(..n - step, step);
            for v in self.power[..step].iter_mut() {
                *v = 0.0;
            }
            for v in self.slow[..step].iter_mut() {
                *v = 0.0;
            }
            for v in self.peak_db[..step].iter_mut() {
                *v = SILENCE_DB;
            }
            for v in self.slow_db[..step].iter_mut() {
                *v = SILENCE_DB;
            }
        } else {
            self.power.copy_within(step.., 0);
            self.slow.copy_within(step.., 0);
            self.peak_db.copy_within(step.., 0);
            self.slow_db.copy_within(step.., 0);
            for v in self.power[n - step..].iter_mut() {
                *v = 0.0;
            }
            for v in self.slow[n - step..].iter_mut() {
                *v = 0.0;
            }
            for v in self.peak_db[n - step..].iter_mut() {
                *v = SILENCE_DB;
            }
            for v in self.slow_db[n - step..].iter_mut() {
                *v = SILENCE_DB;
            }
        }
    }

    pub fn reset(&mut self) {
        self.fifo.clear();
        self.primed = false;
        for v in self.power.iter_mut() {
            *v = 0.0;
        }
        for v in self.db.iter_mut() {
            *v = SILENCE_DB;
        }
        for v in self.peak_db.iter_mut() {
            *v = SILENCE_DB;
        }
        for v in self.slow.iter_mut() {
            *v = 0.0;
        }
        for v in self.slow_db.iter_mut() {
            *v = SILENCE_DB;
        }
    }
}