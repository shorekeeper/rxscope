//! What operators actually send.
//!
//! ## Why a trainer that only sends groups is half a trainer
//!
//! Random groups train one thing: recognizing a character with no context to
//! help. That is the foundation and it is not the skill. What an operator copies
//! on the air is a small vocabulary sent thousands of times, and it is copied as
//! whole shapes rather than letter by letter: nobody spells out `TNX`, they hear
//! it. A student who has only had groups meets real traffic and finds that the
//! speed they can copy has halved, because they are still assembling words a
//! character at a time.
//!
//! So the lists below are the second half. They are short on purpose. The point
//! is not coverage of the language, it is that the same forty shapes come round
//! often enough to stop being letters.
//!
//! ## Why the lists are compiled in
//!
//! A word list in a file is a file that has to exist. The source that reads one
//! stays, because an operator preparing for a particular contest wants their own
//! exchange, but a trainer whose word source produces nothing until a file is
//! written is a trainer with a broken setting.

/// Q codes, with what each one asks or answers.
///
/// The ones heard on the air. The full list runs past a hundred and most of it
/// belongs to maritime and aeronautical service; sending those would be
/// practising vocabulary nobody will meet.
pub const Q_CODES: &[(&str, &str)] = &[
    ("QRL", "the frequency is busy"),
    ("QRM", "interference from a station"),
    ("QRN", "interference from static"),
    ("QRO", "increase power"),
    ("QRP", "low power"),
    ("QRQ", "send faster"),
    ("QRS", "send slower"),
    ("QRT", "stop sending"),
    ("QRU", "nothing for you"),
    ("QRV", "ready"),
    ("QRX", "stand by"),
    ("QRZ", "who is calling"),
    ("QSB", "the signal is fading"),
    ("QSK", "break in"),
    ("QSL", "acknowledged"),
    ("QSO", "a contact"),
    ("QSY", "change frequency"),
    ("QTH", "location"),
    ("QTR", "the time"),
];

/// Abbreviations, with what each one means.
///
/// The working vocabulary of a contact. Most are three characters or fewer,
/// which is what makes them a shape rather than a spelling.
pub const ABBREVIATIONS: &[(&str, &str)] = &[
    ("73", "best regards"),
    ("88", "love and kisses"),
    ("AGN", "again"),
    ("ANT", "antenna"),
    ("BK", "break"),
    ("BTU", "back to you"),
    ("CFM", "confirm"),
    ("CQ", "calling anybody"),
    ("CUL", "see you later"),
    ("DE", "from"),
    ("DX", "distant station"),
    ("ES", "and"),
    ("FB", "fine business"),
    ("GA", "good afternoon"),
    ("GE", "good evening"),
    ("GM", "good morning"),
    ("GN", "good night"),
    ("GUD", "good"),
    ("HI", "laughter"),
    ("HR", "here"),
    ("HW", "how do you copy"),
    ("K", "over to you"),
    ("MNI", "many"),
    ("NR", "number"),
    ("OM", "old man"),
    ("OP", "operator"),
    ("PSE", "please"),
    ("PWR", "power"),
    ("R", "received"),
    ("RIG", "the equipment"),
    ("RST", "the report"),
    ("RX", "receiver"),
    ("SIG", "signal"),
    ("SRI", "sorry"),
    ("TNX", "thanks"),
    ("TU", "thank you"),
    ("TX", "transmitter"),
    ("UR", "your"),
    ("VY", "very"),
    ("WKD", "worked"),
    ("WX", "the weather"),
    ("XYL", "wife"),
    ("YL", "young lady"),
];

/// Plain words, commonest first.
///
/// The hundred or so that carry most of any English text. Ordered by frequency
/// rather than alphabetically, because the pool filter takes the ones the
/// student can spell and the frequent ones are the ones worth having when only
/// a few fit.
pub const WORDS: &[&str] = &[
    "THE", "AND", "YOU", "THAT", "WAS", "FOR", "ARE", "WITH", "HIS", "THEY",
    "THIS", "HAVE", "FROM", "ONE", "HAD", "BUT", "NOT", "WHAT", "ALL", "WERE",
    "WHEN", "YOUR", "CAN", "SAID", "THERE", "USE", "EACH", "WHICH", "SHE", "HOW",
    "THEIR", "WILL", "OTHER", "ABOUT", "OUT", "MANY", "THEN", "THEM", "THESE",
    "SOME", "HER", "WOULD", "MAKE", "LIKE", "HIM", "INTO", "TIME", "HAS", "LOOK",
    "TWO", "MORE", "WRITE", "SEE", "NUMBER", "WAY", "COULD", "PEOPLE", "THAN",
    "FIRST", "WATER", "BEEN", "CALL", "WHO", "NOW", "FIND", "LONG", "DOWN",
    "DAY", "DID", "GET", "COME", "MADE", "MAY", "PART", "OVER", "NEW", "SOUND",
    "TAKE", "ONLY", "LITTLE", "WORK", "KNOW", "PLACE", "YEAR", "LIVE", "BACK",
    "GIVE", "MOST", "VERY", "AFTER", "THING", "OUR", "JUST", "NAME", "GOOD",
    "SENTENCE", "MAN", "THINK", "SAY", "GREAT", "WHERE", "HELP", "THROUGH",
    "MUCH", "BEFORE", "LINE", "RIGHT", "TOO", "MEAN", "OLD", "ANY", "SAME",
    "TELL", "BOY", "FOLLOW", "CAME", "WANT", "SHOW", "ALSO", "AROUND", "FORM",
    "THREE", "SMALL", "SET", "PUT", "END", "DOES", "ANOTHER", "WELL", "LARGE",
    "MUST", "BIG", "SUCH", "EVEN", "HERE", "WHY", "ASK", "WENT", "MEN", "READ",
    "NEED", "LAND", "DIFFERENT", "HOME", "MOVE", "TRY", "KIND", "HAND", "PICTURE",
];

/// Longest text one entry of any list occupies.
///
/// Read by the interface, which reserves a column for the meaning beside the
/// token: measuring it per frame would be measuring a constant.
pub const MAX_TOKEN: usize = 8;

/// Meaning of a token, if one of the lists carries it.
///
/// Looked up rather than carried alongside, because the material generator
/// produces a string and the interface is the only thing that wants the gloss.
/// Linear over sixty entries, consulted once per group.
pub fn meaning(token: &str) -> Option<&'static str> {
    for &(name, text) in Q_CODES {
        if name == token {
            return Some(text);
        }
    }
    for &(name, text) in ABBREVIATIONS {
        if name == token {
            return Some(text);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_can_be_sent() {
        // A token holding a character the alphabet does not carry would be
        // dropped by the keyer, and the student would be asked to copy something
        // shorter than what the panel says was sent.
        for &(name, _) in Q_CODES.iter().chain(ABBREVIATIONS.iter()) {
            for ch in name.chars() {
                assert!(
                    crate::morse::pattern_of(ch).is_some(),
                    "{} holds {:?}, which has no pattern",
                    name,
                    ch
                );
            }
        }
        for word in WORDS {
            for ch in word.chars() {
                assert!(
                    crate::morse::pattern_of(ch).is_some(),
                    "{} holds {:?}, which has no pattern",
                    word,
                    ch
                );
            }
        }
    }

    #[test]
    fn nothing_is_listed_twice() {
        // A duplicate is a token drawn twice as often as the rest, which over a
        // session is practice spent on one shape for no reason.
        let mut seen: Vec<&str> = Vec::new();
        for &(name, _) in Q_CODES.iter().chain(ABBREVIATIONS.iter()) {
            assert!(!seen.contains(&name), "{} appears twice", name);
            seen.push(name);
        }
        let mut words: Vec<&str> = Vec::new();
        for word in WORDS {
            assert!(!words.contains(word), "{} appears twice", word);
            words.push(word);
        }
    }

    #[test]
    fn the_stated_width_holds() {
        for &(name, _) in Q_CODES.iter().chain(ABBREVIATIONS.iter()) {
            assert!(name.len() <= MAX_TOKEN, "{} is wider than stated", name);
        }
    }

    #[test]
    fn every_gloss_is_reachable() {
        for &(name, text) in Q_CODES.iter().chain(ABBREVIATIONS.iter()) {
            assert_eq!(meaning(name), Some(text));
        }
        assert_eq!(meaning("ZZZ"), None);
    }
}