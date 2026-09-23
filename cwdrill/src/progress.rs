//! Per character history.
//!
//! ## Why a window rather than a lifetime average
//!
//! A lifetime average of a character learned three weeks ago cannot fall, so it
//! says nothing about whether the character can be copied this evening. The
//! level has to move on what is happening now, and the window is what "now"
//! means: too short and one mistake withdraws a character, too long and last
//! week decides tonight.
//!
//! The window is bits in one word. Sixty four answers at a group of five is a
//! dozen groups, which is a few minutes of practice: exactly the horizon the
//! decision needs. A longer window would need a vector per character to hold
//! statistics that say nothing about the session in progress, so the setting is
//! bounded rather than the storage grown.
//!
//! The lifetime totals are kept beside it, because they answer a different
//! question and cost two words: how much practice a character has had at all.
//!
//! ## Why the confusion matrix is stored sparsely
//!
//! The full matrix over forty characters is sixteen hundred cells and almost all
//! of them are nought: a student confuses perhaps forty pairs. Written out in
//! full it would be a page of zeros in a file an operator may read, and the
//! entries that matter would be lost in it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::settings::ProgressSettings;
use crate::config::ini::Ini;

/// Answers one character remembers.
///
/// The bound on the setting, stated here because it is a property of the
/// storage: the window is bits in one word.
pub const MAX_WINDOW: u32 = 64;

/// Stands for an omission in the confusion matrix.
///
/// A character rather than a separate table, so the matrix answers one question
/// with one lookup: what happened when this was sent. An omission is the most
/// informative entry in it, because a character that is never written is one the
/// ear did not hear at all rather than one it heard as something else.
pub const OMITTED: char = '_';

/// Answers a character needs before its accuracy is acted on.
///
/// Below this the figure is a coin toss dressed as a measurement, and a level
/// that moved on it would rise and fall on the first group of every session.
const MIN_ANSWERS: u32 = 5;

/// Weight of one reaction time in the running average.
///
/// A twelfth, which settles over a dozen answers: fast enough that an evening of
/// improvement shows, slow enough that one hesitation does not.
const REACTION_WEIGHT: f32 = 1.0 / 12.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct CharStat {
    /// Recent answers, bit nought the newest, set for correct.
    window: u64,
    /// Bits that mean anything.
    len: u32,
    pub sent: u64,
    pub correct: u64,
    /// Running average of the reaction time, in milliseconds.
    ///
    /// Nought until a correct answer has been timed. Only correct answers are
    /// timed, because the interval before a wrong one measures how long the
    /// student thought rather than how long they took to recognize.
    pub reaction_ms: f32,
}

impl CharStat {
    /// Share of the recent window that was correct.
    ///
    /// Nothing until the window holds enough to mean something, so a caller can
    /// tell an untested character from a failing one.
    pub fn accuracy(&self, window: u32) -> Option<f32> {
        let n = self.len.min(window.clamp(1, MAX_WINDOW));
        if n < MIN_ANSWERS {
            return None;
        }
        let mask = if n >= 64 { u64::MAX } else { (1u64 << n) - 1 };
        Some((self.window & mask).count_ones() as f32 / n as f32)
    }

    /// Answers held, bounded by the window.
    pub fn answers(&self, window: u32) -> u32 {
        self.len.min(window.clamp(1, MAX_WINDOW))
    }

    fn record(&mut self, correct: bool, reaction_ms: Option<f32>) {
        self.window = (self.window << 1) | u64::from(correct);
        if self.len < MAX_WINDOW {
            self.len += 1;
        }
        self.sent += 1;
        if correct {
            self.correct += 1;
        }
        if let Some(ms) = reaction_ms {
            if self.reaction_ms <= 0.0 {
                self.reaction_ms = ms;
            } else {
                self.reaction_ms += REACTION_WEIGHT * (ms - self.reaction_ms);
            }
        }
    }
}

/// One row of the readout.
#[derive(Debug, Clone, Copy)]
pub struct CharRow {
    pub ch: char,
    /// Nothing while the character has too few answers to judge.
    pub accuracy: Option<f32>,
    pub answers: u32,
    pub reaction_ms: f32,
}

pub struct Progress {
    chars: HashMap<char, CharStat>,
    confusion: HashMap<(char, char), u32>,
    sessions: u32,
    path: PathBuf,
    /// True while something has been recorded that is not on disk.
    dirty: bool,
}

impl Progress {
    /// Reads the history, or starts an empty one.
    ///
    /// A file that cannot be read is reported and ignored rather than fatal: a
    /// trainer that refuses to run because a statistics file is corrupt is worse
    /// than one that starts over.
    pub fn load(settings: &ProgressSettings) -> Progress {
        let path = resolve(&settings.path);
        let mut out = Progress {
            chars: HashMap::with_capacity(64),
            confusion: HashMap::with_capacity(128),
            sessions: 0,
            path,
            dirty: false,
        };

        if !out.path.exists() {
            return out;
        }
        let document = match Ini::load(&out.path) {
            Ok(d) => d,
            Err(e) => {
                crate::log_warn!("progress", "{}: {}", out.path.display(), e);
                return out;
            }
        };

        out.sessions = document.get_u32("summary", "sessions", 0);

        // One key per character. The value is the window as hexadecimal, its
        // length, the reaction time and the two lifetime totals: five fields
        // that round trip exactly, which a percentage would not.
        for (section, key, value) in document.pairs() {
            if section != "characters" {
                continue;
            }
            let ch = match key.chars().next() {
                Some(c) => c,
                None => continue,
            };
            let mut parts = value.split_whitespace();
            let window = parts
                .next()
                .and_then(|t| u64::from_str_radix(t, 16).ok())
                .unwrap_or(0);
            let len = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0u32);
            let reaction = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0.0f32);
            let sent = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0u64);
            let correct = parts.next().and_then(|t| t.parse().ok()).unwrap_or(0u64);
            out.chars.insert(
                ch,
                CharStat {
                    window,
                    len: len.min(MAX_WINDOW),
                    sent,
                    correct: correct.min(sent),
                    reaction_ms: reaction.max(0.0),
                },
            );
        }

        for (section, key, value) in document.pairs() {
            if section != "confusion" {
                continue;
            }
            let mut it = key.chars();
            let (sent, typed) = match (it.next(), it.next()) {
                (Some(a), Some(b)) => (a, b),
                _ => continue,
            };
            if let Ok(count) = value.trim().parse::<u32>() {
                if count > 0 {
                    out.confusion.insert((sent, typed), count);
                }
            }
        }

        crate::log_info!(
            "progress",
            "{}: {} characters, {} confused pairs, {} sessions",
            out.path.display(),
            out.chars.len(),
            out.confusion.len(),
            out.sessions
        );
        out
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sessions(&self) -> u32 {
        self.sessions
    }

    /// Writes the history when something changed.
    pub fn save(&mut self) -> crate::core::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut document = Ini::new();
        document.comment("", "CWDrill progress");
        document.comment("", "Maintained by the application. Safe to delete, which starts over.");
        document.blank("");

        document.set_u32("summary", "sessions", self.sessions);

        document.comment(
            "characters",
            "window in hexadecimal, its length, mean reaction in ms, sent, correct.",
        );
        document.comment(
            "characters",
            "The window is the recent answers, newest bit first, set for correct.",
        );
        // Sorted so the file is comparable between two saves: a map order that
        // moved would make every save look like a change.
        let mut keys: Vec<char> = self.chars.keys().copied().collect();
        keys.sort_unstable();
        for ch in keys {
            let stat = self.chars[&ch];
            document.set_string(
                "characters",
                &ch.to_string(),
                &format!(
                    "{:x} {} {:.0} {} {}",
                    stat.window, stat.len, stat.reaction_ms, stat.sent, stat.correct
                ),
            );
        }

        document.comment("confusion", "sent and received, then how often. Underscore is an omission.");
        let mut pairs: Vec<(char, char, u32)> = self
            .confusion
            .iter()
            .map(|(&(a, b), &n)| (a, b, n))
            .collect();
        pairs.sort_unstable();
        for (sent, typed, count) in pairs {
            document.set_u32("confusion", &format!("{}{}", sent, typed), count);
        }

        document.save(&self.path)?;
        self.dirty = false;
        Ok(())
    }

    /// Records one answer.
    ///
    /// The typed character is nothing for an omission, which is what puts the
    /// most informative entry into the matrix: a character never written is one
    /// the ear did not hear rather than one it heard as something else.
    pub fn record(&mut self, sent: char, typed: Option<char>, reaction_ms: Option<f32>) {
        let correct = typed == Some(sent);
        // A space carries no pattern, so it has no per character history: what a
        // word boundary tests is the gap rather than a character.
        if sent != ' ' {
            self.chars.entry(sent).or_default().record(correct, reaction_ms);
            if !correct {
                let key = (sent, typed.unwrap_or(OMITTED));
                *self.confusion.entry(key).or_insert(0) += 1;
            }
        }
        self.dirty = true;
    }

    pub fn stat(&self, ch: char) -> Option<&CharStat> {
        self.chars.get(&ch)
    }

    /// Accuracy over a whole set.
    ///
    /// Weighted by answers rather than by character, so a set in which one
    /// character has been practised twenty times and another twice reports what
    /// the student actually did rather than the mean of two unequal readings.
    pub fn set_accuracy(&self, set: &str, window: u32) -> Option<f32> {
        let mut correct = 0u32;
        let mut total = 0u32;
        for ch in set.chars() {
            if let Some(stat) = self.chars.get(&ch) {
                let n = stat.answers(window);
                if n < MIN_ANSWERS {
                    continue;
                }
                let mask = if n >= 64 { u64::MAX } else { (1u64 << n) - 1 };
                correct += (stat.window & mask).count_ones();
                total += n;
            }
        }
        if total == 0 {
            None
        } else {
            Some(correct as f32 / total as f32)
        }
    }

    /// Character of the set with the lowest accuracy.
    ///
    /// An untested character is not the weakest: it is unknown, and reporting it
    /// as weakest would send the student to practise what they have not met.
    pub fn weakest(&self, set: &str, window: u32) -> Option<char> {
        let mut best: Option<(char, f32)> = None;
        for ch in set.chars() {
            let accuracy = self.chars.get(&ch).and_then(|s| s.accuracy(window))?;
            if best.map(|(_, held)| accuracy < held).unwrap_or(true) {
                best = Some((ch, accuracy));
            }
        }
        best.map(|(ch, _)| ch)
    }

    /// Selection weight of one character.
    ///
    /// One plus the weighting times what is missing. An untested character keeps
    /// the base weight rather than being favoured: nothing is known about it, and
    /// treating unknown as bad would spend the session on whatever was
    /// introduced last.
    pub fn weight_of(&self, ch: char, window: u32, weak_weight: f32) -> f32 {
        match self.chars.get(&ch).and_then(|s| s.accuracy(window)) {
            Some(accuracy) => 1.0 + weak_weight.max(0.0) * (1.0 - accuracy),
            None => 1.0,
        }
    }

    /// How often a pair was confused.
    pub fn confusion_of(&self, sent: char, typed: char) -> u32 {
        self.confusion.get(&(sent, typed)).copied().unwrap_or(0)
    }


    /// One row per character of the set, in the order given.
    pub fn rows(&self, set: &str, window: u32, out: &mut Vec<CharRow>) {
        out.clear();
        for ch in set.chars() {
            let stat = self.chars.get(&ch);
            out.push(CharRow {
                ch,
                accuracy: stat.and_then(|s| s.accuracy(window)),
                answers: stat.map(|s| s.answers(window)).unwrap_or(0),
                reaction_ms: stat.map(|s| s.reaction_ms).unwrap_or(0.0),
            });
        }
    }

    /// Where the session log lives.
    ///
    /// Beside the history rather than inside it, because the two are read for
    /// different reasons and by different things: the history is a machine
    /// readable record the application maintains, and the log is a plain list an
    /// operator opens in a text editor to see whether the practice is working.
    pub fn sessions_path(&self) -> PathBuf {
        self.path.with_file_name("sessions.txt")
    }

    /// Appends one line to the session log.
    ///
    /// Coordinated time, so a line read beside a station log needs no offset
    /// applied to it.
    pub fn log_session(
        &mut self,
        settings: &ProgressSettings,
        level: u32,
        char_wpm: f32,
        text_wpm: f32,
        characters: u64,
        accuracy: f32,
        reaction_ms: f32,
    ) {
        self.sessions += 1;
        self.dirty = true;
        if !settings.log_sessions {
            return;
        }

        let path = self.sessions_path();
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(directory);
            }
        }

        let (y, mo, d, h, mi, _) = crate::platform::win32::utc_time_full();
        let line = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}Z  level {:>2}  {:>3.0}/{:>3.0} wpm  {:>5} chars  {:>5.1} %  {:>5.0} ms\r\n",
            y, mo, d, h, mi, level, char_wpm, text_wpm, characters, accuracy * 100.0, reaction_ms
        );

        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut file) => {
                let _ = file.write_all(line.as_bytes());
            }
            Err(e) => {
                crate::log_warn!("progress", "cannot append to {}: {}", path.display(), e);
                return;
            }
        }
        trim_sessions(&path, settings.keep_sessions);
    }

    /// Discards everything.
    ///
    /// The file is rewritten rather than deleted, so the operator sees an empty
    /// history rather than a missing one: a file that vanished looks like a
    /// fault, and an empty one looks like a decision.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.confusion.clear();
        self.sessions = 0;
        self.dirty = true;
        crate::log_info!("progress", "history cleared");
    }
}

/// Keeps the last lines of the session log and discards the rest.
///
/// The whole file is rewritten rather than the head being cut in place, because
/// a file cannot be shortened from the front and the alternative is a second
/// file plus a rename. At the size in question that is machinery for nothing: a
/// thousand lines of eighty characters is eighty kilobytes, written once per
/// session.
///
/// Nought keeps everything, which is what an operator running the log through
/// another tool wants: a reader following the file would lose its position at
/// every rewrite.
///
/// Every failure is silent past the log. Losing the tail of a training log is
/// not a reason to interrupt a session that has just ended well.
fn trim_sessions(path: &Path, keep: u32) {
    if keep == 0 {
        return;
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return,
    };
    let lines: Vec<&str> = text.lines().collect();
    let keep = keep as usize;
    if lines.len() <= keep {
        return;
    }

    let mut out = String::with_capacity(text.len());
    for line in &lines[lines.len() - keep..] {
        out.push_str(line);
        out.push_str("\r\n");
    }
    match std::fs::write(path, out) {
        Ok(()) => crate::log_info!(
            "progress",
            "{}: trimmed to the last {} sessions",
            path.display(),
            keep
        ),
        Err(e) => crate::log_warn!("progress", "cannot trim {}: {}", path.display(), e),
    }
}

/// Resolves a path against the directory of the configuration file.
fn resolve(path: &str) -> PathBuf {
    let stated = Path::new(path);
    if stated.is_absolute() {
        return stated.to_path_buf();
    }
    let base = crate::config::Settings::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    base.join(stated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> Progress {
        Progress {
            chars: HashMap::new(),
            confusion: HashMap::new(),
            sessions: 0,
            path: PathBuf::from("test.ini"),
            dirty: false,
        }
    }

    #[test]
    fn a_character_is_unknown_until_it_has_been_answered_enough() {
        // A figure taken from two answers is a coin toss dressed as a
        // measurement, and a level that moved on it would rise and fall on the
        // first group of every session.
        let mut p = empty();
        for _ in 0..(MIN_ANSWERS - 1) {
            p.record('K', Some('K'), None);
        }
        assert!(p.stat('K').unwrap().accuracy(40).is_none());
        p.record('K', Some('K'), None);
        assert_eq!(p.stat('K').unwrap().accuracy(40), Some(1.0));
    }

    #[test]
    fn the_window_forgets_and_the_lifetime_does_not() {
        let mut p = empty();
        for _ in 0..10 {
            p.record('M', None, None);
        }
        for _ in 0..10 {
            p.record('M', Some('M'), None);
        }
        // The last ten answers were all correct, so a window of ten reads as
        // perfect while the lifetime remembers the first ten.
        assert_eq!(p.stat('M').unwrap().accuracy(10), Some(1.0));
        assert_eq!(p.stat('M').unwrap().sent, 20);
        assert_eq!(p.stat('M').unwrap().correct, 10);
    }

    #[test]
    fn an_omission_and_a_substitution_are_different_entries() {
        let mut p = empty();
        p.record('K', Some('R'), None);
        p.record('K', None, None);
        assert_eq!(p.confusion_of('K', 'R'), 1);
        assert_eq!(p.confusion_of('K', OMITTED), 1);
    }

    #[test]
    fn a_weak_character_is_drawn_more_often() {
        let mut p = empty();
        for _ in 0..10 {
            p.record('K', Some('K'), None);
            p.record('R', None, None);
        }
        let strong = p.weight_of('K', 40, 2.0);
        let weak = p.weight_of('R', 40, 2.0);
        assert!(weak > strong * 2.5, "{} against {}", weak, strong);
        // An untested character keeps the base weight: nothing is known about
        // it, and treating unknown as bad would spend the session on whatever
        // was introduced last.
        assert!((p.weight_of('Z', 40, 2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn the_set_accuracy_is_weighted_by_practice() {
        let mut p = empty();
        for _ in 0..20 {
            p.record('K', Some('K'), None);
        }
        for _ in 0..5 {
            p.record('R', None, None);
        }
        // Twenty right and five wrong is eighty per cent, not the fifty per cent
        // a mean of the two characters would report.
        let accuracy = p.set_accuracy("KR", 40).unwrap();
        assert!((accuracy - 0.8).abs() < 0.01, "{}", accuracy);
    }
}