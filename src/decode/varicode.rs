//! Variable length alphabet of PSK31.
//!
//! ## Why the codes need no length beside them
//!
//! Two properties settle every framing question the decoder would otherwise
//! have to ask. No code contains two consecutive noughts, and every code begins
//! and ends with a one. So a pair of noughts is a character boundary and cannot
//! be anything else, and the boundary needs no counter, no length field and no
//! resynchronization: a decoder that joins a transmission halfway through is in
//! step by the end of the first character.
//!
//! The leading one has a second consequence that the lookup rests on. A code
//! read as an integer determines its own length, because the position of the
//! highest set bit is the length. So the accumulated bits are a unique key on
//! their own and the table needs no length column: two codes of different
//! lengths cannot collide, and two of the same length are different patterns by
//! construction.
//!
//! ## Why the codes are written as text
//!
//! The table is transcribed from the published one and is audited against it,
//! which is a reading of a hundred lines rather than a computation. Bit patterns
//! would make that audit a conversion per line, and a conversion done by eye is
//! where a transcription error hides.
//!
//! The frequency ordering is the whole design and is worth seeing: a space is one
//! bit, the letter e is two, and the letters an operator sends least often reach
//! ten. That is why the format carries as much text per second as it does at a
//! bandwidth of thirty one hertz.

use std::collections::HashMap;

/// Longest code in the table, in bits.
///
/// Read by the decoder as the point past which an accumulation is noise rather
/// than a character. Ten plus the tentative nought that has not yet been
/// resolved into a boundary.
pub const MAX_BITS: u32 = 10;

/// Every code, as text.
///
/// The printable range plus the three control codes that carry meaning on the
/// air. The remaining control codes have codes of their own in the published
/// table and are omitted deliberately: nothing sends them, and a decoder that
/// accepted them would print a control character out of noise rather than
/// counting the noise as a failure.
const TABLE: &[(&str, char)] = &[
    ("11101111", '\t'),
    ("11101", '\n'),
    ("11111", '\r'),
    ("1", ' '),
    ("111111111", '!'),
    ("101011111", '"'),
    ("111110101", '#'),
    ("111011011", '$'),
    ("1011010101", '%'),
    ("1010111011", '&'),
    ("101111111", '\''),
    ("11111011", '('),
    ("11110111", ')'),
    ("101101111", '*'),
    ("111011111", '+'),
    ("1110101", ','),
    ("110101", '-'),
    ("1010111", '.'),
    ("110101111", '/'),
    ("10110111", '0'),
    ("10111101", '1'),
    ("11101101", '2'),
    ("11111111", '3'),
    ("101110111", '4'),
    ("101011011", '5'),
    ("101101011", '6'),
    ("110101011", '7'),
    ("110101101", '8'),
    ("110110111", '9'),
    ("11110101", ':'),
    ("110111101", ';'),
    ("111101101", '<'),
    ("1010101", '='),
    ("111010111", '>'),
    ("1010101111", '?'),
    ("1010111101", '@'),
    ("1111101", 'A'),
    ("11101011", 'B'),
    ("10101101", 'C'),
    ("10110101", 'D'),
    ("1110111", 'E'),
    ("11011011", 'F'),
    ("11111101", 'G'),
    ("101010101", 'H'),
    ("1111111", 'I'),
    ("111111101", 'J'),
    ("101111101", 'K'),
    ("11010111", 'L'),
    ("10111011", 'M'),
    ("11011101", 'N'),
    ("10101011", 'O'),
    ("11010101", 'P'),
    ("111011101", 'Q'),
    ("10101111", 'R'),
    ("1101111", 'S'),
    ("1101101", 'T'),
    ("101010111", 'U'),
    ("110110101", 'V'),
    ("101011101", 'W'),
    ("101110101", 'X'),
    ("101111011", 'Y'),
    ("1010101101", 'Z'),
    ("111110111", '['),
    ("111101111", '\\'),
    ("111111011", ']'),
    ("1010111111", '^'),
    ("101101101", '_'),
    ("1011011111", '`'),
    ("1011", 'a'),
    ("1011111", 'b'),
    ("101111", 'c'),
    ("101101", 'd'),
    ("11", 'e'),
    ("111101", 'f'),
    ("1011011", 'g'),
    ("101011", 'h'),
    ("1101", 'i'),
    ("111101011", 'j'),
    ("10111111", 'k'),
    ("11011", 'l'),
    ("111011", 'm'),
    ("1111", 'n'),
    ("111", 'o'),
    ("111111", 'p'),
    ("110111111", 'q'),
    ("10101", 'r'),
    ("10111", 's'),
    ("101", 't'),
    ("110111", 'u'),
    ("1111011", 'v'),
    ("1101011", 'w'),
    ("11011111", 'x'),
    ("1011101", 'y'),
    ("111010101", 'z'),
    ("1010110111", '{'),
    ("110111011", '|'),
    ("1010110101", '}'),
    ("1011010111", '~'),
];

/// Reads a code as an integer.
///
/// The leading one makes the result a unique key, see the note above.
fn packed(code: &str) -> u16 {
    let mut bits = 0u16;
    for byte in code.bytes() {
        bits = (bits << 1) | u16::from(byte == b'1');
    }
    bits
}

/// Code to character.
pub struct Alphabet {
    map: HashMap<u16, char>,
}

impl Alphabet {
    pub fn new() -> Alphabet {
        let mut map = HashMap::with_capacity(TABLE.len() * 2);
        for &(code, ch) in TABLE {
            map.insert(packed(code), ch);
        }
        Alphabet { map }
    }

    /// Resolves accumulated bits.
    ///
    /// Nothing for a pattern the table does not hold, which is what noise
    /// produces: the framing accepts any run of bits between two boundaries, so
    /// the table is the only thing that distinguishes a character from a
    /// coincidence.
    pub fn decode(&self, bits: u16) -> Option<char> {
        self.map.get(&bits).copied()
    }
}

impl Default for Alphabet {
    fn default() -> Alphabet {
        Alphabet::new()
    }
}

/// Code of a character, as text.
///
/// Present for the transmitter in the tests, which is what makes the decoder
/// checkable without a signal: the round trip is the only way to exercise the
/// whole chain on a desk.
pub fn code_of(ch: char) -> Option<&'static str> {
    TABLE.iter().find(|&&(_, c)| c == ch).map(|&(code, _)| code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_code_holds_the_boundary_pattern() {
        // The one property the framing rests on. A code containing two noughts
        // would be split in the middle by the decoder and neither half would
        // resolve, so this is a transcription check rather than a claim about
        // the format.
        for &(code, ch) in TABLE {
            assert!(!code.contains("00"), "{:?} holds a boundary", ch);
            assert!(code.starts_with('1'), "{:?} does not begin with one", ch);
            assert!(code.ends_with('1'), "{:?} does not end with one", ch);
            assert!(code.len() as u32 <= MAX_BITS, "{:?} is too long", ch);
        }
    }

    #[test]
    fn the_packed_form_is_a_unique_key() {
        // What lets the table hold no length column. A collision would make one
        // character decode as another, which is the failure that reads as a
        // receiver problem rather than as a table problem.
        let alphabet = Alphabet::new();
        assert_eq!(alphabet.map.len(), TABLE.len());
        for &(code, ch) in TABLE {
            assert_eq!(alphabet.decode(packed(code)), Some(ch));
        }
    }

    #[test]
    fn the_common_letters_are_the_short_ones() {
        // The whole reason the format carries usable text at thirty one hertz.
        assert_eq!(code_of(' '), Some("1"));
        assert_eq!(code_of('e'), Some("11"));
        assert_eq!(code_of('t'), Some("101"));
        // And the rare ones are not.
        assert_eq!(code_of('q').map(str::len), Some(9));
        assert_eq!(code_of('z').map(str::len), Some(9));
    }
}