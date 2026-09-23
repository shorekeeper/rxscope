//! Character set and material.
//!
//! ## Why the set and the material are separate
//!
//! The set says which characters the student has met; the material says what is
//! made of them. Folding the two together is the usual mistake and it produces a
//! trainer that can only send random groups: the moment a word list is wanted,
//! the set has to be consulted to decide whether a word is admissible, and that
//! decision belongs to the material rather than to the lesson.
//!
//! ## Which sources ignore the set, and why
//!
//! A callsign has a shape: a prefix carrying a letter, a digit, and one to four
//! letters. Restricted to two characters it produces something that is not a
//! callsign. A Q code is three letters beginning with Q and there are nineteen
//! of them. An abbreviation is a fixed token. None of the three can be built out
//! of a subset, so all three use the whole alphabet and the panel says so: the
//! operator chooses them, and choosing them is the statement that the student is
//! ready.
//!
//! Words are the one source that respects the set, because a word list can be
//! filtered and a short word made of early characters is still a word. What it
//! cannot do is pretend: when nothing in the list fits, the source falls back to
//! groups and reports that it did, because a panel that says words while sending
//! digits is worse than one that admits the level is too low.

use crate::config::settings::{LessonMethod, LessonSettings, MaterialSettings, MaterialSource};
use crate::core::Rng;
use crate::morse;
use crate::vocab;

/// Punctuation offered when the material asks for it.
///
/// The four an operator actually meets. The rest exist in the table and are sent
/// only when the operator states them by hand: a session peppered with dollar
/// signs trains nothing anybody needs.
const PUNCTUATION: &str = ".,?/";

/// Prosigns offered when the material asks for them.
const PROSIGNS: &[&str] = &["<BT>", "<AR>", "<SK>", "<KN>"];

/// Characters the lesson has introduced.
///
/// The level counts characters of a generated order and means nothing for a set
/// the operator wrote out, which is why the two paths do not consult each other.
pub fn character_set(lesson: &LessonSettings) -> String {
    if lesson.method == LessonMethod::Custom {
        let mut out = String::with_capacity(lesson.custom_set.len());
        for ch in lesson.custom_set.chars() {
            let upper = ch.to_ascii_uppercase();
            // A character with no pattern would produce a level at which nothing
            // is sent, which reads as a broken generator rather than as a typing
            // mistake in a configuration file.
            if morse::pattern_of(upper).is_some() && !out.contains(upper) {
                out.push(upper);
            }
        }
        // An empty custom set would leave the generator nothing to choose from,
        // so it falls back rather than sending silence.
        if out.len() >= 2 {
            return out;
        }
        crate::log_warn!("lesson", "the custom set holds fewer than two usable characters");
    }

    let order = match lesson.method {
        LessonMethod::Alphabet => morse::ALPHABET_ORDER,
        LessonMethod::Frequency => morse::FREQUENCY_ORDER,
        _ => morse::KOCH_ORDER,
    };
    let take = (lesson.level as usize).clamp(2, order.chars().count());
    order.chars().take(take).collect()
}

/// Why the source produced something other than what was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// It produced what was asked for.
    None,
    /// The word list holds nothing the current set can spell.
    NoWordFits,
    /// The stated file could not be read, or held no usable word.
    NoFile,
}

impl Fallback {
    pub fn key(self) -> Option<&'static str> {
        match self {
            Fallback::None => None,
            Fallback::NoWordFits => Some("hint.fallback_no_word"),
            Fallback::NoFile => Some("hint.fallback_no_file"),
        }
    }
}

pub struct Material {
    rng: Rng,
    /// Pool the groups are drawn from, rebuilt when the inputs move.
    pool: Vec<char>,
    /// Running total of the selection weights, one entry per pool character.
    cumulative: Vec<f32>,
    /// Character the draw leans towards, and how far.
    focus: Option<char>,
    focus_weight: f32,
    signature: u64,
    /// Words read from a file, empty until one is loaded.
    words: Vec<String>,
    words_from: String,
    /// True when the stated file was tried and produced nothing.
    file_failed: bool,
    /// Why the last draw produced something other than what was asked for.
    fallback: Fallback,
    /// Token of the last draw, when it carries a meaning worth showing.
    ///
    /// Held rather than derived by the caller, because a Q code sent inside a
    /// longer line cannot be found again by searching the text: two tokens of
    /// one line would both match and the gloss would name the wrong one.
    gloss: Option<&'static str>,
}

impl Material {
    pub fn new() -> Material {
        Material {
            rng: Rng::from_clock(),
            pool: Vec::new(),
            cumulative: Vec::new(),
            focus: None,
            focus_weight: 0.0,
            signature: 0,
            words: Vec::new(),
            words_from: String::new(),
            file_failed: false,
            fallback: Fallback::None,
            gloss: None,
        }
    }

    /// Characters currently drawn from, for a readout.
    pub fn pool(&self) -> String {
        self.pool.iter().collect()
    }

    /// Centres the draw on one character.
    ///
    /// Stated by the caller rather than derived here, because which character is
    /// the newest follows from the lesson, and the generator has no reason to
    /// know that lessons exist.
    pub fn set_focus(&mut self, ch: Option<char>, weight: f32) {
        self.focus = ch;
        self.focus_weight = weight.max(0.0);
    }

    /// Why the last draw was not what the source promised.
    pub fn fallback(&self) -> Fallback {
        self.fallback
    }

    /// What the last token means, when it is one that has a meaning.
    pub fn gloss(&self) -> Option<&'static str> {
        self.gloss
    }

    /// Produces the next thing to send.
    ///
    /// One group, word or token. The caller keys it and asks again, which is
    /// what keeps the queue short: a session that generated a minute of material
    /// at once would take a minute to notice a change of speed.
    ///
    /// The history is consulted so the characters being missed come round more
    /// often. Optional because a caller that has none is not a caller with a
    /// perfect student: it is one that has not loaded a history, and drawing
    /// uniformly is the honest answer there.
    pub fn next(
        &mut self,
        lesson: &LessonSettings,
        material: &MaterialSettings,
        progress: Option<&crate::progress::Progress>,
        window: u32,
        weak_weight: f32,
    ) -> String {
        self.sync(lesson, material);
        self.fallback = Fallback::None;
        self.gloss = None;
        if self.pool.is_empty() {
            return String::new();
        }
        self.reweight(progress, window, weak_weight);

        match material.source {
            MaterialSource::Callsigns => self.callsign(),
            MaterialSource::Numbers => self.digits(material),
            MaterialSource::QCodes => self.from_table(vocab::Q_CODES),
            MaterialSource::Abbrev => self.from_table(vocab::ABBREVIATIONS),
            MaterialSource::Words | MaterialSource::File => self.word(material),
            // The structured exchange has a generator of its own, because its
            // unit is a field rather than a character. A caller that reached
            // here asked the wrong generator, so a group is the honest answer.
            MaterialSource::Qso => self.group(material),
            MaterialSource::Groups => self.group(material),
        }
    }

    /// Rebuilds the selection weights.
    ///
    /// A running total rather than a normalized distribution, so a draw is one
    /// random number and one walk over the pool: at forty characters that is
    /// cheaper than dividing forty weights by their sum.
    fn reweight(
        &mut self,
        progress: Option<&crate::progress::Progress>,
        window: u32,
        weak_weight: f32,
    ) {
        self.cumulative.clear();
        let mut total = 0.0f32;
        for &ch in &self.pool {
            let mut weight = match progress {
                Some(p) if weak_weight > 0.0 => p.weight_of(ch, window, weak_weight),
                _ => 1.0,
            };
            // On top of what the history says rather than instead of it: a
            // character that is both new and being missed is the one the whole
            // exercise exists for.
            if self.focus == Some(ch) {
                weight *= 1.0 + self.focus_weight;
            }
            total += weight;
            self.cumulative.push(total);
        }
    }

    /// One character, weighted towards what is being missed.
    #[inline]
    fn draw(&mut self) -> char {
        let total = self.cumulative.last().copied().unwrap_or(0.0);
        if total <= 0.0 {
            return self.pool[self.rng.below(self.pool.len())];
        }
        let target = self.rng.next_f32() * total;
        // Linear rather than a binary search: the pool holds at most a few dozen
        // characters, and a search would be more code to read for a difference
        // no instrument can measure.
        for (index, &edge) in self.cumulative.iter().enumerate() {
            if target < edge {
                return self.pool[index];
            }
        }
        self.pool[self.pool.len() - 1]
    }

    /// Rebuilds the pool when anything that decides it has moved.
    fn sync(&mut self, lesson: &LessonSettings, material: &MaterialSettings) {
        let wanted = signature(lesson, material);
        if wanted == self.signature && !self.pool.is_empty() {
            return;
        }
        self.signature = wanted;

        let set = character_set(lesson);
        self.pool.clear();
        for ch in set.chars() {
            self.pool.push(ch);
        }

        // Digits are added only when the order has not reached any. With the
        // incremental order they arrive on their own, and adding them twice
        // would double their share of the session.
        if material.include_numbers && !self.pool.iter().any(|c| c.is_ascii_digit()) {
            for ch in '0'..='9' {
                self.pool.push(ch);
            }
        }
        if material.include_punctuation {
            for ch in PUNCTUATION.chars() {
                if !self.pool.contains(&ch) {
                    self.pool.push(ch);
                }
            }
        }

        crate::log_debug!("lesson", "pool is {}", self.pool.iter().collect::<String>());
    }

    /// Random characters, of a length inside the stated bounds.
    fn group(&mut self, material: &MaterialSettings) -> String {
        let lo = material.min_group.max(1) as usize;
        let hi = material.max_group.max(material.min_group) as usize;
        let length = self.rng.between(lo, hi);

        let mut out = String::with_capacity(length + 4);
        for _ in 0..length {
            out.push(self.draw());
        }

        // A prosign is appended rather than mixed in, because that is where one
        // appears on the air: at the end of a transmission rather than inside a
        // group.
        if material.include_prosigns && self.rng.chance(0.15) {
            out.push(' ');
            out.push_str(PROSIGNS[self.rng.below(PROSIGNS.len())]);
        }
        out
    }

    /// Digits only, which is what a serial number or a report is.
    fn digits(&mut self, material: &MaterialSettings) -> String {
        let lo = material.min_group.max(1) as usize;
        let hi = material.max_group.max(material.min_group) as usize;
        let length = self.rng.between(lo, hi);
        let mut out = String::with_capacity(length);
        for _ in 0..length {
            out.push((b'0' + self.rng.below(10) as u8) as char);
        }
        out
    }

    /// One entry of a fixed table, with its meaning recorded.
    ///
    /// The whole alphabet, see the note at the head of the file: a Q code built
    /// out of the two characters a beginner has met is not a Q code.
    fn from_table(&mut self, table: &'static [(&'static str, &'static str)]) -> String {
        let (token, meaning) = table[self.rng.below(table.len())];
        self.gloss = Some(meaning);
        token.to_string()
    }

    /// A callsign, using the whole alphabet.
    ///
    /// The shape rather than the set. The forms are the ordinary ones: one or two
    /// letters, a digit, and one to three letters, which covers almost every
    /// callsign an operator meets.
    fn callsign(&mut self) -> String {
        let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut out = String::with_capacity(6);

        let prefix = if self.rng.chance(0.35) { 1 } else { 2 };
        for _ in 0..prefix {
            out.push(letters[self.rng.below(26)] as char);
        }
        out.push((b'0' + self.rng.below(10) as u8) as char);
        let suffix = self.rng.between(1, 3);
        for _ in 0..suffix {
            out.push(letters[self.rng.below(26)] as char);
        }
        out
    }

    /// A word the pool can spell.
    ///
    /// A word holding a character the student has not met is skipped rather than
    /// substituted: sending it would mark them against something never taught,
    /// and substituting would produce a word that is not one.
    ///
    /// The fallback is reported rather than silent. At level two nothing in any
    /// list fits, and a panel that says words while sending digits is worse than
    /// one that admits the level is too low for the source.
    fn word(&mut self, material: &MaterialSettings) -> String {
        self.load_words(material);

        // The list from the file when there is one, and the compiled list
        // otherwise. A source that produced nothing until a file was written
        // would be a setting that does not work.
        let from_file = !self.words.is_empty();
        let count = if from_file { self.words.len() } else { vocab::WORDS.len() };

        // Bounded rather than exhaustive. A full scan of a list of thousands
        // would run per group and would still find nothing at level two, so the
        // fallback is reached by giving up rather than by proving it impossible.
        for _ in 0..48 {
            let index = self.rng.below(count);
            let candidate: &str = if from_file {
                self.words[index].as_str()
            } else {
                vocab::WORDS[index]
            };
            if candidate.chars().all(|c| self.pool.contains(&c)) {
                return candidate.to_string();
            }
        }

        self.fallback = if self.file_failed {
            Fallback::NoFile
        } else {
            Fallback::NoWordFits
        };
        self.group(material)
    }

    /// Reads the word list when the path moved.
    fn load_words(&mut self, material: &MaterialSettings) {
        if material.file_path == self.words_from {
            return;
        }
        self.words_from = material.file_path.clone();
        self.words.clear();
        self.file_failed = false;
        if material.file_path.is_empty() {
            return;
        }

        let path = resolve(&material.file_path);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                crate::log_warn!("lesson", "{}: {}", path.display(), e);
                self.file_failed = true;
                return;
            }
        };

        // Whitespace separated rather than one per line, so a file of prose and a
        // file of words are both usable: what a trainer wants from either is the
        // words.
        for token in text.split_whitespace() {
            let mut word = String::with_capacity(token.len());
            for ch in token.chars() {
                let upper = ch.to_ascii_uppercase();
                if morse::pattern_of(upper).is_some() {
                    word.push(upper);
                }
            }
            if word.len() >= 2 {
                self.words.push(word);
            }
        }
        if self.words.is_empty() {
            crate::log_warn!("lesson", "{}: no usable word", path.display());
            self.file_failed = true;
        } else {
            crate::log_info!("lesson", "{}: {} words", path.display(), self.words.len());
        }
    }
}

impl Default for Material {
    fn default() -> Material {
        Material::new()
    }
}

/// Resolves a path against the directory of the configuration file.
///
/// The working directory is not it: a shortcut started from anywhere has to find
/// the same word list as a double click on the executable.
fn resolve(path: &str) -> std::path::PathBuf {
    let stated = std::path::Path::new(path);
    if stated.is_absolute() {
        return stated.to_path_buf();
    }
    let base = crate::config::Settings::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    base.join(stated)
}

/// Fingerprint of everything that decides the pool.
fn signature(lesson: &LessonSettings, material: &MaterialSettings) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&[lesson.method as u8 + 1]);
    mix(&lesson.level.to_le_bytes());
    mix(lesson.custom_set.as_bytes());
    mix(&[
        u8::from(material.include_numbers),
        u8::from(material.include_punctuation),
        u8::from(material.include_prosigns),
    ]);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(level: u32) -> LessonSettings {
        LessonSettings { level, ..LessonSettings::default() }
    }

    #[test]
    fn the_level_takes_a_prefix_of_the_order() {
        let set = character_set(&lesson(4));
        assert_eq!(set.chars().count(), 4);
        assert!(morse::KOCH_ORDER.starts_with(&set));
    }

    #[test]
    fn the_level_cannot_fall_below_a_choice() {
        // One character carries no decision, so it trains nothing: the generator
        // would send the same letter forever and every answer would be right.
        let set = character_set(&lesson(1));
        assert!(set.chars().count() >= 2);
    }

    #[test]
    fn a_custom_set_drops_what_cannot_be_sent() {
        let mut l = lesson(2);
        l.method = LessonMethod::Custom;
        l.custom_set = "ab\u{263A}cc".to_string();
        // Folded, deduplicated, and the character with no pattern gone.
        assert_eq!(character_set(&l), "ABC");
    }

    #[test]
    fn an_unusable_custom_set_falls_back() {
        let mut l = lesson(5);
        l.method = LessonMethod::Custom;
        l.custom_set = "\u{263A}".to_string();
        // The generated order rather than silence: a set of nothing would leave
        // the session playing no material and reporting no fault.
        assert_eq!(character_set(&l).chars().count(), 5);
    }

    #[test]
    fn a_group_holds_only_the_pool() {
        let mut material = Material::new();
        let l = lesson(6);
        let m = MaterialSettings { include_numbers: false, ..MaterialSettings::default() };
        let pool = character_set(&l);
        for _ in 0..200 {
            let group = material.next(&l, &m, None, 40, 0.0);
            for ch in group.chars() {
                assert!(pool.contains(ch), "{:?} is outside the pool", ch);
            }
        }
    }

    #[test]
    fn a_group_obeys_the_stated_length() {
        let mut material = Material::new();
        let l = lesson(6);
        let m = MaterialSettings {
            min_group: 3,
            max_group: 7,
            include_prosigns: false,
            ..MaterialSettings::default()
        };
        for _ in 0..200 {
            let n = material.next(&l, &m, None, 40, 0.0).chars().count();
            assert!((3..=7).contains(&n), "a group of {} characters", n);
        }
    }

    #[test]
    fn digits_are_added_only_when_the_order_has_none() {
        let mut material = Material::new();
        let m = MaterialSettings { include_numbers: true, ..MaterialSettings::default() };
        material.next(&lesson(6), &m, None, 40, 0.0);
        let with = material.pool();
        assert!(with.contains('5'));
        assert_eq!(with.matches('5').count(), 1);

        material.next(&lesson(30), &m, None, 40, 0.0);
        assert_eq!(material.pool().matches('5').count(), 1);
    }

    #[test]
    fn a_callsign_has_the_shape_of_one() {
        let mut material = Material::new();
        let l = lesson(2);
        let m = MaterialSettings { source: MaterialSource::Callsigns, ..MaterialSettings::default() };
        for _ in 0..200 {
            let call = material.next(&l, &m, None, 40, 0.0);
            let bytes = call.as_bytes();
            assert!(bytes.len() >= 3 && bytes.len() <= 6, "{:?}", call);
            let digit = bytes.iter().position(|b| b.is_ascii_digit()).expect("no digit");
            assert!(digit >= 1 && digit <= 2, "the digit is at {} in {:?}", digit, call);
            assert!(bytes[..digit].iter().all(|b| b.is_ascii_alphabetic()));
            assert!(bytes[digit + 1..].iter().all(|b| b.is_ascii_alphabetic()));
            assert!(!bytes[digit + 1..].is_empty());
        }
    }

    #[test]
    fn a_q_code_arrives_whole_and_explained() {
        // The point of the source. A Q code assembled out of the current set
        // would be three characters that happen to start with Q, which is not
        // vocabulary and cannot be glossed.
        let mut material = Material::new();
        let l = lesson(2);
        let m = MaterialSettings { source: MaterialSource::QCodes, ..MaterialSettings::default() };
        for _ in 0..100 {
            let token = material.next(&l, &m, None, 40, 0.0);
            assert!(token.starts_with('Q'), "{:?}", token);
            assert_eq!(token.len(), 3);
            assert!(material.gloss().is_some(), "{} has no meaning", token);
            assert_eq!(material.fallback(), Fallback::None);
        }
    }

    #[test]
    fn an_abbreviation_arrives_whole() {
        let mut material = Material::new();
        let l = lesson(2);
        let m = MaterialSettings { source: MaterialSource::Abbrev, ..MaterialSettings::default() };
        for _ in 0..100 {
            let token = material.next(&l, &m, None, 40, 0.0);
            assert!(vocab::meaning(&token).is_some(), "{:?} is not in the list", token);
        }
    }

    #[test]
    fn a_word_source_works_without_a_file() {
        // The whole reason the list is compiled in. Before it, this source
        // produced digits and said it was producing words.
        let mut material = Material::new();
        let l = lesson(40);
        let m = MaterialSettings { source: MaterialSource::Words, ..MaterialSettings::default() };
        let mut real = 0;
        for _ in 0..50 {
            let word = material.next(&l, &m, None, 40, 0.0);
            if vocab::WORDS.contains(&word.as_str()) {
                real += 1;
            }
        }
        assert!(real > 40, "only {} of fifty draws were words", real);
    }

    #[test]
    fn a_word_source_says_when_it_gave_up() {
        // At level two nothing spells anything, and the honest answer is a group
        // plus a statement that it is a group.
        let mut material = Material::new();
        let l = lesson(2);
        let m = MaterialSettings { source: MaterialSource::Words, ..MaterialSettings::default() };
        material.next(&l, &m, None, 40, 0.0);
        assert_eq!(material.fallback(), Fallback::NoWordFits);
    }
}