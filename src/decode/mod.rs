//! Decoder bank.
//!
//! Owns the keying channel bank, the teleprinter demodulator, the classifier and
//! the text log. Runs on the interface thread, right after the spectrum: the
//! decoders need the same samples the display consumed, and the classifier needs
//! the spectrum the display produced.
//!
//! Cost is dominated by the tone banks. A keying channel evaluates about six
//! hundred thousand operations per second of audio and the teleprinter path is of
//! the same order, so a full bank stays two orders of magnitude below the frame
//! budget. No separate thread is warranted; adding one would only introduce a
//! second queue and a second source of latency.
//!
//! Two decisions are taken at different scopes and must not be confused. The
//! classifier decides what the band as a whole is carrying, which is what the
//! status line reports and what gates the teleprinter text. Each keying channel
//! decides for itself whether it is decoding real traffic, from its own pattern
//! match rate and element statistics. Gating the keying text globally would
//! silence every carrier as soon as one shifted pair dominated the peak list,
//! which is precisely the case multi channel operation exists to handle.

pub mod baudot;
pub mod channels;
pub mod classify;
pub mod fsk;
pub mod log;
pub mod morse;
pub mod psk;
pub mod tone;
pub mod varicode;

use crate::audio::convert::Converter;
use crate::config::Settings;
use crate::dsp::receiver::iq::Complex;

use channels::{ChannelInfo, CwChannelBank};
use classify::{Classifier, Estimate};
use fsk::FskDecoder;
use log::DecodeLog;
use psk::PskDecoder;

/// Phase clustering the text needs before it is printed.
///
/// Beside the framing rather than instead of it, because the two answer
/// different questions and only the pair separates a transmission from an empty
/// band: the framing says the bits form codes the alphabet holds, which noise
/// manages often, and the clustering says there is modulation to read them out
/// of. Stricter than the figure the classifier scores with, because printing is
/// the irreversible step: a page of punctuation is read as a receiver fault.
const PSK_MIN_QUALITY: f32 = 0.40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Unknown,
    Cw,
    Rtty,
    Navtex,
    Psk31,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Unknown => "----",
            Mode::Cw => "CW",
            Mode::Rtty => "RTTY",
            Mode::Navtex => "NAVTEX",
            Mode::Psk31 => "PSK31",
        }
    }

    /// Localization key. The short form returned by as_str is what goes into the
    /// transcript file, which must stay stable and in one language whatever the
    /// interface is set to.
    pub fn key(self) -> &'static str {
        match self {
            Mode::Unknown => "mode.none",
            Mode::Cw => "mode.cw",
            Mode::Rtty => "mode.rtty",
            Mode::Navtex => "mode.navtex",
            Mode::Psk31 => "mode.psk31",
        }
    }
}

/// Everything the interface reports about the decoders. The keying figures
/// describe the focused channel; the channel list carries the rest.
#[derive(Debug, Clone, Copy)]
pub struct DecoderStatus {
    pub mode: Mode,
    pub confidence: f32,
    pub wpm: f32,
    pub cw_snr_db: f32,
    pub cw_duty: f32,
    pub tone_hz: f32,
    pub mark_hz: f32,
    /// Frequency the focused detector is actually centred on.
    pub cw_tone_hz: f32,
    /// Correction the focused tracking loop is applying.
    pub cw_afc_hz: f32,
    pub shift_hz: f32,
    pub baud: f32,
    pub detected_baud: f32,
    pub fsk_lock: f32,
    pub fsk_level_db: f32,
    pub figures: bool,
    pub characters: u64,
    pub cw_singles: f32,
    pub framing_errors: u64,
    pub cw_quality: f32,
    /// Confidence of the focused channel alone.
    pub cw_confidence: f32,
    /// Condition holding the focused keying decision shut, if any.
    pub cw_gate: crate::decode::morse::Gate,
    pub cw_level_db: f32,
    /// Noise level the focused detector is measuring against.
    pub cw_floor_db: f32,
    /// Equivalent noise bandwidth of the keying detectors.
    pub cw_bandwidth_hz: f32,
    pub cw_channels: usize,
    pub cw_focus: u32,
    /// Observed gap between characters, in units.
    ///
    /// Three for an ordinary transmission and more for a Farnsworth one, which is
    /// what an operator needs to see: it says which of the two is arriving, and
    /// therefore which word boundary is being applied.
    pub cw_char_gap_units: f32,
    /// True while the automatic search holds the tone polarity reversed.
    pub fsk_auto_inverted: bool,
    /// Frequency the phase demodulator is centred on, correction included.
    pub psk_centre_hz: f32,
    pub psk_afc_hz: f32,
    pub psk_level_db: f32,
    /// How tightly the phase clusters, rescaled so noise reads as nought.
    pub psk_quality: f32,
    /// Share of codes that resolved to a character.
    pub psk_lock: f32,
    pub psk_characters: u64,
    pub psk_rejects: u64,
}

pub struct DecoderBank {
    cw: CwChannelBank,
    fsk: FskDecoder,
    psk: PskDecoder,
    classifier: Classifier,
    pub log: DecodeLog,
    rate: u32,
    /// True when the input carries a quadrature pair.
    complex: bool,
    /// Fingerprint of the settings the demodulators were built from.
    signature: u64,
    /// Complex view of the block being fed, so the conversion allocates once.
    iq: Vec<Complex>,
    /// Scratch string, so draining the teleprinter path does not allocate.
    scratch: String,
    /// Scratch identity list, for reconciling the log against the bank.
    ids: Vec<u32>,
    last_mode: Mode,
    /// Seconds since the last mode note. A change that oscillates would
    /// otherwise fill the log faster than the traffic it is annotating.
    note_cooldown: f32,
}

impl DecoderBank {
    pub fn new(settings: &Settings, rate: u32, complex: bool) -> DecoderBank {
        let transcript = if settings.log.transcript_enabled && !settings.log.transcript_path.is_empty()
        {
            Some(std::path::PathBuf::from(&settings.log.transcript_path))
        } else {
            None
        };

        let mut bank = DecoderBank {
            cw: CwChannelBank::new(rate, &settings.morse),
            fsk: FskDecoder::new(rate, &settings.rtty),
            psk: PskDecoder::new(rate, &settings.psk),
            classifier: Classifier::new(),
            log: DecodeLog::new(2000, transcript.as_deref()),
            rate,
            complex,
            signature: signature(settings, rate, complex),
            iq: Vec::with_capacity(4096),
            scratch: String::with_capacity(128),
            ids: Vec::with_capacity(channels::MAX_CHANNELS),
            last_mode: Mode::Unknown,
            note_cooldown: 0.0,
        };
        bank.apply_arrangement();
        bank
    }

    /// Tells every demodulator which arrangement it is in.
    ///
    /// Applied unconditionally rather than on a change, because a rebuild
    /// produces detectors that default to the one sided form and would otherwise
    /// fold the spectrum until the next time the setting moved.
    fn apply_arrangement(&mut self) {
        self.cw.set_complex(self.complex);
        self.fsk.set_complex(self.complex);
        self.psk.set_complex(self.complex);
    }

    pub fn estimate(&self) -> Estimate {
        self.classifier.estimate()
    }

    /// Decayed peak hold the channel allocator searches. Empty until the first
    /// analysis pass has run.
    pub fn held_spectrum(&self) -> &[f32] {
        self.classifier.held_spectrum()
    }

    /// Median of the held spectrum, the reference every peak is measured
    /// against.
    pub fn noise_floor_db(&self) -> f32 {
        self.classifier.noise_floor_db()
    }

    /// Fills the buffer with a view of every keying channel.
    pub fn channels(&self, out: &mut Vec<ChannelInfo>) {
        self.cw.snapshot(out);
    }

    pub fn status(&self) -> DecoderStatus {
        let cw = self.cw.focused_stats();
        let fk = self.fsk.stats();
        let pk = self.psk.stats();
        let est = self.classifier.estimate();
        DecoderStatus {
            mode: est.mode,
            confidence: est.confidence,
            wpm: self.cw.focused_wpm(),
            cw_snr_db: cw.snr_db,
            cw_duty: cw.duty,
            tone_hz: self.cw.focused_anchor_hz(),
            mark_hz: self.fsk.mark_hz(),
            cw_tone_hz: self.cw.focused_tone_hz(),
            cw_afc_hz: self.cw.focused_afc_hz(),
            shift_hz: self.fsk.shift_hz(),
            baud: self.fsk.baud(),
            detected_baud: fk.baud_estimate,
            fsk_lock: fk.lock,
            fsk_level_db: fk.level_db,
            figures: self.fsk.in_figures(),
            characters: fk.characters + cw.elements,
            framing_errors: fk.framing_errors + fk.parity_errors,
            cw_singles: cw.single_ratio,
            cw_quality: cw.quality,
            cw_confidence: classify::keying_confidence(&cw),
            cw_gate: cw.gate,
            cw_level_db: cw.level_db,
            cw_floor_db: cw.floor_db,
            cw_bandwidth_hz: self.cw.detector_bandwidth(),
            cw_channels: self.cw.len(),
            cw_focus: self.cw.focus(),
            cw_char_gap_units: cw.char_gap_units,
            fsk_auto_inverted: fk.auto_inverted,
            psk_centre_hz: pk.centre_hz,
            psk_afc_hz: pk.afc_hz,
            psk_level_db: pk.level_db,
            psk_quality: pk.quality,
            psk_lock: pk.lock,
            psk_characters: pk.characters,
            psk_rejects: pk.rejects,
        }
    }

    /// Rebuilds whatever the settings invalidated. Cheap enough per frame.
    pub fn sync(&mut self, settings: &Settings, rate: u32, complex: bool) {
        let sig = signature(settings, rate, complex);
        if sig != self.signature {
            self.cw.rebuild(rate, &settings.morse);
            self.fsk = FskDecoder::new(rate, &settings.rtty);
            self.psk = PskDecoder::new(rate, &settings.psk);
            self.signature = sig;
            self.rate = rate;
        }
        self.complex = complex;
        self.apply_arrangement();
    }

    /// Consumes one block at the decoder rate.
    ///
    /// The pair rather than its reduction, because a reduction is real and the
    /// spectrum of a real signal is symmetric about nought: on a quadrature
    /// input every station below the tuning point would arrive superimposed on
    /// whatever sits above it, one detector would hear two of them, and the
    /// display would draw the channel on the opposite side of the dial.
    pub fn feed(&mut self, frames: &[[f32; 2]], settings: &Settings) {
        if frames.is_empty() {
            return;
        }
        let dt = frames.len() as f32 / self.rate.max(1) as f32;
        let est = self.classifier.estimate();

        // The same correction the display applies, so a peak the classifier
        // found is a peak the detector can reach. Recomputed per block rather
        // than cached: two trigonometric calls against a few thousand samples.
        let mut samples = std::mem::take(&mut self.iq);
        samples.clear();
        samples.reserve(frames.len());
        if self.complex {
            let rx = &settings.receiver;
            let gain = 10.0f32.powf(rx.iq_gain_db.clamp(-12.0, 12.0) / 20.0);
            let phi = rx.iq_phase_deg.clamp(-45.0, 45.0).to_radians();
            let (sin_phi, cos_phi) = (phi.sin(), phi.cos().max(1e-3));
            for &frame in frames {
                let (i, q) = if rx.iq_swap {
                    (frame[1], frame[0])
                } else {
                    (frame[0], frame[1])
                };
                let q = q / gain;
                samples.push(Complex::new(i, (q - i * sin_phi) / cos_phi));
            }
        } else {
            for &frame in frames {
                let v = Converter::reduce(settings.audio.channel_mode, frame);
                samples.push(Complex::new(v, 0.0));
            }
        }
        let samples: &[Complex] = &samples;

        // Retuning is applied per block rather than through a rebuild, so a
        // manual setting takes effect immediately. With several channels the
        // frequency below only seeds an empty bank; allocation runs on the
        // spectrum pass instead.
        if settings.morse.enabled {
            // Validity is a flag rather than a magnitude, because on a two sided
            // spectrum the tuning point is nought and everything below it is a
            // real position: a test against fifty hertz abandoned half the band.
            let wanted = if settings.morse.auto_tone && est.tone_valid {
                est.tone_hz
            } else {
                settings.morse.tone_hz
            };
            self.cw.anchor(wanted, &settings.morse);
        }
        if settings.rtty.enabled {
            let mark = if settings.rtty.auto_shift && est.tone_valid {
                est.mark_hz
            } else {
                settings.rtty.mark_hz
            };
            let shift = if settings.rtty.auto_shift && est.shift_hz > 20.0 {
                est.shift_hz
            } else {
                settings.rtty.shift_hz
            };
            self.fsk.set_tones(mark, shift);
        }

        if settings.psk.enabled {
            // The peak search finds this format directly, because the signal is
            // a carrier rather than a tone being switched on and off: its own
            // envelope only dips at a reversal, so it looks like a station that
            // never stops transmitting.
            let wanted = if settings.psk.auto_centre && est.tone_valid {
                est.tone_hz
            } else {
                settings.psk.centre_hz
            };
            self.psk.set_centre(wanted);
        }

        if settings.morse.enabled {
            self.cw.feed(samples, &settings.morse);
        }
        self.fsk.feed(samples, &settings.rtty);
        self.psk.feed(samples, &settings.psk);

        // Keying text. The gate is per channel and comes from the keying section
        // rather than from the classifier: a channel that is not producing table
        // entries is not producing traffic, whatever the rest of the band looks
        // like, and the classifier threshold answers a different question at a
        // different scope.
        let auto = settings.classifier.enabled && settings.classifier.auto_switch_decoder;
        let floor = settings.morse.print_threshold;
        if settings.morse.enabled {
            let DecoderBank { cw, log, .. } = self;
            cw.drain(|id, hz, confidence, text| {
                if confidence >= floor {
                    log.push_text(id, hz, text, Mode::Cw);
                }
            });
        }

        // Text from a signal that occupies the whole passband rather than one
        // tone inside it. This stays under the global decision, because a shifted
        // pair and a reversing carrier are properties of the band rather than of
        // one carrier, and the framing lock is an independent check on top of it:
        // a pair of tones can look right while the stop bits never line up.
        //
        // One of the two at a time, whichever the mode names. They share a text
        // line because they are alternatives rather than companions: a band
        // carries a teleprinter signal or a phase modulated one, and interleaving
        // both into one line would destroy each of them.
        let wide_mode = if auto {
            let confident = est.confidence >= settings.classifier.min_confidence;
            if confident { est.mode } else { Mode::Unknown }
        } else if settings.rtty.enabled {
            Mode::Rtty
        } else if settings.psk.enabled {
            Mode::Psk31
        } else {
            Mode::Unknown
        };

        // Both are drained whichever is printed. A buffer nobody empties grows
        // for as long as the other decoder holds the mode, and the text it then
        // released would be minutes old.
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch.clear();
        self.fsk.drain(&mut scratch);
        if matches!(wide_mode, Mode::Rtty | Mode::Navtex) && self.fsk.stats().lock > 0.4 {
            self.log.push_text(0, self.fsk.mark_hz(), &scratch, wide_mode);
        }

        scratch.clear();
        self.psk.drain(&mut scratch);
        let pk = self.psk.stats();
        if wide_mode == Mode::Psk31
            && pk.lock >= settings.psk.print_threshold
            && pk.quality >= PSK_MIN_QUALITY
        {
            self.log.push_text(0, self.psk.centre_hz(), &scratch, Mode::Psk31);
        }
        self.scratch = scratch;

        // Lines belonging to a channel that has been retired are finished, so a
        // partial word does not sit at the bottom of the panel indefinitely.
        self.cw.ids(&mut self.ids);
        self.log.retain_channels(&self.ids);

        self.note_cooldown = (self.note_cooldown - dt).max(0.0);
        let announced = est.mode;
        if announced != self.last_mode {
            if settings.classifier.announce_in_log
                && announced != Mode::Unknown
                && self.note_cooldown <= 0.0
            {
                let note = match announced {
                    Mode::Cw => format!(
                        "CW {:.0} Hz {:.0} wpm, {} channels",
                        self.cw.focused_tone_hz(),
                        self.cw.focused_wpm(),
                        self.cw.len()
                    ),
                    Mode::Rtty | Mode::Navtex => format!(
                        "{} {:.0} Hz shift {:.0} Hz {:.2} baud",
                        announced.as_str(),
                        self.fsk.mark_hz(),
                        self.fsk.shift_hz(),
                        self.fsk.baud()
                    ),
                    Mode::Psk31 => format!(
                        "PSK31 {:.0} Hz, framing {:.0} percent",
                        self.psk.centre_hz(),
                        self.psk.stats().lock * 100.0
                    ),
                    _ => announced.as_str().to_string(),
                };
                self.log.push_note(&note);
                self.note_cooldown = 3.0;
            }
            self.last_mode = announced;
        }

        self.log.tick(dt);

        // The buffer is handed back so the next block reuses the allocation.
        let mut held = samples.to_vec();
        held.clear();
        self.iq = held;
    }

    /// Feeds the classifier one spectrum snapshot and runs one allocation pass.
    ///
    /// Allocation reads the peak list the classifier just refreshed, so the two
    /// run together: doing it per audio block instead would repeat the same
    /// decision several times over one analysis interval.
    ///
    /// The frequency of the first bin travels with the slice, because a two
    /// sided spectrum does not start at nought and every position derived from
    /// it would otherwise be out by the Nyquist frequency.
    pub fn observe_spectrum(
        &mut self,
        bins: &[f32],
        low_hz: f32,
        bin_hz: f32,
        dt: f32,
        settings: &Settings,
    ) {
        // Where a channel may be opened. Two different questions behind one
        // name, and they need two different answers.
        //
        // Audio from a transceiver occupies three kilohertz and the passband
        // setting describes it exactly. A quadrature input occupies whatever the
        // converter captured, which is two orders of magnitude more, and the
        // same setting applied there confines the search to a sliver around the
        // dial: every station on the band is then invisible to the allocator and
        // the bank never opens a second channel.
        let nyquist = self.rate as f32 * 0.5;
        let passband = if self.complex {
            let stated = settings.dsp.search_span_hz;
            let half = if stated > 0.0 { stated.min(nyquist) } else { nyquist };
            (-half, half)
        } else {
            (
                settings.dsp.passband_low_hz,
                settings.dsp.passband_high_hz.min(nyquist),
            )
        };
        let cw = self.cw.best_stats();
        let fk = self.fsk.stats();
        let pk = self.psk.stats();
        self.classifier.update(
            bins,
            low_hz,
            bin_hz,
            dt,
            &settings.classifier,
            &cw,
            &fk,
            &pk,
            passband,
        );

        if settings.morse.enabled {
            // The peak list is copied out because allocation takes the bank
            // mutably while the classifier still owns the slice.
            let peaks: Vec<(f32, f32)> = self.classifier.peaks().to_vec();
            self.cw.allocate(&peaks, &settings.morse, dt);
        }
    }

    /// Slides the search surface after a retune.
    ///
    /// The keying channels are left alone. Each carries its own correction loop
    /// and is re-placed by the allocator within a pass, so a small retune is
    /// absorbed and a large one retires the channel and opens a new one, which
    /// is the right outcome: past a detector width the old estimates describe a
    /// different station.
    pub fn shift_history(&mut self, bins: i32, bin_hz: f32) {
        self.classifier.shift(bins, bin_hz);
    }

    /// Points a keying channel at a frequency the operator picked.
    ///
    /// The frequency is snapped to the nearest carrier first. A pointing device
    /// cannot resolve a few hertz and does not need to: the click states a
    /// neighbourhood, the spectrum states where inside it the signal is, and the
    /// detector closes whatever is left over with its own tracking loop.
    ///
    /// The radius is the caller's business. It has to stay small: a wide search
    /// snaps onto the strongest neighbour rather than onto the carrier the
    /// operator pointed at, which is the opposite of a manual tune.
    ///
    /// Returns the frequency the channel was actually placed on rather than the
    /// one that was asked for. The bank moves a request that falls within half a
    /// detector width of an edge, and a caller that reported the request instead
    /// would name a frequency no detector occupies.
    pub fn tune_to(
        &mut self,
        hz: f32,
        bin_hz: f32,
        radius_hz: f32,
        settings: &Settings,
    ) -> f32 {
        let snapped = self
            .classifier
            .snap_to_peak(hz, radius_hz, bin_hz)
            .unwrap_or(hz);
        if (snapped - hz).abs() > 1.0 {
            crate::log_debug!("decode", "click at {:.0} Hz snapped to {:.1} Hz", hz, snapped);
        }

        // The bank decides first and everything else follows it. Telling the
        // classifier the request rather than the placement would leave it
        // tracking a tone no detector is listening to, and the tracker would
        // then drag the whole bank back towards it.
        let placed = self.cw.tune_to(snapped, &settings.morse);
        self.classifier.force_tone(placed);
        let shift = self.fsk.shift_hz();
        self.fsk.set_tones(placed, shift);
        // The same gesture points every demodulator, because the operator pointed
        // at a signal rather than at one of them: which is the right reading of
        // it is the classifier's decision and it is taken separately.
        self.psk.set_centre(placed);
        placed
    }

    pub fn set_focus(&mut self, id: u32) {
        self.cw.set_focus(id);
    }

    pub fn pin_channel(&mut self, id: u32, pinned: bool) {
        self.cw.set_pinned(id, pinned);
    }

    /// Sets the width of one channel.
    pub fn set_channel_width(&mut self, id: u32, hz: f32) {
        self.cw.set_width(id, hz);
    }

    /// Width one channel was asked for.
    pub fn channel_width(&self, id: u32) -> Option<f32> {
        self.cw.width_of(id)
    }

    /// Moves one channel, and points the other demodulators with it.
    ///
    /// Returns the frequency it was placed on, which differs from the request
    /// within half a detector width of an edge.
    pub fn move_channel(&mut self, id: u32, hz: f32) -> f32 {
        let placed = self.cw.move_to(id, hz);
        // The tracker is told where the bank actually went rather than what was
        // asked for; otherwise it would follow a tone no detector is listening
        // to and would drag the whole bank back towards it.
        self.classifier.force_tone(placed);
        placed
    }

    pub fn drop_channel(&mut self, id: u32) {
        if self.cw.drop_channel(id) {
            self.log.close_channel(id);
        }
    }

    pub fn reset(&mut self) {
        self.cw.reset();
        self.fsk.reset();
        self.psk.reset();
        self.classifier.reset();
    }
}

/// Fingerprint of everything that forces a demodulator rebuild.
///
/// The keying detector plans its analysis window from the requested bandwidth
/// and from the working speed, so both belong here. Values applied per block,
/// such as the tone frequency, the inversion flag or the parity mode, do not,
/// and neither does the channel count: opening and closing channels is the job
/// of the allocator, not of a rebuild.
fn signature(settings: &Settings, rate: u32, complex: bool) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(rate as u64);
    // The arrangement decides what a detector measures as surely as the geometry
    // does, so a change to it has to reach every demodulator.
    mix(u64::from(complex));
    // The width and the working speed are absent deliberately. Both reach the
    // detectors through their own paths, per channel and per block, and folding
    // them in here made every step of either control rebuild the whole bank and
    // discard the level and timing estimates of every channel in it.
    mix(settings.morse.wpm_min.to_bits() as u64);
    mix(settings.morse.wpm_max.to_bits() as u64);
    mix(settings.rtty.baud.to_bits() as u64);
    mix(settings.rtty.data_bits as u64);
    mix(settings.rtty.stop_bits.to_bits() as u64);
    mix(settings.rtty.alphabet as u64 + 1);
    // The phase demodulator contributes only through the rate above. Its symbol
    // rate is stated by the format and is not a setting, so nothing an operator
    // can move changes the geometry of its filter; the centre frequency is
    // applied per block like every other retuning.
    h
}