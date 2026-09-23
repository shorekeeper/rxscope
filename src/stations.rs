//! Known stations and band plan.
//!
//! A frequency an operator returns to is a fact about the band rather than
//! about the receiver, so it lives in a file of its own and not in the
//! configuration: the configuration is rewritten on every exit, and a list an
//! operator maintains by hand must not be reformatted underneath them.
//!
//! ## What it is for
//!
//! A waterfall shows where signals are; it does not show which of them is the
//! net that meets at nine, the beacon that is always there, or the pair of
//! frequencies a contest is worked on. Those are known in advance and are
//! exactly what an operator scans for. Marking them turns a search into a
//! comparison.
//!
//! ## Format
//!
//! One section per entry, named by whatever the operator wants to key it on.
//!
//! ```text
//! [beacon-4u1un]
//! frequency = 14100000
//! label = 4U1UN
//! mode = CW
//! note = IBP beacon
//! ```
//!
//! Everything except the frequency is optional. A section without a usable
//! frequency is reported and skipped, because a marker at nought would sit on
//! the left edge and look like a fault in the display.
//!
//! ## Band plan
//!
//! Built in rather than read from the file. The amateur allocations are a
//! property of the spectrum and do not vary per installation; an operator who
//! wants a different segmentation adds entries instead. The list is the
//! international one, which differs by region at the edges: that difference is
//! smaller than the width of a marker and is not worth three tables.

use std::path::{Path, PathBuf};

use crate::config::ini::Ini;

/// Frequency below which an entry is treated as a mistake.
///
/// Nothing an amateur receiver reaches sits under ten kilohertz, and the value
/// most often written by accident is nought.
const MIN_HZ: i64 = 10_000;

/// One marked frequency.
#[derive(Debug, Clone)]
pub struct Station {
    pub hz: i64,
    /// Text drawn beside the marker. The section name when none was given, so
    /// an entry is always identifiable.
    pub label: String,
    /// Mode as the operator wrote it. Free text rather than an enumeration: a
    /// list of frequencies carries things this application has no detector for,
    /// and refusing them would be refusing the operator's own notes.
    pub mode: String,
    pub note: String,
}

impl Station {
    /// One line for a list, without the frequency.
    pub fn describe(&self) -> String {
        let mut out = self.label.clone();
        if !self.mode.is_empty() {
            out.push_str("  ");
            out.push_str(&self.mode);
        }
        if !self.note.is_empty() {
            out.push_str("  ");
            out.push_str(&self.note);
        }
        out
    }
}

/// One segment of the band plan.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub low_hz: i64,
    pub high_hz: i64,
    pub name: &'static str,
    /// What the segment is used for, in the shortest form that identifies it.
    pub usage: &'static str,
}

/// Amateur allocations, in ascending order.
///
/// The band name is the wavelength an operator names it by rather than the
/// frequency, because that is what appears in every log and every conversation.
const BANDS: &[Segment] = &[
    Segment { low_hz: 135_700, high_hz: 137_800, name: "2200m", usage: "CW" },
    Segment { low_hz: 472_000, high_hz: 479_000, name: "630m", usage: "CW" },
    Segment { low_hz: 1_810_000, high_hz: 1_838_000, name: "160m", usage: "CW" },
    Segment { low_hz: 1_838_000, high_hz: 1_843_000, name: "160m", usage: "digital" },
    Segment { low_hz: 1_843_000, high_hz: 2_000_000, name: "160m", usage: "SSB" },
    Segment { low_hz: 3_500_000, high_hz: 3_570_000, name: "80m", usage: "CW" },
    Segment { low_hz: 3_570_000, high_hz: 3_600_000, name: "80m", usage: "digital" },
    Segment { low_hz: 3_600_000, high_hz: 3_800_000, name: "80m", usage: "SSB" },
    Segment { low_hz: 5_351_500, high_hz: 5_366_500, name: "60m", usage: "mixed" },
    Segment { low_hz: 7_000_000, high_hz: 7_040_000, name: "40m", usage: "CW" },
    Segment { low_hz: 7_040_000, high_hz: 7_060_000, name: "40m", usage: "digital" },
    Segment { low_hz: 7_060_000, high_hz: 7_200_000, name: "40m", usage: "SSB" },
    Segment { low_hz: 10_100_000, high_hz: 10_130_000, name: "30m", usage: "CW" },
    Segment { low_hz: 10_130_000, high_hz: 10_150_000, name: "30m", usage: "digital" },
    Segment { low_hz: 14_000_000, high_hz: 14_070_000, name: "20m", usage: "CW" },
    Segment { low_hz: 14_070_000, high_hz: 14_099_000, name: "20m", usage: "digital" },
    Segment { low_hz: 14_099_000, high_hz: 14_101_000, name: "20m", usage: "beacons" },
    Segment { low_hz: 14_101_000, high_hz: 14_350_000, name: "20m", usage: "SSB" },
    Segment { low_hz: 18_068_000, high_hz: 18_095_000, name: "17m", usage: "CW" },
    Segment { low_hz: 18_095_000, high_hz: 18_109_000, name: "17m", usage: "digital" },
    Segment { low_hz: 18_109_000, high_hz: 18_168_000, name: "17m", usage: "SSB" },
    Segment { low_hz: 21_000_000, high_hz: 21_070_000, name: "15m", usage: "CW" },
    Segment { low_hz: 21_070_000, high_hz: 21_150_000, name: "15m", usage: "digital" },
    Segment { low_hz: 21_150_000, high_hz: 21_450_000, name: "15m", usage: "SSB" },
    Segment { low_hz: 24_890_000, high_hz: 24_915_000, name: "12m", usage: "CW" },
    Segment { low_hz: 24_915_000, high_hz: 24_929_000, name: "12m", usage: "digital" },
    Segment { low_hz: 24_929_000, high_hz: 24_990_000, name: "12m", usage: "SSB" },
    Segment { low_hz: 28_000_000, high_hz: 28_070_000, name: "10m", usage: "CW" },
    Segment { low_hz: 28_070_000, high_hz: 28_190_000, name: "10m", usage: "digital" },
    Segment { low_hz: 28_190_000, high_hz: 28_300_000, name: "10m", usage: "beacons" },
    Segment { low_hz: 28_300_000, high_hz: 29_700_000, name: "10m", usage: "SSB" },
    Segment { low_hz: 50_000_000, high_hz: 50_100_000, name: "6m", usage: "CW" },
    Segment { low_hz: 50_100_000, high_hz: 52_000_000, name: "6m", usage: "SSB" },
    Segment { low_hz: 144_000_000, high_hz: 144_150_000, name: "2m", usage: "CW" },
    Segment { low_hz: 144_150_000, high_hz: 144_400_000, name: "2m", usage: "SSB" },
    Segment { low_hz: 144_400_000, high_hz: 148_000_000, name: "2m", usage: "FM" },
    Segment { low_hz: 430_000_000, high_hz: 440_000_000, name: "70cm", usage: "mixed" },
];

/// One band as an operator names it, with its whole extent.
#[derive(Debug, Clone, Copy)]
pub struct Band {
    pub name: &'static str,
    pub low_hz: i64,
    pub high_hz: i64,
}

impl Band {
    /// Frequency a band button lands on when nothing has been stored.
    ///
    /// A tenth of the way in, bounded at five kilohertz. On every band from
    /// eighty metres upwards that lands in the keyed section, which is where a
    /// skimmer is pointed. The fraction rather than a fixed offset is what keeps
    /// it inside the two narrow low frequency allocations, where five kilohertz
    /// would land past the top of the band.
    pub fn default_hz(&self) -> i64 {
        let width = (self.high_hz - self.low_hz).max(1);
        self.low_hz + (width / 10).min(5_000)
    }
}

/// Bands the plan lists, ascending, one entry per name.
///
/// Merged from the segments rather than stated separately. A band is what an
/// operator names and a segment is how it is divided, so the two are the same
/// list read at two resolutions; a second table would be a second thing to keep
/// in step.
pub fn bands() -> Vec<Band> {
    let mut out: Vec<Band> = Vec::with_capacity(16);
    for segment in BANDS {
        match out.iter_mut().find(|b| b.name == segment.name) {
            Some(band) => {
                band.low_hz = band.low_hz.min(segment.low_hz);
                band.high_hz = band.high_hz.max(segment.high_hz);
            }
            None => out.push(Band {
                name: segment.name,
                low_hz: segment.low_hz,
                high_hz: segment.high_hz,
            }),
        }
    }
    out
}

/// Segment a frequency falls in.
pub fn segment_of(hz: i64) -> Option<&'static Segment> {
    BANDS.iter().find(|s| hz >= s.low_hz && hz < s.high_hz)
}

pub struct Catalog {
    stations: Vec<Station>,
    path: PathBuf,
}

impl Catalog {
    /// Reads the file, writing an example when it is absent.
    ///
    /// The example is written rather than the file left missing, because a
    /// feature nobody can find is a feature nobody has. An operator who does
    /// not want markers deletes the entries or switches the setting off; one
    /// who does has a template that already parses.
    pub fn load(path: &Path) -> Catalog {
        let mut catalog = Catalog { stations: Vec::new(), path: path.to_path_buf() };

        if !path.exists() {
            if let Err(e) = Catalog::write_example(path) {
                crate::log_warn!("stations", "cannot write {}: {}", path.display(), e);
            }
            return catalog;
        }

        let document = match Ini::load(path) {
            Ok(d) => d,
            Err(e) => {
                crate::log_warn!("stations", "{}: {}", path.display(), e);
                return catalog;
            }
        };

        for name in document.section_names() {
            let hz = document.get_f64(name, "frequency", 0.0) as i64;
            if hz < MIN_HZ {
                crate::log_warn!("stations", "[{}] has no usable frequency, skipped", name);
                continue;
            }
            let label = {
                let text = document.get_string(name, "label", "");
                if text.is_empty() { name.to_string() } else { text }
            };
            catalog.stations.push(Station {
                hz,
                label,
                mode: document.get_string(name, "mode", ""),
                note: document.get_string(name, "note", ""),
            });
        }

        // Ascending, so a list reads the way a dial turns and two markers close
        // together are drawn in a stable order.
        catalog
            .stations
            .sort_by(|a, b| a.hz.cmp(&b.hz));
        crate::log_info!(
            "stations",
            "{} entries from {}",
            catalog.stations.len(),
            path.display()
        );
        catalog
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_empty(&self) -> bool {
        self.stations.is_empty()
    }

    pub fn len(&self) -> usize {
        self.stations.len()
    }

    pub fn all(&self) -> &[Station] {
        &self.stations
    }

    /// Entries inside a frequency range, in ascending order.
    ///
    /// The range comes from the two ends of the visible spectrum, so an entry
    /// outside it costs nothing: the search is a scan over a list that holds a
    /// few dozen items at most.
    ///
    /// The lifetime is named because the destination is a mutable reference and
    /// is therefore invariant over what it holds. Without a name the borrow
    /// checker takes the element lifetime to be local to the call, and the
    /// entries plainly outlive it: they belong to the catalogue.
    pub fn in_range<'a>(
        &'a self,
        low_hz: i64,
        high_hz: i64,
        into: &mut Vec<&'a Station>,
    ) {
        into.clear();
        for station in &self.stations {
            if station.hz >= low_hz && station.hz <= high_hz {
                into.push(station);
            }
        }
    }

    /// Appends an entry to the file and to the list in memory.
    ///
    /// The file is appended to rather than rewritten through the document model.
    /// It is maintained by hand, and a rewrite would reformat every comment and
    /// reorder every section the operator put in a deliberate order, which is
    /// exactly the reason the list lives outside the configuration.
    pub fn append(
        &mut self,
        hz: i64,
        label: &str,
        mode: &str,
        note: &str,
    ) -> crate::core::Result<()> {
        if hz < MIN_HZ {
            return Err(crate::core::Error::config(
                "a marker below ten kilohertz is a mistake rather than a frequency",
            ));
        }

        let label = label.trim();
        let label = if label.is_empty() {
            // The frequency itself, so an entry saved in a hurry is still
            // identifiable rather than being called nothing.
            format!("{:.2} kHz", hz as f64 / 1000.0)
        } else {
            label.to_string()
        };
        let key = self.unique_key(&label);

        if let Some(directory) = self.path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }

        let mut text = String::with_capacity(160);
        text.push_str("\r\n[");
        text.push_str(&key);
        text.push_str("]\r\nfrequency = ");
        text.push_str(&hz.to_string());
        text.push_str("\r\nlabel = ");
        text.push_str(&label);
        if !mode.is_empty() {
            text.push_str("\r\nmode = ");
            text.push_str(mode);
        }
        if !note.is_empty() {
            text.push_str("\r\nnote = ");
            text.push_str(note);
        }
        text.push_str("\r\n");

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(text.as_bytes())?;

        self.stations.push(Station {
            hz,
            label,
            mode: mode.to_string(),
            note: note.to_string(),
        });
        self.stations.sort_by(|a, b| a.hz.cmp(&b.hz));
        crate::log_info!("stations", "[{}] added at {} Hz", key, hz);
        Ok(())
    }

    /// Section name for a label, free of anything the format reads specially.
    ///
    /// Collisions are tested against the file rather than against the list. A
    /// section the operator wrote by hand may hold no usable frequency and be
    /// absent from the list, and reusing its name would replace it silently on
    /// the next read.
    fn unique_key(&self, label: &str) -> String {
        let mut base = String::with_capacity(label.len());
        for ch in label.chars() {
            if ch.is_ascii_alphanumeric() {
                base.push(ch.to_ascii_lowercase());
            } else if !base.is_empty() && !base.ends_with('-') {
                base.push('-');
            }
        }
        let trimmed = base.trim_matches('-');
        let base = if trimmed.is_empty() { "marker" } else { trimmed }.to_string();

        let taken = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut key = base.clone();
        let mut suffix = 2u32;
        while taken.contains(&format!("[{}]", key)) {
            key = format!("{}-{}", base, suffix);
            suffix += 1;
            if suffix > 999 {
                break;
            }
        }
        key
    }

    fn write_example(path: &Path) -> std::io::Result<()> {
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }
        let text = "\
; RXScope station list
;
; One section per marked frequency. The section name is the key; everything
; except the frequency may be left out.
;
; Markers are drawn on the spectrum when the transceiver reports a dial
; frequency, because without one there is nothing to convert an audio position
; into. Switch them off under [waterfall] show_stations.

[example-beacon]
frequency = 14100000
label = 4U1UN
mode = CW
note = IBP beacon

[example-net]
frequency = 7130000
label = net
mode = SSB
note = 0900 local
";
        std::fs::write(path, text)
    }
}