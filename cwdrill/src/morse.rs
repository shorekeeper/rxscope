//! The code, and the orders a lesson introduces it in.
//!
//! Data rather than logic. The table is transcribed from the international
//! alphabet and is audited against it, which is a reading of ninety lines
//! rather than a computation.
//!
//! ## Why the patterns are text
//!
//! A dot and a dash written as characters can be compared against a printed
//! table by eye. Packed into bits they would have to be converted per line to
//! be checked, and a conversion done by eye is where a transcription error
//! hides. The keyer walks the string once per character, which at fifty words a
//! minute is a few hundred character comparisons per second.
//!
//! ## Prosigns
//!
//! Several are indistinguishable from punctuation, because that is what they
//! are: the plus sign and the end of message are the same five elements, and a
//! receiver tells them apart from context rather than from the signal. Only the
//! ones with no punctuation of their own are listed separately.

/// Character to pattern.
const TABLE: &[(char, &str)] = &[
    ('A', ".-"),
    ('B', "-..."),
    ('C', "-.-."),
    ('D', "-.."),
    ('E', "."),
    ('F', "..-."),
    ('G', "--."),
    ('H', "...."),
    ('I', ".."),
    ('J', ".---"),
    ('K', "-.-"),
    ('L', ".-.."),
    ('M', "--"),
    ('N', "-."),
    ('O', "---"),
    ('P', ".--."),
    ('Q', "--.-"),
    ('R', ".-."),
    ('S', "..."),
    ('T', "-"),
    ('U', "..-"),
    ('V', "...-"),
    ('W', ".--"),
    ('X', "-..-"),
    ('Y', "-.--"),
    ('Z', "--.."),
    ('0', "-----"),
    ('1', ".----"),
    ('2', "..---"),
    ('3', "...--"),
    ('4', "....-"),
    ('5', "....."),
    ('6', "-...."),
    ('7', "--..."),
    ('8', "---.."),
    ('9', "----."),
    ('.', ".-.-.-"),
    (',', "--..--"),
    ('?', "..--.."),
    ('\'', ".----."),
    ('!', "-.-.--"),
    ('/', "-..-."),
    ('(', "-.--."),
    (')', "-.--.-"),
    ('&', ".-..."),
    (':', "---..."),
    (';', "-.-.-."),
    ('=', "-...-"),
    ('+', ".-.-."),
    ('-', "-....-"),
    ('_', "..--.-"),
    ('"', ".-..-."),
    ('$', "...-..-"),
    ('@', ".--.-."),
];

/// Prosigns that no punctuation already covers.
///
/// The name is what appears between angle brackets in the material and in the
/// transcript, so an operator reading either sees the same token.
const PROSIGNS: &[(&str, &str)] = &[
    ("SK", "...-.-"),
    ("SN", "...-."),
    ("CT", "-.-.-"),
    ("HH", "........"),
];

/// Order the incremental method introduces characters in.
///
/// Two characters that are opposites, then one at a time, and the digits and
/// punctuation interleaved rather than saved for the end. That interleaving is
/// the point: a set of letters alone teaches the ear a letter shaped
/// expectation, and the first digit then arrives as a different alphabet.
pub const KOCH_ORDER: &str = "KMURESNAPTLWI.JZ=FOY,VG5/Q92H38B?47C1D6X0";

/// Alphabetical order, for a student following a printed course.
pub const ALPHABET_ORDER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// Roughly by frequency in English text, digits after the letters.
///
/// Offered because it produces readable words earliest, which is what keeps a
/// student going; it is the worst order for reflex recognition, because the
/// short patterns come first and the ear learns to count elements.
pub const FREQUENCY_ORDER: &str = "ETAOINSHRDLCUMWFGYPBVKJXQZ0123456789";

/// Pattern of one character, folded to upper case.
pub fn pattern_of(ch: char) -> Option<&'static str> {
    let upper = ch.to_ascii_uppercase();
    TABLE.iter().find(|&&(c, _)| c == upper).map(|&(_, p)| p)
}

/// Character one pattern stands for.
///
/// The decoder reads the table in this direction: what a paddle produces is a
/// sequence of elements, and the only way back to a character is a lookup.
/// Linear, because the table is ninety entries and a character arrives a few
/// times a second at most.
pub fn char_of(pattern: &str) -> Option<char> {
    TABLE.iter().find(|&&(_, p)| p == pattern).map(|&(c, _)| c)
}

/// Pattern of a prosign named without its brackets.
///
/// Falls through to the punctuation table, so the several prosigns that are
/// also punctuation resolve rather than being reported as unknown.
pub fn prosign(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    if let Some(&(_, p)) = PROSIGNS.iter().find(|&&(n, _)| n == upper) {
        return Some(p);
    }
    match upper.as_str() {
        "AR" => pattern_of('+'),
        "BT" => pattern_of('='),
        "AS" => pattern_of('&'),
        "KN" => pattern_of('('),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_characters_share_a_pattern() {
        // A collision would make one character decode as another, which reads as
        // a student mistake rather than as a table mistake.
        for (i, &(ca, pa)) in TABLE.iter().enumerate() {
            for &(cb, pb) in TABLE.iter().skip(i + 1) {
                assert_ne!(pa, pb, "{:?} and {:?} share {}", ca, cb, pa);
            }
        }
    }

    #[test]
    fn every_pattern_holds_only_the_two_symbols() {
        for &(ch, pattern) in TABLE {
            assert!(!pattern.is_empty(), "{:?} has no pattern", ch);
            assert!(
                pattern.chars().all(|c| c == '.' || c == '-'),
                "{:?} holds something other than a dot or a dash",
                ch
            );
        }
    }

    #[test]
    fn every_order_names_characters_the_table_holds() {
        // An order that names a character with no pattern would produce a level
        // at which nothing is sent, which reads as a broken generator.
        for order in [KOCH_ORDER, ALPHABET_ORDER, FREQUENCY_ORDER] {
            for ch in order.chars() {
                assert!(pattern_of(ch).is_some(), "{:?} is not in the table", ch);
            }
        }
    }

    #[test]
    fn the_incremental_order_holds_no_repeat() {
        // A repeat would silently shorten the course by one level.
        let mut seen = Vec::new();
        for ch in KOCH_ORDER.chars() {
            assert!(!seen.contains(&ch), "{:?} appears twice", ch);
            seen.push(ch);
        }
    }

    #[test]
    fn the_shared_prosigns_resolve_through_the_punctuation() {
        assert_eq!(prosign("AR"), pattern_of('+'));
        assert_eq!(prosign("BT"), pattern_of('='));
        assert_eq!(prosign("SK"), Some("...-.-"));
        assert_eq!(prosign("ZZ"), None);
    }

}