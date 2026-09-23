//! Complete settings tree.
//!
//! Every subsystem owns one section. Sections implement load and store so a
//! new option is added in exactly one place, and the generated INI carries
//! inline documentation for the operator.
//!
//! Values are validated on load. Anything that could produce a click, a wrong
//! element length or a silent output is clamped and reported.
//!
//! ## Why so many numbers
//!
//! A Morse trainer that offers a speed and a tone is a metronome. What decides
//! whether the operator can copy real traffic afterwards is everything else:
//! the ratio of a dash to a dot, the gaps between characters and between words,
//! how much those gaps wander, the shape of the keying edge, and what the band
//! is doing underneath. Each of those is a separate axis because each of them
//! is a separate skill, and a preset that folded them together would train the
//! preset rather than the skill.

use std::path::{Path, PathBuf};

use crate::config::ini::{ConfigEnum, Ini};
use crate::core::log::Level;
use crate::core::Result;

/// Declares a config enum with text mapping, variant list and a default.
macro_rules! config_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? } default $def:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),+ }

        impl ConfigEnum for $name {
            fn from_config(text: &str) -> Option<Self> {
                match text {
                    $($text => Some($name::$variant),)+
                    _ => None,
                }
            }
            fn to_config(&self) -> &'static str {
                match self {
                    $($name::$variant => $text,)+
                }
            }
            fn variants() -> &'static [&'static str] {
                &[$($text),+]
            }
        }

        impl Default for $name {
            fn default() -> Self { $name::$def }
        }
    };
}

trait SectionIo: Sized + Default {
    const NAME: &'static str;
    fn load(ini: &Ini) -> Self;
    fn store(&self, ini: &mut Ini);
}

// ------------------------------------------------------------------ enums

config_enum!(LogLevelCfg {
    Trace => "trace",
    Debug => "debug",
    Info => "info",
    Warn => "warn",
    Error => "error",
    Off => "off",
} default Info);

config_enum!(PresentModeCfg {
    Auto => "auto",
    Fifo => "fifo",
    FifoRelaxed => "fifo_relaxed",
    Mailbox => "mailbox",
    Immediate => "immediate",
} default Auto);

config_enum!(AnimCurve {
    Linear => "linear",
    EaseOut => "ease_out",
    EaseInOut => "ease_in_out",
} default EaseOut);

impl AnimCurve {
    /// Maps a linear phase to the eased one.
    ///
    /// Every curve maps nought to nought, which is what lets a caller test the
    /// eased value to decide whether anything is still moving.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            AnimCurve::Linear => t,
            AnimCurve::EaseOut => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
            AnimCurve::EaseInOut => t * t * (3.0 - 2.0 * t),
        }
    }
}

config_enum!(TabStyle {
    Underline => "underline",
    Attached => "attached",
} default Underline);

/// Shape of the keying edge.
///
/// The one setting an operator hears before they hear anything else. A hard
/// edge splashes across the whole passband and, on a trainer, teaches the ear
/// to key on the click rather than on the tone: the student then cannot copy a
/// properly shaped signal at all. A raised cosine over a few milliseconds is
/// what a transmitter produces and what the ear should be trained against.
///
/// The hard edge is offered because recognizing it is itself worth practising,
/// and because a student who has only ever heard shaped keying is surprised by
/// a badly adjusted transmitter on the air.
config_enum!(EnvelopeShape {
    Hard => "hard",
    RaisedCosine => "raised_cosine",
    Gaussian => "gaussian",
} default RaisedCosine);

/// How the character set grows.
///
/// The incremental method introduces two characters at full speed and adds one
/// at a time, which is the arrangement that produces reflex recognition rather
/// than counting. The alphabetical order is offered because it is what most
/// printed courses use, and a student following one needs to match it.
config_enum!(LessonMethod {
    Koch => "koch",
    Alphabet => "alphabet",
    Frequency => "frequency",
    Custom => "custom",
} default Koch);

/// What the material is made of.
config_enum!(MaterialSource {
    Groups => "groups",
    Words => "words",
    Callsigns => "callsigns",
    Numbers => "numbers",
    QCodes => "qcodes",
    Abbrev => "abbrev",
    Qso => "qso",
    File => "file",
} default Groups);

/// What the student does with what they hear.
config_enum!(PracticeMode {
    Listen => "listen",
    Copy => "copy",
    HeadCopy => "head_copy",
    Send => "send",
} default Copy);

config_enum!(TextCase {
    Upper => "upper",
    Lower => "lower",
} default Upper);

/// How the key behaves.
///
/// A straight key sends what the hand does, which is the harder and more honest
/// exercise: the length of every element is the operator's to get right. The two
/// iambic forms keep the timing themselves and differ in exactly one place, which
/// is what the release of a squeeze does; both are in wide use and an operator
/// who learned one is thrown by the other.
config_enum!(PaddleMode {
    Straight => "straight",
    IambicA => "iambic_a",
    IambicB => "iambic_b",
} default IambicB);

/// Where the contacts come from.
///
/// A paddle clipped across two mouse switches is the usual arrangement, because
/// the switches are already there and the operating system already debounces
/// them. The keyboard is offered for a machine with no spare mouse and for
/// trying the modes before wiring anything.
config_enum!(PaddleSource {
    Mouse => "mouse",
    Keyboard => "keyboard",
    Both => "both",
} default Mouse);

/// Single character exercise.
///
/// Three exercises rather than one setting with three switches, because the
/// three differ in both halves at once: what is played and what answers it.
/// Recall gives the character and asks for the pattern, which is the harder
/// direction and the one a sender needs. Echo plays the character first, which
/// is how a shape is learned before it is produced. Blind plays it and withholds
/// it, which is ordinary copying narrowed to one unit so a single character can
/// be drilled without a group around it.
config_enum!(DrillMode {
    Off => "off",
    Recall => "recall",
    Echo => "echo",
    Blind => "blind",
} default Off);

/// Unit one drill prompt carries.
config_enum!(DrillUnit {
    Character => "character",
    Word => "word",
} default Character);

config_enum!(PanelSection {
    Session => "session",
    Input => "input",
    Paddle => "paddle",
    Progress => "progress",
    Lesson => "lesson",
    Material => "material",
    Timing => "timing",
    Tone => "tone",
    Conditions => "conditions",
    Device => "device",
    Scope => "scope",
    Heatmap => "heatmap",
} default Session);

impl PanelSection {
    /// Localization key of the group title.
    pub fn key(self) -> &'static str {
        match self {
            PanelSection::Session => "group.session",
            PanelSection::Input => "group.input",
            PanelSection::Paddle => "group.paddle",
            PanelSection::Progress => "group.progress",
            PanelSection::Lesson => "group.lesson",
            PanelSection::Material => "group.material",
            PanelSection::Timing => "group.timing",
            PanelSection::Tone => "group.tone",
            PanelSection::Conditions => "group.conditions",
            PanelSection::Device => "group.device",
            PanelSection::Scope => "group.scope",
            PanelSection::Heatmap => "group.heatmap",
        }
    }

    pub fn all() -> Vec<PanelSection> {
        PanelSection::variants()
            .iter()
            .filter_map(|name| PanelSection::from_config(name))
            .collect()
    }
}

// ------------------------------------------------------------------- panel

/// Composable tabs. The settings tab is deliberately absent: it carries the
/// controls that configure everything else, including this list, so a layout
/// that removed it would be unrecoverable from inside the application.
pub const PANEL_TABS: usize = 4;

#[derive(Debug, Clone)]
pub struct PanelSettings {
    pub tabs: [Vec<PanelSection>; PANEL_TABS],
    /// Sections this configuration has already seen.
    ///
    /// Without it there is no way to tell a section the operator removed from
    /// one a later build added, because both are absent from every tab. The
    /// first must stay removed and the second must appear.
    known: Vec<PanelSection>,
}

impl Default for PanelSettings {
    fn default() -> Self {
        use PanelSection::*;
        PanelSettings {
            tabs: [
                vec![Session, Input, Paddle, Progress],
                vec![Lesson, Material],
                vec![Device, Tone, Timing, Conditions],
                vec![Scope, Heatmap],
            ],
            known: PanelSection::all(),
        }
    }
}

impl PanelSettings {
    const KEYS: [&'static str; PANEL_TABS] = ["practice", "lesson", "sound", "display"];

    pub fn sections(&self, tab: usize) -> &[PanelSection] {
        self.tabs.get(tab).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn sections_mut(&mut self, tab: usize) -> Option<&mut Vec<PanelSection>> {
        self.tabs.get_mut(tab)
    }

    /// Restores one tab to the composition the build ships with.
    ///
    /// The editor can empty a tab, and an empty tab offers no way to tell
    /// whether the composition is deliberate or lost.
    pub fn reset_tab(&mut self, tab: usize) {
        let defaults = PanelSettings::default();
        if let (Some(target), Some(source)) = (self.tabs.get_mut(tab), defaults.tabs.get(tab)) {
            *target = source.clone();
        }
    }

    fn adopt_unknown(&mut self) {
        let defaults = PanelSettings::default();
        for section in PanelSection::all() {
            if self.known.contains(&section) {
                continue;
            }
            let home = defaults
                .tabs
                .iter()
                .position(|t| t.contains(&section))
                .unwrap_or(0);
            self.tabs[home].push(section);
            self.known.push(section);
            crate::log_info!(
                "config",
                "[panel] section '{}' is new to this configuration, added to '{}'",
                section.to_config(),
                PanelSettings::KEYS[home]
            );
        }
    }
}

impl SectionIo for PanelSettings {
    const NAME: &'static str = "panel";

    fn load(ini: &Ini) -> Self {
        let mut result = PanelSettings { tabs: Default::default(), known: Vec::new() };
        let mut stored = false;

        for (index, key) in Self::KEYS.iter().enumerate() {
            let raw = ini.get_list(Self::NAME, key);
            if raw.is_empty() {
                result.tabs[index] = PanelSettings::default().tabs[index].clone();
                continue;
            }
            stored = true;
            let mut list = Vec::with_capacity(raw.len());
            for name in &raw {
                match PanelSection::from_config(name.to_ascii_lowercase().as_str()) {
                    Some(s) => {
                        // Duplicates would give one group two identity scopes
                        // and two sets of retained state.
                        if !list.contains(&s) {
                            list.push(s);
                        }
                    }
                    None => crate::log_warn!("config", "[panel] {}: unknown section '{}'", key, name),
                }
            }
            result.tabs[index] = list;
        }

        if !stored {
            return PanelSettings::default();
        }

        for name in ini.get_list(Self::NAME, "known") {
            if let Some(s) = PanelSection::from_config(name.to_ascii_lowercase().as_str()) {
                if !result.known.contains(&s) {
                    result.known.push(s);
                }
            }
        }
        if result.known.is_empty() {
            for tab in &result.tabs {
                for s in tab {
                    if !result.known.contains(s) {
                        result.known.push(*s);
                    }
                }
            }
        }

        result.adopt_unknown();
        result
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Composition of the side panel, one ordered list per tab.");
        ini.comment(s, "Sections: session, input, paddle, progress, lesson, material, timing,");
        ini.comment(s, "tone, conditions, device, scope, heatmap. Edited from the settings tab.");
        for (index, key) in Self::KEYS.iter().enumerate() {
            let names: Vec<String> =
                self.tabs[index].iter().map(|v| v.to_config().to_string()).collect();
            ini.set_list(s, key, &names);
        }
        let mut seen: Vec<PanelSection> = self.known.clone();
        for tab in &self.tabs {
            for section in tab {
                if !seen.contains(section) {
                    seen.push(*section);
                }
            }
        }
        ini.comment(s, "known lists the sections this configuration has met. One absent from");
        ini.comment(s, "it is new to the build and is placed on its default tab; one present");
        ini.comment(s, "here but on no tab was removed deliberately and stays removed.");
        let names: Vec<String> = seen.iter().map(|v| v.to_config().to_string()).collect();
        ini.set_list(s, "known", &names);
    }
}

// -------------------------------------------------------------------- tone

/// What the note sounds like.
#[derive(Debug, Clone, PartialEq)]
pub struct ToneSettings {
    pub pitch_hz: f32,
    /// Nought to one, applied as a curve rather than a ratio, see the helper.
    pub volume: f32,
    pub shape: EnvelopeShape,
    /// Edge durations, stated separately.
    ///
    /// A rise and a fall of the same length is the usual case and not the only
    /// one: a slightly longer fall removes the perceived click at the end of a
    /// dot without lengthening the dot itself, which is what a transmitter with
    /// a shaping network actually does.
    pub rise_ms: f32,
    pub fall_ms: f32,
    /// Balance between the two output channels, minus one to plus one.
    ///
    /// Present because binaural practice is a real technique: the material in
    /// one ear and the interference in the other is measurably easier, and
    /// moving towards the centre is how the difficulty is raised.
    pub pan: f32,
}

impl ToneSettings {
    /// Amplitude for the stated volume.
    ///
    /// A cube rather than a ratio, because loudness is roughly logarithmic and
    /// a linear control spends nine tenths of its travel above comfortable.
    pub fn gain(&self) -> f32 {
        let v = self.volume.clamp(0.0, 1.0);
        v * v * v
    }
}

impl Default for ToneSettings {
    fn default() -> Self {
        ToneSettings {
            // Around six hundred is where the ear resolves keying best and
            // where most operators set a sidetone.
            pitch_hz: 600.0,
            volume: 0.35,
            shape: EnvelopeShape::RaisedCosine,
            rise_ms: 5.0,
            fall_ms: 5.0,
            pan: 0.0,
        }
    }
}

impl SectionIo for ToneSettings {
    const NAME: &'static str = "tone";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        ToneSettings {
            pitch_hz: ini.get_f32_clamped(Self::NAME, "pitch_hz", d.pitch_hz, 200.0, 1500.0),
            volume: ini.get_f32_clamped(Self::NAME, "volume", d.volume, 0.0, 1.0),
            shape: ini.get_enum(Self::NAME, "shape", d.shape),
            rise_ms: ini.get_f32_clamped(Self::NAME, "rise_ms", d.rise_ms, 0.0, 20.0),
            fall_ms: ini.get_f32_clamped(Self::NAME, "fall_ms", d.fall_ms, 0.0, 20.0),
            pan: ini.get_f32_clamped(Self::NAME, "pan", d.pan, -1.0, 1.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "The note itself. Around six hundred hertz is where the ear resolves");
        ini.comment(s, "keying best; a very low or very high pitch is worth practising once");
        ini.comment(s, "the material is comfortable, because a real signal lands anywhere.");
        ini.set_f32(s, "pitch_hz", self.pitch_hz);
        ini.set_f32(s, "volume", self.volume);
        ini.comment(s, "shape: raised_cosine is what a transmitter produces, hard splashes");
        ini.comment(s, "and teaches the ear to key on the click rather than on the tone.");
        ini.comment(s, "Practise against hard keying deliberately, not by accident.");
        ini.set_enum(s, "shape", self.shape);
        ini.comment(s, "The two edges are stated separately: a slightly longer fall removes");
        ini.comment(s, "the click at the end of a dot without lengthening the dot.");
        ini.set_f32(s, "rise_ms", self.rise_ms);
        ini.set_f32(s, "fall_ms", self.fall_ms);
        ini.comment(s, "pan puts the material in one ear. Binaural separation is measurably");
        ini.comment(s, "easier, so moving towards the centre is how the difficulty is raised.");
        ini.set_f32(s, "pan", self.pan);
    }
}

// ------------------------------------------------------------------ timing

/// Element and gap lengths.
///
/// ## Why two speeds
///
/// A character sent at twenty words a minute and a text sent at twenty words a
/// minute are different things. The first is a statement about the elements,
/// the second about the elements plus the gaps. Stretching only the gaps is
/// what lets a student hear a character at its final speed from the first
/// lesson and still have time to write it down, and it is the one arrangement
/// that does not have to be unlearned later.
#[derive(Debug, Clone, PartialEq)]
pub struct TimingSettings {
    /// Speed the elements themselves are sent at.
    pub char_wpm: f32,
    /// Speed the whole text comes out at, gaps included.
    pub text_wpm: f32,
    /// Stretch the gaps rather than the elements.
    pub farnsworth: bool,
    /// Dash length as a multiple of a dot.
    ///
    /// Three by definition and not in practice: a hand key sits between two and
    /// a half and three and a half, and a student who has only heard exactly
    /// three cannot copy a hand.
    pub weight: f32,
    /// Gaps, in dot units.
    pub element_gap: f32,
    pub char_gap: f32,
    pub word_gap: f32,
    /// Random variation of every duration, as a percentage.
    ///
    /// The single most valuable setting here. Machine timing is a different
    /// signal from a human one, and a student trained on nought per cent copies
    /// a machine and nothing else.
    pub jitter_percent: f32,
    /// Systematic shortening of the first element of a pair, as a percentage.
    ///
    /// What a mechanical bug produces and what an experienced operator does
    /// without noticing. Distinct from jitter because it is a bias rather than
    /// a spread, and the ear learns the two separately.
    pub swing_percent: f32,
}

impl TimingSettings {
    /// Dot length, in seconds, for a stated speed.
    ///
    /// One point two over the speed, which follows from the reference word
    /// occupying fifty dot units.
    pub fn dot_seconds(&self) -> f32 {
        1.2 / self.char_wpm.max(1.0)
    }

    /// The three gap durations, in seconds.
    ///
    /// ## Where the two stretched gaps come from
    ///
    /// With the two speeds apart the gaps are not settings: they follow from the
    /// requirement that the text occupy the time the text speed asks for.
    ///
    /// The reference word is fifty dot units, of which thirty one are elements
    /// and their internal gaps and nineteen are the gaps between characters and
    /// words. So the time the gaps must occupy is the required word duration,
    /// sixty over the text speed, less the time the elements take, thirty seven
    /// and a fifth over the character speed. That total is then split in the
    /// ratio the standard gaps already have: three parts to each of the four
    /// character gaps and seven to the word gap.
    ///
    /// It follows that the two stated gap settings do nothing while the speeds
    /// are apart, which is why the interface refuses them there rather than
    /// applying them silently.
    pub fn gaps(&self) -> (f32, f32, f32) {
        let dot = self.dot_seconds();
        let element = dot * self.element_gap;

        if !self.farnsworth || self.text_wpm >= self.char_wpm {
            return (element, dot * self.char_gap, dot * self.word_gap);
        }

        let c = self.char_wpm.max(1.0);
        let t = self.text_wpm.max(1.0);
        let total = (60.0 / t - 37.2 / c).max(0.0);
        // Never below the standard gaps. A text speed above the character speed
        // is refused on load, but a rounding at the boundary could otherwise
        // produce a character gap shorter than an element gap, which is not a
        // faster text but a different alphabet.
        let char_gap = (3.0 * total / 19.0).max(dot * 3.0);
        let word_gap = (7.0 * total / 19.0).max(dot * 7.0);
        (element, char_gap, word_gap)
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    fn at(char_wpm: f32, text_wpm: f32) -> TimingSettings {
        TimingSettings {
            char_wpm,
            text_wpm,
            farnsworth: true,
            ..TimingSettings::default()
        }
    }

    #[test]
    fn the_standard_gaps_are_one_three_and_seven() {
        // With the speeds together nothing is derived, so the stated ratios have
        // to come through exactly: a factor applied where none was asked for
        // would change every timing in the application.
        let t = at(20.0, 20.0);
        let dot = t.dot_seconds();
        let (element, character, word) = t.gaps();
        assert!((element / dot - 1.0).abs() < 1e-5);
        assert!((character / dot - 3.0).abs() < 1e-5);
        assert!((word / dot - 7.0).abs() < 1e-5);
    }

    #[test]
    fn the_stretched_word_takes_the_time_the_text_speed_states() {
        // The one arithmetic check that matters. Thirty one units of element and
        // the derived gaps have to add up to a word at the text speed, or the
        // whole arrangement is a slower character speed wearing a label.
        let t = at(20.0, 12.0);
        let dot = t.dot_seconds();
        let (_, character, word) = t.gaps();
        let total = 31.0 * dot + 4.0 * character + word;
        assert!((total - 5.0).abs() < 0.01, "the word took {:.4} s", total);
    }

    #[test]
    fn the_gaps_keep_their_ratio_while_they_stretch() {
        let t = at(25.0, 10.0);
        let (_, character, word) = t.gaps();
        assert!((word / character - 7.0 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn a_text_speed_at_the_character_speed_derives_nothing() {
        let t = at(18.0, 18.0);
        let dot = t.dot_seconds();
        let (_, character, _) = t.gaps();
        assert!((character / dot - 3.0).abs() < 1e-5);
    }
}

impl Default for TimingSettings {
    fn default() -> Self {
        TimingSettings {
            char_wpm: 20.0,
            text_wpm: 12.0,
            farnsworth: true,
            weight: 3.0,
            element_gap: 1.0,
            char_gap: 3.0,
            word_gap: 7.0,
            jitter_percent: 0.0,
            swing_percent: 0.0,
        }
    }
}

impl SectionIo for TimingSettings {
    const NAME: &'static str = "timing";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let char_wpm = ini.get_f32_clamped(Self::NAME, "char_wpm", d.char_wpm, 5.0, 60.0);
        let text_wpm = ini.get_f32_clamped(Self::NAME, "text_wpm", d.text_wpm, 3.0, char_wpm);
        TimingSettings {
            char_wpm,
            text_wpm,
            farnsworth: ini.get_bool(Self::NAME, "farnsworth", d.farnsworth),
            weight: ini.get_f32_clamped(Self::NAME, "weight", d.weight, 2.0, 4.5),
            element_gap: ini.get_f32_clamped(Self::NAME, "element_gap", d.element_gap, 0.5, 2.0),
            char_gap: ini.get_f32_clamped(Self::NAME, "char_gap", d.char_gap, 2.0, 12.0),
            word_gap: ini.get_f32_clamped(Self::NAME, "word_gap", d.word_gap, 4.0, 24.0),
            jitter_percent: ini.get_f32_clamped(Self::NAME, "jitter_percent", d.jitter_percent, 0.0, 40.0),
            swing_percent: ini.get_f32_clamped(Self::NAME, "swing_percent", d.swing_percent, 0.0, 30.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "char_wpm is the speed of the elements, text_wpm the speed of the whole");
        ini.comment(s, "text. With farnsworth on, only the gaps are stretched, so a character");
        ini.comment(s, "is heard at its final speed from the first lesson and nothing has to");
        ini.comment(s, "be unlearned when the gaps close.");
        ini.set_f32(s, "char_wpm", self.char_wpm);
        ini.set_f32(s, "text_wpm", self.text_wpm);
        ini.set_bool(s, "farnsworth", self.farnsworth);
        ini.comment(s, "weight is the dash as a multiple of a dot. Three by definition and not");
        ini.comment(s, "in practice: a hand key sits between two and a half and three and a");
        ini.comment(s, "half, and a student who has only heard three cannot copy a hand.");
        ini.set_f32(s, "weight", self.weight);
        ini.comment(s, "Gaps in dot units. One, three and seven are the standard. The last two");
        ini.comment(s, "are ignored while farnsworth is on, because the gaps then follow from");
        ini.comment(s, "the text speed rather than being chosen: switch it off to state them.");
        ini.set_f32(s, "element_gap", self.element_gap);
        ini.set_f32(s, "char_gap", self.char_gap);
        ini.set_f32(s, "word_gap", self.word_gap);
        ini.comment(s, "jitter_percent varies every duration at random. The most valuable");
        ini.comment(s, "number here: machine timing is a different signal from a human one,");
        ini.comment(s, "and a student trained at nought copies a machine and nothing else.");
        ini.set_f32(s, "jitter_percent", self.jitter_percent);
        ini.comment(s, "swing_percent shortens the first element of a pair systematically,");
        ini.comment(s, "which is what a mechanical bug produces. A bias rather than a spread,");
        ini.comment(s, "so the ear learns it separately from jitter.");
        ini.set_f32(s, "swing_percent", self.swing_percent);
    }
}

// ------------------------------------------------------------------ lesson

#[derive(Debug, Clone, PartialEq)]
pub struct LessonSettings {
    pub method: LessonMethod,
    /// Characters introduced so far, for the incremental method.
    pub level: u32,
    /// Set used when the method is custom.
    pub custom_set: String,
    pub session_seconds: u32,
    /// Share of correct characters needed before the next one is introduced.
    pub advance_accuracy: f32,
    /// Share below which the newest character is withdrawn again.
    ///
    /// Present because a level that only ever rises turns a bad evening into a
    /// permanent wall: the student is then practising a set they cannot copy,
    /// which is the one condition under which nothing improves.
    pub regress_accuracy: f32,
    pub auto_advance: bool,
    /// Extra weight given to the characters being missed.
    ///
    /// Nought sends every character equally often, which spends most of the
    /// session on the ones already known.
    pub weak_weight: f32,
}

impl Default for LessonSettings {
    fn default() -> Self {
        LessonSettings {
            method: LessonMethod::Koch,
            // Two characters, because a single one carries no decision and
            // therefore trains nothing.
            level: 2,
            custom_set: String::new(),
            session_seconds: 300,
            advance_accuracy: 0.90,
            regress_accuracy: 0.55,
            auto_advance: true,
            weak_weight: 2.0,
        }
    }
}

impl SectionIo for LessonSettings {
    const NAME: &'static str = "lesson";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let advance = ini.get_f32_clamped(Self::NAME, "advance_accuracy", d.advance_accuracy, 0.5, 1.0);
        LessonSettings {
            method: ini.get_enum(Self::NAME, "method", d.method),
            level: ini.get_u32_clamped(Self::NAME, "level", d.level, 2, 64),
            custom_set: ini.get_string(Self::NAME, "custom_set", &d.custom_set),
            session_seconds: ini.get_u32_clamped(Self::NAME, "session_seconds", d.session_seconds, 30, 3600),
            advance_accuracy: advance,
            // Bounded below the advance threshold, otherwise the level would
            // rise and fall on the same reading.
            regress_accuracy: ini
                .get_f32_clamped(Self::NAME, "regress_accuracy", d.regress_accuracy, 0.0, advance - 0.05),
            auto_advance: ini.get_bool(Self::NAME, "auto_advance", d.auto_advance),
            weak_weight: ini.get_f32_clamped(Self::NAME, "weak_weight", d.weak_weight, 0.0, 8.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "method: koch introduces two characters at full speed and adds one at a");
        ini.comment(s, "time, which produces reflex recognition rather than counting.");
        ini.comment(s, "alphabet and frequency exist to match a printed course.");
        ini.set_enum(s, "method", self.method);
        ini.comment(s, "level is how many characters have been introduced. Maintained by the");
        ini.comment(s, "session and safe to set by hand.");
        ini.set_u32(s, "level", self.level);
        ini.set_string(s, "custom_set", &self.custom_set);
        ini.set_u32(s, "session_seconds", self.session_seconds);
        ini.comment(s, "The level rises above advance_accuracy and falls below the other. It");
        ini.comment(s, "has to be able to fall: a level that only rises turns a bad evening");
        ini.comment(s, "into a wall, and practising a set you cannot copy improves nothing.");
        ini.set_f32(s, "advance_accuracy", self.advance_accuracy);
        ini.set_f32(s, "regress_accuracy", self.regress_accuracy);
        ini.set_bool(s, "auto_advance", self.auto_advance);
        ini.comment(s, "weak_weight sends the characters being missed more often. Nought");
        ini.comment(s, "spends most of the session on the ones already known.");
        ini.set_f32(s, "weak_weight", self.weak_weight);
    }
}

// ---------------------------------------------------------------- material

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialSettings {
    pub source: MaterialSource,
    pub min_group: u32,
    pub max_group: u32,
    pub include_numbers: bool,
    pub include_punctuation: bool,
    pub include_prosigns: bool,
    /// Word list or plain text, read when the source names a file.
    pub file_path: String,
    /// Times the material is sent before the answer is taken.
    ///
    /// One is the ordinary case. Above it the same material is sent again before
    /// anything is asked, which is what an operator receives after asking for
    /// `AGN`: a second hearing of the same thing rather than a second question.
    ///
    /// Reaction times are not recorded while this is above one. An interval
    /// measured from a character heard three times is not a reaction, and
    /// averaging it in would make the figure improve as the setting rose.
    pub repeat: u32,
}

impl Default for MaterialSettings {
    fn default() -> Self {
        MaterialSettings {
            source: MaterialSource::Groups,
            // Five is the group length every printed exercise and every code
            // test uses, so a student can compare against them.
            min_group: 5,
            max_group: 5,
            include_numbers: true,
            include_punctuation: false,
            include_prosigns: false,
            file_path: String::new(),
            repeat: 1,
        }
    }
}

impl SectionIo for MaterialSettings {
    const NAME: &'static str = "material";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let min = ini.get_u32_clamped(Self::NAME, "min_group", d.min_group, 1, 20);
        MaterialSettings {
            source: ini.get_enum(Self::NAME, "source", d.source),
            min_group: min,
            max_group: ini.get_u32_clamped(Self::NAME, "max_group", d.max_group.max(min), min, 20),
            include_numbers: ini.get_bool(Self::NAME, "include_numbers", d.include_numbers),
            include_punctuation: ini.get_bool(Self::NAME, "include_punctuation", d.include_punctuation),
            include_prosigns: ini.get_bool(Self::NAME, "include_prosigns", d.include_prosigns),
            file_path: ini.get_string(Self::NAME, "file_path", &d.file_path),
            repeat: ini.get_u32_clamped(Self::NAME, "repeat", d.repeat, 1, 5),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "source: groups is random characters from the current set, which trains");
        ini.comment(s, "recognition and nothing else. qcodes, abbrev, callsigns, words and qso");
        ini.comment(s, "train what an operator actually copies, which is a small vocabulary");
        ini.comment(s, "heard as whole shapes rather than spelled out. Reach them early.");
        ini.comment(s, "The first four ignore the lesson level, because a Q code or a callsign");
        ini.comment(s, "built out of two characters is not one.");
        ini.set_enum(s, "source", self.source);
        ini.comment(s, "Five is the group length every printed exercise uses, so a student");
        ini.comment(s, "can compare against one. Equal bounds give a fixed length.");
        ini.set_u32(s, "min_group", self.min_group);
        ini.set_u32(s, "max_group", self.max_group);
        ini.set_bool(s, "include_numbers", self.include_numbers);
        ini.set_bool(s, "include_punctuation", self.include_punctuation);
        ini.set_bool(s, "include_prosigns", self.include_prosigns);
        ini.set_string(s, "file_path", &self.file_path);
        ini.comment(s, "repeat sends the material again before the answer is taken, which is");
        ini.comment(s, "what an operator receives after asking for AGN: a second hearing rather");
        ini.comment(s, "than a second question. Reaction times are not recorded above one,");
        ini.comment(s, "because an interval measured across three hearings is not a reaction.");
        ini.set_u32(s, "repeat", self.repeat);
    }
}

// ---------------------------------------------------------------- practice

#[derive(Debug, Clone, PartialEq)]
pub struct PracticeSettings {
    pub mode: PracticeMode,
    pub case: TextCase,
    /// Exercise in force, which replaces the mode while it is not off.
    pub drill: DrillMode,
    pub drill_unit: DrillUnit,
    /// How much the character introduced last outweighs the rest.
    ///
    /// The incremental method is a set that grows by one and a session spent on
    /// the one that arrived: drawn evenly, a drill spends most of itself on
    /// characters that were learned weeks ago.
    pub drill_focus: f32,
    /// Show what was sent after the answer.
    pub reveal: bool,
    /// Wait before revealing, in milliseconds.
    ///
    /// A reveal that arrives with the answer removes the moment where the ear
    /// commits, which is the moment the learning happens.
    pub reveal_delay_ms: u32,
    pub allow_backspace: bool,
    /// Longest wait for an answer before the group counts as missed.
    pub answer_timeout_ms: u32,
    /// Count a substitution and an omission separately.
    pub strict: bool,
}

impl PracticeSettings {
    /// True when the answer arrives on the key rather than on the keyboard.
    pub fn keying(&self) -> bool {
        match self.drill {
            DrillMode::Off => self.mode == PracticeMode::Send,
            DrillMode::Recall | DrillMode::Echo => true,
            DrillMode::Blind => false,
        }
    }

    /// True when the material is played.
    ///
    /// Held apart from the predicate above because the echo drill is both: it
    /// plays the material and takes a keyed answer, which the practice mode has
    /// no way to state.
    pub fn sounded(&self) -> bool {
        match self.drill {
            DrillMode::Off => self.mode != PracticeMode::Send,
            DrillMode::Recall => false,
            DrillMode::Echo | DrillMode::Blind => true,
        }
    }

    /// True when nothing is asked for.
    pub fn listening(&self) -> bool {
        self.drill == DrillMode::Off && self.mode == PracticeMode::Listen
    }
}

impl Default for PracticeSettings {
    fn default() -> Self {
        PracticeSettings {
            mode: PracticeMode::Copy,
            case: TextCase::Upper,
            drill: DrillMode::Off,
            drill_unit: DrillUnit::Character,
            drill_focus: 3.0,
            reveal: true,
            reveal_delay_ms: 400,
            allow_backspace: false,
            answer_timeout_ms: 8000,
            strict: true,
        }
    }
}

impl SectionIo for PracticeSettings {
    const NAME: &'static str = "practice";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        PracticeSettings {
            mode: ini.get_enum(Self::NAME, "mode", d.mode),
            case: ini.get_enum(Self::NAME, "case", d.case),
            drill: ini.get_enum(Self::NAME, "drill", d.drill),
            drill_unit: ini.get_enum(Self::NAME, "drill_unit", d.drill_unit),
            drill_focus: ini.get_f32_clamped(Self::NAME, "drill_focus", d.drill_focus, 0.0, 8.0),
            reveal: ini.get_bool(Self::NAME, "reveal", d.reveal),
            reveal_delay_ms: ini.get_u32_clamped(Self::NAME, "reveal_delay_ms", d.reveal_delay_ms, 0, 5000),
            allow_backspace: ini.get_bool(Self::NAME, "allow_backspace", d.allow_backspace),
            answer_timeout_ms: ini
                .get_u32_clamped(Self::NAME, "answer_timeout_ms", d.answer_timeout_ms, 500, 60000),
            strict: ini.get_bool(Self::NAME, "strict", d.strict),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "mode: listen plays without asking, copy expects the text typed,");
        ini.comment(s, "head_copy asks only at the end of a group, send waits for a key.");
        ini.set_enum(s, "mode", self.mode);
        ini.set_enum(s, "case", self.case);
        ini.comment(s, "drill replaces the mode while it is not off, because each of the three");
        ini.comment(s, "states both what is played and what answers it: recall shows the");
        ini.comment(s, "character and asks for the pattern, echo plays it first, blind plays it");
        ini.comment(s, "and withholds it. drill_unit is what one prompt carries.");
        ini.set_enum(s, "drill", self.drill);
        ini.set_enum(s, "drill_unit", self.drill_unit);
        ini.comment(s, "drill_focus is how much the character introduced last outweighs the");
        ini.comment(s, "rest. Nought draws evenly, which spends most of a drill on characters");
        ini.comment(s, "already known and is not what the incremental method is.");
        ini.set_f32(s, "drill_focus", self.drill_focus);
        ini.comment(s, "reveal_delay_ms holds the answer back briefly. A reveal that arrives");
        ini.comment(s, "with the keystroke removes the moment the ear commits, which is the");
        ini.comment(s, "moment the learning happens.");
        ini.set_bool(s, "reveal", self.reveal);
        ini.set_u32(s, "reveal_delay_ms", self.reveal_delay_ms);
        ini.comment(s, "allow_backspace off is deliberate: on the air there is no correcting");
        ini.comment(s, "a character once it has gone past.");
        ini.set_bool(s, "allow_backspace", self.allow_backspace);
        ini.set_u32(s, "answer_timeout_ms", self.answer_timeout_ms);
        ini.set_bool(s, "strict", self.strict);
    }
}

// ------------------------------------------------------------------ paddle

/// The key the student sends with.
///
/// The speed is not here: it is the character speed the rest of the application
/// already states, and a second one would let the two disagree. Nor is the dash
/// length, for the same reason: what a keyer sends as a dash is the weight
/// setting, and an operator practising against a hand key wants the two to
/// match.
#[derive(Debug, Clone, PartialEq)]
pub struct PaddleSettings {
    pub mode: PaddleMode,
    pub source: PaddleSource,
    /// The left contact sends the dash.
    ///
    /// Present because a paddle is wired once and a left handed operator wires
    /// it the other way round, and the alternative is unsoldering it.
    pub swap: bool,
    /// Hear what is being keyed.
    pub sidetone: bool,
    /// Letters the keyboard contacts are bound to.
    ///
    /// Letters only. Punctuation reaches the application as a virtual key that
    /// depends on the layout, so a comma bound on one keyboard is something else
    /// on the next, and a setting that means two things is worse than a setting
    /// that offers fewer.
    pub key_dit: String,
    pub key_dah: String,
}

impl PaddleSettings {
    /// The letter one contact is bound to, folded to upper case.
    ///
    /// Nought when the setting holds nothing usable, which the caller reads as a
    /// contact that is not bound rather than as one bound to a stray character.
    pub fn letter(text: &str) -> u8 {
        match text.chars().next() {
            Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase() as u8,
            _ => 0,
        }
    }
}

impl Default for PaddleSettings {
    fn default() -> Self {
        PaddleSettings {
            mode: PaddleMode::IambicB,
            source: PaddleSource::Mouse,
            swap: false,
            sidetone: true,
            key_dit: "Z".to_string(),
            key_dah: "X".to_string(),
        }
    }
}

impl SectionIo for PaddleSettings {
    const NAME: &'static str = "paddle";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let mut out = PaddleSettings {
            mode: ini.get_enum(Self::NAME, "mode", d.mode),
            source: ini.get_enum(Self::NAME, "source", d.source),
            swap: ini.get_bool(Self::NAME, "swap", d.swap),
            sidetone: ini.get_bool(Self::NAME, "sidetone", d.sidetone),
            key_dit: ini.get_string(Self::NAME, "key_dit", &d.key_dit),
            key_dah: ini.get_string(Self::NAME, "key_dah", &d.key_dah),
        };
        // A contact bound to nothing cannot be pressed, which reads as a broken
        // key rather than as a typing mistake in a configuration file.
        if PaddleSettings::letter(&out.key_dit) == 0 {
            crate::log_warn!("config", "[paddle] key_dit: '{}' is not a letter", out.key_dit);
            out.key_dit = d.key_dit.clone();
        }
        if PaddleSettings::letter(&out.key_dah) == 0 {
            crate::log_warn!("config", "[paddle] key_dah: '{}' is not a letter", out.key_dah);
            out.key_dah = d.key_dah.clone();
        }
        // Two contacts on one letter is one contact, and the squeeze that makes
        // an iambic keyer an iambic keyer would be unreachable.
        if PaddleSettings::letter(&out.key_dit) == PaddleSettings::letter(&out.key_dah) {
            crate::log_warn!("config", "[paddle] both contacts are on the same letter");
            out.key_dit = d.key_dit;
            out.key_dah = d.key_dah;
        }
        out
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "The key the student sends with. The speed comes from timing.char_wpm");
        ini.comment(s, "and the dash from timing.weight: a second pair here would let the two");
        ini.comment(s, "disagree, and a keyer that sends a different dash from the trainer is");
        ini.comment(s, "a keyer that teaches the wrong dash.");
        ini.comment(s, "mode: straight sends what the hand does, which is the harder exercise;");
        ini.comment(s, "the iambic forms keep time themselves and differ only in what the");
        ini.comment(s, "release of a squeeze does. iambic_b sends one more element there.");
        ini.set_enum(s, "mode", self.mode);
        ini.comment(s, "source mouse expects a paddle clipped across the two mouse switches,");
        ini.comment(s, "which is the usual arrangement. While a send session runs the buttons");
        ini.comment(s, "are contacts and not a pointer; escape ends the session.");
        ini.set_enum(s, "source", self.source);
        ini.set_bool(s, "swap", self.swap);
        ini.set_bool(s, "sidetone", self.sidetone);
        ini.comment(s, "Keyboard contacts, one letter each. Punctuation is not offered because");
        ini.comment(s, "its virtual key depends on the layout, so a comma bound on one keyboard");
        ini.comment(s, "is something else on the next.");
        ini.set_string(s, "key_dit", &self.key_dit);
        ini.set_string(s, "key_dah", &self.key_dah);
    }
}

// -------------------------------------------------------------- conditions

/// What the band is doing underneath the material.
///
/// The gap between a trainer and the air. A clean tone in silence is a signal
/// nobody ever receives, and a student who has only copied one hears real
/// traffic as a different alphabet.
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionsSettings {
    pub noise: bool,
    /// Ratio of the tone to the noise, in decibels.
    pub snr_db: f32,
    /// Slow fading.
    pub qsb: bool,
    pub qsb_rate_hz: f32,
    pub qsb_depth_db: f32,
    /// A second station beside the one being copied.
    pub qrm: bool,
    pub qrm_offset_hz: f32,
    pub qrm_level_db: f32,
    /// Impulse noise.
    pub qrn: bool,
    pub qrn_per_minute: f32,
    /// Slow drift of the pitch, in hertz per minute.
    ///
    /// What an unstable transmitter does, and what teaches the ear not to lock
    /// onto an exact frequency.
    pub drift_hz_per_min: f32,
}

impl Default for ConditionsSettings {
    fn default() -> Self {
        ConditionsSettings {
            noise: false,
            snr_db: 12.0,
            qsb: false,
            qsb_rate_hz: 0.2,
            qsb_depth_db: 12.0,
            qrm: false,
            qrm_offset_hz: 120.0,
            qrm_level_db: -6.0,
            qrn: false,
            qrn_per_minute: 20.0,
            drift_hz_per_min: 0.0,
        }
    }
}

impl SectionIo for ConditionsSettings {
    const NAME: &'static str = "conditions";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        ConditionsSettings {
            noise: ini.get_bool(Self::NAME, "noise", d.noise),
            snr_db: ini.get_f32_clamped(Self::NAME, "snr_db", d.snr_db, -10.0, 40.0),
            qsb: ini.get_bool(Self::NAME, "qsb", d.qsb),
            qsb_rate_hz: ini.get_f32_clamped(Self::NAME, "qsb_rate_hz", d.qsb_rate_hz, 0.02, 2.0),
            qsb_depth_db: ini.get_f32_clamped(Self::NAME, "qsb_depth_db", d.qsb_depth_db, 1.0, 40.0),
            qrm: ini.get_bool(Self::NAME, "qrm", d.qrm),
            qrm_offset_hz: ini.get_f32_clamped(Self::NAME, "qrm_offset_hz", d.qrm_offset_hz, 20.0, 800.0),
            qrm_level_db: ini.get_f32_clamped(Self::NAME, "qrm_level_db", d.qrm_level_db, -30.0, 6.0),
            qrn: ini.get_bool(Self::NAME, "qrn", d.qrn),
            qrn_per_minute: ini.get_f32_clamped(Self::NAME, "qrn_per_minute", d.qrn_per_minute, 1.0, 600.0),
            drift_hz_per_min: ini
                .get_f32_clamped(Self::NAME, "drift_hz_per_min", d.drift_hz_per_min, -120.0, 120.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Band conditions. A clean tone in silence is a signal nobody receives,");
        ini.comment(s, "and a student who has only copied one hears real traffic as a");
        ini.comment(s, "different alphabet. Raise these once the material is comfortable.");
        ini.set_bool(s, "noise", self.noise);
        ini.set_f32(s, "snr_db", self.snr_db);
        ini.comment(s, "qsb is slow fading, which is what makes a signal disappear mid word.");
        ini.set_bool(s, "qsb", self.qsb);
        ini.set_f32(s, "qsb_rate_hz", self.qsb_rate_hz);
        ini.set_f32(s, "qsb_depth_db", self.qsb_depth_db);
        ini.comment(s, "qrm is a second station beside the first. The offset is what decides");
        ini.comment(s, "whether the ear can separate them at all.");
        ini.set_bool(s, "qrm", self.qrm);
        ini.set_f32(s, "qrm_offset_hz", self.qrm_offset_hz);
        ini.set_f32(s, "qrm_level_db", self.qrm_level_db);
        ini.set_bool(s, "qrn", self.qrn);
        ini.set_f32(s, "qrn_per_minute", self.qrn_per_minute);
        ini.comment(s, "drift teaches the ear not to lock onto an exact frequency.");
        ini.set_f32(s, "drift_hz_per_min", self.drift_hz_per_min);
    }
}

// ------------------------------------------------------------------- audio

/// Where the sound goes.
///
/// Three keys, and every one of them does something. There is no rate and no
/// exclusive mode: in shared mode the mixer hands back its own format whatever
/// is asked for, so a stated rate is a setting that cannot fail and cannot take
/// effect, and exclusive mode would lock the endpoint away from every other
/// application in exchange for a latency nothing here responds to and a bit
/// perfect path for a sine this application generated.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSettings {
    /// Render endpoint, empty for the system default.
    pub device_id: String,
    pub device_name: String,
    pub buffer_ms: u32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            device_id: String::new(),
            device_name: String::new(),
            // Twenty milliseconds is well under the shortest element at any
            // speed this trainer offers, so the keying is never quantized by
            // the buffer.
            buffer_ms: 20,
        }
    }
}

impl SectionIo for AudioSettings {
    const NAME: &'static str = "audio";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        AudioSettings {
            device_id: ini.get_string(Self::NAME, "device_id", &d.device_id),
            device_name: ini.get_string(Self::NAME, "device_name", &d.device_name),
            buffer_ms: ini.get_u32_clamped(Self::NAME, "buffer_ms", d.buffer_ms, 2, 200),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Output endpoint. Empty selects the system default.");
        ini.set_string(s, "device_id", &self.device_id);
        ini.set_string(s, "device_name", &self.device_name);
        ini.comment(s, "The rate is not a setting: in shared mode the mixer hands back its own");
        ini.comment(s, "format whatever is asked for, and the generator produces samples at");
        ini.comment(s, "whatever rate that turns out to be, so there is nothing to resample.");
        ini.comment(s, "buffer_ms is well under the shortest element at any speed offered, so");
        ini.comment(s, "the keying is never quantized by the buffer. Applied on the next open.");
        ini.set_u32(s, "buffer_ms", self.buffer_ms);
    }
}

// ------------------------------------------------------------------- scope

/// The keying picture.
///
/// A trainer that only plays leaves the student guessing whether a character
/// was long or badly timed. Drawing the envelope against a unit grid answers
/// that directly, which is why it is here and not in the appearance section:
/// it is instrumentation rather than decoration.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeSettings {
    pub visible: bool,
    /// Seconds of history the picture holds.
    pub seconds: f32,
    /// Vertical lines at dot boundaries.
    pub unit_grid: bool,
    /// Mark where the ideal element edges would have been.
    ///
    /// The whole reason to draw this at all under jitter: the difference
    /// between what was sent and what should have been sent is the lesson.
    pub show_ideal: bool,
    /// Characters, above the trace, where they sounded.
    pub show_labels: bool,
    /// Element lengths against the lengths they should have had.
    pub show_timing: bool,
    pub height_fraction: f32,
}

impl Default for ScopeSettings {
    fn default() -> Self {
        ScopeSettings {
            visible: true,
            seconds: 4.0,
            unit_grid: true,
            show_ideal: true,
            show_labels: true,
            show_timing: true,
            height_fraction: 0.35,
        }
    }
}

impl SectionIo for ScopeSettings {
    const NAME: &'static str = "scope";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        ScopeSettings {
            visible: ini.get_bool(Self::NAME, "visible", d.visible),
            // Down to a twentieth of a second, which is where a single edge
            // fills the picture: at sixty words a minute a dot is twenty
            // milliseconds, and examining its shape is exactly what the shape
            // setting is judged by.
            seconds: ini.get_f32_clamped(Self::NAME, "seconds", d.seconds, 0.05, 30.0),
            unit_grid: ini.get_bool(Self::NAME, "unit_grid", d.unit_grid),
            show_ideal: ini.get_bool(Self::NAME, "show_ideal", d.show_ideal),
            show_labels: ini.get_bool(Self::NAME, "show_labels", d.show_labels),
            show_timing: ini.get_bool(Self::NAME, "show_timing", d.show_timing),
            height_fraction: ini
                .get_f32_clamped(Self::NAME, "height_fraction", d.height_fraction, 0.1, 0.7),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "The keying picture. Instrumentation rather than decoration: without");
        ini.comment(s, "it a student cannot tell a long dash from a badly timed one.");
        ini.set_bool(s, "visible", self.visible);
        ini.set_f32(s, "seconds", self.seconds);
        ini.set_bool(s, "unit_grid", self.unit_grid);
        ini.comment(s, "show_ideal marks where the edges would have been without jitter. The");
        ini.comment(s, "difference between sent and ideal is the whole lesson.");
        ini.set_bool(s, "show_ideal", self.show_ideal);
        ini.comment(s, "show_labels names each burst, so a picture read after the fact says");
        ini.comment(s, "what was sent rather than only how long it was. show_timing is the");
        ini.comment(s, "error of every element against its ideal length, which is the reading");
        ini.comment(s, "the envelope cannot give: a shape states what happened and not how far");
        ini.comment(s, "off it was.");
        ini.set_bool(s, "show_labels", self.show_labels);
        ini.set_bool(s, "show_timing", self.show_timing);
        ini.set_f32(s, "height_fraction", self.height_fraction);
    }
}

// ---------------------------------------------------------------- progress

#[derive(Debug, Clone, PartialEq)]
pub struct ProgressSettings {
    /// Where the per character history is kept.
    pub path: String,
    /// Lines the session log keeps, nought keeping everything.
    ///
    /// The log is rewritten to its tail when it grows past this, which is the
    /// only way to shorten a file from the front. Nought is offered because an
    /// operator running the log through another tool needs it: a reader following
    /// the file would lose its position at every rewrite.
    pub keep_sessions: u32,
    /// Answers each character remembers.
    ///
    /// The window the accuracy is measured over. Too short and one mistake
    /// withdraws a character; too long and yesterday decides today.
    pub window: u32,
    pub log_sessions: bool,
}

impl Default for ProgressSettings {
    fn default() -> Self {
        ProgressSettings {
            path: "progress/cwdrill.ini".to_string(),
            keep_sessions: 200,
            window: 40,
            log_sessions: true,
        }
    }
}

impl SectionIo for ProgressSettings {
    const NAME: &'static str = "progress";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        ProgressSettings {
            path: ini.get_string(Self::NAME, "path", &d.path),
            keep_sessions: ini.get_u32_clamped(Self::NAME, "keep_sessions", d.keep_sessions, 0, 100_000),
            // Bounded by the storage rather than by taste: the window is bits in
            // one word, and a longer one would need a vector per character to
            // hold statistics that say nothing about the session in progress.
            window: ini.get_u32_clamped(Self::NAME, "window", d.window, 5, 64),
            log_sessions: ini.get_bool(Self::NAME, "log_sessions", d.log_sessions),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "A relative path is taken from the directory of the configuration.");
        ini.set_string(s, "path", &self.path);
        ini.comment(s, "keep_sessions bounds sessions.txt, which is rewritten to its tail when");
        ini.comment(s, "it grows past it. Nought keeps everything, for a log another tool is");
        ini.comment(s, "following: a reader would lose its position at every rewrite.");
        ini.set_u32(s, "keep_sessions", self.keep_sessions);
        ini.comment(s, "window is how many answers each character remembers, up to sixty four.");
        ini.comment(s, "Too short and one mistake withdraws a character; too long and yesterday");
        ini.comment(s, "decides today. Sixty four answers is a dozen groups, which is the");
        ini.comment(s, "horizon the level decision actually needs.");
        ini.set_u32(s, "window", self.window);
        ini.set_bool(s, "log_sessions", self.log_sessions);
    }
}

// -------------------------------------------------------------- appearance

/// Visual style.
///
/// Held apart from the interface section because the two answer different
/// questions. That one decides how large the window is and which font it uses;
/// this one decides how the same widgets look and how much of the drawing is
/// chrome rather than data.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceSettings {
    pub custom_frame: bool,
    pub caption_height: f32,
    pub focus_ring: bool,
    pub accent_hover: bool,
    pub group_tick: bool,
    pub tab_style: TabStyle,
    pub animate: bool,
    pub anim_ms: f32,
    pub anim_curve: AnimCurve,
    pub hint_scale: f32,
    pub value_column: bool,
    pub numeric_entry: bool,
    pub popup_shade: f32,
    pub group_activity: bool,
    pub keyboard_focus: bool,
    pub splitter_grip: bool,
    pub separator_alpha: f32,

    pub panel_margin: f32,
    pub group_padding: f32,
    pub row_height: f32,
    pub gap: f32,

    /// Background of the scope and the prompt area.
    pub data_background_rgb: u32,
    pub grid_minor_alpha: f32,
    pub grid_major_every: u32,
    pub axis_gutters: bool,
    pub axis_gutter_left: f32,
    pub axis_gutter_bottom: f32,
    pub trace_fill: bool,
    pub trace_fill_alpha: f32,
    pub trace_thickness: f32,

    /// Prompt and score, drawn over the keying picture.
    pub hud: bool,
    /// Share of the picture it occupies.
    ///
    /// The text inside is sized to fill what these leave, so the frame decides
    /// how large the material is read at: there is no separate font size for it,
    /// and a second one could only disagree with the first.
    pub hud_width: f32,
    pub hud_height: f32,
    pub hud_opacity: f32,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings {
            custom_frame: true,
            caption_height: 28.0,
            focus_ring: true,
            accent_hover: false,
            group_tick: false,
            tab_style: TabStyle::Underline,
            animate: true,
            anim_ms: 120.0,
            anim_curve: AnimCurve::EaseOut,
            hint_scale: 0.85,
            value_column: true,
            numeric_entry: true,
            popup_shade: 0.18,
            group_activity: true,
            keyboard_focus: true,
            splitter_grip: true,
            separator_alpha: 0.7,
            panel_margin: 6.0,
            group_padding: 6.0,
            row_height: 22.0,
            gap: 4.0,
            data_background_rgb: 0x0A0A0C,
            grid_minor_alpha: 0.45,
            grid_major_every: 5,
            axis_gutters: true,
            axis_gutter_left: 34.0,
            axis_gutter_bottom: 14.0,
            trace_fill: true,
            trace_fill_alpha: 0.22,
            trace_thickness: 1.0,
            hud: true,
            hud_width: 0.42,
            hud_height: 0.42,
            hud_opacity: 0.94,
        }
    }
}

impl SectionIo for AppearanceSettings {
    const NAME: &'static str = "appearance";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let data_bg = {
            let text = ini.get_string(Self::NAME, "data_background_rgb", "0A0A0C");
            let cleaned = text.trim().trim_start_matches('#').trim_start_matches("0x");
            u32::from_str_radix(cleaned, 16)
                .map(|v| v & 0x00FF_FFFF)
                .unwrap_or_else(|_| {
                    crate::log_warn!("config", "[appearance] data_background_rgb: bad hex '{}'", text);
                    d.data_background_rgb
                })
        };
        AppearanceSettings {
            custom_frame: ini.get_bool(Self::NAME, "custom_frame", d.custom_frame),
            caption_height: ini.get_f32_clamped(Self::NAME, "caption_height", d.caption_height, 18.0, 48.0),
            focus_ring: ini.get_bool(Self::NAME, "focus_ring", d.focus_ring),
            accent_hover: ini.get_bool(Self::NAME, "accent_hover", d.accent_hover),
            group_tick: ini.get_bool(Self::NAME, "group_tick", d.group_tick),
            tab_style: ini.get_enum(Self::NAME, "tab_style", d.tab_style),
            animate: ini.get_bool(Self::NAME, "animate", d.animate),
            anim_ms: ini.get_f32_clamped(Self::NAME, "anim_ms", d.anim_ms, 30.0, 500.0),
            anim_curve: ini.get_enum(Self::NAME, "anim_curve", d.anim_curve),
            hint_scale: ini.get_f32_clamped(Self::NAME, "hint_scale", d.hint_scale, 0.6, 1.0),
            value_column: ini.get_bool(Self::NAME, "value_column", d.value_column),
            numeric_entry: ini.get_bool(Self::NAME, "numeric_entry", d.numeric_entry),
            popup_shade: ini.get_f32_clamped(Self::NAME, "popup_shade", d.popup_shade, 0.0, 0.6),
            group_activity: ini.get_bool(Self::NAME, "group_activity", d.group_activity),
            keyboard_focus: ini.get_bool(Self::NAME, "keyboard_focus", d.keyboard_focus),
            splitter_grip: ini.get_bool(Self::NAME, "splitter_grip", d.splitter_grip),
            separator_alpha: ini.get_f32_clamped(Self::NAME, "separator_alpha", d.separator_alpha, 0.1, 1.0),
            panel_margin: ini.get_f32_clamped(Self::NAME, "panel_margin", d.panel_margin, 0.0, 24.0),
            group_padding: ini.get_f32_clamped(Self::NAME, "group_padding", d.group_padding, 0.0, 20.0),
            row_height: ini.get_f32_clamped(Self::NAME, "row_height", d.row_height, 16.0, 40.0),
            gap: ini.get_f32_clamped(Self::NAME, "gap", d.gap, 0.0, 16.0),
            data_background_rgb: data_bg,
            grid_minor_alpha: ini.get_f32_clamped(Self::NAME, "grid_minor_alpha", d.grid_minor_alpha, 0.0, 1.0),
            grid_major_every: ini.get_u32_clamped(Self::NAME, "grid_major_every", d.grid_major_every, 1, 20),
            axis_gutters: ini.get_bool(Self::NAME, "axis_gutters", d.axis_gutters),
            axis_gutter_left: ini
                .get_f32_clamped(Self::NAME, "axis_gutter_left", d.axis_gutter_left, 0.0, 90.0),
            axis_gutter_bottom: ini
                .get_f32_clamped(Self::NAME, "axis_gutter_bottom", d.axis_gutter_bottom, 0.0, 40.0),
            trace_fill: ini.get_bool(Self::NAME, "trace_fill", d.trace_fill),
            trace_fill_alpha: ini
                .get_f32_clamped(Self::NAME, "trace_fill_alpha", d.trace_fill_alpha, 0.0, 0.8),
            trace_thickness: ini
                .get_f32_clamped(Self::NAME, "trace_thickness", d.trace_thickness, 1.0, 4.0),
            hud: ini.get_bool(Self::NAME, "hud", d.hud),
            hud_width: ini.get_f32_clamped(Self::NAME, "hud_width", d.hud_width, 0.2, 1.0),
            hud_height: ini.get_f32_clamped(Self::NAME, "hud_height", d.hud_height, 0.15, 0.9),
            hud_opacity: ini
                .get_f32_clamped(Self::NAME, "hud_opacity", d.hud_opacity, 0.3, 1.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Visual style. Nothing here changes what is sent, only how much of the");
        ini.comment(s, "drawing is chrome and how it is marked.");
        ini.set_bool(s, "custom_frame", self.custom_frame);
        ini.set_f32(s, "caption_height", self.caption_height);
        ini.set_bool(s, "focus_ring", self.focus_ring);
        ini.comment(s, "accent_hover off keeps the accent for data and focus, which is what");
        ini.comment(s, "makes it mean anything.");
        ini.set_bool(s, "accent_hover", self.accent_hover);
        ini.set_bool(s, "group_tick", self.group_tick);
        ini.set_enum(s, "tab_style", self.tab_style);
        ini.comment(s, "The curve is applied to a phase that moves both ways, so an ease_out");
        ini.comment(s, "reveal is an ease_in dismissal: the first pixels of motion are what");
        ini.comment(s, "say the press registered, and a slow start reads as a stall.");
        ini.set_bool(s, "animate", self.animate);
        ini.set_f32(s, "anim_ms", self.anim_ms);
        ini.set_enum(s, "anim_curve", self.anim_curve);
        ini.set_f32(s, "hint_scale", self.hint_scale);
        ini.set_bool(s, "value_column", self.value_column);
        ini.comment(s, "numeric_entry lets a slider value be typed: double click the number.");
        ini.set_bool(s, "numeric_entry", self.numeric_entry);
        ini.set_f32(s, "popup_shade", self.popup_shade);
        ini.set_bool(s, "group_activity", self.group_activity);
        ini.comment(s, "keyboard_focus lets tab walk the controls. Off returns space to the");
        ini.comment(s, "session transport.");
        ini.set_bool(s, "keyboard_focus", self.keyboard_focus);
        ini.set_bool(s, "splitter_grip", self.splitter_grip);
        ini.set_f32(s, "separator_alpha", self.separator_alpha);
        ini.set_f32(s, "panel_margin", self.panel_margin);
        ini.set_f32(s, "group_padding", self.group_padding);
        ini.set_f32(s, "row_height", self.row_height);
        ini.set_f32(s, "gap", self.gap);
        ini.comment(s, "Data area: the scope and the prompt.");
        ini.set_string(s, "data_background_rgb", &format!("{:06X}", self.data_background_rgb));
        ini.set_f32(s, "grid_minor_alpha", self.grid_minor_alpha);
        ini.set_u32(s, "grid_major_every", self.grid_major_every);
        ini.set_bool(s, "axis_gutters", self.axis_gutters);
        ini.set_f32(s, "axis_gutter_left", self.axis_gutter_left);
        ini.set_f32(s, "axis_gutter_bottom", self.axis_gutter_bottom);
        ini.set_bool(s, "trace_fill", self.trace_fill);
        ini.set_f32(s, "trace_fill_alpha", self.trace_fill_alpha);
        ini.set_f32(s, "trace_thickness", self.trace_thickness);
        ini.comment(s, "hud is the prompt, the answer and the score drawn over the keying");
        ini.comment(s, "picture, where the eyes are during a group. hud_width is the share of");
        ini.comment(s, "the picture it starts from; it widens past that to hold long material.");
        ini.set_bool(s, "hud", self.hud);
        ini.set_f32(s, "hud_width", self.hud_width);
        ini.set_f32(s, "hud_height", self.hud_height);
        ini.set_f32(s, "hud_opacity", self.hud_opacity);
    }
}

// ---------------------------------------------------------------------- ui

#[derive(Debug, Clone)]
pub struct UiSettings {
    pub scale: f32,
    pub font_path: String,
    pub mono_font_path: String,
    pub font_size_pt: f32,
    /// Size of the prompt and the answer.
    ///
    /// Larger than the interface font by default, because it is read at a
    /// glance while the ear is busy and is the one thing on screen that
    /// matters during a session.
    pub prompt_font_size_pt: f32,
    pub text_gamma: f32,
    pub glyph_atlas_size: u32,
    pub accent_rgb: u32,
    pub vsync: bool,
    pub target_fps: u32,
    pub window_x: i32,
    pub window_y: i32,
    pub window_width: u32,
    pub window_height: u32,
    pub maximized: bool,
    pub show_settings_panel: bool,
    pub side_panel_width: f32,
    pub prompt_panel_fraction: f32,
    pub show_debug_overlay: bool,
    pub language: String,
    pub localization_path: String,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            scale: 1.0,
            font_path: String::new(),
            mono_font_path: String::new(),
            font_size_pt: 12.0,
            prompt_font_size_pt: 22.0,
            text_gamma: 1.2,
            glyph_atlas_size: 1024,
            accent_rgb: 0x2F81F7,
            vsync: true,
            target_fps: 0,
            window_x: -1,
            window_y: -1,
            window_width: 1100,
            window_height: 700,
            maximized: false,
            show_settings_panel: true,
            side_panel_width: 320.0,
            prompt_panel_fraction: 0.45,
            show_debug_overlay: false,
            language: String::new(),
            localization_path: "lang".to_string(),
        }
    }
}

impl SectionIo for UiSettings {
    const NAME: &'static str = "ui";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let accent = {
            let text = ini.get_string(Self::NAME, "accent_rgb", "2F81F7");
            let cleaned = text.trim().trim_start_matches('#').trim_start_matches("0x");
            match u32::from_str_radix(cleaned, 16) {
                Ok(v) => v & 0x00FF_FFFF,
                Err(_) => {
                    crate::log_warn!("config", "[ui] accent_rgb: bad hex '{}'", text);
                    d.accent_rgb
                }
            }
        };
        UiSettings {
            scale: ini.get_f32_clamped(Self::NAME, "scale", d.scale, 0.5, 4.0),
            font_path: ini.get_string(Self::NAME, "font_path", &d.font_path),
            mono_font_path: ini.get_string(Self::NAME, "mono_font_path", &d.mono_font_path),
            font_size_pt: ini.get_f32_clamped(Self::NAME, "font_size_pt", d.font_size_pt, 6.0, 48.0),
            prompt_font_size_pt: ini
                .get_f32_clamped(Self::NAME, "prompt_font_size_pt", d.prompt_font_size_pt, 8.0, 96.0),
            text_gamma: ini.get_f32_clamped(Self::NAME, "text_gamma", d.text_gamma, 0.5, 3.0),
            glyph_atlas_size: ini
                .get_u32_clamped(Self::NAME, "glyph_atlas_size", d.glyph_atlas_size, 256, 8192),
            accent_rgb: accent,
            vsync: ini.get_bool(Self::NAME, "vsync", d.vsync),
            target_fps: ini.get_u32_clamped(Self::NAME, "target_fps", d.target_fps, 0, 480),
            window_x: ini.get_i32(Self::NAME, "window_x", d.window_x),
            window_y: ini.get_i32(Self::NAME, "window_y", d.window_y),
            window_width: ini.get_u32_clamped(Self::NAME, "window_width", d.window_width, 640, 16384),
            window_height: ini.get_u32_clamped(Self::NAME, "window_height", d.window_height, 400, 16384),
            maximized: ini.get_bool(Self::NAME, "maximized", d.maximized),
            show_settings_panel: ini.get_bool(Self::NAME, "show_settings_panel", d.show_settings_panel),
            side_panel_width: ini
                .get_f32_clamped(Self::NAME, "side_panel_width", d.side_panel_width, 180.0, 900.0),
            prompt_panel_fraction: ini
                .get_f32_clamped(Self::NAME, "prompt_panel_fraction", d.prompt_panel_fraction, 0.1, 0.9),
            show_debug_overlay: ini.get_bool(Self::NAME, "show_debug_overlay", d.show_debug_overlay),
            language: ini.get_string(Self::NAME, "language", &d.language),
            localization_path: ini.get_string(Self::NAME, "localization_path", &d.localization_path),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "UI scale multiplies the system DPI factor.");
        ini.set_f32(s, "scale", self.scale);
        ini.comment(s, "Font files are TTF, parsed internally. Empty falls back to a system");
        ini.comment(s, "font search under the Windows Fonts directory.");
        ini.set_string(s, "font_path", &self.font_path);
        ini.set_string(s, "mono_font_path", &self.mono_font_path);
        ini.set_f32(s, "font_size_pt", self.font_size_pt);
        ini.comment(s, "prompt_font_size_pt is the material and the answer, read at a glance");
        ini.comment(s, "while the ear is busy. Larger than the interface font on purpose.");
        ini.set_f32(s, "prompt_font_size_pt", self.prompt_font_size_pt);
        ini.set_f32(s, "text_gamma", self.text_gamma);
        ini.set_u32(s, "glyph_atlas_size", self.glyph_atlas_size);
        ini.set_string(s, "accent_rgb", &format!("{:06X}", self.accent_rgb));
        ini.set_bool(s, "vsync", self.vsync);
        ini.comment(s, "target_fps 0 disables the limiter.");
        ini.set_u32(s, "target_fps", self.target_fps);
        ini.comment(s, "Window geometry, -1 lets Windows place the window.");
        ini.set_i32(s, "window_x", self.window_x);
        ini.set_i32(s, "window_y", self.window_y);
        ini.set_u32(s, "window_width", self.window_width);
        ini.set_u32(s, "window_height", self.window_height);
        ini.set_bool(s, "maximized", self.maximized);
        ini.set_bool(s, "show_settings_panel", self.show_settings_panel);
        ini.set_f32(s, "side_panel_width", self.side_panel_width);
        ini.set_f32(s, "prompt_panel_fraction", self.prompt_panel_fraction);
        ini.set_bool(s, "show_debug_overlay", self.show_debug_overlay);
        ini.comment(s, "language selects a file <code>.lang from localization_path.");
        ini.comment(s, "Empty or en uses the wording built into the executable.");
        ini.set_string(s, "language", &self.language);
        ini.set_string(s, "localization_path", &self.localization_path);
    }
}

// ------------------------------------------------------------------ render

#[derive(Debug, Clone)]
pub struct RenderSettings {
    pub validation: bool,
    pub device_name: String,
    pub device_index: i32,
    pub frames_in_flight: u32,
    pub swapchain_images: u32,
    pub present_mode: PresentModeCfg,
    pub background_rgb: u32,
    pub vertex_buffer_kb: u32,
    pub index_buffer_kb: u32,
    pub max_textures: u32,
    pub log_device_info: bool,
    pub gpu_timing: bool,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings {
            validation: cfg!(debug_assertions),
            device_name: String::new(),
            device_index: -1,
            frames_in_flight: 2,
            swapchain_images: 0,
            present_mode: PresentModeCfg::Auto,
            background_rgb: 0x1E1E1E,
            vertex_buffer_kb: 256,
            index_buffer_kb: 128,
            max_textures: 32,
            log_device_info: true,
            gpu_timing: true,
        }
    }
}

impl SectionIo for RenderSettings {
    const NAME: &'static str = "render";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let background = {
            let text = ini.get_string(Self::NAME, "background_rgb", "1E1E1E");
            let cleaned = text.trim().trim_start_matches('#').trim_start_matches("0x");
            u32::from_str_radix(cleaned, 16).map(|v| v & 0x00FF_FFFF).unwrap_or_else(|_| {
                crate::log_warn!("config", "[render] background_rgb: bad hex '{}'", text);
                d.background_rgb
            })
        };
        RenderSettings {
            validation: ini.get_bool(Self::NAME, "validation", d.validation),
            device_name: ini.get_string(Self::NAME, "device_name", &d.device_name),
            device_index: ini.get_i32(Self::NAME, "device_index", d.device_index),
            frames_in_flight: ini
                .get_u32_clamped(Self::NAME, "frames_in_flight", d.frames_in_flight, 1, 4),
            swapchain_images: ini
                .get_u32_clamped(Self::NAME, "swapchain_images", d.swapchain_images, 0, 8),
            present_mode: ini.get_enum(Self::NAME, "present_mode", d.present_mode),
            background_rgb: background,
            vertex_buffer_kb: ini
                .get_u32_clamped(Self::NAME, "vertex_buffer_kb", d.vertex_buffer_kb, 64, 65536),
            index_buffer_kb: ini
                .get_u32_clamped(Self::NAME, "index_buffer_kb", d.index_buffer_kb, 32, 65536),
            max_textures: ini.get_u32_clamped(Self::NAME, "max_textures", d.max_textures, 4, 1024),
            log_device_info: ini.get_bool(Self::NAME, "log_device_info", d.log_device_info),
            gpu_timing: ini.get_bool(Self::NAME, "gpu_timing", d.gpu_timing),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "validation requires the Vulkan SDK layers, it is off in release.");
        ini.set_bool(s, "validation", self.validation);
        ini.set_string(s, "device_name", &self.device_name);
        ini.set_i32(s, "device_index", self.device_index);
        ini.set_u32(s, "frames_in_flight", self.frames_in_flight);
        ini.set_u32(s, "swapchain_images", self.swapchain_images);
        ini.comment(s, "present_mode: auto follows ui.vsync.");
        ini.set_enum(s, "present_mode", self.present_mode);
        ini.set_string(s, "background_rgb", &format!("{:06X}", self.background_rgb));
        ini.set_u32(s, "vertex_buffer_kb", self.vertex_buffer_kb);
        ini.set_u32(s, "index_buffer_kb", self.index_buffer_kb);
        ini.set_u32(s, "max_textures", self.max_textures);
        ini.set_bool(s, "log_device_info", self.log_device_info);
        ini.set_bool(s, "gpu_timing", self.gpu_timing);
    }
}

// --------------------------------------------------------------------- log

#[derive(Debug, Clone)]
pub struct LogSettings {
    pub level: Level,
    pub file_path: String,
    pub to_debugger: bool,
    pub max_size_kb: u32,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            level: Level::Info,
            file_path: "logs/cwdrill.log".to_string(),
            to_debugger: true,
            max_size_kb: 2048,
        }
    }
}

impl SectionIo for LogSettings {
    const NAME: &'static str = "log";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        LogSettings {
            level: ini
                .get_enum(Self::NAME, "level", LogLevelCfg::from_level(d.level))
                .to_level(),
            file_path: ini.get_string(Self::NAME, "file_path", &d.file_path),
            to_debugger: ini.get_bool(Self::NAME, "to_debugger", d.to_debugger),
            max_size_kb: ini.get_u32(Self::NAME, "max_size_kb", d.max_size_kb),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "level: trace, debug, info, warn, error, off.");
        ini.set_enum(s, "level", LogLevelCfg::from_level(self.level));
        ini.comment(s, "file_path empty disables the file sink.");
        ini.set_string(s, "file_path", &self.file_path);
        ini.set_bool(s, "to_debugger", self.to_debugger);
        ini.set_u32(s, "max_size_kb", self.max_size_kb);
    }
}

impl LogLevelCfg {
    pub fn to_level(self) -> Level {
        match self {
            LogLevelCfg::Trace => Level::Trace,
            LogLevelCfg::Debug => Level::Debug,
            LogLevelCfg::Info => Level::Info,
            LogLevelCfg::Warn => Level::Warn,
            LogLevelCfg::Error => Level::Error,
            LogLevelCfg::Off => Level::Off,
        }
    }
    pub fn from_level(l: Level) -> Self {
        match l {
            Level::Trace => LogLevelCfg::Trace,
            Level::Debug => LogLevelCfg::Debug,
            Level::Info => LogLevelCfg::Info,
            Level::Warn => LogLevelCfg::Warn,
            Level::Error => LogLevelCfg::Error,
            Level::Off => LogLevelCfg::Off,
        }
    }
}

// ---------------------------------------------------------------- settings

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub tone: ToneSettings,
    pub timing: TimingSettings,
    pub lesson: LessonSettings,
    pub material: MaterialSettings,
    pub practice: PracticeSettings,
    pub paddle: PaddleSettings,
    pub conditions: ConditionsSettings,
    pub audio: AudioSettings,
    pub scope: ScopeSettings,
    pub progress: ProgressSettings,
    pub panel: PanelSettings,
    pub appearance: AppearanceSettings,
    pub ui: UiSettings,
    pub render: RenderSettings,
    pub log: LogSettings,
}

impl Settings {
    /// Config lives next to the executable so portable installs work.
    pub fn default_path() -> PathBuf {
        match std::env::current_exe() {
            Ok(exe) => exe.with_file_name("cwdrill.ini"),
            Err(_) => PathBuf::from("cwdrill.ini"),
        }
    }

    pub fn load(path: &Path) -> Result<Settings> {
        if !path.exists() {
            crate::log_info!("config", "{} not found, writing defaults", path.display());
            let s = Settings::default();
            s.save(path)?;
            return Ok(s);
        }
        let ini = Ini::load(path)?;
        Ok(Settings::from_ini(&ini))
    }

    pub fn from_ini(ini: &Ini) -> Settings {
        Settings {
            tone: ToneSettings::load(ini),
            timing: TimingSettings::load(ini),
            lesson: LessonSettings::load(ini),
            material: MaterialSettings::load(ini),
            practice: PracticeSettings::load(ini),
            paddle: PaddleSettings::load(ini),
            conditions: ConditionsSettings::load(ini),
            audio: AudioSettings::load(ini),
            scope: ScopeSettings::load(ini),
            progress: ProgressSettings::load(ini),
            panel: PanelSettings::load(ini),
            appearance: AppearanceSettings::load(ini),
            ui: UiSettings::load(ini),
            render: RenderSettings::load(ini),
            log: LogSettings::load(ini),
        }
    }

    /// Serializes into a fresh document. Comments come from the store
    /// implementations, so the written file is self documenting.
    pub fn to_ini(&self) -> Ini {
        let mut ini = Ini::new();
        ini.comment("", "CWDrill configuration");
        ini.comment("", "Generated automatically, edited values are preserved on restart.");
        ini.blank("");
        self.tone.store(&mut ini);
        self.timing.store(&mut ini);
        self.lesson.store(&mut ini);
        self.material.store(&mut ini);
        self.practice.store(&mut ini);
        self.paddle.store(&mut ini);
        self.conditions.store(&mut ini);
        self.audio.store(&mut ini);
        self.scope.store(&mut ini);
        self.progress.store(&mut ini);
        self.panel.store(&mut ini);
        self.appearance.store(&mut ini);
        self.ui.store(&mut ini);
        self.render.store(&mut ini);
        self.log.store(&mut ini);
        ini
    }

    /// Directory the translation files are read from.
    ///
    /// Resolved against the directory of the configuration file rather than the
    /// working directory, so a shortcut started from anywhere finds the same
    /// files as a double click on the executable.
    pub fn language_dir(&self) -> PathBuf {
        let base = Settings::default_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_default();
        base.join(&self.ui.localization_path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.to_ini().save(path)
    }
}