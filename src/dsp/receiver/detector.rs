//! Demodulators.
//!
//! Each takes the baseband the filter produced and returns one real sample.
//!
//! ## What arrives here, and what has to be undone
//!
//! The filter mixed down by the tuning point plus the middle of the passband,
//! so the sample is centred on the middle of the passband and not on the thing
//! being received. Every detector begins by putting the middle back, which
//! leaves the signal referenced to the tuning point: the audio a detector
//! produces is the offset from what the operator tuned to.
//!
//! Doing that first, for every mode, is what makes an asymmetric passband
//! usable. A carrier detector needs the carrier at nought, and with a symmetric
//! passband the middle is nought so the two coincide by accident; shift the
//! passband and the carrier sits at the shift, where an envelope detector turns
//! it into a beat at that frequency. Restoring the middle removes the accident.
//!
//! ## Where the sideband is selected
//!
//! Not here. The complex bandpass upstream has independent edges and is the only
//! stage that can tell the two halves of a spectrum apart, so the sideband is
//! decided by where its passband was placed: above the tuning point for the
//! upper, below for the lower. Everything after it works on one band that has
//! already been isolated.
//!
//! That is why the two sideband selections share one code path. An earlier
//! arrangement conjugated the signal for the lower one, which does not select
//! anything: it mirrors, so a tone one kilohertz above the carrier emerged at
//! two kilohertz. Taking the real part of a correctly filtered baseband gives
//! the right mapping for either half without a special case, because a band
//! below nought unfolds with its offsets ascending, which is what the lower
//! sideband means.
//!
//! On a real input the analytic signal has no negative half, so the lower
//! sideband preset stays above the tuning point and the two selections coincide.
//! That is not a limitation being worked around: a transceiver delivering audio
//! has already chosen the sideband, and no processing downstream can revisit it.
//!
//! ## Why synchronous detection is worth its complexity
//!
//! An envelope detector on a fading path reproduces the fade, because the
//! envelope is what fades. A synchronous detector recovers the carrier with a
//! loop whose bandwidth is a few hertz, and a loop that slow does not follow a
//! fade: the recovered carrier stays put while the sidebands move around it.
//! The result is a signal that stays readable through a fade an envelope
//! detector turns into mush, which is precisely the condition worth having a
//! second detector for.

use crate::config::settings::Detector as Kind;

use super::iq::Complex;

/// Loop bandwidth of the synchronous detector, in hertz.
///
/// Wide enough to acquire a carrier a few tens of hertz off and to follow the
/// drift of an ordinary transmitter, narrow enough that it does not follow the
/// sidebands and turn itself into an envelope detector.
const SAM_LOOP_HZ: f32 = 8.0;

/// Corner of the direct current blocker on the amplitude detectors.
///
/// An amplitude detector produces a large offset by construction, and an offset
/// is not audio. Twenty hertz removes it without touching the lowest note a
/// speech path carries.
const DC_CORNER_HZ: f32 = 20.0;

pub struct DetectorBank {
    kind: Kind,
    rate: f32,
    /// Distance from the tuning point to the middle of the passband, which the
    /// filter removed and this stage puts back.
    rel_centre_hz: f32,

    /// Mixer that puts it back.
    phase: f64,
    step: f64,

    /// Direct current blocker state for the amplitude detectors.
    dc_r: f32,
    dc_x1: f32,
    dc_y1: f32,

    /// Recovered carrier of the synchronous detector, and its loop.
    sam_phase: f64,
    sam_freq: f64,
    sam_alpha: f32,
    sam_beta: f32,
    /// True once the loop has settled, for a readout.
    sam_locked: bool,
    sam_error_avg: f32,

    /// Previous sample, for the frequency discriminator.
    previous: Complex,
    /// Scale that turns a phase step into a unit output.
    fm_scale: f32,
}

impl DetectorBank {
    pub fn new(rate: u32) -> DetectorBank {
        let fs = rate.max(1) as f32;
        // Critically damped second order loop. The two gains are the standard
        // pair for a damping factor of one over the square root of two, which is
        // the fastest settling that does not overshoot into a false lock.
        let w = std::f32::consts::TAU * SAM_LOOP_HZ / fs;
        DetectorBank {
            kind: Kind::Usb,
            rate: fs,
            rel_centre_hz: 0.0,
            phase: 0.0,
            step: 0.0,
            dc_r: (1.0 - std::f32::consts::TAU * DC_CORNER_HZ / fs).clamp(0.5, 0.9999),
            dc_x1: 0.0,
            dc_y1: 0.0,
            sam_phase: 0.0,
            sam_freq: 0.0,
            sam_alpha: 1.414 * w,
            sam_beta: w * w,
            sam_locked: false,
            sam_error_avg: 0.0,
            previous: Complex::new(1.0, 0.0),
            // A phase step of pi corresponds to the Nyquist frequency, so
            // dividing by pi puts full deviation at unity.
            fm_scale: 1.0 / std::f32::consts::PI,
        }
    }

    /// Sets the demodulator and the offset the filter removed.
    ///
    /// The offset is the distance from the tuning point to the middle of the
    /// passband, not the mixer frequency of the filter. The difference between
    /// the two is the tuning point itself, and leaving it out is what makes the
    /// audio a statement about the tuning rather than about the digitizer.
    ///
    /// A keyed mode on a complex input adds the beat oscillator to it, because
    /// its passband is symmetric about the tuning point and a signal sitting
    /// there would otherwise emerge at nought hertz. That is legitimate here
    /// and nowhere else: keying takes the real part, so a frequency shift of
    /// the whole band is exactly a beat oscillator, whereas an amplitude or a
    /// frequency detector needs its carrier at nought and would beat against
    /// the same shift.
    ///
    /// The value may be negative, which is the ordinary case for a lower
    /// sideband passband. Nothing here treats that specially.
    pub fn configure(&mut self, kind: Kind, offset_hz: f32) {
        if kind != self.kind {
            self.kind = kind;
            // A detector carries state that means nothing to the next one: a
            // recovered carrier is not a direct current offset.
            self.reset();
        }
        if (offset_hz - self.rel_centre_hz).abs() > 0.5 {
            self.rel_centre_hz = offset_hz;
            self.step = std::f64::consts::TAU * offset_hz as f64 / self.rate as f64;
        }
    }

    /// True while the synchronous detector holds a carrier.
    pub fn locked(&self) -> bool {
        self.sam_locked
    }

    /// Offset the synchronous loop is holding, in hertz. Reads as the tuning
    /// error against the station.
    pub fn carrier_offset_hz(&self) -> f32 {
        (self.sam_freq / std::f64::consts::TAU * self.rate as f64) as f32
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
        self.dc_x1 = 0.0;
        self.dc_y1 = 0.0;
        self.sam_phase = 0.0;
        self.sam_freq = 0.0;
        self.sam_locked = false;
        self.sam_error_avg = 0.0;
        self.previous = Complex::new(1.0, 0.0);
    }

    #[inline]
    pub fn sample(&mut self, z: Complex) -> f32 {
        let z = self.restore(z);
        match self.kind {
            // Every sideband selection is one operation. Which half was kept is
            // a property of the filter, and the data modes differ from plain
            // sideband only in the preset width.
            Kind::Cw | Kind::Usb | Kind::DigU | Kind::Lsb | Kind::DigL => z.re,
            Kind::Am => self.block_dc(z.magnitude()),
            Kind::Sam => self.synchronous(z),
            Kind::Fm => self.discriminate(z),
        }
    }

    /// Puts back the offset the filter removed, so the sample is referenced to
    /// the tuning point.
    #[inline]
    fn restore(&mut self, z: Complex) -> Complex {
        if self.step == 0.0 {
            return z;
        }
        self.phase += self.step;
        if self.phase > std::f64::consts::TAU {
            self.phase -= std::f64::consts::TAU;
        } else if self.phase < -std::f64::consts::TAU {
            self.phase += std::f64::consts::TAU;
        }
        let (s, c) = (self.phase.sin() as f32, self.phase.cos() as f32);
        z.mul(Complex::new(c, s))
    }

    /// Single pole high pass, the same shape the capture front end uses.
    #[inline]
    fn block_dc(&mut self, x: f32) -> f32 {
        let y = x - self.dc_x1 + self.dc_r * self.dc_y1;
        self.dc_x1 = x;
        self.dc_y1 = y;
        y
    }

    /// Synchronous amplitude detection.
    ///
    /// The loop drives the recovered carrier so that the signal, rotated by its
    /// conjugate, has no quadrature component. The in phase component is then
    /// the modulation plus the carrier, and the blocker removes the carrier.
    #[inline]
    fn synchronous(&mut self, z: Complex) -> f32 {
        let (s, c) = (self.sam_phase.sin() as f32, self.sam_phase.cos() as f32);
        let rotated = z.mul_conj(Complex::new(c, s));

        // The phase error, approximated by the quadrature component normalized
        // by the magnitude. Normalizing is what keeps the loop bandwidth
        // independent of the signal level, so a fade does not slow the loop down
        // exactly when it is needed most.
        let magnitude = rotated.magnitude().max(1e-9);
        let error = rotated.im / magnitude;

        self.sam_freq += (self.sam_beta * error) as f64;
        self.sam_phase += self.sam_freq + (self.sam_alpha * error) as f64;
        if self.sam_phase > std::f64::consts::TAU {
            self.sam_phase -= std::f64::consts::TAU;
        } else if self.sam_phase < 0.0 {
            self.sam_phase += std::f64::consts::TAU;
        }

        // Lock is judged from the mean square error over a second or so. A loop
        // chasing noise has an error near its full range; one that is locked has
        // an error near nought.
        self.sam_error_avg += 0.0005 * (error * error - self.sam_error_avg);
        self.sam_locked = self.sam_error_avg < 0.02;

        self.block_dc(rotated.re)
    }

    /// Frequency discriminator.
    ///
    /// The phase difference between two consecutive samples is the instantaneous
    /// frequency. Taken as the argument of the product with the conjugate, which
    /// needs one arctangent and no unwrapping: the result is already in the
    /// principal range by construction.
    #[inline]
    fn discriminate(&mut self, z: Complex) -> f32 {
        let d = z.mul_conj(self.previous);
        self.previous = z;
        if d.re.abs() < 1e-12 && d.im.abs() < 1e-12 {
            return 0.0;
        }
        d.im.atan2(d.re) * self.fm_scale
    }
}