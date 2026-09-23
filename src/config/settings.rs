//! Complete settings tree.
//!
//! Every subsystem owns one section. Sections implement load and store so a
//! new option is added in exactly one place, and the generated INI carries
//! inline documentation for the operator.
//!
//! Values are validated on load. Anything that could destabilize the DSP
//! chain, such as an FFT size that is not a power of two, is clamped and
//! reported.

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

/// Capture API.
///
/// The direction is not part of this: WASAPI exposes both input endpoints and
/// output endpoints, the latter readable through the loopback path, and which
/// one is used follows from the selected device rather than from a separate
/// mode. The trait is implemented by hand instead of through the macro so the
/// identifier written by earlier builds keeps loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
    Wasapi,
    WaveIn,
}

impl ConfigEnum for AudioBackend {
    fn from_config(text: &str) -> Option<Self> {
        match text {
            // The loopback spelling used to be a backend of its own.
            "wasapi" | "wasapi_loopback" => Some(AudioBackend::Wasapi),
            // The misspelling shipped in an earlier build.
            "wavein" | "waveein" => Some(AudioBackend::WaveIn),
            _ => None,
        }
    }

    fn to_config(&self) -> &'static str {
        match self {
            AudioBackend::Wasapi => "wasapi",
            AudioBackend::WaveIn => "wavein",
        }
    }

    fn variants() -> &'static [&'static str] {
        &["wasapi", "wavein"]
    }
}

impl Default for AudioBackend {
    fn default() -> Self {
        AudioBackend::Wasapi
    }
}

config_enum!(ChannelMode {
    Left => "left",
    Right => "right",
    Mix => "mix",
    Difference => "difference",
} default Left);

config_enum!(WindowFn {
    Rectangular => "rectangular",
    Hann => "hann",
    Hamming => "hamming",
    Blackman => "blackman",
    BlackmanHarris => "blackman_harris",
    Nuttall => "nuttall",
    FlatTop => "flattop",
    Kaiser => "kaiser",
} default BlackmanHarris);

config_enum!(ColorMap {
    Grayscale => "grayscale",
    BlueSteel => "blue_steel",
    Inferno => "inferno",
    Viridis => "viridis",
    Turbo => "turbo",
} default BlueSteel);

config_enum!(WaterfallStyle {
    Classic => "classic",
    Skimmer => "skimmer",
} default Classic);

/// What follows the dial when it moves.
///
/// The audio span is centred on the dial by construction, so a retune moves
/// every station to a different audio frequency. Three answers, and the last two
/// differ only in whether the view moves with the record.
config_enum!(AnchorMode {
    Off => "off",
    Audio => "audio",
    Band => "band",
} default Audio);

/// Shape of an interface animation.
///
/// One curve rather than one per direction. The phase moves linearly in both
/// directions and the curve is applied to it, so a curve that is fast at the
/// start and gentle at the end reads that way on the way in and reads as the
/// mirror on the way out. That pair is the correct one: the first pixels of
/// motion are what tell the operator the press registered, so a slow start
/// reads as a stall, while a dismissal may accelerate away.
///
/// A slow start on appearance is therefore not offered. It would be a setting
/// that can only be set wrong.
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
            // Cubic rather than quadratic: it leaves the start decisively and
            // settles without the last tenth of the travel being visible.
            AnimCurve::EaseOut => {
                let u = 1.0 - t;
                1.0 - u * u * u
            }
            // Smoothstep, which is its own mirror, so this one is symmetric in
            // both directions.
            AnimCurve::EaseInOut => t * t * (3.0 - 2.0 * t),
        }
    }
}

/// How a selected tab is marked.
///
/// Two idioms exist and using both at once is what makes a strip look
/// unfinished: an underline states the selection with one mark, an attached
/// tab states it by continuing the surface below. Either is defensible, the
/// pair is not.
config_enum!(TabStyle {
    Underline => "underline",
    Attached => "attached",
} default Underline);

config_enum!(PanelSection {
    Rig => "rig",
    Receiver => "receiver",
    Meter => "meter",
    Audio => "audio",
    Monitor => "monitor",
    Spectrum => "spectrum",
    Waterfall => "waterfall",
    CwChannels => "cw_channels",
    CwDecoder => "cw_decoder",
    RttyDecoder => "rtty_decoder",
    PskDecoder => "psk_decoder",
    Classifier => "classifier",
    Callsign => "callsign",
    Spots => "spots",
} default Meter);

config_enum!(MeterScale {
    SUnits => "s_units",
    Dbm => "dbm",
    DbFs => "dbfs",
} default SUnits);

config_enum!(TextCase {
    Upper => "upper",
    Lower => "lower",
    AsReceived => "as_received",
} default Upper);

config_enum!(RttyAlphabet {
    Baudot => "baudot",
    Ascii => "ascii",
} default Baudot);

config_enum!(RttyParity {
    None => "none",
    Even => "even",
    Odd => "odd",
    Mark => "mark",
    Space => "space",
} default None);

config_enum!(CallsignSource {
    LocalFile => "local_file",
    Cty => "cty",
    None => "none",
} default Cty);

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

config_enum!(MonitorWidth {
    Detector => "detector",
    Independent => "independent",
} default Detector);

/// How the port is reached.
///
/// Directly means this application holds it, which is what a single application
/// wants and what costs nothing. Through the service means one exchange loop
/// serves several applications, which is the only way a logging program and this
/// one can address the same transceiver: a serial port carries one conversation
/// and two programs sending the same request produce two identical replies that
/// nothing can tell apart.
config_enum!(RigTransport {
    Direct => "direct",
    Shared => "shared",
} default Direct);

/// State a modem control line is held in.
///
/// Low by default and the reason is safety rather than convention: on many
/// interface cables one of these keys the transmitter, so a port opened with
/// them asserted would transmit before the operator had decided anything.
config_enum!(RigLine {
    Low => "low",
    High => "high",
} default Low);

/// Which side of the dial the audio spectrum sits on.
///
/// Derived from the mode the transceiver reports whenever it reports one. The
/// two explicit settings exist for a receiver whose description cannot read the
/// mode, and for a converter that inverts the spectrum.
config_enum!(SidebandMode {
    Auto => "auto",
    Upper => "upper",
    Lower => "lower",
} default Auto);

/// What the application is being used as.
///
/// The two are not variations of one arrangement, they are different
/// instruments. Recognising keying wants a detector per carrier, a threshold
/// derived from the envelope statistics and no gain loop anywhere near the
/// signal. Listening to one signal wants a filter, a demodulator, noise
/// reduction and a gain loop. Several of the second set actively destroy the
/// first, which is why the mode exists at all rather than a row of independent
/// switches.
config_enum!(OperatingMode {
    Skimmer => "skimmer",
    Sdr => "sdr",
} default Skimmer);

/// Demodulator used in the receiver mode.
///
/// Independent of what the transceiver is set to. The two are coupled by the
/// setting below when the operator asks for it, and are otherwise separate:
/// a receiver fed I/Q demodulates for itself, and a receiver fed audio still
/// needs a mode to derive its filter and its decoder gating from.
config_enum!(Detector {
    Cw => "cw",
    Usb => "usb",
    Lsb => "lsb",
    DigU => "dig_u",
    DigL => "dig_l",
    Am => "am",
    Sam => "sam",
    Fm => "fm",
} default Usb);

/// Which noise reduction runs.
///
/// The two methods do different things and neither is better in general.
///
/// A predictor separates the predictable part of a signal from the
/// unpredictable part and keeps the first. A steady tone is predictable and
/// noise is not, so on keying and on data it works and costs nothing in
/// latency. Speech is not predictable over the horizon a short filter sees, so
/// on voice it removes the speech along with the noise.
///
/// Spectral subtraction estimates the noise floor per frequency and removes it,
/// which is what voice needs. It costs one block of latency and produces
/// musical noise when pushed, which is the characteristic failure of the method
/// and the reason many operators switch it off.
///
/// Automatic chooses from the detector, because the detector already states
/// which kind of signal is being received. That is a decision the operator has
/// made once and should not have to make twice.
config_enum!(NrMethod {
    Auto => "auto",
    Predictor => "predictor",
    Spectral => "spectral",
} default Auto);

config_enum!(ModeLink {
    Independent => "independent",
    Follow => "follow",
    Drive => "drive",
    Both => "both",
} default Follow);

/// Sample layout inside a ring segment.
///
/// Integer by default. A sound card input carries sixty to seventy decibels of
/// usable range and sixteen bits hold ninety six, so nothing measurable is
/// lost, and the file is half the size of the float one. Float exists for a
/// receiver that genuinely delivers more, and for a session where the recording
/// is the measurement rather than a note of it.
config_enum!(RecordFormat {
    I16 => "i16",
    F32 => "f32",
} default I16);

/// Container an export is written into.
///
/// Three, because they answer three different questions and none of them
/// answers two.
///
/// Uncompressed opens everywhere and is what a second tool expects. Lossless
/// halves the size and keeps every sample, which is what archiving a signal
/// worth re-decoding needs. Lossy is a quarter of the size and destroys the
/// weak signals first, which makes it right for sending a recording to somebody
/// and wrong for keeping one.
config_enum!(ExportFormat {
    Wav => "wav",
    Lossless => "lossless",
    Qoa => "qoa",
} default Wav);

impl Detector {
    /// Filter edges the mode is normally worked with, in hertz.
    ///
    /// Applied when the mode changes, which is what makes a mode selection do
    /// something visible rather than merely rename the detector. The operator
    /// may move either edge afterwards and the preset is not reimposed until
    /// the mode changes again.
    ///
    /// The lower edge sits at nought for every mode that has no reason to
    /// exclude the carrier region. An earlier arrangement held it above nought
    /// to keep the offset of the sound card out of the passband, but the
    /// capture front end removes that offset before anything downstream sees
    /// it, so the reason had already gone. On a quadrature input the honest
    /// upper sideband is everything above the carrier; the three hundred hertz
    /// gap is a convention borrowed from a crystal filter that had to be offset
    /// and that this chain does not have.
    ///
    /// The two inputs still need different presets, and not by a little.
    ///
    /// Demodulated audio from a transceiver is entirely positive: the
    /// transceiver already chose the sideband and delivered the result at
    /// baseband, so both sidebands come out as the same positive band and there
    /// is nothing below nought to pass. The edges are absolute audio.
    ///
    /// A complex signal is centred on the tuning point and genuinely has two
    /// halves, so the edges are measured from that point and the lower sideband
    /// lives below it. Keying is symmetric there, because the tuning point is
    /// the signal itself; what makes it audible is the beat oscillator rather
    /// than an offset in the filter.
    pub fn filter_preset(self, complex_input: bool) -> (f32, f32) {
        if !complex_input {
            return match self {
                // Narrow enough to separate two stations a hundred hertz apart,
                // wide enough not to ring on the keying it is passing, and
                // placed where a transceiver puts its sidetone.
                Detector::Cw => (400.0, 1000.0),
                Detector::Usb | Detector::DigU | Detector::Lsb | Detector::DigL => {
                    (0.0, 2700.0)
                }
                Detector::Am | Detector::Sam => (0.0, 4000.0),
                Detector::Fm => (0.0, 6000.0),
            };
        }

        match self {
            // Symmetric about the tuning point. A beat oscillator supplies the
            // pitch, which is what a receiver with an intermediate frequency
            // does and what keeps the readout naming the signal rather than
            // naming a point six hundred hertz below it.
            Detector::Cw => (-300.0, 300.0),
            Detector::Usb | Detector::DigU => (0.0, 2700.0),
            Detector::Lsb | Detector::DigL => (-2700.0, 0.0),
            // Double sideband, so the passband is symmetric about the carrier.
            Detector::Am | Detector::Sam => (-4000.0, 4000.0),
            Detector::Fm => (-6000.0, 6000.0),
        }
    }

    /// True when the mode selects one half of a two sided spectrum.
    ///
    /// Only such a mode is affected by which half a complex input carries, and
    /// only such a mode collapses onto its opposite on a real input.
    pub fn is_sideband(self) -> bool {
        matches!(
            self,
            Detector::Cw | Detector::Usb | Detector::Lsb | Detector::DigU | Detector::DigL
        )
    }

    /// True when the mode places its passband below nought on a complex input.
    pub fn is_lower_sideband(self) -> bool {
        matches!(self, Detector::Lsb | Detector::DigL)
    }

    /// True when the mode is keyed and therefore needs a beat oscillator on a
    /// complex input.
    pub fn is_keyed(self) -> bool {
        matches!(self, Detector::Cw)
    }

    /// True when the mode needs a carrier the receiver has not already removed.
    ///
    /// Amplitude and frequency detection operate on a modulated carrier. Audio
    /// from a transceiver has none: the transceiver detected it already, and
    /// detecting the result a second time produces noise rather than a signal.
    pub fn needs_carrier(self) -> bool {
        matches!(self, Detector::Am | Detector::Sam | Detector::Fm)
    }
}

trait SectionIo: Sized + Default {
    const NAME: &'static str;
    fn load(ini: &Ini) -> Self;
    fn store(&self, ini: &mut Ini);
}

impl PanelSection {
    /// Localization key of the group title.
    pub fn key(self) -> &'static str {
        match self {
            PanelSection::Rig => "group.rig",
            PanelSection::Receiver => "group.receiver",
            PanelSection::Meter => "group.meter",
            PanelSection::Audio => "group.audio",
            PanelSection::Monitor => "group.monitor",
            PanelSection::Spectrum => "group.spectrum",
            PanelSection::Waterfall => "group.waterfall",
            PanelSection::CwChannels => "group.cw_channels",
            PanelSection::CwDecoder => "group.cw_decoder",
            PanelSection::RttyDecoder => "group.rtty_decoder",
            PanelSection::PskDecoder => "group.psk_decoder",
            PanelSection::Classifier => "group.classifier",
            PanelSection::Callsign => "group.callsign",
            PanelSection::Spots => "group.spots",
        }
    }

    /// Every section the build knows, in declaration order.
    pub fn all() -> Vec<PanelSection> {
        PanelSection::variants()
            .iter()
            .filter_map(|name| PanelSection::from_config(name))
            .collect()
    }
}

/// Composable tabs. The settings tab is deliberately absent: it carries the
/// controls that configure everything else, including this list, so a layout
/// that removed it would be unrecoverable from inside the application.
pub const PANEL_TABS: usize = 4;

#[derive(Debug, Clone)]
pub struct PanelSettings {
    /// One ordered list per composable tab, indexed by the tab position.
    pub tabs: [Vec<PanelSection>; PANEL_TABS],
    /// Sections this configuration has already seen.
    ///
    /// Without it there is no way to tell a section the operator removed from
    /// one a later build added, because both are absent from every tab. The
    /// first must stay removed and the second must appear, and those are
    /// opposite actions on the same evidence.
    known: Vec<PanelSection>,
}

impl Default for PanelSettings {
    fn default() -> Self {
        use PanelSection::*;
        PanelSettings {
            // Every section appears exactly once. A section in two tabs shares
            // its settings between them, which is legal and confusing: the same
            // control in two places invites the belief that they are two
            // settings, and the tab names stop meaning anything.
            tabs: [
                // Receive: the radio and what it is doing to one signal.
                vec![Rig, Receiver, Meter],
                // Audio: sound coming in and sound going out.
                vec![Audio, Monitor],
                // Decode: everything that reads text out of the air.
                vec![
                    Spots,
                    CwChannels,
                    CwDecoder,
                    RttyDecoder,
                    PskDecoder,
                    Classifier,
                    Callsign,
                ],
                // Display: how the picture is drawn.
                vec![Spectrum, Waterfall],
            ],
            known: PanelSection::all(),
        }
    }
}

impl PanelSettings {
    /// Keys the four lists are stored under.
    const KEYS: [&'static str; PANEL_TABS] = ["receive", "audio", "decode", "display"];

    pub fn sections(&self, tab: usize) -> &[PanelSection] {
        self.tabs.get(tab).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn sections_mut(&mut self, tab: usize) -> Option<&mut Vec<PanelSection>> {
        self.tabs.get_mut(tab)
    }

    /// Restores one tab to the composition the build ships with.
    ///
    /// The editor can empty a tab, and an empty tab offers no way to tell
    /// whether the composition is deliberate or lost. A restore per tab is the
    /// smallest action that always leads back to a known state.
    pub fn reset_tab(&mut self, tab: usize) {
        let defaults = PanelSettings::default();
        if let (Some(target), Some(source)) = (self.tabs.get_mut(tab), defaults.tabs.get(tab)) {
            *target = source.clone();
        }
    }

    /// Places sections the configuration has never seen.
    ///
    /// A section added by a later build reaches no tab, and nothing in the
    /// interface reveals that it exists. It is placed where the defaults hold
    /// it, which is the one position that is not arbitrary.
    ///
    /// A section the operator removed is equally absent, and is left alone: it
    /// appears in the seen list, so the two cases are distinguished by evidence
    /// rather than by guessing which is more likely.
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
        let mut result =
            PanelSettings { tabs: Default::default(), known: Vec::new() };
        let mut stored = false;

        for (index, key) in Self::KEYS.iter().enumerate() {
            let raw = ini.get_list(Self::NAME, key);
            if raw.is_empty() {
                // An absent key means the file predates this tab, so the
                // default composition stands for it.
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
                    None => {
                        crate::log_warn!("config", "[panel] {}: unknown section '{}'", key, name);
                    }
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
        // A file written before the seen list existed states it implicitly:
        // whatever is on a tab has been seen, and nothing else has.
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
        ini.comment(s, "Sections: rig, receiver, meter, audio, monitor, spectrum, waterfall,");
        ini.comment(s, "cw_channels, cw_decoder, rtty_decoder, psk_decoder, classifier,");
        ini.comment(s, "callsign, spots.");
        ini.comment(s, "Edited from the settings tab.");
        for (index, key) in Self::KEYS.iter().enumerate() {
            let names: Vec<String> = self.tabs[index]
                .iter()
                .map(|v| v.to_config().to_string())
                .collect();
            ini.set_list(s, key, &names);
        }

        // Everything on a tab has been seen by definition; the rest of the list
        // is what was removed on purpose.
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

// ---------------------------------------------------------------- bands

/// Where the operator last was on each band.
///
/// A band stack, which every transceiver front panel and every logging program
/// has, and which an operator reaches for without thinking: press the band and
/// be back where you left it, in the mode you were using. Without one a band
/// change is a frequency to remember and a mode to reselect, and the operator
/// remembers neither after the third band.
///
/// The mode is part of the entry rather than left alone. An operator works one
/// band in keying and the next in voice, and a stack that restored the frequency
/// and not the mode would put them on the right frequency listening through the
/// wrong filter.
#[derive(Debug, Clone, Default)]
pub struct BandSettings {
    pub entries: Vec<BandEntry>,
}

#[derive(Debug, Clone)]
pub struct BandEntry {
    /// Band name as the plan states it, which is what a button carries.
    pub band: String,
    pub hz: i64,
    pub detector: Detector,
}

impl BandSettings {
    pub fn find(&self, band: &str) -> Option<&BandEntry> {
        self.entries.iter().find(|e| e.band.eq_ignore_ascii_case(band))
    }

    /// Records where the operator was on a band.
    pub fn remember(&mut self, band: &str, hz: i64, detector: Detector) {
        if hz <= 0 || band.is_empty() {
            return;
        }
        match self.entries.iter_mut().find(|e| e.band.eq_ignore_ascii_case(band)) {
            Some(entry) => {
                entry.hz = hz;
                entry.detector = detector;
            }
            None => self.entries.push(BandEntry {
                band: band.to_string(),
                hz,
                detector,
            }),
        }
    }
}

impl SectionIo for BandSettings {
    const NAME: &'static str = "bands";

    fn load(ini: &Ini) -> Self {
        let mut out = BandSettings::default();
        // One list rather than one key per band. The set of bands is stated by
        // the plan rather than by this file, so a key per band would mean this
        // code holding a second copy of that set.
        for item in ini.get_list(Self::NAME, "stack") {
            let mut parts = item.split(':');
            let band = parts.next().unwrap_or("").trim().to_string();
            let hz: i64 = parts.next().unwrap_or("").trim().parse().unwrap_or(0);
            let detector = parts
                .next()
                .and_then(|d| Detector::from_config(d.trim().to_ascii_lowercase().as_str()))
                .unwrap_or_default();
            if band.is_empty() || hz <= 0 {
                crate::log_warn!("config", "[bands] stack: '{}' is not an entry", item);
                continue;
            }
            out.entries.push(BandEntry { band, hz, detector });
        }
        out
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Band stack: where the operator last was on each band and in which");
        ini.comment(s, "mode, written as band:frequency:mode. Maintained by the band buttons;");
        ini.comment(s, "editing it by hand works as well, and a band absent from it lands a");
        ini.comment(s, "tenth of the way in.");
        let items: Vec<String> = self
            .entries
            .iter()
            .map(|e| format!("{}:{}:{}", e.band, e.hz, e.detector.to_config()))
            .collect();
        ini.set_list(s, "stack", &items);
    }
}

// ----------------------------------------------------------- appearance

/// Visual style.
///
/// Held apart from the interface section because the two answer different
/// questions. The interface section decides how large the window is and which
/// font it uses; this one decides how the same widgets look and how much of the
/// drawing is chrome rather than data.
///
/// Every entry here is a choice rather than a requirement, which is why they
/// are settings at all: the skeleton of the layout is not configurable and the
/// decoration is.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceSettings {
    /// Draw the window frame and the caption with the same style as the rest.
    ///
    /// The system frame is drawn by the compositor in the system palette, so it
    /// never matches a dark instrument panel and cannot be made to. Removing it
    /// costs a hit test and a row of buttons.
    pub custom_frame: bool,
    /// Height of the caption strip, in logical units. Also the toolbar height,
    /// because the two are the same row.
    pub caption_height: f32,
    /// Outline the widget that holds keyboard focus.
    pub focus_ring: bool,
    /// Let the accent mark hover and press as well as focus and data.
    ///
    /// Off by default. The accent already marks the trace, the meter, the
    /// markers and the frequency; adding hover to that list means it marks
    /// nothing in particular, and it makes the panel flicker as the pointer
    /// crosses it.
    pub accent_hover: bool,
    /// Accent stripe on the leading edge of every group header.
    pub group_tick: bool,
    pub tab_style: TabStyle,
    /// Animate the transitions the interface has.
    ///
    /// A list revealing itself, a switch travelling, the tab bar moving between
    /// two positions. Off snaps every one of them, which is what an operator who
    /// wants no motion means: one switch rather than one per element, because two
    /// independent animation systems drift apart.
    pub animate: bool,
    /// Duration of one transition, in milliseconds.
    pub anim_ms: f32,
    pub anim_curve: AnimCurve,
    /// Size of a hint line, as a fraction of the interface font.
    pub hint_scale: f32,
    /// Align the numeric readouts of one group into a column.
    pub value_column: bool,
    /// Type a value into the numeric cell of a slider.
    ///
    /// Dragging resolves to the width of the track, which on a span of twelve
    /// kilohertz is eighty hertz per pixel: a stated value cannot be reached by
    /// pointing at all, only approached.
    pub numeric_entry: bool,
    /// Opacity of the shade drawn under an open list, nought disabling it.
    ///
    /// States modality without motion. An open list otherwise hangs over a panel
    /// with nothing to say that the panel beneath it takes no presses.
    pub popup_shade: f32,
    /// Mark a folded group that holds something switched on.
    ///
    /// A folded group hides whether anything inside it is running, which turns
    /// folding from a summary into concealment.
    pub group_activity: bool,
    /// Move focus with the keyboard.
    ///
    /// Tab and shift tab walk the controls in declaration order, the arrows step
    /// a slider by the quantum it displays, space and enter operate a switch or a
    /// button. Without it a panel of two hundred controls is reachable by pointer
    /// alone, which means it is unreachable while one hand is on the transceiver.
    ///
    /// A switch rather than a fixed capability, because space on a focused
    /// control competes with space for the replay transport: an operator who uses
    /// the second and not the first is entitled to say so.
    pub keyboard_focus: bool,
    /// Three short marks on a splitter, so it reads as a handle.
    pub splitter_grip: bool,
    /// Opacity of a separator hairline.
    pub separator_alpha: f32,

    /// Inset of the scrolling side panel from its own edges.
    pub panel_margin: f32,
    /// Inset of the content of a group from the box around it.
    pub group_padding: f32,
    pub row_height: f32,
    pub gap: f32,

    /// Background of the spectrum, the waterfall and their axis gutters.
    pub data_background_rgb: u32,
    /// Opacity of a grid line that is not a major one.
    pub grid_minor_alpha: f32,
    /// Every n-th grid line is drawn at full strength and carries the label.
    pub grid_major_every: u32,
    /// Reserve strips for the axis labels instead of drawing them over the data.
    pub axis_gutters: bool,
    pub axis_gutter_left: f32,
    pub axis_gutter_bottom: f32,
    /// Vertical line under the pointer, across the spectrum and the waterfall.
    pub crosshair: bool,
    /// Shade the area under the live trace.
    pub trace_fill: bool,
    pub trace_fill_alpha: f32,
    pub trace_thickness: f32,

    /// Draw the meter as discrete blocks rather than one bar.
    pub meter_segmented: bool,
    pub meter_segment_px: f32,
    pub meter_segment_gap_px: f32,
    /// Print the S unit values under the tick marks.
    pub meter_scale_labels: bool,
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
            // Long enough to be a movement rather than a jump, short enough that
            // a list is fully open before the pointer has travelled to a row.
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
            // Darker than the panel by a clear margin, so the palette of the
            // waterfall has room at its bottom end.
            data_background_rgb: 0x0A0A0C,
            grid_minor_alpha: 0.45,
            grid_major_every: 5,
            axis_gutters: true,
            axis_gutter_left: 34.0,
            axis_gutter_bottom: 14.0,
            crosshair: true,
            trace_fill: true,
            trace_fill_alpha: 0.22,
            trace_thickness: 1.0,
            meter_segmented: false,
            meter_segment_px: 4.0,
            meter_segment_gap_px: 1.0,
            meter_scale_labels: true,
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
            axis_gutter_left: ini.get_f32_clamped(Self::NAME, "axis_gutter_left", d.axis_gutter_left, 0.0, 90.0),
            axis_gutter_bottom: ini.get_f32_clamped(Self::NAME, "axis_gutter_bottom", d.axis_gutter_bottom, 0.0, 40.0),
            crosshair: ini.get_bool(Self::NAME, "crosshair", d.crosshair),
            trace_fill: ini.get_bool(Self::NAME, "trace_fill", d.trace_fill),
            trace_fill_alpha: ini.get_f32_clamped(Self::NAME, "trace_fill_alpha", d.trace_fill_alpha, 0.0, 0.8),
            trace_thickness: ini.get_f32_clamped(Self::NAME, "trace_thickness", d.trace_thickness, 1.0, 4.0),
            meter_segmented: ini.get_bool(Self::NAME, "meter_segmented", d.meter_segmented),
            meter_segment_px: ini.get_f32_clamped(Self::NAME, "meter_segment_px", d.meter_segment_px, 2.0, 12.0),
            meter_segment_gap_px: ini.get_f32_clamped(Self::NAME, "meter_segment_gap_px", d.meter_segment_gap_px, 0.0, 6.0),
            meter_scale_labels: ini.get_bool(Self::NAME, "meter_scale_labels", d.meter_scale_labels),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Visual style. Nothing here changes what the application measures,");
        ini.comment(s, "only how much of the drawing is chrome and how it is marked.");
        ini.comment(s, "custom_frame replaces the system caption with one in this palette.");
        ini.set_bool(s, "custom_frame", self.custom_frame);
        ini.set_f32(s, "caption_height", self.caption_height);
        ini.set_bool(s, "focus_ring", self.focus_ring);
        ini.comment(s, "accent_hover lets the accent mark hover and press as well. Off keeps");
        ini.comment(s, "the accent for data and focus, which is what makes it mean anything.");
        ini.set_bool(s, "accent_hover", self.accent_hover);
        ini.set_bool(s, "group_tick", self.group_tick);
        ini.comment(s, "tab_style: underline or attached. Using both at once is what makes a");
        ini.comment(s, "strip look unfinished.");
        ini.set_enum(s, "tab_style", self.tab_style);
        ini.comment(s, "animate covers the list reveal, the switch travel and the tab bar.");
        ini.comment(s, "The curve is applied to a phase that moves linearly both ways, so an");
        ini.comment(s, "ease_out reveal is an ease_in dismissal: the first pixels of motion are");
        ini.comment(s, "what say the press registered, and a slow start reads as a stall.");
        ini.set_bool(s, "animate", self.animate);
        ini.set_f32(s, "anim_ms", self.anim_ms);
        ini.set_enum(s, "anim_curve", self.anim_curve);
        ini.set_f32(s, "hint_scale", self.hint_scale);
        ini.comment(s, "value_column aligns the numeric readouts of one group.");
        ini.set_bool(s, "value_column", self.value_column);
        ini.comment(s, "numeric_entry lets a slider value be typed: double click the number.");
        ini.comment(s, "Dragging resolves to the track width, so a stated value can otherwise");
        ini.comment(s, "only be approached.");
        ini.set_bool(s, "numeric_entry", self.numeric_entry);
        ini.comment(s, "popup_shade dims the panel under an open list, which is what says the");
        ini.comment(s, "panel takes no presses. Nought disables it.");
        ini.set_f32(s, "popup_shade", self.popup_shade);
        ini.comment(s, "group_activity marks a folded group that holds something switched on.");
        ini.set_bool(s, "group_activity", self.group_activity);
        ini.comment(s, "keyboard_focus lets tab walk the controls, the arrows step a slider and");
        ini.comment(s, "space operate one. Off returns space to the replay transport.");
        ini.set_bool(s, "keyboard_focus", self.keyboard_focus);
        ini.set_bool(s, "splitter_grip", self.splitter_grip);
        ini.set_f32(s, "separator_alpha", self.separator_alpha);
        ini.comment(s, "Density. panel_margin is the inset of the side panel, group_padding");
        ini.comment(s, "the inset of the content inside a group box.");
        ini.set_f32(s, "panel_margin", self.panel_margin);
        ini.set_f32(s, "group_padding", self.group_padding);
        ini.set_f32(s, "row_height", self.row_height);
        ini.set_f32(s, "gap", self.gap);
        ini.comment(s, "Data area. axis_gutters reserves strips for the labels rather than");
        ini.comment(s, "drawing them over the trace, which is what makes it read as an");
        ini.comment(s, "instrument rather than as an illustration.");
        ini.set_string(s, "data_background_rgb", &format!("{:06X}", self.data_background_rgb));
        ini.set_f32(s, "grid_minor_alpha", self.grid_minor_alpha);
        ini.set_u32(s, "grid_major_every", self.grid_major_every);
        ini.set_bool(s, "axis_gutters", self.axis_gutters);
        ini.set_f32(s, "axis_gutter_left", self.axis_gutter_left);
        ini.set_f32(s, "axis_gutter_bottom", self.axis_gutter_bottom);
        ini.set_bool(s, "crosshair", self.crosshair);
        ini.set_bool(s, "trace_fill", self.trace_fill);
        ini.set_f32(s, "trace_fill_alpha", self.trace_fill_alpha);
        ini.set_f32(s, "trace_thickness", self.trace_thickness);
        ini.comment(s, "meter_segmented draws discrete blocks. A solid bar is the default");
        ini.comment(s, "because it resolves a small change that a block cannot.");
        ini.set_bool(s, "meter_segmented", self.meter_segmented);
        ini.set_f32(s, "meter_segment_px", self.meter_segment_px);
        ini.set_f32(s, "meter_segment_gap_px", self.meter_segment_gap_px);
        ini.set_bool(s, "meter_scale_labels", self.meter_scale_labels);
    }
}

// ---------------------------------------------------------------- record

/// Cyclic recording.
///
/// The audio is taken at the decoder rate rather than at the device rate, which
/// fixes two things a device rate recording cannot. The rate is stable across a
/// change of capture device, so a segment recorded yesterday plays back through
/// the same chain today. And the pair is still a pair, so a quadrature input
/// survives into the file and the replay can show both halves of the spectrum.
///
/// Segments rather than one wrapping file. A crash then costs one segment
/// instead of the whole ring, the oldest is discarded with a file delete rather
/// than a rewrite, and the operator can copy one out without a tool.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSettings {
    pub enabled: bool,
    /// Directory the segments live in, relative to the configuration file.
    pub path: String,
    pub segment_seconds: u32,
    /// Total the directory may occupy. The oldest segments are deleted once it
    /// is exceeded, which is what makes the recording cyclic.
    pub budget_mb: u32,
    pub format: RecordFormat,
    /// Duration of one block, which is also the granularity a replay can seek
    /// to and the interval the dial frequency is sampled at.
    ///
    /// Half a second: fine enough that scrubbing lands where the pointer was
    /// aimed, coarse enough that the block markers are a per cent of the file
    /// rather than a tenth of it.
    pub block_seconds: f32,
    /// Begin recording as soon as capture starts.
    pub auto_start: bool,

    pub export_format: ExportFormat,
    pub export_path: String,
    /// Bits per sample of an uncompressed export. Thirty two means float.
    pub export_bits: u32,
}

impl Default for RecordSettings {
    fn default() -> Self {
        RecordSettings {
            enabled: false,
            path: "recordings".to_string(),
            segment_seconds: 60,
            budget_mb: 2048,
            format: RecordFormat::I16,
            block_seconds: 0.5,
            auto_start: false,
            export_format: ExportFormat::Wav,
            export_path: "exports".to_string(),
            export_bits: 16,
        }
    }
}

impl SectionIo for RecordSettings {
    const NAME: &'static str = "record";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        RecordSettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            path: ini.get_string(Self::NAME, "path", &d.path),
            segment_seconds: ini.get_u32_clamped(Self::NAME, "segment_seconds", d.segment_seconds, 5, 3600),
            budget_mb: ini.get_u32_clamped(Self::NAME, "budget_mb", d.budget_mb, 16, 1_000_000),
            format: ini.get_enum(Self::NAME, "format", d.format),
            block_seconds: ini.get_f32_clamped(Self::NAME, "block_seconds", d.block_seconds, 0.05, 5.0),
            auto_start: ini.get_bool(Self::NAME, "auto_start", d.auto_start),
            export_format: ini.get_enum(Self::NAME, "export_format", d.export_format),
            export_path: ini.get_string(Self::NAME, "export_path", &d.export_path),
            export_bits: ini.get_u32_clamped(Self::NAME, "export_bits", d.export_bits, 16, 32),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Cyclic recording. The audio is taken at the decoder rate, so a");
        ini.comment(s, "segment plays back through the same chain whatever device recorded");
        ini.comment(s, "it, and a quadrature pair survives into the file.");
        ini.set_bool(s, "enabled", self.enabled);
        ini.set_bool(s, "auto_start", self.auto_start);
        ini.comment(s, "A relative path is taken from the directory of the configuration.");
        ini.set_string(s, "path", &self.path);
        ini.comment(s, "Segments rather than one wrapping file: a crash costs one segment,");
        ini.comment(s, "and the oldest is discarded with a delete rather than a rewrite.");
        ini.set_u32(s, "segment_seconds", self.segment_seconds);
        ini.comment(s, "budget_mb is the whole directory. Once past it the oldest segments");
        ini.comment(s, "are deleted, which is what makes the recording cyclic.");
        ini.set_u32(s, "budget_mb", self.budget_mb);
        ini.comment(s, "format: i16 holds ninety six decibels, which is more than a sound");
        ini.comment(s, "card input carries, and is half the size of f32.");
        ini.set_enum(s, "format", self.format);
        ini.comment(s, "block_seconds is also the seek granularity and the interval the dial");
        ini.comment(s, "frequency is sampled at.");
        ini.set_f32(s, "block_seconds", self.block_seconds);
        ini.comment(s, "export_format: wav opens everywhere, lossless halves the size and");
        ini.comment(s, "keeps every sample, qoa is a quarter of the size and loses the weak");
        ini.comment(s, "signals first.");
        ini.set_enum(s, "export_format", self.export_format);
        ini.set_string(s, "export_path", &self.export_path);
        ini.comment(s, "export_bits applies to wav alone. Thirty two means float.");
        ini.set_u32(s, "export_bits", self.export_bits);
    }
}

// ----------------------------------------------------------------- rig

#[derive(Debug, Clone)]
pub struct RigSettings {
    pub enabled: bool,
    /// Name of a description file, without the extension.
    pub profile: String,
    /// Directory the descriptions are read from. A relative path is resolved
    /// against the directory of the executable, so a shortcut started from
    /// anywhere finds the same files as a double click.
    pub profiles_path: String,
    pub port: String,
    pub baud: u32,
    pub transport: RigTransport,
    pub dtr: RigLine,
    pub rts: RigLine,
    /// Which side of the dial the audio spectrum sits on.
    pub sideband: SidebandMode,
    /// Sidetone pitch assumed when the transceiver cannot report one.
    ///
    /// A keyed transceiver places the dial on the signal rather than on the
    /// carrier, so a station exactly on the dial frequency is heard at this
    /// pitch. Several descriptions cannot read it, and a wrong value moves every
    /// frequency reading by the difference.
    pub cw_pitch_hz: f32,
    /// Correction added to every frequency reading.
    ///
    /// For a converter, a transverter, or a description that reports an
    /// oscillator other than the one in use. None of those is discoverable from
    /// here, so the operator states it.
    pub offset_hz: f32,
    /// Show the frequency over the display.
    pub show_readout: bool,
    /// Size of the readout, as a multiple of the interface font.
    pub readout_scale: f32,
    /// Show the last digit, giving a resolution of one hertz rather than ten.
    pub readout_fine: bool,
    /// Repeat while a readout digit is held.
    ///
    /// A front panel does. Without it, crossing a kilohertz by the hundred hertz
    /// digit is ten separate presses, and by the ten hertz digit a hundred.
    pub readout_repeat: bool,
    /// Label the spectrum in frequencies on the air rather than in audio.
    pub rf_axis: bool,
    /// Step a wheel over the readout moves when no digit is under the pointer.
    pub tune_step_hz: u32,
    /// A click on the display moves the transceiver rather than pointing a
    /// decoder at the signal.
    ///
    /// Off by default. Pointing a decoder is what the skimmer mode is for, and
    /// an operator watching a band does not want the receiver to move every time
    /// they look at a trace.
    pub click_tunes: bool,
    /// Script answering in place of a transceiver.
    ///
    /// A path, relative to the configuration file. Present because the
    /// conditions worth exercising are the ones hardware cannot easily produce,
    /// and because it is the only way to bring the whole application up against
    /// a transceiver on a machine that has none.
    ///
    /// Takes precedence over the port and over the service, both of which hold
    /// real hardware. Empty means no script, which is the ordinary case.
    pub replay_path: String,
}

impl Default for RigSettings {
    fn default() -> Self {
        RigSettings {
            enabled: false,
            profile: String::new(),
            profiles_path: "rigs".to_string(),
            port: String::new(),
            baud: 19_200,
            transport: RigTransport::Direct,
            dtr: RigLine::Low,
            rts: RigLine::Low,
            sideband: SidebandMode::Auto,
            cw_pitch_hz: 700.0,
            offset_hz: 0.0,
            show_readout: true,
            readout_scale: 2.6,
            readout_fine: false,
            readout_repeat: true,
            rf_axis: true,
            tune_step_hz: 100,
            click_tunes: false,
            replay_path: String::new(),
        }
    }
}

impl SectionIo for RigSettings {
    const NAME: &'static str = "rig";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        RigSettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            profile: ini.get_string(Self::NAME, "profile", &d.profile),
            profiles_path: ini.get_string(Self::NAME, "profiles_path", &d.profiles_path),
            port: ini.get_string(Self::NAME, "port", &d.port),
            baud: ini.get_u32_clamped(Self::NAME, "baud", d.baud, 300, 921_600),
            transport: ini.get_enum(Self::NAME, "transport", d.transport),
            dtr: ini.get_enum(Self::NAME, "dtr", d.dtr),
            rts: ini.get_enum(Self::NAME, "rts", d.rts),
            sideband: ini.get_enum(Self::NAME, "sideband", d.sideband),
            cw_pitch_hz: ini.get_f32_clamped(Self::NAME, "cw_pitch_hz", d.cw_pitch_hz, 100.0, 3000.0),
            offset_hz: ini.get_f32_clamped(Self::NAME, "offset_hz", d.offset_hz, -100_000.0, 100_000.0),
            show_readout: ini.get_bool(Self::NAME, "show_readout", d.show_readout),
            readout_scale: ini.get_f32_clamped(Self::NAME, "readout_scale", d.readout_scale, 1.0, 8.0),
            readout_fine: ini.get_bool(Self::NAME, "readout_fine", d.readout_fine),
            readout_repeat: ini.get_bool(Self::NAME, "readout_repeat", d.readout_repeat),
            rf_axis: ini.get_bool(Self::NAME, "rf_axis", d.rf_axis),
            tune_step_hz: ini.get_u32_clamped(Self::NAME, "tune_step_hz", d.tune_step_hz, 1, 1_000_000),
            click_tunes: ini.get_bool(Self::NAME, "click_tunes", d.click_tunes),
            replay_path: ini.get_string(Self::NAME, "replay_path", &d.replay_path),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Transceiver control. The dial frequency is what turns the audio");
        ini.comment(s, "spectrum into a picture of a band; without it the display still");
        ini.comment(s, "works and is labelled in audio.");
        ini.set_bool(s, "enabled", self.enabled);
        ini.comment(s, "profile names a file under profiles_path, without the extension.");
        ini.comment(s, "A relative path is taken from the directory of the executable.");
        ini.set_string(s, "profile", &self.profile);
        ini.set_string(s, "profiles_path", &self.profiles_path);
        ini.set_string(s, "port", &self.port);
        ini.set_u32(s, "baud", self.baud);
        ini.comment(s, "transport: direct holds the port, shared reaches it through the");
        ini.comment(s, "service so a logging application can address the same transceiver.");
        ini.set_enum(s, "transport", self.transport);
        ini.comment(s, "Modem lines. Low unless a cable needs otherwise: on many interface");
        ini.comment(s, "cables one of these keys the transmitter.");
        ini.set_enum(s, "dtr", self.dtr);
        ini.set_enum(s, "rts", self.rts);
        ini.comment(s, "sideband: auto follows the mode the transceiver reports. The two");
        ini.comment(s, "explicit settings are for a receiver whose description cannot read");
        ini.comment(s, "the mode, and for a converter that inverts the spectrum.");
        ini.set_enum(s, "sideband", self.sideband);
        ini.comment(s, "cw_pitch_hz is where a station on the dial frequency is heard in a");
        ini.comment(s, "keyed mode. Used only when the transceiver cannot report its own.");
        ini.set_f32(s, "cw_pitch_hz", self.cw_pitch_hz);
        ini.comment(s, "offset_hz corrects a converter or a transverter.");
        ini.set_f32(s, "offset_hz", self.offset_hz);
        ini.set_bool(s, "show_readout", self.show_readout);
        ini.set_f32(s, "readout_scale", self.readout_scale);
        ini.comment(s, "readout_fine shows the last digit, one hertz instead of ten.");
        ini.set_bool(s, "readout_fine", self.readout_fine);
        ini.comment(s, "readout_repeat steps a held digit after a pause, as a front panel does.");
        ini.set_bool(s, "readout_repeat", self.readout_repeat);
        ini.comment(s, "rf_axis labels the spectrum on the air rather than in audio.");
        ini.set_bool(s, "rf_axis", self.rf_axis);
        ini.set_u32(s, "tune_step_hz", self.tune_step_hz);
        ini.comment(s, "click_tunes moves the transceiver on a click instead of pointing a");
        ini.comment(s, "decoder, which is what the skimmer mode uses the click for.");
        ini.set_bool(s, "click_tunes", self.click_tunes);
        ini.comment(s, "replay_path answers from a script instead of a transceiver and takes");
        ini.comment(s, "precedence over the port. One exchange per line, both in hexadecimal:");
        ini.comment(s, "  FEFE88E003FD = FEFE88E003FD FEFEE0880300506204 00FD");
        ini.set_string(s, "replay_path", &self.replay_path);
    }
}

// ------------------------------------------------------------- receiver

#[derive(Debug, Clone, PartialEq)]
pub struct ReceiverSettings {
    pub mode: OperatingMode,
    pub detector: Detector,
    pub mode_link: ModeLink,
    /// Allow the receiver to tune independently of the transceiver.
    ///
    /// Off by default, and the default is the point. With it off the software
    /// oscillator is held at nought, so the receiver sits on the dial and every
    /// gesture that would move it moves the transceiver instead: the readout,
    /// the display and the far end all name one frequency. With it on the
    /// oscillator moves inside the captured span while the dial stays put,
    /// which is what a panoramic receiver wants and is also the arrangement in
    /// which the two quietly stop agreeing.
    ///
    /// Clicking the display keeps working either way. What changes is which of
    /// the two the click moves.
    pub tune_enabled: bool,
    /// Where the receiver listens inside the captured span, in hertz.
    ///
    /// The software local oscillator, and it exists only for a complex input.
    /// There the application is the receiver: this point selects the signal,
    /// the filter edges below are measured from it, and the audio a detector
    /// produces is the offset from it.
    ///
    /// A real input is audio a transceiver already demodulated, so there is
    /// nothing left to tune: the chain is a filter and must be transparent in
    /// frequency, a tone at f leaving at f. The value is forced to nought
    /// there, and moving the passband then changes which audio is heard and
    /// with it the pitch of that audio, which is what filtering audio means.
    pub tune_hz: f32,
    /// Beat oscillator for keying on a complex input, in hertz.
    ///
    /// A keyed passband is symmetric about the tuning point, so a signal on
    /// that point emerges at nought hertz and cannot be heard. This offsets the
    /// output, which is what a beat oscillator does and the reason a receiver
    /// with an intermediate frequency has one.
    ///
    /// Distinct from the pitch under the transceiver section. That one states
    /// what the transceiver does to its own audio and is a reading; this one is
    /// a choice about what this application does.
    pub cw_pitch_hz: f32,
    /// Edges of the receiver filter, relative to the tuning point.
    ///
    /// Separate rather than a centre and a width, because the two edges are not
    /// symmetric about anything an operator cares about: a single sideband
    /// signal starts at the carrier and ends where the speech does, and the two
    /// boundaries are moved for different reasons.
    ///
    /// Relative rather than absolute, because that is the only way a passband
    /// shift can leave the pitch alone. Moving both edges together moves the
    /// mixer and the detector reference by the same amount, so what survives
    /// changes and what survives keeps its pitch, which is what a passband
    /// control does on a receiver with an intermediate frequency.
    ///
    /// Distinct from the passband under the spectrum section, which bounds
    /// where the channel allocator may open a decoder. That one says where to
    /// look, this one says what reaches the ear.
    pub filter_low_hz: f32,
    pub filter_high_hz: f32,
    /// Treat the two capture channels as the two halves of a complex signal.
    ///
    /// A receiver that delivers them gives a spectrum with a distinguishable
    /// upper and lower half, so the display covers twice the width and the image
    /// of a signal is suppressed rather than folded onto it.
    pub iq_input: bool,
    pub iq_swap: bool,
    /// Amplitude and phase correction between the two channels.
    ///
    /// The image rejection of a direct conversion receiver is decided entirely
    /// by how well the two paths match, and no hardware matches them exactly.
    pub iq_gain_db: f32,
    pub iq_phase_deg: f32,
    /// Impulse blanker ahead of the filter, on the whole captured width.
    pub nb_wide: bool,
    pub nb_wide_threshold: f32,
    /// Impulse blanker after the filter, on the demodulated signal.
    ///
    /// The two catch different things. A wideband impulse is short and reaches
    /// every part of the spectrum, so it is removed most cheaply before any
    /// filter has spread it in time. What survives the filter is spread and no
    /// longer looks like an impulse there, which is why a second one operating
    /// on a different time scale is worth having.
    pub nb_narrow: bool,
    pub nb_narrow_threshold: f32,
    pub nr_enabled: bool,
    pub nr_strength: f32,
    pub nr_method: NrMethod,
    pub notch_enabled: bool,
    pub notch_hz: f32,
    pub notch_width_hz: f32,
    /// Gain control on the receiver output.
    ///
    /// Separate from the one in the monitor section and separate again from the
    /// decoder path, which has none at all. A gain loop fast enough to be useful
    /// operates on the same time scale as keying, so it flattens the very
    /// envelope a keying detector measures.
    pub agc_enabled: bool,
    pub agc_attack_ms: f32,
    pub agc_hang_ms: f32,
    pub agc_release_ms: f32,
    pub agc_target_db: f32,
    pub squelch_enabled: bool,
    pub squelch_db: f32,
}

impl ReceiverSettings {
    /// Distance from the tuning point to the middle of the passband.
    ///
    /// What the filter mixes away beyond the tuning point, and what the
    /// detector puts back. Not a frequency an operator reads: it moves with
    /// either edge, so a readout built on it would drift whenever the width
    /// changed.
    pub fn relative_centre_hz(&self) -> f32 {
        (self.filter_low_hz + self.filter_high_hz) * 0.5
    }

    /// Middle of the passband in the audio that arrives, in hertz.
    ///
    /// Where a signal has to be for the receiver to be listening to it. This is
    /// the target a click brings a signal to, and the centre the monitor
    /// filter takes in the other mode.
    pub fn listen_hz(&self) -> f32 {
        self.tune_hz + self.relative_centre_hz()
    }

    /// Absolute edges of the passband inside the captured span.
    pub fn absolute_band(&self) -> (f32, f32) {
        (self.tune_hz + self.filter_low_hz, self.tune_hz + self.filter_high_hz)
    }

    /// Frequency the detector shifts its output by, in hertz.
    ///
    /// The beat oscillator and nothing else. Stated here rather than derived
    /// where it is needed, because two places need it and they have to agree:
    /// the chain applies it, and the display converts a notch frequency back
    /// through it to decide where the notch is on screen. A second copy of the
    /// rule is a second place for the picture to disagree with the ear.
    pub fn detector_offset_hz(&self, complex: bool) -> f32 {
        if complex && self.detector.is_keyed() {
            self.cw_pitch_hz
        } else {
            0.0
        }
    }
}

impl Default for ReceiverSettings {
    fn default() -> Self {
        ReceiverSettings {
            mode: OperatingMode::Skimmer,
            detector: Detector::Usb,
            mode_link: ModeLink::Follow,
            tune_enabled: false,
            tune_hz: 0.0,
            cw_pitch_hz: 700.0,
            filter_low_hz: 0.0,
            filter_high_hz: 2700.0,
            iq_input: false,
            iq_swap: false,
            iq_gain_db: 0.0,
            iq_phase_deg: 0.0,
            nb_wide: false,
            nb_wide_threshold: 8.0,
            nb_narrow: false,
            nb_narrow_threshold: 6.0,
            nr_enabled: false,
            nr_strength: 0.5,
            nr_method: NrMethod::Auto,
            notch_enabled: false,
            notch_hz: 1000.0,
            notch_width_hz: 80.0,
            agc_enabled: true,
            agc_attack_ms: 5.0,
            agc_hang_ms: 300.0,
            agc_release_ms: 500.0,
            agc_target_db: -12.0,
            squelch_enabled: false,
            squelch_db: -80.0,
        }
    }
}

impl SectionIo for ReceiverSettings {
    const NAME: &'static str = "receiver";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let low = ini.get_f32_clamped(Self::NAME, "filter_low_hz", d.filter_low_hz, -12000.0, 12000.0);
        let mut high =
            ini.get_f32_clamped(Self::NAME, "filter_high_hz", d.filter_high_hz, -11900.0, 12000.0);
        if high <= low {
            crate::log_warn!("config", "[receiver] filter_high_hz <= filter_low_hz");
            high = low + 100.0;
        }
        ReceiverSettings {
            mode: ini.get_enum(Self::NAME, "mode", d.mode),
            detector: ini.get_enum(Self::NAME, "detector", d.detector),
            mode_link: ini.get_enum(Self::NAME, "mode_link", d.mode_link),
            tune_enabled: ini.get_bool(Self::NAME, "tune_enabled", d.tune_enabled),
            tune_hz: ini.get_f32_clamped(Self::NAME, "tune_hz", d.tune_hz, -24000.0, 24000.0),
            cw_pitch_hz: ini.get_f32_clamped(Self::NAME, "cw_pitch_hz", d.cw_pitch_hz, 200.0, 1500.0),
            filter_low_hz: low,
            filter_high_hz: high,
            iq_input: ini.get_bool(Self::NAME, "iq_input", d.iq_input),
            iq_swap: ini.get_bool(Self::NAME, "iq_swap", d.iq_swap),
            iq_gain_db: ini.get_f32_clamped(Self::NAME, "iq_gain_db", d.iq_gain_db, -12.0, 12.0),
            iq_phase_deg: ini.get_f32_clamped(Self::NAME, "iq_phase_deg", d.iq_phase_deg, -45.0, 45.0),
            nb_wide: ini.get_bool(Self::NAME, "nb_wide", d.nb_wide),
            nb_wide_threshold: ini.get_f32_clamped(Self::NAME, "nb_wide_threshold", d.nb_wide_threshold, 1.0, 40.0),
            nb_narrow: ini.get_bool(Self::NAME, "nb_narrow", d.nb_narrow),
            nb_narrow_threshold: ini.get_f32_clamped(Self::NAME, "nb_narrow_threshold", d.nb_narrow_threshold, 1.0, 40.0),
            nr_enabled: ini.get_bool(Self::NAME, "nr_enabled", d.nr_enabled),
            nr_strength: ini.get_f32_clamped(Self::NAME, "nr_strength", d.nr_strength, 0.0, 1.0),
            nr_method: ini.get_enum(Self::NAME, "nr_method", d.nr_method),
            notch_enabled: ini.get_bool(Self::NAME, "notch_enabled", d.notch_enabled),
            notch_hz: ini.get_f32_clamped(Self::NAME, "notch_hz", d.notch_hz, 50.0, 12000.0),
            notch_width_hz: ini.get_f32_clamped(Self::NAME, "notch_width_hz", d.notch_width_hz, 10.0, 500.0),
            agc_enabled: ini.get_bool(Self::NAME, "agc_enabled", d.agc_enabled),
            agc_attack_ms: ini.get_f32_clamped(Self::NAME, "agc_attack_ms", d.agc_attack_ms, 0.1, 200.0),
            agc_hang_ms: ini.get_f32_clamped(Self::NAME, "agc_hang_ms", d.agc_hang_ms, 0.0, 5000.0),
            agc_release_ms: ini.get_f32_clamped(Self::NAME, "agc_release_ms", d.agc_release_ms, 10.0, 8000.0),
            agc_target_db: ini.get_f32_clamped(Self::NAME, "agc_target_db", d.agc_target_db, -60.0, 0.0),
            squelch_enabled: ini.get_bool(Self::NAME, "squelch_enabled", d.squelch_enabled),
            squelch_db: ini.get_f32_clamped(Self::NAME, "squelch_db", d.squelch_db, -140.0, 0.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Operating mode. skimmer decodes several keyed carriers at once and");
        ini.comment(s, "leaves the signal alone; sdr filters and demodulates one signal.");
        ini.comment(s, "Several settings below actively harm the first, which is why the");
        ini.comment(s, "mode exists rather than a row of independent switches.");
        ini.set_enum(s, "mode", self.mode);
        ini.comment(s, "detector: cw, usb, lsb, dig_u, dig_l, am, sam, fm. Changing it");
        ini.comment(s, "applies the filter preset of the new mode.");
        ini.set_enum(s, "detector", self.detector);
        ini.comment(s, "mode_link: independent leaves the two apart, follow takes the mode");
        ini.comment(s, "from the transceiver, drive sends this one to it. Following is right");
        ini.comment(s, "for audio from a transceiver, which already chose the sideband.");
        ini.set_enum(s, "mode_link", self.mode_link);
        ini.comment(s, "tune_enabled lets the receiver move inside the captured span while");
        ini.comment(s, "the dial stays put. Off, the receiver sits on the dial and a click on");
        ini.comment(s, "the display moves the transceiver, so the two always name the same");
        ini.comment(s, "frequency. Clicking keeps working either way.");
        ini.set_bool(s, "tune_enabled", self.tune_enabled);
        ini.comment(s, "tune_hz is the software local oscillator, and it applies to a complex");
        ini.comment(s, "input only. A real input is audio a transceiver already demodulated,");
        ini.comment(s, "so the chain there is a filter and is transparent in frequency.");
        ini.set_f32(s, "tune_hz", self.tune_hz);
        ini.comment(s, "cw_pitch_hz makes a keyed signal audible on a complex input, where the");
        ini.comment(s, "passband is symmetric about the tuning point and the offset is nought.");
        ini.set_f32(s, "cw_pitch_hz", self.cw_pitch_hz);
        ini.comment(s, "Filter edges, stated separately because the two are moved for");
        ini.comment(s, "different reasons. Absolute audio on a real input; measured from the");
        ini.comment(s, "tuning point on a complex one, where the lower sideband is negative.");
        ini.comment(s, "The lower edge may sit at nought: the capture front end has already");
        ini.comment(s, "removed the offset that used to make that a bad idea.");
        ini.comment(s, "Distinct from the spectrum passband, which bounds where a decoder");
        ini.comment(s, "channel may be opened rather than what reaches the ear.");
        ini.set_f32(s, "filter_low_hz", self.filter_low_hz);
        ini.set_f32(s, "filter_high_hz", self.filter_high_hz);
        ini.comment(s, "iq_input reads the two capture channels as one complex signal, which");
        ini.comment(s, "doubles the visible width and separates the two halves of it.");
        ini.set_bool(s, "iq_input", self.iq_input);
        ini.comment(s, "iq_swap conjugates the signal, so it decides which way round the");
        ini.comment(s, "spectrum is. The display applies it as well, or the picture would");
        ini.comment(s, "show the opposite sideband from the one being heard.");
        ini.set_bool(s, "iq_swap", self.iq_swap);
        ini.comment(s, "Image rejection is decided by how well the two paths match, and no");
        ini.comment(s, "hardware matches them exactly.");
        ini.set_f32(s, "iq_gain_db", self.iq_gain_db);
        ini.set_f32(s, "iq_phase_deg", self.iq_phase_deg);
        ini.comment(s, "Two blankers on two time scales. A wideband impulse is removed most");
        ini.comment(s, "cheaply before a filter has spread it; what survives the filter no");
        ini.comment(s, "longer looks like an impulse and needs the second one.");
        ini.set_bool(s, "nb_wide", self.nb_wide);
        ini.set_f32(s, "nb_wide_threshold", self.nb_wide_threshold);
        ini.set_bool(s, "nb_narrow", self.nb_narrow);
        ini.set_f32(s, "nb_narrow_threshold", self.nb_narrow_threshold);
        ini.set_bool(s, "nr_enabled", self.nr_enabled);
        ini.set_f32(s, "nr_strength", self.nr_strength);
        ini.comment(s, "nr_method: auto follows the detector, predictor suits keying and");
        ini.comment(s, "data, spectral suits voice. Neither is better in general.");
        ini.set_enum(s, "nr_method", self.nr_method);
        ini.set_bool(s, "notch_enabled", self.notch_enabled);
        ini.set_f32(s, "notch_hz", self.notch_hz);
        ini.set_f32(s, "notch_width_hz", self.notch_width_hz);
        ini.comment(s, "Gain control on the receiver output only. The decoder path has none");
        ini.comment(s, "at all: a loop fast enough to be useful flattens the envelope a");
        ini.comment(s, "keying detector measures.");
        ini.set_bool(s, "agc_enabled", self.agc_enabled);
        ini.set_f32(s, "agc_attack_ms", self.agc_attack_ms);
        ini.comment(s, "hang holds the gain through a pause so it does not chase the noise.");
        ini.set_f32(s, "agc_hang_ms", self.agc_hang_ms);
        ini.set_f32(s, "agc_release_ms", self.agc_release_ms);
        ini.set_f32(s, "agc_target_db", self.agc_target_db);
        ini.set_bool(s, "squelch_enabled", self.squelch_enabled);
        ini.set_f32(s, "squelch_db", self.squelch_db);
    }
}

// ---------------------------------------------------------------- audio

#[derive(Debug, Clone)]
pub struct AudioSettings {
    pub backend: AudioBackend,
    /// Empty means the system default capture endpoint.
    pub device_id: String,
    /// Friendly name kept for the UI, not used for matching.
    pub device_name: String,
    /// 0 means follow the device mix format.
    pub sample_rate: u32,
    pub channel_mode: ChannelMode,
    pub capture_buffer_ms: u32,
    /// Ring buffer depth between capture and DSP threads.
    pub ring_seconds: f32,
    /// Internal processing rate after decimation.
    pub dsp_sample_rate: u32,
    pub input_gain_db: f32,
    pub dc_block: bool,
    pub exclusive_mode: bool,
    pub monitor_enabled: bool,
    /// Render endpoint the monitor plays to, empty for the system default.
    pub monitor_device_id: String,
    pub monitor_device_name: String,
    pub monitor_volume: f32,
    /// Narrow the listening path to a band around the tracked carrier.
    pub monitor_filter: bool,
    pub monitor_width_mode: MonitorWidth,
    /// Width used when the mode is independent, in hertz.
    pub monitor_bandwidth_hz: f32,
    /// Centre the filter on the focused keying channel rather than on a stated
    /// frequency. Following is what makes the monitor useful while the tracker
    /// moves; a stated centre is what makes it usable when nothing is tracked.
    pub monitor_follow: bool,
    pub monitor_centre_hz: f32,
    /// Pitch the skimmer monitor brings the selected band down to, in hertz.
    ///
    /// The skimmer path translates rather than merely filtering, so a channel is
    /// heard at the same pitch wherever it sits in the span. Without the
    /// translation a station three kilohertz from the dial is heard at three
    /// kilohertz, which is a whistle rather than a signal.
    pub monitor_pitch_hz: f32,
    /// Warn in the UI when the capture stream underruns this often.
    pub xrun_warn_per_minute: u32,
}

impl Default for AudioSettings {
    fn default() -> Self {
        AudioSettings {
            backend: AudioBackend::Wasapi,
            device_id: String::new(),
            device_name: String::new(),
            sample_rate: 0,
            channel_mode: ChannelMode::Left,
            capture_buffer_ms: 20,
            ring_seconds: 4.0,
            dsp_sample_rate: 12000,
            input_gain_db: 0.0,
            dc_block: true,
            exclusive_mode: false,
            monitor_enabled: false,
            monitor_device_id: String::new(),
            monitor_device_name: String::new(),
            monitor_volume: 0.5,
            monitor_filter: true,
            monitor_width_mode: MonitorWidth::Detector,
            monitor_bandwidth_hz: 300.0,
            monitor_follow: true,
            monitor_centre_hz: 700.0,
            monitor_pitch_hz: 700.0,
            xrun_warn_per_minute: 3,
        }
    }
}

impl SectionIo for AudioSettings {
    const NAME: &'static str = "audio";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        AudioSettings {
            backend: ini.get_enum(Self::NAME, "backend", d.backend),
            device_id: ini.get_string(Self::NAME, "device_id", &d.device_id),
            device_name: ini.get_string(Self::NAME, "device_name", &d.device_name),
            sample_rate: ini.get_u32(Self::NAME, "sample_rate", d.sample_rate),
            channel_mode: ini.get_enum(Self::NAME, "channel_mode", d.channel_mode),
            capture_buffer_ms: ini.get_u32_clamped(Self::NAME, "capture_buffer_ms", d.capture_buffer_ms, 2, 200),
            ring_seconds: ini.get_f32_clamped(Self::NAME, "ring_seconds", d.ring_seconds, 0.25, 60.0),
            dsp_sample_rate: ini.get_u32_clamped(Self::NAME, "dsp_sample_rate", d.dsp_sample_rate, 4000, 192_000),
            input_gain_db: ini.get_f32_clamped(Self::NAME, "input_gain_db", d.input_gain_db, -40.0, 40.0),
            dc_block: ini.get_bool(Self::NAME, "dc_block", d.dc_block),
            exclusive_mode: ini.get_bool(Self::NAME, "exclusive_mode", d.exclusive_mode),
            monitor_enabled: ini.get_bool(Self::NAME, "monitor_enabled", d.monitor_enabled),
            monitor_device_id: ini.get_string(Self::NAME, "monitor_device_id", &d.monitor_device_id),
            monitor_device_name: ini.get_string(Self::NAME, "monitor_device_name", &d.monitor_device_name),
            monitor_volume: ini.get_f32_clamped(Self::NAME, "monitor_volume", d.monitor_volume, 0.0, 1.0),
            monitor_filter: ini.get_bool(Self::NAME, "monitor_filter", d.monitor_filter),
            monitor_width_mode: ini.get_enum(Self::NAME, "monitor_width_mode", d.monitor_width_mode),
            monitor_bandwidth_hz: ini.get_f32_clamped(Self::NAME, "monitor_bandwidth_hz", d.monitor_bandwidth_hz, 50.0, 3000.0),
            monitor_follow: ini.get_bool(Self::NAME, "monitor_follow", d.monitor_follow),
            monitor_centre_hz: ini.get_f32_clamped(Self::NAME, "monitor_centre_hz", d.monitor_centre_hz, -24000.0, 24000.0),
            monitor_pitch_hz: ini.get_f32_clamped(Self::NAME, "monitor_pitch_hz", d.monitor_pitch_hz, 200.0, 1500.0),
            xrun_warn_per_minute: ini.get_u32(Self::NAME, "xrun_warn_per_minute", d.xrun_warn_per_minute),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Capture source. A device identifier beginning with out: is a render");
        ini.comment(s, "endpoint recorded through the loopback path.");
        ini.comment(s, "device_id empty selects the system default input.");
        ini.set_enum(s, "backend", self.backend);
        ini.set_string(s, "device_id", &self.device_id);
        ini.set_string(s, "device_name", &self.device_name);
        ini.comment(s, "sample_rate 0 follows the device format.");
        ini.set_u32(s, "sample_rate", self.sample_rate);
        ini.comment(s, "channel_mode: left, right, mix, difference.");
        ini.set_enum(s, "channel_mode", self.channel_mode);
        ini.comment(s, "Capture period and ring depth. Lower period means lower latency.");
        ini.set_u32(s, "capture_buffer_ms", self.capture_buffer_ms);
        ini.set_f32(s, "ring_seconds", self.ring_seconds);
        ini.comment(s, "Rate the processing path runs at, before the reduction under [dsp].");
        ini.comment(s, "A quadrature pair is two sided, so the rate is the whole width the");
        ini.comment(s, "display can show rather than half of it: a converter delivering one");
        ini.comment(s, "hundred and ninety two thousand covers that many hertz of band, and a");
        ini.comment(s, "path set to half of it throws away half the span before anything sees");
        ini.comment(s, "it.");
        ini.set_u32(s, "dsp_sample_rate", self.dsp_sample_rate);
        ini.set_f32(s, "input_gain_db", self.input_gain_db);
        ini.set_bool(s, "dc_block", self.dc_block);
        ini.set_bool(s, "exclusive_mode", self.exclusive_mode);
        ini.comment(s, "Headphone monitor. A second path out of the capture chain, taken");
        ini.comment(s, "at the decoder rate, with its own filter and gain control. Neither");
        ini.comment(s, "reaches the decoders: what an ear wants is what a keying detector");
        ini.comment(s, "must not have.");
        ini.set_bool(s, "monitor_enabled", self.monitor_enabled);
        ini.comment(s, "monitor_device_id is a bare render endpoint, empty for the default.");
        ini.set_string(s, "monitor_device_id", &self.monitor_device_id);
        ini.set_string(s, "monitor_device_name", &self.monitor_device_name);
        ini.set_f32(s, "monitor_volume", self.monitor_volume);
        ini.comment(s, "monitor_width_mode: detector follows the keying filter, independent");
        ini.comment(s, "uses monitor_bandwidth_hz. monitor_follow centres on the focused");
        ini.comment(s, "channel; without it monitor_centre_hz is used.");
        ini.set_bool(s, "monitor_filter", self.monitor_filter);
        ini.set_enum(s, "monitor_width_mode", self.monitor_width_mode);
        ini.set_f32(s, "monitor_bandwidth_hz", self.monitor_bandwidth_hz);
        ini.set_bool(s, "monitor_follow", self.monitor_follow);
        ini.set_f32(s, "monitor_centre_hz", self.monitor_centre_hz);
        ini.comment(s, "monitor_pitch_hz is where the skimmer monitor puts the chosen band.");
        ini.comment(s, "The path translates rather than filtering only, so a channel sounds the");
        ini.comment(s, "same wherever it sits; a wide band is pushed up until its lower edge");
        ini.comment(s, "reaches nought, because a real output folds about it.");
        ini.set_f32(s, "monitor_pitch_hz", self.monitor_pitch_hz);
        ini.set_u32(s, "xrun_warn_per_minute", self.xrun_warn_per_minute);
    }
}

// ------------------------------------------------------------------ dsp

#[derive(Debug, Clone)]
pub struct DspSettings {
    pub fft_size: u32,
    pub fft_window: WindowFn,
    pub kaiser_beta: f32,
    pub overlap_percent: u32,
    /// Widen the transform as the display is magnified.
    ///
    /// The hop follows the requested line rate and the overlap only bounds it
    /// from above, so a wider transform raises that bound and leaves the hop
    /// alone: the waterfall keeps its speed and every line is finer.
    pub zoom_resolution: bool,
    /// Run the spectrum transform in a compute shader.
    /// pub gpu_fft: bool,
    pub average_frames: u32,
    pub decimation: u32,
    pub passband_low_hz: f32,
    pub passband_high_hz: f32,
    /// How far either side of the tuning point a channel may be opened, in
    /// hertz. Nought means the whole captured span.
    ///
    /// A quadrature input needs its own bound because the passband above cannot
    /// serve as one: that setting describes audio from a transceiver, where the
    /// whole spectrum is three kilohertz wide. Applied to a span of two hundred
    /// kilohertz it confines the search to a sliver around the dial, and every
    /// station on the band outside it is invisible to the allocator.
    pub search_span_hz: f32,
    pub noise_blanker: bool,
    pub noise_blanker_threshold: f32,
    pub auto_notch: bool,
    pub agc_enabled: bool,
    pub agc_attack_ms: f32,
    pub agc_release_ms: f32,
    pub agc_target_db: f32,
    /// Worker threads for CPU DSP, 0 means pick from the core count.
    pub worker_threads: u32,
}

impl Default for DspSettings {
    fn default() -> Self {
        DspSettings {
            fft_size: 4096,
            fft_window: WindowFn::BlackmanHarris,
            kaiser_beta: 8.6,
            overlap_percent: 50,
            zoom_resolution: true,
            // gpu_fft: true,
            average_frames: 2,
            decimation: 1,
            passband_low_hz: 100.0,
            passband_high_hz: 3200.0,
            search_span_hz: 0.0,
            noise_blanker: false,
            noise_blanker_threshold: 6.0,
            auto_notch: false,
            agc_enabled: true,
            agc_attack_ms: 5.0,
            agc_release_ms: 250.0,
            agc_target_db: -18.0,
            worker_threads: 0,
        }
    }
}

impl DspSettings {
    /// Threads the processing path is allowed to use.
    ///
    /// Nought derives the count from the processor, which is what an operator
    /// means by leaving it alone.
    ///
    /// The path itself runs on one thread and the reason is arithmetic rather
    /// than laziness: the transform is a few hundred microseconds at the largest
    /// size offered, and a full bank of keying detectors is two orders of
    /// magnitude below the frame budget. A worker there would add a queue and a
    /// frame of latency to buy nothing. The value is resolved and reported so the
    /// operator can see what it came to, and it bounds anything added later that
    /// genuinely is heavy.
    pub fn resolved_worker_threads(&self) -> u32 {
        if self.worker_threads > 0 {
            return self.worker_threads;
        }
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(1)
    }

    /// Rate the processing path settles on, after the reduction.
    ///
    /// The reduction composes with the stated rate rather than replacing it,
    /// which is what makes it worth having: halving is a step of a small integer
    /// and retyping a rate is not.
    pub fn effective_rate(&self, dsp_sample_rate: u32) -> u32 {
        (dsp_sample_rate / self.decimation.max(1)).max(1000)
    }
}

impl SectionIo for DspSettings {
    const NAME: &'static str = "dsp";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let mut fft = ini.get_u32_clamped(Self::NAME, "fft_size", d.fft_size, 256, 65536);
        if !fft.is_power_of_two() {
            crate::log_warn!("config", "[dsp] fft_size {} is not a power of two", fft);
            fft = fft.next_power_of_two().min(65536);
        }
        DspSettings {
            fft_size: fft,
            fft_window: ini.get_enum(Self::NAME, "fft_window", d.fft_window),
            kaiser_beta: ini.get_f32_clamped(Self::NAME, "kaiser_beta", d.kaiser_beta, 0.1, 20.0),
            overlap_percent: ini.get_u32_clamped(Self::NAME, "overlap_percent", d.overlap_percent, 0, 90),
            zoom_resolution: ini.get_bool(Self::NAME, "zoom_resolution", d.zoom_resolution),
            //gpu_fft: ini.get_bool(Self::NAME, "gpu_fft", d.gpu_fft),
            average_frames: ini.get_u32_clamped(Self::NAME, "average_frames", d.average_frames, 1, 64),
            decimation: ini.get_u32_clamped(Self::NAME, "decimation", d.decimation, 1, 16),
            passband_low_hz: ini.get_f32_clamped(Self::NAME, "passband_low_hz", d.passband_low_hz, 0.0, 20000.0),
            passband_high_hz: ini.get_f32_clamped(Self::NAME, "passband_high_hz", d.passband_high_hz, 50.0, 24000.0),
            search_span_hz: ini.get_f32_clamped(Self::NAME, "search_span_hz", d.search_span_hz, 0.0, 120000.0),
            noise_blanker: ini.get_bool(Self::NAME, "noise_blanker", d.noise_blanker),
            noise_blanker_threshold: ini.get_f32_clamped(Self::NAME, "noise_blanker_threshold", d.noise_blanker_threshold, 1.0, 40.0),
            auto_notch: ini.get_bool(Self::NAME, "auto_notch", d.auto_notch),
            agc_enabled: ini.get_bool(Self::NAME, "agc_enabled", d.agc_enabled),
            agc_attack_ms: ini.get_f32_clamped(Self::NAME, "agc_attack_ms", d.agc_attack_ms, 0.1, 500.0),
            agc_release_ms: ini.get_f32_clamped(Self::NAME, "agc_release_ms", d.agc_release_ms, 1.0, 5000.0),
            agc_target_db: ini.get_f32_clamped(Self::NAME, "agc_target_db", d.agc_target_db, -60.0, 0.0),
            worker_threads: ini.get_u32_clamped(Self::NAME, "worker_threads", d.worker_threads, 0, 64),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "fft_size must be a power of two. Larger means finer resolution,");
        ini.comment(s, "smaller means faster response for CW at high speed.");
        ini.set_u32(s, "fft_size", self.fft_size);
        ini.comment(s, "fft_window: rectangular, hann, hamming, blackman, blackman_harris,");
        ini.comment(s, "nuttall, flattop, kaiser.");
        ini.set_enum(s, "fft_window", self.fft_window);
        ini.set_f32(s, "kaiser_beta", self.kaiser_beta);
        ini.set_u32(s, "overlap_percent", self.overlap_percent);
        ini.comment(s, "zoom_resolution widens the transform with the magnification, up to four");
        ini.comment(s, "times, in steps of two so it is replanned at three magnifications rather");
        ini.comment(s, "than on every notch. The line rate does not change: the hop follows the");
        ini.comment(s, "speed setting and the overlap only bounds it, so a wider transform raises");
        ini.comment(s, "that bound. It also doubles the stored waterfall width, which is the");
        ini.comment(s, "memory cost.");
        ini.set_bool(s, "zoom_resolution", self.zoom_resolution);
        //ini.comment(s, "gpu_fft moves the transform to a compute shader.");
        //ini.set_bool(s, "gpu_fft", self.gpu_fft);
        ini.set_u32(s, "average_frames", self.average_frames);
        ini.comment(s, "decimation divides dsp_sample_rate again, so the path settles at the");
        ini.comment(s, "quotient. It composes with the rate rather than replacing it: halving");
        ini.comment(s, "is a step of a small integer, retyping a rate is not.");
        ini.set_u32(s, "decimation", self.decimation);
        ini.comment(s, "The passband bounds where a decoder channel may be opened. The");
        ini.comment(s, "receiver filter under its own section is a different thing: that one");
        ini.comment(s, "bounds what reaches the ear.");
        ini.set_f32(s, "passband_low_hz", self.passband_low_hz);
        ini.set_f32(s, "passband_high_hz", self.passband_high_hz);
        ini.comment(s, "search_span_hz bounds the same thing on a quadrature input, measured");
        ini.comment(s, "either side of the tuning point. Nought is the whole captured span.");
        ini.comment(s, "The pair above cannot serve: it describes audio from a transceiver,");
        ini.comment(s, "where the whole spectrum is three kilohertz, and applied to a span of");
        ini.comment(s, "two hundred it hides every station but the few beside the dial.");
        ini.set_f32(s, "search_span_hz", self.search_span_hz);
        ini.comment(s, "The blanker here is ahead of the transform and serves the display and");
        ini.comment(s, "the decoders. The two under the receiver section serve the ear and are");
        ini.comment(s, "on the monitor thread, so neither of them reaches this path.");
        ini.comment(s, "The threshold is a multiple of the running level of the whole");
        ini.comment(s, "passband. Lowered far enough it starts clipping keying edges, and the");
        ini.comment(s, "symptom is a decoder reporting elements shorter than they are.");
        ini.set_bool(s, "noise_blanker", self.noise_blanker);
        ini.set_f32(s, "noise_blanker_threshold", self.noise_blanker_threshold);
        ini.set_bool(s, "auto_notch", self.auto_notch);
        ini.comment(s, "Gain control applies to the monitor output only. It is kept out of");
        ini.comment(s, "the decoder path on purpose: its time constants overlap the keying");
        ini.comment(s, "and it flattens the envelope the detectors measure.");
        ini.set_bool(s, "agc_enabled", self.agc_enabled);
        ini.set_f32(s, "agc_attack_ms", self.agc_attack_ms);
        ini.set_f32(s, "agc_release_ms", self.agc_release_ms);
        ini.set_f32(s, "agc_target_db", self.agc_target_db);
        ini.comment(s, "worker_threads 0 derives the count from the CPU.");
        ini.set_u32(s, "worker_threads", self.worker_threads);
    }
}

// ------------------------------------------------------------ waterfall

#[derive(Debug, Clone)]
pub struct WaterfallSettings {
    pub colormap: ColorMap,
    pub style: WaterfallStyle,
    pub min_db: f32,
    pub max_db: f32,
    pub auto_range: bool,
    pub gamma: f32,
    /// New lines per second.
    pub scroll_lines_per_second: f32,
    pub history_lines: u32,
    pub center_hz: f32,
    pub span_hz: f32,
    pub show_grid: bool,
    pub show_labels: bool,
    /// Horizontal lines at round levels, with a scale down the left edge.
    ///
    /// The frequency grid says where a signal is; without this one there is no
    /// way to say how strong it is except by reading a separate meter, which
    /// answers about the whole passband rather than about the trace.
    pub show_level_grid: bool,
    pub show_cursor_readout: bool,
    pub smoothing: f32,
    pub spectrum_visible: bool,
    pub spectrum_height_fraction: f32,
    pub peak_hold: bool,
    /// Magnification of the visible span, one meaning the whole of it.
    pub zoom: f32,
    /// Centre of the visible span, as a fraction of the whole.
    ///
    /// A fraction rather than a frequency: the whole span changes with the
    /// sample rate and with the quadrature setting, and a stored frequency
    /// would land somewhere arbitrary after either.
    pub view_centre: f32,
    /// What follows the dial when it moves.
    ///
    /// Off leaves the record where it is, so a station changes column while its
    /// history does not and the vertical stripe an operator reads a transmission
    /// by is broken at every retune.
    ///
    /// Audio shifts the record so the stripe survives, and leaves the view where
    /// it is. The receiver therefore stays at the same place on screen and the
    /// panorama travels underneath it, which is what a receiver with a fixed
    /// intermediate frequency does.
    ///
    /// Band shifts the view with it, so the panorama stands still and the
    /// receiver marker travels across it. That is what a panoramic receiver does,
    /// where the dial is the tuner centre and the oscillator moves inside the
    /// captured span. The view stops at the edge of the span, and past that point
    /// the picture travels again because there is nothing further to show.
    pub anchor: AnchorMode,
    /// Columns the history texture holds, nought meaning the transform width.
    ///
    /// Below the transform width the picture is coarser than the trace drawn over
    /// it, which is invisible at full span and obvious under magnification.
    /// Above it there is nothing further to store. Applied at the next start,
    /// because the texture cannot be resized without discarding what it holds.
    pub columns: u32,
    /// Interpolate between stored columns rather than showing them as blocks.
    ///
    /// Under magnification one stored column covers many pixels. Blocks state
    /// exactly what was measured and read as coarse; interpolation reads as a
    /// picture and invents the values between two columns, which on a narrow
    /// carrier widens it to the width of the interpolation.
    pub smooth: bool,
    /// Mark known frequencies from the station list.
    pub show_stations: bool,
    /// Overlay the decayed peak hold the channel allocator searches.
    ///
    /// The allocator does not look at the live spectrum: a keyed carrier is
    /// absent from it roughly four frames in ten, so the strongest bin of the
    /// moment belongs to whoever happens to be transmitting. It searches a
    /// decayed hold instead, and that surface is what decides where a channel
    /// opens. Showing it is the only way to answer why a channel appeared on one
    /// peak and not on another.
    pub show_held_trace: bool,
    /// Mark decoded signal positions on the waterfall.
    pub mark_decoders: bool,
    /// Overlay the long power average.
    ///
    /// Noise averages towards its own mean and a steady carrier does not, so a
    /// tone below the noise of any single frame stands above the noise of a
    /// hundred. It is the one trace that finds a weak beacon, and it is useless
    /// for anything that changes: keying averages down by its duty cycle.
    pub show_average_trace: bool,
    /// Age of the history down the left edge of the waterfall.
    ///
    /// The waterfall says what was received and not when. The line rate is
    /// known exactly, so this is arithmetic rather than a measurement.
    pub time_axis: bool,
    /// Store the history as one channel and map it in the shader.
    ///
    /// Quarter of the memory and quarter of the upload bandwidth, and a change
    /// to the palette, the gamma or the ceiling repaints the whole history
    /// instead of only the lines drawn after it. Read at startup, because
    /// switching means a differently formatted texture and the history stored in
    /// the old one cannot be reinterpreted.
    pub gpu_palette: bool,
}

impl Default for WaterfallSettings {
    fn default() -> Self {
        WaterfallSettings {
            colormap: ColorMap::BlueSteel,
            style: WaterfallStyle::Classic,
            min_db: -120.0,
            max_db: -20.0,
            auto_range: true,
            gamma: 1.0,
            scroll_lines_per_second: 25.0,
            history_lines: 2048,
            center_hz: 1500.0,
            span_hz: 3000.0,
            show_grid: true,
            show_labels: true,
            show_level_grid: true,
            show_cursor_readout: true,
            smoothing: 0.2,
            spectrum_visible: true,
            spectrum_height_fraction: 0.35,
            peak_hold: false,
            zoom: 1.0,
            view_centre: 0.5,
            anchor: AnchorMode::Audio,
            columns: 0,
            smooth: false,
            show_stations: true,
            show_held_trace: false,
            mark_decoders: true,
            show_average_trace: false,
            time_axis: true,
            gpu_palette: true,
        }
    }
}

impl SectionIo for WaterfallSettings {
    const NAME: &'static str = "waterfall";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let min_db = ini.get_f32_clamped(Self::NAME, "min_db", d.min_db, -180.0, 0.0);
        let mut max_db = ini.get_f32_clamped(Self::NAME, "max_db", d.max_db, -179.0, 20.0);
        if max_db <= min_db {
            crate::log_warn!("config", "[waterfall] max_db <= min_db, using default span");
            max_db = min_db + 60.0;
        }
        WaterfallSettings {
            colormap: ini.get_enum(Self::NAME, "colormap", d.colormap),
            style: ini.get_enum(Self::NAME, "style", d.style),
            min_db,
            max_db,
            auto_range: ini.get_bool(Self::NAME, "auto_range", d.auto_range),
            gamma: ini.get_f32_clamped(Self::NAME, "gamma", d.gamma, 0.2, 4.0),
            scroll_lines_per_second: ini.get_f32_clamped(Self::NAME, "scroll_lines_per_second", d.scroll_lines_per_second, 1.0, 200.0),
            history_lines: ini.get_u32_clamped(Self::NAME, "history_lines", d.history_lines, 128, 16384),
            // A quadrature display reaches below nought, so the stored view has
            // to be able to name a frequency there. A floor at nought would fold
            // every recalled view of the lower half onto the middle.
            center_hz: ini.get_f32_clamped(Self::NAME, "center_hz", d.center_hz, -24000.0, 24000.0),
            span_hz: ini.get_f32_clamped(Self::NAME, "span_hz", d.span_hz, 100.0, 48000.0),
            show_grid: ini.get_bool(Self::NAME, "show_grid", d.show_grid),
            show_labels: ini.get_bool(Self::NAME, "show_labels", d.show_labels),
            show_level_grid: ini.get_bool(Self::NAME, "show_level_grid", d.show_level_grid),
            show_cursor_readout: ini.get_bool(Self::NAME, "show_cursor_readout", d.show_cursor_readout),
            smoothing: ini.get_f32_clamped(Self::NAME, "smoothing", d.smoothing, 0.0, 0.95),
            spectrum_visible: ini.get_bool(Self::NAME, "spectrum_visible", d.spectrum_visible),
            spectrum_height_fraction: ini.get_f32_clamped(Self::NAME, "spectrum_height_fraction", d.spectrum_height_fraction, 0.1, 0.8),
            peak_hold: ini.get_bool(Self::NAME, "peak_hold", d.peak_hold),
            zoom: ini.get_f32_clamped(Self::NAME, "zoom", d.zoom, 1.0, 64.0),
            view_centre: ini.get_f32_clamped(Self::NAME, "view_centre", d.view_centre, 0.0, 1.0),
            anchor: ini.get_enum(Self::NAME, "anchor", d.anchor),
            columns: ini.get_u32_clamped(Self::NAME, "columns", d.columns, 0, 16384),
            smooth: ini.get_bool(Self::NAME, "smooth", d.smooth),
            show_stations: ini.get_bool(Self::NAME, "show_stations", d.show_stations),
            show_held_trace: ini.get_bool(Self::NAME, "show_held_trace", d.show_held_trace),
            mark_decoders: ini.get_bool(Self::NAME, "mark_decoders", d.mark_decoders),
            show_average_trace: ini.get_bool(Self::NAME, "show_average_trace", d.show_average_trace),
            time_axis: ini.get_bool(Self::NAME, "time_axis", d.time_axis),
            gpu_palette: ini.get_bool(Self::NAME, "gpu_palette", d.gpu_palette),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "colormap: grayscale, blue_steel, inferno, viridis, turbo.");
        ini.set_enum(s, "colormap", self.colormap);
        ini.comment(s, "style: classic maps absolute levels, skimmer anchors the palette");
        ini.comment(s, "to the noise floor of each line so fading does not dim the traces.");
        ini.comment(s, "In skimmer mode min_db only sets the span together with max_db.");
        ini.set_enum(s, "style", self.style);
        ini.comment(s, "Display range in dB. auto_range tracks the noise floor.");
        ini.set_f32(s, "min_db", self.min_db);
        ini.set_f32(s, "max_db", self.max_db);
        ini.set_bool(s, "auto_range", self.auto_range);
        ini.set_f32(s, "gamma", self.gamma);
        ini.set_f32(s, "scroll_lines_per_second", self.scroll_lines_per_second);
        ini.comment(s, "history_lines is the GPU texture height, memory is width x lines x 1 byte.");
        ini.set_u32(s, "history_lines", self.history_lines);
        ini.comment(s, "center_hz and span_hz are one saved view, written and recalled from");
        ini.comment(s, "the display section. zoom and view_centre above are the live one, so");
        ini.comment(s, "the pair below is a bookmark rather than a second statement of it.");
        ini.set_f32(s, "center_hz", self.center_hz);
        ini.set_f32(s, "span_hz", self.span_hz);
        ini.set_bool(s, "show_grid", self.show_grid);
        ini.set_bool(s, "show_labels", self.show_labels);
        ini.comment(s, "show_level_grid draws horizontal lines at round levels, so a level");
        ini.comment(s, "can be read off the trace rather than from a separate meter.");
        ini.set_bool(s, "show_level_grid", self.show_level_grid);
        ini.set_bool(s, "show_cursor_readout", self.show_cursor_readout);
        ini.set_f32(s, "smoothing", self.smoothing);
        ini.set_bool(s, "spectrum_visible", self.spectrum_visible);
        ini.set_f32(s, "spectrum_height_fraction", self.spectrum_height_fraction);
        ini.set_bool(s, "peak_hold", self.peak_hold);
        ini.comment(s, "zoom magnifies the visible span, one meaning all of it. view_centre");
        ini.comment(s, "is a fraction of the whole span rather than a frequency, so it keeps");
        ini.comment(s, "its meaning when the sample rate or the quadrature setting changes.");
        ini.set_f32(s, "zoom", self.zoom);
        ini.set_f32(s, "view_centre", self.view_centre);
        ini.comment(s, "show_stations marks the frequencies listed in stations.ini.");
        ini.comment(s, "Only meaningful with a dial frequency: without one there is no");
        ini.comment(s, "way to place a frequency on an audio axis.");
        ini.comment(s, "anchor decides what follows the dial. off leaves the record alone and");
        ini.comment(s, "breaks the stripe of every station at each retune. audio shifts the");
        ini.comment(s, "record and holds the view, so the receiver stays put and the panorama");
        ini.comment(s, "travels. band shifts the view as well, so the panorama stands still");
        ini.comment(s, "and the receiver marker travels across it. Needs a dial and rf_axis.");
        ini.set_enum(s, "anchor", self.anchor);
        ini.comment(s, "columns nought takes the transform width, which is what stops the");
        ini.comment(s, "history from being coarser than the trace drawn over it. Memory is");
        ini.comment(s, "columns times history_lines, times four unless gpu_palette is on.");
        ini.set_u32(s, "columns", self.columns);
        ini.comment(s, "smooth interpolates between stored columns. It reads as a picture and");
        ini.comment(s, "widens a narrow carrier to the width of the interpolation.");
        ini.set_bool(s, "smooth", self.smooth);
        ini.set_bool(s, "show_stations", self.show_stations);
        ini.comment(s, "show_held_trace overlays the decayed peak hold the channel");
        ini.comment(s, "allocator searches, which is not the same surface as the live trace.");
        ini.set_bool(s, "show_held_trace", self.show_held_trace);
        ini.set_bool(s, "mark_decoders", self.mark_decoders);
        ini.comment(s, "show_average_trace overlays the long power average, which is what");
        ini.comment(s, "finds a carrier below the noise of one frame and loses anything that");
        ini.comment(s, "changes. time_axis states the age of the history down the left edge.");
        ini.set_bool(s, "show_average_trace", self.show_average_trace);
        ini.set_bool(s, "time_axis", self.time_axis);
        ini.comment(s, "gpu_palette stores one channel and maps it in the shader, which is a");
        ini.comment(s, "quarter of the memory and lets a palette or gamma change repaint the");
        ini.comment(s, "whole history. Applied at the next start.");
        ini.set_bool(s, "gpu_palette", self.gpu_palette);
        
    }
}
// ---------------------------------------------------------------- meter

#[derive(Debug, Clone)]
pub struct MeterSettings {
    pub scale: MeterScale,
    /// dBFS level that corresponds to S9 on the displayed scale.
    pub s9_reference_dbfs: f32,
    /// Extra offset for external attenuators or preamps.
    pub calibration_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub peak_hold_ms: f32,
    pub show_peak: bool,
    pub show_numeric: bool,
    /// Measure the whole passband or only the decoder bandwidth.
    pub narrow_band_measure: bool,
}

impl Default for MeterSettings {
    fn default() -> Self {
        MeterSettings {
            scale: MeterScale::SUnits,
            s9_reference_dbfs: -30.0,
            calibration_db: 0.0,
            attack_ms: 10.0,
            release_ms: 300.0,
            peak_hold_ms: 800.0,
            show_peak: true,
            show_numeric: true,
            narrow_band_measure: true,
        }
    }
}

impl SectionIo for MeterSettings {
    const NAME: &'static str = "meter";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        MeterSettings {
            scale: ini.get_enum(Self::NAME, "scale", d.scale),
            s9_reference_dbfs: ini.get_f32_clamped(Self::NAME, "s9_reference_dbfs", d.s9_reference_dbfs, -120.0, 0.0),
            calibration_db: ini.get_f32_clamped(Self::NAME, "calibration_db", d.calibration_db, -60.0, 60.0),
            attack_ms: ini.get_f32_clamped(Self::NAME, "attack_ms", d.attack_ms, 0.5, 500.0),
            release_ms: ini.get_f32_clamped(Self::NAME, "release_ms", d.release_ms, 10.0, 5000.0),
            peak_hold_ms: ini.get_f32_clamped(Self::NAME, "peak_hold_ms", d.peak_hold_ms, 0.0, 10000.0),
            show_peak: ini.get_bool(Self::NAME, "show_peak", d.show_peak),
            show_numeric: ini.get_bool(Self::NAME, "show_numeric", d.show_numeric),
            narrow_band_measure: ini.get_bool(Self::NAME, "narrow_band_measure", d.narrow_band_measure),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "scale: s_units, dbm, dbfs.");
        ini.set_enum(s, "scale", self.scale);
        ini.comment(s, "S9 reference in dBFS of the sound card input. Calibrate once per rig.");
        ini.set_f32(s, "s9_reference_dbfs", self.s9_reference_dbfs);
        ini.set_f32(s, "calibration_db", self.calibration_db);
        ini.set_f32(s, "attack_ms", self.attack_ms);
        ini.set_f32(s, "release_ms", self.release_ms);
        ini.set_f32(s, "peak_hold_ms", self.peak_hold_ms);
        ini.set_bool(s, "show_peak", self.show_peak);
        ini.set_bool(s, "show_numeric", self.show_numeric);
        ini.comment(s, "narrow_band_measure limits the reading to the band being worked: the");
        ini.comment(s, "receiver filter in the receiver mode and the keying detector otherwise.");
        ini.comment(s, "Taken as a ratio against the whole passband and applied to the same");
        ini.comment(s, "figure the wide reading uses, so the calibration above stays valid.");
        ini.set_bool(s, "narrow_band_measure", self.narrow_band_measure);
    }
}

// ---------------------------------------------------------------- morse

#[derive(Debug, Clone)]
pub struct MorseSettings {
    pub enabled: bool,
    pub auto_tone: bool,
    /// Track the tone inside the detector passband. Independent of auto_tone,
    /// which chooses which signal to listen to; this keeps the detector centred
    /// on the signal already chosen.
    pub afc: bool,
    /// Radius the click search covers, and the limit the tracking may move from
    /// the anchor. One value for both: they answer the same question, how far
    /// from the stated frequency the signal may be.
    pub capture_range_hz: f32,
    pub tone_hz: f32,
    pub filter_bandwidth_hz: f32,
    /// Decode several carriers at once. With this off the bank holds exactly one
    /// channel, anchored by tone_hz or by the automatic tracker, which is the
    /// behaviour of a conventional single signal decoder.
    pub multi_channel: bool,
    /// Upper bound on simultaneously decoded carriers. Each one costs about half
    /// a million operations per second of audio, so the practical limit is set
    /// by how much text the operator can read rather than by the processor.
    pub max_channels: u32,
    /// Smallest separation two channels may have. Two peaks closer than this are
    /// one signal seen twice, most often a carrier and its keying sideband, so
    /// the weaker one is not given a channel of its own.
    pub channel_spacing_hz: f32,
    /// Confidence a channel needs before its text is printed. This is a keying
    /// decision, taken per channel from the pattern match rate of that channel
    /// alone, and it is deliberately not the classifier threshold: the
    /// classifier answers what the band is carrying, which is a different
    /// question and a stricter one. Zero prints everything the detector
    /// assembles, which is what a diagnostic session wants.
    pub print_threshold: f32,
    pub auto_speed: bool,
    pub wpm: f32,
    pub wpm_min: f32,
    pub wpm_max: f32,
    /// Adaptation rate of the element length tracker, 0 to 1.
    pub speed_tracking: f32,
    pub adaptive_threshold: bool,
    pub squelch_db: f32,
    pub min_snr_db: f32,
    /// Multipliers of the dot length used to split characters and words.
    pub char_gap_factor: f32,
    pub word_gap_factor: f32,
    pub farnsworth_aware: bool,
    pub output_case: TextCase,
    pub show_prosigns: bool,
    /// Insert a marker when the decoder loses sync.
    pub mark_dropouts: bool,
}

impl Default for MorseSettings {
    fn default() -> Self {
        MorseSettings {
            enabled: true,
            auto_tone: true,
            afc: true,
            capture_range_hz: 150.0,
            tone_hz: 700.0,
            filter_bandwidth_hz: 200.0,
            multi_channel: true,
            max_channels: 4,
            channel_spacing_hz: 120.0,
            // Above what a channel scores before it has produced any pattern the
            // alphabet could not have held by chance, which is the one state
            // where the confidence rests on nothing.
            print_threshold: 0.45,
            auto_speed: true,
            wpm: 20.0,
            wpm_min: 5.0,
            wpm_max: 60.0,
            speed_tracking: 0.25,
            adaptive_threshold: true,
            // A backstop rather than the operating control. The absolute level
            // of a narrow detector moves with its own width, which the speed
            // tracker changes by itself, and with the band noise, so a threshold
            // set against it stops meaning the same thing within an hour. Low
            // enough here to catch a dead input and nothing else.
            squelch_db: -120.0,
            // The operating control. The level above the noise floor keeps its
            // meaning whatever the width and the band do, which the absolute one
            // cannot. Eight decibels is above what the ratio of two percentiles
            // of the same noise produces and below any signal worth decoding.
            min_snr_db: 8.0,
            char_gap_factor: 2.0,
            word_gap_factor: 5.0,
            farnsworth_aware: true,
            output_case: TextCase::Upper,
            show_prosigns: true,
            mark_dropouts: false,
        }
    }
}

impl SectionIo for MorseSettings {
    const NAME: &'static str = "morse";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        let wpm_min = ini.get_f32_clamped(Self::NAME, "wpm_min", d.wpm_min, 3.0, 100.0);
        let wpm_max = ini.get_f32_clamped(Self::NAME, "wpm_max", d.wpm_max, wpm_min + 1.0, 200.0);
        MorseSettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            auto_tone: ini.get_bool(Self::NAME, "auto_tone", d.auto_tone),
            afc: ini.get_bool(Self::NAME, "afc", d.afc),
            capture_range_hz: ini.get_f32_clamped(Self::NAME, "capture_range_hz", d.capture_range_hz, 10.0, 500.0),
            // Two sided, because a quadrature input has a band below the tuning
            // point and it is half of what the receiver captured.
            tone_hz: ini.get_f32_clamped(Self::NAME, "tone_hz", d.tone_hz, -24000.0, 24000.0),
            filter_bandwidth_hz: ini.get_f32_clamped(Self::NAME, "filter_bandwidth_hz", d.filter_bandwidth_hz, 20.0, 1000.0),
            multi_channel: ini.get_bool(Self::NAME, "multi_channel", d.multi_channel),
            max_channels: ini.get_u32_clamped(Self::NAME, "max_channels", d.max_channels, 1, 16),
            channel_spacing_hz: ini.get_f32_clamped(Self::NAME, "channel_spacing_hz", d.channel_spacing_hz, 40.0, 1000.0),
            print_threshold: ini.get_f32_clamped(Self::NAME, "print_threshold", d.print_threshold, 0.0, 1.0),
            auto_speed: ini.get_bool(Self::NAME, "auto_speed", d.auto_speed),
            wpm: ini.get_f32_clamped(Self::NAME, "wpm", d.wpm, wpm_min, wpm_max),
            wpm_min,
            wpm_max,
            speed_tracking: ini.get_f32_clamped(Self::NAME, "speed_tracking", d.speed_tracking, 0.0, 1.0),
            adaptive_threshold: ini.get_bool(Self::NAME, "adaptive_threshold", d.adaptive_threshold),
            squelch_db: ini.get_f32_clamped(Self::NAME, "squelch_db", d.squelch_db, -140.0, 0.0),
            min_snr_db: ini.get_f32_clamped(Self::NAME, "min_snr_db", d.min_snr_db, -10.0, 40.0),
            char_gap_factor: ini.get_f32_clamped(Self::NAME, "char_gap_factor", d.char_gap_factor, 1.2, 4.0),
            word_gap_factor: ini.get_f32_clamped(Self::NAME, "word_gap_factor", d.word_gap_factor, 3.0, 12.0),
            farnsworth_aware: ini.get_bool(Self::NAME, "farnsworth_aware", d.farnsworth_aware),
            output_case: ini.get_enum(Self::NAME, "output_case", d.output_case),
            show_prosigns: ini.get_bool(Self::NAME, "show_prosigns", d.show_prosigns),
            mark_dropouts: ini.get_bool(Self::NAME, "mark_dropouts", d.mark_dropouts),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.set_bool(s, "enabled", self.enabled);
        ini.comment(s, "auto_tone locks the detector onto the strongest tone in the passband.");
        ini.comment(s, "It applies to single channel operation only; with multi_channel on,");
        ini.comment(s, "channels are allocated from the peak list instead.");
        ini.set_bool(s, "auto_tone", self.auto_tone);
        ini.set_f32(s, "tone_hz", self.tone_hz);
        ini.comment(s, "afc keeps the detector centred once a signal is selected.");
        ini.comment(s, "capture_range_hz is how far a click may miss the carrier.");
        ini.set_bool(s, "afc", self.afc);
        ini.set_f32(s, "capture_range_hz", self.capture_range_hz);
        ini.comment(s, "filter_bandwidth_hz is the equivalent noise bandwidth of the keying");
        ini.comment(s, "detector. It is widened automatically when the working speed needs a");
        ini.comment(s, "shorter analysis window.");
        ini.set_f32(s, "filter_bandwidth_hz", self.filter_bandwidth_hz);
        ini.comment(s, "Multi channel operation. Each channel is an independent detector with");
        ini.comment(s, "its own speed, threshold and text line; channel_spacing_hz is the");
        ini.comment(s, "smallest separation two of them may have.");
        ini.set_bool(s, "multi_channel", self.multi_channel);
        ini.set_u32(s, "max_channels", self.max_channels);
        ini.set_f32(s, "channel_spacing_hz", self.channel_spacing_hz);
        ini.comment(s, "print_threshold gates the text of one channel by the confidence of");
        ini.comment(s, "that channel. Zero prints everything, which is the diagnostic setting.");
        ini.set_f32(s, "print_threshold", self.print_threshold);
        ini.comment(s, "auto_speed estimates WPM from the element histogram.");
        ini.set_bool(s, "auto_speed", self.auto_speed);
        ini.comment(s, "wpm is the working speed the analysis window is sized for, not just a");
        ini.comment(s, "starting point: wpm_min and wpm_max only bound where the tracker may go.");
        ini.set_f32(s, "wpm", self.wpm);
        ini.set_f32(s, "wpm_min", self.wpm_min);
        ini.set_f32(s, "wpm_max", self.wpm_max);
        ini.set_f32(s, "speed_tracking", self.speed_tracking);
        ini.set_bool(s, "adaptive_threshold", self.adaptive_threshold);
        ini.comment(s, "Two gates and only the second is the operating one. squelch_db is an");
        ini.comment(s, "absolute level and moves with the detector width, which the speed");
        ini.comment(s, "tracker changes by itself, and with the band noise: a threshold set");
        ini.comment(s, "against it stops meaning the same thing within an hour. It is a");
        ini.comment(s, "backstop for a dead input. min_snr_db is the level above the measured");
        ini.comment(s, "noise floor, which keeps its meaning whatever the two of them do.");
        ini.set_f32(s, "squelch_db", self.squelch_db);
        ini.set_f32(s, "min_snr_db", self.min_snr_db);
        ini.comment(s, "Gap factors in dot units, used to place letter and word breaks.");
        ini.set_f32(s, "char_gap_factor", self.char_gap_factor);
        ini.set_f32(s, "word_gap_factor", self.word_gap_factor);
        ini.set_bool(s, "farnsworth_aware", self.farnsworth_aware);
        ini.comment(s, "output_case: upper, lower, as_received.");
        ini.set_enum(s, "output_case", self.output_case);
        ini.set_bool(s, "show_prosigns", self.show_prosigns);
        ini.set_bool(s, "mark_dropouts", self.mark_dropouts);
    }
}

// ----------------------------------------------------------------- rtty

#[derive(Debug, Clone)]
pub struct RttySettings {
    pub enabled: bool,
    pub alphabet: RttyAlphabet,
    pub baud: f32,
    pub shift_hz: f32,
    pub mark_hz: f32,
    pub invert: bool,
    pub data_bits: u32,
    pub stop_bits: f32,
    pub parity: RttyParity,
    /// Unshift on space, the classic USOS behaviour.
    pub usos: bool,
    pub auto_baud: bool,
    pub auto_shift: bool,
    pub afc: bool,
    pub afc_range_hz: f32,
    pub squelch_db: f32,
    /// Threshold correction for selective fading, 0 disables it.
    pub atc: f32,
    pub bit_inversion_retry: bool,
}

impl Default for RttySettings {
    fn default() -> Self {
        RttySettings {
            enabled: true,
            alphabet: RttyAlphabet::Baudot,
            baud: 45.45,
            shift_hz: 170.0,
            mark_hz: 2125.0,
            invert: false,
            data_bits: 5,
            stop_bits: 1.5,
            parity: RttyParity::None,
            usos: true,
            auto_baud: true,
            auto_shift: true,
            afc: true,
            afc_range_hz: 100.0,
            squelch_db: -90.0,
            atc: 0.5,
            bit_inversion_retry: true,
        }
    }
}

impl SectionIo for RttySettings {
    const NAME: &'static str = "rtty";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        RttySettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            alphabet: ini.get_enum(Self::NAME, "alphabet", d.alphabet),
            baud: ini.get_f32_clamped(Self::NAME, "baud", d.baud, 10.0, 1200.0),
            shift_hz: ini.get_f32_clamped(Self::NAME, "shift_hz", d.shift_hz, 20.0, 2000.0),
            mark_hz: ini.get_f32_clamped(Self::NAME, "mark_hz", d.mark_hz, -24000.0, 24000.0),
            invert: ini.get_bool(Self::NAME, "invert", d.invert),
            data_bits: ini.get_u32_clamped(Self::NAME, "data_bits", d.data_bits, 5, 8),
            stop_bits: ini.get_f32_clamped(Self::NAME, "stop_bits", d.stop_bits, 1.0, 2.0),
            parity: ini.get_enum(Self::NAME, "parity", d.parity),
            usos: ini.get_bool(Self::NAME, "usos", d.usos),
            auto_baud: ini.get_bool(Self::NAME, "auto_baud", d.auto_baud),
            auto_shift: ini.get_bool(Self::NAME, "auto_shift", d.auto_shift),
            afc: ini.get_bool(Self::NAME, "afc", d.afc),
            afc_range_hz: ini.get_f32_clamped(Self::NAME, "afc_range_hz", d.afc_range_hz, 5.0, 500.0),
            squelch_db: ini.get_f32_clamped(Self::NAME, "squelch_db", d.squelch_db, -140.0, 0.0),
            atc: ini.get_f32_clamped(Self::NAME, "atc", d.atc, 0.0, 1.0),
            bit_inversion_retry: ini.get_bool(Self::NAME, "bit_inversion_retry", d.bit_inversion_retry),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.set_bool(s, "enabled", self.enabled);
        ini.comment(s, "alphabet: baudot for classic RTTY, ascii for 7 or 8 bit links.");
        ini.set_enum(s, "alphabet", self.alphabet);
        ini.comment(s, "Common combinations: 45.45 baud 170 Hz, 50 baud 425 Hz, 75 baud 850 Hz.");
        ini.set_f32(s, "baud", self.baud);
        ini.set_f32(s, "shift_hz", self.shift_hz);
        ini.comment(s, "mark_hz is the higher tone by convention, space is mark minus shift.");
        ini.set_f32(s, "mark_hz", self.mark_hz);
        ini.set_bool(s, "invert", self.invert);
        ini.set_u32(s, "data_bits", self.data_bits);
        ini.set_f32(s, "stop_bits", self.stop_bits);
        ini.comment(s, "parity: none, even, odd, mark, space.");
        ini.set_enum(s, "parity", self.parity);
        ini.set_bool(s, "usos", self.usos);
        ini.comment(s, "auto_baud and auto_shift feed from the classifier estimates.");
        ini.set_bool(s, "auto_baud", self.auto_baud);
        ini.set_bool(s, "auto_shift", self.auto_shift);
        ini.set_bool(s, "afc", self.afc);
        ini.set_f32(s, "afc_range_hz", self.afc_range_hz);
        ini.set_f32(s, "squelch_db", self.squelch_db);
        ini.comment(s, "atc compensates one faded tone, 0 disables the correction.");
        ini.set_f32(s, "atc", self.atc);
        ini.set_bool(s, "bit_inversion_retry", self.bit_inversion_retry);
    }
}

// ------------------------------------------------------------------ psk

/// Phase shift keying at thirty one and a quarter baud.
///
/// The symbol rate is not a setting. The format states it as two thousand over
/// sixty four and every station on the air derives it the same way, so there is
/// nothing to choose and nothing to estimate; a control offering it would be a
/// control that can only be set wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct PskSettings {
    pub enabled: bool,
    /// Frequency the demodulator is pointed at, in hertz.
    pub centre_hz: f32,
    /// Follow the strongest carrier the peak search found.
    pub auto_centre: bool,
    pub afc: bool,
    /// Bound on the correction, in hertz.
    ///
    /// Capped at a quarter of the symbol rate by the demodulator, which is where
    /// the phase measurement stops being able to tell a positive error from a
    /// negative one.
    pub afc_range_hz: f32,
    pub squelch_db: f32,
    /// Framing confidence the text needs before it is printed.
    ///
    /// The framing accepts any run of bits between two boundaries, so noise
    /// assembles codes and some of them resolve. The share that resolves is the
    /// one figure that separates a signal from an empty band.
    pub print_threshold: f32,
}

impl Default for PskSettings {
    fn default() -> Self {
        PskSettings {
            enabled: true,
            centre_hz: 1000.0,
            auto_centre: true,
            afc: true,
            afc_range_hz: 7.0,
            squelch_db: -100.0,
            // High, because the framing is a weak test on its own. The alphabet
            // holds most of the short patterns it accepts, so noise assembles
            // codes that resolve at a rate a low threshold would admit, and the
            // result is a page of punctuation that reads as a receiver fault.
            print_threshold: 0.8,
        }
    }
}

impl SectionIo for PskSettings {
    const NAME: &'static str = "psk";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        PskSettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            centre_hz: ini.get_f32_clamped(Self::NAME, "centre_hz", d.centre_hz, -24000.0, 24000.0),
            auto_centre: ini.get_bool(Self::NAME, "auto_centre", d.auto_centre),
            afc: ini.get_bool(Self::NAME, "afc", d.afc),
            afc_range_hz: ini.get_f32_clamped(Self::NAME, "afc_range_hz", d.afc_range_hz, 1.0, 7.8),
            squelch_db: ini.get_f32_clamped(Self::NAME, "squelch_db", d.squelch_db, -140.0, 0.0),
            print_threshold: ini.get_f32_clamped(Self::NAME, "print_threshold", d.print_threshold, 0.0, 1.0),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Phase shift keying at thirty one and a quarter baud. The symbol rate");
        ini.comment(s, "is stated by the format rather than chosen, so it is not a setting.");
        ini.set_bool(s, "enabled", self.enabled);
        ini.comment(s, "auto_centre follows the strongest carrier found in the passband, which");
        ini.comment(s, "for this format is the signal itself: it is a carrier with its phase");
        ini.comment(s, "reversed rather than a tone that is switched on and off.");
        ini.set_bool(s, "auto_centre", self.auto_centre);
        ini.set_f32(s, "centre_hz", self.centre_hz);
        ini.comment(s, "The correction is read from the phase step between two symbols, which");
        ini.comment(s, "cannot tell a positive error from a negative one past a quarter of the");
        ini.comment(s, "symbol rate. That is the ceiling on the range below.");
        ini.set_bool(s, "afc", self.afc);
        ini.set_f32(s, "afc_range_hz", self.afc_range_hz);
        ini.set_f32(s, "squelch_db", self.squelch_db);
        ini.comment(s, "print_threshold gates the text on the share of codes that resolved.");
        ini.comment(s, "Nought prints whatever the framing assembled, which on an empty band");
        ini.comment(s, "is a page of punctuation.");
        ini.set_f32(s, "print_threshold", self.print_threshold);
    }
}

// ----------------------------------------------------------- classifier

#[derive(Debug, Clone)]
pub struct ClassifierSettings {
    pub enabled: bool,
    pub analysis_window_ms: u32,
    pub update_interval_ms: u32,
    pub min_confidence: f32,
    /// Time a decision stays latched before a new mode can win.
    pub hold_time_ms: u32,
    pub detect_cw: bool,
    pub detect_rtty: bool,
    pub detect_psk31: bool,
    pub detect_navtex: bool,
    pub baud_search_min: f32,
    pub baud_search_max: f32,
    pub shift_search_min: f32,
    pub shift_search_max: f32,
    /// Automatically switch the active decoder to the detected mode.
    pub auto_switch_decoder: bool,
    pub announce_in_log: bool,
}

impl Default for ClassifierSettings {
    fn default() -> Self {
        ClassifierSettings {
            enabled: true,
            analysis_window_ms: 2000,
            update_interval_ms: 250,
            min_confidence: 0.6,
            hold_time_ms: 3000,
            detect_cw: true,
            detect_rtty: true,
            detect_psk31: true,
            detect_navtex: true,
            baud_search_min: 30.0,
            baud_search_max: 120.0,
            shift_search_min: 100.0,
            shift_search_max: 1000.0,
            auto_switch_decoder: true,
            announce_in_log: true,
        }
    }
}

impl SectionIo for ClassifierSettings {
    const NAME: &'static str = "classifier";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        ClassifierSettings {
            enabled: ini.get_bool(Self::NAME, "enabled", d.enabled),
            analysis_window_ms: ini.get_u32_clamped(Self::NAME, "analysis_window_ms", d.analysis_window_ms, 200, 20000),
            update_interval_ms: ini.get_u32_clamped(Self::NAME, "update_interval_ms", d.update_interval_ms, 50, 5000),
            min_confidence: ini.get_f32_clamped(Self::NAME, "min_confidence", d.min_confidence, 0.1, 1.0),
            hold_time_ms: ini.get_u32_clamped(Self::NAME, "hold_time_ms", d.hold_time_ms, 0, 60000),
            detect_cw: ini.get_bool(Self::NAME, "detect_cw", d.detect_cw),
            detect_rtty: ini.get_bool(Self::NAME, "detect_rtty", d.detect_rtty),
            detect_psk31: ini.get_bool(Self::NAME, "detect_psk31", d.detect_psk31),
            detect_navtex: ini.get_bool(Self::NAME, "detect_navtex", d.detect_navtex),
            baud_search_min: ini.get_f32_clamped(Self::NAME, "baud_search_min", d.baud_search_min, 10.0, 500.0),
            baud_search_max: ini.get_f32_clamped(Self::NAME, "baud_search_max", d.baud_search_max, 20.0, 2000.0),
            shift_search_min: ini.get_f32_clamped(Self::NAME, "shift_search_min", d.shift_search_min, 20.0, 2000.0),
            shift_search_max: ini.get_f32_clamped(Self::NAME, "shift_search_max", d.shift_search_max, 50.0, 4000.0),
            auto_switch_decoder: ini.get_bool(Self::NAME, "auto_switch_decoder", d.auto_switch_decoder),
            announce_in_log: ini.get_bool(Self::NAME, "announce_in_log", d.announce_in_log),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Automatic mode recognition over the selected passband.");
        ini.set_bool(s, "enabled", self.enabled);
        ini.set_u32(s, "analysis_window_ms", self.analysis_window_ms);
        ini.set_u32(s, "update_interval_ms", self.update_interval_ms);
        ini.set_f32(s, "min_confidence", self.min_confidence);
        ini.set_u32(s, "hold_time_ms", self.hold_time_ms);
        ini.set_bool(s, "detect_cw", self.detect_cw);
        ini.set_bool(s, "detect_rtty", self.detect_rtty);
        ini.set_bool(s, "detect_psk31", self.detect_psk31);
        ini.set_bool(s, "detect_navtex", self.detect_navtex);
        ini.comment(s, "Search ranges for the FSK baud and shift estimators.");
        ini.set_f32(s, "baud_search_min", self.baud_search_min);
        ini.set_f32(s, "baud_search_max", self.baud_search_max);
        ini.set_f32(s, "shift_search_min", self.shift_search_min);
        ini.set_f32(s, "shift_search_max", self.shift_search_max);
        ini.set_bool(s, "auto_switch_decoder", self.auto_switch_decoder);
        ini.set_bool(s, "announce_in_log", self.announce_in_log);
    }
}

// ------------------------------------------------------------- callsign

#[derive(Debug, Clone)]
pub struct CallsignSettings {
    pub lookup_enabled: bool,
    pub source: CallsignSource,
    /// cty.dat style prefix database for country and zone resolution.
    pub prefix_db_path: String,
    /// Optional flat text or CSV database of known stations.
    pub local_db_path: String,
    pub cache_entries: u32,
    pub auto_lookup_on_decode: bool,
    pub min_callsign_length: u32,
    /// Highlight recognized callsigns in the decode window.
    pub highlight_in_text: bool,
    pub history_path: String,
    pub history_limit: u32,
}

impl Default for CallsignSettings {
    fn default() -> Self {
        CallsignSettings {
            lookup_enabled: true,
            source: CallsignSource::Cty,
            prefix_db_path: "data/cty.dat".to_string(),
            local_db_path: String::new(),
            cache_entries: 4096,
            auto_lookup_on_decode: true,
            min_callsign_length: 4,
            highlight_in_text: true,
            history_path: "logs/callsigns.txt".to_string(),
            history_limit: 5000,
        }
    }
}

impl SectionIo for CallsignSettings {
    const NAME: &'static str = "callsign";

    fn load(ini: &Ini) -> Self {
        let d = Self::default();
        CallsignSettings {
            lookup_enabled: ini.get_bool(Self::NAME, "lookup_enabled", d.lookup_enabled),
            source: ini.get_enum(Self::NAME, "source", d.source),
            prefix_db_path: ini.get_string(Self::NAME, "prefix_db_path", &d.prefix_db_path),
            local_db_path: ini.get_string(Self::NAME, "local_db_path", &d.local_db_path),
            cache_entries: ini.get_u32_clamped(Self::NAME, "cache_entries", d.cache_entries, 64, 1_000_000),
            auto_lookup_on_decode: ini.get_bool(Self::NAME, "auto_lookup_on_decode", d.auto_lookup_on_decode),
            min_callsign_length: ini.get_u32_clamped(Self::NAME, "min_callsign_length", d.min_callsign_length, 3, 12),
            highlight_in_text: ini.get_bool(Self::NAME, "highlight_in_text", d.highlight_in_text),
            history_path: ini.get_string(Self::NAME, "history_path", &d.history_path),
            history_limit: ini.get_u32_clamped(Self::NAME, "history_limit", d.history_limit, 0, 1_000_000),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "Offline callsign resolution. Online services are not used yet.");
        ini.set_bool(s, "lookup_enabled", self.lookup_enabled);
        ini.comment(s, "source: cty, local_file, none.");
        ini.set_enum(s, "source", self.source);
        ini.set_string(s, "prefix_db_path", &self.prefix_db_path);
        ini.set_string(s, "local_db_path", &self.local_db_path);
        ini.set_u32(s, "cache_entries", self.cache_entries);
        ini.set_bool(s, "auto_lookup_on_decode", self.auto_lookup_on_decode);
        ini.set_u32(s, "min_callsign_length", self.min_callsign_length);
        ini.set_bool(s, "highlight_in_text", self.highlight_in_text);
        ini.set_string(s, "history_path", &self.history_path);
        ini.set_u32(s, "history_limit", self.history_limit);
    }
}

// ------------------------------------------------------------------- ui

#[derive(Debug, Clone)]
pub struct UiSettings {
    /// Extra scale applied on top of the system DPI factor.
    pub scale: f32,
    pub font_path: String,
    pub mono_font_path: String,
    pub font_size_pt: f32,
    pub decode_font_size_pt: f32,
    /// Coverage curve applied to glyph bitmaps. Values above 1.0 thicken
    /// light text on a dark background.
    pub text_gamma: f32,
    /// Square side of the glyph atlas texture in pixels.
    pub glyph_atlas_size: u32,
    /// Accent colour as 0xRRGGBB.
    pub accent_rgb: u32,
    pub vsync: bool,
    /// 0 means no frame limiter, only meaningful with vsync off.
    pub target_fps: u32,
    pub window_x: i32,
    pub window_y: i32,
    pub window_width: u32,
    pub window_height: u32,
    pub maximized: bool,
    pub show_settings_panel: bool,
    pub show_meter: bool,
    pub show_decode_log: bool,
    pub decode_panel_fraction: f32,
    pub side_panel_width: f32,
    pub show_debug_overlay: bool,
    /// Folded state of the band panel.
    ///
    /// Stored because a panel occupying a third of the window is part of the
    /// arrangement an operator made, and losing it on restart means making it
    /// again.
    pub band_panel_open: bool,
    /// Language code, empty or "en" selects the reference wording.
    pub language: String,
    /// Directory holding the translation files, relative to the config file.
    pub localization_path: String,
}

impl Default for UiSettings {
    fn default() -> Self {
        UiSettings {
            scale: 1.0,
            font_path: String::new(),
            mono_font_path: String::new(),
            font_size_pt: 12.0,
            decode_font_size_pt: 14.0,
            accent_rgb: 0x2F81F7,
            vsync: true,
            target_fps: 0,
            window_x: -1,
            window_y: -1,
            window_width: 1280,
            window_height: 800,
            maximized: false,
            show_settings_panel: true,
            show_meter: true,
            show_decode_log: true,
            decode_panel_fraction: 0.3,
            side_panel_width: 320.0,
            show_debug_overlay: false,
            band_panel_open: true,
            language: String::new(),
            localization_path: "lang".to_string(),
            text_gamma: 1.2,
            glyph_atlas_size: 1024,
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
            decode_font_size_pt: ini.get_f32_clamped(Self::NAME, "decode_font_size_pt", d.decode_font_size_pt, 6.0, 64.0),
            text_gamma: ini.get_f32_clamped(Self::NAME, "text_gamma", d.text_gamma, 0.5, 3.0),
            glyph_atlas_size: ini.get_u32_clamped(Self::NAME, "glyph_atlas_size", d.glyph_atlas_size, 256, 8192),
            accent_rgb: accent,
            vsync: ini.get_bool(Self::NAME, "vsync", d.vsync),
            target_fps: ini.get_u32_clamped(Self::NAME, "target_fps", d.target_fps, 0, 480),
            window_x: ini.get_i32(Self::NAME, "window_x", d.window_x),
            window_y: ini.get_i32(Self::NAME, "window_y", d.window_y),
            window_width: ini.get_u32_clamped(Self::NAME, "window_width", d.window_width, 640, 16384),
            window_height: ini.get_u32_clamped(Self::NAME, "window_height", d.window_height, 400, 16384),
            maximized: ini.get_bool(Self::NAME, "maximized", d.maximized),
            show_settings_panel: ini.get_bool(Self::NAME, "show_settings_panel", d.show_settings_panel),
            show_meter: ini.get_bool(Self::NAME, "show_meter", d.show_meter),
            show_decode_log: ini.get_bool(Self::NAME, "show_decode_log", d.show_decode_log),
            decode_panel_fraction: ini.get_f32_clamped(Self::NAME, "decode_panel_fraction", d.decode_panel_fraction, 0.1, 0.8),
            side_panel_width: ini.get_f32_clamped(Self::NAME, "side_panel_width", d.side_panel_width, 180.0, 900.0),
            show_debug_overlay: ini.get_bool(Self::NAME, "show_debug_overlay", d.show_debug_overlay),
            band_panel_open: ini.get_bool(Self::NAME, "band_panel_open", d.band_panel_open),
            language: ini.get_string(Self::NAME, "language", &d.language),
            localization_path: ini.get_string(Self::NAME, "localization_path", &d.localization_path),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "UI scale multiplies the system DPI factor.");
        ini.set_f32(s, "scale", self.scale);
        ini.comment(s, "Font files are TTF, parsed internally. Empty falls back to a");
        ini.comment(s, "system font search under the Windows Fonts directory.");
        ini.set_string(s, "font_path", &self.font_path);
        ini.set_string(s, "mono_font_path", &self.mono_font_path);
        ini.set_f32(s, "font_size_pt", self.font_size_pt);
        ini.set_f32(s, "decode_font_size_pt", self.decode_font_size_pt);
        ini.comment(s, "text_gamma above 1.0 thickens antialiased glyph edges.");
        ini.set_f32(s, "text_gamma", self.text_gamma);
        ini.comment(s, "glyph_atlas_size is the side of the single channel atlas texture.");
        ini.set_u32(s, "glyph_atlas_size", self.glyph_atlas_size);
        ini.comment(s, "accent_rgb is a hex colour, used for focus and active states.");
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
        ini.set_bool(s, "show_meter", self.show_meter);
        ini.set_bool(s, "show_decode_log", self.show_decode_log);
        ini.set_f32(s, "decode_panel_fraction", self.decode_panel_fraction);
        ini.set_f32(s, "side_panel_width", self.side_panel_width);
        ini.set_bool(s, "show_debug_overlay", self.show_debug_overlay);
        ini.set_bool(s, "band_panel_open", self.band_panel_open);
        ini.comment(s, "language selects a file <code>.lang from localization_path.");
        ini.comment(s, "Empty or en uses the wording built into the executable.");
        ini.set_string(s, "language", &self.language);
        ini.set_string(s, "localization_path", &self.localization_path);
    }
}

// ------------------------------------------------------------------ log

#[derive(Debug, Clone)]
pub struct LogSettings {
    pub level: Level,
    pub file_path: String,
    pub to_debugger: bool,
    /// Rotate when the file exceeds this size, 0 disables rotation.
    pub max_size_kb: u32,
    /// Append every decoded line to a separate transcript file.
    pub transcript_enabled: bool,
    pub transcript_path: String,
}

impl Default for LogSettings {
    fn default() -> Self {
        LogSettings {
            level: Level::Info,
            file_path: "logs/rxscope.log".to_string(),
            to_debugger: true,
            max_size_kb: 4096,
            transcript_enabled: true,
            transcript_path: "logs/transcript.txt".to_string(),
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
            transcript_enabled: ini.get_bool(Self::NAME, "transcript_enabled", d.transcript_enabled),
            transcript_path: ini.get_string(Self::NAME, "transcript_path", &d.transcript_path),
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
        ini.set_bool(s, "transcript_enabled", self.transcript_enabled);
        ini.set_string(s, "transcript_path", &self.transcript_path);
    }
}

// --------------------------------------------------------------- render

#[derive(Debug, Clone)]
pub struct RenderSettings {
    pub validation: bool,
    /// Substring match against the GPU name, empty means automatic choice.
    pub device_name: String,
    /// Explicit index from the enumeration order, -1 means automatic.
    pub device_index: i32,
    pub frames_in_flight: u32,
    /// 0 lets the driver minimum plus one decide.
    pub swapchain_images: u32,
    pub present_mode: PresentModeCfg,
    /// Window background as 0xRRGGBB.
    pub background_rgb: u32,
    pub vertex_buffer_kb: u32,
    pub index_buffer_kb: u32,
    pub max_textures: u32,
    pub log_device_info: bool,
    /// Measure the device side frame time with timestamp queries.
    ///
    /// Two commands per frame and a query pool of two entries per frame slot,
    /// which is negligible beside a few dozen draw commands. Present as a switch
    /// because a timestamp query is one of the few paths where implementations
    /// have differed, and because the reading is diagnostic rather than
    /// operational.
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
            vertex_buffer_kb: 512,
            index_buffer_kb: 256,
            max_textures: 64,
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
            frames_in_flight: ini.get_u32_clamped(Self::NAME, "frames_in_flight", d.frames_in_flight, 1, 4),
            swapchain_images: ini.get_u32_clamped(Self::NAME, "swapchain_images", d.swapchain_images, 0, 8),
            present_mode: ini.get_enum(Self::NAME, "present_mode", d.present_mode),
            background_rgb: background,
            vertex_buffer_kb: ini.get_u32_clamped(Self::NAME, "vertex_buffer_kb", d.vertex_buffer_kb, 64, 65536),
            index_buffer_kb: ini.get_u32_clamped(Self::NAME, "index_buffer_kb", d.index_buffer_kb, 32, 65536),
            max_textures: ini.get_u32_clamped(Self::NAME, "max_textures", d.max_textures, 4, 1024),
            log_device_info: ini.get_bool(Self::NAME, "log_device_info", d.log_device_info),
            gpu_timing: ini.get_bool(Self::NAME, "gpu_timing", d.gpu_timing),
        }
    }

    fn store(&self, ini: &mut Ini) {
        let s = Self::NAME;
        ini.comment(s, "validation requires the Vulkan SDK layers, it is off in release.");
        ini.set_bool(s, "validation", self.validation);
        ini.comment(s, "device_name matches a substring of the GPU name, device_index is");
        ini.comment(s, "the enumeration position. Both empty or -1 means automatic.");
        ini.set_string(s, "device_name", &self.device_name);
        ini.set_i32(s, "device_index", self.device_index);
        ini.set_u32(s, "frames_in_flight", self.frames_in_flight);
        ini.set_u32(s, "swapchain_images", self.swapchain_images);
        ini.comment(s, "present_mode: auto, fifo, fifo_relaxed, mailbox, immediate.");
        ini.comment(s, "auto follows ui.vsync.");
        ini.set_enum(s, "present_mode", self.present_mode);
        ini.set_string(s, "background_rgb", &format!("{:06X}", self.background_rgb));
        ini.comment(s, "Initial geometry buffer sizes, they grow automatically.");
        ini.set_u32(s, "vertex_buffer_kb", self.vertex_buffer_kb);
        ini.set_u32(s, "index_buffer_kb", self.index_buffer_kb);
        ini.set_u32(s, "max_textures", self.max_textures);
        ini.set_bool(s, "log_device_info", self.log_device_info);
        ini.comment(s, "gpu_timing measures the device side frame time and feeds it to the");
        ini.comment(s, "debug overlay, which otherwise reports the processor side only. Off on");
        ini.comment(s, "a device whose queue reports no timestamp bits, whatever this says.");
        ini.set_bool(s, "gpu_timing", self.gpu_timing);
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

// -------------------------------------------------------------- settings

#[derive(Debug, Clone, Default)]
pub struct Settings {
    pub rig: RigSettings,
    pub receiver: ReceiverSettings,
    pub audio: AudioSettings,
    pub dsp: DspSettings,
    pub waterfall: WaterfallSettings,
    pub meter: MeterSettings,
    pub morse: MorseSettings,
    pub rtty: RttySettings,
    pub psk: PskSettings,
    pub classifier: ClassifierSettings,
    pub callsign: CallsignSettings,
    pub record: RecordSettings,
    pub bands: BandSettings,
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
            Ok(exe) => exe.with_file_name("rxscope.ini"),
            Err(_) => PathBuf::from("rxscope.ini"),
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
            rig: RigSettings::load(ini),
            receiver: ReceiverSettings::load(ini),
            audio: AudioSettings::load(ini),
            dsp: DspSettings::load(ini),
            waterfall: WaterfallSettings::load(ini),
            meter: MeterSettings::load(ini),
            morse: MorseSettings::load(ini),
            rtty: RttySettings::load(ini),
            psk: PskSettings::load(ini),
            classifier: ClassifierSettings::load(ini),
            callsign: CallsignSettings::load(ini),
            record: RecordSettings::load(ini),
            bands: BandSettings::load(ini),
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
        ini.comment("", "RXScope configuration");
        ini.comment("", "Generated automatically, edited values are preserved on restart.");
        ini.blank("");
        self.rig.store(&mut ini);
        self.receiver.store(&mut ini);
        self.audio.store(&mut ini);
        self.dsp.store(&mut ini);
        self.waterfall.store(&mut ini);
        self.meter.store(&mut ini);
        self.morse.store(&mut ini);
        self.rtty.store(&mut ini);
        self.psk.store(&mut ini);
        self.classifier.store(&mut ini);
        self.callsign.store(&mut ini);
        self.record.store(&mut ini);
        self.bands.store(&mut ini);
        self.panel.store(&mut ini);
        self.appearance.store(&mut ini);
        self.ui.store(&mut ini);
        self.render.store(&mut ini);
        self.log.store(&mut ini);
        ini
    }

    /// True when the receiver chain is in use.
    ///
    /// Asked wherever a control is offered, because several of them are not
    /// merely useless in the other mode but harmful: a gain loop, a noise
    /// reduction and a narrow filter each destroy something the keying detectors
    /// depend on. The interface greys them rather than hiding them, so the
    /// dependency is visible next to the setting that caused it.
    pub fn sdr_mode(&self) -> bool {
        self.receiver.mode == OperatingMode::Sdr
    }

    /// True when the operator stated that the input carries a quadrature pair.
    ///
    /// A property of the wiring rather than of what the audio is being used
    /// for, which is why the operating mode does not appear. A receiver that
    /// delivers a pair delivers one whether the application is demodulating it
    /// or watching it, and the spectrum of that pair has two distinguishable
    /// halves in both cases.
    ///
    /// Tying it to the receiver mode is the arrangement this replaces, and it
    /// produced exactly one visible symptom: in the skimmer the pair was reduced
    /// to one channel, so the spectrum folded about nought and a station below
    /// the tuning point was drawn on top of one above it.
    ///
    /// What the mode still decides is what the keying detectors are fed. They
    /// take the channel reduction, so they hear the two halves folded together
    /// whatever the display shows. That is a limitation of the detectors and not
    /// a reason to fold the picture as well.
    ///
    /// Three things follow from the answer and they must agree, which is why it
    /// is one question asked in one place. The display is two sided. The lower
    /// sideband filter sits below nought. And the correspondence to the band
    /// carries no sideband, because the sign is in the samples.
    ///
    /// The device has the last word and states it elsewhere: a mono endpoint
    /// duplicates its one channel, and a pair of identical channels read as a
    /// quadrature one is a spectrum symmetric about nought. See the predicate in
    /// the shell that folds the two answers together.
    pub fn complex_signal(&self) -> bool {
        self.receiver.iq_input
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