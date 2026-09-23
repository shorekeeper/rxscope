//! Five bit teleprinter alphabet.
//!
//! The tables are the widely used variant of the international telegraph
//! alphabet number two: the letters case is standard, the figures case follows
//! the American teleprinter assignment that amateur traffic uses. The index is
//! the code value with the first transmitted bit as the least significant one,
//! which is the order the framing produces.

/// Control positions that are not printable characters.
const NUL: char = '\0';
const CR: char = '\r';
const LF: char = '\n';
const BELL: char = '\u{7}';

const LETTERS: [char; 32] = [
    NUL, 'E', LF, 'A', ' ', 'S', 'I', 'U', //
    CR, 'D', 'R', 'J', 'N', 'F', 'C', 'K', //
    'T', 'Z', 'L', 'W', 'H', 'Y', 'P', 'Q', //
    'O', 'B', 'G', NUL, 'M', 'X', 'V', NUL,
];

const FIGURES: [char; 32] = [
    NUL, '3', LF, '-', ' ', BELL, '8', '7', //
    CR, '$', '4', '\'', ',', '!', ':', '(', //
    '5', '"', ')', '2', '#', '6', '0', '1', //
    '9', '?', '&', NUL, '.', '/', ';', NUL,
];

/// Shift codes, which occupy the two positions left blank in both tables.
const CODE_FIGURES: u8 = 0x1B;
const CODE_LETTERS: u8 = 0x1F;
const CODE_SPACE: u8 = 0x04;

/// Shift state machine.
pub struct Ita2 {
    figures: bool,
    /// Return to the letters case after a space. Standard practice on the bands
    /// and the only way to recover from a lost letters shift within a word.
    pub unshift_on_space: bool,
}

impl Ita2 {
    pub fn new(unshift_on_space: bool) -> Ita2 {
        Ita2 { figures: false, unshift_on_space }
    }

    pub fn reset(&mut self) {
        self.figures = false;
    }

    pub fn in_figures(&self) -> bool {
        self.figures
    }

    /// Decodes one code value. Returns None for the shift codes and for the
    /// positions that carry no character.
    pub fn decode(&mut self, code: u8) -> Option<char> {
        let code = code & 0x1F;
        match code {
            CODE_FIGURES => {
                self.figures = true;
                None
            }
            CODE_LETTERS => {
                self.figures = false;
                None
            }
            CODE_SPACE => {
                if self.unshift_on_space {
                    self.figures = false;
                }
                Some(' ')
            }
            _ => {
                let ch = if self.figures {
                    FIGURES[code as usize]
                } else {
                    LETTERS[code as usize]
                };
                if ch == NUL {
                    None
                } else {
                    Some(ch)
                }
            }
        }
    }
}

/// Decodes a value from a link that carries plain character codes rather than
/// the five bit alphabet. The high bit is dropped because a seven bit link with
/// a parity bit in the eighth position is the common case.
pub fn decode_ascii(code: u8, data_bits: usize) -> Option<char> {
    let value = if data_bits >= 8 { code } else { code & 0x7F };
    match value {
        0x0A | 0x0D => Some(value as char),
        0x20..=0x7E => Some(value as char),
        _ => None,
    }
}