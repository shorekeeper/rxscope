//! Keying channel bank.
//!
//! One keying detector follows one carrier. A band carries several at once, so
//! the bank holds several detectors.
//!
//! Cost is what makes this practical. The detector evaluates three tones over a
//! window of n samples once per quarter window, which is forty eight operations
//! per input sample regardless of the window length, or about six hundred
//! thousand per second of audio at the decoder rate. Eight channels stay three
//! orders of magnitude below the frame budget, so the channels share nothing
//! and no scheduling is needed: they are simply fed the same block in turn.
//!
//! ## Who owns a channel frequency
//!
//! The detector, and nothing else. An earlier arrangement let the allocator
//! retune any unpinned channel onto the nearest peak on every analysis pass,
//! which made every channel the operator had not explicitly held wander several
//! times a second. Worse, a move of more than half the detector width discards
//! the level and timing estimates, so a wandering channel spent much of its life
//! warming up and produced nothing.
//!
//! So the allocator opens channels and closes them, and never moves one. Fine
//! tracking is the business of the frequency correction inside each detector,
//! which is a control the operator can switch off; with it off a channel stays
//! exactly where it was put, which is what off means.
//!
//! ## Which slot is scarce
//!
//! Not the arithmetic. The peak search runs on a decayed peak hold, which is
//! what lets it see an intermittent keyed signal, and the same property makes it
//! report local maxima of noise. Those maxima persist, so a channel opened on
//! one is never retired by the presence test: its carrier is always there. The
//! bank therefore ranks its channels by what they produce rather than by what
//! they receive. A detector that has accepted no element holds a weak claim on
//! its slot, gives way to any qualified candidate, and is retired outright once
//! it has been silent long enough.
//!
//! A channel the operator placed is exempt from all of it. It is pinned, and a
//! pinned channel is never retired, never displaced and never moved: the whole
//! point of placing one by hand is that the machine stops having opinions about
//! where it belongs.
//!
//! ## Width
//!
//! Per channel rather than per bank. Two stations on one band are rarely the
//! same width, and a bank wide setting also meant that every step of the control
//! rebuilt every detector and discarded the estimates of the channels the
//! operator was not adjusting.

use crate::config::settings::MorseSettings;
use crate::dsp::receiver::iq::Complex;

use super::classify::keying_confidence;
use super::morse::{Gate, KeyingStats, MorseDecoder};

/// Hard ceiling, independent of the operator setting. The bank keeps its per
/// pass bookkeeping in fixed arrays of this size so an analysis pass does not
/// allocate.
pub const MAX_CHANNELS: usize = 16;

/// Analysis passes a candidate has to be seen on before a channel is opened for
/// it. At the default interval this is three quarters of a second, which is
/// shorter than the shortest transmission worth decoding.
const ALLOCATION_HITS: u32 = 3;

/// Passes a candidate survives without being seen.
const CANDIDATE_IDLE: u32 = 2;

/// Candidates tracked at once. Larger than the peak list so a band in flux does
/// not lose the entry it was about to promote.
const MAX_CANDIDATES: usize = 12;

/// Seconds a channel survives after its carrier left the held spectrum.
const RETIRE_S: f32 = 6.0;

/// Seconds a channel may go without accepting a single element before it is
/// retired. Longer than the pause inside a transmission, short enough that a
/// slot is not held for a minute by a peak that never keys.
const BARREN_S: f32 = 12.0;

/// Seconds a fresh channel is protected from displacement, so a detector is not
/// evicted before its level windows have even filled.
const GRACE_S: f32 = 3.0;

/// Fraction of the detector bandwidth that must fit between a channel and the
/// edges of the representable band. Below this the detector reaches direct
/// current or the Nyquist frequency, where its outer bins measure something
/// that is not a signal.
const EDGE_MARGIN: f32 = 0.6;

/// Fallback used before any detector exists, in hertz.
const DEFAULT_BANDWIDTH_HZ: f32 = 200.0;

/// Read only view of one channel, for the interface.
#[derive(Debug, Clone, Copy)]
pub struct ChannelInfo {
    pub id: u32,
    /// Frequency the detector is centred on, anchor plus its own correction.
    pub hz: f32,
    /// Equivalent noise bandwidth of this detector, which is what the marker
    /// draws and what a drag on its edge changes.
    pub width_hz: f32,
    pub wpm: f32,
    pub snr_db: f32,
    pub level_db: f32,
    /// Noise level this detector is measuring against.
    pub floor_db: f32,
    pub quality: f32,
    /// Confidence that this channel is decoding real traffic.
    pub confidence: f32,
    /// Condition holding the keying decision shut, if any.
    pub gate: Gate,
    /// Elements the detector accepted. Zero alongside an open gate means the
    /// durations are being rejected rather than classified, which is a speed
    /// range problem and not a level problem.
    pub elements: u64,
    pub rejects: u64,
    /// True while the carrier is still present in the held spectrum.
    pub present: bool,
    /// True while the operator holds the channel in place.
    pub pinned: bool,
    pub focused: bool,
    pub chars: u64,
}

struct Channel {
    id: u32,
    decoder: MorseDecoder,
    /// Width the operator asked for, which the detector may widen at speed.
    width_hz: f32,
    /// Seconds since the carrier was last seen in the peak list.
    missing_for: f32,
    /// Seconds since the detector last accepted an element.
    barren_for: f32,
    /// Seconds since the channel was opened.
    age: f32,
    /// Element count at the previous pass, for the barren test.
    last_elements: u64,
    pinned: bool,
    chars: u64,
}

/// Peak that no channel covers yet.
struct Candidate {
    hz: f32,
    level_db: f32,
    hits: u32,
    /// Passes since the candidate was last seen.
    idle: u32,
}

/// Ordering key for choosing between channels.
///
/// Confidence alone is not enough. A bank of silent detectors ties at the warm
/// up value, and a tie is broken by position, which puts every readout and every
/// eviction decision on whichever peak happened to be allocated first. A channel
/// that has accepted elements outranks every silent one whatever its confidence,
/// because it is the only kind that has demonstrated anything.
fn rank(stats: &KeyingStats) -> f32 {
    let productive = if stats.elements > 0 { 1.0 } else { 0.0 };
    productive + keying_confidence(stats)
}

pub struct CwChannelBank {
    channels: Vec<Channel>,
    candidates: Vec<Candidate>,
    /// True when the input carries a quadrature pair.
    ///
    /// Decides where a channel may be placed as well as how it is measured: on a
    /// two sided spectrum the band below the tuning point is half of what was
    /// captured, and bounding placement at nought would put it out of reach.
    complex: bool,
    next_id: u32,
    /// Channel the operator selected, zero meaning follow the best one.
    focus: u32,
    rate: u32,
    /// Scratch string, so draining does not allocate per block.
    text: String,
}

impl CwChannelBank {
    pub fn new(rate: u32, cfg: &MorseSettings) -> CwChannelBank {
        let mut bank = CwChannelBank {
            channels: Vec::with_capacity(4),
            candidates: Vec::with_capacity(MAX_CANDIDATES),
            complex: false,
            next_id: 0,
            focus: 0,
            rate,
            text: String::with_capacity(64),
        };
        if !cfg.multi_channel || cfg.max_channels <= 1 {
            bank.spawn(cfg.tone_hz, cfg.filter_bandwidth_hz, cfg, false);
        }
        bank
    }

    /// Rebuilds every detector after a setting that changes the analysis window.
    ///
    /// The frequency, the width and the pinned flag are carried over, because
    /// they are operator intent rather than derived state. Everything a detector
    /// learned about level and speed is lost, which is unavoidable: those
    /// estimates are expressed in frames and the frame period just changed.
    pub fn rebuild(&mut self, rate: u32, cfg: &MorseSettings) {
        let carried: Vec<(u32, f32, f32, bool, u64)> = self
            .channels
            .iter()
            .map(|c| (c.id, c.decoder.tone_hz(), c.width_hz, c.pinned, c.chars))
            .collect();

        self.rate = rate;
        self.channels.clear();
        self.candidates.clear();

        for (id, hz, width, pinned, chars) in carried {
            let mut decoder = MorseDecoder::new(rate, cfg);
            decoder.set_complex(self.complex);
            decoder.set_bandwidth(width);
            decoder.set_tone(hz);
            self.channels.push(Channel {
                id,
                decoder,
                width_hz: width,
                missing_for: 0.0,
                barren_for: 0.0,
                age: 0.0,
                last_elements: 0,
                pinned,
                chars,
            });
        }
        if self.channels.is_empty() && (!cfg.multi_channel || cfg.max_channels <= 1) {
            self.spawn(cfg.tone_hz, cfg.filter_bandwidth_hz, cfg, false);
        }
    }

    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    pub fn focus(&self) -> u32 {
        self.focused_index().map(|i| self.channels[i].id).unwrap_or(0)
    }

    /// States whether the input carries a quadrature pair.
    pub fn set_complex(&mut self, complex: bool) {
        self.complex = complex;
        for ch in self.channels.iter_mut() {
            ch.decoder.set_complex(complex);
        }
    }

    /// Consumes one block of audio. Every channel sees the same samples.
    pub fn feed(&mut self, samples: &[Complex], cfg: &MorseSettings) {
        for ch in self.channels.iter_mut() {
            ch.decoder.feed(samples, cfg);
        }
    }

    /// Anchors the single channel case.
    ///
    /// Called per block rather than per analysis pass, so moving the tone control
    /// takes effect immediately. With several channels this does nothing at all:
    /// allocation is the job of the allocator, and an empty bank is a legitimate
    /// state that means the band is empty.
    ///
    /// A pinned channel is left alone even here. Pinning is the operator saying
    /// where the channel goes, and a stated tone must not override it silently.
    pub fn anchor(&mut self, hz: f32, cfg: &MorseSettings) {
        if cfg.multi_channel && cfg.max_channels > 1 {
            return;
        }

        while self.channels.len() > 1 {
            if let Some(c) = self.channels.pop() {
                crate::log_debug!("decode", "cw channel {} closed, single channel mode", c.id);
            }
        }
        if self.channels.is_empty() {
            self.spawn(hz, cfg.filter_bandwidth_hz, cfg, false);
            return;
        }

        let ch = &mut self.channels[0];
        ch.missing_for = 0.0;
        if !ch.pinned {
            // No test against nought: on a two sided spectrum every frequency is
            // a real position, and the tuning point itself is where a station
            // sits after a click on the readout.
            ch.decoder.set_tone(hz);
        }
    }

    /// Runs one allocation pass over the peaks the classifier published.
    ///
    /// Peaks are given strongest first, as pairs of frequency and level in
    /// decibels. The time step is the interval since the previous pass, used for
    /// the retirement timers.
    ///
    /// No channel is moved here, see the note at the head of the file. A peak
    /// near a channel marks that channel as present and nothing more.
    pub fn allocate(&mut self, peaks: &[(f32, f32)], cfg: &MorseSettings, dt: f32) {
        for ch in self.channels.iter_mut() {
            ch.age += dt;
        }
        if !cfg.multi_channel || cfg.max_channels <= 1 {
            self.candidates.clear();
            return;
        }

        let radius = self.merge_radius(cfg);
        let limit = (cfg.max_channels as usize).min(MAX_CHANNELS);

        // Presence. A peak inside the merge radius of a channel is that channel,
        // whatever the exact frequency: the detector has its own correction loop
        // for the last few hertz, and the operator may have switched it off.
        let mut seen = [false; MAX_CHANNELS];
        for &(hz, _) in peaks {
            if let Some(index) = self.nearest(hz, radius) {
                if index < MAX_CHANNELS {
                    seen[index] = true;
                }
            }
        }

        for (i, ch) in self.channels.iter_mut().enumerate() {
            if i < MAX_CHANNELS && seen[i] {
                ch.missing_for = 0.0;
            } else {
                ch.missing_for += dt;
            }

            let elements = ch.decoder.stats().elements;
            if elements != ch.last_elements {
                ch.last_elements = elements;
                ch.barren_for = 0.0;
            } else {
                ch.barren_for += dt;
            }
        }

        // Retirement. A pinned channel is operator intent and stays until it is
        // dropped explicitly.
        let mut closed = Vec::new();
        self.channels.retain(|ch| {
            if ch.pinned {
                return true;
            }
            if ch.missing_for >= RETIRE_S {
                closed.push((ch.id, ch.decoder.tone_hz(), "carrier gone"));
                return false;
            }
            if ch.barren_for >= BARREN_S {
                closed.push((ch.id, ch.decoder.tone_hz(), "no elements"));
                return false;
            }
            true
        });
        for (id, hz, why) in closed {
            crate::log_info!("decode", "cw channel {} at {:.0} Hz closed, {}", id, hz, why);
        }

        // Candidates. Everything the channels do not cover accumulates hits.
        for c in self.candidates.iter_mut() {
            c.idle += 1;
        }
        for &(hz, level) in peaks {
            if !self.admissible(hz) || self.nearest(hz, radius).is_some() {
                continue;
            }
            match self.candidates.iter_mut().find(|c| (c.hz - hz).abs() < radius) {
                Some(c) => {
                    c.hz = hz;
                    c.level_db = level;
                    c.hits = c.hits.saturating_add(1);
                    c.idle = 0;
                }
                None => {
                    if self.candidates.len() < MAX_CANDIDATES {
                        self.candidates.push(Candidate { hz, level_db: level, hits: 1, idle: 0 });
                    }
                }
            }
        }
        self.candidates.retain(|c| c.idle <= CANDIDATE_IDLE);

        // Promotion, one per pass, which spreads the cost of building detectors
        // across several frames.
        while self.channels.len() < limit {
            match self.strongest_candidate() {
                Some(i) => {
                    let c = self.candidates.remove(i);
                    if self.nearest(c.hz, radius).is_none() {
                        self.spawn(c.hz, cfg.filter_bandwidth_hz, cfg, false);
                    }
                }
                None => break,
            }
        }

        // Displacement. A full bank must not lock out a carrier that just
        // appeared. An untried candidate is worth more than a detector that has
        // been sitting on its frequency without accepting an element, because
        // the second one has already had its chance.
        if self.channels.len() >= limit {
            if let Some(ci) = self.strongest_candidate() {
                if let Some(wi) = self.weakest_index() {
                    let (pinned, elements, age) = {
                        let w = &self.channels[wi];
                        (w.pinned, w.decoder.stats().elements, w.age)
                    };
                    if !pinned && elements == 0 && age > GRACE_S {
                        let c = self.candidates.remove(ci);
                        let old = self.channels.remove(wi);
                        crate::log_info!(
                            "decode",
                            "cw channel {} at {:.0} Hz gave way to {:.0} Hz",
                            old.id,
                            old.decoder.tone_hz(),
                            c.hz
                        );
                        self.spawn(c.hz, cfg.filter_bandwidth_hz, cfg, false);
                    }
                }
            }
        }
    }

    /// Hands the text every channel produced to the sink, as identity, centre
    /// frequency, confidence and the text itself.
    pub fn drain(&mut self, mut sink: impl FnMut(u32, f32, f32, &str)) {
        let mut scratch = std::mem::take(&mut self.text);
        for ch in self.channels.iter_mut() {
            scratch.clear();
            ch.decoder.drain(&mut scratch);
            if scratch.is_empty() {
                continue;
            }
            ch.chars += scratch.chars().count() as u64;
            let confidence = keying_confidence(&ch.decoder.stats());
            sink(ch.id, ch.decoder.tone_hz(), confidence, &scratch);
        }
        self.text = scratch;
    }

    /// Statistics of the channel the classifier should judge the band by.
    pub fn best_stats(&self) -> KeyingStats {
        match self.best_index() {
            Some(i) => self.channels[i].decoder.stats(),
            None => KeyingStats::default(),
        }
    }

    /// Statistics of the channel the interface reports in detail.
    pub fn focused_stats(&self) -> KeyingStats {
        match self.focused_index() {
            Some(i) => self.channels[i].decoder.stats(),
            None => KeyingStats::default(),
        }
    }

    pub fn focused_tone_hz(&self) -> f32 {
        self.focused_index()
            .map(|i| self.channels[i].decoder.tone_hz())
            .unwrap_or(0.0)
    }

    pub fn focused_anchor_hz(&self) -> f32 {
        self.focused_index()
            .map(|i| self.channels[i].decoder.anchor_hz())
            .unwrap_or(0.0)
    }

    pub fn focused_afc_hz(&self) -> f32 {
        self.focused_index()
            .map(|i| self.channels[i].decoder.afc_offset_hz())
            .unwrap_or(0.0)
    }

    pub fn focused_wpm(&self) -> f32 {
        self.focused_index()
            .map(|i| self.channels[i].decoder.wpm())
            .unwrap_or(0.0)
    }

    /// Equivalent noise bandwidth of the channel being reported.
    ///
    /// Per channel now, so this describes the focused one rather than the bank.
    /// Read by the meter and by the readout, both of which report that channel.
    pub fn detector_bandwidth(&self) -> f32 {
        self.focused_index()
            .map(|i| self.channels[i].decoder.bandwidth_hz())
            .unwrap_or(DEFAULT_BANDWIDTH_HZ)
    }

    /// Fills the buffer with a view of every channel, lowest frequency first.
    /// The order is by frequency rather than by strength so a row does not move
    /// under the pointer when the levels change.
    pub fn snapshot(&self, out: &mut Vec<ChannelInfo>) {
        let focus = self.focus();
        out.clear();
        for ch in &self.channels {
            let stats = ch.decoder.stats();
            out.push(ChannelInfo {
                id: ch.id,
                hz: ch.decoder.tone_hz(),
                // The effective width rather than the request: the two differ
                // whenever the working speed forced a shorter window, and the
                // marker has to draw what the detector is actually hearing
                // through.
                width_hz: ch.decoder.bandwidth_hz(),
                wpm: ch.decoder.wpm(),
                snr_db: stats.snr_db,
                level_db: stats.level_db,
                floor_db: stats.floor_db,
                quality: stats.quality,
                confidence: keying_confidence(&stats),
                gate: stats.gate,
                elements: stats.elements,
                rejects: stats.rejects,
                present: ch.missing_for < 1.0,
                pinned: ch.pinned,
                focused: ch.id == focus,
                chars: ch.chars,
            });
        }
        out.sort_by(|a, b| a.hz.partial_cmp(&b.hz).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// Points a channel at a frequency the operator picked.
    ///
    /// An existing channel within the merge radius is focused and pinned rather
    /// than duplicated: clicking near a station that is already being decoded is
    /// a request to look at it, not to open a second detector on the same tone.
    ///
    /// The frequency is moved into the band a detector can occupy rather than
    /// refused. A detector centred within half its own width of an edge reaches
    /// the Nyquist frequency with an outer bin and measures something that is not
    /// a signal, so the request cannot be carried out exactly as stated; refusing
    /// it outright leaves the operator with a click that does nothing, which is
    /// the worse of the two answers and the harder one to attribute.
    ///
    /// Returns the frequency the channel was actually placed on.
    pub fn tune_to(&mut self, hz: f32, cfg: &MorseSettings) -> f32 {
        let wanted = self.clamp_to_band(hz);
        if (wanted - hz).abs() > 1.0 {
            crate::log_info!(
                "decode",
                "{:.0} Hz is within half a detector width of the edge, placed at {:.0} Hz",
                hz,
                wanted
            );
        }

        let radius = self.merge_radius(cfg);
        if let Some(i) = self.nearest(wanted, radius) {
            let id = {
                let ch = &mut self.channels[i];
                ch.pinned = true;
                ch.missing_for = 0.0;
                ch.barren_for = 0.0;
                ch.decoder.set_tone(wanted);
                ch.id
            };
            self.focus = id;
            return wanted;
        }

        let limit = if cfg.multi_channel {
            (cfg.max_channels as usize).min(MAX_CHANNELS)
        } else {
            1
        };
        if self.channels.len() >= limit {
            if let Some(i) = self.weakest_index() {
                let old = self.channels.remove(i);
                crate::log_info!(
                    "decode",
                    "cw channel {} at {:.0} Hz replaced",
                    old.id,
                    old.decoder.tone_hz()
                );
            }
        }
        let id = self.spawn(wanted, cfg.filter_bandwidth_hz, cfg, true);
        self.focus = id;
        wanted
    }

    /// Selects a channel, or releases the selection when given nought.
    ///
    /// Releasing matters because the selection is what a press moves. With one
    /// held there is no gesture left to open a second channel, so the operator
    /// has to be able to put it down.
    pub fn set_focus(&mut self, id: u32) {
        if id == 0 {
            self.focus = 0;
            return;
        }
        if self.channels.iter().any(|c| c.id == id) {
            self.focus = id;
        }
    }

    /// Holds a channel in place, or lets the allocator have it back.
    pub fn set_pinned(&mut self, id: u32, pinned: bool) {
        if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) {
            ch.pinned = pinned;
        }
    }

    /// Sets the width of one channel.
    pub fn set_width(&mut self, id: u32, hz: f32) {
        if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) {
            ch.width_hz = hz;
            ch.decoder.set_bandwidth(hz);
        }
    }

    /// Width one channel was asked for, which is what a drag adjusts against.
    pub fn width_of(&self, id: u32) -> Option<f32> {
        self.channels.iter().find(|c| c.id == id).map(|c| c.width_hz)
    }

    /// Moves one channel to a frequency.
    ///
    /// Nothing is snapped to a peak. A drag follows the pointer, and a search
    /// that pulled the channel onto the nearest carrier would make it jump away
    /// from where the operator is holding it.
    ///
    /// Returns the frequency the channel was placed on.
    pub fn move_to(&mut self, id: u32, hz: f32) -> f32 {
        let wanted = self.clamp_to_band(hz);
        if let Some(ch) = self.channels.iter_mut().find(|c| c.id == id) {
            ch.pinned = true;
            ch.missing_for = 0.0;
            ch.barren_for = 0.0;
            ch.decoder.set_tone(wanted);
        }
        wanted
    }

    /// Removes a channel. Returns true when one was found, so the caller can
    /// close its text line.
    pub fn drop_channel(&mut self, id: u32) -> bool {
        let before = self.channels.len();
        self.channels.retain(|c| c.id != id);
        if self.channels.len() == before {
            return false;
        }
        if self.focus == id {
            self.focus = 0;
        }
        crate::log_info!("decode", "cw channel {} dropped", id);
        true
    }

    /// Identities present in the bank, for the caller to reconcile against its
    /// own per channel state.
    pub fn ids(&self, out: &mut Vec<u32>) {
        out.clear();
        out.extend(self.channels.iter().map(|c| c.id));
    }

    pub fn reset(&mut self) {
        for ch in self.channels.iter_mut() {
            ch.decoder.reset();
            ch.missing_for = 0.0;
            ch.barren_for = 0.0;
            ch.age = 0.0;
            ch.last_elements = 0;
        }
        self.candidates.clear();
    }

    // ------------------------------------------------------------ internals

    /// True when a detector centred here fits inside the representable band.
    ///
    /// Used by the allocator, which passes over such a peak because there is
    /// always another one. A request the operator made is moved instead, see
    /// clamp_to_band.
    fn admissible(&self, hz: f32) -> bool {
        let margin = self.detector_bandwidth() * EDGE_MARGIN;
        let nyquist = self.rate as f32 * 0.5;
        if self.complex {
            // Both edges of a two sided span. The middle is not excluded: it is
            // the tuning point, which is where a station sits after a click on
            // the readout, and the offset blocker only empties a few hertz of it.
            hz > -nyquist + margin && hz < nyquist - margin
        } else {
            hz > margin && hz < nyquist - margin
        }
    }

    /// Nearest frequency a detector may be centred on.
    fn clamp_to_band(&self, hz: f32) -> f32 {
        let nyquist = self.rate as f32 * 0.5;
        let margin = self.detector_bandwidth() * EDGE_MARGIN;
        let high = (nyquist - margin).max(nyquist * 0.6);
        let low = if self.complex {
            -high
        } else {
            margin.min(nyquist * 0.4)
        };
        hz.clamp(low, high.max(low + 1.0))
    }

    /// Distance below which two frequencies are the same signal. The detector
    /// passband is the physical answer; the operator setting raises it when a
    /// band is crowded enough that adjacent carriers keep merging.
    fn merge_radius(&self, cfg: &MorseSettings) -> f32 {
        cfg.channel_spacing_hz.max(cfg.filter_bandwidth_hz)
    }

    fn nearest(&self, hz: f32, radius: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, ch) in self.channels.iter().enumerate() {
            let d = (ch.decoder.tone_hz() - hz).abs();
            if d > radius {
                continue;
            }
            if best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    }

    fn strongest_candidate(&self) -> Option<usize> {
        self.candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.hits >= ALLOCATION_HITS)
            .max_by(|a, b| {
                a.1.level_db
                    .partial_cmp(&b.1.level_db)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    fn spawn(&mut self, hz: f32, width_hz: f32, cfg: &MorseSettings, pinned: bool) -> u32 {
        let mut decoder = MorseDecoder::new(self.rate, cfg);
        decoder.set_complex(self.complex);
        decoder.set_bandwidth(width_hz);
        decoder.set_tone(hz);
        self.next_id += 1;
        let id = self.next_id;
        self.channels.push(Channel {
            id,
            decoder,
            width_hz,
            missing_for: 0.0,
            barren_for: 0.0,
            age: 0.0,
            last_elements: 0,
            pinned,
            chars: 0,
        });
        crate::log_info!(
            "decode",
            "cw channel {} opened at {:.0} Hz, {} Hz wide, {} in bank",
            id,
            hz,
            width_hz,
            self.channels.len()
        );
        id
    }

    fn best_index(&self) -> Option<usize> {
        self.channels
            .iter()
            .enumerate()
            .max_by(|a, b| {
                rank(&a.1.decoder.stats())
                    .partial_cmp(&rank(&b.1.decoder.stats()))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    fn focused_index(&self) -> Option<usize> {
        if self.focus != 0 {
            if let Some(i) = self.channels.iter().position(|c| c.id == self.focus) {
                return Some(i);
            }
        }
        self.best_index()
    }

    /// Channel with the weakest claim on its slot. Operator intent outranks
    /// everything, then productivity, then confidence.
    fn weakest_index(&self) -> Option<usize> {
        let tenure = |ch: &Channel| {
            let pinned = if ch.pinned { 10.0 } else { 0.0 };
            pinned + rank(&ch.decoder.stats())
        };
        self.channels
            .iter()
            .enumerate()
            .min_by(|a, b| {
                tenure(a.1)
                    .partial_cmp(&tenure(b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }
}