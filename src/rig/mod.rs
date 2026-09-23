//! Transceiver control.
//!
//! Owns the link to the radio and the two things that link is for: the dial
//! frequency, which turns the audio spectrum into a picture of a band, and the
//! ability to move it.
//!
//! ## Why this lives beside the interface rather than inside the processing
//!
//! The dial frequency is metadata about the display and not part of the signal.
//! Every decoder works in audio and has to keep working with no transceiver
//! present at all, which is the ordinary case for a receiver fed from a line
//! output. Threading a frequency through the processing chain would make the
//! chain depend on hardware it does not need and cannot use.
//!
//! ## Why the Rust interface rather than the C one
//!
//! The library offers both. The C one exists so a program written in another
//! language can reach it, and using it from Rust would mean opaque handles and
//! result codes in exchange for nothing. The Rust interface hands over a driver
//! that already hides whether the port is held here or by the sharing service.
//!
//! ## Threading
//!
//! The library runs its own thread and publishes into a queue and a shared
//! structure. This module drains both once per frame, from the interface
//! thread, which is the same arrangement the capture queue already uses.

use std::path::{Path, PathBuf};

use detent::transport::{FlowControl, PortInfo, SerialConfig};
use detent::{
    Attach, Catalogue, Counters, Driver, Event, LineState, LinkState, Mode, Param, RigState,
    Signals, Worker,
};

use crate::config::settings::{RigLine, RigSettings, RigTransport, SidebandMode};

/// Characters a readout may occupy, separators included.
///
/// Three groups of at most three digits and two separators. The bound exists so
/// the layout is a fixed array rather than an allocation: the readout is rebuilt
/// on every frame and on every hit test, and neither is a place to allocate.
pub const MAX_READOUT: usize = 12;

/// Which side of the dial the audio spectrum sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sideband {
    /// Audio adds to the dial frequency.
    Upper,
    /// Audio subtracts from it.
    Lower,
}

impl Sideband {
    /// Direction the audio offset is added in.
    ///
    /// Public because the display needs it as well: placing stored history at
    /// the frequency it was captured on is a shift whose direction is exactly
    /// this, and deriving it a second time elsewhere would be one more place
    /// for the two to disagree.
    pub fn sign(self) -> i64 {
        match self {
            Sideband::Upper => 1,
            Sideband::Lower => -1,
        }
    }
}

/// Label of a transceiver mode.
///
/// Stated here rather than taken from the control library, because the two
/// keyed modes are named after their audio sideband there and after their front
/// panel label everywhere an operator reads one. A description binds the upper
/// sideband entry to the reversed mode, because on this family of transceivers
/// the normal keyed mode inverts the audio spectrum, so a label derived from the
/// sideband reports each of the two as the other.
pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::CwUpper => "CW-R",
        Mode::CwLower => "CW",
        Mode::SsbUpper => "USB",
        Mode::SsbLower => "LSB",
        Mode::DigUpper => "DIG-U",
        Mode::DigLower => "DIG-L",
        Mode::Am => "AM",
        Mode::Fm => "FM",
    }
}

/// How an audio frequency corresponds to a frequency on the air.
///
/// The whole reason a transceiver is worth reading. Without it the display is a
/// picture of a sound card; with it, it is a picture of a band.
///
/// Two rules and the second is the one that is usually got wrong.
///
/// In a single sideband mode the dial names the suppressed carrier, so audio of
/// f corresponds to the dial plus f on the upper side and minus f on the lower.
///
/// In a keyed mode the dial names the signal rather than the carrier: a station
/// exactly on the dial frequency is heard at the sidetone pitch, not at nought.
/// The offset is therefore the audio frequency less the pitch. Ignoring that
/// puts every reading out by the pitch, which is several hundred hertz and is
/// far wider than anything the display resolves.
#[derive(Debug, Clone, Copy)]
pub struct Mapping {
    /// Frequency the transceiver reports.
    pub dial_hz: i64,
    pub sideband: Sideband,
    /// Audio frequency at which a station on the dial frequency is heard.
    ///
    /// The sidetone pitch in a keyed mode and nought in every other, because a
    /// single sideband receiver places a carrier on the dial at nought hertz by
    /// construction.
    pub zero_hz: f32,
    /// Correction the operator stated, added after everything else.
    ///
    /// Present because a converter, a transverter or a description that reports
    /// the wrong oscillator all shift the whole picture by a constant, and none
    /// of them is discoverable from here.
    pub trim_hz: f32,
}

impl Mapping {
    /// Frequency on the air of a tone in the audio spectrum.
    pub fn rf_of(&self, audio_hz: f32) -> i64 {
        let offset = (audio_hz - self.zero_hz + self.trim_hz) as i64;
        self.dial_hz + self.sideband.sign() * offset
    }

    /// Audio frequency a signal on the air is heard at.
    ///
    /// The inverse of the above, and it exists so a caller that knows where it
    /// wants to be can ask where that lands in the spectrum it is drawing.
    pub fn audio_of(&self, rf_hz: i64) -> f32 {
        let offset = (rf_hz - self.dial_hz) * self.sideband.sign();
        offset as f32 + self.zero_hz - self.trim_hz
    }
}

/// A frequency split into the groups a radio front panel shows.
///
/// Built rather than formatted, because the interface needs more than the text:
/// clicking a digit has to move that decade, and the only honest way to know
/// which decade a character belongs to is to record it while the character is
/// being placed.
pub struct Readout {
    chars: [u8; MAX_READOUT],
    /// Power of ten each character carries, or minus one for a separator.
    decades: [i8; MAX_READOUT],
    len: usize,
    /// Leading characters that are zeros of a decade above the value.
    ///
    /// Drawn dim rather than blank. Blanking them would move the rest of the
    /// text sideways whenever the frequency crossed a decade, and a display that
    /// shifts under the pointer is a display that cannot be clicked.
    leading: usize,
}

impl Readout {
    /// Builds the layout for a frequency.
    ///
    /// The lowest decade shown is the argument: nought for full resolution, one
    /// for the ten hertz steps a front panel shows. Megahertz occupy at least
    /// two characters and grow with the value, so a receiver above one hundred
    /// megahertz reads correctly without a setting for it.
    pub fn new(hz: i64, lowest_decade: u32) -> Readout {
        let value = hz.max(0);
        let lowest = lowest_decade.min(2) as i8;

        // Megahertz width, at least two so the layout does not shrink on the
        // bands where it matters most.
        let mut top = 7i8;
        while top < 9 && value >= 10i64.pow(top as u32 + 1) {
            top += 1;
        }

        let mut out = Readout {
            chars: [b' '; MAX_READOUT],
            decades: [-1; MAX_READOUT],
            len: 0,
            leading: 0,
        };

        let mut counting_leading = true;
        let mut decade = top;
        while decade >= lowest {
            // The separators sit below the megahertz and the kilohertz groups,
            // which is where a front panel puts them and where an operator
            // expects to find the boundary of a group.
            if decade == 5 || decade == 2 {
                out.push(b'.', -1);
                counting_leading = false;
            }
            let digit = (value / 10i64.pow(decade as u32)) % 10;
            if digit != 0 {
                counting_leading = false;
            }
            if counting_leading {
                out.leading += 1;
            }
            out.push(b'0' + digit as u8, decade);
            decade -= 1;
        }
        out
    }

    fn push(&mut self, ch: u8, decade: i8) {
        if self.len < MAX_READOUT {
            self.chars[self.len] = ch;
            self.decades[self.len] = decade;
            self.len += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn char_at(&self, index: usize) -> char {
        if index < self.len {
            self.chars[index] as char
        } else {
            ' '
        }
    }

    /// Step a character moves, or nothing for a separator.
    pub fn step_at(&self, index: usize) -> Option<i64> {
        if index >= self.len || self.decades[index] < 0 {
            return None;
        }
        Some(10i64.pow(self.decades[index] as u32))
    }

    /// True when the character is a zero above the value and is drawn dim.
    pub fn is_leading(&self, index: usize) -> bool {
        index < self.leading
    }

    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.len);
        for index in 0..self.len {
            out.push(self.chars[index] as char);
        }
        out
    }
}

/// Reads a frequency an operator typed.
///
/// The one path to a stated frequency. The digits of the readout are stepped and
/// clicked, which is right for moving and wrong for arriving: an operator reading
/// a spot from a cluster or a schedule for a beacon holds a number rather than a
/// direction, and stepping to it from wherever the dial happens to be is a dozen
/// gestures.
///
/// ## What is accepted
///
/// A unit if one is stated, and the magnitude otherwise.
///
/// Below a hundred is megahertz, because nobody arrives at fifty kilohertz by
/// typing fifty. Below a million is kilohertz, which is how a frequency is
/// quoted on the air and which has to reach the whole way: two metres is a
/// hundred and forty four thousand and seventy centimetres is four hundred and
/// thirty thousand, so a range that stopped at a hundred thousand would put
/// both of them three decades low. Above that is hertz, because a value that
/// large read as kilohertz would be past a gigahertz.
///
/// One overlap survives and it is narrow. A full hertz figure for the six
/// hundred and thirty metre band, such as 472000, reads as kilohertz and lands
/// at four hundred and seventy two megahertz. It cannot be settled without a
/// band table in here, and it does not need to be: 472 is what an operator types
/// for that band, and a stated unit resolves it in either direction.
///
/// Group separators are accepted because a front panel prints them and an
/// operator copies what they read. A single dot is a decimal point and several
/// are separators, which settles the one ambiguity without asking.
///
/// A band name is not handled here. It cannot be a frequency, so admitting it
/// would mean deciding what to do with it, and what to do with it needs the band
/// stack: the caller tries the name first.
pub fn parse_frequency(text: &str) -> Option<i64> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let lowered = raw.to_ascii_lowercase();

    // Longest suffix first. A shorter one tested first would strip the tail of
    // the longer and leave a body that parses as nothing.
    let (body, stated) = if let Some(head) = lowered.strip_suffix("mhz") {
        (head, Some(1_000_000.0f64))
    } else if let Some(head) = lowered.strip_suffix("khz") {
        (head, Some(1_000.0))
    } else if let Some(head) = lowered.strip_suffix("hz") {
        (head, Some(1.0))
    } else if let Some(head) = lowered.strip_suffix('m') {
        (head, Some(1_000_000.0))
    } else if let Some(head) = lowered.strip_suffix('k') {
        (head, Some(1_000.0))
    } else if let Some(head) = lowered.strip_suffix('h') {
        (head, Some(1.0))
    } else {
        (lowered.as_str(), None)
    };

    let dots = body.chars().filter(|&c| c == '.').count();
    let mut digits = String::with_capacity(body.len());
    for ch in body.chars() {
        match ch {
            '0'..='9' => digits.push(ch),
            '.' if dots == 1 => digits.push('.'),
            '.' | ',' | ' ' | '\'' | '_' => {}
            // Anything else is a mistake rather than a separator, and guessing
            // at it would produce a frequency the operator did not ask for.
            _ => return None,
        }
    }

    let value: f64 = digits.parse().ok()?;
    if !value.is_finite() || value <= 0.0 {
        return None;
    }

    let hz = match stated {
        Some(unit) => value * unit,
        None if value < 100.0 => value * 1_000_000.0,
        None if value < 1_000_000.0 => value * 1_000.0,
        None => value,
    };

    // Bounded at both ends. Below ten kilohertz nothing receives, and above a
    // gigahertz nothing this application drives; a value outside is a typing
    // error rather than an unusual receiver.
    if hz < 10_000.0 || hz > 1_000_000_000.0 {
        return None;
    }
    Some(hz.round() as i64)
}

/// One description in the catalogue, reduced to what a list needs.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub name: String,
    /// False when the description carries an error and cannot drive anything.
    ///
    /// Listed rather than hidden. An operator whose transceiver stopped
    /// appearing is told which file is wrong instead of left to guess.
    pub usable: bool,
    pub issues: usize,
}

/// What the interface reports about the link.
#[derive(Debug, Clone)]
pub struct RigStatus {
    pub running: bool,
    pub link: LinkState,
    pub counters: Counters,
    pub signals: Signals,
    /// Reason the link could not be established, or ended.
    pub error: String,
    /// True while the port is reached through the sharing service.
    pub shared: bool,
}

impl RigStatus {
    pub fn idle() -> RigStatus {
        RigStatus {
            running: false,
            link: LinkState::Closed,
            counters: Counters::default(),
            signals: Signals::default(),
            error: String::new(),
            shared: false,
        }
    }
}

pub struct RigLink {
    catalogue: Catalogue,
    driver: Option<Driver>,
    state: RigState,
    link: LinkState,
    counters: Counters,
    signals: Signals,
    error: String,
    shared: bool,

    profiles: Vec<ProfileEntry>,
    ports: Vec<PortInfo>,
    /// Fingerprint of the settings the link was opened with.
    signature: u64,
    /// Scratch, so draining does not allocate per frame.
    events: Vec<Event>,
    /// Requested frequency that has not been read back yet.
    ///
    /// Held because a transceiver answers a write on the next polling pass, and
    /// a readout that snapped back to the old value for a tenth of a second on
    /// every step would be unusable for tuning.
    pending: Option<(i64, crate::core::Instant)>,
}

/// How long a requested frequency stands in for the reading.
///
/// Longer than a polling pass at any rate a description states, and short
/// enough that a request the transceiver refused is visible as the value
/// returning rather than as a display that never corrects itself.
const PENDING_HOLD_S: f64 = 1.5;

impl RigLink {
    /// Reads the catalogue and lists the ports. Nothing is opened.
    pub fn new(settings: &RigSettings) -> RigLink {
        let mut link = RigLink {
            catalogue: Catalogue::new(),
            driver: None,
            state: RigState::default(),
            link: LinkState::Closed,
            counters: Counters::default(),
            signals: Signals::default(),
            error: String::new(),
            shared: false,
            profiles: Vec::new(),
            ports: Vec::new(),
            signature: 0,
            events: Vec::with_capacity(32),
            pending: None,
        };
        link.rescan_profiles(settings);
        link.rescan_ports();
        link.signature = signature(settings);
        link
    }

    /// Directory the descriptions are read from.
    ///
    /// Resolved against the directory of the executable when the setting is a
    /// relative path, so a shortcut started from anywhere finds the same files
    /// as a double click.
    pub fn profile_directory(settings: &RigSettings) -> PathBuf {
        let stated = Path::new(&settings.profiles_path);
        if stated.is_absolute() {
            return stated.to_path_buf();
        }
        let base = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_default();
        base.join(stated)
    }

    pub fn rescan_profiles(&mut self, settings: &RigSettings) {
        let directory = RigLink::profile_directory(settings);
        self.catalogue = Catalogue::from_directory(&directory);

        self.profiles.clear();
        for entry in self.catalogue.entries() {
            self.profiles.push(ProfileEntry {
                name: entry.name.clone(),
                usable: entry.usable(),
                issues: entry.report.issues().len(),
            });
        }
        crate::log_info!(
            "rig",
            "{} descriptions in {}",
            self.profiles.len(),
            directory.display()
        );
    }

    pub fn rescan_ports(&mut self) {
        self.ports = detent::transport::enumerate();
        crate::log_info!("rig", "{} serial ports present", self.ports.len());
    }

    pub fn profiles(&self) -> &[ProfileEntry] {
        &self.profiles
    }

    pub fn ports(&self) -> &[PortInfo] {
        &self.ports
    }

    /// Diagnostics of one description, for an operator asking why it is
    /// unusable.
    pub fn profile_issues(&self, name: &str, into: &mut Vec<String>) {
        into.clear();
        if let Some(entry) = self.catalogue.find(name) {
            for issue in entry.report.issues() {
                into.push(issue.to_string());
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.driver.is_some()
    }

    pub fn status(&self) -> RigStatus {
        RigStatus {
            running: self.driver.is_some(),
            link: self.link,
            counters: self.counters,
            signals: self.signals,
            error: self.error.clone(),
            shared: self.shared,
        }
    }

    pub fn state(&self) -> &RigState {
        &self.state
    }

    /// Frequency to display.
    ///
    /// A request that has not been read back yet stands in for the reading, so
    /// a readout being stepped moves under the pointer rather than a polling
    /// pass later. The stand in expires, which is what makes a refused request
    /// visible: the value returns to what the transceiver actually holds.
    pub fn display_hz(&self) -> Option<i64> {
        if let Some((value, at)) = self.pending {
            if at.elapsed_secs() < PENDING_HOLD_S {
                return Some(value);
            }
        }
        self.state.freq
    }

    pub fn mode(&self) -> Option<Mode> {
        self.state.mode
    }

    /// Correspondence between the audio spectrum and the band.
    ///
    /// Absent when there is no frequency to anchor it to, and, on a real input,
    /// when the mode is one where a single mapping does not exist: amplitude
    /// and frequency modulation place the carrier at the dial and the audio at
    /// baseband, so a tone at one kilohertz is one kilohertz away on both sides
    /// at once and there is nothing to report.
    ///
    /// A complex input has no such case and no sideband. The samples carry the
    /// sign of the offset, so audio adds to the dial whichever half it sits in;
    /// the sideband setting exists precisely because a real input threw that
    /// sign away. Reading a complex spectrum through the lower sideband rule
    /// would report every signal on the wrong side of the dial, which is what a
    /// mirrored display then makes invisible.
    pub fn mapping(&self, settings: &RigSettings, complex_input: bool) -> Option<Mapping> {
        let dial = self.display_hz()?;

        if complex_input {
            // The keyed pitch is dropped with the sideband and for the same
            // reason: it is an audio side convention of a transceiver that
            // demodulated for itself. A complex baseband is referenced to the
            // dial directly, and whatever the hardware does to that reference
            // is what the correction below is for.
            return Some(Mapping {
                dial_hz: dial,
                sideband: Sideband::Upper,
                zero_hz: 0.0,
                trim_hz: settings.offset_hz,
            });
        }

        let sideband = match settings.sideband {
            SidebandMode::Upper => Some(Sideband::Upper),
            SidebandMode::Lower => Some(Sideband::Lower),
            SidebandMode::Auto => match self.state.mode {
                Some(Mode::SsbUpper) | Some(Mode::DigUpper) | Some(Mode::CwUpper) => {
                    Some(Sideband::Upper)
                }
                Some(Mode::SsbLower) | Some(Mode::DigLower) | Some(Mode::CwLower) => {
                    Some(Sideband::Lower)
                }
                // Nothing to derive from, so nothing is claimed.
                Some(Mode::Am) | Some(Mode::Fm) | None => None,
            },
        }?;

        // The keyed modes place the dial on the signal rather than on the
        // carrier, so a station on the dial frequency is heard at the pitch. The
        // transceiver reports the pitch when its description can read one; the
        // stated value stands in when it cannot, which is the common case.
        let keyed = matches!(self.state.mode, Some(Mode::CwUpper) | Some(Mode::CwLower));
        let zero_hz = if keyed {
            self.state.pitch.map(|v| v as f32).unwrap_or(settings.cw_pitch_hz)
        } else {
            0.0
        };

        Some(Mapping { dial_hz: dial, sideband, zero_hz, trim_hz: settings.offset_hz })
    }

    // ------------------------------------------------------------- lifetime

    /// Opens the link.
    ///
    /// A failure is recorded rather than returned. There is nobody to return it
    /// to at the point this is called, and the operator reads it from the panel
    /// next to the control that caused it.
    pub fn start(&mut self, settings: &RigSettings) {
        self.stop();
        self.error.clear();
        self.signature = signature(settings);

        if !settings.enabled {
            return;
        }
        if settings.profile.is_empty() {
            self.error = "no description chosen".to_string();
            return;
        }
        // A script replaces the hardware, so the absence of a port is not a gap
        // in that case. It is checked first for the same reason: nothing else
        // has anything to hold.
        if !settings.replay_path.is_empty() {
            self.start_replay(settings);
            return;
        }
        if settings.port.is_empty() && settings.transport == RigTransport::Direct {
            self.error = "no port chosen".to_string();
            return;
        }

        match settings.transport {
            RigTransport::Shared => self.start_shared(settings),
            RigTransport::Direct => self.start_direct(settings),
        }
    }

    /// Answers from a script rather than from a transceiver.
    ///
    /// The path is resolved against the configuration file, so a shortcut
    /// started from anywhere finds the same script as a double click.
    ///
    /// A script that reads as empty is refused rather than started. An empty
    /// script answers nothing, so the link would report a stall and name the
    /// transceiver, which is the one diagnosis that is certainly wrong here.
    fn start_replay(&mut self, settings: &RigSettings) {
        let profile = match self.load_profile(settings) {
            Some(p) => p,
            None => return,
        };

        let stated = Path::new(&settings.replay_path);
        let path = if stated.is_absolute() {
            stated.to_path_buf()
        } else {
            let base = crate::config::Settings::default_path()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default();
            base.join(stated)
        };

        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error = format!("{}: {}", path.display(), e);
                crate::log_warn!("rig", "cannot read {}: {}", path.display(), e);
                return;
            }
        };

        let script = detent::transport::replay::parse_script(&text);
        if script.is_empty() {
            self.error = format!("{}: no exchanges", path.display());
            crate::log_warn!("rig", "{} states no exchanges", path.display());
            return;
        }

        let exchanges = script.len();
        let transport = Box::new(detent::transport::replay::Replay::new(script));
        self.driver = Some(Driver::Local(Worker::start(profile, transport)));
        self.link = LinkState::Starting;
        self.shared = false;
        crate::log_info!(
            "rig",
            "{} answering from {} with {} exchanges",
            settings.profile,
            path.display(),
            exchanges
        );
    }

    /// Reads the description a slot names, recording why when it cannot.
    ///
    /// Held apart because three transports need it and the failure has to be
    /// reported the same way in all three: an operator reads one condition
    /// property, not three.
    fn load_profile(&mut self, settings: &RigSettings) -> Option<detent::Profile> {
        let entry = match self.catalogue.find(&settings.profile) {
            Some(e) => e,
            None => {
                self.error = format!("no description named {}", settings.profile);
                return None;
            }
        };
        match &entry.profile {
            Some(p) => Some(p.clone()),
            None => {
                let first = entry
                    .report
                    .issues()
                    .first()
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| "cannot be used".to_string());
                self.error = format!("{}: {}", entry.name, first);
                None
            }
        }
    }

    fn start_direct(&mut self, settings: &RigSettings) {
        let profile = match self.load_profile(settings) {
            Some(p) => p,
            None => return,
        };

        let config = SerialConfig {
            port: settings.port.clone(),
            baud: settings.baud,
            dtr: line_state(settings.dtr),
            rts: line_state(settings.rts),
            // Never handshaking. The lines it consumes are the lines an
            // interface cable uses to key the transmitter, and a receiver
            // application has no business asserting either by accident.
            flow_control: FlowControl::None,
            ..SerialConfig::default()
        };

        match detent::transport::open_serial(&config) {
            Ok(transport) => {
                self.driver = Some(Driver::Local(Worker::start(profile, transport)));
                self.link = LinkState::Starting;
                self.shared = false;
                crate::log_info!(
                    "rig",
                    "{} on {} at {} baud",
                    settings.profile,
                    settings.port,
                    settings.baud
                );
            }
            Err(e) => {
                self.error = e.to_string();
                crate::log_warn!("rig", "cannot open {}: {}", settings.port, e);
            }
        }
    }

    /// Reaches the port through the sharing service.
    ///
    /// The case this whole library exists for. A serial port carries one
    /// conversation, so a logging application and this one cannot both hold it;
    /// the service holds it and runs one exchange loop for everybody.
    fn start_shared(&mut self, settings: &RigSettings) {
        let directory = RigLink::profile_directory(settings);
        let request = Attach {
            endpoint: String::new(),
            profile: settings.profile.clone(),
            port: settings.port.clone(),
            baud: settings.baud,
            profiles: Some(directory.to_string_lossy().to_string()),
            program: None,
            auto_start: true,
        };

        match detent::Remote::attach(&request) {
            Ok(remote) => {
                crate::log_info!(
                    "rig",
                    "attached to the service holding {} on {}",
                    remote.profile_name(),
                    remote.port_name()
                );
                self.driver = Some(Driver::Remote(remote));
                self.link = LinkState::Starting;
                self.shared = true;
            }
            Err(e) => {
                self.error = e.to_string();
                crate::log_warn!("rig", "cannot attach: {}", e);
            }
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut driver) = self.driver.take() {
            driver.stop();
            crate::log_info!("rig", "link closed");
        }
        self.link = LinkState::Closed;
        self.state = RigState::default();
        self.signals = Signals::default();
        self.counters = Counters::default();
        self.pending = None;
        self.shared = false;
    }

    /// Reopens when a setting that decides the link has moved.
    ///
    /// Cheap enough to call every frame. Only the settings that decide which
    /// port carries which protocol take part; the display and the correction
    /// are read where they are used and force nothing.
    pub fn sync(&mut self, settings: &RigSettings) {
        let wanted = signature(settings);
        if wanted == self.signature {
            return;
        }
        self.signature = wanted;
        

        // A change to the directory means the catalogue is stale whether or not
        // the link is about to be reopened.
        self.rescan_profiles(settings);

        if self.driver.is_some() || settings.enabled {
            self.start(settings);
        }
    }

    /// Reads whatever the link produced. Called once per frame.
    pub fn poll(&mut self) {
        let driver = match self.driver.as_ref() {
            Some(d) => d,
            None => return,
        };

        // The events are drained even though the state is taken separately.
        // Leaving them would grow the queue until it dropped the oldest, and a
        // fault that produced no state change still has to be reported.
        self.events.clear();
        driver.drain_events(64, &mut self.events);
        for event in &self.events {
            match event {
                detent::Event::Link(state) => {
                    crate::log_info!("rig", "link {:?}", state);
                }
                detent::Event::Exchange { command, fault, setting } => {
                    // Logged rather than shown. An exchange that failed once is
                    // ordinary on a serial link, and a panel that reported each
                    // one would report nothing else.
                    match setting {
                        Some(s) => crate::log_debug!(
                            "rig",
                            "{} failed while setting {}: {}",
                            command,
                            s.describe(),
                            fault.describe()
                        ),
                        None => crate::log_debug!(
                            "rig",
                            "{} failed: {}",
                            command,
                            fault.describe()
                        ),
                    }
                }
                _ => {}
            }
        }

        let next = driver.state();
        // The stand in is dropped the moment the transceiver reports the value
        // it was asked for, so the two never disagree for longer than one pass.
        if let Some((wanted, _)) = self.pending {
            if next.freq == Some(wanted) {
                self.pending = None;
            }
        }

        self.state = next;
        self.link = driver.link();
        self.counters = driver.counters();
        self.signals = driver.signals();

        if let Some(reason) = driver.ended() {
            if self.error.is_empty() {
                self.error = reason;
            }
        }
    }

    // -------------------------------------------------------------- tuning

    /// True when the description defines a way to set the frequency.
    ///
    /// Consulted before a control is offered rather than after it is used, so a
    /// transceiver that cannot be tuned over the interface presents a dead
    /// control beside the setting rather than a control that does nothing.
    pub fn can_tune(&self) -> bool {
        match self.driver.as_ref() {
            Some(d) => d.can_write(Param::Freq),
            None => false,
        }
    }

    /// Asks for a frequency.
    ///
    /// The request is queued rather than sent, so a readout being stepped
    /// continuously produces one message per polling pass instead of one per
    /// step. The value stands in for the reading meanwhile, which is what makes
    /// the display follow the pointer.
    pub fn set_frequency(&mut self, hz: i64) -> bool {
        let driver = match self.driver.as_ref() {
            Some(d) => d,
            None => return false,
        };
        if !driver.can_write(Param::Freq) {
            return false;
        }
        let wanted = hz.max(0);
        if !driver.request_value(Param::Freq, wanted) {
            return false;
        }
        self.pending = Some((wanted, crate::core::Instant::now()));
        true
    }

    /// Moves the frequency by a step.
    ///
    /// Measured from whatever is displayed rather than from the last reading, so
    /// several steps inside one polling pass accumulate instead of each landing
    /// on the same place.
    pub fn tune_by(&mut self, delta_hz: i64) -> bool {
        let from = match self.display_hz() {
            Some(v) => v,
            None => return false,
        };
        self.set_frequency(from + delta_hz)
    }

    /// Moves the dial so a tone in the audio spectrum lands on the target.
    ///
    /// The target is where the operator wants to hear a signal: the sidetone
    /// pitch in a keyed mode, and whatever the receiver filter is centred on
    /// otherwise. Clicking a trace then brings that trace to the ear rather than
    /// merely reporting where it is.
    pub fn tune_audio_to(
        &mut self,
        audio_hz: f32,
        target_hz: f32,
        settings: &RigSettings,
        complex_input: bool,
    ) -> bool {
        let mapping = match self.mapping(settings, complex_input) {
            Some(m) => m,
            None => return false,
        };
        let wanted = mapping.rf_of(audio_hz);
        // Where the dial has to sit for that frequency to arrive at the target.
        let dial = wanted - mapping.sideband.sign() * (target_hz - mapping.zero_hz) as i64;
        self.set_frequency(dial)
    }

    /// Sets the transmission mode.
    pub fn set_mode(&mut self, mode: Mode) -> bool {
        match self.driver.as_ref() {
            Some(d) => {
                let param = mode.to_param();
                d.can_write(param) && d.request_flag(param)
            }
            None => false,
        }
    }

    /// Moves the dial so a frequency on the air lands on the target.
    ///
    /// The inverse of the gesture above and the one a station list needs: the
    /// entry states where a signal is on the band, not where it currently sits
    /// in the audio, and it may well be outside the visible span altogether.
    pub fn tune_to_rf(
        &mut self,
        rf_hz: i64,
        target_hz: f32,
        settings: &RigSettings,
        complex_input: bool,
    ) -> bool {
        let mapping = match self.mapping(settings, complex_input) {
            Some(m) => m,
            None => return false,
        };
        let dial = rf_hz - mapping.sideband.sign() * (target_hz - mapping.zero_hz) as i64;
        self.set_frequency(dial)
    }

    /// Modes the description can select.
    ///
    /// Empty while no link is open, which is the truth rather than a caution:
    /// nothing can be selected on a transceiver that is not there.
    pub fn writable_modes(&self, into: &mut Vec<Mode>) {
        into.clear();
        let driver = match self.driver.as_ref() {
            Some(d) => d,
            None => return,
        };
        for mode in [
            Mode::CwUpper,
            Mode::CwLower,
            Mode::SsbUpper,
            Mode::SsbLower,
            Mode::DigUpper,
            Mode::DigLower,
            Mode::Am,
            Mode::Fm,
        ] {
            if driver.can_write(mode.to_param()) {
                into.push(mode);
            }
        }
    }
}

impl Drop for RigLink {
    fn drop(&mut self) {
        self.stop();
    }
}

fn line_state(mode: RigLine) -> LineState {
    match mode {
        RigLine::High => LineState::High,
        RigLine::Low => LineState::Low,
    }
}

/// Fingerprint of everything that decides which port carries which protocol.
///
/// The correction, the pitch and every display setting are absent on purpose:
/// they are read where they are used and changing one must not take the link
/// down.
fn signature(settings: &RigSettings) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&[u8::from(settings.enabled)]);
    mix(settings.profile.as_bytes());
    mix(settings.profiles_path.as_bytes());
    mix(settings.port.as_bytes());
    mix(settings.replay_path.as_bytes());
    mix(&settings.baud.to_le_bytes());
    mix(&[settings.transport as u8 + 1]);
    mix(&[settings.dtr as u8 + 1, settings.rts as u8 + 1]);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_sideband_mapping_places_audio_beside_the_dial() {
        let upper = Mapping {
            dial_hz: 14_200_000,
            sideband: Sideband::Upper,
            zero_hz: 0.0,
            trim_hz: 0.0,
        };
        assert_eq!(upper.rf_of(1000.0), 14_201_000);
        assert_eq!(upper.audio_of(14_201_000), 1000.0);

        let lower = Mapping { sideband: Sideband::Lower, ..upper };
        assert_eq!(lower.rf_of(1000.0), 14_199_000);
        assert_eq!(lower.audio_of(14_199_000), 1000.0);
    }

    #[test]
    fn a_keyed_mapping_places_the_dial_at_the_pitch() {
        // The distinction that is usually got wrong. A station exactly on the
        // dial frequency is heard at the sidetone pitch rather than at nought,
        // so ignoring the pitch puts every reading out by several hundred hertz.
        let keyed = Mapping {
            dial_hz: 14_025_000,
            sideband: Sideband::Upper,
            zero_hz: 700.0,
            trim_hz: 0.0,
        };
        assert_eq!(keyed.rf_of(700.0), 14_025_000);
        assert_eq!(keyed.rf_of(800.0), 14_025_100);
        assert_eq!(keyed.rf_of(600.0), 14_024_900);
    }

    #[test]
    fn the_correction_moves_the_whole_picture() {
        let plain = Mapping {
            dial_hz: 7_000_000,
            sideband: Sideband::Upper,
            zero_hz: 0.0,
            trim_hz: 0.0,
        };
        let trimmed = Mapping { trim_hz: 250.0, ..plain };
        assert_eq!(trimmed.rf_of(1000.0) - plain.rf_of(1000.0), 250);
    }

    #[test]
    fn the_readout_groups_a_frequency_the_way_a_front_panel_does() {
        let readout = Readout::new(4_625_000, 1);
        assert_eq!(readout.text(), "04.625.00");
        // Two leading zeros would be one too many: the value has seven digits
        // and the layout has eight, so exactly one is above it.
        assert_eq!(readout.is_leading(0), true);
        assert_eq!(readout.is_leading(1), false);
    }

    #[test]
    fn a_character_names_the_step_it_moves() {
        let readout = Readout::new(14_025_000, 1);
        assert_eq!(readout.text(), "14.025.00");
        assert_eq!(readout.step_at(0), Some(10_000_000));
        assert_eq!(readout.step_at(1), Some(1_000_000));
        // The separator moves nothing, which is what lets a click on one be
        // ignored rather than rounded to a neighbour.
        assert_eq!(readout.step_at(2), None);
        assert_eq!(readout.step_at(3), Some(100_000));
        assert_eq!(readout.step_at(8), Some(10));
    }

    #[test]
    fn the_layout_grows_rather_than_truncating() {
        // Above one hundred megahertz a fixed width of two would drop the
        // leading digit and report a frequency on another band entirely.
        let readout = Readout::new(144_300_000, 1);
        assert_eq!(readout.text(), "144.300.00");
        assert_eq!(readout.step_at(0), Some(100_000_000));
    }

    #[test]
    fn a_typed_frequency_is_read_by_magnitude() {
        // The three ranges, each in the form an operator naturally types it.
        assert_eq!(parse_frequency("14.025"), Some(14_025_000));
        assert_eq!(parse_frequency("14025"), Some(14_025_000));
        assert_eq!(parse_frequency("14025000"), Some(14_025_000));
        // The low frequency allocations, where the kilohertz reading is the
        // only one that lands on a band at all.
        assert_eq!(parse_frequency("137"), Some(137_000));
        assert_eq!(parse_frequency("472"), Some(472_000));
        assert_eq!(parse_frequency("1810"), Some(1_810_000));
        // Above the basic plane of a shortwave receiver.
        assert_eq!(parse_frequency("144300"), Some(144_300_000));
        assert_eq!(parse_frequency("50.313"), Some(50_313_000));
    }

    #[test]
    fn separators_are_accepted_as_typed() {
        // Several dots are how a front panel prints it, one dot is a decimal
        // point, and the two cannot be told apart without counting them.
        assert_eq!(parse_frequency("14.025.000"), Some(14_025_000));
        assert_eq!(parse_frequency("14 025 000"), Some(14_025_000));
        assert_eq!(parse_frequency("14,025"), Some(14_025_000));
        assert_eq!(parse_frequency("14025.5"), Some(14_025_500));
    }

    #[test]
    fn a_stated_unit_overrides_the_magnitude() {
        // Mostly it agrees, and that is the design rather than a weakness: the
        // magnitude reading is chosen to mean what an operator means, so a
        // stated unit confirms it and costs nothing.
        assert_eq!(parse_frequency("7100k"), Some(7_100_000));
        assert_eq!(parse_frequency("7.1M"), Some(7_100_000));
        assert_eq!(parse_frequency("7100000hz"), Some(7_100_000));
        assert_eq!(parse_frequency("7100kHz"), Some(7_100_000));

        // Where it does not agree. Bare, both of these read as kilohertz and
        // land three decades away, and the second is the one overlap the
        // magnitude rule cannot settle on its own.
        assert_eq!(parse_frequency("14025hz"), Some(14_025));
        assert_eq!(parse_frequency("472000hz"), Some(472_000));
    }

    #[test]
    fn a_mistake_is_refused_rather_than_guessed() {
        assert_eq!(parse_frequency(""), None);
        assert_eq!(parse_frequency("abc"), None);
        assert_eq!(parse_frequency("14x025"), None);
        // Below anything that receives and above anything that is driven.
        assert_eq!(parse_frequency("0.001"), None);
        assert_eq!(parse_frequency("9999M"), None);
    }

    #[test]
    fn full_resolution_adds_the_last_digit() {
        let readout = Readout::new(14_025_123, 0);
        assert_eq!(readout.text(), "14.025.123");
        assert_eq!(readout.step_at(9), Some(1));
    }
}