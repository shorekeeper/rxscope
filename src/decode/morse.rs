//! Continuous wave decoder.
//!
//! Stages: tone amplitude, median filter, threshold derived from two level
//! estimates, element length classification against a tracked unit, then a
//! table lookup on the accumulated pattern.
//!
//! Five properties of a real transmission drive the design.
//!
//! A hard keyed transmitter splashes broadband energy at every edge. Through a
//! narrow filter that splash reads as a burst several times taller than the
//! steady tone, and it lasts a few frames at most. A median filter removes it
//! exactly: a level present in fewer than half the taps cannot be the median,
//! while a level that occupies the whole window passes through untouched. An
//! exponential smoother cannot do this at all, because it responds to the area
//! under the burst rather than to its duration.
//!
//! The on level has to appear within the first element, otherwise the gaps of
//! the first character are missed and its elements merge into one. A percentile
//! over a long window cannot deliver that: with a keying duty around four
//! tenths, an upper percentile only becomes a mark once marks fill more than
//! the percentile share of the window, which takes a good fraction of a second.
//! The on level therefore comes from a follower with a fast attack, applied to
//! the median filtered envelope where the splash no longer exists. The
//! percentile is kept as a floor under it, which is what holds the estimate
//! steady during continuous traffic.
//!
//! The off level is the opposite case. Noise moves slowly and an outlier must
//! not drag the estimate, so a low percentile over the long window is exactly
//! right and has no timing requirement to meet.
//!
//! The unit estimate has one more trap: a running average over classified
//! elements has a stable wrong solution, because a collapsed unit makes every
//! real element measure three units, classify as long, and confirm the
//! collapsed value. The estimate is taken from order statistics instead, and
//! the gaps inside a character provide a second anchor that depends on no
//! classification at all.
//!
//! The level windows are never discarded when the speed estimate moves. The
//! estimate moves on every accepted element while traffic is being read, and a
//! discarded window means the levels are unknown until it refills, during which
//! the detector has to stay shut and reports no signal to noise ratio at all.
//! The window length is therefore a count of the most recent entries in a ring
//! of fixed capacity: changing it costs nothing and loses nothing.
//!
//! Window length is the single most consequential number here, because it fixes
//! the noise bandwidth and the time resolution at once and the two pull in
//! opposite directions. It follows the requested bandwidth and is then capped so
//! it never exceeds a third of a dot at the working speed. The cap uses the
//! working speed rather than the upper speed limit: the limit only bounds where
//! the speed tracker may go, and sizing the filter for it leaves the detector
//! several hundred hertz wide, which admits every neighbouring carrier and
//! throws away the selectivity the filter exists to provide.

use crate::config::settings::{MorseSettings, TextCase};
use crate::dsp::receiver::iq::Complex;

use super::tone::{amp_to_db, coefficient, ToneBank};

/// Pattern to character. Ordered by length only for readability; the lookup is a
/// linear scan over sixty entries, which costs nothing at a few characters per
/// second.
const TABLE: &[(&str, char)] = &[
    (".-", 'A'),
    ("-...", 'B'),
    ("-.-.", 'C'),
    ("-..", 'D'),
    (".", 'E'),
    ("..-.", 'F'),
    ("--.", 'G'),
    ("....", 'H'),
    ("..", 'I'),
    (".---", 'J'),
    ("-.-", 'K'),
    (".-..", 'L'),
    ("--", 'M'),
    ("-.", 'N'),
    ("---", 'O'),
    (".--.", 'P'),
    ("--.-", 'Q'),
    (".-.", 'R'),
    ("...", 'S'),
    ("-", 'T'),
    ("..-", 'U'),
    ("...-", 'V'),
    (".--", 'W'),
    ("-..-", 'X'),
    ("-.--", 'Y'),
    ("--..", 'Z'),
    ("-----", '0'),
    (".----", '1'),
    ("..---", '2'),
    ("...--", '3'),
    ("....-", '4'),
    (".....", '5'),
    ("-....", '6'),
    ("--...", '7'),
    ("---..", '8'),
    ("----.", '9'),
    (".-.-.-", '.'),
    ("--..--", ','),
    ("..--..", '?'),
    (".----.", '\''),
    ("-.-.--", '!'),
    ("-..-.", '/'),
    ("-.--.", '('),
    ("-.--.-", ')'),
    (".-...", '&'),
    ("---...", ':'),
    ("-.-.-.", ';'),
    ("-...-", '='),
    (".-.-.", '+'),
    ("-....-", '-'),
    ("..--.-", '_'),
    (".-..-.", '"'),
    ("...-..-", '$'),
    (".--.-.", '@'),
];

/// Patterns that only exist as procedural signals. The ones that double as
/// punctuation, such as the equals sign, stay in the main table because real
/// traffic uses them as characters.
const PROSIGNS: &[(&str, &str)] = &[
    ("...-.-", "<SK>"),
    ("...-.", "<SN>"),
    ("-.-.-", "<KA>"),
    ("........", "<ERR>"),
    ("-.--.-.", "<KN>"),
];

/// Geometric midpoint between one unit and three units. Splitting there rather
/// than at two units keeps the decision equidistant in the ratio domain, which
/// is where the error of a hand sent element actually lives.
const ELEMENT_SPLIT: f32 = 1.732;

/// Longest a single element can be, in units. The alphabet has nothing above
/// three, so anything past this is two elements that merged or a carrier, and
/// emitting it as a dash would corrupt the character around it.
const MAX_ELEMENT_UNITS: f32 = 4.5;

/// Bins in the composite detector. Three is the smallest count that gives both
/// a flat response and a signed error estimate; more would widen the noise
/// bandwidth for no further benefit.
const DETECTOR_BINS: usize = 3;

/// Spacing of the outer bins from the centre, as a fraction of the main lobe
/// width of one bin. At a half the responses cross near their own half power
/// points, so the composite is flat between them.
const BIN_SPACING_FRACTION: f32 = 0.5;

/// Equivalent noise bandwidth of the composite, expressed as a multiple of the
/// reciprocal window length. One Hann windowed bin contributes one and a half,
/// and the three bins barely overlap because the spacing equals the distance
/// from a centre to its own first null.
///
/// This is the figure the operator sets and sees. The main lobe of the
/// composite is wider than this, but the main lobe is not what determines how
/// much noise reaches the threshold logic.
const COMPOSITE_ENBW: f32 = 4.5;

/// Fraction of one dot the analysis window may occupy. Beyond this the window
/// integrates a meaningful part of the shortest element and the keying edges
/// round off faster than the threshold can follow.
const WINDOW_DOT_FRACTION: f32 = 0.35;

/// Largest factor by which a fast working speed may widen the filter beyond the
/// requested noise bandwidth.
///
/// Time resolution and selectivity are traded against each other, and the trade
/// has to stop somewhere. Without a bound a runaway speed estimate opens the
/// filter until the noise it admits produces the very short false elements that
/// raised the estimate, which is a loop that only ends at the maximum window.
const MAX_WIDENING: f32 = 2.5;

/// Bimodality a speed estimate needs before it is allowed to reconfigure the
/// analysis window.
///
/// The figure measures whether the mark durations form two clusters three units
/// apart, which is the one property that separates keying from anything else. A
/// stream of noise produces a single cluster and scores zero, so this is exactly
/// the test that keeps a meaningless estimate out of the filter geometry.
const CREDIBLE_BIMODALITY: f32 = 0.35;

/// Bounds on the analysis window, in samples.
const MIN_WINDOW: usize = 32;
const MAX_WINDOW: usize = 4096;

/// Fraction of the residual error corrected per frame. Slow enough that keying
/// transients and noise average out, fast enough to close a tuning error inside
/// one character.
const AFC_GAIN: f32 = 0.02;

/// Discriminator scale. The normalized imbalance of the outer bins reaches
/// roughly this fraction of unity at an error of one spacing, so dividing by it
/// converts the reading into hertz.
const AFC_SLOPE: f32 = 0.55;

/// Taps in the median filter. Odd values only.
const MAX_MEDIAN: usize = 9;

/// Recent durations kept for the timing estimate.
const RING: usize = 48;

/// Durations required before a timing estimate is published.
const MIN_DURATIONS: usize = 8;

/// Long gaps required before the character gap is estimated.
///
/// Lower than the mark requirement because a long gap arrives once per character
/// rather than three or four times, so waiting for eight of them would mean
/// waiting for eight characters before the word boundary could be placed at all.
const MIN_LONG_GAPS: usize = 5;

/// Shortest gap admitted to the long gap population, in units.
///
/// Above the intra character gap, which is one unit, and below the character gap
/// of an ordinary transmission, which is three. Anything between is jitter on the
/// first and belongs in neither population.
const LONG_GAP_UNITS: f32 = 1.6;

/// Word gap as a multiple of the observed character gap.
///
/// The standard ratio is seven to three, so a little over two. Biased low,
/// because the cost of the two errors is not symmetric: a threshold slightly too
/// low breaks one word in two and a threshold too high runs two words together,
/// and a reader recovers from the first without noticing.
///
/// The same figure serves an ordinary transmission and a Farnsworth one, which is
/// the whole reason it is expressed against the character gap. At three units it
/// gives five and a half, which is where the stated default already sits.
const WORD_OVER_CHAR: f32 = 1.8;

/// Envelope samples required before the noise floor is published.
const MIN_ENVELOPE: usize = 64;

/// Capacity of the envelope ring, in samples. The analysis window is a count of
/// the most recent entries and never exceeds this.
const MAX_ENVELOPE: usize = 1024;

/// Frames between two floor refreshes. The floor tracks noise, which moves over
/// seconds, so there is nothing to gain from refreshing it per frame.
const REFRESH_FRAMES: u32 = 12;

/// Percentile taken as the off level. Well inside the gap distribution for any
/// duty cycle a transmission can have.
const FLOOR_PERCENTILE: f32 = 0.15;

/// Percentile taken as the steady state on level.
///
/// The figure states the lowest duty the estimate survives: at nine tenths the
/// percentile sits inside the mark distribution as long as marks occupy more
/// than a tenth of the window, and it falls into the noise below that.
///
/// Raised from the older value of eighty five hundredths, which needed a duty
/// above fifteen hundredths. Real traffic keys between two tenths and half the
/// time, so the older figure had almost no margin: a word gap inside a short
/// window was enough to drop the estimate into the noise, and the reported level
/// then fell by the whole signal to noise ratio for as long as the gap lasted.
const PEAK_PERCENTILE: f32 = 0.90;

/// Longest pattern the alphabet holds, in elements.
const MAX_PATTERN: usize = 8;

/// Reason the keying decision is held shut.
///
/// A detector that produces nothing and a detector that produces nonsense look
/// identical from the outside: both give an empty text line. They call for
/// opposite actions, so the blocking condition is published rather than folded
/// into a single squelched flag. The order the conditions are tested in is the
/// order they are worth acting on: an absolute level gate is the operator's
/// crude control and shadows everything below it, a ratio gate is the
/// meaningful one, and a flat envelope means the two level estimates landed in
/// the same population, which is one state rather than two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Gate {
    /// Threshold logic is running.
    Open,
    /// Level windows have not filled yet.
    #[default]
    Warmup,
    /// The keying decoder is switched off.
    Disabled,
    /// Envelope peak is below the operator level squelch.
    Level,
    /// Peak to floor ratio is below the operator minimum.
    Snr,
    /// Peak and floor are the same population: a carrier, or silence.
    Flat,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Gate::Open => "open",
            Gate::Warmup => "warm",
            Gate::Disabled => "off",
            Gate::Level => "sqlch",
            Gate::Snr => "snr",
            Gate::Flat => "flat",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Gate::Open => "gate.open",
            Gate::Warmup => "gate.warm",
            Gate::Disabled => "gate.off",
            Gate::Level => "gate.sqlch",
            Gate::Snr => "gate.snr",
            Gate::Flat => "gate.flat",
        }
    }

    pub fn is_open(self) -> bool {
        self == Gate::Open
    }
}

/// Statistics the classifier reads out of the keying detector.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyingStats {
    /// Ratio of frames with the tone present, over roughly the last second.
    pub duty: f32,
    pub snr_db: f32,
    pub level_db: f32,
    /// Noise level the detector is measuring against, in decibels.
    ///
    /// Published because the level above it is the only figure that keeps its
    /// meaning: the absolute one moves with the detector width, which the speed
    /// tracker changes by itself, and with the band noise, which changes without
    /// anybody touching a control. An operator setting a threshold has to be able
    /// to see both ends of the ratio.
    pub floor_db: f32,
    /// Confidence that the mark durations form two clusters three units apart,
    /// which is what separates keying from a steady carrier.
    pub bimodal: f32,
    /// Share of multi element patterns that matched a table entry.
    pub quality: f32,
    /// False until enough multi element patterns have accumulated for the
    /// quality figure to mean anything.
    pub quality_known: bool,
    /// Share of patterns that consist of a single element. Noise produces almost
    /// nothing else, ordinary text stays around a quarter.
    pub single_ratio: f32,
    pub wpm: f32,
    pub elements: u64,
    pub rejects: u64,
    /// Observed gap between characters, in units.
    ///
    /// Three for an ordinary transmission and considerably more for a Farnsworth
    /// one, which is what makes it worth reporting: it is the one figure that
    /// says which of the two is being received.
    pub char_gap_units: f32,
    /// Condition currently holding the keying decision shut.
    pub gate: Gate,
}

/// Share of random patterns of each length that the alphabet happens to hold.
///
/// The one figure that makes the match rate mean anything. Every pattern of one,
/// two or three elements is a letter: two of two, four of four and eight of
/// eight, so a decoder assembling noise into short patterns finds all of them in
/// the table and reports a perfect match rate on an empty band. Twelve of the
/// sixteen four element patterns are letters, about half of the thirty two five
/// element ones, and a fifth of the six element ones; past that the table is
/// nearly empty and a match is real evidence.
///
/// Counted from the table rather than stated, so a correction to the alphabet
/// corrects this with it. The procedural signals are counted whether or not they
/// are being printed, which raises the chance slightly and is the safe direction:
/// it can only make the resulting figure more conservative.
fn chance_table() -> [f32; MAX_PATTERN + 1] {
    let mut counts = [0u32; MAX_PATTERN + 1];
    for &(pattern, _) in TABLE {
        if pattern.len() <= MAX_PATTERN {
            counts[pattern.len()] += 1;
        }
    }
    for &(pattern, _) in PROSIGNS {
        if pattern.len() <= MAX_PATTERN {
            counts[pattern.len()] += 1;
        }
    }

    let mut out = [1.0f32; MAX_PATTERN + 1];
    for len in 1..=MAX_PATTERN {
        let possible = (1u32 << len) as f32;
        out[len] = (counts[len] as f32 / possible).min(1.0);
    }
    out
}

fn sort(values: &mut [f32]) {
    values.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f32 * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Speed the detector is built around. The tracker may move away from it, and
/// the window is re-planned when it moves far enough to matter.
fn working_wpm(cfg: &MorseSettings) -> f32 {
    let lo = cfg.wpm_min.max(3.0);
    let hi = cfg.wpm_max.max(lo + 1.0);
    cfg.wpm.clamp(lo, hi)
}

/// Longest window the requested noise bandwidth allows.
fn bandwidth_window(fs: f32, bandwidth_hz: f32) -> usize {
    let wanted = bandwidth_hz.clamp(20.0, 2000.0);
    (COMPOSITE_ENBW * fs / wanted).round().max(MIN_WINDOW as f32) as usize
}

/// Longest window a dot of the given length tolerates.
fn speed_window(fs: f32, dot_seconds: f32) -> usize {
    (dot_seconds.max(1e-3) * fs * WINDOW_DOT_FRACTION)
        .round()
        .max(MIN_WINDOW as f32) as usize
}

/// Frequencies of the composite detector around a centre.
fn composite(centre: f32, spacing: f32) -> [f32; DETECTOR_BINS] {
    [centre - spacing, centre, centre + spacing]
}

// ------------------------------------------------------------------ levels

/// Envelope conditioning and the two levels the threshold is built from.
struct Levels {
    /// Short median filter. The tap count is odd so the median is an element of
    /// the window rather than an interpolation between two.
    med: [f32; MAX_MEDIAN],
    med_scratch: [f32; MAX_MEDIAN],
    med_len: usize,
    med_pos: usize,
    med_filled: usize,

    /// Ring of fixed capacity holding the filtered envelope.
    ring: Vec<f32>,
    /// Next write position, so the newest entry sits one below it.
    pos: usize,
    filled: usize,
    /// Entries the percentiles are taken over, counted back from the newest.
    window: usize,
    /// Sorting scratch, reused so a refresh does not allocate.
    scratch: Vec<f32>,
    refresh_left: u32,

    floor: f32,
    /// Percentile on level: slow to appear, steady once it does.
    peak_slow: f32,
    /// Follower on level: appears inside the first element.
    peak_fast: f32,
    attack: f32,
    release: f32,
    ready: bool,
}

impl Levels {
    fn new() -> Levels {
        Levels {
            med: [0.0; MAX_MEDIAN],
            med_scratch: [0.0; MAX_MEDIAN],
            med_len: 1,
            med_pos: 0,
            med_filled: 0,
            ring: vec![0.0; MAX_ENVELOPE],
            pos: 0,
            filled: 0,
            window: MIN_ENVELOPE,
            scratch: Vec::with_capacity(MAX_ENVELOPE),
            refresh_left: 0,
            floor: 0.0,
            peak_slow: 0.0,
            peak_fast: 0.0,
            attack: 1.0,
            release: 1.0,
            ready: false,
        }
    }

    /// Applies the geometry that depends on the unit and the frame period.
    ///
    /// The median window is a quarter of a unit: long enough to swallow a
    /// keying splash, short enough that the shortest gap, one unit, survives
    /// with most of its length. The follower attacks over a third of a unit, so
    /// the on level is established well inside the first element, and releases
    /// over three units, so an intra character gap barely moves it while a word
    /// gap lets it fall.
    ///
    /// Nothing accumulated so far is discarded. Only the number of entries the
    /// percentiles look at changes, and a longer window simply becomes valid
    /// again once enough entries exist.
    fn configure(&mut self, dot: f32, dt: f32) {
        let mut taps = (0.25 * dot / dt).round() as usize;
        if taps >= 2 && taps % 2 == 0 {
            // Rounded up rather than down, so a request for two taps becomes a
            // real median instead of a pass through.
            taps += 1;
        }
        let taps = taps.clamp(1, MAX_MEDIAN);
        if taps != self.med_len {
            self.med_len = taps;
            self.med_pos = 0;
            self.med_filled = 0;
        }

        self.attack = coefficient((0.33 * dot).clamp(0.004, 0.050), dt);
        // Ten units rather than three. A word gap is seven units and a
        // Farnsworth one is longer, so a release of three decays by twenty
        // decibels across every gap between two words: the reported level then
        // dips below any threshold set from the level during a word, and the
        // gate chatters once per word. Ten bridges the gap and still lets a
        // station that has stopped fall away within half a second.
        self.release = coefficient((10.0 * dot).clamp(0.150, 2.000), dt);

        // The floor window spans about twenty units, which covers two characters
        // with their gaps, so both keying states are represented whatever the
        // text. The bound in seconds keeps a very slow or a very fast
        // transmission inside a sane adaptation time.
        let seconds = (20.0 * dot).clamp(0.8, 2.0);
        self.window = ((seconds / dt) as usize).clamp(MIN_ENVELOPE, MAX_ENVELOPE);
    }

    /// Full reset. Only used when the detector moves to a different signal, or
    /// when the stream restarts.
    fn clear(&mut self) {
        self.med_pos = 0;
        self.med_filled = 0;
        self.pos = 0;
        self.filled = 0;
        self.refresh_left = 0;
        self.floor = 0.0;
        self.peak_slow = 0.0;
        self.peak_fast = 0.0;
        self.ready = false;
    }

    /// Consumes one raw amplitude and returns the filtered envelope.
    fn push(&mut self, raw: f32) -> f32 {
        let filtered = self.median(raw);

        // Follower on the filtered value. The splash is already gone, so a fast
        // attack is safe here in a way it never was on the raw amplitude.
        let c = if filtered > self.peak_fast { self.attack } else { self.release };
        self.peak_fast += c * (filtered - self.peak_fast);

        self.ring[self.pos] = filtered;
        self.pos = (self.pos + 1) % MAX_ENVELOPE;
        self.filled = (self.filled + 1).min(MAX_ENVELOPE);

        if self.refresh_left > 0 {
            self.refresh_left -= 1;
        } else {
            self.refresh_left = REFRESH_FRAMES;
            self.refresh();
        }
        filtered
    }

    fn median(&mut self, raw: f32) -> f32 {
        if self.med_len <= 1 {
            return raw;
        }
        self.med[self.med_pos] = raw;
        self.med_pos = (self.med_pos + 1) % self.med_len;
        self.med_filled = (self.med_filled + 1).min(self.med_len);

        let n = self.med_filled;
        self.med_scratch[..n].copy_from_slice(&self.med[..n]);
        sort(&mut self.med_scratch[..n]);
        self.med_scratch[n / 2]
    }

    fn refresh(&mut self) {
        let n = self.window.min(self.filled);
        if n < MIN_ENVELOPE {
            self.ready = false;
            return;
        }
        self.scratch.clear();
        for k in 1..=n {
            self.scratch.push(self.ring[(self.pos + MAX_ENVELOPE - k) % MAX_ENVELOPE]);
        }
        sort(&mut self.scratch);
        self.floor = percentile(&self.scratch, FLOOR_PERCENTILE);
        self.peak_slow = percentile(&self.scratch, PEAK_PERCENTILE);
        self.ready = true;
    }

    /// On level. The follower wins at the start of a transmission, the
    /// percentile wins once the window has filled with traffic, and taking the
    /// larger of the two is what makes the transition seamless.
    fn peak(&self) -> f32 {
        if self.peak_fast > self.peak_slow {
            self.peak_fast
        } else {
            self.peak_slow
        }
    }
}

// ------------------------------------------------------------------ timing

/// Ring of recent durations plus the unit estimate derived from them.
struct Timing {
    marks: [f32; RING],
    mark_len: usize,
    mark_pos: usize,
    gaps: [f32; RING],
    gap_len: usize,
    gap_pos: usize,
    /// Gaps long enough to be a boundary rather than jitter on an element gap.
    ///
    /// A population of its own, because it answers a different question. The
    /// short gaps give the unit, which under Farnsworth is unaffected; these give
    /// the character gap, which under Farnsworth is stretched and is the only
    /// thing the word boundary can be measured against.
    long_gaps: [f32; RING],
    long_len: usize,
    long_pos: usize,
    /// Sorting scratch, so an estimate does not allocate.
    scratch: [f32; RING],

    dot: f32,
    /// Character gap in seconds, nought until enough have been seen.
    char_gap: f32,
    /// False until the unit estimate has been established once.
    ///
    /// The estimate snaps to the first measurement and is smoothed afterwards. A
    /// smoother applied from the start would crawl towards the truth from
    /// whatever the working speed happened to be, and the whole first
    /// transmission would be decoded against a wrong unit.
    primed: bool,
    dot_min: f32,
    dot_max: f32,
    /// Durations outside this range are not keying at any speed the operator
    /// allowed. Absolute bounds are what breaks the false lock: a test relative
    /// to the current estimate would accept whatever confirms it.
    accept_min: f32,
    accept_max: f32,
    /// Ratio of the long cluster to the short one.
    ratio: f32,
}

impl Timing {
    fn new(cfg: &MorseSettings) -> Timing {
        let dot_min = 1.2 / cfg.wpm_max.max(5.0);
        let dot_max = 1.2 / cfg.wpm_min.max(3.0);
        Timing {
            marks: [0.0; RING],
            mark_len: 0,
            mark_pos: 0,
            gaps: [0.0; RING],
            gap_len: 0,
            gap_pos: 0,
            long_gaps: [0.0; RING],
            long_len: 0,
            long_pos: 0,
            scratch: [0.0; RING],
            dot: 1.2 / working_wpm(cfg),
            char_gap: 0.0,
            primed: false,
            dot_min,
            dot_max,
            // Six tenths of the shortest unit absorbs the jitter the analysis
            // window adds; four units of the longest covers a character gap.
            accept_min: dot_min * 0.6,
            accept_max: dot_max * 4.0,
            ratio: 1.0,
        }
    }

    fn accepts(&self, d: f32) -> bool {
        d >= self.accept_min && d <= self.accept_max
    }

    fn push_mark(&mut self, d: f32) {
        self.marks[self.mark_pos] = d;
        self.mark_pos = (self.mark_pos + 1) % RING;
        self.mark_len = (self.mark_len + 1).min(RING);
    }

    fn push_gap(&mut self, d: f32) {
        self.gaps[self.gap_pos] = d;
        self.gap_pos = (self.gap_pos + 1) % RING;
        self.gap_len = (self.gap_len + 1).min(RING);

        // Split against the current unit rather than against a fixed duration,
        // so the boundary between the two populations follows the speed.
        if d > self.dot * LONG_GAP_UNITS {
            self.long_gaps[self.long_pos] = d;
            self.long_pos = (self.long_pos + 1) % RING;
            self.long_len = (self.long_len + 1).min(RING);
        }
    }

    fn clear(&mut self) {
        self.mark_len = 0;
        self.mark_pos = 0;
        self.gap_len = 0;
        self.gap_pos = 0;
        self.long_len = 0;
        self.long_pos = 0;
        self.char_gap = 0.0;
        self.primed = false;
        self.ratio = 1.0;
    }

    /// Unit taken from the mark durations.
    fn unit_from_marks(&mut self) -> Option<f32> {
        if self.mark_len < MIN_DURATIONS {
            return None;
        }
        let n = self.mark_len;
        self.scratch[..n].copy_from_slice(&self.marks[..n]);
        sort(&mut self.scratch[..n]);
        let short = percentile(&self.scratch[..n], 0.20);
        let long = percentile(&self.scratch[..n], 0.80);
        if short <= 0.0 {
            return None;
        }
        self.ratio = long / short;

        // Two clusters roughly three apart: the lower one is the unit. The
        // percentiles rather than the extremes keep a single stray duration from
        // moving the answer.
        if self.ratio >= 2.0 && self.ratio <= 4.5 {
            return Some(short);
        }

        // One cluster only, so the classification is undecidable from marks
        // alone. Reading the cluster as long elements is what creates the stable
        // wrong solution; reading it as units cannot lock, because a run of pure
        // long elements is rare and corrects itself as soon as a short one
        // arrives.
        Some(percentile(&self.scratch[..n], 0.50))
    }

    /// Unit taken from the gaps.
    ///
    /// The gap inside a character is exactly one unit and is present in every
    /// character of more than one element, so it outnumbers the character and
    /// word gaps in any real text. It is also the one measurement a Farnsworth
    /// transmission leaves alone, and it needs no classification, which is why
    /// it is trusted when the two estimates disagree.
    fn unit_from_gaps(&mut self) -> Option<f32> {
        if self.gap_len < MIN_DURATIONS {
            return None;
        }
        let n = self.gap_len;
        self.scratch[..n].copy_from_slice(&self.gaps[..n]);
        sort(&mut self.scratch[..n]);
        let unit = percentile(&self.scratch[..n], 0.20);
        if unit <= 0.0 {
            None
        } else {
            Some(unit)
        }
    }

    /// Gap between characters, taken from the long gap population.
    ///
    /// A low percentile rather than the median, because the population holds two
    /// clusters: the character gaps and the word gaps, and there are several
    /// characters per word. The lower cluster is the one wanted, and taking it
    /// low keeps the word gaps out of it whatever the word length happens to be.
    fn refresh_char_gap(&mut self) {
        if self.long_len < MIN_LONG_GAPS {
            return;
        }
        let n = self.long_len;
        self.scratch[..n].copy_from_slice(&self.long_gaps[..n]);
        sort(&mut self.scratch[..n]);
        let gap = percentile(&self.scratch[..n], 0.25);
        if gap > 0.0 {
            self.char_gap = gap;
        }
    }

    fn refresh(&mut self, tracking: f32) {
        let from_marks = self.unit_from_marks();
        let from_gaps = self.unit_from_gaps();
        let unit = match (from_marks, from_gaps) {
            (Some(m), Some(g)) => {
                // The analysis window widens a mark and narrows the gap beside
                // it by the same amount, so averaging the two cancels that bias.
                let spread = if m > g { m / g } else { g / m };
                if spread <= 2.0 {
                    0.5 * (m + g)
                } else {
                    g
                }
            }
            (Some(m), None) => m,
            (None, Some(g)) => g,
            (None, None) => return,
        };
        let wanted = unit.clamp(self.dot_min, self.dot_max);

        // Smoothed rather than assigned. The estimate is recomputed on every
        // accepted element, so an unsmoothed one moves on the jitter of a single
        // duration, and every move that is large enough replans the analysis
        // window and rebuilds the level geometry underneath the decision that
        // caused it.
        //
        // Snapped on the first estimate, see the note on the flag.
        let rate = tracking.clamp(0.0, 1.0);
        if !self.primed || rate >= 0.999 {
            self.dot = wanted;
            self.primed = true;
        } else if rate > 0.0 {
            self.dot += rate * (wanted - self.dot);
        }

        self.refresh_char_gap();
    }

    /// Confidence that the mark durations are bimodal three units apart.
    fn bimodality(&self) -> f32 {
        if self.mark_len < MIN_DURATIONS {
            return 0.0;
        }
        (1.0 - (self.ratio - 3.0).abs() / 1.5).clamp(0.0, 1.0)
    }
}

// ----------------------------------------------------------------- decoder

pub struct MorseDecoder {
    bank: ToneBank,
    rate: u32,
    /// True when the input carries a quadrature pair.
    ///
    /// Held because the analysis window is replanned whenever the working speed
    /// moves, and a bank rebuilt in the one sided arrangement would fold the
    /// spectrum back onto itself: every channel below the tuning point would
    /// jump to its mirror in the middle of a transmission.
    complex: bool,
    /// Window length the detector currently runs at, in samples.
    n: usize,
    /// Longest window the requested bandwidth allows, in samples.
    by_bandwidth: usize,
    /// Set when the width changed and the plan has to be redone whatever the
    /// hysteresis says.
    ///
    /// The hysteresis exists to keep a jittering speed estimate from rebuilding
    /// the detector, and a stated width is not an estimate: an operator moving
    /// the control expects the filter to move with it.
    force_replan: bool,
    /// Dot length of the working speed, in seconds. The window is planned around
    /// this whenever the tracked estimate has not earned the right to replace it.
    nominal_dot: f32,
    dt: f32,
    /// Equivalent noise bandwidth of the composite, which is what the operator
    /// sets and what the display shades.
    bandwidth_hz: f32,
    /// Distance of the outer bins from the centre.
    spacing_hz: f32,
    /// Frequency the operator or the tracker stated. The correction is measured
    /// from here and bounded relative to it, so the loop cannot walk away onto a
    /// neighbouring signal however long it runs.
    anchor_hz: f32,
    /// Correction the loop accumulated, in hertz.
    afc_hz: f32,

    levels: Levels,
    /// Filtered envelope of the current frame.
    env: f32,

    key: bool,
    /// Seconds spent in the current keying state.
    run: f32,
    timing: Timing,
    /// Unit the level geometry was computed for.
    tuned_for: f32,

    pattern: String,
    /// True once the current gap has produced a word space, so a long pause does
    /// not emit a run of them.
    space_emitted: bool,
    /// True once the current gap has flushed a character.
    char_flushed: bool,

    duty_avg: f32,
    /// Share of random patterns of each length the alphabet holds.
    chance: [f32; MAX_PATTERN + 1],
    /// Decayed counters behind the quality figure.
    ///
    /// Three rather than two, and the third is what makes the figure mean
    /// anything. A raw match rate is not evidence: every pattern up to three
    /// elements is in the table, so noise assembling short patterns scores a
    /// hundred per cent on an empty band. The expectation counter accumulates
    /// what a random stream would have matched, and the quality is the excess
    /// over it, which reads nought on noise by construction.
    matched: f32,
    expected: f32,
    counted: f32,
    singles: f32,

    stats: KeyingStats,
    out: String,
}

impl MorseDecoder {
    pub fn new(rate: u32, cfg: &MorseSettings) -> MorseDecoder {
        let fs = rate.max(1) as f32;
        let by_bandwidth = bandwidth_window(fs, cfg.filter_bandwidth_hz);
        let wpm = working_wpm(cfg);
        let nominal_dot = 1.2 / wpm;
        let by_speed = speed_window(fs, nominal_dot);
        let widest = ((by_bandwidth as f32 / MAX_WIDENING) as usize).max(MIN_WINDOW);
        let n = by_bandwidth
            .min(by_speed)
            .max(widest)
            .clamp(MIN_WINDOW, MAX_WINDOW);
        let hop = (n / 4).max(1);

        let single = 4.0 * fs / n as f32;
        let spacing_hz = single * BIN_SPACING_FRACTION;
        let bandwidth_hz = COMPOSITE_ENBW * fs / n as f32;

        let bank = ToneBank::new(rate, n, hop, &composite(cfg.tone_hz, spacing_hz));
        let dt = bank.frame_seconds();

        if bandwidth_hz > cfg.filter_bandwidth_hz * 1.1 {
            crate::log_info!(
                "decode",
                "cw bandwidth widened from {:.0} to {:.0} Hz so an element at {:.0} wpm still resolves",
                cfg.filter_bandwidth_hz,
                bandwidth_hz,
                wpm
            );
        }
        crate::log_debug!(
            "decode",
            "cw detector at {:.0} Hz, {} bins spaced {:.0} Hz, window {} samples, \
             {:.0} Hz noise bandwidth, {:.2} ms frames",
            cfg.tone_hz,
            DETECTOR_BINS,
            spacing_hz,
            n,
            bandwidth_hz,
            dt * 1000.0
        );

        let mut decoder = MorseDecoder {
            bank,
            rate,
            complex: false,
            n,
            by_bandwidth,
            force_replan: false,
            nominal_dot,
            dt,
            bandwidth_hz,
            spacing_hz,
            anchor_hz: cfg.tone_hz,
            afc_hz: 0.0,
            levels: Levels::new(),
            env: 0.0,
            key: false,
            run: 0.0,
            timing: Timing::new(cfg),
            tuned_for: 0.0,
            pattern: String::with_capacity(8),
            space_emitted: true,
            char_flushed: true,
            duty_avg: 0.0,
            chance: chance_table(),
            matched: 0.0,
            expected: 0.0,
            counted: 0.0,
            singles: 0.0,
            stats: KeyingStats::default(),
            out: String::with_capacity(64),
        };
        decoder.retime();
        decoder
    }

    /// Rebuilds the detector when the speed it is sized for no longer fits.
    ///
    /// A window sized for twenty words per minute smears an element at forty,
    /// and a window sized for forty throws away three decibels at twenty. The
    /// hysteresis is a quarter of the current length in both directions, which is
    /// far more than the estimate jitters inside one transmission, so a rebuild
    /// only happens when the traffic really changed.
    ///
    /// The tracked estimate is used only once it has demonstrated that the mark
    /// durations are bimodal. An estimate taken from noise is a number like any
    /// other and would otherwise shorten the window, widen the filter, admit more
    /// noise and confirm itself. Until then the working speed stands in, which is
    /// a stated intention rather than a measurement and cannot drift.
    fn replan(&mut self) {
        let fs = self.rate.max(1) as f32;
        let credible = self.timing.bimodality() >= CREDIBLE_BIMODALITY;
        let dot = if credible { self.timing.dot } else { self.nominal_dot };

        let allowance = speed_window(fs, dot);
        let widest = ((self.by_bandwidth as f32 / MAX_WIDENING) as usize).max(MIN_WINDOW);
        let target = self
            .by_bandwidth
            .min(allowance)
            .max(widest)
            .clamp(MIN_WINDOW, MAX_WINDOW);

        let grow = target * 4 > self.n * 5;
        let shrink = target * 5 < self.n * 4;
        let forced = std::mem::take(&mut self.force_replan);
        if !forced && !grow && !shrink {
            return;
        }

        let hop = (target / 4).max(1);
        let single = 4.0 * fs / target as f32;
        self.spacing_hz = single * BIN_SPACING_FRACTION;
        self.bandwidth_hz = COMPOSITE_ENBW * fs / target as f32;
        self.bank = ToneBank::new(
            self.rate,
            target,
            hop,
            &composite(self.anchor_hz + self.afc_hz, self.spacing_hz),
        );
        self.bank.set_complex(self.complex);
        self.dt = self.bank.frame_seconds();
        self.n = target;
        // The level geometry is expressed in frames, so a new frame period has
        // to be pushed through it.
        self.tuned_for = 0.0;

        crate::log_debug!(
            "decode",
            "cw window replanned to {} samples, {:.0} Hz noise bandwidth, {:.0} wpm, {}",
            target,
            self.bandwidth_hz,
            1.2 / dot.max(1e-4),
            if credible { "tracked" } else { "nominal" }
        );
    }

    /// Reapplies the detector geometry when the unit estimate moved far enough
    /// to matter. The guard keeps a jittering estimate from doing any work.
    fn retime(&mut self) {
        self.replan();
        let dot = self.timing.dot;
        if (dot - self.tuned_for).abs() < self.tuned_for * 0.05 {
            return;
        }
        self.tuned_for = dot;
        self.levels.configure(dot, self.dt);
    }

    /// Frequency the detector is actually centred on, anchor plus correction.
    pub fn tone_hz(&self) -> f32 {
        self.anchor_hz + self.afc_hz
    }

    /// Frequency the detector was pointed at, before any correction.
    pub fn anchor_hz(&self) -> f32 {
        self.anchor_hz
    }

    /// Correction the tracking loop is currently applying.
    pub fn afc_offset_hz(&self) -> f32 {
        self.afc_hz
    }

    /// Equivalent noise bandwidth of the composite detector.
    pub fn bandwidth_hz(&self) -> f32 {
        self.bandwidth_hz
    }

    /// Sets the requested width of this detector alone.
    ///
    /// Per detector rather than per bank, because two stations on one band are
    /// rarely the same width: one is a machine at forty words a minute and the
    /// next is a hand key at twelve, and a width that suits either is wrong for
    /// the other. A bank wide setting also forced a rebuild of every channel on
    /// every step of the control, which discarded the level and timing estimates
    /// of channels the operator was not adjusting.
    pub fn set_bandwidth(&mut self, hz: f32) {
        let fs = self.rate.max(1) as f32;
        let wanted = bandwidth_window(fs, hz);
        if wanted == self.by_bandwidth {
            return;
        }
        self.by_bandwidth = wanted;
        self.force_replan = true;
        self.retime();
    }

    /// States whether the input carries a quadrature pair.
    ///
    /// Everything measured so far is discarded, because the same samples mean a
    /// different spectrum once the second channel carries information: a level
    /// that was the sum of two stations becomes the level of one.
    pub fn set_complex(&mut self, complex: bool) {
        if complex == self.complex {
            return;
        }
        self.complex = complex;
        self.bank.set_complex(complex);
        self.timing.clear();
        self.levels.clear();
        self.pattern.clear();
    }

    /// Points the detector at a frequency.
    ///
    /// A move small enough to stay inside the composite passband is treated as a
    /// correction of the same signal: the accumulated timing and level estimates
    /// stay, because they still describe it. Only a move beyond the passband
    /// means a different station, and only then is everything discarded.
    pub fn set_tone(&mut self, hz: f32) {
        let delta = (hz - self.anchor_hz).abs();
        if delta < 0.5 {
            return;
        }
        let different_signal = delta > self.bandwidth_hz * 0.5;

        self.anchor_hz = hz;
        // The correction was measured against the old anchor and means nothing
        // against the new one.
        self.afc_hz = 0.0;
        self.bank.set_frequencies(&composite(hz, self.spacing_hz));

        if different_signal {
            self.timing.clear();
            self.levels.clear();
            self.pattern.clear();
        }
    }

    pub fn stats(&self) -> KeyingStats {
        self.stats
    }

    pub fn wpm(&self) -> f32 {
        1.2 / self.timing.dot.max(1e-4)
    }

    /// Consumes samples and appends whatever text they produced.
    pub fn feed(&mut self, input: &[Complex], cfg: &MorseSettings) {
        // The working speed is what the filter falls back to whenever the tracked
        // estimate is not credible, so it follows the control even while
        // automatic tracking is on.
        self.nominal_dot = 1.2 / working_wpm(cfg);

        // A manual speed is applied here rather than at construction, so moving
        // the control takes effect without rebuilding the detector.
        if !cfg.auto_speed {
            let wanted = self.nominal_dot;
            if (wanted - self.timing.dot).abs() > self.timing.dot * 0.02 {
                self.timing.dot = wanted;
                self.retime();
            }
        }

        self.bank.feed(input);
        while self.bank.next_frame() {
            // The amplitudes are copied out so the borrow of the bank ends
            // before the frame handler takes the decoder mutably, which also
            // lets the handler replace the bank outright.
            let amps = {
                let a = self.bank.amps();
                [
                    a.first().copied().unwrap_or(0.0),
                    a.get(1).copied().unwrap_or(0.0),
                    a.get(2).copied().unwrap_or(0.0),
                ]
            };
            self.on_frame(amps, cfg);
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
        self.key = false;
        self.run = 0.0;
        self.afc_hz = 0.0;
        self.bank.set_frequencies(&composite(self.anchor_hz, self.spacing_hz));
        self.pattern.clear();
        self.space_emitted = true;
        self.char_flushed = true;
        self.env = 0.0;
        self.levels.clear();
        self.timing.clear();
        self.matched = 0.0;
        self.expected = 0.0;
        self.counted = 0.0;
        self.singles = 0.0;
        self.stats.quality = 0.0;
        self.stats.quality_known = false;
        self.stats.single_ratio = 0.0;
        self.stats.char_gap_units = 0.0;
        self.stats.bimodal = 0.0;
        self.stats.snr_db = 0.0;
        self.stats.gate = Gate::Warmup;
    }

    fn on_frame(&mut self, amps: [f32; DETECTOR_BINS], cfg: &MorseSettings) {
        let (lo, mid, hi) = (amps[0], amps[1], amps[2]);

        // Power sum across the composite. Summing powers rather than amplitudes
        // is what makes the response flat: the individual lobes overlap at their
        // half power points, so the total stays level between the outer centres
        // and a tuning error of tens of hertz costs a fraction of a decibel.
        let raw = (lo * lo + mid * mid + hi * hi).sqrt();

        self.env = self.levels.push(raw);
        let e = self.env;

        let floor = self.levels.floor;
        let peak = self.levels.peak();
        let span = peak - floor;

        // Levels are only meaningful once the floor window has filled. Publishing
        // them earlier reports the difference against an uninitialized floor,
        // which comes out as an impossible ratio and is then read by the
        // classifier as evidence of a perfect signal.
        if self.levels.ready {
            self.stats.level_db = amp_to_db(peak);
            self.stats.floor_db = amp_to_db(floor.max(1e-8));
            // The ceiling is above anything a sound card can deliver and below
            // anything the logarithm floor can produce, so it catches a bad
            // estimate without ever clipping a real reading.
            self.stats.snr_db = (self.stats.level_db - self.stats.floor_db).clamp(0.0, 90.0);
        } else {
            self.stats.level_db = amp_to_db(e);
            self.stats.floor_db = self.stats.level_db;
            self.stats.snr_db = 0.0;
        }

        // Squelch. Several independent reasons to stay shut, each of which alone
        // is enough to make the keying decision meaningless. The first one that
        // fires is published, because that is the one the operator has to clear
        // before any of the others can even be evaluated.
        let contrast = span / floor.max(1e-9);
        self.stats.gate = if !cfg.enabled {
            Gate::Disabled
        } else if !self.levels.ready {
            Gate::Warmup
        } else if self.stats.level_db < cfg.squelch_db {
            Gate::Level
        } else if self.stats.snr_db < cfg.min_snr_db {
            Gate::Snr
        } else if contrast < 0.25 {
            Gate::Flat
        } else {
            Gate::Open
        };
        let squelched = !self.stats.gate.is_open();

        // The threshold decision, taken whether or not the gate is shut.
        //
        // Slightly below the midpoint, because the analysis window rounds the
        // element edges and a centred threshold clips both ends of every mark.
        // The hysteresis suppresses chatter on the slopes without hiding a short
        // element.
        let crossed = if cfg.adaptive_threshold {
            let mid = floor + span * 0.45;
            let hyst = span * 0.10;
            if self.key {
                e > mid - hyst
            } else {
                e > mid + hyst
            }
        } else {
            amp_to_db(e) > cfg.squelch_db
        };
        let want = crossed && !squelched;

        // Measured from the threshold rather than from the gated decision, so
        // the readout describes the envelope rather than describing the squelch.
        // An operator setting a threshold reads the duty to judge whether there
        // is keying underneath it, and a figure that is nought by construction
        // whenever the gate is shut answers nothing at all.
        let target = if crossed { 1.0 } else { 0.0 };
        self.duty_avg += coefficient(1.0, self.dt) * (target - self.duty_avg);
        self.stats.duty = self.duty_avg;

        // Frequency tracking.
        //
        // The outer bins respond symmetrically only when the tone sits exactly
        // between them, so their normalized imbalance is a signed measure of the
        // error. It is read only while the tone is present and well above the
        // noise: in a gap the bins carry noise alone and the reading is a random
        // walk that would pull the detector off the signal.
        if cfg.afc && want && self.levels.ready && self.stats.snr_db > 10.0 {
            let total = lo + mid + hi;
            if total > 1e-6 {
                let imbalance = (hi - lo) / total;
                let error_hz = imbalance / AFC_SLOPE * self.spacing_hz;
                let limit = cfg.capture_range_hz.min(self.bandwidth_hz * 0.5);
                self.afc_hz = (self.afc_hz + AFC_GAIN * error_hz).clamp(-limit, limit);
                self.bank
                    .set_frequencies(&composite(self.anchor_hz + self.afc_hz, self.spacing_hz));
            }
        }

        if want != self.key {
            let length = self.run;
            self.run = 0.0;
            if self.key {
                self.finish_mark(length, cfg);
            } else {
                self.finish_gap(length);
            }
            self.key = want;
        }
        self.run += self.dt;

        if !self.key {
            // Character and word boundaries are decided on elapsed silence
            // rather than on the next tone, so text appears as it arrives.
            let dot = self.timing.dot;
            let char_threshold = dot * cfg.char_gap_factor;
            if !self.char_flushed && !self.pattern.is_empty() && self.run > char_threshold {
                self.emit_char(cfg);
                self.char_flushed = true;
            }

            // Farnsworth stretches the gaps between characters and words and
            // leaves the gap inside a character alone, so the unit stays correct
            // and the word boundary does not: at eighteen over eight the
            // character gap is longer than the stated five units, and every
            // letter becomes a word.
            //
            // Measured against the observed character gap it works for both. An
            // ordinary transmission has a character gap of three units, and the
            // ratio then gives five and a half, which is where the stated default
            // already sits.
            let word_threshold = if cfg.farnsworth_aware && self.timing.char_gap > 0.0 {
                // Never below the character threshold. A word boundary at or
                // under it would fire on every character gap, which is the very
                // failure this exists to remove.
                (self.timing.char_gap * WORD_OVER_CHAR).max(char_threshold * 1.5)
            } else {
                dot * cfg.word_gap_factor
            };
            if !self.space_emitted && self.run > word_threshold {
                if !self.out.is_empty() || self.stats.elements > 0 {
                    self.out.push(' ');
                }
                self.space_emitted = true;
            }
        }
    }

    fn finish_mark(&mut self, length: f32, cfg: &MorseSettings) {
        // The absolute range comes from the speed limits the operator set, not
        // from the current estimate, so a wrong estimate cannot admit the
        // durations that would keep it wrong.
        if !self.timing.accepts(length) {
            self.stats.rejects += 1;
            // A mark far too long is a carrier or a tuning signal, and the
            // partial character it interrupted is worthless.
            if length > self.timing.accept_max {
                self.pattern.clear();
            }
            return;
        }

        // The measurement is kept whatever happens below, so a unit estimate
        // that is too low can still be corrected by the durations that expose
        // it. Only the emission decision is taken relative to the estimate.
        self.timing.push_mark(length);
        if cfg.auto_speed {
            self.timing.refresh(cfg.speed_tracking);
            self.retime();
        }
        self.stats.char_gap_units = if self.timing.char_gap > 0.0 {
            self.timing.char_gap / self.timing.dot.max(1e-4)
        } else {
            0.0
        };

        let dot = self.timing.dot;
        if length > dot * MAX_ELEMENT_UNITS {
            // Two elements whose gap was missed, or the tail of a carrier.
            // Emitting it as a dash would put a wrong character on the screen;
            // dropping the partial pattern at least keeps the error local.
            self.stats.rejects += 1;
            self.pattern.clear();
            return;
        }

        self.pattern.push(if length > dot * ELEMENT_SPLIT { '-' } else { '.' });
        self.stats.elements += 1;
        self.stats.bimodal = self.timing.bimodality();
        self.stats.wpm = self.wpm();

        // Eight elements is longer than any table entry; a longer run means the
        // boundary detection lost the thread.
        if self.pattern.len() > 8 {
            self.pattern.clear();
            self.stats.rejects += 1;
        }
    }

    fn finish_gap(&mut self, length: f32) {
        // Gaps feed the second unit anchor. The character and word decisions were
        // already taken by the timeout logic, so only the measurement is left.
        if self.timing.accepts(length) {
            self.timing.push_gap(length);
        }
        self.space_emitted = false;
        self.char_flushed = false;
    }

    fn emit_char(&mut self, cfg: &MorseSettings) {
        let pattern = std::mem::take(&mut self.pattern);
        if pattern.is_empty() {
            return;
        }

        // Prosigns are checked first: several are longer than any character and
        // would otherwise count as a failed lookup.
        let prosign = if cfg.show_prosigns {
            PROSIGNS.iter().find(|&&(p, _)| p == pattern).map(|&(_, name)| name)
        } else {
            None
        };
        let letter = TABLE.iter().find(|&&(p, _)| p == pattern).map(|&(_, ch)| ch);
        let found = prosign.is_some() || letter.is_some();

        // Decay applies on every decision, so the figures track the recent past
        // rather than the whole session.
        const DECAY: f32 = 0.97;
        self.matched *= DECAY;
        self.expected *= DECAY;
        self.counted *= DECAY;
        self.singles *= DECAY;

        // Every pattern takes part, short ones included. Their chance is one, so
        // they add the same amount to both sides of the ratio below and move it
        // nowhere; excluding them by hand would be the same arithmetic written
        // twice, and it would leave the four element patterns, whose chance is
        // three quarters, counting as full evidence.
        let len = pattern.len().min(MAX_PATTERN);
        self.counted += 1.0;
        self.expected += self.chance[len];
        if found {
            self.matched += 1.0;
        }
        if len < 2 {
            self.singles += 1.0;
        }

        // How much of what arrived a random stream could not have produced. Two
        // patterns of real evidence is a handful of characters of ordinary text,
        // and on noise it accumulates just as slowly, which is why the figure is
        // reported as unknown rather than as nought until then.
        let evidence = self.counted - self.expected;
        self.stats.quality_known = evidence >= 2.0;
        self.stats.quality = if self.stats.quality_known {
            ((self.matched - self.expected) / evidence).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.stats.single_ratio = if self.counted > 0.5 {
            self.singles / self.counted
        } else {
            0.0
        };

        if let Some(name) = prosign {
            self.out.push_str(name);
            return;
        }

        match letter {
            Some(ch) => {
                let ch = match cfg.output_case {
                    TextCase::Upper => ch.to_ascii_uppercase(),
                    TextCase::Lower => ch.to_ascii_lowercase(),
                    TextCase::AsReceived => ch,
                };
                self.out.push(ch);
            }
            None => {
                self.stats.rejects += 1;
                if cfg.mark_dropouts {
                    self.out.push('*');
                }
            }
        }
    }
}