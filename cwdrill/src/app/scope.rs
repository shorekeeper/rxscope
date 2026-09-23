//! Keying picture state.
//!
//! Presentation rather than synthesis: the envelope arrives from the generator
//! and everything here is about where the operator is looking and what they are
//! measuring.
//!
//! ## Why a ring rather than a growing buffer
//!
//! The history is bounded by the longest span the operator may ask for, and the
//! oldest entry is discarded when a new one arrives. A buffer drained from the
//! front would move the whole history on every frame, which at a thousand
//! entries a second and a thirty second span is a hundred kilobytes of copying
//! sixty times a second to make room for seventeen entries.
//!
//! ## Why the axis is stated as an age
//!
//! The picture is anchored on the right at the newest sample, because that is
//! where the material is arriving. Every position is therefore how long ago
//! rather than when, and the operator moves the anchor back rather than moving a
//! window along an absolute timeline. That removes the one arithmetic the
//! alternative needs: converting a stored absolute time into a position when the
//! origin itself is moving at one second per second.

use crate::synth::{Edge, SCOPE_RATE};

/// Longest history held, in seconds.
///
/// The ceiling the span control offers, so the picture can always fill itself.
/// Thirty seconds at the scope rate is thirty thousand entries, which is a
/// hundred and twenty kilobytes.
pub const MAX_SECONDS: f32 = 30.0;

/// Element boundaries held.
///
/// Thirty seconds of the fastest keying this trainer offers is a few thousand
/// elements, and the marks are only drawn for what is on screen.
const MAX_EDGES: usize = 4096;

/// Characters held for the label ribbon.
///
/// Thirty seconds at the fastest speed offered is under three hundred, so this
/// covers the whole history the picture can show.
const MAX_LABELS: usize = 512;

/// Envelope level at which an edge is taken to have happened.
///
/// Half amplitude. The two shaped envelopes both cross it at the middle of the
/// ramp, so a measurement between two crossings is the element length whatever
/// the shape, which is what makes the reading comparable across the setting.
const EDGE_LEVEL: f32 = 0.5;

/// Pixels either side of the pointer that a snap searches.
///
/// Ten is about the width of a fingertip on a pointing device. Wider would snap
/// past the edge the operator aimed at when two are close; narrower would make
/// the snap something that has to be aimed at, which defeats it.
pub const SNAP_PIXELS: f32 = 10.0;

/// One measurement cursor.
#[derive(Debug, Clone, Copy)]
pub struct Cursor {
    /// Seconds before the newest sample.
    pub age: f32,
    /// True when the position came from an edge rather than from the pointer.
    ///
    /// Reported because it says whether the reading is a measurement or an
    /// estimate: a cursor placed by eye is within a pixel of where it looks, and
    /// a pixel at a four second span is five milliseconds.
    pub snapped: bool,
}

pub struct Scope {
    /// Envelope history. A ring, newest at the position before the head.
    trace: Vec<f32>,
    head: usize,
    filled: usize,
    /// Sample index of the newest entry.
    newest_sample: u64,
    /// Samples one entry covers.
    step: f32,
    /// Sample rate the generator is running at.
    rate: f32,

    edges: std::collections::VecDeque<Edge>,
    /// Characters that have sounded, with the sample each began at.
    labels: std::collections::VecDeque<(u64, char)>,

    /// Seconds before the newest sample that the right edge shows.
    ///
    /// Nought is live. Anything else is the operator having moved back through
    /// the history, and the picture then stands still while the material carries
    /// on arriving.
    pub end: f32,
    pub cursor_a: Option<Cursor>,
    pub cursor_b: Option<Cursor>,
}

impl Scope {
    pub fn new() -> Scope {
        let capacity = (MAX_SECONDS * SCOPE_RATE) as usize;
        Scope {
            trace: vec![0.0; capacity],
            head: 0,
            filled: 0,
            newest_sample: 0,
            step: 48.0,
            rate: 48_000.0,
            edges: std::collections::VecDeque::with_capacity(256),
            labels: std::collections::VecDeque::with_capacity(256),
            end: 0.0,
            cursor_a: None,
            cursor_b: None,
        }
    }

    /// States the geometry the generator is publishing at.
    ///
    /// Called whenever the stream is opened. The history is discarded rather
    /// than rescaled: entries recorded at another rate describe a different
    /// duration, and a picture that mixed the two would read as a speed change
    /// that never happened.
    pub fn set_rate(&mut self, rate: u32, step: f32) {
        let rate = rate.max(1) as f32;
        if (rate - self.rate).abs() < 0.5 && (step - self.step).abs() < 0.01 {
            return;
        }
        self.rate = rate;
        self.step = step.max(1.0);
        self.clear();
    }

    pub fn clear(&mut self) {
        for v in self.trace.iter_mut() {
            *v = 0.0;
        }
        self.head = 0;
        self.filled = 0;
        self.newest_sample = 0;
        self.edges.clear();
        self.labels.clear();
        self.cursor_a = None;
        self.cursor_b = None;
        self.end = 0.0;
    }

    /// Appends envelope entries, oldest first.
    pub fn push_trace(&mut self, values: &[f32]) {
        if self.trace.is_empty() {
            return;
        }
        for &v in values {
            self.trace[self.head] = v;
            self.head = (self.head + 1) % self.trace.len();
            if self.filled < self.trace.len() {
                self.filled += 1;
            }
        }
    }

    /// States where the newest entry sits in the stream.
    pub fn set_newest(&mut self, sample: u64) {
        self.newest_sample = sample;
    }

    /// Appends element boundaries, oldest first.
    pub fn push_edges(&mut self, edges: &[Edge]) {
        for &edge in edges {
            self.edges.push_back(edge);
        }
        while self.edges.len() > MAX_EDGES {
            self.edges.pop_front();
        }
    }

    /// Records what a burst was, at the sample it began.
    pub fn push_label(&mut self, at: u64, ch: char) {
        self.labels.push_back((at, ch));
        while self.labels.len() > MAX_LABELS {
            self.labels.pop_front();
        }
    }

    /// Characters inside an age range, oldest first.
    pub fn labels_between(&self, older: f32, newer: f32, out: &mut Vec<(f32, char)>) {
        out.clear();
        if self.rate <= 0.0 {
            return;
        }
        for &(at, ch) in &self.labels {
            if at > self.newest_sample {
                continue;
            }
            let age = (self.newest_sample - at) as f32 / self.rate;
            if age >= newer && age <= older {
                out.push((age, ch));
            }
        }
    }

    /// Seconds a sample count covers.
    ///
    /// Exposed so the timing lane can compare an element against the ideal the
    /// generator stated, which travels as samples.
    pub fn seconds_of(&self, samples: u32) -> f32 {
        if self.rate <= 0.0 {
            0.0
        } else {
            samples as f32 / self.rate
        }
    }

    pub fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// Seconds of history held.
    pub fn history_seconds(&self) -> f32 {
        self.filled as f32 * self.step / self.rate
    }

    /// Envelope value a number of entries back, nought being the newest.
    #[inline]
    pub fn back(&self, k: usize) -> f32 {
        if k >= self.filled {
            return 0.0;
        }
        let at = (self.head + self.trace.len() - 1 - k) % self.trace.len();
        self.trace[at]
    }

    /// Largest envelope value inside an age range, which is what a column draws.
    ///
    /// The peak rather than a sample: at a wide span one column covers many
    /// entries, and a dot that occupied one of them would otherwise be invisible
    /// exactly when the operator zoomed out to find it.
    pub fn peak_between(&self, older: f32, newer: f32) -> f32 {
        let per_entry = self.step / self.rate;
        if per_entry <= 0.0 {
            return 0.0;
        }
        let lo = (newer / per_entry).floor().max(0.0) as usize;
        let hi = (older / per_entry).ceil().max(0.0) as usize;
        let mut peak = 0.0f32;
        for k in lo..=hi.min(self.filled.saturating_sub(1)) {
            let v = self.back(k);
            if v > peak {
                peak = v;
            }
        }
        peak
    }

    /// Age of a position across the picture, nought at the left edge.
    #[inline]
    pub fn age_at(&self, fraction: f32, span: f32) -> f32 {
        (self.end + span * (1.0 - fraction.clamp(0.0, 1.0))).max(0.0)
    }

    /// Position of an age, nought at the left edge.
    #[inline]
    pub fn fraction_of(&self, age: f32, span: f32) -> f32 {
        if span <= 0.0 {
            return 1.0;
        }
        1.0 - (age - self.end) / span
    }

    /// Nearest envelope crossing to an age.
    ///
    /// The one thing that makes a measurement a measurement. Placed by eye a
    /// cursor is within a pixel, which at a four second span across a thousand
    /// pixels is four milliseconds, and an element at twenty words a minute is
    /// sixty: the reading would be within seven per cent, which is the same order
    /// as the jitter being measured.
    pub fn snap(&self, age: f32, tolerance: f32) -> Option<f32> {
        let per_entry = self.step / self.rate;
        if per_entry <= 0.0 || self.filled < 2 {
            return None;
        }
        let centre = (age / per_entry).round().max(0.0) as usize;
        let span = (tolerance / per_entry).ceil().max(1.0) as usize;
        let lo = centre.saturating_sub(span);
        let hi = (centre + span).min(self.filled.saturating_sub(2));
        if hi <= lo {
            return None;
        }

        let mut best: Option<(usize, f32)> = None;
        for k in lo..=hi {
            // Entry k is newer than k plus one, so the pair straddles a crossing
            // when the two sit either side of the level.
            let newer = self.back(k);
            let older = self.back(k + 1);
            let crosses = (older < EDGE_LEVEL && newer >= EDGE_LEVEL)
                || (older >= EDGE_LEVEL && newer < EDGE_LEVEL);
            if !crosses {
                continue;
            }
            // Linear between the two entries. The envelope is monotonic across a
            // ramp, so one step of interpolation places the crossing to a
            // fraction of an entry, which is a fraction of a millisecond.
            let denom = newer - older;
            let t = if denom.abs() > 1e-6 {
                ((EDGE_LEVEL - older) / denom).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let found = (k as f32 + 1.0 - t) * per_entry;
            let distance = (found - age).abs();
            if best.map(|(_, d)| distance < d).unwrap_or(true) {
                best = Some((k, distance));
                // Kept so the loop reports the position rather than the index.
                if distance <= per_entry {
                    return Some(found);
                }
            }
        }

        // The nearest crossing found, recomputed rather than carried: the loop
        // above returns early on the common case and this is the remainder.
        best.and_then(|(k, _)| {
            let newer = self.back(k);
            let older = self.back(k + 1);
            let denom = newer - older;
            let t = if denom.abs() > 1e-6 {
                ((EDGE_LEVEL - older) / denom).clamp(0.0, 1.0)
            } else {
                0.5
            };
            Some((k as f32 + 1.0 - t) * per_entry)
        })
    }

    /// Places a cursor, snapping when asked.
    pub fn place(&mut self, which: bool, age: f32, snap: bool, tolerance: f32) {
        let (age, snapped) = if snap {
            match self.snap(age, tolerance) {
                Some(found) => (found, true),
                None => (age, false),
            }
        } else {
            (age, false)
        };
        let cursor = Cursor { age: age.max(0.0), snapped };
        if which {
            self.cursor_a = Some(cursor);
        } else {
            self.cursor_b = Some(cursor);
        }
    }

    pub fn clear_cursors(&mut self) {
        self.cursor_a = None;
        self.cursor_b = None;
    }

    /// Seconds between the two cursors.
    pub fn measurement(&self) -> Option<f32> {
        match (self.cursor_a, self.cursor_b) {
            (Some(a), Some(b)) => Some((a.age - b.age).abs()),
            _ => None,
        }
    }

    /// Keeps the anchor inside the history.
    ///
    /// Called after a pan and after a span change. Without it the operator can
    /// scroll past the oldest entry into a picture of nothing, and the way back
    /// is a pan of the same distance in the dark.
    pub fn clamp(&mut self, span: f32) {
        let oldest = (self.history_seconds() - span).max(0.0);
        self.end = self.end.clamp(0.0, oldest);
    }

    /// Element boundaries inside an age range, oldest first.
    ///
    /// The marks the picture draws. Returned as ages so the caller needs no
    /// knowledge of the sample counter.
    pub fn edges_between(&self, older: f32, newer: f32, out: &mut Vec<(f32, Edge)>) {
        out.clear();
        if self.rate <= 0.0 {
            return;
        }
        for &edge in &self.edges {
            // The counter is monotonic, so an edge in the future of the newest
            // published entry is one the picture has not caught up with.
            if edge.at > self.newest_sample {
                continue;
            }
            let age = (self.newest_sample - edge.at) as f32 / self.rate;
            if age >= newer && age <= older {
                out.push((age, edge));
            }
        }
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// Ideal boundaries of the character an element belongs to.
    ///
    /// Laid out from the start of the character rather than from the start of the
    /// stream, because that is where the ear resynchronizes: an operator does not
    /// carry the error of the previous character into the next one.
    ///
    /// Returned as ages, newest first, so the caller draws them without knowing
    /// which direction the sample counter runs in.
    pub fn ideal_marks(&self, out: &mut Vec<f32>) {
        out.clear();
        if self.rate <= 0.0 || self.edges.is_empty() {
            return;
        }

        // Walked forward so the accumulation is in the direction the character
        // was sent, then converted to ages at the end.
        let mut character_start: Option<u64> = None;
        let mut accumulated: u64 = 0;
        for &edge in &self.edges {
            if edge.sync {
                character_start = Some(edge.at);
                accumulated = 0;
            }
            let start = match character_start {
                Some(s) => s,
                // An element before the first synchronization belongs to a
                // character the picture did not see the start of, so there is
                // nothing to measure it against.
                None => continue,
            };
            accumulated += edge.ideal_samples as u64;
            let boundary = start + accumulated;
            if boundary > self.newest_sample {
                continue;
            }
            out.push((self.newest_sample - boundary) as f32 / self.rate);
        }
    }
}

impl Default for Scope {
    fn default() -> Scope {
        Scope::new()
    }
}