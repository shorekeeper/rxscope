//! PSK31 decoder.
//!
//! ## Why differential detection rather than a carrier loop
//!
//! The format is differentially encoded by design: a phase reversal is a nought
//! and its absence is a one. So the natural demodulator multiplies each symbol
//! by the conjugate of the one before it and reads the sign of the result.
//! Nothing needs to know the absolute phase, which removes the carrier recovery
//! loop and with it the acquisition delay and the phase ambiguity such a loop
//! has to resolve.
//!
//! The cost is three decibels against coherent detection, because the reference
//! carries noise of its own. That is the trade the format already made, and
//! reversing it here would mean recovering a carrier in order to undo the
//! encoding a transmitter applied precisely so that nobody would have to.
//!
//! ## Where the timing comes from
//!
//! The transmitted envelope is a raised cosine that reaches nought at a phase
//! reversal, which is what keeps a signal carrying fifty words a minute inside
//! thirty one hertz. That nought sits exactly halfway between two sampling
//! instants, so the envelope is largest where a symbol should be read and
//! smallest between two of them: the signal carries its own clock in its
//! amplitude.
//!
//! Extracted as one bin of a transform at the symbol rate, which is a complex
//! accumulator and two multiplies per frame. Its argument is the sampling phase
//! directly, so there is no loop to tune and nothing to converge on. A decaying
//! average is what carries the estimate through a run of ones, where there are
//! no reversals, the envelope is flat, and the signal states no timing at all.
//!
//! ## What the frequency correction can and cannot reach
//!
//! The phase step between two symbols is the frequency error times the symbol
//! period, so the error is read off the same product the bit came from and costs
//! one arctangent per symbol. The sign of the symbol is folded out first, which
//! bounds the range to a quarter of the symbol rate: about eight hertz. That is
//! narrow and it is enough, because a signal thirty one hertz wide has to be
//! found to within a few hertz before any of this runs, and the peak search that
//! finds it resolves better than one hertz.

use crate::config::settings::PskSettings;
use crate::dsp::receiver::iq::Complex;

use super::tone::{amp_to_db, coefficient};
use super::varicode::{Alphabet, MAX_BITS};

/// Symbol rate, in symbols per second.
///
/// Exact rather than approximate: the format states it as two thousand over
/// sixty four, and every implementation on the air derives it the same way, so
/// there is nothing to estimate and nothing to track.
pub const BAUD: f32 = 31.25;

/// Matched filter evaluations per symbol.
///
/// Eight places the timing to within a sixteenth of a symbol before
/// interpolation and within a fiftieth after it, which costs a fraction of a
/// decibel against perfect timing. Sixteen would halve that and double the
/// arithmetic, which is the largest cost in the whole decoder.
const OVERSAMPLE: usize = 8;

/// Time constant of the timing estimate, in symbols.
///
/// Eight symbols is a quarter of a second, which is long enough that a run of
/// ones does not lose the estimate and short enough that a station arriving
/// mid transmission is read within one word.
const TIMING_SYMBOLS: f32 = 8.0;

/// Fraction of the frequency error corrected per symbol.
///
/// Slow, because the reading is one arctangent of one noisy product. A loop fast
/// enough to follow that noise would put the mixer somewhere different on every
/// symbol, and a mixer that moves is a mixer that reintroduces the very phase
/// error it is measuring.
const AFC_GAIN: f32 = 0.05;

/// Phase clustering a signal of pure noise produces.
///
/// The mean of the absolute cosine of a uniform phase, which is two over pi. It
/// is the floor of the measurement rather than nought, so the published figure
/// is rescaled against it: a reading of six tenths would otherwise look like a
/// signal decoding tolerably when it is an empty band.
const NOISE_CLUSTERING: f32 = 0.6366;

/// Time constant of the published averages, in symbols.
const STATS_SYMBOLS: f32 = 24.0;

/// What the interface reads out of the demodulator.
#[derive(Debug, Clone, Copy, Default)]
pub struct PskStats {
    pub level_db: f32,
    /// How tightly the phase clusters at nought and pi, nought to one.
    ///
    /// Rescaled against what noise produces, see the note on the floor. It says
    /// whether the signal is phase modulated at all, which is the one question a
    /// spectrum cannot answer.
    pub quality: f32,
    /// Share of accumulated codes that resolved to a character.
    ///
    /// The strongest evidence available, and independent of the above: the
    /// framing accepts any run of bits between two boundaries, so a code that
    /// resolves is a coincidence the alphabet had to permit.
    pub lock: f32,
    /// Correction the loop is applying, in hertz.
    pub afc_hz: f32,
    /// Frequency the demodulator is centred on, correction included.
    pub centre_hz: f32,
    pub characters: u64,
    /// Codes that resolved to nothing, which is what noise produces.
    pub rejects: u64,
}

pub struct PskDecoder {
    rate: u32,
    /// True when the input carries a quadrature pair.
    ///
    /// Decides two things. A real carrier mixed down leaves half its amplitude
    /// at baseband and the other half at twice the centre, so the scale below
    /// carries a factor of two that a complex one does not. And a negative
    /// centre frequency is a real position rather than a mirror, so the bound is
    /// two sided.
    complex: bool,
    /// Samples in one symbol, which is the matched filter length.
    window_len: usize,
    /// Cosine weighting over one symbol.
    window: Vec<f32>,
    /// Sum of the window, kept so the scale can be recomputed on a change of
    /// arrangement without rebuilding the table.
    window_sum: f32,
    /// Turns a filtered magnitude into an amplitude ratio, so a full scale
    /// carrier at the centre reads one.
    norm: f32,

    /// Baseband history.
    history: Vec<Complex>,
    pos: usize,
    /// Samples accumulated towards the next frame, fractional so the symbol rate
    /// does not have to divide the sample rate.
    frame_accum: f32,
    frame_hop: f32,

    /// Mixer.
    anchor_hz: f32,
    afc_hz: f32,
    phase: f64,
    phase_step: f64,

    /// Frame position inside the symbol, free running.
    ///
    /// One symbol per wrap, unconditionally. The sampling instant is a fractional
    /// offset read out of the frame ring rather than a frame the counter is
    /// steered onto, which is what stops a jittering estimate from taking two
    /// symbols in one period or none.
    sub: usize,
    /// Ring of filter outputs, longer than a symbol so the interpolation always
    /// has two neighbours that are adjacent in time.
    frames: Vec<Complex>,
    frame_pos: usize,
    /// Envelope component at the symbol rate. Its argument is the sampling phase.
    timing: Complex,
    timing_alpha: f32,

    previous: Complex,
    /// False until one symbol has been taken, so the first difference is not
    /// read against an empty reference.
    primed: bool,

    /// Accumulated code, and the run of noughts that ends it.
    bits: u16,
    len: u32,
    zeros: u32,

    alphabet: Alphabet,

    level: f32,
    clustering: f32,
    lock: f32,
    stats: PskStats,
    out: String,
}

impl PskDecoder {
    pub fn new(rate: u32, cfg: &PskSettings) -> PskDecoder {
        let fs = rate.max(1) as f32;
        // One symbol, which is the pulse the transmitter shaped. Bounded so an
        // unusual rate cannot ask for an evaluation that dominates the frame.
        let window_len = ((fs / BAUD).round() as usize).clamp(16, 8192);

        let mut window = vec![0.0f32; window_len];
        let denom = (window_len - 1).max(1) as f32;
        for (i, slot) in window.iter_mut().enumerate() {
            let t = i as f32 / denom;
            *slot = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
        }
        let window_sum: f32 = window.iter().sum();
        // Twice the reciprocal, because mixing a real carrier down puts half its
        // amplitude at baseband and the other half at twice the centre, where the
        // filter rejects it. A complex carrier leaves all of it at baseband, so
        // the factor is dropped when the arrangement changes.
        let norm = if window_sum > 1e-9 { 2.0 / window_sum } else { 1.0 };

        let frame_hop = fs / (BAUD * OVERSAMPLE as f32);
        let anchor = cfg.centre_hz.clamp(50.0, fs * 0.45);

        crate::log_info!(
            "decode",
            "psk detector at {:.0} Hz, {:.2} baud, window {} samples, {:.1} frames per symbol",
            anchor,
            BAUD,
            window_len,
            OVERSAMPLE as f32
        );

        PskDecoder {
            rate,
            complex: false,
            window_len,
            window,
            window_sum,
            norm,
            history: vec![Complex::default(); window_len],
            pos: 0,
            frame_accum: 0.0,
            frame_hop,
            anchor_hz: anchor,
            afc_hz: 0.0,
            phase: 0.0,
            phase_step: std::f64::consts::TAU * anchor as f64 / fs as f64,
            sub: 0,
            frames: vec![Complex::default(); OVERSAMPLE * 2],
            frame_pos: 0,
            timing: Complex::default(),
            timing_alpha: (1.0 / (TIMING_SYMBOLS * OVERSAMPLE as f32)).clamp(1e-4, 1.0),
            previous: Complex::new(1.0, 0.0),
            primed: false,
            bits: 0,
            len: 0,
            zeros: 0,
            alphabet: Alphabet::new(),
            level: 0.0,
            clustering: NOISE_CLUSTERING,
            lock: 0.0,
            stats: PskStats::default(),
            out: String::with_capacity(64),
        }
    }

    /// Frequency the demodulator is centred on, correction included.
    pub fn centre_hz(&self) -> f32 {
        self.anchor_hz + self.afc_hz
    }

    /// Frequency it was pointed at, before any correction.
    pub fn anchor_hz(&self) -> f32 {
        self.anchor_hz
    }

    pub fn stats(&self) -> PskStats {
        self.stats
    }

    /// Points the demodulator at a frequency.
    ///
    /// A move small enough to stay inside the signal is a correction of the same
    /// station and the accumulated state describes it still. A larger one means
    /// another station, and the framing position, the timing phase and the
    /// correction all refer to the one that has been left.
    /// States whether the input carries a quadrature pair.
    pub fn set_complex(&mut self, complex: bool) {
        if complex == self.complex {
            return;
        }
        self.complex = complex;
        let factor = if complex { 1.0 } else { 2.0 };
        self.norm = if self.window_sum > 1e-9 {
            factor / self.window_sum
        } else {
            1.0
        };
        // The centre was clamped under the other arrangement, so it is reapplied
        // rather than left where the bound pushed it.
        let held = self.anchor_hz;
        self.anchor_hz = 0.0;
        self.set_centre(held);
        self.reset();
    }

    pub fn set_centre(&mut self, hz: f32) {
        let limit = self.rate as f32 * 0.45;
        let floor = if self.complex { -limit } else { 50.0 };
        let wanted = hz.clamp(floor, limit);
        let moved = (wanted - self.anchor_hz).abs();
        if moved < 0.5 {
            return;
        }
        self.anchor_hz = wanted;
        // Measured against the old anchor, so it says nothing about the new one.
        self.afc_hz = 0.0;
        self.retune();

        // Half the signal width. Inside it the estimates still describe what is
        // being received; outside it they describe something else.
        if moved > BAUD * 0.5 {
            self.bits = 0;
            self.len = 0;
            self.zeros = 0;
            self.primed = false;
            self.timing = Complex::default();
        }
    }

    fn retune(&mut self) {
        self.phase_step =
            std::f64::consts::TAU * self.centre_hz() as f64 / self.rate.max(1) as f64;
    }

    pub fn feed(&mut self, input: &[Complex], cfg: &PskSettings) {
        if !cfg.enabled {
            return;
        }
        for &sample in input {
            // Mixed down to baseband. The phase is wrapped rather than left to
            // grow, so a long session does not lose precision in the sine.
            self.phase -= self.phase_step;
            if self.phase < -std::f64::consts::TAU {
                self.phase += std::f64::consts::TAU;
            }
            let (s, c) = (self.phase.sin() as f32, self.phase.cos() as f32);
            // One complex multiply, which for a real input reduces to the two
            // products the one sided form performed: the quadrature part is
            // nought and contributes nothing to either output.
            self.history[self.pos] = sample.mul(Complex::new(c, s));
            self.pos = (self.pos + 1) % self.window_len;

            self.frame_accum += 1.0;
            if self.frame_accum < self.frame_hop {
                continue;
            }
            self.frame_accum -= self.frame_hop;
            self.on_frame(cfg);
        }
    }

    pub fn drain(&mut self, dst: &mut String) {
        if !self.out.is_empty() {
            dst.push_str(&self.out);
            self.out.clear();
        }
    }

    pub fn reset(&mut self) {
        for slot in self.history.iter_mut() {
            *slot = Complex::default();
        }
        for slot in self.frames.iter_mut() {
            *slot = Complex::default();
        }
        self.pos = 0;
        self.frame_pos = 0;
        self.frame_accum = 0.0;
        self.sub = 0;
        self.timing = Complex::default();
        self.previous = Complex::new(1.0, 0.0);
        self.primed = false;
        self.bits = 0;
        self.len = 0;
        self.zeros = 0;
        // The correction is kept. It describes where the station is rather than
        // anything about the audio that has just been interrupted, and
        // discarding it would make every restart pay the acquisition again.
    }

    /// Matched filter over the most recent symbol.
    fn matched(&self) -> Complex {
        let n = self.window_len;
        let mut re = 0.0f32;
        let mut im = 0.0f32;
        for k in 0..n {
            let at = (self.pos + n - 1 - k) % n;
            let w = self.window[k];
            re += self.history[at].re * w;
            im += self.history[at].im * w;
        }
        Complex::new(re * self.norm, im * self.norm)
    }

    /// One filter evaluation and, once a symbol is complete, one bit.
    fn on_frame(&mut self, cfg: &PskSettings) {
        let y = self.matched();
        let magnitude = y.magnitude();

        // The envelope at the symbol rate. Its argument is where inside the
        // symbol the sampling instant lies, so nothing has to be steered.
        let theta = std::f32::consts::TAU * self.sub as f32 / OVERSAMPLE as f32;
        let alpha = self.timing_alpha;
        self.timing.re += alpha * (magnitude * theta.cos() - self.timing.re);
        self.timing.im += alpha * (magnitude * theta.sin() - self.timing.im);

        self.frames[self.frame_pos] = y;
        self.frame_pos = (self.frame_pos + 1) % self.frames.len();

        self.sub += 1;
        if self.sub < OVERSAMPLE {
            return;
        }
        self.sub = 0;
        self.on_symbol(cfg);
    }

    /// Reads the filter output at the sampling instant.
    ///
    /// The instant is a fractional position inside the symbol that has just
    /// finished, so it is counted backwards from the newest frame. That way the
    /// two frames the interpolation uses are always adjacent in time, including
    /// where the position falls at either end of the symbol: a ring indexed
    /// forwards would there interpolate between the two ends of the symbol,
    /// which are eight frames apart.
    fn sample_at(&self, position: f32) -> Complex {
        let back = OVERSAMPLE as f32 - position;
        let whole = back.floor().max(0.0);
        let frac = back - whole;
        let a = self.frame_back(whole as usize);
        let b = self.frame_back(whole as usize + 1);
        Complex::new(
            a.re * (1.0 - frac) + b.re * frac,
            a.im * (1.0 - frac) + b.im * frac,
        )
    }

    /// Filter output a number of frames ago, nought being the newest.
    fn frame_back(&self, k: usize) -> Complex {
        let n = self.frames.len();
        let at = (self.frame_pos + n - 1 - k.min(n - 1)) % n;
        self.frames[at]
    }

    fn on_symbol(&mut self, cfg: &PskSettings) {
        // Sampling phase, as a fractional frame position inside the symbol.
        let phase = self.timing.im.atan2(self.timing.re);
        let position = (phase / std::f32::consts::TAU * OVERSAMPLE as f32)
            .rem_euclid(OVERSAMPLE as f32);
        let y = self.sample_at(position);

        let magnitude = y.magnitude();
        let smoothing = 1.0 / STATS_SYMBOLS;
        self.level += smoothing * (magnitude - self.level);
        self.stats.level_db = amp_to_db(self.level);
        self.stats.centre_hz = self.centre_hz();
        self.stats.afc_hz = self.afc_hz;

        if !self.primed {
            self.previous = y;
            self.primed = true;
            return;
        }

        // The difference. A reversal is a nought and its absence is a one, which
        // is the encoding rather than a convention chosen here.
        let d = y.mul_conj(self.previous);
        self.previous = y;
        let scale = d.magnitude();
        if scale < 1e-12 {
            return;
        }

        // How tightly the phase sits at nought or pi. Rescaled against what
        // noise produces, so the published figure reads as nought on an empty
        // band rather than as six tenths.
        let clustered = (d.re.abs() / scale).clamp(0.0, 1.0);
        self.clustering += smoothing * (clustered - self.clustering);
        self.stats.quality = ((self.clustering - NOISE_CLUSTERING)
            / (1.0 - NOISE_CLUSTERING))
            .clamp(0.0, 1.0);

        // Frequency correction. The sign of the symbol is folded out first, which
        // is what bounds the range to a quarter of the symbol rate.
        if cfg.afc {
            let folded = if d.re < 0.0 { Complex::new(-d.re, -d.im) } else { d };
            let error = folded.im.atan2(folded.re);
            let hz = error / std::f32::consts::TAU * BAUD;
            let limit = cfg.afc_range_hz.clamp(1.0, BAUD * 0.25);
            self.afc_hz = (self.afc_hz + AFC_GAIN * hz).clamp(-limit, limit);
            self.retune();
        }

        // Held shut below the stated level. Above it every symbol produces a bit
        // whether or not there is a signal, and the framing then assembles noise
        // into codes that occasionally resolve.
        if self.stats.level_db < cfg.squelch_db {
            self.bits = 0;
            self.len = 0;
            self.zeros = 0;
            return;
        }

        let bit = d.re > 0.0;
        self.accept(bit);
    }

    /// Folds one bit into the framing.
    ///
    /// A nought is tentatively part of the code, because a single nought is
    /// legitimate inside one and only a pair ends it. The tentative one is taken
    /// back when the second arrives, which is the whole of the framing: no
    /// counter, no length field and nothing to resynchronize.
    fn accept(&mut self, bit: bool) {
        if bit {
            self.zeros = 0;
            self.bits = (self.bits << 1) | 1;
            self.len += 1;
            // Longer than the longest code, so this is noise being assembled
            // rather than a character arriving.
            if self.len > MAX_BITS {
                self.bits = 0;
                self.len = 0;
            }
            return;
        }

        self.zeros += 1;
        if self.zeros == 1 {
            self.bits <<= 1;
            self.len += 1;
            if self.len > MAX_BITS + 1 {
                self.bits = 0;
                self.len = 0;
            }
            return;
        }

        // The boundary. The tentative nought is removed and what is left is the
        // code, which needs at least one bit to be one.
        if self.len >= 2 {
            self.len -= 1;
            self.bits >>= 1;
            self.emit();
        }
        self.bits = 0;
        self.len = 0;
    }

    fn emit(&mut self) {
        let resolved = self.alphabet.decode(self.bits);
        let smoothing = 1.0 / STATS_SYMBOLS;
        self.lock += smoothing * (f32::from(resolved.is_some()) - self.lock);
        self.stats.lock = self.lock.clamp(0.0, 1.0);

        match resolved {
            Some(ch) => {
                // A carriage return on its own would overwrite the line in a
                // view that honours it, so only the line feed is kept.
                if ch != '\r' {
                    self.out.push(ch);
                }
                self.stats.characters += 1;
            }
            None => self.stats.rejects += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::varicode::code_of;

    /// Symbols of idle sent before and after the text.
    ///
    /// Idle is a continuous run of noughts, so every symbol is a reversal and the
    /// envelope carries its clock at full strength: the timing estimate settles
    /// within its own time constant. Sixty four symbols is two seconds, which is
    /// what a transmitting station sends before its call for exactly this reason.
    const IDLE: usize = 64;

    /// Builds a transmission.
    ///
    /// The whole point of having this. Every stage of the decoder can be wrong in
    /// a way that still produces plausible output, and the only test that catches
    /// that is the round trip: text in, the same text out, through the shaping
    /// and the mixing a real transmitter applies.
    fn transmit(text: &str, rate: u32, tone_hz: f32, noise: f32) -> Vec<f32> {
        let mut bits: Vec<bool> = Vec::with_capacity(text.len() * 8 + IDLE * 2);
        for _ in 0..IDLE {
            bits.push(false);
        }
        for ch in text.chars() {
            let code = code_of(ch).unwrap_or_else(|| panic!("{:?} is not in the alphabet", ch));
            for byte in code.bytes() {
                bits.push(byte == b'1');
            }
            // The boundary. Sent after every character rather than between two,
            // so the last one is delimited as well.
            bits.push(false);
            bits.push(false);
        }
        for _ in 0..IDLE {
            bits.push(false);
        }

        // Differential encoding. A nought reverses and a one holds, which is what
        // the decoder reads back out of the product of two symbols.
        let mut symbols: Vec<f32> = Vec::with_capacity(bits.len() + 1);
        let mut state = 1.0f32;
        symbols.push(state);
        for &bit in &bits {
            if !bit {
                state = -state;
            }
            symbols.push(state);
        }

        // Raised cosine pulses, one per symbol, each spanning two symbol periods.
        // Adjacent pulses overlap so the sum is the symbol value at every
        // sampling instant and reaches nought between two of opposite sign, which
        // is the envelope the timing recovery reads.
        let per_symbol = rate as f64 / BAUD as f64;
        let total = ((symbols.len() as f64 + 2.0) * per_symbol) as usize;
        let mut out = vec![0.0f32; total];
        for (k, &value) in symbols.iter().enumerate() {
            let centre = (k as f64 + 1.0) * per_symbol;
            let from = (centre - per_symbol).ceil().max(0.0) as usize;
            let to = ((centre + per_symbol).floor() as usize).min(total - 1);
            for i in from..=to {
                let t = (i as f64 - centre) / per_symbol;
                if t.abs() >= 1.0 {
                    continue;
                }
                let shape = 0.5 * (1.0 + (std::f64::consts::PI * t).cos());
                out[i] += value * shape as f32;
            }
        }

        // Onto the carrier. The baseband is real, so this is one multiply; the
        // mixer in the decoder starts at an arbitrary phase and the differential
        // detection is what makes that irrelevant.
        for (i, sample) in out.iter_mut().enumerate() {
            let phase = std::f64::consts::TAU * tone_hz as f64 * i as f64 / rate as f64;
            *sample *= phase.cos() as f32;
        }

        if noise > 0.0 {
            // Uniform rather than Gaussian, and stated as such: this asks whether
            // the chain survives interference rather than measuring a sensitivity,
            // and a generator written here would be one more thing to be wrong.
            let mut state = 0x2545_F491_4F6C_DD1Du64;
            for sample in out.iter_mut() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let uniform = (state >> 11) as f32 / (1u64 << 53) as f32 - 0.5;
                *sample += uniform * noise;
            }
        }
        out
    }

    /// Settings pointed at a stated centre.
    ///
    /// A parameter rather than a constant, because the demodulator is a matched
    /// filter one symbol wide and the correction reaches eight hertz. A test
    /// transmitting anywhere else measures the rejection of that filter rather
    /// than the decoder behind it, and reads noise assembled into codes.
    fn settings(centre_hz: f32) -> PskSettings {
        PskSettings {
            enabled: true,
            centre_hz,
            auto_centre: false,
            afc: true,
            afc_range_hz: 8.0,
            // Open, because the generator produces a known amplitude and a
            // threshold here would be testing the threshold.
            squelch_db: -200.0,
            print_threshold: 0.0,
        }
    }

    fn decode(audio: &[f32], rate: u32, cfg: &PskSettings) -> String {
        let mut decoder = PskDecoder::new(rate, cfg);
        let mut out = String::new();
        // A real input, which is what a transmitter puts on the air and what a
        // transceiver hands back: the quadrature part is silent and the
        // arrangement stays one sided.
        let samples: Vec<Complex> = audio.iter().map(|&v| Complex::new(v, 0.0)).collect();
        // Fed in blocks, because that is how it is fed in the application and a
        // decoder that only works on one long slice would pass a test and fail in
        // service.
        for block in samples.chunks(1024) {
            decoder.feed(block, cfg);
            decoder.drain(&mut out);
        }
        out
    }

    #[test]
    fn a_transmission_decodes_back_to_its_text() {
        let cfg = settings(1000.0);
        let text = "CQ CQ DE RA0FF K";
        let audio = transmit(text, 12_000, 1000.0, 0.0);
        let out = decode(&audio, 12_000, &cfg);
        assert!(out.contains(text), "decoded {:?}", out);
    }

    #[test]
    fn the_shortest_and_the_longest_codes_both_survive() {
        // A space is one bit and a question mark is ten, so this exercises both
        // ends of the alphabet and the framing between them.
        let cfg = settings(1200.0);
        let text = "e  ?zq e";
        let audio = transmit(text, 12_000, 1200.0, 0.0);
        let out = decode(&audio, 12_000, &cfg);
        assert!(out.contains(text), "decoded {:?}", out);
    }

    #[test]
    fn the_rate_need_not_divide_the_symbol_rate() {
        // Forty four thousand one hundred over thirty one and a quarter is not a
        // whole number, which is the case the fractional frame accumulator exists
        // for and the one every sound card actually delivers.
        let cfg = settings(1500.0);
        let text = "the quick brown fox";
        let audio = transmit(text, 44_100, 1500.0, 0.0);
        let out = decode(&audio, 44_100, &cfg);
        assert!(out.contains(text), "decoded {:?}", out);
    }

    #[test]
    fn a_frequency_error_is_corrected() {
        // Sent four hertz away from where the decoder is pointed, which is more
        // than an eighth of the signal width and well beyond what the framing
        // survives uncorrected.
        let cfg = settings(1000.0);
        let text = "test de rr";
        let audio = transmit(text, 12_000, 1004.0, 0.0);
        let out = decode(&audio, 12_000, &cfg);
        assert!(out.contains(text), "decoded {:?}", out);
    }

    #[test]
    fn interference_costs_characters_rather_than_the_transmission() {
        let cfg = settings(1000.0);
        let text = "cq cq de ra0ff ra0ff k";
        let audio = transmit(text, 12_000, 1000.0, 0.5);
        let out = decode(&audio, 12_000, &cfg);
        // Not the whole string: at this level of interference some characters are
        // lost, and demanding all of them would be demanding a sensitivity rather
        // than a working chain.
        assert!(out.contains("ra0ff"), "decoded {:?}", out);
    }

    #[test]
    fn an_empty_band_produces_no_text_and_says_so() {
        // The failure worth guarding: the framing accepts any run of bits between
        // two boundaries, so noise assembles codes and a few of them resolve. The
        // published figures are what an operator judges that by, and they have to
        // read as nothing rather than as a weak signal.
        let cfg = settings(1000.0);
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut audio = vec![0.0f32; 12_000 * 3];
        for sample in audio.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *sample = (state >> 11) as f32 / (1u64 << 53) as f32 - 0.5;
        }

        let samples: Vec<Complex> = audio.iter().map(|&v| Complex::new(v, 0.0)).collect();
        let mut decoder = PskDecoder::new(12_000, &cfg);
        decoder.feed(&samples, &cfg);
        let stats = decoder.stats();

        assert!(stats.quality < 0.35, "noise clustered at {:.2}", stats.quality);
        assert!(stats.lock < 0.5, "noise resolved at {:.2}", stats.lock);
    }
}