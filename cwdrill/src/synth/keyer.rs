//! Text to keying elements.
//!
//! ## What an element is
//!
//! A duration and whether the tone is on. Everything about the timing is decided
//! here, on the interface thread, and the audio thread only renders what it is
//! handed; the alternative would put the jitter generator and the gap policy on
//! a thread that must not stall, in exchange for nothing.
//!
//! Each element carries the duration it was given and the duration it should
//! have had. The difference is the whole of the jitter and the swing, and
//! publishing both is what lets the keying picture mark where the edge would
//! have been. Without it, a picture under jitter shows an element that is
//! plainly not three units long and says nothing about how far off it was.
//!
//! ## Why the gaps are computed rather than stated
//!
//! With the two speeds apart, the gaps are not a setting: they follow from the
//! requirement that the text occupy the time the text speed asks for. The
//! reference word is fifty units, of which thirty one are elements and nineteen
//! are gaps, so the gap time per word is the required word duration less the
//! time the elements take. That total is then split in the ratio the standard
//! gaps already have, three parts to a character and seven to a word.
//!
//! It follows that the stated gap settings do nothing while the two speeds are
//! apart. They are not silently overridden: the interface refuses them, because
//! a control that is read only in one arrangement and ignored in the other is a
//! control that can only be misread.

use crate::config::settings::TimingSettings;
use crate::core::Rng;
use crate::morse;

/// One keyed interval.
#[derive(Debug, Clone, Copy, Default)]
pub struct Element {
    pub on: bool,
    /// Duration as it will be sent.
    pub seconds: f32,
    /// Duration with no jitter and no swing.
    pub ideal_seconds: f32,
    /// True on the first element of a character.
    ///
    /// The point the ideal timeline is measured from, because that is where the
    /// ear resynchronizes: an operator does not accumulate the error of the
    /// previous character, they hear the next one as a fresh shape.
    pub sync: bool,
}

/// Which gap follows the token just emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Gap {
    None,
    Character,
    Word,
}

pub struct Keyer {
    rng: Rng,
}

impl Keyer {
    pub fn new() -> Keyer {
        Keyer { rng: Rng::from_clock() }
    }

    pub fn with_seed(seed: u64) -> Keyer {
        Keyer { rng: Rng::new(seed) }
    }

    /// Appends the elements one text produces.
    ///
    /// Returns the text that was actually keyed. A character the table does not
    /// hold is dropped, and returning what survived is what keeps a transcript
    /// aligned with what is heard: a prompt that showed the input rather than
    /// the output would claim a character the student never had a chance at.
    ///
    /// A prosign is written between angle brackets and is one character for
    /// every purpose here: no gap inside it, one synchronization point, one
    /// token in the transcript.
    pub fn encode(
        &mut self,
        text: &str,
        timing: &TimingSettings,
        out: &mut Vec<Element>,
    ) -> String {
        let dot = timing.dot_seconds();
        let dash = dot * timing.weight;
        let (element_gap, char_gap, word_gap) = timing.gaps();

        let mut kept = String::with_capacity(text.len());
        let mut gap = Gap::None;
        let mut chars = text.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch.is_whitespace() {
                // Several spaces in a row are one word gap. A text that arrived
                // with a line break in it would otherwise pause for seconds.
                gap = Gap::Word;
                continue;
            }

            // A prosign is read as a token so the elements inside it are not
            // separated by character gaps.
            let (pattern, token) = if ch == '<' {
                let mut name = String::with_capacity(4);
                let mut closed = false;
                while let Some(&next) = chars.peek() {
                    chars.next();
                    if next == '>' {
                        closed = true;
                        break;
                    }
                    name.push(next);
                }
                if !closed {
                    continue;
                }
                match morse::prosign(&name) {
                    Some(p) => (p, format!("<{}>", name.to_ascii_uppercase())),
                    None => continue,
                }
            } else {
                match morse::pattern_of(ch) {
                    Some(p) => (p, ch.to_ascii_uppercase().to_string()),
                    None => continue,
                }
            };

            match gap {
                Gap::Character => self.push_off(out, char_gap, timing),
                Gap::Word => {
                    self.push_off(out, word_gap, timing);
                    kept.push(' ');
                }
                Gap::None => {}
            }
            gap = Gap::Character;

            for (index, symbol) in pattern.chars().enumerate() {
                if index > 0 {
                    self.push_off(out, element_gap, timing);
                }
                let ideal = if symbol == '-' { dash } else { dot };
                // Swing shortens the first of a pair and lengthens the second,
                // so the character keeps its length and only its internal
                // proportion moves. That is a bias rather than a spread, and the
                // ear learns the two separately.
                let bias = if index % 2 == 0 {
                    1.0 - timing.swing_percent * 0.01
                } else {
                    1.0 + timing.swing_percent * 0.01
                };
                out.push(Element {
                    on: true,
                    seconds: self.jitter(ideal * bias, timing),
                    ideal_seconds: ideal,
                    sync: index == 0,
                });
            }

            kept.push_str(&token);
        }

        kept
    }

    fn push_off(&mut self, out: &mut Vec<Element>, seconds: f32, timing: &TimingSettings) {
        out.push(Element {
            on: false,
            seconds: self.jitter(seconds, timing),
            ideal_seconds: seconds,
            sync: false,
        });
    }

    /// Perturbs one duration.
    ///
    /// Uniform rather than normal, and stated as such. What the ear is being
    /// trained against is the presence of variation rather than its
    /// distribution, and a generator with a tail would occasionally produce an
    /// element far enough out to be a different element.
    ///
    /// Bounded below so a large setting cannot produce a duration of nought,
    /// which the envelope would render as a click with no tone.
    #[inline]
    fn jitter(&mut self, seconds: f32, timing: &TimingSettings) -> f32 {
        let spread = timing.jitter_percent * 0.01;
        if spread <= 0.0 {
            return seconds;
        }
        let factor = 1.0 + self.rng.symmetric() * spread;
        (seconds * factor).max(seconds * 0.25)
    }
}

impl Default for Keyer {
    fn default() -> Keyer {
        Keyer::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seconds a sequence occupies.
    ///
    /// A test helper rather than an interface: the generator consumes elements
    /// one at a time and never needs a total, so publishing one would be
    /// publishing an answer nobody asks for.
    fn duration(elements: &[Element]) -> f32 {
        elements.iter().map(|e| e.seconds).sum()
    }

    fn plain() -> TimingSettings {
        TimingSettings {
            char_wpm: 20.0,
            text_wpm: 20.0,
            farnsworth: false,
            weight: 3.0,
            element_gap: 1.0,
            char_gap: 3.0,
            word_gap: 7.0,
            jitter_percent: 0.0,
            swing_percent: 0.0,
        }
    }

    #[test]
    fn a_character_is_its_pattern_with_gaps_between() {
        let mut keyer = Keyer::with_seed(1);
        let t = plain();
        let mut out = Vec::new();
        let kept = keyer.encode("A", &t, &mut out);

        assert_eq!(kept, "A");
        // A dot, a gap, a dash: three elements and no trailing gap, because a
        // gap belongs before the next character rather than after this one.
        assert_eq!(out.len(), 3);
        assert!(out[0].on && out[0].sync);
        assert!(!out[1].on);
        assert!(out[2].on && !out[2].sync);

        let dot = t.dot_seconds();
        assert!((out[0].seconds - dot).abs() < 1e-6);
        assert!((out[1].seconds - dot).abs() < 1e-6);
        assert!((out[2].seconds - dot * 3.0).abs() < 1e-6);
    }

    #[test]
    fn the_reference_word_takes_the_time_the_speed_states() {
        // The one arithmetic check that matters: PARIS at twenty words a minute
        // is three seconds, gaps included. A weight or a gap factor that is off
        // shows up here rather than as a vague sense that the speed is wrong.
        let mut keyer = Keyer::with_seed(2);
        let t = plain();
        let mut out = Vec::new();
        keyer.encode("PARIS ", &t, &mut out);
        // The trailing space produces no element of its own, so the word gap is
        // added by hand to complete the reference.
        let total = duration(&out) + t.dot_seconds() * 7.0;
        assert!((total - 3.0).abs() < 0.01, "the word took {:.3} s", total);
    }

    #[test]
    fn stretching_the_gaps_leaves_the_elements_alone() {
        // The whole point of the arrangement. A character has to arrive at its
        // own speed from the first lesson, or the speed has to be unlearned when
        // the gaps close.
        let mut keyer = Keyer::with_seed(3);
        let mut fast = plain();
        let mut slow = plain();
        slow.text_wpm = 10.0;
        slow.farnsworth = true;
        fast.text_wpm = 20.0;

        let mut a = Vec::new();
        let mut b = Vec::new();
        keyer.encode("K", &fast, &mut a);
        keyer.encode("K", &slow, &mut b);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((x.seconds - y.seconds).abs() < 1e-6, "an element changed length");
        }
    }

    #[test]
    fn the_stretched_word_takes_the_text_speed() {
        let mut keyer = Keyer::with_seed(4);
        let mut t = plain();
        t.char_wpm = 20.0;
        t.text_wpm = 12.0;
        t.farnsworth = true;

        let mut out = Vec::new();
        keyer.encode("PARIS ", &t, &mut out);
        let (_, _, word) = t.gaps();
        let total = duration(&out) + word;
        // Five seconds is one word at twelve words a minute.
        assert!((total - 5.0).abs() < 0.02, "the word took {:.3} s", total);
    }

    #[test]
    fn a_word_gap_replaces_the_character_gap_rather_than_adding_to_it() {
        let mut keyer = Keyer::with_seed(5);
        let t = plain();
        let mut out = Vec::new();
        keyer.encode("E E", &t, &mut out);
        // A mark, one gap, a mark. Two gaps would mean the word gap was appended
        // to a character gap and every word would be ten units apart.
        assert_eq!(out.len(), 3);
        assert!((out[1].seconds - t.dot_seconds() * 7.0).abs() < 1e-6);
    }

    #[test]
    fn several_spaces_are_one_gap() {
        let mut keyer = Keyer::with_seed(6);
        let t = plain();
        let mut out = Vec::new();
        let kept = keyer.encode("E   E", &t, &mut out);
        assert_eq!(kept, "E E");
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_prosign_is_one_character() {
        let mut keyer = Keyer::with_seed(7);
        let t = plain();
        let mut out = Vec::new();
        let kept = keyer.encode("<SK>", &t, &mut out);
        assert_eq!(kept, "<SK>");
        // Six elements and five gaps, with the synchronization on the first and
        // nowhere else: the ear hears one shape rather than two characters.
        assert_eq!(out.iter().filter(|e| e.on).count(), 6);
        assert_eq!(out.iter().filter(|e| e.sync).count(), 1);
    }

    #[test]
    fn an_unknown_character_is_dropped_from_both() {
        // The transcript has to match what was heard, or the student is marked
        // against a character that was never sent.
        let mut keyer = Keyer::with_seed(8);
        let t = plain();
        let mut out = Vec::new();
        let kept = keyer.encode("A\u{263A}B", &t, &mut out);
        assert_eq!(kept, "AB");
    }

    #[test]
    fn jitter_moves_the_duration_and_not_the_ideal() {
        // Two runs of the same text rather than one against a constant: the
        // marks are one unit and the gaps between characters are three, so a
        // single expected duration is wrong for two thirds of the sequence.
        // What the ideal has to satisfy is that it is the same in both runs,
        // which is the whole claim the field makes.
        let mut plainer = Keyer::with_seed(9);
        let mut jittered = Keyer::with_seed(9);
        let clean = plain();
        let mut noisy = plain();
        noisy.jitter_percent = 20.0;

        let mut a = Vec::new();
        let mut b = Vec::new();
        plainer.encode("HHHHH", &clean, &mut a);
        jittered.encode("HHHHH", &noisy, &mut b);
        assert_eq!(a.len(), b.len(), "the two runs produced different sequences");

        let mut moved = 0;
        for (index, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                (x.ideal_seconds - y.ideal_seconds).abs() < 1e-6,
                "the ideal moved at element {}",
                index
            );
            // At nought the two are the same number, which is what says the
            // perturbation is applied and not merely recorded.
            assert!((x.seconds - x.ideal_seconds).abs() < 1e-6);

            if (y.seconds - y.ideal_seconds).abs() > y.ideal_seconds * 0.01 {
                moved += 1;
            }
            // The floor is what keeps a large setting from producing an element
            // of nought, which renders as a click with no tone.
            assert!(y.seconds > y.ideal_seconds * 0.2);
        }
        assert!(moved > b.len() / 2, "hardly anything was perturbed");
    }

    #[test]
    fn swing_keeps_the_character_length() {
        // A bias that shortened without lengthening would be a speed change
        // dressed up as a bias, and the ear would learn the wrong thing.
        let mut keyer = Keyer::with_seed(10);
        let mut plainer = plain();
        let mut swung = plain();
        swung.swing_percent = 20.0;

        let mut a = Vec::new();
        let mut b = Vec::new();
        plainer.jitter_percent = 0.0;
        keyer.encode("H", &plainer, &mut a);
        keyer.encode("H", &swung, &mut b);

        let sum_a: f32 = a.iter().filter(|e| e.on).map(|e| e.seconds).sum();
        let sum_b: f32 = b.iter().filter(|e| e.on).map(|e| e.seconds).sum();
        assert!((sum_a - sum_b).abs() < 1e-5, "the character changed length");
        // And the first element really did shorten.
        assert!(b[0].seconds < a[0].seconds * 0.9);
    }
}