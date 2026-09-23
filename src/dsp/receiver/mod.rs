//! Receiver chain.
//!
//! Built only in the receiver mode. Not switched off in the other one, not
//! built: the keying detectors derive their threshold from the envelope and
//! measure element durations against it, so a filter that rounds the edges, a
//! noise reduction that removes the unpredictable part of a keying transition,
//! or a gain loop with time constants overlapping the keying each destroy the
//! measurement. A chain that exists and is bypassed invites a future edit to
//! route through it; one that was never constructed does not.
//!
//! ## Order, and where it departs from the obvious one
//!
//! ```text
//!   complex input, imbalance corrected
//!   wide blanker            impulse is still an impulse
//!   complex bandpass        independent edges
//!   narrow blanker          what the filter made of the impulse
//!   detector                complex to real
//!   notch                   one carrier out of speech
//!   noise reduction         predictable against unpredictable
//!   gain control            with hang
//!   squelch                 measured before the gain
//! ```
//!
//! Two stages sit later than a first sketch would put them.
//!
//! Noise reduction is after the detector rather than before it. An adaptive
//! predictor separates a signal from noise by predictability, and on a complex
//! baseband the most predictable component is the carrier itself, which for
//! amplitude modulation is exactly what the detector still needs. On a frequency
//! modulated signal the argument is stronger: noise there is not additive at the
//! output, so removing it before the discriminator removes the wrong thing.
//!
//! The squelch measures before the gain control and gates after it. Measuring
//! after would be measuring the target level, which every signal reaches by
//! construction and which therefore carries no information about whether there
//! is a signal at all.
//!
//! ## Cost
//!
//! The two convolutions dominate. A narrow filter plans a few hundred taps and
//! is evaluated on a complex signal, so roughly a thousand operations per input
//! sample; the predictor adds a hundred and twenty. At the decoder rate that is
//! around fourteen million operations a second, which is a low single digit
//! percentage of one core and two orders of magnitude below the frame budget.

pub mod agc;
pub mod detector;
pub mod filter;
pub mod iq;
pub mod nb;
pub mod nr;

use crate::config::settings::{ReceiverSettings, Settings};

use agc::{Agc, Squelch};
use detector::DetectorBank;
use filter::Bandpass;
use iq::{Complex, IqFront};
use nb::Blanker;
use nr::{Notch, NoiseReduction};

/// Window of the blanker ahead of the filter, in milliseconds.
///
/// Under a millisecond, because that is the duration of the event itself: an
/// impulse that has not been filtered yet occupies a handful of samples.
const WIDE_WINDOW_MS: f32 = 0.5;

/// Window of the blanker after it.
///
/// Longer by an order of magnitude, because what the filter produced from that
/// impulse is its own impulse response and lasts as long as the filter does.
const NARROW_WINDOW_MS: f32 = 4.0;

/// What the chain reports about itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverStatus {
    /// Gain the loop is applying, in decibels.
    pub gain_db: f32,
    /// True while the squelch is passing.
    pub open: bool,
    /// True while the synchronous detector holds a carrier.
    pub locked: bool,
    /// Tuning error the synchronous loop is absorbing, in hertz.
    pub carrier_offset_hz: f32,
    /// Impulses each blanker acted on.
    pub wide_events: u64,
    pub narrow_events: u64,
    /// Total delay through the chain, in samples. A readout rather than a
    /// setting: it follows from the filter and it is what an operator hears as
    /// the lag between a keying edge and the display.
    pub delay_samples: usize,
}

pub struct ReceiverChain {
    rate: u32,
    front: IqFront,
    wide: Blanker,
    filter: Bandpass,
    narrow: Blanker,
    detector: DetectorBank,
    notch: Notch,
    nr: NoiseReduction,
    agc: Agc,
    squelch: Squelch,
    status: ReceiverStatus,
}

impl ReceiverChain {
    pub fn new(rate: u32, settings: &ReceiverSettings) -> ReceiverChain {
        let mut chain = ReceiverChain {
            rate,
            front: IqFront::new(),
            wide: Blanker::new(rate, WIDE_WINDOW_MS),
            filter: Bandpass::new(rate),
            narrow: Blanker::new(rate, NARROW_WINDOW_MS),
            detector: DetectorBank::new(rate),
            notch: Notch::new(rate),
            nr: NoiseReduction::new(rate),
            agc: Agc::new(rate),
            squelch: Squelch::new(rate),
            status: ReceiverStatus::default(),
        };
        chain.sync_settings(settings);
        chain
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn status(&self) -> ReceiverStatus {
        self.status
    }

    /// Pushes every setting the chain reads.
    ///
    /// Takes the section rather than the whole configuration tree, because the
    /// chain runs on the monitor thread and the tree is owned by the interface
    /// one. Cheap enough to call per block: each stage decides for itself
    /// whether a value moved far enough to be worth acting on, which is what
    /// keeps a slider being dragged from replanning a five hundred tap filter
    /// on every frame.
    pub fn sync_settings(&mut self, rx: &ReceiverSettings) {
        self.front
            .configure(rx.iq_input, rx.iq_swap, rx.iq_gain_db, rx.iq_phase_deg);
        self.wide.configure(rx.nb_wide, rx.nb_wide_threshold);
        // The tuning point applies to a complex input and to nothing else. A
        // real input is audio a transceiver already demodulated, so the chain
        // must be transparent in frequency: with the point at nought the filter
        // mixer and the detector offset are the same number, they cancel, and a
        // tone at f leaves at f without a branch anywhere.
        let tune = if rx.iq_input { rx.tune_hz } else { 0.0 };
        self.filter.set_band(tune, rx.filter_low_hz, rx.filter_high_hz);

        // The detector is given the distance from the tuning point to the
        // middle of the passband, never the whole mixer frequency. That is what
        // leaves the audio referenced to the tuning point on a complex input,
        // and it is why a passband shift is silent while a retune is not.
        //
        // Keying on a complex input adds the beat oscillator, without which a
        // signal on the tuning point emerges at nought hertz.
        let bfo = rx.detector_offset_hz(rx.iq_input);
        self.detector
            .configure(rx.detector, self.filter.rel_centre_hz() + bfo);
        self.notch.configure(rx.notch_enabled, rx.notch_hz, rx.notch_width_hz);
        self.nr.configure(rx.nr_enabled, rx.nr_strength, rx.nr_method, rx.detector);
        self.agc.configure(
            self.rate,
            rx.agc_enabled,
            rx.agc_attack_ms,
            rx.agc_hang_ms,
            rx.agc_release_ms,
            rx.agc_target_db,
        );
        self.squelch.configure(rx.squelch_enabled, rx.squelch_db);

        self.status.delay_samples = self.front.delay()
            + self.wide.delay()
            + self.filter.delay()
            + self.narrow.delay()
            + self.nr.delay();
            
    }

    /// Convenience for a caller that holds the whole tree.
    pub fn sync(&mut self, settings: &Settings) {
        self.sync_settings(&settings.receiver);
    }

    /// Clears every stage.
    ///
    /// Called when the stream restarts. A filter holding samples from before a
    /// discontinuity releases them across it, and a gain loop that adapted to a
    /// level that no longer exists opens the next block at the wrong one.
    pub fn reset(&mut self) {
        self.front.reset();
        self.wide.reset();
        self.filter.reset();
        self.narrow.reset();
        self.detector.reset();
        self.notch.reset();
        self.nr.reset();
        self.agc.reset();
        self.squelch.reset();
    }

    /// Processes one block, writing the demodulated audio into the destination.
    ///
    /// Takes pairs and produces one real channel, because that is what the two
    /// ends genuinely are: the input may be a quadrature pair and the output is
    /// what reaches an ear. The quadrature slot is ignored on a real input,
    /// which lets the caller hand over whatever it has without knowing the mode.
    pub fn process(&mut self, input: &[[f32; 2]], out: &mut Vec<f32>) {
        out.clear();
        out.reserve(input.len());

        for frame in input {
            let mut z: Complex = self.front.sample(frame[0], frame[1]);

            z = self.wide.sample(z);
            z = self.filter.sample(z);
            z = self.narrow.sample(z);

            let mut audio = self.detector.sample(z);
            audio = self.notch.sample(audio);
            audio = self.nr.sample(audio);

            // The gate reads the signal before the gain loop touches it, and is
            // applied to the output. Measuring the gain controlled signal would
            // be measuring the target, which every signal reaches.
            let gate = self.squelch.sample(audio);
            out.push(self.agc.sample(audio) * gate);
        }

        self.status.gain_db = self.agc.gain_db();
        self.status.open = self.squelch.is_open();
        self.status.locked = self.detector.locked();
        self.status.carrier_offset_hz = self.detector.carrier_offset_hz();
        self.status.wide_events = self.wide.events();
        self.status.narrow_events = self.narrow.events();
    }
}