//! Narrow band tone amplitude estimation.
//!
//! Both decoders need the amplitude of one or two known frequencies at a rate
//! far below the sample rate: the keying detector wants an envelope a few times
//! faster than the shortest element, the frequency shift demodulator wants a
//! matched filter output around eight times the baud rate. A short time
//! transform would deliver that as well, but a Goertzel evaluation costs one
//! multiply and two adds per sample per tone, which is an order of magnitude
//! less work than a transform whose bins are almost all discarded.
//!
//! ## Why the input is complex
//!
//! A quadrature receiver produces a spectrum with two distinguishable halves,
//! and the half below the tuning point is where half the band is. Reducing the
//! pair to one channel before the detectors would throw that distinction away:
//! the spectrum of a real signal is symmetric about nought, so a station ten
//! kilohertz below the dial arrives in the detector at plus ten kilohertz,
//! superimposed on whatever is there. Two stations then share one detector, the
//! display draws the channel on the wrong side of the dial, and no setting can
//! separate them afterwards.
//!
//! So the bank takes a complex sample and evaluates a signed frequency. A real
//! input is handed over with a silent quadrature part, and the arithmetic then
//! reduces exactly to the one sided form: the closed expression below collapses
//! to the familiar power identity when the imaginary state stays at nought, so
//! there is one code path and no second thing to keep in step.
//!
//! The two arrangements differ only in scale. Mixing a real carrier produces
//! half its amplitude at the bin and half at the mirror; a complex one produces
//! all of it at the bin. The reciprocal of the window sum therefore carries a
//! factor of two in the real case and not in the complex one, which is what
//! keeps a full scale tone reading unity in both.
//!
//! The analysis window is Hann. A rectangular window leaks a strong neighbour
//! into the estimate through a first sidelobe only thirteen decibels down; the
//! Hann sidelobe is thirty one decibels down, and the resolution loss does not
//! matter for a filter this short.

use crate::dsp::receiver::iq::Complex;

/// Sliding bank of single frequency estimators sharing one window and one hop.
pub struct ToneBank {
    rate: u32,
    /// Window length in samples, which sets the analysis bandwidth.
    n: usize,
    /// Samples consumed per output frame.
    hop: usize,
    window: Vec<f32>,
    /// Sum of the window, kept so the scale can be recomputed when the
    /// arrangement changes without rebuilding the table.
    window_sum: f32,
    /// Turns a bin magnitude into the amplitude of the tone that produced it.
    norm: f32,
    /// True when the input carries a quadrature pair.
    complex: bool,
    /// Two times the cosine of the normalized angular frequency, per tone.
    coeffs: Vec<f32>,
    /// Cosine and sine of the same angle, for the closing multiply.
    ///
    /// The recurrence alone cannot tell a positive frequency from a negative
    /// one, because it holds only the cosine and the cosine is even. The sine
    /// enters through the term that turns the two state variables into the
    /// transform value, and it is odd, so that is where the sign lives.
    cosines: Vec<f32>,
    sines: Vec<f32>,
    freqs: Vec<f32>,
    fifo: Vec<Complex>,
    amps: Vec<f32>,
}

impl ToneBank {
    pub fn new(rate: u32, n: usize, hop: usize, freqs: &[f32]) -> ToneBank {
        let n = n.clamp(8, 8192);
        let hop = hop.clamp(1, n);

        let mut window = vec![0.0f32; n];
        let denom = (n - 1).max(1) as f32;
        for (i, slot) in window.iter_mut().enumerate() {
            let t = i as f32 / denom;
            *slot = 0.5 - 0.5 * (std::f32::consts::TAU * t).cos();
        }
        let window_sum: f32 = window.iter().sum();

        let mut bank = ToneBank {
            rate,
            n,
            hop,
            window,
            window_sum,
            norm: 1.0,
            complex: false,
            coeffs: Vec::new(),
            cosines: Vec::new(),
            sines: Vec::new(),
            freqs: Vec::new(),
            fifo: Vec::with_capacity(n * 4),
            amps: Vec::new(),
        };
        bank.rescale();
        bank.set_frequencies(freqs);
        bank
    }

    /// States whether the input carries a quadrature pair.
    ///
    /// The frequencies are reapplied, because a negative one stated while the
    /// input was real was clamped away and has to be recovered rather than
    /// silently kept at the bound it was pushed to.
    pub fn set_complex(&mut self, complex: bool) {
        if complex == self.complex {
            return;
        }
        self.complex = complex;
        self.rescale();
        let held: Vec<f32> = self.freqs.clone();
        self.set_frequencies(&held);
        // Nothing measured in one arrangement describes the other: the same
        // buffered samples mean a different spectrum once the second channel
        // starts carrying information.
        self.reset();
    }

    pub fn is_complex(&self) -> bool {
        self.complex
    }

    fn rescale(&mut self) {
        let factor = if self.complex { 1.0 } else { 2.0 };
        self.norm = if self.window_sum > 1e-9 {
            factor / self.window_sum
        } else {
            1.0
        };
    }

    /// Retunes without discarding the buffered samples. Cheap enough to call
    /// whenever the automatic tone search moves.
    pub fn set_frequencies(&mut self, freqs: &[f32]) {
        self.freqs.clear();
        self.coeffs.clear();
        self.cosines.clear();
        self.sines.clear();

        // A tone at or above the Nyquist frequency would alias onto a lower one.
        // Below nought is a real position on a two sided spectrum and a mirror
        // on a one sided one, so it is admitted only in the first case.
        let limit = self.rate as f32 * 0.49;
        let floor = if self.complex { -limit } else { 1.0 };

        for &f in freqs {
            let clamped = f.clamp(floor, limit);
            let k = clamped / self.rate as f32;
            let w = std::f32::consts::TAU * k;
            self.freqs.push(clamped);
            self.coeffs.push(2.0 * w.cos());
            self.cosines.push(w.cos());
            self.sines.push(w.sin());
        }
        self.amps.clear();
        self.amps.resize(self.freqs.len(), 0.0);
    }

    pub fn frequencies(&self) -> &[f32] {
        &self.freqs
    }

    pub fn window_len(&self) -> usize {
        self.n
    }

    /// Output frames per second.
    pub fn frame_rate(&self) -> f32 {
        self.rate as f32 / self.hop as f32
    }

    /// Seconds represented by one output frame.
    pub fn frame_seconds(&self) -> f32 {
        self.hop as f32 / self.rate as f32
    }

    /// Nominal bandwidth of the estimator, taken as the main lobe width of the
    /// window. Used only for reporting.
    pub fn bandwidth_hz(&self) -> f32 {
        4.0 * self.rate as f32 / self.n as f32
    }

    pub fn feed(&mut self, input: &[Complex]) {
        if input.is_empty() {
            return;
        }
        self.fifo.extend_from_slice(input);

        // A backlog can only appear if the caller stops draining. Trimming
        // keeps the decoder aligned with real time rather than replaying old
        // audio, which would corrupt every timing estimate downstream.
        let limit = self.n + self.hop * 256;
        if self.fifo.len() > limit {
            let excess = self.fifo.len() - limit;
            self.fifo.drain(..excess);
        }
    }

    /// Evaluates one frame and consumes one hop. Returns false when there is
    /// not enough data yet.
    pub fn next_frame(&mut self) -> bool {
        if self.fifo.len() < self.n {
            return false;
        }

        for t in 0..self.coeffs.len() {
            let coeff = self.coeffs[t];
            let cw = self.cosines[t];
            let sw = self.sines[t];

            // The transform value is the pair of state variables closed with one
            // rotation: X equals the newer state less the older one turned by
            // the tone angle. The real branch is written out rather than fed
            // through the complex one, because half the multiplies there would
            // be against a quadrature part that is always nought.
            let (xre, xim) = if self.complex {
                let mut s1r = 0.0f32;
                let mut s2r = 0.0f32;
                let mut s1i = 0.0f32;
                let mut s2i = 0.0f32;
                for i in 0..self.n {
                    let w = self.window[i];
                    let z = self.fifo[i];
                    let sr = coeff * s1r - s2r + z.re * w;
                    let si = coeff * s1i - s2i + z.im * w;
                    s2r = s1r;
                    s1r = sr;
                    s2i = s1i;
                    s1i = si;
                }
                (s1r - cw * s2r - sw * s2i, s1i - cw * s2i + sw * s2r)
            } else {
                let mut s1 = 0.0f32;
                let mut s2 = 0.0f32;
                for i in 0..self.n {
                    let s = coeff * s1 - s2 + self.fifo[i].re * self.window[i];
                    s2 = s1;
                    s1 = s;
                }
                (s1 - cw * s2, sw * s2)
            };

            self.amps[t] = (xre * xre + xim * xim).max(0.0).sqrt() * self.norm;
        }

        self.fifo.drain(..self.hop);
        true
    }

    pub fn amps(&self) -> &[f32] {
        &self.amps
    }

    pub fn reset(&mut self) {
        self.fifo.clear();
        for a in self.amps.iter_mut() {
            *a = 0.0;
        }
    }
}

/// Amplitude ratio to decibels with a floor that keeps the result finite for a
/// silent buffer.
pub fn amp_to_db(a: f32) -> f32 {
    if a <= 1e-9 {
        -180.0
    } else {
        20.0 * a.log10()
    }
}

/// One pole smoothing coefficient for a time constant expressed in seconds.
pub fn coefficient(tau_s: f32, dt: f32) -> f32 {
    if tau_s <= 1e-6 {
        return 1.0;
    }
    (1.0 - (-dt / tau_s).exp()).clamp(1e-6, 1.0)
}