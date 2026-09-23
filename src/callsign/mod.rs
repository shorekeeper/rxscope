//! Call sign extraction and the spot list.
//!
//! ## What a skimmer produces
//!
//! Not text. Text is what a decoder produces, and a decoder is one component of
//! this. A skimmer produces a list: who was heard, on what frequency, how
//! strongly, at what speed, when. Unique entries rather than lines, because the
//! same station repeats its call several times per transmission and an operator
//! deciding where to point the receiver wants one row per station.
//!
//! Reading that list out of a running text panel is what the operator is
//! currently obliged to do, and it is the one job a machine does better: the
//! call is a structured token, the frequency is already recorded per line, and
//! the country follows from a table.
//!
//! ## Extraction
//!
//! A state machine over token structure rather than a pattern match over
//! characters. Keying arrives with elements missing and characters merged, and a
//! pattern loose enough to catch a real call is loose enough to catch a signal
//! report as well. The structure is narrow: a prefix carrying at least one
//! letter, then a digit, then one to four letters, with the whole thing
//! delimited and optionally carrying portable qualifiers behind a solidus.
//!
//! False positives are the expensive failure, not false negatives. A station
//! repeats its call, so a missed instance costs nothing and is recovered on the
//! next repetition; an invented call sits in the list until it ages out and the
//! operator has already tried to work it.
//!
//! ## Two markers that matter
//!
//! A line holding the call for contacts and a call following the station
//! separator are different facts. The first says the station wants a contact,
//! which is what makes it worth pointing a receiver at; the second names who is
//! transmitting rather than who is being called, which is what stops the list
//! filling with the call signs of stations that were merely addressed. Both are
//! recorded per spot and neither is inferred from the other.

pub mod cty;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::settings::{CallsignSettings, CallsignSource};
use crate::core::Instant;

use cty::{Prefixes, Resolved};

/// Longest token examined.
///
/// A call with two qualifiers reaches about fifteen characters, and nothing
/// longer is a call. The bound also stops a corrupt run of characters from being
/// walked as a candidate.
const MAX_TOKEN: usize = 16;

/// Frequency within which two sightings are one station, in hertz.
///
/// The receiver measures a carrier to a few hertz, and a station drifts by tens
/// over a transmission. Wider than that would merge two stations working close
/// together, which on a crowded band is the common case.
const SAME_STATION_HZ: f64 = 250.0;

/// Seconds a spot survives without being heard again.
///
/// Half an hour covers a band session: a station heard at the start is still
/// worth knowing about when the operator comes back round. Beyond that the
/// propagation has changed and the entry is a claim about a band that no longer
/// exists.
const RETIRE_S: f64 = 1800.0;

/// Spots held at once.
///
/// A busy contest evening produces a few hundred. The cap exists so a session
/// left running overnight does not grow without bound, and the oldest is
/// discarded first because that is the one the operator has already read.
const MAX_SPOTS: usize = 400;

/// Appends checked against the history limit every so many spots.
///
/// Trimming reads the whole file, so it is paid on a counter rather than per
/// spot. A quarter of the limit bounds the overshoot to something nobody
/// notices.
const TRIM_EVERY: usize = 256;

/// Tokens that pass the structure test and are not call signs.
///
/// Short and closed on purpose. The structure test rejects almost everything by
/// itself: an abbreviation carries no digit, a signal report carries no leading
/// letter, a serial number carries no letter at all. What remains is the handful
/// of forms that genuinely look like a call and are not one.
const NOT_CALLS: &[&str] = &["QRZ", "QSL", "CQ", "DE", "5NN", "599", "K3", "K9"];

/// One recognized call sign inside a line of text.
#[derive(Debug, Clone, Copy)]
pub struct Found {
    /// Byte range inside the line, so the drawing can colour it in place.
    pub start: usize,
    pub end: usize,
    /// True when the token before this one was the station separator, which
    /// names the transmitting station rather than the one being addressed.
    pub from_de: bool,
    /// True when the line holds the call for contacts.
    pub cq: bool,
}

/// One station heard.
#[derive(Debug, Clone)]
pub struct Spot {
    /// Call as it was received, qualifiers included.
    pub call: String,
    /// Audio frequency of the channel that produced it.
    pub hz: f32,
    /// Frequency on the air, nought while no dial reading was available.
    ///
    /// Recorded rather than derived at display time, because the dial moves and
    /// a spot names where the station was when it was heard.
    pub rf_hz: i64,
    /// Coordinated time of the first and the latest sighting.
    pub first: (u16, u16, u16),
    pub last: (u16, u16, u16),
    /// Monotonic instant of the latest sighting, for the age and the retirement.
    seen: Instant,
    /// Sightings, which separates a station calling repeatedly from a single
    /// instance that may well be a misread.
    pub count: u32,
    pub snr_db: f32,
    pub wpm: f32,
    /// True once the call was seen after the station separator.
    pub from_de: bool,
    /// True once it was seen in a line calling for contacts.
    pub cq: bool,
    /// Resolution, absent when no database is loaded or resolution is off.
    pub country: String,
    pub dxcc: String,
    pub cq_zone: u8,
    pub itu_zone: u8,
    pub continent: String,
    /// Note from the operator maintained list, empty when there is none.
    pub note: String,
}

impl Spot {
    /// Seconds since the latest sighting.
    pub fn age_s(&self) -> f64 {
        self.seen.elapsed_secs()
    }

    /// Confidence that the entry is a real station rather than a misread.
    ///
    /// Three independent pieces of evidence and they are weighted by how hard
    /// each is to produce by accident. A call after the station separator is
    /// almost impossible to invent, because the separator itself has to decode
    /// correctly first. Repetition is next: a corrupt token twice in the same
    /// form is unlikely. The call for contacts is the weakest, because it is two
    /// characters and decodes readily out of noise.
    pub fn confidence(&self) -> f32 {
        let mut score = 0.25f32;
        if self.from_de {
            score += 0.45;
        }
        if self.count > 1 {
            score += 0.20;
        }
        if self.count > 3 {
            score += 0.05;
        }
        if self.cq {
            score += 0.05;
        }
        score.min(1.0)
    }
}

/// Trims the punctuation a decoder leaves around a token.
///
/// The solidus survives because it is part of a portable call. Everything else
/// that is not alphanumeric is either sent punctuation or a dropout marker, and
/// neither belongs in the token being classified.
fn trim(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/')
}

/// True when the part has the shape of a call sign core.
///
/// The digit that separates the prefix from the suffix is the last digit in the
/// part, which is what makes a suffix carrying no digit unambiguous and handles
/// the prefixes that hold two.
fn is_core(part: &str) -> bool {
    let bytes = part.as_bytes();
    if bytes.len() < 3 || bytes.len() > 8 {
        return false;
    }
    if !bytes.iter().all(|b| b.is_ascii_alphanumeric()) {
        return false;
    }

    let split = match bytes.iter().rposition(|b| b.is_ascii_digit()) {
        Some(at) => at,
        None => return false,
    };

    let prefix = &bytes[..split];
    let suffix = &bytes[split + 1..];

    // A prefix of three characters is real, as in the southern African
    // allocations, and one of four is not.
    if prefix.is_empty() || prefix.len() > 3 {
        return false;
    }
    if !prefix.iter().any(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    // A suffix carrying a digit exists in special event calls and is rare
    // enough that admitting it would cost more in false positives than it
    // recovers.
    if suffix.is_empty() || suffix.len() > 4 || !suffix.iter().all(|b| b.is_ascii_alphabetic()) {
        return false;
    }
    true
}

/// True when the part is a plausible qualifier.
fn is_qualifier(part: &str) -> bool {
    let bytes = part.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 4
        && bytes.iter().all(|b| b.is_ascii_alphanumeric())
}

/// True when the token is a call sign.
pub fn is_callsign(token: &str, min_len: usize) -> bool {
    if token.len() < min_len.max(3) || token.len() > MAX_TOKEN {
        return false;
    }
    if !token.is_ascii() {
        return false;
    }
    let upper = token.to_ascii_uppercase();
    if NOT_CALLS.contains(&upper.as_str()) {
        return false;
    }

    let mut parts = 0usize;
    let mut cores = 0usize;
    for part in upper.split('/') {
        parts += 1;
        if parts > 3 {
            return false;
        }
        if is_core(part) {
            cores += 1;
        } else if !is_qualifier(part) {
            return false;
        }
    }
    cores >= 1
}

/// Part of a call the country lookup is keyed on.
///
/// A leading part shorter than the core is the allocation the station is
/// operating from, which is what decides the country: a German operator in the
/// Canary Islands is a Canary Islands station for every purpose a spot serves.
/// A trailing part is a portable indicator and says nothing about where.
pub fn lookup_key(call: &str) -> String {
    let upper = call.to_ascii_uppercase();
    let parts: Vec<&str> = upper.split('/').collect();
    if parts.len() < 2 {
        return upper;
    }

    let core = parts
        .iter()
        .filter(|p| is_core(p))
        .max_by_key(|p| p.len())
        .copied()
        .unwrap_or(parts[0]);

    let first = parts[0];
    if first != core && first.len() < core.len() {
        return first.to_string();
    }
    core.to_string()
}

/// Finds every call sign in one line.
///
/// The partial flag holds back the final token, which is still growing while a
/// line is being assembled: a call is not distinguishable from its own prefix
/// until the character after it has arrived.
pub fn extract(text: &str, min_len: usize, partial: bool, out: &mut Vec<Found>) {
    out.clear();
    if text.is_empty() {
        return;
    }

    // The call for contacts is a property of the line rather than of a token, so
    // it is settled before anything is emitted. Scanned as a delimited token so
    // it is not found inside a word.
    let upper = text.to_ascii_uppercase();
    let cq = upper.split_whitespace().any(|t| trim(t) == "CQ");

    let ends_open = partial && !text.ends_with(char::is_whitespace);
    let count = text.split_whitespace().count();

    let mut previous_de = false;
    for (index, raw) in text.split_whitespace().enumerate() {
        if ends_open && index + 1 == count {
            break;
        }
        let token = trim(raw);
        if token.is_empty() {
            continue;
        }

        let folded = token.to_ascii_uppercase();
        if folded == "DE" {
            previous_de = true;
            continue;
        }

        if is_callsign(token, min_len) {
            // The byte range is found by offset rather than by search, so a
            // call repeated in the same line colours the instance that was
            // matched rather than the first one.
            let start = raw.as_ptr() as usize - text.as_ptr() as usize;
            let inner = token.as_ptr() as usize - raw.as_ptr() as usize;
            out.push(Found {
                start: start + inner,
                end: start + inner + token.len(),
                from_de: previous_de,
                cq,
            });
        }
        previous_de = false;
    }
}

/// Everything the resolution needs, plus the list it produces.
pub struct Book {
    prefixes: Prefixes,
    /// Notes from the operator maintained list, keyed by call.
    local: HashMap<String, String>,
    /// Resolutions already computed.
    ///
    /// The lookup is a handful of hash probes, so the memo is not there for
    /// speed: it is there because a resolution allocates four strings, and a
    /// station repeating its call would allocate them on every sighting.
    memo: HashMap<String, Option<Resolved>>,
    memo_limit: usize,
    spots: Vec<Spot>,
    /// Where the history is appended, absent when it is switched off.
    history: Option<PathBuf>,
    history_lines: usize,
    history_limit: usize,
    appends: usize,
    /// Scratch, so a scan does not allocate per line.
    found: Vec<Found>,
}

impl Book {
    pub fn new(settings: &CallsignSettings) -> Book {
        let mut book = Book {
            prefixes: Prefixes::empty(),
            local: HashMap::new(),
            memo: HashMap::new(),
            memo_limit: settings.cache_entries.max(64) as usize,
            spots: Vec::with_capacity(64),
            history: None,
            history_lines: 0,
            history_limit: settings.history_limit as usize,
            appends: 0,
            found: Vec::with_capacity(8),
        };
        book.reload(settings);
        book
    }

    /// Rereads whatever the settings name.
    ///
    /// The memo is discarded, because an entry in it was resolved through a
    /// database that may have just been replaced.
    pub fn reload(&mut self, settings: &CallsignSettings) {
        self.memo.clear();
        self.memo_limit = settings.cache_entries.max(64) as usize;
        self.history_limit = settings.history_limit as usize;

        self.prefixes = if settings.lookup_enabled && settings.source == CallsignSource::Cty {
            let path = crate::record::resolve(&settings.prefix_db_path);
            if path.is_file() {
                Prefixes::load(&path)
            } else {
                crate::log_info!(
                    "callsign",
                    "{} is absent, calls will not be resolved",
                    path.display()
                );
                Prefixes::empty()
            }
        } else {
            Prefixes::empty()
        };

        self.local.clear();
        if settings.lookup_enabled && !settings.local_db_path.is_empty() {
            let path = crate::record::resolve(&settings.local_db_path);
            self.load_local(&path);
        }

        self.history = if settings.history_path.is_empty() {
            None
        } else {
            let path = crate::record::resolve(&settings.history_path);
            self.history_lines = count_lines(&path);
            Some(path)
        };
    }

    /// Reads the operator maintained list.
    ///
    /// One call per line, optionally followed by a note after a comma or a
    /// space. Free form on purpose: the file is edited by hand and a format
    /// that refuses a line is a format that loses a note.
    fn load_local(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                crate::log_warn!("callsign", "{}: {}", path.display(), e);
                return;
            }
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            let (call, note) = match line.find([',', '\t', ' ']) {
                Some(at) => (line[..at].trim(), line[at + 1..].trim()),
                None => (line, ""),
            };
            if call.is_empty() {
                continue;
            }
            self.local.insert(call.to_ascii_uppercase(), note.to_string());
        }
        crate::log_info!("callsign", "{}: {} known stations", path.display(), self.local.len());
    }

    pub fn prefix_count(&self) -> usize {
        self.prefixes.prefixes()
    }

    pub fn country_count(&self) -> usize {
        self.prefixes.countries()
    }

    pub fn spots(&self) -> &[Spot] {
        &self.spots
    }

    pub fn clear(&mut self) {
        self.spots.clear();
    }

    /// Resolves a call through the memo.
    fn resolve(&mut self, call: &str) -> Option<Resolved> {
        let key = lookup_key(call);
        if let Some(cached) = self.memo.get(&key) {
            return cached.clone();
        }
        let resolved = self.prefixes.resolve(&key);
        // Cleared wholesale rather than evicted one at a time. A least recently
        // used order would need a second structure to maintain, and the miss
        // cost here is a handful of hash probes.
        if self.memo.len() >= self.memo_limit {
            self.memo.clear();
        }
        self.memo.insert(key, resolved.clone());
        resolved
    }

    /// Records one sighting.
    ///
    /// The frequency on the air is optional, because a receiver with no
    /// transceiver attached still produces a usable list: the audio frequency
    /// identifies the station within the session even though it means nothing
    /// outside it.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        call: &str,
        hz: f32,
        rf_hz: Option<i64>,
        snr_db: f32,
        wpm: f32,
        from_de: bool,
        cq: bool,
        settings: &CallsignSettings,
    ) {
        let call = call.to_ascii_uppercase();

        // Matched on the call and the frequency together. The call alone would
        // merge a station that moved across the band, and the frequency alone
        // would merge two stations working next to each other.
        let existing = self.spots.iter().position(|s| {
            if s.call != call {
                return false;
            }
            match (rf_hz, s.rf_hz) {
                (Some(wanted), stored) if stored != 0 => {
                    (wanted - stored).abs() as f64 <= SAME_STATION_HZ
                }
                _ => (s.hz - hz).abs() as f64 <= SAME_STATION_HZ,
            }
        });

        let now = crate::platform::utc_time_hms();

        if let Some(index) = existing {
            let spot = &mut self.spots[index];
            spot.last = now;
            spot.seen = Instant::now();
            spot.count = spot.count.saturating_add(1);
            spot.hz = hz;
            if let Some(rf) = rf_hz {
                spot.rf_hz = rf;
            }
            // The strongest and the fastest reading are kept rather than the
            // latest. A station fades within a transmission, and the figure
            // worth reporting is what it reached rather than where it ended.
            if snr_db > spot.snr_db {
                spot.snr_db = snr_db;
            }
            if wpm > 0.0 {
                spot.wpm = wpm;
            }
            spot.from_de |= from_de;
            spot.cq |= cq;
            return;
        }

        let resolved = if settings.auto_lookup_on_decode {
            self.resolve(&call)
        } else {
            None
        };
        let note = self.local.get(&call).cloned().unwrap_or_default();

        let spot = Spot {
            call: call.clone(),
            hz,
            rf_hz: rf_hz.unwrap_or(0),
            first: now,
            last: now,
            seen: Instant::now(),
            count: 1,
            snr_db,
            wpm,
            from_de,
            cq,
            country: resolved.as_ref().map(|r| r.name.clone()).unwrap_or_default(),
            dxcc: resolved.as_ref().map(|r| r.dxcc.clone()).unwrap_or_default(),
            cq_zone: resolved.as_ref().map(|r| r.cq_zone).unwrap_or(0),
            itu_zone: resolved.as_ref().map(|r| r.itu_zone).unwrap_or(0),
            continent: resolved.as_ref().map(|r| r.continent.clone()).unwrap_or_default(),
            note,
        };

        self.append_history(&spot);
        self.spots.push(spot);

        // The oldest sighting is discarded rather than the oldest entry. A
        // station heard an hour ago and again a minute ago is current, and its
        // first sighting says nothing about whether it is worth keeping.
        if self.spots.len() > MAX_SPOTS {
            if let Some(oldest) = self
                .spots
                .iter()
                .enumerate()
                .max_by(|a, b| {
                    a.1.age_s().partial_cmp(&b.1.age_s()).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
            {
                self.spots.remove(oldest);
            }
        }
    }

    /// Retires entries nothing has been heard from.
    ///
    /// Sorted by frequency afterwards, so the list reads the way a dial turns
    /// and a row does not move under the pointer when a station is heard again.
    pub fn tick(&mut self) {
        self.spots.retain(|s| s.age_s() < RETIRE_S);
        self.spots.sort_by(|a, b| {
            let ak = if a.rf_hz != 0 { a.rf_hz as f64 } else { a.hz as f64 };
            let bk = if b.rf_hz != 0 { b.rf_hz as f64 } else { b.hz as f64 };
            ak.partial_cmp(&bk).unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    /// Appends one spot to the history file.
    ///
    /// Only a first sighting is written. A repetition carries nothing the file
    /// does not already hold, and writing each one would make the file a copy of
    /// the transcript rather than a list of stations.
    fn append_history(&mut self, spot: &Spot) {
        let path = match self.history.as_ref() {
            Some(p) => p.clone(),
            None => return,
        };
        if self.history_limit == 0 {
            return;
        }

        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(directory);
            }
        }

        let (y, mo, d, _, _, _) = crate::platform::utc_time_full();
        let line = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}Z  {:>10}  {:<12} {:>5.1} dB  {}\r\n",
            y,
            mo,
            d,
            spot.first.0,
            spot.first.1,
            spot.first.2,
            if spot.rf_hz != 0 {
                format!("{:.2}", spot.rf_hz as f64 / 1000.0)
            } else {
                format!("{:.0}", spot.hz)
            },
            spot.call,
            spot.snr_db,
            spot.country
        );

        use std::io::Write;
        let opened = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        match opened {
            Ok(mut file) => {
                if file.write_all(line.as_bytes()).is_ok() {
                    self.history_lines += 1;
                    self.appends += 1;
                }
            }
            Err(e) => {
                crate::log_warn!("callsign", "cannot append to {}: {}", path.display(), e);
                // Dropped rather than retried per spot. A directory that cannot
                // be written to will not become writable during the session, and
                // one warning is worth more than one per station.
                self.history = None;
                return;
            }
        }

        if self.appends >= TRIM_EVERY && self.history_lines > self.history_limit {
            self.appends = 0;
            self.trim_history(&path);
        }
    }

    fn trim_history(&mut self, path: &Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() <= self.history_limit {
            self.history_lines = lines.len();
            return;
        }
        let keep = &lines[lines.len() - self.history_limit..];
        let mut out = String::with_capacity(self.history_limit * 72);
        for line in keep {
            out.push_str(line);
            out.push_str("\r\n");
        }
        if std::fs::write(path, out).is_ok() {
            self.history_lines = keep.len();
            crate::log_info!(
                "callsign",
                "{} trimmed to {} entries",
                path.display(),
                self.history_lines
            );
        }
    }

    /// Scans one line and records whatever it holds.
    ///
    /// The signal figures are handed in rather than looked up, because the
    /// caller knows which channel produced the line and this does not.
    #[allow(clippy::too_many_arguments)]
    pub fn scan(
        &mut self,
        text: &str,
        hz: f32,
        rf_hz: Option<i64>,
        snr_db: f32,
        wpm: f32,
        partial: bool,
        settings: &CallsignSettings,
    ) {
        if !settings.lookup_enabled || text.is_empty() {
            return;
        }
        let min = settings.min_callsign_length as usize;

        // The scratch is moved out because the recording borrows the book
        // mutably while the spans are being read.
        let mut found = std::mem::take(&mut self.found);
        extract(text, min, partial, &mut found);
        for item in &found {
            let call = &text[item.start..item.end];
            self.observe(call, hz, rf_hz, snr_db, wpm, item.from_de, item.cq, settings);
        }
        self.found = found;
    }
}

/// Lines an existing history file holds.
fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        extract(text, 4, false, &mut out);
        out.iter().map(|f| text[f.start..f.end].to_string()).collect()
    }

    #[test]
    fn a_plain_exchange_yields_both_stations() {
        assert_eq!(spans("UA3XYZ DE DL1ABC K"), vec!["UA3XYZ", "DL1ABC"]);
    }

    #[test]
    fn the_separator_names_the_transmitting_station() {
        // The distinction the list is built on: one of these two stations is
        // sending and the other is being addressed, and only the first is
        // evidence of anything.
        let text = "UA3XYZ DE DL1ABC K";
        let mut found = Vec::new();
        extract(text, 4, false, &mut found);
        assert_eq!(found.len(), 2);
        assert!(!found[0].from_de);
        assert!(found[1].from_de);
    }

    #[test]
    fn a_call_for_contacts_marks_the_whole_line() {
        let text = "CQ CQ DE RA0FF RA0FF K";
        let mut found = Vec::new();
        extract(text, 4, false, &mut found);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.cq));
    }

    #[test]
    fn the_ordinary_exchange_is_not_a_call_sign() {
        // Everything a contact is made of passes through the extractor, and
        // almost all of it has to be refused: a report, a serial, an
        // abbreviation, a prosign.
        assert!(spans("TNX FER QSO 599 5NN 73 GL ES CUL AGN").is_empty());
        assert!(spans("RST 599 599 QTH MOSCOW OP IVAN").is_empty());
        assert!(spans("<BK> R R TU <SK>").is_empty());
    }

    #[test]
    fn portable_and_visiting_forms_survive() {
        assert_eq!(spans("DL1ABC/P"), vec!["DL1ABC/P"]);
        assert_eq!(spans("EA8/DL1ABC"), vec!["EA8/DL1ABC"]);
        assert_eq!(spans("K1ABC/QRP"), vec!["K1ABC/QRP"]);
    }

    #[test]
    fn the_country_is_keyed_on_where_the_station_is() {
        // A visiting operator is a station of the country being visited, which
        // is the leading part rather than the core.
        assert_eq!(lookup_key("EA8/DL1ABC"), "EA8");
        assert_eq!(lookup_key("DL1ABC/P"), "DL1ABC");
        assert_eq!(lookup_key("DL1ABC"), "DL1ABC");
    }

    #[test]
    fn a_growing_line_holds_back_its_last_token() {
        // The reason the flag exists: a prefix is indistinguishable from the
        // call it will become until the character after it arrives, and a spot
        // created from the prefix would sit in the list as a station that does
        // not exist.
        let mut found = Vec::new();
        extract("CQ DE UA3XY", 4, true, &mut found);
        assert!(found.is_empty());
        extract("CQ DE UA3XYZ ", 4, true, &mut found);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn the_structure_admits_the_awkward_real_forms() {
        assert!(is_callsign("2E0ABC", 4));
        assert!(is_callsign("9A1AA", 4));
        assert!(is_callsign("3DA0RS", 4));
        assert!(is_callsign("4X4XX", 4));
        assert!(is_callsign("W100AW", 4));
        // Two letters and no digit, which is every abbreviation on the air.
        assert!(!is_callsign("TNX", 4));
        // A report, which is the most frequent token that is not a call.
        assert!(!is_callsign("599", 4));
        // A prefix with nothing after the digit.
        assert!(!is_callsign("EA8", 4));
    }
}