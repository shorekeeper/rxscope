//! INI document model.
//!
//! Format rules:
//!   - section headers in square brackets;
//!   - key = value, value is trimmed and unquoted if wrapped in quotes;
//!   - comments start with ; or #, kept in the model so a rewrite preserves
//!     the documentation lines that ship with the default file;
//!   - unknown sections and keys are preserved on save.
//!
//! Parsing never fails: malformed lines are reported to the log and skipped,
//! because a broken config must not prevent the receiver from starting.

use std::path::Path;

use crate::core::{Error, Result};

/// Enum stored as text in the config file.
pub trait ConfigEnum: Sized + Copy {
    fn from_config(text: &str) -> Option<Self>;
    fn to_config(&self) -> &'static str;
    fn variants() -> &'static [&'static str];
}

#[derive(Debug, Clone)]
enum Entry {
    Pair { key: String, value: String },
    Comment(String),
    Blank,
}

#[derive(Debug, Clone)]
struct Section {
    name: String,
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: Vec<Section>,
}

impl Ini {
    pub fn new() -> Ini {
        Ini::default()
    }

    pub fn parse(text: &str) -> Ini {
        let mut doc = Ini::new();
        let mut current = String::new();
        doc.sections.push(Section { name: String::new(), entries: Vec::new() });

        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() {
                doc.section_mut(&current).entries.push(Entry::Blank);
                continue;
            }
            if line.starts_with(';') || line.starts_with('#') {
                doc.section_mut(&current).entries.push(Entry::Comment(line[1..].trim().to_string()));
                continue;
            }
            if line.starts_with('[') {
                match line.find(']') {
                    Some(end) => {
                        current = line[1..end].trim().to_string();
                        doc.ensure_section(&current);
                    }
                    None => {
                        crate::log_warn!("config", "line {}: unterminated section header", lineno + 1);
                    }
                }
                continue;
            }
            match line.find('=') {
                Some(eq) => {
                    let key = line[..eq].trim().to_string();
                    let value = unquote(line[eq + 1..].trim());
                    if key.is_empty() {
                        crate::log_warn!("config", "line {}: empty key", lineno + 1);
                        continue;
                    }
                    let sec = doc.section_mut(&current);
                    // Later duplicates win, which matches operator intuition.
                    if let Some(existing) = sec.entries.iter_mut().find_map(|e| match e {
                        Entry::Pair { key: k, value: v } if k.eq_ignore_ascii_case(&key) => Some(v),
                        _ => None,
                    }) {
                        *existing = value;
                    } else {
                        sec.entries.push(Entry::Pair { key, value });
                    }
                }
                None => crate::log_warn!("config", "line {}: missing '='", lineno + 1),
            }
        }
        doc
    }

    pub fn load(path: &Path) -> Result<Ini> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::config(format!("{}: {}", path.display(), e)))?;
        Ok(Ini::parse(&text))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(Error::from)?;
            }
        }
        std::fs::write(path, self.to_text()).map_err(Error::from)
    }

    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(8 * 1024);
        for (i, sec) in self.sections.iter().enumerate() {
            if !sec.name.is_empty() {
                if i > 0 {
                    out.push('\n');
                }
                out.push('[');
                out.push_str(&sec.name);
                out.push_str("]\n");
            }
            for entry in &sec.entries {
                match entry {
                    Entry::Pair { key, value } => {
                        out.push_str(key);
                        out.push_str(" = ");
                        out.push_str(&quote_if_needed(value));
                        out.push('\n');
                    }
                    Entry::Comment(c) => {
                        out.push_str("; ");
                        out.push_str(c);
                        out.push('\n');
                    }
                    Entry::Blank => out.push('\n'),
                }
            }
        }
        out
    }

    fn ensure_section(&mut self, name: &str) {
        if !self.sections.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            self.sections.push(Section { name: name.to_string(), entries: Vec::new() });
        }
    }

    fn section_mut(&mut self, name: &str) -> &mut Section {
        self.ensure_section(name);
        self.sections
            .iter_mut()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .expect("section was just ensured")
    }

    fn find(&self, section: &str, key: &str) -> Option<&str> {
        let sec = self.sections.iter().find(|s| s.name.eq_ignore_ascii_case(section))?;
        sec.entries.iter().find_map(|e| match e {
            Entry::Pair { key: k, value } if k.eq_ignore_ascii_case(key) => Some(value.as_str()),
            _ => None,
        })
    }

    pub fn has_section(&self, section: &str) -> bool {
        self.sections.iter().any(|s| s.name.eq_ignore_ascii_case(section))
    }

    // Typed readers. Every one falls back to the default and logs the reason
    // so a typo in the file is visible instead of silently ignored.

    pub fn get_string(&self, section: &str, key: &str, default: &str) -> String {
        self.find(section, key).map(|s| s.to_string()).unwrap_or_else(|| default.to_string())
    }

    pub fn get_bool(&self, section: &str, key: &str, default: bool) -> bool {
        match self.find(section, key) {
            None => default,
            Some(v) => match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                other => {
                    crate::log_warn!("config", "[{}] {}: bad bool '{}'", section, key, other);
                    default
                }
            },
        }
    }

    pub fn get_u32(&self, section: &str, key: &str, default: u32) -> u32 {
        self.get_parsed(section, key, default)
    }
    pub fn get_i32(&self, section: &str, key: &str, default: i32) -> i32 {
        self.get_parsed(section, key, default)
    }
    pub fn get_usize(&self, section: &str, key: &str, default: usize) -> usize {
        self.get_parsed(section, key, default)
    }
    pub fn get_f32(&self, section: &str, key: &str, default: f32) -> f32 {
        self.get_parsed(section, key, default)
    }
    pub fn get_f64(&self, section: &str, key: &str, default: f64) -> f64 {
        self.get_parsed(section, key, default)
    }

    fn get_parsed<T: std::str::FromStr + Copy>(&self, section: &str, key: &str, default: T) -> T {
        match self.find(section, key) {
            None => default,
            Some(v) => match v.trim().parse::<T>() {
                Ok(x) => x,
                Err(_) => {
                    crate::log_warn!("config", "[{}] {}: bad number '{}'", section, key, v);
                    default
                }
            },
        }
    }

    /// Clamped numeric read for values where an out of range entry would
    /// break DSP invariants, for example an FFT size of zero.
    pub fn get_u32_clamped(&self, section: &str, key: &str, default: u32, lo: u32, hi: u32) -> u32 {
        let v = self.get_u32(section, key, default);
        if v < lo || v > hi {
            crate::log_warn!("config", "[{}] {}: {} out of [{}, {}]", section, key, v, lo, hi);
            return default.clamp(lo, hi);
        }
        v
    }

    pub fn get_f32_clamped(&self, section: &str, key: &str, default: f32, lo: f32, hi: f32) -> f32 {
        let v = self.get_f32(section, key, default);
        if !v.is_finite() || v < lo || v > hi {
            crate::log_warn!("config", "[{}] {}: {} out of [{}, {}]", section, key, v, lo, hi);
            return default.clamp(lo, hi);
        }
        v
    }

    pub fn get_enum<T: ConfigEnum>(&self, section: &str, key: &str, default: T) -> T {
        match self.find(section, key) {
            None => default,
            Some(v) => match T::from_config(v.trim().to_ascii_lowercase().as_str()) {
                Some(x) => x,
                None => {
                    crate::log_warn!(
                        "config",
                        "[{}] {}: unknown value '{}', expected one of {:?}",
                        section,
                        key,
                        v,
                        T::variants()
                    );
                    default
                }
            },
        }
    }

    /// Comma separated list, empty items removed.
    pub fn get_list(&self, section: &str, key: &str) -> Vec<String> {
        self.find(section, key)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    // Writers.

    pub fn set_string(&mut self, section: &str, key: &str, value: &str) {
        let sec = self.section_mut(section);
        if let Some(v) = sec.entries.iter_mut().find_map(|e| match e {
            Entry::Pair { key: k, value } if k.eq_ignore_ascii_case(key) => Some(value),
            _ => None,
        }) {
            *v = value.to_string();
        } else {
            sec.entries.push(Entry::Pair { key: key.to_string(), value: value.to_string() });
        }
    }

    pub fn set_bool(&mut self, section: &str, key: &str, value: bool) {
        self.set_string(section, key, if value { "true" } else { "false" });
    }
    pub fn set_u32(&mut self, section: &str, key: &str, value: u32) {
        self.set_string(section, key, &value.to_string());
    }
    pub fn set_i32(&mut self, section: &str, key: &str, value: i32) {
        self.set_string(section, key, &value.to_string());
    }
    pub fn set_usize(&mut self, section: &str, key: &str, value: usize) {
        self.set_string(section, key, &value.to_string());
    }
    pub fn set_f32(&mut self, section: &str, key: &str, value: f32) {
        // Trim trailing zeros so the file stays readable.
        let s = format!("{}", (value as f64 * 1000.0).round() / 1000.0);
        self.set_string(section, key, &s);
    }
    pub fn set_enum<T: ConfigEnum>(&mut self, section: &str, key: &str, value: T) {
        self.set_string(section, key, value.to_config());
    }
    pub fn set_list(&mut self, section: &str, key: &str, items: &[String]) {
        self.set_string(section, key, &items.join(", "));
    }

    /// Appends a documentation line to a section.
    pub fn comment(&mut self, section: &str, text: &str) {
        self.section_mut(section).entries.push(Entry::Comment(text.to_string()));
    }

    pub fn blank(&mut self, section: &str) {
        self.section_mut(section).entries.push(Entry::Blank);
    }
    
    /// Every stated pair, in file order.
    ///
    /// Present so two documents can be compared: a configuration and the one the
    /// build would have written are the same set of keys, so a difference between
    /// them is a difference in a value and nothing else.
    pub fn pairs<'a>(&'a self) -> impl Iterator<Item = (&'a str, &'a str, &'a str)> + 'a {
        self.sections.iter().flat_map(|section| {
            let name = section.name.as_str();
            section.entries.iter().filter_map(move |entry| match entry {
                Entry::Pair { key, value } => Some((name, key.as_str(), value.as_str())),
                _ => None,
            })
        })
    }

    /// Value as it stands in the document, without conversion.
    ///
    /// The typed readers apply a default and clamp, which is what a subsystem
    /// wants and what a comparison must not have: a clamped value would read as
    /// equal to the default it was clamped towards.
    pub fn raw(&self, section: &str, key: &str) -> Option<&str> {
        self.find(section, key)
    }

    /// Sections in file order, for a caller that iterates rather than looks up.
    ///
    /// Exposed because a station list is keyed by whatever the operator chose,
    /// so there is no fixed set of names to ask for.
    pub fn section_names(&self) -> impl Iterator<Item = &str> {
        self.sections
            .iter()
            .map(|s| s.name.as_str())
            .filter(|n| !n.is_empty())
    }
}

fn unquote(s: &str) -> String {
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn quote_if_needed(s: &str) -> String {
    // Quote when leading or trailing spaces would be lost on reread.
    if s != s.trim() {
        format!("\"{}\"", s)
    } else {
        s.to_string()
    }
}