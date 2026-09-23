//! Prefix database.
//!
//! Reads the country file the contest community maintains, which is the one
//! offline source that answers what a call sign is worth knowing about during
//! reception: the country, the two zone systems and the continent. Everything
//! else a lookup service offers, the address and the operator name, is read
//! after the contact rather than during it, and belongs in a log program.
//!
//! ## Why offline
//!
//! An online service fails exactly when it is needed. A contest, a portable
//! operation, a weak link: those are the sessions where an unresolved call
//! matters, and they are the sessions where a request does not complete. The
//! file is half a megabyte, is revised quarterly, and answers in microseconds.
//!
//! ## Format
//!
//! One record per country, a header line of eight colon separated fields
//! followed by a comma separated prefix list terminated by a semicolon:
//!
//! ```text
//! Fed. Republic of Germany:  14:  28:  EU:   51.00:   -10.00:    -1.0:  DL:
//!     DA,DB,DC,=DL1ABC,DL0(15)[18];
//! ```
//!
//! An entry beginning with an equals sign is a whole call rather than a prefix,
//! which is how a station operating from somewhere other than its own country
//! is handled. Parentheses override the CQ zone, brackets the ITU zone, and the
//! remaining bracket forms carry coordinates and a time offset that nothing
//! here reads.
//!
//! ## Lookup
//!
//! Exact matches first, then the longest prefix. Longest rather than first,
//! because the file lists both broad and narrow prefixes for the same country
//! and the narrow one is the one that carries the zone override: resolving `EA8`
//! through `EA` would report mainland Spain for the Canary Islands.

use std::collections::HashMap;
use std::path::Path;

/// One country record.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    /// Primary allocation prefix, which is what a log program records.
    pub dxcc: String,
    pub cq_zone: u8,
    pub itu_zone: u8,
    /// Two letter continent code.
    pub continent: String,
}

/// What a lookup produced.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub name: String,
    pub dxcc: String,
    pub cq_zone: u8,
    pub itu_zone: u8,
    pub continent: String,
}

/// A prefix and the overrides attached to it.
#[derive(Debug, Clone, Copy)]
struct Alias {
    entry: u32,
    /// Nought when the record value stands.
    cq_zone: u8,
    itu_zone: u8,
}

pub struct Prefixes {
    entries: Vec<Entry>,
    /// Prefix to alias. The key is upper case and holds no override syntax.
    by_prefix: HashMap<String, Alias>,
    /// Whole calls, which take precedence over every prefix.
    by_call: HashMap<String, Alias>,
    /// Longest prefix in the file, so the lookup knows where to start.
    longest: usize,
}

impl Prefixes {
    pub fn empty() -> Prefixes {
        Prefixes {
            entries: Vec::new(),
            by_prefix: HashMap::new(),
            by_call: HashMap::new(),
            longest: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn countries(&self) -> usize {
        self.entries.len()
    }

    pub fn prefixes(&self) -> usize {
        self.by_prefix.len() + self.by_call.len()
    }

    /// Reads a country file.
    ///
    /// A malformed record is skipped rather than fatal. The file is edited by
    /// hand by whoever publishes it and a single bad line is not a reason to
    /// resolve nothing at all.
    pub fn load(path: &Path) -> Prefixes {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                crate::log_warn!("callsign", "{}: {}", path.display(), e);
                return Prefixes::empty();
            }
        };

        let mut out = Prefixes::empty();
        // A record spans several lines, so the whole thing is accumulated and
        // then split on the terminator: the line breaks inside a prefix list
        // carry no meaning and are only there to keep the file readable.
        let mut record = String::with_capacity(512);
        let mut skipped = 0usize;

        for line in text.lines() {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            record.push_str(line);
            if !line.ends_with(';') {
                continue;
            }

            if !out.parse_record(&record) {
                skipped += 1;
            }
            record.clear();
        }

        crate::log_info!(
            "callsign",
            "{}: {} countries, {} prefixes, {} records skipped",
            path.display(),
            out.entries.len(),
            out.prefixes(),
            skipped
        );
        out
    }

    fn parse_record(&mut self, record: &str) -> bool {
        // The header ends at the eighth colon and the prefix list follows it.
        // Counting rather than splitting on the whole string, because a country
        // name may itself hold a colon.
        let mut colons = 0usize;
        let mut split = None;
        for (at, ch) in record.char_indices() {
            if ch == ':' {
                colons += 1;
                if colons == 8 {
                    split = Some(at);
                    break;
                }
            }
        }
        let split = match split {
            Some(s) => s,
            None => return false,
        };

        let head: Vec<&str> = record[..split].split(':').map(|f| f.trim()).collect();
        if head.len() < 8 {
            return false;
        }

        let entry = Entry {
            name: head[0].to_string(),
            dxcc: head[7].to_ascii_uppercase(),
            cq_zone: head[1].parse().unwrap_or(0),
            itu_zone: head[2].parse().unwrap_or(0),
            continent: head[3].to_ascii_uppercase(),
        };
        if entry.name.is_empty() {
            return false;
        }

        let index = self.entries.len() as u32;
        self.entries.push(entry);

        let list = record[split + 1..].trim_end_matches(';');
        for raw in list.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            self.parse_alias(item, index);
        }
        true
    }

    fn parse_alias(&mut self, item: &str, entry: u32) {
        let exact = item.starts_with('=');
        let body = if exact { &item[1..] } else { item };

        let mut key = String::with_capacity(body.len());
        let mut cq_zone = 0u8;
        let mut itu_zone = 0u8;
        // The overrides are read as they are met and the coordinate and offset
        // forms are consumed and discarded, because a bracket left in the key
        // would make it match nothing.
        let mut number = String::new();
        let mut inside: Option<char> = None;

        for ch in body.chars() {
            match inside {
                None => match ch {
                    '(' | '[' | '<' | '{' | '~' => {
                        inside = Some(ch);
                        number.clear();
                    }
                    c if c.is_ascii_alphanumeric() || c == '/' => {
                        key.push(c.to_ascii_uppercase());
                    }
                    _ => {}
                },
                Some(open) => {
                    let closes = match open {
                        '(' => ch == ')',
                        '[' => ch == ']',
                        '<' => ch == '>',
                        '{' => ch == '}',
                        _ => ch == '~',
                    };
                    if closes {
                        match open {
                            '(' => cq_zone = number.parse().unwrap_or(0),
                            '[' => itu_zone = number.parse().unwrap_or(0),
                            _ => {}
                        }
                        inside = None;
                    } else {
                        number.push(ch);
                    }
                }
            }
        }

        if key.is_empty() {
            return;
        }
        let alias = Alias { entry, cq_zone, itu_zone };
        if exact {
            self.by_call.insert(key, alias);
        } else {
            self.longest = self.longest.max(key.len());
            // Later duplicates win. The published file holds a few, and the
            // last one is the correction.
            self.by_prefix.insert(key, alias);
        }
    }

    /// Resolves a call or a prefix.
    ///
    /// The key must already be upper case and free of qualifiers, which is what
    /// the extractor hands over.
    pub fn resolve(&self, key: &str) -> Option<Resolved> {
        if self.entries.is_empty() || key.is_empty() {
            return None;
        }

        let alias = match self.by_call.get(key) {
            Some(a) => *a,
            None => {
                // Longest prefix. Starting from the whole key rather than from
                // the longest prefix in the file, so a key shorter than that
                // costs fewer probes than it otherwise would.
                let mut alias = None;
                let mut len = key.len().min(self.longest);
                while len > 0 {
                    if let Some(found) = self.by_prefix.get(&key[..len]) {
                        alias = Some(*found);
                        break;
                    }
                    len -= 1;
                }
                alias?
            }
        };

        let entry = self.entries.get(alias.entry as usize)?;
        Some(Resolved {
            name: entry.name.clone(),
            dxcc: entry.dxcc.clone(),
            cq_zone: if alias.cq_zone > 0 { alias.cq_zone } else { entry.cq_zone },
            itu_zone: if alias.itu_zone > 0 { alias.itu_zone } else { entry.itu_zone },
            continent: entry.continent.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Prefixes {
        let mut p = Prefixes::empty();
        assert!(p.parse_record(
            "Fed. Republic of Germany:  14:  28:  EU:   51.00:   -10.00:    -1.0:  DL:  DA,DB,DL;"
        ));
        assert!(p.parse_record(
            "Canary Islands:  33:  36:  AF:   28.32:    15.85:     1.0:  EA8:  EA8,=EA8/DL1ABC;"
        ));
        assert!(p.parse_record(
            "Spain:  14:  37:  EU:   40.37:     3.72:    -1.0:  EA:  EA,EA5(15)[37];"
        ));
        p
    }

    #[test]
    fn a_longer_prefix_wins() {
        // The whole reason the lookup walks down from the full key: mainland
        // Spain and the Canary Islands share the leading letters, and the
        // shorter match would report the wrong continent.
        let p = sample();
        assert_eq!(p.resolve("EA8AB").unwrap().name, "Canary Islands");
        assert_eq!(p.resolve("EA1AB").unwrap().name, "Spain");
        assert_eq!(p.resolve("EA8AB").unwrap().continent, "AF");
    }

    #[test]
    fn an_exact_entry_outranks_every_prefix() {
        let p = sample();
        assert_eq!(p.resolve("EA8/DL1ABC").unwrap().name, "Canary Islands");
        assert_eq!(p.resolve("DL1ABC").unwrap().name, "Fed. Republic of Germany");
    }

    #[test]
    fn an_override_replaces_the_record_zone() {
        let p = sample();
        let plain = p.resolve("EA1AB").unwrap();
        let shifted = p.resolve("EA5AB").unwrap();
        assert_eq!(plain.cq_zone, 14);
        assert_eq!(shifted.cq_zone, 15);
        // The record value stands where no override was stated.
        assert_eq!(shifted.itu_zone, 37);
    }

    #[test]
    fn an_unknown_key_resolves_to_nothing() {
        let p = sample();
        assert!(p.resolve("ZZ9ZZ").is_none());
        assert!(p.resolve("").is_none());
    }
}