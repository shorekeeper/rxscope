//! Mode recognition and signal tracking.
//!
//! The decision combines two independent views. The spectrum shows how many
//! carriers there are and how far apart they sit, which separates a single tone
//! transmission from a shifted pair. The decoders report their own statistics,
//! which separate a keyed tone from a steady one and confirm that a tone pair is
//! actually carrying framed characters.
//!
//! Peaks are taken from a decayed peak hold rather than from a single snapshot.
//! A keyed signal is absent from any given frame roughly four times out of ten,
//! so the loudest bin of the moment belongs to whichever station happens to be
//! transmitting right then. On a band carrying a dozen of them that makes the
//! tracker migrate continuously and nothing accumulates long enough to decode.
//! A held peak keeps an intermittent but persistent signal at its real level,
//! and the decay is what lets a station that stopped transmitting fall out.
//!
//! Peak hold rather than an average, because the input is already logarithmic:
//! averaging in that domain drags an intermittent signal towards the noise in
//! proportion to its duty cycle, which is exactly the information being sought.
//!
//! Moving the tracked tone costs the decoder its accumulated timing, so a move
//! has to be worth it. A challenger must be clearly stronger, hold that
//! advantage across several passes, and wait out a dwell period. All three
//! together, because any one of them alone still leaves room to oscillate
//! between two comparable signals.

use crate::config::settings::ClassifierSettings;

use super::fsk::FskStats;
use super::morse::KeyingStats;
use super::psk::PskStats;
use super::Mode;

/// Height above the noise floor a bin needs before it counts as a carrier.
const PEAK_MARGIN_DB: f32 = 9.0;

/// Bins on each side that a candidate has to exceed. A single bin neighbourhood
/// admits every tooth of a keying sideband comb.
const PEAK_NEIGHBOURHOOD: usize = 3;

/// Advantage a challenger needs before it may take the tracked tone.
const TONE_SWITCH_MARGIN_DB: f32 = 8.0;

/// Seconds the tracked tone stays put before another signal may take over.
const TONE_DWELL_S: f32 = 2.5;

/// Passes a challenger has to win in a row before the tone moves to it.
const TONE_CHALLENGE_HITS: u32 = 4;

/// Decay of the held spectrum. A station that stops transmitting drops out of
/// the peak list in about three seconds, which is longer than any gap inside a
/// transmission and shorter than the pause between two of them.
const HOLD_DECAY_DB_PER_S: f32 = 12.0;

/// Peaks kept from one analysis pass.
///
/// Twelve rather than six, because the list feeds the channel allocator as well
/// as the mode decision and the bank holds up to sixteen. Truncation happens
/// before the pruning that removes the skirts of one carrier, so a short list on
/// a crowded band can hold six readings of two stations and leave the allocator
/// nothing to open a third channel on.
const MAX_PEAKS: usize = 12;

/// Consecutive passes that have to agree before a mode is published.
const REQUIRED_HITS: u32 = 3;

/// Phase clustering a carrier must reach before its framing is believed.
///
/// The framing is not evidence on its own. The alphabet accepts any run of bits
/// between two boundaries and holds most of the short patterns, so noise
/// assembles codes that resolve at a high rate on an empty band. The clustering
/// is the one reading that says the carrier is phase modulated at all, and a
/// spectrum cannot supply it, so it is a condition rather than a term in a sum.
///
/// A quarter of the way from noise to a clean reversal, on the rescaled figure.
const PSK_MIN_CLUSTERING: f32 = 0.25;

/// Confidence that a keying detector is decoding real traffic.
///
/// The share of assembled patterns that decoded into a real character carries
/// most of the weight: an adaptive threshold produces marks from noise just as
/// readily as from keying, and only the table lookup tells the two apart. A
/// stream made mostly of single element patterns is the signature of noise even
/// when each of them is a valid table entry, so it is penalized separately.
///
/// Before enough characters exist for that ratio to mean anything, its weight
/// is removed from the sum and the rest is renormalized. Counting absent
/// evidence as evidence against would cap a fresh channel below any useful
/// threshold, and since the ratio only grows from decoded characters the
/// channel could never climb out.
///
/// The warm up result is capped short of certainty: a channel that has not yet
/// produced a readable character has not proved anything, it has only failed to
/// disprove itself.
///
/// The figure is computed per detector rather than per band, which is what lets
/// several carriers be judged independently: a strong shifted pair elsewhere in
/// the passband says nothing about whether a given tone is being keyed.
pub fn keying_confidence(cw: &KeyingStats) -> f32 {
    const QUALITY_WEIGHT: f32 = 0.45;
    const OTHER_WEIGHT: f32 = 0.55;
    const WARMUP_CEILING: f32 = 0.75;

    let duty_ok = if cw.duty > 0.08 && cw.duty < 0.80 { 1.0 } else { 0.0 };
    let snr_ok = ((cw.snr_db - 6.0) / 14.0).clamp(0.0, 1.0);
    let singles_ok = 1.0 - ((cw.single_ratio - 0.40) / 0.25).clamp(0.0, 1.0);
    let other = 0.20 * singles_ok + 0.15 * cw.bimodal + 0.10 * duty_ok + 0.10 * snr_ok;

    if cw.quality_known {
        (other + QUALITY_WEIGHT * cw.quality).clamp(0.0, 1.0)
    } else {
        (other / OTHER_WEIGHT).clamp(0.0, WARMUP_CEILING)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Estimate {
    pub mode: Mode,
    pub confidence: f32,
    /// True once a carrier has been tracked.
    ///
    /// A flag rather than a sentinel value, because on a two sided spectrum
    /// nought is a legitimate frequency: it is the tuning point, which is where
    /// a station sits after a click on the readout. Every test of the form
    /// "above fifty hertz" treated the whole half below the tuning point as
    /// nothing tracked, which is half the band on a quadrature input.
    pub tone_valid: bool,
    /// Tracked carrier, the automatic tone for the keying decoder.
    pub tone_hz: f32,
    /// Higher tone of the detected pair.
    pub mark_hz: f32,
    pub shift_hz: f32,
    pub baud: f32,
    pub noise_floor_db: f32,
    pub peaks: usize,
}

impl Default for Estimate {
    fn default() -> Estimate {
        Estimate {
            mode: Mode::Unknown,
            confidence: 0.0,
            tone_valid: false,
            tone_hz: 0.0,
            mark_hz: 0.0,
            shift_hz: 0.0,
            baud: 0.0,
            noise_floor_db: -120.0,
            peaks: 0,
        }
    }
}

pub struct Classifier {
    current: Estimate,
    /// Seconds the current mode decision stays latched.
    hold_left: f32,
    /// Seconds until the next analysis pass.
    next_in: f32,
    /// Mode the last passes agreed on, and how many of them did.
    pending: Mode,
    pending_hits: u32,

    /// Decayed peak hold over the spectrum, in decibels.
    held: Vec<f32>,
    /// Frequency of the first bin, which on a two sided spectrum is negative.
    ///
    /// Carried rather than assumed to be nought. Every peak position and every
    /// level lookup is computed from it, and a two sided spectrum with the
    /// assumption in place puts each of them out by the Nyquist frequency,
    /// which is to say onto a different signal entirely.
    low_hz: f32,
    /// True once a carrier has been tracked, see the note on the estimate.
    tone_valid: bool,
    /// Seconds since the tracked tone last moved.
    tone_dwell: f32,
    /// Signal trying to take the tracked tone, and how many passes it has won.
    tone_candidate: f32,
    tone_candidate_hits: u32,

    /// Sorting scratch, for the median. Kept to avoid a per pass allocation over
    /// a few thousand values.
    scratch: Vec<f32>,
    peaks: Vec<(f32, f32)>,
}

impl Classifier {
    pub fn new() -> Classifier {
        Classifier {
            current: Estimate::default(),
            hold_left: 0.0,
            next_in: 0.0,
            pending: Mode::Unknown,
            pending_hits: 0,
            held: Vec::new(),
            low_hz: 0.0,
            tone_valid: false,
            tone_dwell: 0.0,
            tone_candidate: 0.0,
            tone_candidate_hits: 0,
            scratch: Vec::new(),
            peaks: Vec::with_capacity(MAX_PEAKS),
        }
    }

    pub fn estimate(&self) -> Estimate {
        self.current
    }

    /// Held spectrum, for a display that wants to show what the tracker sees.
    pub fn held_spectrum(&self) -> &[f32] {
        &self.held
    }

    /// Carriers found on the last analysis pass, strongest first. The channel
    /// allocator reads this instead of running its own peak search: the held
    /// spectrum behind it already rejects the intermittent absence of a keyed
    /// signal, which a live snapshot does not.
    pub fn peaks(&self) -> &[(f32, f32)] {
        &self.peaks
    }

    pub fn noise_floor_db(&self) -> f32 {
        self.current.noise_floor_db
    }

    pub fn reset(&mut self) {
        self.current = Estimate::default();
        self.hold_left = 0.0;
        self.pending = Mode::Unknown;
        self.pending_hits = 0;
        self.held.clear();
        self.tone_valid = false;
        self.tone_dwell = 0.0;
        self.tone_candidate = 0.0;
        self.tone_candidate_hits = 0;
    }

    /// Overrides the tracked tone.
    ///
    /// The dwell is reset rather than left running, so the tracker cannot move
    /// away on the very next pass from what the operator just picked.
    pub fn force_tone(&mut self, hz: f32) {
        self.current.tone_hz = hz;
        self.current.tone_valid = true;
        self.tone_valid = true;
        self.current.mark_hz = hz;
        self.tone_dwell = 0.0;
        self.tone_candidate = 0.0;
        self.tone_candidate_hits = 0;
    }

    /// Slides the held spectrum after a retune.
    ///
    /// The held surface is what the channel allocator searches, so leaving it
    /// in place after a retune would keep the tracker on the audio position a
    /// station used to occupy and open channels on nothing. Shifting it moves
    /// every peak to where its station now is, and the allocator carries on
    /// without waiting out the decay.
    ///
    /// The tracked tone moves with it for the same reason: it is an audio
    /// frequency and it names a station rather than a place in the passband.
    pub fn shift(&mut self, bins: i32, bin_hz: f32) {
        if bins == 0 || self.held.is_empty() {
            return;
        }
        let n = self.held.len();
        let step = bins.unsigned_abs() as usize;
        let floor = self.current.noise_floor_db;

        if step >= n {
            for v in self.held.iter_mut() {
                *v = floor;
            }
        } else if bins > 0 {
            self.held.copy_within(..n - step, step);
            for v in self.held[..step].iter_mut() {
                *v = floor;
            }
        } else {
            self.held.copy_within(step.., 0);
            for v in self.held[n - step..].iter_mut() {
                *v = floor;
            }
        }

        let moved = bins as f32 * bin_hz;
        if self.current.tone_hz > 0.0 {
            self.current.tone_hz += moved;
        }
        if self.current.mark_hz > 0.0 {
            self.current.mark_hz += moved;
        }
        // The challenge that was in progress refers to a frequency that has
        // just moved, so it is abandoned rather than reinterpreted.
        self.tone_candidate = 0.0;
        self.tone_candidate_hits = 0;
        self.peaks.clear();
    }

    /// Runs one analysis pass if the interval elapsed.
    ///
    /// The spectrum is given in decibels in display order, and the frequency of
    /// its first bin is given with it: a two sided spectrum starts at minus the
    /// Nyquist frequency, and taking that for nought would place every peak on
    /// the wrong signal.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        bins: &[f32],
        low_hz: f32,
        bin_hz: f32,
        dt: f32,
        cfg: &ClassifierSettings,
        cw: &KeyingStats,
        fsk: &FskStats,
        psk: &PskStats,
        passband: (f32, f32),
    ) {
        self.low_hz = low_hz;
        self.hold_left = (self.hold_left - dt).max(0.0);
        self.tone_dwell += dt;
        self.next_in -= dt;
        // The switch is not tested here. Two jobs live in this pass and only one
        // of them is mode recognition: the peak search and the tracked tone are
        // what the channel allocator opens channels from, so gating them here
        // meant that switching mode recognition off left the bank with one
        // channel and no way to get another. The switch is applied below, where
        // the decision is actually taken.
        if bins.is_empty() || bin_hz <= 0.0 {
            return;
        }

        // The hold is fed on every call rather than once per pass, so its decay
        // follows real time regardless of how the analysis interval is set.
        self.accumulate(bins, dt);

        if self.next_in > 0.0 {
            return;
        }
        self.next_in = cfg.update_interval_ms as f32 * 0.001;

        let floor = self.noise_floor();
        self.find_peaks(bin_hz, floor, passband);

        let tone = self.track_tone(bin_hz, floor);

        // Published before the switch is consulted, because the allocator and
        // the manual controls read these three whether or not anything is being
        // recognized.
        self.current.tone_hz = tone;
        self.current.tone_valid = self.tone_valid;
        self.current.noise_floor_db = floor;
        self.current.peaks = self.peaks.len();

        if !cfg.enabled {
            // Nothing is claimed about the mode. Reported as unknown rather than
            // left at whatever was decided before the switch was moved, which
            // would be a stale decision presented as a current one.
            if self.current.mode != Mode::Unknown {
                self.current.mode = Mode::Unknown;
                self.current.confidence = 0.0;
                self.pending = Mode::Unknown;
                self.pending_hits = 0;
                self.hold_left = 0.0;
            }
            return;
        }

        let mut next = Estimate {
            mode: Mode::Unknown,
            confidence: 0.0,
            tone_valid: self.tone_valid,
            tone_hz: tone,
            mark_hz: 0.0,
            shift_hz: 0.0,
            baud: 0.0,
            noise_floor_db: floor,
            peaks: self.peaks.len(),
        };

        // Tone pair. The two strongest peaks are taken in frequency order and
        // their spacing is tested against the search range.
        let mut pair_score = 0.0f32;
        if self.peaks.len() >= 2 {
            let a = self.peaks[0].0;
            let b = self.peaks[1].0;
            let (low, high) = if a < b { (a, b) } else { (b, a) };
            let spacing = high - low;
            if spacing >= cfg.shift_search_min && spacing <= cfg.shift_search_max {
                // Two peaks of similar strength are far more likely to be one
                // shifted signal than two independent stations, and the framing
                // lock decides whether they carry characters at all.
                let balance = (self.peaks[1].1 - floor) / (self.peaks[0].1 - floor).max(1.0);
                let framing = fsk.lock.clamp(0.0, 1.0);
                pair_score = (0.30 * balance.clamp(0.0, 1.0) + 0.70 * framing).clamp(0.0, 1.0);
                next.mark_hz = high;
                next.shift_hz = spacing;
                next.baud = if fsk.baud_estimate > cfg.baud_search_min
                    && fsk.baud_estimate < cfg.baud_search_max
                {
                    fsk.baud_estimate
                } else {
                    0.0
                };
            }
        }

        // Keyed single tone. The statistics come from whichever detector is doing
        // best, so the decision describes the band rather than one arbitrary
        // carrier inside it.
        let keying_score = if self.peaks.len() == 1 || (self.peaks.len() >= 2 && pair_score < 0.3) {
            keying_confidence(cw)
        } else {
            0.0
        };

        // Phase modulation. One narrow carrier that stays on, which is what the
        // spectrum can see, and a demodulator reading reversals out of it, which
        // is what it cannot: a spectrum has no phase in it at all.
        //
        // The framing carries most of the weight, for the same reason it does in
        // the pair test above. The clustering says the signal is phase modulated
        // and the framing says the modulation is carrying this alphabet, and only
        // the second distinguishes it from every other two phase format.
        let psk_score = if self.peaks.len() == 1
            && cw.duty > 0.85
            && psk.quality > PSK_MIN_CLUSTERING
        {
            let clustering = psk.quality.clamp(0.0, 1.0);
            let framing = psk.lock.clamp(0.0, 1.0);
            (0.30 * clustering + 0.70 * framing).clamp(0.0, 1.0)
        } else {
            0.0
        };

        if cfg.detect_rtty && pair_score >= keying_score && pair_score >= psk_score {
            // A hundred baud on a hundred and seventy hertz shift is the maritime
            // safety format, worth naming separately because its message
            // structure differs.
            let navtex = cfg.detect_navtex
                && next.baud > 90.0
                && next.baud < 110.0
                && next.shift_hz > 140.0
                && next.shift_hz < 200.0;
            next.mode = if navtex { Mode::Navtex } else { Mode::Rtty };
            next.confidence = pair_score;
        } else if cfg.detect_cw && keying_score >= psk_score {
            next.mode = Mode::Cw;
            next.confidence = keying_score;
        } else if cfg.detect_psk31 && psk_score > 0.0 {
            next.mode = Mode::Psk31;
            next.confidence = psk_score;
        }

        // The tone and shift readings are published even when the mode is not
        // confident enough to switch, because the manual controls use them.
        self.current.tone_hz = next.tone_hz;
        self.current.tone_valid = self.tone_valid;
        self.current.noise_floor_db = next.noise_floor_db;
        self.current.peaks = next.peaks;
        if next.mark_hz > 0.0 {
            self.current.mark_hz = next.mark_hz;
            self.current.shift_hz = next.shift_hz;
        }
        if next.baud > 0.0 {
            self.current.baud = next.baud;
        }

        // Debounce. A candidate has to win several passes in a row, which costs
        // under a second and removes the flapping between two weak hypotheses.
        if next.mode == self.pending {
            self.pending_hits = self.pending_hits.saturating_add(1);
        } else {
            self.pending = next.mode;
            self.pending_hits = 1;
        }

        let confident = next.confidence >= cfg.min_confidence && next.mode != Mode::Unknown;
        let settled = self.pending_hits >= REQUIRED_HITS;
        let free = self.hold_left <= 0.0 || next.mode == self.current.mode;

        if confident && settled && free {
            if next.mode != self.current.mode {
                self.hold_left = cfg.hold_time_ms as f32 * 0.001;
                if cfg.announce_in_log {
                    crate::log_info!(
                        "decode",
                        "mode {} at {:.0} Hz confidence {:.2}",
                        next.mode.as_str(),
                        if next.mode == Mode::Cw { next.tone_hz } else { next.mark_hz },
                        next.confidence
                    );
                }
            }
            self.current.mode = next.mode;
            self.current.confidence = next.confidence;
        } else if self.hold_left <= 0.0
            && self.pending == Mode::Unknown
            && self.pending_hits >= REQUIRED_HITS
        {
            // Nothing convincing for long enough: drop back to unknown rather
            // than leaving a stale decision on screen.
            self.current.mode = Mode::Unknown;
            self.current.confidence = next.confidence;
        }
    }

    /// Folds one spectrum snapshot into the held maximum.
    fn accumulate(&mut self, bins: &[f32], dt: f32) {
        if self.held.len() != bins.len() {
            // A resize means the transform geometry changed, so nothing held so
            // far refers to the same frequencies.
            self.held.clear();
            self.held.extend_from_slice(bins);
            return;
        }
        let fall = HOLD_DECAY_DB_PER_S * dt;
        for (held, &v) in self.held.iter_mut().zip(bins) {
            *held = if v > *held { v } else { (*held - fall).max(v) };
        }
    }

    /// Keeps the tracked carrier unless a challenger earns the move.
    fn track_tone(&mut self, bin_hz: f32, floor: f32) -> f32 {
        let strongest = match self.peaks.first() {
            Some(&(hz, level)) => (hz, level),
            None => return self.current.tone_hz,
        };
        let current = self.current.tone_hz;

        // Nothing tracked yet, so there is nothing to defend. Asked of the flag
        // rather than of the value: on a two sided spectrum nought is the tuning
        // point and everything below it is a real position, so a test against
        // nought would abandon half the band on every pass.
        if !self.tone_valid {
            self.tone_valid = true;
            self.tone_dwell = 0.0;
            self.tone_candidate_hits = 0;
            return strongest.0;
        }

        // A peak inside the same signal is a refinement of the current estimate,
        // not a move to another station: it neither resets the dwell nor has to
        // win a challenge.
        let near = self
            .peaks
            .iter()
            .find(|p| (p.0 - current).abs() < bin_hz * 4.0)
            .map(|&(hz, _)| hz);
        if let Some(hz) = near {
            self.tone_candidate_hits = 0;
            return hz;
        }

        // The tracked signal left the held spectrum altogether, which after the
        // decay means it has been off for several seconds.
        let current_level = level_at(&self.held, self.low_hz, bin_hz, current);
        if current_level < floor + PEAK_MARGIN_DB - 3.0 {
            self.tone_dwell = 0.0;
            self.tone_candidate_hits = 0;
            return strongest.0;
        }

        let strong_enough = strongest.1 > current_level + TONE_SWITCH_MARGIN_DB;
        let same_candidate = (strongest.0 - self.tone_candidate).abs() < bin_hz * 4.0;
        if strong_enough && same_candidate {
            self.tone_candidate_hits += 1;
        } else if strong_enough {
            self.tone_candidate = strongest.0;
            self.tone_candidate_hits = 1;
        } else {
            self.tone_candidate_hits = 0;
        }

        if self.tone_candidate_hits >= TONE_CHALLENGE_HITS && self.tone_dwell >= TONE_DWELL_S {
            self.tone_dwell = 0.0;
            self.tone_candidate_hits = 0;
            return strongest.0;
        }
        current
    }

    /// Median of the held spectrum. Robust as a noise reference: a few strong
    /// carriers cannot move it the way a mean would. Taken from the held values
    /// rather than the live ones so it sits on the same scale as the peaks it is
    /// compared against.
    fn noise_floor(&mut self) -> f32 {
        if self.held.is_empty() {
            return -120.0;
        }
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.held);
        self.scratch
            .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        self.scratch[self.scratch.len() / 2]
    }

    /// Collects local maxima of the held spectrum inside the passband,
    /// strongest first.
    fn find_peaks(&mut self, bin_hz: f32, floor: f32, passband: (f32, f32)) {
        self.peaks.clear();
        let bins = &self.held;
        if bins.is_empty() {
            return;
        }
        let threshold = floor + PEAK_MARGIN_DB;
        let w = PEAK_NEIGHBOURHOOD;

        // The passband is stated in frequency and the search runs over indices,
        // so the conversion goes through the first bin rather than through
        // nought.
        let to_index = |hz: f32| ((hz - self.low_hz) / bin_hz).round();
        let lo = to_index(passband.0).max(w as f32) as usize;
        let hi = (to_index(passband.1) as usize).min(bins.len().saturating_sub(w + 1));
        if hi <= lo {
            return;
        }

        for k in lo..=hi {
            let v = bins[k];
            if v < threshold {
                continue;
            }
            // A wider neighbourhood is what keeps the comb of keying sidebands
            // from registering as a row of separate carriers.
            let dominates =
                bins[k - w..k].iter().all(|&x| v >= x) && bins[k + 1..=k + w].iter().all(|&x| v > x);
            if !dominates {
                continue;
            }
            // Parabolic interpolation over the three central bins puts the peak
            // at a fraction of a bin, which matters because one bin can be
            // several hertz wide and a shift measurement needs better than that.
            let a = bins[k - 1];
            let b = v;
            let c = bins[k + 1];
            let denom = a - 2.0 * b + c;
            let offset = if denom.abs() > 1e-6 { 0.5 * (a - c) / denom } else { 0.0 };
            let hz = self.low_hz + (k as f32 + offset.clamp(-0.5, 0.5)) * bin_hz;
            self.peaks.push((hz, v));
        }

        self.peaks
            .sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        self.peaks.truncate(MAX_PEAKS);

        // Peaks close together are the skirts of the same carrier; dropping them
        // keeps the pair test honest.
        let mut i = 1usize;
        while i < self.peaks.len() {
            let too_close = self.peaks[..i]
                .iter()
                .any(|p| (p.0 - self.peaks[i].0).abs() < bin_hz * (w as f32 + 1.0));
            if too_close {
                self.peaks.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// Refines a frequency the operator pointed at.
    ///
    /// A pointing device cannot hit a carrier exactly and does not have to: the
    /// click states a neighbourhood, and the strongest local maximum inside it
    /// is the answer. Parabolic interpolation over the three bins around that
    /// maximum places the result to a fraction of a bin, which is better than a
    /// hundredth of the search radius.
    ///
    /// The held spectrum is searched rather than the live one, because a keyed
    /// signal is absent from any given frame about four times out of ten and a
    /// click landing in one of those frames would otherwise find nothing.
    ///
    /// Returns None when the neighbourhood holds nothing above the noise, so the
    /// caller can leave the tuning alone instead of moving it onto noise.
    pub fn snap_to_peak(&self, hz: f32, radius_hz: f32, bin_hz: f32) -> Option<f32> {
        if bin_hz <= 0.0 || self.held.len() < 8 || radius_hz <= 0.0 {
            return None;
        }
        let bins = &self.held;

        // The floor is recomputed here rather than reused: this runs on an
        // operator action, not per frame, and the stored one may be a pass old.
        let mut sorted: Vec<f32> = bins.to_vec();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let floor = sorted[sorted.len() / 2];
        let threshold = floor + PEAK_MARGIN_DB * 0.5;

        let centre = ((hz - self.low_hz) / bin_hz).round() as isize;
        let span = (radius_hz / bin_hz).ceil() as isize;
        let lo = (centre - span).max(1) as usize;
        let hi = ((centre + span).max(0) as usize).min(bins.len().saturating_sub(2));
        if hi <= lo {
            return None;
        }

        let mut best = lo;
        for k in lo..=hi {
            if bins[k] > bins[best] {
                best = k;
            }
        }
        if bins[best] < threshold {
            return None;
        }

        // Parabola through the peak bin and its neighbours. The vertex offset is
        // bounded to half a bin because a larger value means the three samples do
        // not describe a single peak.
        let a = bins[best - 1];
        let b = bins[best];
        let c = bins[best + 1];
        let denom = a - 2.0 * b + c;
        let offset = if denom.abs() > 1e-6 { 0.5 * (a - c) / denom } else { 0.0 };
        Some(self.low_hz + (best as f32 + offset.clamp(-0.5, 0.5)) * bin_hz)
    }
}

/// Level of the bin nearest a frequency.
fn level_at(bins: &[f32], low_hz: f32, bin_hz: f32, hz: f32) -> f32 {
    if bin_hz <= 0.0 || bins.is_empty() {
        return f32::MIN;
    }
    let k = ((hz - low_hz) / bin_hz).round();
    if k < 0.0 {
        return f32::MIN;
    }
    bins.get((k as usize).min(bins.len() - 1)).copied().unwrap_or(f32::MIN)
}

impl Default for Classifier {
    fn default() -> Classifier {
        Classifier::new()
    }
}