//! Frequency shift keying demodulator with asynchronous framing.
//!
//! Two matched filters, one per tone, are evaluated at eight times the baud
//! rate. Their difference is the discriminator; its sign is the bit. Framing is
//! the classic start and stop arrangement: a transition into the space state
//! arms the receiver, the state is confirmed half a bit later, then every bit is
//! sampled at its centre and the stop position has to read as mark.
//!
//! The discriminator is optionally normalized by the total energy of the two
//! tones. On a path with selective fading one tone can drop while the other
//! stays, which shifts a plain difference away from zero and biases the slicer;
//! dividing by the sum removes the level dependence entirely. The setting blends
//! the two because full normalization also amplifies noise in the gaps.

use crate::config::settings::{RttyAlphabet, RttyParity, RttySettings};

use crate::dsp::receiver::iq::Complex;

use super::baudot::{decode_ascii, Ita2};
use super::tone::{amp_to_db, coefficient, ToneBank};

/// Discriminator samples per bit. Eight is enough for the timing recovery used
/// here and keeps the filter evaluation rate low.
const FRAMES_PER_BIT: f32 = 8.0;

/// Seconds between two decisions about the tone polarity.
///
/// Long enough to hold a few dozen characters at any usable speed, which is what
/// makes the error rate a measurement rather than a sample.
const POLARITY_WINDOW_S: f32 = 4.0;

/// Seconds the search waits after a trial that did not help.
///
/// The polarity is one of two things, so a trial that failed has settled the
/// question. Retrying immediately would flip continuously on a signal that is
/// simply too weak to frame, which removes the characters that were decoding.
const POLARITY_REST_S: f32 = 30.0;

/// Frames a window needs before its error rate is acted on.
const POLARITY_MIN_FRAMES: u32 = 8;

/// Framing error rate above which the polarity is suspected.
///
/// Well above what a weak but correctly framed signal produces and well below
/// what an inverted one produces, which is nearly everything: with the sense
/// reversed the start bit is a mark and the receiver never arms at all, so the
/// few frames that do complete are noise.
const POLARITY_BAD: f32 = 0.40;

/// How much a trial has to improve the rate before it is kept.
const POLARITY_BETTER: f32 = 0.6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Waiting for the transition that starts a character.
    Hunt,
    /// Half a bit into a candidate start bit, about to confirm it.
    Confirm,
    Data,
    Stop,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FskStats {
    pub level_db: f32,
    /// Ratio of accepted frames to attempted ones, over the recent history.
    pub lock: f32,
    /// Baud rate implied by the shortest observed transition interval.
    pub baud_estimate: f32,
    /// Offset of the tone pair from where it is expected, in hertz.
    pub afc_offset_hz: f32,
    pub characters: u64,
    pub framing_errors: u64,
    pub parity_errors: u64,
    /// True while the automatic search is holding the polarity reversed.
    ///
    /// Reported separately from the stated setting, because the two are
    /// independent and an operator who cannot see the correction would read a
    /// working receiver as evidence that the setting is right.
    pub auto_inverted: bool,
}

pub struct FskDecoder {
    bank: ToneBank,
    dt: f32,
    rate: u32,
    /// True when the input carries a quadrature pair.
    complex: bool,

    mark_hz: f32,
    space_hz: f32,
    baud: f32,
    frames_per_bit: f32,
    data_bits: usize,
    stop_bits: f32,
    parity: RttyParity,
    alphabet: RttyAlphabet,
    invert: bool,
    atc: f32,

    /// Slow average of the combined tone energy, used to scale the normalized
    /// term of the discriminator and to drive the squelch.
    level: f32,
    /// Slow average of the discriminator, removed before slicing.
    bias: f32,
    last_mark: bool,

    state: State,
    /// Frames remaining until the next decision point, fractional so the bit
    /// period does not have to divide the frame period.
    countdown: f32,
    bits: u16,
    bit_index: usize,

    alpha: Ita2,
    /// Correction the automatic search settled on, applied over the setting.
    ///
    /// Held apart from the stated flag rather than written into it. The setting
    /// is what the operator asked for and is pushed in on every block, so a
    /// correction written there would be overwritten immediately; and a setting
    /// the application edits behind the operator is one they can no longer trust.
    auto_invert: bool,
    /// Seconds until the polarity is reconsidered.
    polarity_timer: f32,
    /// Frames accepted and refused since the last decision.
    window_ok: u32,
    window_bad: u32,
    /// Error rate before a trial reversal, so one that did not help is undone.
    trial_rate: f32,
    trial: bool,
    /// Refused frames of the window that has just closed.
    ///
    /// Carried across rather than read back out of the counters, because those
    /// are cleared before the decision is taken, so the next window starts
    /// clean whichever branch that decision chooses.
    closed_bad: u32,
    /// Frames since the last sign change, for the baud estimator.
    since_transition: f32,
    shortest: f32,
    lock_avg: f32,

    stats: FskStats,
    out: String,
}

impl FskDecoder {
    pub fn new(rate: u32, cfg: &RttySettings) -> FskDecoder {
        let baud = cfg.baud.clamp(10.0, 1200.0);
        // The matched filter spans one bit, which maximizes the tone
        // discrimination; the hop is what sets the timing resolution.
        let n = ((rate as f32 / baud).round() as usize).clamp(8, 8192);
        let hop = ((n as f32 / FRAMES_PER_BIT).round() as usize).max(1);

        let mark = cfg.mark_hz;
        let space = (cfg.mark_hz - cfg.shift_hz).max(20.0);
        let bank = ToneBank::new(rate, n, hop, &[mark, space]);
        let dt = bank.frame_seconds();
        let frames_per_bit = (1.0 / baud) / dt;

        crate::log_info!(
            "decode",
            "fsk detector mark {:.0} Hz space {:.0} Hz, {:.2} baud, window {} samples, {:.2} frames per bit",
            mark,
            space,
            baud,
            n,
            frames_per_bit
        );

        FskDecoder {
            bank,
            dt,
            rate,
            complex: false,
            mark_hz: mark,
            space_hz: space,
            baud,
            frames_per_bit,
            data_bits: cfg.data_bits.clamp(5, 8) as usize,
            stop_bits: cfg.stop_bits.clamp(1.0, 2.0),
            parity: cfg.parity,
            alphabet: cfg.alphabet,
            invert: cfg.invert,
            atc: cfg.atc.clamp(0.0, 1.0),
            level: 0.0,
            bias: 0.0,
            last_mark: true,
            state: State::Hunt,
            countdown: 0.0,
            bits: 0,
            bit_index: 0,
            alpha: Ita2::new(cfg.usos),
            auto_invert: false,
            polarity_timer: POLARITY_WINDOW_S,
            window_ok: 0,
            window_bad: 0,
            trial_rate: 1.0,
            trial: false,
            closed_bad: 0,
            since_transition: 0.0,
            shortest: f32::MAX,
            lock_avg: 0.0,
            stats: FskStats::default(),
            out: String::with_capacity(64),
        }
    }

    pub fn mark_hz(&self) -> f32 {
        self.mark_hz
    }

    pub fn shift_hz(&self) -> f32 {
        self.mark_hz - self.space_hz
    }

    pub fn baud(&self) -> f32 {
        self.baud
    }

    pub fn stats(&self) -> FskStats {
        self.stats
    }

    pub fn in_figures(&self) -> bool {
        self.alpha.in_figures()
    }

    /// Retunes the tone pair. Used by the automatic frequency control and by
    /// the classifier when it locks onto a different signal.
    pub fn set_tones(&mut self, mark_hz: f32, shift_hz: f32) {
        // A quadrature input has two distinguishable halves, so a pair below the
        // tuning point is a real pair rather than the mirror of one above it.
        let limit = self.rate as f32 * 0.45;
        let floor = if self.complex { -limit } else { 50.0 };
        let mark = mark_hz.clamp(floor, limit);
        let space = (mark - shift_hz.clamp(20.0, 2000.0)).max(floor);
        if (mark - self.mark_hz).abs() < 0.5 && (space - self.space_hz).abs() < 0.5 {
            return;
        }
        self.mark_hz = mark;
        self.space_hz = space;
        self.bank.set_frequencies(&[mark, space]);
        // Retuning invalidates the framing position, so the receiver hunts
        // again rather than finishing a character with mixed data.
        self.state = State::Hunt;
        // The window was measured on another signal, and a trial in progress was
        // a question about that signal rather than this one.
        self.window_ok = 0;
        self.window_bad = 0;
        self.trial = false;
    }

    /// States whether the input carries a quadrature pair.
    pub fn set_complex(&mut self, complex: bool) {
        if complex == self.complex {
            return;
        }
        self.complex = complex;
        self.bank.set_complex(complex);
        // The tones were clamped under the other arrangement, so they are
        // reapplied rather than left where the bound pushed them.
        let (mark, shift) = (self.mark_hz, self.shift_hz());
        self.mark_hz = 0.0;
        self.set_tones(mark, shift);
        self.state = State::Hunt;
        self.window_ok = 0;
        self.window_bad = 0;
        self.trial = false;
    }

    pub fn feed(&mut self, input: &[Complex], cfg: &RttySettings) {
        if !cfg.enabled {
            return;
        }
        // Runtime settings that do not change the filter geometry are picked up
        // per block, so a toggle takes effect immediately.
        self.invert = cfg.invert;
        self.atc = cfg.atc.clamp(0.0, 1.0);
        self.parity = cfg.parity;
        self.alphabet = cfg.alphabet;
        self.alpha.unshift_on_space = cfg.usos;

        self.bank.feed(input);
        while self.bank.next_frame() {
            let amps = self.bank.amps();
            let (m, s) = (amps[0], amps[1]);
            self.on_frame(m, s, cfg);
        }
    }

    pub fn drain(&mut self, dst: &mut String) {
        if !self.out.is_empty() {
            dst.push_str(&self.out);
            self.out.clear();
        }
    }

    pub fn reset(&mut self) {
        self.bank.reset();
        self.state = State::Hunt;
        self.bits = 0;
        self.bit_index = 0;
        self.alpha.reset();
        self.bias = 0.0;
        self.shortest = f32::MAX;
        self.window_ok = 0;
        self.window_bad = 0;
        self.trial = false;
        // The correction is kept. It describes the wiring between the receiver
        // and the sound card, which a discontinuity in the audio does not change,
        // and discarding it would make every restart cost the search again.
    }

    /// Decides whether the tone polarity is the wrong way round.
    ///
    /// Tried rather than predicted, which is what the setting name says. There is
    /// no cheap test for the other sense: with the polarity reversed the start bit
    /// is a mark, so the receiver never arms and there is no shadow frame to
    /// compare against. A second framing machine running on the inverted sense
    /// would answer it and would double the cost of the one thing this decoder
    /// does per frame.
    ///
    /// So the rate is measured, the polarity is reversed on suspicion, and the
    /// rate is measured again. A reversal that did not help is undone and the
    /// question is left alone for a while: polarity is one of two things, and a
    /// trial that failed has settled it.
    fn reconsider_polarity(&mut self, cfg: &RttySettings) {
        if !cfg.bit_inversion_retry {
            self.trial = false;
            return;
        }
        self.polarity_timer -= self.dt;
        if self.polarity_timer > 0.0 {
            return;
        }
        self.polarity_timer = POLARITY_WINDOW_S;

        let total = self.window_ok + self.window_bad;
        // Held before the counters are cleared, so the decision below reads the
        // window that closed rather than the one just beginning.
        self.closed_bad = self.window_bad;
        self.window_ok = 0;
        self.window_bad = 0;
        if self.trial {
            // A window with too few frames is evidence against the trial rather
            // than absence of evidence. The trial was only entered because frames
            // were completing, and reversing the sense stops the receiver arming
            // at all: the start bit becomes a mark and never triggers. So a
            // window that produced almost nothing is the shape of a reversal that
            // was wrong, and waiting it out leaves the wrong one standing.
            //
            // A signal that merely stopped reads the same way, and undoing costs
            // nothing there: it returns the polarity to where the trial began.
            let rate = if total < POLARITY_MIN_FRAMES {
                1.0
            } else {
                self.last_rate(total)
            };
            if rate < self.trial_rate * POLARITY_BETTER {
                self.trial = false;
                crate::log_info!(
                    "decode",
                    "fsk polarity reversed, framing errors {:.0} percent to {:.0} percent",
                    self.trial_rate * 100.0,
                    rate * 100.0
                );
            } else {
                self.auto_invert = !self.auto_invert;
                self.trial = false;
                self.polarity_timer = POLARITY_REST_S;
                crate::log_debug!(
                    "decode",
                    "fsk polarity was not the fault, framing errors stayed at {:.0} percent",
                    rate * 100.0
                );
            }
            self.stats.auto_inverted = self.auto_invert;
            return;
        }

        if total < POLARITY_MIN_FRAMES {
            // Too few frames to be a measurement. Nothing is in progress here, so
            // a quiet band reads as a perfect error rate and is taken for a signal
            // that needs nothing done to it, which is the right reading.
            return;
        }
        let rate = self.last_rate(total);
        if rate > POLARITY_BAD {
            self.trial_rate = rate;
            self.trial = true;
            self.auto_invert = !self.auto_invert;
            self.stats.auto_inverted = self.auto_invert;
            // The framing position was derived under the other sense, so it means
            // nothing under this one.
            self.state = State::Hunt;
            crate::log_debug!(
                "decode",
                "fsk framing errors at {:.0} percent, trying the other polarity",
                rate * 100.0
            );
        }
    }

    /// Error rate of the window that has just closed.
    ///
    /// The counters are cleared before the decision so the next window starts
    /// clean whichever branch is taken, so the rate is carried across in a field
    /// rather than read back out of them.
    fn last_rate(&self, total: u32) -> f32 {
        if total == 0 {
            return 0.0;
        }
        (self.closed_bad as f32 / total as f32).clamp(0.0, 1.0)
    }

    fn on_frame(&mut self, m: f32, s: f32, cfg: &RttySettings) {
        let total = m + s;
        self.level += coefficient(0.200, self.dt) * (total - self.level);
        self.stats.level_db = amp_to_db(self.level * 0.5);

        // Blended discriminator. The normalized term is scaled by the tracked
        // level so both contributions stay in the same range.
        let raw = m - s;
        let normalized = raw / (total + 1e-9) * self.level;
        let d = (1.0 - self.atc) * raw + self.atc * normalized;

        // Slow bias removal. Twenty bit periods is long enough not to follow
        // the data and short enough to track a drifting receiver.
        self.bias += coefficient(20.0 / self.baud, self.dt) * (d - self.bias);
        let y = d - self.bias;

        let squelched = self.stats.level_db < cfg.squelch_db;
        if squelched {
            self.state = State::Hunt;
            self.lock_avg += coefficient(1.0, self.dt) * (0.0 - self.lock_avg);
            self.stats.lock = self.lock_avg;
            return;
        }

        // The stated setting and the correction the search found, combined. Two
        // reversals are none, which is why this is a difference rather than a
        // pair of tests.
        let inverted = self.invert != self.auto_invert;
        let mark = if inverted { y < 0.0 } else { y > 0.0 };

        // Transition intervals feed the baud estimator. The shortest interval
        // observed is one bit, so its reciprocal is the baud rate; it decays
        // slowly upwards so a single glitch does not pin it forever.
        self.since_transition += self.dt;
        if mark != self.last_mark {
            if self.since_transition < self.shortest {
                self.shortest = self.since_transition;
            }
            self.since_transition = 0.0;
        }
        self.shortest *= 1.0 + 0.00002;
        if self.shortest.is_finite() && self.shortest > 1e-4 {
            self.stats.baud_estimate = 1.0 / self.shortest;
        }

        // Automatic frequency control. The residual bias is proportional to the
        // frequency error, so it is fed back as a correction of both tones.
        if cfg.afc && self.state == State::Data {
            let slope = self.shift_hz() * 0.25;
            let correction = (self.bias / self.level.max(1e-6)) * slope;
            let limited = correction.clamp(-cfg.afc_range_hz, cfg.afc_range_hz);
            self.stats.afc_offset_hz += 0.02 * (limited - self.stats.afc_offset_hz);
        }

        match self.state {
            State::Hunt => {
                // A falling edge into the space state is a candidate start bit.
                if self.last_mark && !mark {
                    self.state = State::Confirm;
                    self.countdown = self.frames_per_bit * 0.5;
                }
            }
            State::Confirm => {
                self.countdown -= 1.0;
                if self.countdown <= 0.0 {
                    if !mark {
                        self.state = State::Data;
                        self.bits = 0;
                        self.bit_index = 0;
                        self.countdown = self.frames_per_bit;
                    } else {
                        // The edge was noise, not a start bit.
                        self.state = State::Hunt;
                    }
                }
            }
            State::Data => {
                self.countdown -= 1.0;
                if self.countdown <= 0.0 {
                    // Data bits arrive with the least significant one first.
                    if mark {
                        self.bits |= 1 << self.bit_index;
                    }
                    self.bit_index += 1;
                    self.countdown += self.frames_per_bit;
                    let expected = self.data_bits + usize::from(self.parity != RttyParity::None);
                    if self.bit_index >= expected {
                        self.state = State::Stop;
                    }
                }
            }
            State::Stop => {
                self.countdown -= 1.0;
                if self.countdown <= 0.0 {
                    // The stop element is the mark state. A space here means the
                    // framing slipped, so the character is discarded and the
                    // receiver hunts for the next edge.
                    if mark {
                        self.accept();
                        self.window_ok = self.window_ok.saturating_add(1);
                        self.lock_avg += coefficient(1.0, self.dt) * (1.0 - self.lock_avg);
                    } else {
                        self.stats.framing_errors += 1;
                        // Counted apart from the parity failures inside accept.
                        // A stop bit in the wrong state is evidence about the
                        // polarity; a parity failure is evidence about the noise.
                        self.window_bad = self.window_bad.saturating_add(1);
                        self.lock_avg += coefficient(1.0, self.dt) * (0.0 - self.lock_avg);
                    }
                    self.stats.lock = self.lock_avg;
                    self.state = State::Hunt;
                }
            }
        }

        self.last_mark = mark;
        self.reconsider_polarity(cfg);
    }

    fn accept(&mut self) {
        let mut code = self.bits;

        if self.parity != RttyParity::None {
            let parity_bit = (code >> self.data_bits) & 1 == 1;
            code &= (1u16 << self.data_bits) - 1;
            let ones = code.count_ones() % 2 == 1;
            let expected = match self.parity {
                RttyParity::Even => ones,
                RttyParity::Odd => !ones,
                RttyParity::Mark => true,
                RttyParity::Space => false,
                RttyParity::None => parity_bit,
            };
            if parity_bit != expected {
                self.stats.parity_errors += 1;
                return;
            }
        }

        let decoded = match self.alphabet {
            RttyAlphabet::Baudot => self.alpha.decode(code as u8),
            RttyAlphabet::Ascii => decode_ascii(code as u8, self.data_bits),
        };

        if let Some(ch) = decoded {
            // A carriage return on its own would overwrite the line in a text
            // view that honours it, so only the line feed is kept.
            if ch != '\r' {
                self.out.push(ch);
            }
            self.stats.characters += 1;
        }
    }
}