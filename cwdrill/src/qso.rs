//! Structured exchange.
//!
//! ## Why an exchange is not a group of characters
//!
//! A group is five characters and every one of them carries the same weight. An
//! exchange is a callsign, a report and a number wrapped in wording that carries
//! no information at all: the letters of `CQ`, the `DE`, the `TU` at the front
//! of a contest reply. Scored per character, a student who copied the callsign
//! and the serial perfectly would still be marked down for missing a `K` at the
//! end, and one who missed the callsign entirely would keep most of their score
//! because the wording around it is long.
//!
//! So the exchange is generated as fields, and only the fields that carry
//! something are asked for. The rest is sent, heard, and never scored. That is
//! also what a real contact is judged on: an operator who logs the wrong serial
//! has made an error and one who did not hear the `TU` has not.
//!
//! ## Why the whole alphabet
//!
//! For the same reason a callsign uses it. A callsign made of the two characters
//! a beginner has met is not a callsign, and a name spelled out of them is not a
//! name. The source is chosen deliberately, and choosing it is the statement that
//! the student is ready for the whole set.

use crate::core::Rng;

/// What one field of an exchange carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Wording that carries nothing: sent, heard, never asked for.
    Filler,
    Call,
    /// Readability, strength and tone.
    Rst,
    /// Contest serial.
    Serial,
    Name,
    Qth,
}

impl FieldKind {
    /// True when the student is expected to write it down.
    pub fn scored(self) -> bool {
        self != FieldKind::Filler
    }

    /// Localization key of the label the interface shows.
    pub fn key(self) -> &'static str {
        match self {
            FieldKind::Filler => "field.qso.filler",
            FieldKind::Call => "field.qso.call",
            FieldKind::Rst => "field.qso.rst",
            FieldKind::Serial => "field.qso.serial",
            FieldKind::Name => "field.qso.name",
            FieldKind::Qth => "field.qso.qth",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub text: String,
    pub kind: FieldKind,
    /// True when this token repeats one already sent.
    ///
    /// A contest operator sends the serial twice, and the student writes it once.
    /// So the repetition is part of the material and not part of the answer, and
    /// the second copy is filler that happens to look like the first.
    pub echo: bool,
}

impl Field {
    fn plain(text: &str) -> Field {
        Field { text: text.to_string(), kind: FieldKind::Filler, echo: false }
    }

    fn of(kind: FieldKind, text: String) -> Field {
        Field { text, kind, echo: false }
    }

    fn again(&self) -> Field {
        Field { text: self.text.clone(), kind: self.kind, echo: true }
    }

    /// True when this field is one of the answers.
    pub fn wanted(&self) -> bool {
        self.kind.scored() && !self.echo
    }
}

/// One transmission, as the student hears it.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub fields: Vec<Field>,
}

impl Exchange {
    /// The whole thing, ready for the keyer.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(64);
        for (index, field) in self.fields.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(&field.text);
        }
        out
    }

    /// The fields the student is expected to write down, in order.
    pub fn answers(&self) -> Vec<&Field> {
        self.fields.iter().filter(|f| f.wanted()).collect()
    }

    /// Field index of every character of the text, spaces excluded.
    ///
    /// The index refers to the answer list rather than to the field list, and is
    /// nothing for a character that carries no answer. Built here rather than by
    /// the session because it follows from the composition, and reconstructing it
    /// by matching text would go wrong the moment two fields held the same value.
    pub fn character_fields(&self) -> Vec<Option<usize>> {
        let mut out = Vec::with_capacity(64);
        let mut answer = 0usize;
        for field in &self.fields {
            let index = if field.wanted() {
                let current = answer;
                answer += 1;
                Some(current)
            } else {
                None
            };
            for ch in field.text.chars() {
                if ch != ' ' {
                    out.push(index);
                }
            }
        }
        out
    }
}

/// Shapes an exchange is drawn from.
///
/// Three, and each is a different thing to copy. The call is one field under no
/// time pressure. The contest exchange is the one an operator meets a thousand
/// times in a weekend and has to copy at speed. The ragchew fragment is where
/// the fields are words rather than groups, which the ear treats differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Call,
    Contest,
    Ragchew,
}

/// Names an exchange draws from.
///
/// Short and from several languages, because a name is a word the ear has to
/// take whole and a list of five English ones would be five shapes memorized
/// rather than a skill.
const NAMES: &[&str] = &[
    "JOHN", "MIKE", "ANNA", "PETE", "OLEG", "HANS", "LUIS", "KEN", "TOM", "IVAN",
    "ERIK", "PAUL", "YURI", "JOSE", "NILS",
];

const PLACES: &[&str] = &[
    "MOSCOW", "BERLIN", "TOKYO", "PARIS", "RIGA", "OSLO", "LIMA", "DELHI",
    "CAIRO", "PERTH", "BOSTON", "MILAN", "KIEV", "SOFIA", "DUBLIN",
];

/// Readability and strength an exchange reports.
///
/// A real report varies and a contest report does not, which is why the two
/// shapes treat it differently: the contest one sends the conventional token as
/// wording, and the conversational one sends a figure worth copying.
const REPORTS: &[&str] = &["599", "579", "559", "469", "339", "588"];

pub struct Generator {
    rng: Rng,
}

impl Generator {
    pub fn new() -> Generator {
        Generator { rng: Rng::from_clock() }
    }

    pub fn with_seed(seed: u64) -> Generator {
        Generator { rng: Rng::new(seed) }
    }

    pub fn next(&mut self) -> Exchange {
        // Weighted towards the contest exchange, because it is the one an
        // operator meets most and the one speed matters in.
        let roll = self.rng.below(10);
        let shape = if roll < 3 {
            Shape::Call
        } else if roll < 8 {
            Shape::Contest
        } else {
            Shape::Ragchew
        };
        match shape {
            Shape::Call => self.call_shape(),
            Shape::Contest => self.contest_shape(),
            Shape::Ragchew => self.ragchew_shape(),
        }
    }

    /// A station calling, which is one field and a great deal of wording.
    fn call_shape(&mut self) -> Exchange {
        let call = Field::of(FieldKind::Call, self.callsign());
        let mut fields = vec![Field::plain("CQ"), Field::plain("CQ"), Field::plain("DE")];
        fields.push(call.clone());
        // Sent twice, which is what a caller does and what the student has to
        // learn not to write down twice.
        fields.push(call.again());
        fields.push(Field::plain("K"));
        Exchange { fields }
    }

    /// A contest reply: who, the conventional report, and the number.
    fn contest_shape(&mut self) -> Exchange {
        let call = Field::of(FieldKind::Call, self.callsign());
        let serial = Field::of(FieldKind::Serial, format!("{:03}", self.rng.between(1, 999)));

        let mut fields = Vec::with_capacity(6);
        if self.rng.chance(0.5) {
            fields.push(Field::plain("TU"));
        }
        fields.push(call);
        // The conventional token rather than a figure. It is the same in every
        // contest contact ever made, so asking for it would be asking the student
        // to copy a constant.
        fields.push(Field::plain("5NN"));
        fields.push(serial.clone());
        if self.rng.chance(0.6) {
            fields.push(serial.again());
        }
        Exchange { fields }
    }

    /// A conversational opening, where the fields are words.
    fn ragchew_shape(&mut self) -> Exchange {
        let call = Field::of(FieldKind::Call, self.callsign());
        let rst = Field::of(
            FieldKind::Rst,
            REPORTS[self.rng.below(REPORTS.len())].to_string(),
        );
        let name = Field::of(FieldKind::Name, NAMES[self.rng.below(NAMES.len())].to_string());
        let qth = Field::of(FieldKind::Qth, PLACES[self.rng.below(PLACES.len())].to_string());

        let mut fields = Vec::with_capacity(12);
        fields.push(Field::plain("DE"));
        fields.push(call);
        fields.push(Field::plain("<BT>"));
        fields.push(Field::plain("UR"));
        fields.push(Field::plain("RST"));
        fields.push(rst.clone());
        if self.rng.chance(0.5) {
            fields.push(rst.again());
        }
        fields.push(Field::plain("<BT>"));
        fields.push(Field::plain("NAME"));
        fields.push(name);
        fields.push(Field::plain("<BT>"));
        fields.push(Field::plain("QTH"));
        fields.push(qth);
        fields.push(Field::plain("K"));
        Exchange { fields }
    }

    /// A callsign, using the whole alphabet.
    ///
    /// The same shape the callsign source produces, and duplicated here rather
    /// than shared because the two are drawn from different generators: an
    /// exchange that reached into the material generator would advance its
    /// sequence and a seeded session would stop replaying.
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
}

impl Default for Generator {
    fn default() -> Generator {
        Generator::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_exchange_asks_for_something() {
        // An exchange with no scored field would be a group of characters with
        // extra syllables, which is the arrangement this module exists to avoid.
        let mut g = Generator::with_seed(1);
        for _ in 0..200 {
            let exchange = g.next();
            assert!(!exchange.answers().is_empty(), "{}", exchange.text());
        }
    }

    #[test]
    fn a_repeated_token_is_asked_for_once() {
        // The whole reason the echo flag exists. A caller sends the serial twice
        // and the student writes it once, so a second answer slot would mark a
        // correct copy as one field short.
        let mut g = Generator::with_seed(2);
        for _ in 0..200 {
            let exchange = g.next();
            let answers = exchange.answers();
            for pair in answers.windows(2) {
                if pair[0].kind == pair[1].kind {
                    panic!("two answers of one kind in {}", exchange.text());
                }
            }
        }
    }

    #[test]
    fn the_character_map_covers_the_text_exactly() {
        // The map is what colours the prompt, so a map one character short would
        // shift every verdict after the field it belongs to.
        let mut g = Generator::with_seed(3);
        for _ in 0..200 {
            let exchange = g.next();
            let text = exchange.text();
            let marks = exchange.character_fields();
            let counted = text.chars().filter(|&c| c != ' ').count();
            assert_eq!(marks.len(), counted, "{}", text);
        }
    }

    #[test]
    fn the_map_agrees_with_the_answers() {
        let mut g = Generator::with_seed(4);
        for _ in 0..200 {
            let exchange = g.next();
            let answers = exchange.answers();
            let marks = exchange.character_fields();
            for index in 0..answers.len() {
                let counted = marks.iter().filter(|&&m| m == Some(index)).count();
                let expected = answers[index].text.chars().filter(|&c| c != ' ').count();
                assert_eq!(counted, expected, "field {} of {}", index, exchange.text());
            }
        }
    }

    #[test]
    fn the_wording_carries_no_answer() {
        // The claim the module makes: a student is not marked on the punctuation
        // of a contact.
        let mut g = Generator::with_seed(5);
        for _ in 0..200 {
            let exchange = g.next();
            for field in &exchange.fields {
                if field.text == "CQ" || field.text == "DE" || field.text == "K" {
                    assert!(!field.wanted(), "{} is asked for", field.text);
                }
            }
        }
    }
}