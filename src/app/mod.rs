//! Application shell.
//!
//! Owns the window, the renderer, the font system, the widget system, the
//! capture stream, the processing engine, the decoder bank and the transceiver
//! link. What the operator sees is declared in the panel module; this one
//! supplies it with state and applies what it asks for.
//!
//! Frame order is fixed and every step depends on the previous one:
//!   1. drain the message queue and fold the events into the input snapshot;
//!   2. advance the clock;
//!   3. drain the capture queue into the processing engine and the decoders,
//!      which queue the waterfall rows and the decoded text they produced;
//!   4. probe the elements drawn in the top layer, so a press reaches them
//!      before the tree that will be declared underneath;
//!   5. declare the interface, which solves the layout and records commands;
//!   6. draw the widget tree, the reserved areas and the top layer;
//!   7. apply the commands, queue pending glyph rows and submit.
//!
//! Steps three and seven both queue texture uploads; the renderer records them
//! at the head of the frame command buffer, before the render pass, so the draw
//! calls of the same frame already see the new data.

pub mod clock;
pub mod panel;
pub mod axis;

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::axis::Axis;
use crate::audio::{AudioStatus, CaptureConfig, CaptureStream, DeviceInfo};
use crate::callsign::Book;
use crate::config::settings::{MeterScale, MeterSettings};
use crate::config::Settings;
use crate::core::Result;
use crate::decode::channels::ChannelInfo;
use crate::decode::log::DecodeLog;
use crate::decode::{DecoderBank, Mode};
use crate::dsp::{DspEngine, SMeter};
use crate::font::{FontId, FontSystem};
use crate::gui::theme::Theme;
use crate::gui::{Frame, Ui};
use crate::i18n::Catalog;
use crate::platform::{self, Event, Key, Window, WindowConfig};
use crate::render::{Color, DrawList, Rect, Renderer};
use crate::rig::{Mapping, Readout, RigLink};
use crate::record::replay::{ReplayStatus, ReplayStream, Timeline};
use crate::record::{rxr, Recorder, RecorderConfig, RecorderStatus, SharedMeta};
use crate::audio::monitor::SharedReceiver;
use crate::config::settings::{Detector, ModeLink};

use clock::FrameClock;
use panel::{
    Selections, StatusInfo, UiCommands, BANDWIDTH_MAX_HZ, BANDWIDTH_MIN_HZ, SIDE_PANEL_MAX,
    SIDE_PANEL_MAX_FRACTION, SIDE_PANEL_MIN, TAG_DECODE, TAG_METER, TAG_SPECTRUM, TAG_TIMELINE,
    TAG_WATERFALL, ZOOM_MAX,
};

/// Point size to pixels at the ninety six dpi baseline. The display factor is
/// applied separately through the interface scale.
const PT_TO_PX: f32 = 4.0 / 3.0;

/// Samples read from the capture queue per iteration.
const AUDIO_CHUNK: usize = 4096;

/// Quantization of a width set by dragging. The detectors are rebuilt whenever
/// the value changes, so a drag that reported every pixel would rebuild them a
/// hundred times per second for no visible difference.
const BANDWIDTH_STEP_HZ: f32 = 5.0;

/// Quantization of a filter edge set by dragging, in hertz.
const FILTER_STEP_HZ: f32 = 10.0;

/// Narrowest receiver passband a drag may produce.
const FILTER_MIN_WIDTH_HZ: f32 = 50.0;

/// Magnification per wheel notch.
///
/// A fifth per notch: five turns double the magnification, which is fast enough
/// to cross the useful range in a gesture and slow enough that one accidental
/// notch does not lose the place.
const ZOOM_PER_NOTCH: f32 = 1.2;

/// Delay before the first attempt to bring a failed device back, in seconds.
const RECOVER_FIRST_S: f32 = 1.0;

/// Longest the delay grows to.
///
/// Bounded rather than unbounded, and never abandoned. A cable that was knocked
/// out is plugged back in minutes later, and a receiver that had given up by then
/// is a receiver the operator has to notice and restart. Half a minute is short
/// enough that the return is not waited for and long enough that a device which
/// is genuinely gone costs nothing to keep asking about.
const RECOVER_MAX_S: f32 = 30.0;

/// Pause before a held readout digit begins to repeat, in seconds.
///
/// Long enough that a single step is a single step, short enough that an operator
/// who meant to travel does not wait for it.
const READOUT_DELAY_S: f32 = 0.35;

/// Interval between two repeats.
///
/// About sixteen a second, which is what a front panel encoder produces under a
/// deliberate turn and is fast enough to cross a band without being a blur.
const READOUT_PERIOD_S: f32 = 0.06;

/// Repeats one frame may deliver at once.
///
/// A stall must not produce a burst: an operator who lost a frame did not ask for
/// twenty steps.
const READOUT_MAX_BURST: i64 = 4;

/// Interval between two automatic notch searches, in seconds.
///
/// A heterodyne appears when somebody switches a transmitter on and stays for as
/// long as they leave it on, so nothing here happens on the scale of a frame. A
/// quarter of a second is fast enough that the operator does not wait for it.
const NOTCH_POLL_S: f32 = 0.25;

/// Height above the noise a tone needs before the notch will consider it.
const NOTCH_MARGIN_DB: f32 = 12.0;

/// How far the long average may sit below the peak hold and still be steady.
///
/// The one discrimination that matters, and it is only a few decibels wide. A
/// carrier that is always there is interference; one that is keyed is somebody
/// working, and removing that is the single thing the operator did not ask for.
/// For a steady tone the average and the peak agree; for keying at the ordinary
/// four tenths duty the average sits about four decibels below.
///
/// So a weak steady carrier and a strong intermittent one can be confused. That
/// is why the search only ever moves a notch the operator already switched on.
const NOTCH_STEADY_DB: f32 = 3.0;

/// Passes a candidate has to win before the notch moves to it.
///
/// The notch is a hole in what is being listened to, and one that hunts between
/// two tones removes both of them and neither properly.
const NOTCH_HITS: u32 = 3;

/// Interval between two scans for call signs, in seconds.
///
/// A station sends its call over about a second at any usable speed, so a scan
/// several times per second sees every one of them and a scan per frame would
/// see each of them a hundred times. The retirement timer runs on the same pass,
/// which needs nothing faster than this.
const CALLSIGN_POLL_S: f32 = 0.25;

/// Frequency within which a decoded line belongs to a channel, in hertz.
///
/// A line records the frequency of the detector that produced it, and the
/// channel list records the same figure, so the match is exact until the
/// tracking loop moves one of them. Half a detector width covers that.
const CHANNEL_MATCH_HZ: f32 = 120.0;

/// Interval between two configuration comparisons, in seconds.
///
/// Two documents are written per refresh, which is a few hundred short strings:
/// negligible twice a second and not negligible per frame. Fast enough that the
/// list follows a control being moved.
const DEVIATION_POLL_S: f32 = 0.5;

/// Lines the report shows before it states a remainder.
const MAX_DEVIATIONS: usize = 30;

/// Keys the application writes without being asked.
///
/// A difference in one of these says nothing about a decision. The geometry of
/// the window, the part of the span in view, the device that happens to be
/// selected and the composition of the panel are all written by the application
/// itself, and every one of them differs from the shipped value on any real
/// installation; left in, they would bury the three lines that matter.
///
/// The keying tone and the teleprinter mark are deliberately absent even though a
/// click on the display moves both. A tracking switch turned off by a stray click
/// is exactly the fault this report exists to surface.
///
/// A key added later and not listed here appears in the report. That is the safe
/// failure: one line of noise rather than a setting nobody can find.
const SESSION_KEYS: &[(&str, &str)] = &[
    ("ui", "window_x"),
    ("ui", "window_y"),
    ("ui", "window_width"),
    ("ui", "window_height"),
    ("ui", "maximized"),
    ("ui", "side_panel_width"),
    ("ui", "decode_panel_fraction"),
    ("ui", "show_settings_panel"),
    ("ui", "band_panel_open"),
    ("waterfall", "zoom"),
    ("waterfall", "view_centre"),
    ("waterfall", "center_hz"),
    ("waterfall", "span_hz"),
    ("audio", "device_id"),
    ("audio", "device_name"),
    ("audio", "monitor_device_id"),
    ("audio", "monitor_device_name"),
    ("receiver", "tune_hz"),
    ("rig", "port"),
    ("rig", "profile"),
    ("bands", "stack"),
    ("panel", "receive"),
    ("panel", "audio"),
    ("panel", "decode"),
    ("panel", "display"),
    ("panel", "known"),
];

/// Interval between two checks for segments the recorder appended.
///
/// A rebuild reads the whole directory, so it is paid on a timer rather than
/// per frame. Two seconds is far below the shortest segment worth recording and
/// far above the cost of a directory listing.
const TIMELINE_POLL_S: f32 = 2.0;

/// What a receiver filter drag grabbed.
///
/// Decided at the press and held for the whole gesture. A band that narrows
/// under the pointer would otherwise change which edge is nearest, and the drag
/// would jump from one to the other halfway through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterGrab {
    None,
    Low,
    High,
    /// Both edges, keeping the width.
    Band,
}

/// Text the operator is typing.
///
/// Session state and deliberately not configuration. A half typed frequency
/// restored on the next start would sit in the field looking like a pending
/// action, and the operator would either apply it by accident or spend a moment
/// working out where it came from.
#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// Frequency being typed.
    pub frequency: String,
    /// Name for a frequency being recorded.
    pub label: String,
}

/// Position and filter of the decoded text panel.
///
/// Session state rather than configuration. A filter carried across a restart
/// would hide traffic from an operator who has forgotten they set it, and the
/// symptom of that is a decoder that appears to have stopped working, which is
/// the most expensive kind of false report to chase.
#[derive(Debug, Clone, Default)]
pub struct DecodeView {
    /// Lines held back from the newest, counted in admitted lines.
    pub scroll: usize,
    /// Substring the panel is limited to, empty for everything.
    pub filter: String,
}

/// Where one decoded line was drawn, and what frequency it came from.
///
/// Recorded while drawing rather than recomputed on a click. The layout depends
/// on the filter, on the scroll position and on how many lines are still being
/// assembled; a second implementation of that arithmetic is a second thing that
/// can disagree with the first, and the operator sees the disagreement as a
/// click landing on the wrong station.
#[derive(Debug, Clone, Copy)]
struct DecodeHit {
    top: f32,
    bottom: f32,
    hz: f32,
}

/// Colours and switches the data area drawing reads.
///
/// Assembled once per frame from the theme and the appearance section. The
/// alternative is fifteen arguments on every helper, and a helper that takes
/// fifteen arguments is one nobody checks the order of.
struct DataLook {
    scale: f32,
    /// Size of an axis label. Smaller than the interface font: an axis is read
    /// by position and the number only confirms it.
    label_px: f32,
    accent: Color,
    text: Color,
    dim: Color,
    faint: Color,
    grid_major: Color,
    grid_minor: Color,
    background: Color,
    held: Color,
    monitor: Color,
    station: Color,
    /// Shade over the part of the span no channel may be opened in.
    shade: Color,
    shade_edge: Color,
    /// Colour a recognized call sign is drawn in.
    ///
    /// Deliberately not the accent. The accent already marks the trace, the
    /// tuning point and the focused channel, and a fourth meaning would leave it
    /// meaning nothing in particular.
    call: Color,
    call_highlight: bool,
    /// Colour the notch is drawn in.
    ///
    /// Its own, because it is the one mark that states what does not survive.
    /// The passband and the monitor band are both statements about what does,
    /// and sharing a colour with either would make the three tell one story.
    notch: Color,
    /// Colour of the measurement reference.
    ///
    /// Neutral rather than accented. It is a ruler, and a ruler is not coloured.
    reference: Color,
    /// Shortest token the extractor accepts, so the drawing and the spot list
    /// agree about what a call is.
    call_min: usize,
    edge_grab: f32,
    /// Every n-th grid line is drawn at full strength and carries the label.
    major_every: u32,
    labels: bool,
    trace_fill: bool,
    trace_fill_alpha: f32,
    trace_thickness: f32,
    crosshair: bool,
}

/// Colours and switches the meter drawing reads.
struct MeterLook {
    scale: f32,
    ui_px: f32,
    label_px: f32,
    accent: Color,
    text: Color,
    dim: Color,
    faint: Color,
    well: Color,
    border: Color,
    segmented: bool,
    segment_px: f32,
    segment_gap_px: f32,
    scale_labels: bool,
}

pub struct App {
    settings: Settings,
    window: Window,
    renderer: Renderer,
    fonts: FontSystem,
    gui: Ui,
    dsp: DspEngine,
    decode: DecoderBank,
    rig: RigLink,
    draw_list: DrawList,
    clock: FrameClock,
    events: Vec<Event>,
    gpu_name: String,
    font_name: String,
    /// Channel view refreshed once per frame, so neither the interface nor the
    /// drawing pass has to allocate for it.
    channel_view: Vec<ChannelInfo>,
    /// Translation files found at startup, plus the built in wording.
    languages: Vec<String>,
    language_dir: PathBuf,
    monitor: Option<crate::audio::MonitorStream>,
    /// Receiver configuration published to the monitor thread.
    monitor_receiver: Arc<SharedReceiver>,
    /// Detector the mode coupling last acted on, so a change is recognized.
    last_detector: Detector,
    /// Transceiver mode the coupling last saw.
    last_rig_mode: Option<detent::Mode>,
    /// Dial the display accumulators were last aligned to.
    last_dial: Option<i64>,
    /// Fraction of a column and of a bin the shift has not yet spent.
    shift_columns: f32,
    shift_bins: f32,
    monitor_status: crate::audio::MonitorStatus,
    monitor_devices: Vec<DeviceInfo>,
    /// Band the monitor is passing, refreshed once per frame so the panel and
    /// the display state the same thing.
    listen_band: Option<(f32, f32)>,

    audio: Option<CaptureStream>,
    audio_status: AudioStatus,
    /// True while the capture is meant to be running.
    ///
    /// The one thing that separates a device that failed from one the operator
    /// stopped, and it cannot be read off the stream: a reopen that failed leaves
    /// no stream at all, which looks exactly like having been stopped by hand.
    audio_wanted: bool,
    audio_retry: f32,
    audio_backoff: f32,
    audio_recoveries: u32,
    /// True while the transceiver link is meant to be up.
    ///
    /// Distinct from the enabling setting, which says the feature is available
    /// rather than that the link should be open: the panel offers a start and a
    /// stop beside that switch, and pressing stop must not be undone.
    rig_wanted: bool,
    rig_retry: f32,
    rig_backoff: f32,
    rig_recoveries: u32,
    /// Frames read from the capture queue, both channels.
    ///
    /// Handed to the decoders as a pair rather than reduced. A reduction is real
    /// and folds a two sided spectrum about nought, so a station below the dial
    /// would be superimposed on one above it.
    audio_buf: Vec<[f32; 2]>,
    devices: Vec<DeviceInfo>,

    /// Names as the catalogue knows them, and the labels shown beside them.
    rig_profile_names: Vec<String>,
    rig_profile_labels: Vec<String>,
    rig_port_names: Vec<String>,
    rig_port_labels: Vec<String>,
    /// Diagnostics of the selected description, empty when it is sound.
    rig_issues: Vec<String>,
    /// Mode the transceiver reports, empty when it reports none.
    rig_mode: &'static str,
    /// Rectangle the frequency readout occupied on the previous frame.
    readout_rect: Rect,
    /// Advance of one monospaced character in that readout.
    readout_char_w: f32,
    /// Digit the repeat is running on, and how long it has been held.
    ///
    /// The index is part of the state because moving to another digit restarts
    /// the pause: a pointer that slid across the readout while held has asked for
    /// a different decade, not for the previous one to keep going.
    readout_index: usize,
    readout_hold: f32,
    readout_next: f32,
    /// Step of the previous frame, so the repeat has a clock.
    ///
    /// Recorded rather than threaded through the declaration, because the readout
    /// is probed from inside the frame build and the step is measured outside it.
    frame_dt: f32,

    /// What the current receiver filter drag grabbed.
    filter_grab: FilterGrab,
    /// Pointer frequency at the press, for a whole band move.
    filter_anchor_hz: f32,
    /// Edges at the press, so a band pushed against a limit recovers its
    /// position when the pointer comes back rather than staying squashed.
    filter_base: (f32, f32),

    /// Frequency latched under the pointer when a panorama drag began.
    pan_anchor_hz: f32,
    /// Channel a keying gesture grabbed, held for the rest of the drag.
    ///
    /// Decided at the press. A gesture that recomputed its target per frame
    /// would hand the drag to whichever marker the pointer happened to cross,
    /// which turns a move into a series of jumps between channels.
    channel_drag: Option<u32>,

    /// Known frequencies, read once at startup.
    stations: crate::stations::Catalog,
    /// Entries inside the visible span, as indices into the catalogue.
    visible_stations: Vec<usize>,
    /// Those entries formatted for the band panel.
    station_rows: Vec<panel::StationRow>,
    /// Segment of the band plan the dial sits in, empty when outside one.
    rig_segment: String,

    /// Call sign resolution and the spot list.
    book: Book,
    /// Spots formatted for the panel.
    spot_rows: Vec<panel::SpotRow>,
    /// Frequencies behind those rows, so a press names a station.
    spot_targets: Vec<i64>,
    /// Seconds until the next scan.
    callsign_poll: f32,
    /// Finished lines already scanned, as a count of everything appended.
    lines_scanned: u64,
    /// Fingerprint of the settings the book was built from.
    callsign_signature: u64,

    /// Text the operator is typing.
    entry: Entry,
    /// Bands the plan lists, read once because the plan does not change.
    band_list: Vec<crate::stations::Band>,
    /// Band the dial was in, and the last frequency inside it.
    ///
    /// Both are needed and the second is why: the stack has to record where the
    /// operator settled on the band they are leaving, and by the time the change
    /// is noticed the dial already names the new one.
    last_band: Option<&'static str>,
    last_band_hz: Option<i64>,
    /// Point a measurement is taken from, in audio.
    reference_hz: Option<f32>,
    /// Seconds until the next automatic notch search.
    notch_poll: f32,
    /// Tone the search is proposing, and how many passes it has won.
    notch_candidate: f32,
    notch_hits: u32,

    /// Positions the operator picked in the lists.
    sel: Selections,
    /// Position and filter of the decoded text panel.
    decode_view: DecodeView,
    /// Line rectangles the previous frame drew, for the click gesture.
    decode_hits: Vec<DecodeHit>,

    /// Pointer position over the display, as a fraction of the data area.
    ///
    /// Held for one frame because two consumers need it and they run at
    /// different points: the status line is formatted during the declaration
    /// and the crosshair is drawn after it.
    cursor_fraction: Option<f32>,
    /// Cyclic recorder, absent while nothing is recording.
    recorder: Option<Recorder>,
    recorder_status: RecorderStatus,
    /// State of the reception, published to the recorder once per frame.
    record_meta: Arc<SharedMeta>,
    /// Replay, absent while the display is fed from the air.
    ///
    /// Its presence is what switches the source. An enumeration would state the
    /// same thing and would have exactly two variants forever, because a display
    /// is fed either by a device or by a file and there is no third case.
    replay: Option<ReplayStream>,
    replay_status: Option<ReplayStatus>,
    /// Generation the chain was last reset for. A seek raises it, and audio from
    /// before and after one is not continuous.
    replay_generation: u32,
    /// Copy of the timeline, refreshed on the poll interval so the scrub bar is
    /// drawn without taking the lock the replay thread holds per block.
    replay_timeline: Timeline,
    /// Seconds until the next check for appended segments.
    timeline_poll: f32,
    /// Segments on disk, formatted for the list.
    segment_rows: Vec<panel::SegmentRow>,
    /// Paths behind those rows, so an export names a file rather than a row.
    segment_paths: Vec<PathBuf>,
    segments_total: String,
    surface_size: (u32, u32),
    dpi_scale: f32,
    ui_scale: f32,
    accent: Color,
    /// Settings that differ from the values the build ships with.
    ///
    /// Formatted here rather than in the panel, because the comparison reads two
    /// whole documents and the panel is declared every frame.
    deviations: Vec<String>,
    deviation_poll: f32,
    /// Active tab, and the two selections the layout editor keeps.
    tab: usize,
    editing_tab: usize,
    add_section: usize,

    minimized: bool,
    in_modal_resize: bool,
    running: bool,
    high_res_timer: bool,
    time: f32,
}

impl App {
    pub fn new(settings: Settings) -> Result<App> {
        let cfg = WindowConfig {
            title: format!("RXScope {}", env!("CARGO_PKG_VERSION")),
            width: settings.ui.window_width,
            height: settings.ui.window_height,
            x: if settings.ui.window_x >= 0 { Some(settings.ui.window_x) } else { None },
            y: if settings.ui.window_y >= 0 { Some(settings.ui.window_y) } else { None },
            min_width: 900,
            min_height: 560,
            maximized: settings.ui.maximized,
            custom_frame: settings.appearance.custom_frame,
        };
        let window = Window::new(&cfg)?;

        // A one millisecond timer period keeps the frame limiter and the audio
        // period honest; the default granularity is around fifteen.
        let high_res_timer = platform::win32::begin_high_resolution_timing();
        if !high_res_timer {
            crate::log_warn!("app", "timeBeginPeriod failed, frame pacing will be coarse");
        }

        let dpi_scale = window.dpi_scale();
        let surface_size = window.client_size();
        let ui_scale = dpi_scale * settings.ui.scale;

        let mut renderer = Renderer::new(
            window.hwnd(),
            window.hinstance(),
            surface_size,
            settings.ui.vsync,
            &settings.render,
        )?;

        let fonts = FontSystem::new(
            &settings.ui.font_path,
            &settings.ui.mono_font_path,
            settings.ui.glyph_atlas_size,
            settings.ui.text_gamma,
            &mut renderer,
        )?;
        let font_name = fonts.family_name().to_string();

        let dsp = DspEngine::new(&settings, settings.audio.dsp_sample_rate, &mut renderer)?;
        let decode = DecoderBank::new(
            &settings,
            settings.audio.dsp_sample_rate,
            settings.complex_signal(),
        );
        let rig = RigLink::new(&settings.rig);

        let gpu_name = renderer.device_name().to_string();
        let accent = Color::hex(settings.ui.accent_rgb);
        let mut theme = Theme::dark(accent);
        theme.apply(accent, &settings.appearance);
        let gui = Ui::new(theme);

        let language_dir = settings.language_dir();
        let languages = scan_languages(&language_dir);

        crate::log_info!(
            "app",
            "surface {}x{} ui_scale {:.2} vsync {} gpu {}",
            surface_size.0,
            surface_size.1,
            ui_scale,
            settings.ui.vsync,
            gpu_name
        );

        // Read before the settings move into the structure. A struct literal
        // evaluates its fields in the order written and the settings field is
        // first, so anything below it would be reading a moved value.
        let monitor_receiver = SharedReceiver::new(&settings.receiver, settings.sdr_mode());
        let last_detector = settings.receiver.detector;
        // Cloned rather than borrowed across the literal below. A struct literal
        // evaluates its fields in the order written and the settings field is
        // first, so a borrow taken here would be a borrow of a moved value by
        // the time the book is constructed.
        let settings_for_book = settings.clone();
        // Beside the configuration rather than inside it. The configuration is
        // rewritten on every exit, and a list an operator maintains by hand must
        // not be reformatted underneath them.
        let stations = crate::stations::Catalog::load(
            &Settings::default_path().with_file_name("stations.ini"),
        );

        let mut app = App {
            settings,
            window,
            renderer,
            fonts,
            gui,
            dsp,
            decode,
            rig,
            draw_list: DrawList::new(),
            clock: FrameClock::new(),
            events: Vec::with_capacity(128),
            gpu_name,
            font_name,
            channel_view: Vec::with_capacity(crate::decode::channels::MAX_CHANNELS),
            languages,
            language_dir,
            monitor: None,
            monitor_status: crate::audio::MonitorStatus::idle(),
            monitor_receiver,
            last_detector,
            last_rig_mode: None,
            last_dial: None,
            shift_columns: 0.0,
            shift_bins: 0.0,
            monitor_devices: Vec::new(),
            listen_band: None,
            audio: None,
            audio_status: AudioStatus::idle(),
            audio_wanted: false,
            audio_retry: 0.0,
            audio_backoff: RECOVER_FIRST_S,
            audio_recoveries: 0,
            rig_wanted: false,
            rig_retry: 0.0,
            rig_backoff: RECOVER_FIRST_S,
            rig_recoveries: 0,
            audio_buf: vec![[0.0, 0.0]; AUDIO_CHUNK],
            devices: Vec::new(),
            rig_profile_names: Vec::new(),
            rig_profile_labels: Vec::new(),
            rig_port_names: Vec::new(),
            rig_port_labels: Vec::new(),
            rig_issues: Vec::new(),
            rig_mode: "",
            readout_rect: Rect::default(),
            readout_char_w: 0.0,
            readout_index: usize::MAX,
            readout_hold: 0.0,
            readout_next: 0.0,
            frame_dt: 0.0,
            filter_grab: FilterGrab::None,
            filter_anchor_hz: 0.0,
            filter_base: (0.0, 0.0),
            pan_anchor_hz: 0.0,
            channel_drag: None,
            stations,
            visible_stations: Vec::with_capacity(32),
            station_rows: Vec::with_capacity(32),
            rig_segment: String::new(),
            book: Book::new(&settings_for_book.callsign),
            spot_rows: Vec::with_capacity(64),
            spot_targets: Vec::with_capacity(64),
            callsign_poll: 0.0,
            lines_scanned: 0,
            callsign_signature: callsign_signature(&settings_for_book.callsign),
            entry: Entry::default(),
            band_list: crate::stations::bands(),
            last_band: None,
            last_band_hz: None,
            reference_hz: None,
            notch_poll: 0.0,
            notch_candidate: 0.0,
            notch_hits: 0,
            sel: Selections::default(),
            decode_view: DecodeView::default(),
            decode_hits: Vec::with_capacity(64),
            cursor_fraction: None,
            recorder: None,
            recorder_status: RecorderStatus::idle(),
            record_meta: SharedMeta::new(),
            replay: None,
            replay_status: None,
            replay_generation: 0,
            replay_timeline: Timeline::default(),
            timeline_poll: 0.0,
            segment_rows: Vec::new(),
            segment_paths: Vec::new(),
            segments_total: String::new(),
            surface_size,
            dpi_scale,
            ui_scale,
            accent,
            deviations: Vec::new(),
            deviation_poll: 0.0,
            tab: 0,
            editing_tab: 0,
            add_section: 0,
            minimized: false,
            in_modal_resize: false,
            running: true,
            high_res_timer,
            time: 0.0,
        };

        app.rescan_devices();
        app.rescan_monitor_devices();
        app.refresh_rig_lists();
        // The receiver is expected to be live on start; a failure only lands in
        // the status line so the interface stays usable.
        app.start_audio();
        if app.settings.rig.enabled {
            app.rig_wanted = true;
            app.rig.start(&app.settings.rig);
        }
        app.rescan_segments();
        if app.settings.record.enabled && app.settings.record.auto_start {
            app.start_recording();
        }
        Ok(app)
    }

    // ------------------------------------------------------------- audio

    fn rescan_devices(&mut self) {
        self.devices = crate::audio::enumerate(self.settings.audio.backend);
        let wanted = self.settings.audio.device_id.clone();
        self.sel.device = self.devices.iter().position(|d| d.id == wanted).unwrap_or(0);
        crate::log_info!("app", "{} capture devices found", self.devices.len());
    }

    fn apply_device_selection(&mut self) {
        if let Some(d) = self.devices.get(self.sel.device) {
            self.settings.audio.device_id = d.id.clone();
            self.settings.audio.device_name = d.name.clone();
        }
    }

    /// True when the selected endpoint is a render device read through the
    /// loopback path. The identifier carries the direction as a prefix, so this
    /// needs no call into the device layer.
    fn selected_is_loopback(&self) -> bool {
        self.settings.audio.device_id.starts_with("out:")
    }

    fn stop_audio(&mut self) {
        // Recorded before anything is torn down, and set again by the start that
        // follows an internal stop. A caller that meant to restart states so
        // afterwards; a caller that meant to stop does not.
        self.audio_wanted = false;

        // The recorder is stopped first and on purpose. It holds a tap of the
        // stream, and a recorder left running against a stream that has gone
        // would spin on an empty queue and close its segment only on the next
        // stop request.
        self.stop_recording();
        self.stop_monitor();

        if let Some(mut s) = self.audio.take() {
            s.stop();
        }
        self.audio_status = AudioStatus::idle();
        self.dsp.reset();
        self.decode.reset();
    }

    fn start_audio(&mut self) {
        self.stop_audio();
        self.audio_wanted = true;
        let cfg = CaptureConfig::from_settings(&self.settings.audio, &self.settings.dsp);
        match CaptureStream::start(cfg) {
            Ok(stream) => {
                let rate = self.source_rate();
                self.audio = Some(stream);
                let complex = self.signal_is_complex();
                self.dsp.sync(&self.settings, rate);
                self.dsp.spectrum.set_complex(complex);
                self.decode.sync(&self.settings, rate, complex);
                self.start_monitor();
                if self.settings.record.enabled && self.settings.record.auto_start {
                    self.start_recording();
                }
            }
            Err(e) => {
                crate::log_error!("app", "cannot start capture: {}", e);
                let mut status = AudioStatus::idle();
                status.error = e.to_string();
                self.audio_status = status;
            }
        }
    }

    fn rescan_monitor_devices(&mut self) {
        self.monitor_devices = crate::audio::enumerate_output();
        let wanted = self.settings.audio.monitor_device_id.clone();
        self.sel.monitor = self
            .monitor_devices
            .iter()
            .position(|d| d.id == wanted)
            .unwrap_or(0);
    }

    fn apply_monitor_selection(&mut self) {
        if let Some(d) = self.monitor_devices.get(self.sel.monitor) {
            self.settings.audio.monitor_device_id = d.id.clone();
            self.settings.audio.monitor_device_name = d.name.clone();
        }
    }

    /// True when playing the monitor would feed the output back into the input.
    ///
    /// The loopback path records what the system plays, so a monitor playing to
    /// the same endpoint closes a loop with gain in it. The result is not merely
    /// unpleasant: the gain control raises it until it saturates, and the
    /// decoders see a carrier that is not on the air.
    fn monitor_feedback(&self) -> bool {
        let capture = match self.settings.audio.device_id.strip_prefix("out:") {
            Some(raw) => raw,
            None => return false,
        };
        let monitor = self.settings.audio.monitor_device_id.as_str();
        capture == monitor
    }

    fn stop_monitor(&mut self) {
        if let Some(mut m) = self.monitor.take() {
            m.stop();
        }
        if let Some(s) = self.audio.as_ref() {
            s.set_monitor(false);
        }
        self.monitor_status = crate::audio::MonitorStatus::idle();
    }

    /// Starts the monitor on the tap of the running capture stream.
    fn start_monitor(&mut self) {
        self.stop_monitor();
        if !self.settings.audio.monitor_enabled || self.monitor_feedback() {
            return;
        }

        let (rate, tap) = match self.audio.as_ref() {
            Some(s) => (s.rate(), s.tap()),
            None => return,
        };

        let cfg = crate::audio::MonitorConfig {
            device_id: self.settings.audio.monitor_device_id.clone(),
            source_rate: rate,
            period_ms: self.settings.audio.capture_buffer_ms.max(10),
            agc: crate::audio::AgcConfig {
                enabled: self.settings.dsp.agc_enabled,
                attack_ms: self.settings.dsp.agc_attack_ms,
                release_ms: self.settings.dsp.agc_release_ms,
                target_db: self.settings.dsp.agc_target_db,
            },
            receiver: self.monitor_receiver.clone(),
        };

        match crate::audio::MonitorStream::start(cfg, tap) {
            Ok(stream) => {
                if let Some(s) = self.audio.as_ref() {
                    s.set_monitor(true);
                }
                self.monitor = Some(stream);
            }
            Err(e) => {
                crate::log_error!("app", "cannot start the monitor: {}", e);
                let mut status = crate::audio::MonitorStatus::idle();
                status.error = e.to_string();
                self.monitor_status = status;
            }
        }
    }

    // ---------------------------------------------------- record and replay

    fn stop_recording(&mut self) {
        if let Some(mut r) = self.recorder.take() {
            r.stop();
        }
        if let Some(s) = self.audio.as_ref() {
            s.set_recording(false);
        }
        self.recorder_status = RecorderStatus::idle();
        self.rescan_segments();
    }

    /// Starts the recorder on the tap of the running capture stream.
    ///
    /// The geometry is derived from the stream rather than from the settings
    /// alone: the rate is whatever the chain settled on, and the channel count
    /// follows the quadrature setting, because a real input duplicates its one
    /// channel and storing the copy would double the file for nothing.
    fn start_recording(&mut self) {
        self.stop_recording();
        if !self.settings.record.enabled {
            return;
        }

        let (rate, tap) = match self.audio.as_ref() {
            Some(s) => (s.rate(), s.record_tap()),
            None => {
                crate::log_warn!("record", "nothing to record, the capture is not running");
                return;
            }
        };

        // The live answer rather than the source one: the tap is fed by the
        // capture thread, so a recording opened for playback says nothing about
        // what is being written now.
        let complex = self.live_complex();
        let cfg = RecorderConfig::from_settings(
            &self.settings.record,
            rate,
            complex,
            self.record_meta.clone(),
        );

        match Recorder::start(cfg, tap) {
            Ok(recorder) => {
                if let Some(s) = self.audio.as_ref() {
                    s.set_recording(true);
                }
                self.recorder = Some(recorder);
            }
            Err(e) => {
                crate::log_error!("record", "cannot start: {}", e);
                let mut status = RecorderStatus::idle();
                status.error = e.to_string();
                self.recorder_status = status;
            }
        }
    }

    /// Publishes what the recorder writes into every block marker.
    ///
    /// Called once per frame. The values move at the rate an operator turns a
    /// dial, so a marker per block is already finer than the thing it describes.
    fn sync_record_meta(&mut self) {
        let complex = self.live_complex();
        let mapping = self.rig.mapping(&self.settings.rig, complex);
        let mode = self.rig.mode().map(mode_code).unwrap_or(0);
        let sideband = mapping.map(|m| sideband_code(m.sideband)).unwrap_or(0);

        self.record_meta.publish(
            mapping.map(|m| m.dial_hz),
            self.settings.receiver.tune_hz,
            mode,
            sideband,
            self.settings.sdr_mode(),
        );

        if let Some(r) = self.recorder.as_ref() {
            self.recorder_status = r.status();
        }
    }

    /// Rereads the segment directory.
    ///
    /// An unreadable file is listed rather than skipped: an operator whose
    /// recording will not open is told which one is at fault instead of finding
    /// it missing.
    fn rescan_segments(&mut self) {
        let directory = crate::record::resolve(&self.settings.record.path);
        let found = crate::record::scan(&directory);

        self.segment_rows.clear();
        self.segment_paths.clear();
        let mut total = 0u64;

        for entry in &found {
            total += entry.bytes;
            let detail = if entry.info.is_some() {
                format!(
                    "{:>6.0} s  {:>7.1} MB",
                    entry.seconds,
                    entry.bytes as f64 / (1024.0 * 1024.0)
                )
            } else {
                format!("{:>7.1} MB  (!)", entry.bytes as f64 / (1024.0 * 1024.0))
            };
            self.segment_rows.push(panel::SegmentRow {
                name: entry.name.clone(),
                detail,
                usable: entry.info.is_some() && entry.blocks > 0,
            });
            self.segment_paths.push(entry.path.clone());
        }

        self.segments_total = format!(
            "{} seg   {:.0} MB",
            found.len(),
            total as f64 / (1024.0 * 1024.0)
        );
    }

    /// Writes one segment into an export container.
    fn export_segment(&mut self, index: usize) {
        let source = match self.segment_paths.get(index) {
            Some(p) => p.clone(),
            None => return,
        };
        let directory = crate::record::resolve(&self.settings.record.export_path);
        let result = crate::record::export_segment(
            &source,
            &directory,
            self.settings.record.export_format,
            self.settings.record.export_bits,
        );
        if let Err(e) = result {
            crate::log_error!("record", "export failed: {}", e);
        }
    }

    /// Opens the whole directory as one recording.
    ///
    /// The directory rather than a file, because the timeline spans the segments
    /// and a seek across a boundary is what makes a cyclic recording browsable
    /// at all. A single segment is the same thing with one entry.
    fn open_replay(&mut self) {
        self.close_replay();

        let directory = crate::record::resolve(&self.settings.record.path);
        let timeline = Timeline::build(&directory);
        if timeline.is_empty() {
            crate::log_warn!("replay", "{} holds no readable audio", directory.display());
            return;
        }

        let rate = timeline.rate;
        let copy = timeline.clone();
        match ReplayStream::open(timeline) {
            Ok(stream) => {
                // The monitor is stopped rather than switched over. Its tap is
                // fed by the capture thread, so with a replay open it would play
                // live audio beside a display showing something else, which is
                // worse than silence.
                self.stop_monitor();

                // The chain is rebuilt for the recording, which may have been
                // made at another rate or through another input. The arrangement
                // comes from the recording rather than from the settings: a
                // segment holds whichever it was made with.
                let complex = copy.complex;
                self.dsp.sync(&self.settings, rate);
                self.dsp.spectrum.set_complex(complex);
                self.decode.sync(&self.settings, rate, complex);
                self.dsp.reset();
                self.decode.reset();

                self.replay_generation = stream.status().generation;
                self.replay_timeline = copy;
                self.replay = Some(stream);
                self.timeline_poll = 0.0;
                crate::log_info!(
                    "replay",
                    "open, {:.1} seconds across {} segments",
                    self.replay_timeline.seconds(),
                    self.replay_timeline.segments.len()
                );
            }
            Err(e) => crate::log_error!("replay", "cannot open: {}", e),
        }
    }

    fn close_replay(&mut self) {
        if let Some(mut r) = self.replay.take() {
            r.stop();
        }
        self.replay_status = None;
        self.replay_timeline = Timeline::default();

        // The chain holds replayed audio, and the rate may differ from the one
        // the device delivers.
        let rate = self
            .audio
            .as_ref()
            .map(|s| s.rate())
            .unwrap_or(self.settings.audio.dsp_sample_rate);
        let complex = self.signal_is_complex();
        self.dsp.sync(&self.settings, rate);
        self.dsp.spectrum.set_complex(complex);
        self.decode.sync(&self.settings, rate, complex);
        self.dsp.reset();
        self.decode.reset();
        self.start_monitor();
    }

    /// True while the display is fed from a recording.
    fn replaying(&self) -> bool {
        self.replay.is_some()
    }

    /// Rate the processing chain runs at.
    fn source_rate(&self) -> u32 {
        match self.replay.as_ref() {
            Some(r) => r.rate(),
            None => self
                .audio
                .as_ref()
                .map(|s| s.rate())
                .unwrap_or(self.settings.audio.dsp_sample_rate),
        }
    }

    /// True when the live capture is a quadrature pair.
    ///
    /// Two facts folded together, and the second one is the device. A mono
    /// endpoint duplicates its single channel into both slots of a frame, and a
    /// pair of identical channels read as a quadrature one produces a spectrum
    /// exactly symmetric about nought: every station drawn twice, once on each
    /// side. That is the one failure a two sided display must not have, because
    /// it looks like a band rather than like a fault.
    ///
    /// Held apart from the source predicate below, which answers for whatever is
    /// feeding the display and during replay describes the recording. The
    /// recorder taps the capture rather than the replay, so it needs the live
    /// answer even while a recording is open.
    fn live_complex(&self) -> bool {
        self.settings.complex_signal() && self.audio_status.channels >= 2
    }

    /// True when the signal is complex from the source to the display.
    ///
    /// During replay the answer is a property of the recording rather than of
    /// the settings: a segment recorded from a quadrature input stays two sided
    /// however the receiver is configured now, and one recorded from a real
    /// input cannot be made two sided by a switch.
    fn signal_is_complex(&self) -> bool {
        if self.replaying() {
            return self.replay_timeline.complex;
        }
        self.live_complex()
    }

    /// Correspondence between the spectrum and the band, from whichever source
    /// is feeding the display.
    ///
    /// During replay it comes from the block marker. Reading the transceiver
    /// instead would label a recording made an hour ago with the frequency the
    /// dial happens to sit on now, which is the one labelling that is certainly
    /// wrong.
    fn current_mapping(&self) -> Option<Mapping> {
        if !self.settings.rig.rf_axis {
            return None;
        }
        let complex = self.signal_is_complex();

        match self.replay.as_ref() {
            None => self.rig.mapping(&self.settings.rig, complex),
            Some(stream) => {
                let marker = stream.marker();
                let dial = marker.dial()?;

                // A complex recording carries the sign of the offset in its
                // samples, so there is no sideband to apply. A real one needs
                // the one that was in force when it was made.
                let sideband = if complex {
                    crate::rig::Sideband::Upper
                } else {
                    match marker.sideband {
                        2 => crate::rig::Sideband::Lower,
                        1 => crate::rig::Sideband::Upper,
                        // The recording states none, most often because the
                        // description could not read the mode. The current
                        // setting stands in, which is the only value available
                        // and is right whenever the receiver has not changed.
                        _ => match self.settings.rig.sideband {
                            crate::config::settings::SidebandMode::Lower => {
                                crate::rig::Sideband::Lower
                            }
                            _ => crate::rig::Sideband::Upper,
                        },
                    }
                };

                // The sidetone pitch is a property of the transceiver rather
                // than of the moment, so it is taken from the settings. Storing
                // it per block would record the same number several thousand
                // times.
                let keyed = !complex && matches!(marker.mode, 1 | 2);
                let zero_hz = if keyed { self.settings.rig.cw_pitch_hz } else { 0.0 };

                Some(Mapping {
                    dial_hz: dial,
                    sideband,
                    zero_hz,
                    trim_hz: self.settings.rig.offset_hz,
                })
            }
        }
    }

    /// Reacts to a seek and refreshes the timeline.
    ///
    /// Two jobs on two clocks. A seek has to be noticed on the frame it
    /// happened, because the chain then holds audio from either side of a
    /// discontinuity; a new segment only has to be noticed within a couple of
    /// seconds, and finding one costs a directory read.
    fn sync_replay(&mut self, dt: f32) {
        // The status is read and the borrow released before anything acts on
        // it. Holding a reference to the stream across the work below would
        // hold a borrow of the whole structure, and two of the steps write to
        // it: the chain reset and the directory rescan.
        let status = match self.replay.as_ref() {
            Some(s) => s.status(),
            None => return,
        };

        if status.generation != self.replay_generation {
            self.replay_generation = status.generation;
            // Everything downstream measures against a continuous stream: the
            // keying detectors hold a threshold and an element clock, the
            // spectrum holds an average, and both would carry the moment before
            // the jump into the moment after it.
            //
            // The waterfall is left alone. A hard seam between two times is
            // honest and readable, and clearing it would cost a full texture
            // upload for a picture the operator can already interpret.
            self.dsp.reset();
            self.decode.reset();
        }
        self.replay_status = Some(status);

        self.timeline_poll -= dt;
        if self.timeline_poll > 0.0 {
            return;
        }
        self.timeline_poll = TIMELINE_POLL_S;

        let directory = crate::record::resolve(&self.settings.record.path);

        // The tail is cheap and the rebuild is not, so the cheap one runs first
        // and the expensive one only when a segment actually appeared. Both
        // answers are taken in one short borrow.
        let (grew, stale) = match self.replay.as_ref() {
            Some(s) => (s.refresh_tail(), self.replay_timeline.is_stale(&directory)),
            None => return,
        };

        if stale {
            // Built outside the lock the replay thread takes per block, and
            // swapped in afterwards, so a rebuild of a long recording does not
            // stall playback.
            let next = Timeline::build(&directory);
            if !next.is_empty() {
                if let Some(s) = self.replay.as_ref() {
                    s.replace_timeline(next);
                }
            }
            self.rescan_segments();
        } else if grew == 0 {
            // Neither grew nor changed, so the copy already held is current.
            return;
        }

        if let Some(s) = self.replay.as_ref() {
            self.replay_timeline = s.timeline();
        }
    }

    /// Pushes the settings the monitor reads live and records the band it is
    /// passing.
    fn sync_monitor(&mut self) {
        use crate::config::settings::MonitorWidth;

        let sdr = self.settings.sdr_mode();
        self.monitor_receiver.publish(&self.settings.receiver, sdr);

        if sdr {
            // The band shown is the filter placed around the tuning point, taken
            // from the same numbers the chain was planned from, so the display
            // and the ear agree about where it sits as well as how wide it is.
            self.listen_band = Some(self.settings.receiver.absolute_band());
        } else {
            let centre = if self.settings.audio.monitor_follow {
                // Existence rather than magnitude: a channel below the tuning
                // point has a negative centre, and a test against twenty hertz
                // sent the monitor back to the stated frequency instead.
                if self.decode.status().cw_channels > 0 {
                    self.decode.status().cw_tone_hz
                } else {
                    self.settings.audio.monitor_centre_hz
                }
            } else {
                self.settings.audio.monitor_centre_hz
            };

            // Following the detector reads the effective width rather than the
            // requested one. The two differ whenever the working speed forced a
            // shorter analysis window, and the effective figure is the one the
            // decoder is actually hearing through.
            let width = match self.settings.audio.monitor_width_mode {
                MonitorWidth::Detector => self.decode.status().cw_bandwidth_hz.max(50.0),
                MonitorWidth::Independent => self.settings.audio.monitor_bandwidth_hz,
            };

            self.listen_band = if self.settings.audio.monitor_filter {
                Some((centre - width * 0.5, centre + width * 0.5))
            } else {
                None
            };

            if let Some(monitor) = self.monitor.as_ref() {
                monitor.set_filter(self.settings.audio.monitor_filter);
                monitor.set_agc(self.settings.dsp.agc_enabled);
                monitor.set_passband(centre, width);
                monitor.set_pitch(self.settings.audio.monitor_pitch_hz);
                // Without this the listening path forms its own analytic signal
                // from a reduction that has already folded, so a channel below
                // the tuning point is heard as its mirror above it.
                monitor.set_complex(self.signal_is_complex());
            }
        }

        if let Some(monitor) = self.monitor.as_ref() {
            monitor
                .set_volume(crate::audio::monitor::volume_gain(self.settings.audio.monitor_volume));
            monitor.set_channel_mode(self.settings.audio.channel_mode);
            self.monitor_status = monitor.status();
        }
    }

    /// Keeps the local detector and the transceiver mode in step.
    fn sync_mode_link(&mut self) {
        let rig_mode = self.rig.mode();
        let local_moved = self.settings.receiver.detector != self.last_detector;
        let rig_moved = rig_mode != self.last_rig_mode;

        let link = self.settings.receiver.mode_link;
        let may_drive = matches!(link, ModeLink::Drive | ModeLink::Both);
        let may_follow = matches!(link, ModeLink::Follow | ModeLink::Both);

        if local_moved && may_drive {
            if let Some(mode) = rig_mode_of(self.settings.receiver.detector) {
                if Some(mode) != rig_mode && self.rig.set_mode(mode) {
                    crate::log_info!("app", "transceiver driven to {}", mode.name());
                }
            }
        } else if rig_moved && may_follow {
            if let Some(mode) = rig_mode {
                let wanted = detector_of(mode);
                if wanted != self.settings.receiver.detector {
                    self.settings.receiver.detector = wanted;
                    self.apply_filter_preset();
                    crate::log_info!(
                        "app",
                        "detector follows the transceiver to {}",
                        mode.name()
                    );
                }
            }
        }

        // A mode chosen locally sets the filter whatever the coupling says,
        // including none at all: choosing a mode has to do something visible,
        // and the passband is what it does.
        if local_moved {
            self.apply_filter_preset();
        }

        self.last_detector = self.settings.receiver.detector;
        self.last_rig_mode = rig_mode;
    }

    /// Keeps the record and the view aligned as the dial moves.
    ///
    /// The record is shifted so a station keeps its place on the band, which is
    /// what makes one transmission read as one vertical stripe across a retune.
    /// Whether the view shifts with it is the operator decision: holding the view
    /// keeps the receiver at the same place on screen and lets the panorama
    /// travel, moving it holds the panorama and lets the receiver travel across
    /// it.
    fn sync_display_anchor(&mut self) {
        let mode = self.settings.waterfall.anchor;
        let anchored = mode != crate::config::settings::AnchorMode::Off;
        let mapping = if anchored { self.current_mapping() } else { None };

        let previous = self.last_dial;
        self.last_dial = mapping.map(|m| m.dial_hz);

        if mapping.is_none() {
            // Nothing to be aligned to, so a remainder carried from an earlier
            // session of anchoring describes a correspondence that no longer
            // exists.
            self.shift_columns = 0.0;
            self.shift_bins = 0.0;
            return;
        }

        if let (Some(m), Some(prev)) = (mapping, previous) {
            if prev != m.dial_hz {
                let moved_hz = self.shift_record(m, prev);
                if mode == crate::config::settings::AnchorMode::Band {
                    self.shift_view(moved_hz);
                }
            }
        }

        // Only the arrangement that holds the view has to be pulled back to what
        // is being received. The other one moves the view deliberately, and a
        // pull would undo exactly that.
        if mode == crate::config::settings::AnchorMode::Audio {
            self.follow_reference();
        }
    }

    /// Moves the view by the same amount the record moved.
    ///
    /// The view is stored as a fraction of the whole span, so the shift is the
    /// moved frequency over that span. Clamped at the edges, past which the
    /// panorama travels again because there is nothing further to show.
    fn shift_view(&mut self, moved_hz: f32) {
        let full_span = self.dsp.spectrum.high_hz() - self.dsp.spectrum.low_hz();
        if full_span <= 0.0 {
            return;
        }
        let zoom = self.settings.waterfall.zoom.max(1.0);
        let half = 0.5 / zoom;
        let wanted = self.settings.waterfall.view_centre + moved_hz / full_span;
        self.settings.waterfall.view_centre = wanted.clamp(half, 1.0 - half);
    }

    /// Sets the filter to the preset of the current mode.
    fn apply_filter_preset(&mut self) {
        let complex = self.settings.complex_signal();
        let (low, high) = self.settings.receiver.detector.filter_preset(complex);
        self.settings.receiver.filter_low_hz = low;
        self.settings.receiver.filter_high_hz = high;
    }

    /// Moves whatever the source produced into the engine and the decoders.
    ///
    /// The capture queue is drained whether or not it feeds the display. With a
    /// replay open nobody consumes it, and a queue that fills makes the capture
    /// thread count overruns on a stream that is working perfectly; discarding
    /// is what says the samples were not wanted rather than lost. The recorder
    /// has a tap of its own and is unaffected, which is the point: an operator
    /// scrubbing back through the last minute is still recording the present.
    fn pump_audio(&mut self) -> Result<()> {
        if self.replaying() {
            if let Some(s) = self.audio.as_ref() {
                let pending = s.available();
                if pending > 0 {
                    s.skip(pending);
                }
            }
        }

        // Fields are destructured so the engine, the decoders and the renderer
        // can be held mutably at the same time; they are disjoint members, which
        // a chain of method calls on self would not prove.
        let App {
            audio,
            replay,
            dsp,
            decode,
            renderer,
            audio_buf,
            settings,
            ..
        } = self;

        let skimmer = !settings.sdr_mode();
        let rate;
        let mut consumed = 0usize;

        // The reading differs and everything after it does not, so the two are
        // separated by a closure over the destination rather than by two copies
        // of the chain.
        let mut feed = |frames: &mut [[f32; 2]], dsp: &mut DspEngine, decode: &mut DecoderBank|
         -> Result<()> {
            // Before the transform, because afterwards an impulse is a line
            // across every bin and nothing can tell it from a hundred stations
            // arriving at once. Once, because the same blanked samples go to the
            // display and to the decoders.
            dsp.blank(frames);
            dsp.feed(frames, renderer)?;
            if skimmer {
                // The pair, not its reduction. The reduction is real and a real
                // spectrum is symmetric about nought, so a station below the
                // tuning point would arrive folded onto whatever is above it:
                // one detector would hear two stations and the marker would be
                // drawn on the wrong side of the dial.
                decode.feed(frames, settings);
            }
            Ok(())
        };

        match replay.as_ref() {
            Some(stream) => {
                rate = stream.rate().max(1);
                loop {
                    let n = stream.read(audio_buf);
                    if n == 0 {
                        break;
                    }
                    feed(&mut audio_buf[..n], dsp, decode)?;
                    consumed += n;
                }
            }
            None => {
                let capture = match audio.as_ref() {
                    Some(s) => s,
                    None => return Ok(()),
                };
                rate = capture.rate().max(1);

                // A backlog means the interface stalled. Trimming keeps the
                // waterfall in real time instead of replaying old audio, which
                // would also corrupt every timing estimate in the decoders,
                // hence the reset.
                let capacity = capture.capacity();
                let pending = capture.available();
                if pending > capacity * 3 / 4 {
                    let dropped = capture.skip(pending - capacity / 8);
                    crate::log_warn!("app", "queue backlog, dropped {} frames", dropped);
                    if skimmer {
                        decode.reset();
                    }
                }

                loop {
                    let n = capture.read(audio_buf);
                    if n == 0 {
                        break;
                    }
                    feed(&mut audio_buf[..n], dsp, decode)?;
                    consumed += n;
                }
            }
        }

        // The classifier and the channel allocator run once per drained block
        // rather than per spectrum frame: their own interval timer is far longer
        // than a frame anyway.
        if skimmer && consumed > 0 {
            let dt = consumed as f32 / rate as f32;
            let bin_hz = dsp.spectrum.bin_hz();
            let low_hz = dsp.spectrum.low_hz();
            decode.observe_spectrum(dsp.spectrum.bins(), low_hz, bin_hz, dt, settings);
        }
        Ok(())
    }

    /// Scans the decoded text for call signs and ages the list.
    ///
    /// Two granularities and both are needed. A line still being assembled is
    /// rescanned every pass, because a station sends its call and then pauses for
    /// several seconds and waiting for the line to finish would report it after
    /// the operator had already missed the transmission. A finished line is
    /// scanned once, because its final token was held back while it was
    /// incomplete and would otherwise never be examined.
    fn sync_callsigns(&mut self, dt: f32) {
        // The book is rebuilt when a path or the source moves, which is an
        // operator action rather than a per frame condition.
        let wanted = callsign_signature(&self.settings.callsign);
        if wanted != self.callsign_signature {
            self.callsign_signature = wanted;
            self.book.reload(&self.settings.callsign);
        }

        self.callsign_poll -= dt;
        if self.callsign_poll > 0.0 {
            return;
        }
        self.callsign_poll = CALLSIGN_POLL_S;

        if !self.settings.callsign.lookup_enabled {
            return;
        }
        // The receiver mode does not feed the decoders, so there is no text and
        // the list would age out for no reason the operator could act on.
        if self.settings.sdr_mode() {
            return;
        }

        let mapping = self.current_mapping();
        let channels = std::mem::take(&mut self.channel_view);

        // The signal figures come from whichever channel sits nearest the
        // frequency of the line. A line carries no strength of its own, and the
        // strength is what an operator sorts a spot list by.
        let figures = |hz: f32| -> (f32, f32) {
            let mut best: Option<&ChannelInfo> = None;
            for channel in &channels {
                if (channel.hz - hz).abs() > CHANNEL_MATCH_HZ {
                    continue;
                }
                if best.map(|b| (channel.hz - hz).abs() < (b.hz - hz).abs()).unwrap_or(true) {
                    best = Some(channel);
                }
            }
            best.map(|c| (c.snr_db, c.wpm)).unwrap_or((0.0, 0.0))
        };

        // The pending text is copied out because the scan takes the book
        // mutably and the log lives beside it.
        let mut pending: Vec<(String, f32)> = Vec::new();
        for line in self.decode.log.pending() {
            if !line.text.is_empty() {
                pending.push((line.text.clone(), line.hz));
            }
        }

        let appended = self.decode.log.appended();
        let fresh = appended.saturating_sub(self.lines_scanned);
        let mut finished: Vec<(String, f32)> = Vec::new();
        if fresh > 0 {
            let take = fresh.min(self.decode.log.len() as u64) as usize;
            for line in self.decode.log.lines().rev().take(take) {
                finished.push((line.text.clone(), line.hz));
            }
        }
        self.lines_scanned = appended;

        for (text, hz) in &finished {
            let rf = mapping.map(|m| m.rf_of(*hz));
            let (snr, wpm) = figures(*hz);
            self.book
                .scan(text, *hz, rf, snr, wpm, false, &self.settings.callsign);
        }
        for (text, hz) in &pending {
            let rf = mapping.map(|m| m.rf_of(*hz));
            let (snr, wpm) = figures(*hz);
            self.book
                .scan(text, *hz, rf, snr, wpm, true, &self.settings.callsign);
        }

        self.channel_view = channels;
        self.book.tick();
    }

    /// Formats the spot list for the panel.
    ///
    /// Rebuilt once per frame rather than kept in step incrementally, because a
    /// spot changes on almost every sighting and the list holds a few hundred
    /// rows at most.
    fn refresh_spot_rows(&mut self) {
        self.spot_rows.clear();
        self.spot_targets.clear();

        for spot in self.book.spots() {
            let frequency = if spot.rf_hz != 0 {
                Readout::new(spot.rf_hz, 2).text()
            } else {
                format!("{:.0} Hz", spot.hz)
            };

            // The zone is stated beside the country because a contest exchange
            // asks for it and the operator would otherwise look it up
            // separately.
            let mut where_from = spot.country.clone();
            if spot.cq_zone > 0 {
                where_from.push_str(&format!(" z{}", spot.cq_zone));
            }
            if !spot.note.is_empty() {
                if !where_from.is_empty() {
                    where_from.push_str("  ");
                }
                where_from.push_str(&spot.note);
            }

            let age = spot.age_s() as u64;
            let detail = format!(
                "{:<12}{:<22}{:>5.1}dB {:>3.0}w x{:<3} {}{}:{:02}",
                spot.call,
                where_from,
                spot.snr_db,
                spot.wpm,
                spot.count.min(999),
                if spot.cq { "CQ " } else { "   " },
                age / 60,
                age % 60
            );

            self.spot_rows.push(panel::SpotRow {
                frequency,
                detail,
                // Confidence decides the colour rather than being printed. The
                // operator wants to know which rows to trust, and a second
                // number in a row of numbers is one more thing to read.
                confident: spot.confidence() >= 0.6,
                tunable: spot.rf_hz != 0,
            });
            self.spot_targets.push(spot.rf_hz);
        }
    }

    /// Brings a failed device or link back on its own.
    ///
    /// The failure this exists for is a cable: a USB interface that is knocked,
    /// a converter that is unplugged, a port that a sleeping machine took away.
    /// The stream ends, the reason reaches the status line, and nothing else
    /// happens, so the operator has to notice a display that stopped scrolling
    /// and press start. A receiver that comes back on its own is one nobody has
    /// to watch.
    ///
    /// The delay grows and is never abandoned, see the note on the bound. It is
    /// reset on success rather than decayed, because the next fault is a new
    /// fault and inheriting the delay of the previous one would make a second
    /// knock take half a minute to recover from.
    ///
    /// A recovery that happened is counted and reported. A receiver that quietly
    /// restarts itself twenty times an hour is a cable that wants replacing, and
    /// silently papering over it would hide exactly that.
    fn sync_recovery(&mut self, dt: f32) {
        // The intention is what separates a fault from a stop. A reopen that
        // failed leaves no stream, which is indistinguishable from having been
        // stopped by hand unless the intention is recorded separately.
        let capture_down =
            self.audio_wanted && (self.audio.is_none() || !self.audio_status.running);
        if capture_down {
            self.audio_retry -= dt;
            if self.audio_retry <= 0.0 {
                self.audio_recoveries += 1;
                crate::log_warn!(
                    "app",
                    "capture is down ({}), attempt {} after {:.0} s",
                    if self.audio_status.error.is_empty() {
                        "no reason given"
                    } else {
                        self.audio_status.error.as_str()
                    },
                    self.audio_recoveries,
                    self.audio_backoff
                );
                // The delay for the next attempt, not this one. Doubling before
                // the attempt is what makes the first retry prompt and the
                // twentieth patient.
                self.audio_backoff = (self.audio_backoff * 2.0).min(RECOVER_MAX_S);
                self.audio_retry = self.audio_backoff;
                self.start_audio();
            }
        } else {
            self.audio_backoff = RECOVER_FIRST_S;
            self.audio_retry = 0.0;
        }

        // The link reports its own reason, which is set by a failed open and by
        // the worker ending. Neither is a transient exchange fault: those are
        // logged and counted and never reach here.
        let rig_down = self.rig_wanted
            && self.settings.rig.enabled
            && !self.rig.status().error.is_empty();
        if rig_down {
            self.rig_retry -= dt;
            if self.rig_retry <= 0.0 {
                self.rig_recoveries += 1;
                crate::log_warn!(
                    "app",
                    "the transceiver link is down ({}), attempt {} after {:.0} s",
                    self.rig.status().error,
                    self.rig_recoveries,
                    self.rig_backoff
                );
                self.rig_backoff = (self.rig_backoff * 2.0).min(RECOVER_MAX_S);
                self.rig_retry = self.rig_backoff;
                self.rig.start(&self.settings.rig);
            }
        } else {
            self.rig_backoff = RECOVER_FIRST_S;
            self.rig_retry = 0.0;
        }
    }

    /// Level the meter is fed.
    ///
    /// The whole passband or the band being worked, which is the receiver filter
    /// in the receiver mode and the keying detector otherwise.
    ///
    /// The narrow figure is built as a ratio rather than measured absolutely. The
    /// transform normalization and the front end root mean square are two
    /// different scales, and a reading derived from the first would need its own
    /// calibration; the ratio between two bands of the same transform is exact
    /// whatever that scale is, so applying it to the wide reading leaves the
    /// calibration the operator set against a signal generator valid.
    fn meter_level_db(&self) -> f32 {
        let wide = self.audio_status.rms_db;
        if !self.settings.meter.narrow_band_measure {
            return wide;
        }

        let (low, high) = if self.settings.sdr_mode() {
            self.settings.receiver.absolute_band()
        } else {
            if self.decode.status().cw_channels == 0 {
                // No channel, so there is no band to narrow to and the wide
                // reading is the honest one rather than a reading of an arbitrary
                // slice. Asked of the channel count rather than of the frequency:
                // a channel below the tuning point has a negative centre.
                return wide;
            }
            let centre = self.decode.status().cw_tone_hz;
            let width = self.decode.status().cw_bandwidth_hz.max(50.0);
            (centre - width * 0.5, centre + width * 0.5)
        };

        let (band, total) = self.dsp.spectrum.band_power(low, high);
        if total <= 1e-20 || band <= 1e-20 {
            return wide;
        }
        // Bounded above at nought: narrowing can only remove power, and a
        // positive correction would mean the band held more than the whole.
        let correction = (10.0 * (band / total).log10()).min(0.0);
        wide + correction
    }

    /// Records where the operator was on the band they are leaving.
    ///
    /// Written on the change rather than continuously, so the entry names where
    /// the operator settled rather than wherever the dial happened to be passing
    /// through on the way out.
    fn sync_band_stack(&mut self) {
        let dial = self.rig.display_hz();
        let band = dial
            .and_then(crate::stations::segment_of)
            .map(|segment| segment.name);

        if band != self.last_band {
            if let (Some(previous), Some(hz)) = (self.last_band, self.last_band_hz) {
                self.settings
                    .bands
                    .remember(previous, hz, self.settings.receiver.detector);
            }
            self.last_band = band;
        }
        // Updated after the comparison, so the value read above is the one from
        // the band being left rather than the one that caused the change.
        if let Some(hz) = dial {
            self.last_band_hz = Some(hz);
        }
    }

    /// Brings the transceiver to a band.
    ///
    /// The stored place if there is one, and a tenth of the way in otherwise.
    /// The mode goes through the same field a menu would set, so the coupling to
    /// the transceiver and the filter preset both happen exactly as they do for
    /// any other change of mode.
    fn go_to_band(&mut self, index: usize) {
        let band = match self.band_list.get(index) {
            Some(b) => *b,
            None => return,
        };

        let stored = self.settings.bands.find(band.name).cloned();
        let wanted = stored
            .as_ref()
            .map(|entry| entry.hz)
            .unwrap_or_else(|| band.default_hz());
        // Clamped into the band, because a stored entry may predate a change to
        // the plan and because an operator editing the file by hand can put it
        // anywhere.
        let hz = wanted.clamp(band.low_hz, (band.high_hz - 1).max(band.low_hz));

        if !self.rig.set_frequency(hz) {
            crate::log_warn!("app", "the description defines no way to set the frequency");
            return;
        }
        if let Some(entry) = stored {
            self.settings.receiver.detector = entry.detector;
        }
        crate::log_info!("app", "{} at {} Hz", band.name, hz);
    }

    /// Acts on a frequency the operator typed.
    ///
    /// A band name is tried first. It cannot be a frequency, and the stack is
    /// the more useful reading: somebody typing forty metres wants to be back
    /// where they were, not at forty megahertz.
    fn apply_typed_frequency(&mut self, text: &str) {
        let wanted = text.trim().to_string();
        self.entry.frequency.clear();
        if wanted.is_empty() {
            return;
        }

        if let Some(index) = self
            .band_list
            .iter()
            .position(|band| band.name.eq_ignore_ascii_case(&wanted))
        {
            self.go_to_band(index);
            return;
        }

        match crate::rig::parse_frequency(&wanted) {
            Some(hz) => {
                if self.rig.set_frequency(hz) {
                    crate::log_info!("app", "'{}' read as {} Hz", wanted, hz);
                } else {
                    crate::log_warn!("app", "the description defines no way to set the frequency");
                }
            }
            // Reported rather than ignored. A field that clears itself and does
            // nothing is a field the operator assumes is broken.
            None => crate::log_warn!("app", "'{}' is not a frequency", wanted),
        }
    }

    /// Records the current frequency in the station list.
    ///
    /// The list is otherwise edited by hand, which means an operator who finds
    /// something worth returning to either interrupts the session or loses it.
    fn mark_station(&mut self) {
        let hz = match self.rig.display_hz() {
            Some(v) => v,
            None => return,
        };
        let label = std::mem::take(&mut self.entry.label);
        let (y, mo, d, h, mi, _) = crate::platform::utc_time_full();
        // Coordinated, because the note is read beside a log and a local stamp
        // would force whoever reads it to know which offset applied that day.
        let note = format!("marked {:04}-{:02}-{:02} {:02}{:02}Z", y, mo, d, h, mi);

        if let Err(e) = self.stations.append(hz, &label, self.rig_mode, &note) {
            crate::log_warn!("app", "cannot record the frequency: {}", e);
        }
    }

    /// Compares the configuration against the one the build would write.
    ///
    /// Two hundred settings and one of them wrong is a receiver behaving strangely
    /// for a reason nothing states. The comparison turns that into a glance: what
    /// the list holds is everything an operator has touched.
    ///
    /// Both documents come from the same code, so the two hold the same keys and a
    /// difference is a difference in a value. Session state is excluded, see the
    /// note beside the list of keys.
    fn sync_deviations(&mut self, dt: f32) {
        // Only while the tab that shows it is open, so no other tab pays for it.
        if self.tab != panel::TAB_SETTINGS || !self.settings.ui.show_settings_panel {
            return;
        }
        self.deviation_poll -= dt;
        if self.deviation_poll > 0.0 {
            return;
        }
        self.deviation_poll = DEVIATION_POLL_S;

        let current = self.settings.to_ini();
        let stock = Settings::default().to_ini();

        self.deviations.clear();
        for (section, key, value) in current.pairs() {
            if SESSION_KEYS.iter().any(|&(s, k)| s == section && k == key) {
                continue;
            }
            let stated = match stock.raw(section, key) {
                Some(v) => v,
                // Impossible, because the same code wrote both documents. Skipped
                // rather than reported as a difference against nothing.
                None => continue,
            };
            if value != stated {
                self.deviations
                    .push(format!("{}.{} = {}   was {}", section, key, value, stated));
            }
        }

        // Bounded, because a heavily adjusted installation would otherwise fill
        // the panel with a list nobody reads to the end.
        let total = self.deviations.len();
        if total > MAX_DEVIATIONS {
            self.deviations.truncate(MAX_DEVIATIONS);
            self.deviations.push(format!("and {} more", total - MAX_DEVIATIONS));
        }
    }

    /// Writes the current view into the stored one.
    fn store_view(&mut self) {
        let axis = self.axis();
        self.settings.waterfall.span_hz = axis.span_hz();
        self.settings.waterfall.center_hz = (axis.low_hz() + axis.high_hz()) * 0.5;
        crate::log_info!(
            "app",
            "view stored, {:.0} Hz at {:.0} Hz",
            self.settings.waterfall.span_hz,
            self.settings.waterfall.center_hz
        );
    }

    /// Sets the view from the stored one.
    ///
    /// Converted through the current full span rather than applied directly, so
    /// a view stored at one sample rate still lands on the same frequencies at
    /// another.
    fn recall_view(&mut self) {
        let full_low = self.dsp.spectrum.low_hz();
        let full_span = self.dsp.spectrum.high_hz() - full_low;
        if full_span <= 0.0 {
            return;
        }
        let span = self
            .settings
            .waterfall
            .span_hz
            .clamp(full_span / ZOOM_MAX, full_span);
        let zoom = (full_span / span).clamp(1.0, ZOOM_MAX);
        let half = 0.5 / zoom;
        let centre = ((self.settings.waterfall.center_hz - full_low) / full_span)
            .clamp(half, 1.0 - half);
        self.settings.waterfall.zoom = zoom;
        self.settings.waterfall.view_centre = centre;
        crate::log_info!("app", "view recalled, {:.0} Hz span", span);
    }

    /// Points the notch at the strongest steady tone in the passband.
    ///
    /// Steadiness is the whole of it, see the note on the threshold. The search
    /// only moves a notch the operator already switched on, because the
    /// discrimination is a few decibels wide and switching one on by mistake
    /// removes a station.
    fn sync_auto_notch(&mut self, dt: f32) {
        self.notch_poll -= dt;
        if self.notch_poll > 0.0 {
            return;
        }
        self.notch_poll = NOTCH_POLL_S;

        if !self.settings.dsp.auto_notch
            || !self.settings.sdr_mode()
            || !self.settings.receiver.notch_enabled
        {
            self.notch_hits = 0;
            return;
        }

        let bin_hz = self.dsp.spectrum.bin_hz();
        let low_hz = self.dsp.spectrum.low_hz();
        if bin_hz <= 0.0 {
            return;
        }
        let average = self.dsp.spectrum.average();
        let peaks = self.dsp.spectrum.peaks();
        if average.len() < 16 || peaks.len() != average.len() {
            return;
        }

        // Only inside what the receiver passes. A tone outside it has already
        // been removed and notching it would remove nothing.
        let (band_lo, band_hi) = self.settings.receiver.absolute_band();
        let to_index = |hz: f32| ((hz - low_hz) / bin_hz).round();
        let lo = (to_index(band_lo.min(band_hi)).max(1.0)) as usize;
        let hi = (to_index(band_lo.max(band_hi)).max(0.0) as usize).min(average.len() - 2);
        if hi <= lo + 1 {
            return;
        }

        // Median as the reference, for the reason the classifier uses one: a
        // mean is lifted by the very carrier being searched for.
        let mut sorted: Vec<f32> = average[lo..=hi].to_vec();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let floor = sorted[sorted.len() / 2];

        let mut best: Option<(usize, f32)> = None;
        for k in lo..=hi {
            let level = average[k];
            if level < floor + NOTCH_MARGIN_DB {
                continue;
            }
            // A local maximum, so the skirt of one carrier is not taken for a
            // second one beside it.
            if level < average[k - 1] || level < average[k + 1] {
                continue;
            }
            if peaks[k] - level > NOTCH_STEADY_DB {
                continue;
            }
            if best.map(|(_, held)| level > held).unwrap_or(true) {
                best = Some((k, level));
            }
        }

        let index = match best {
            Some((k, _)) => k,
            None => {
                self.notch_hits = 0;
                return;
            }
        };
        let wanted = low_hz + index as f32 * bin_hz;

        // Into the frame the notch works in, which is the detector output.
        let complex = self.settings.complex_signal();
        let bfo = self.settings.receiver.detector_offset_hz(complex);
        let relative = wanted - self.settings.receiver.tune_hz + bfo;
        // The detector output is real, so a component below the reference and
        // one above it are the same samples by then and the magnitude is the
        // whole of the setting. That is also what lets the notch reach a lower
        // sideband passband, every frequency of which is negative here.
        let relative = relative.abs();

        let width = self.settings.receiver.notch_width_hz.max(10.0);
        if (relative - self.settings.receiver.notch_hz).abs() < width * 0.5 {
            self.notch_hits = 0;
            return;
        }

        if (wanted - self.notch_candidate).abs() < width {
            self.notch_hits = self.notch_hits.saturating_add(1);
        } else {
            self.notch_candidate = wanted;
            self.notch_hits = 1;
        }
        if self.notch_hits < NOTCH_HITS {
            return;
        }

        self.notch_hits = 0;
        let nyquist = self.dsp.spectrum.nyquist_hz();
        self.settings.receiver.notch_hz = relative.clamp(50.0, nyquist);
        crate::log_info!(
            "app",
            "notch moved to {:.0} Hz, {:.0} Hz from the tuning point",
            wanted,
            relative
        );
    }

    /// Builds the axis for this frame.
    fn axis(&self) -> Axis {
        let axis = Axis::new(
            self.dsp.spectrum.low_hz(),
            self.dsp.spectrum.high_hz(),
            self.settings.waterfall.zoom,
            self.settings.waterfall.view_centre,
            self.current_mapping(),
        );
        // The receiver oscillator applies to live audio alone. A recording was
        // made through whatever oscillator was in force then, and the marker
        // already carries where the dial was; adding the present offset would
        // move the whole picture by a number that has nothing to do with it.
        if self.settings.sdr_mode() && !self.replaying() {
            axis.with_tuning(self.settings.receiver.tune_hz)
        } else {
            axis
        }
    }

    /// Width of the left axis gutter, in pixels.
    ///
    /// Nought when the gutters are off, which is what makes the whole feature a
    /// single test rather than a branch at every call site.
    fn gutter_left(&self) -> f32 {
        if self.settings.appearance.axis_gutters {
            (self.settings.appearance.axis_gutter_left * self.ui_scale).round()
        } else {
            0.0
        }
    }

    fn gutter_bottom(&self) -> f32 {
        if self.settings.appearance.axis_gutters {
            (self.settings.appearance.axis_gutter_bottom * self.ui_scale).round()
        } else {
            0.0
        }
    }

    /// Converts a position over a reserved area into a position on the axis.
    ///
    /// The widget system reports a fraction of the whole reserved rectangle,
    /// and the axis occupies only the part of it left over by the gutters. A
    /// caller that skipped this would place a click a gutter width to the left
    /// of where it was aimed, which at a hundredth of a span is worse than the
    /// pointer resolution the gesture was designed around.
    fn axis_fraction(&self, t: f32) -> f32 {
        let gutter = self.gutter_left();
        if gutter <= 0.0 {
            return t;
        }
        let width = self
            .gui
            .custom_rect(TAG_WATERFALL)
            .or_else(|| self.gui.custom_rect(TAG_SPECTRUM))
            .map(|r| r.w)
            .unwrap_or(0.0);
        let usable = width - gutter;
        if usable <= 1.0 {
            return t;
        }
        ((t * width - gutter) / usable).clamp(0.0, 1.0)
    }

    /// Audio frequency the view is kept around.
    fn working_reference_hz(&self) -> f32 {
        if self.settings.sdr_mode() {
            return self.settings.receiver.listen_hz();
        }
        if self.decode.status().cw_channels > 0 {
            return self.decode.status().cw_tone_hz;
        }
        // Nothing tracked yet, so the middle of the band a channel may be
        // opened in: that is where a signal will be found if there is one.
        let lo = self.settings.dsp.passband_low_hz;
        let hi = self
            .settings
            .dsp
            .passband_high_hz
            .min(self.dsp.spectrum.nyquist_hz());
        (lo + hi) * 0.5
    }

    /// Moves the stored picture to match a retune.
    ///
    /// Returns the audio frequency the picture moved by, so a caller that also
    /// moves the view applies the same amount rather than deriving it again.
    fn shift_record(&mut self, m: Mapping, previous_dial: i64) -> f32 {
        // A station keeps its frequency on the air, so its audio position moves
        // against the dial. Everything below follows the station.
        let moved_hz = -(m.sideband.sign() as f32) * (m.dial_hz - previous_dial) as f32;
        let full_low = self.dsp.spectrum.low_hz();
        let full_span = self.dsp.spectrum.high_hz() - full_low;
        if full_span <= 0.0 {
            return 0.0;
        }

        let per_column = full_span / self.dsp.waterfall.width().max(1) as f32;
        self.shift_columns += moved_hz / per_column;
        let columns = self.shift_columns.round();
        // The remainder is kept. A hundred steps of a kilohertz would otherwise
        // walk the record a quarter of a column off the band.
        self.shift_columns -= columns;
        if columns != 0.0 {
            if let Err(e) = self.dsp.waterfall.shift(columns as i32, &mut self.renderer) {
                crate::log_warn!("dsp", "waterfall could not follow the dial: {}", e);
            }
        }

        let bin_hz = self.dsp.spectrum.bin_hz();
        if bin_hz > 0.0 {
            self.shift_bins += moved_hz / bin_hz;
            let bins = self.shift_bins.round();
            self.shift_bins -= bins;
            if bins != 0.0 {
                self.dsp.shift_history(bins as i32);
                self.decode.shift_history(bins as i32, bin_hz);
            }
        }
        moved_hz
    }

    /// Keeps the point being received inside the view.
    fn follow_reference(&mut self) {
        let zoom = self.settings.waterfall.zoom.max(1.0);
        if zoom <= 1.0 {
            return;
        }
        let full_low = self.dsp.spectrum.low_hz();
        let full_span = self.dsp.spectrum.high_hz() - full_low;
        if full_span <= 0.0 {
            return;
        }

        let at = ((self.working_reference_hz() - full_low) / full_span).clamp(0.0, 1.0);
        let half = 0.5 / zoom;
        // Four fifths of the view. A pan inside it is left alone; a pan that
        // pushes the reference out of it is pulled back, because a magnified
        // view that no longer shows what is being received has stopped being a
        // receiver display.
        let margin = 0.4 / zoom;
        let lo = (at - margin).max(half);
        let hi = (at + margin).min(1.0 - half);

        let current = self.settings.waterfall.view_centre;
        self.settings.waterfall.view_centre = if lo <= hi {
            current.clamp(lo, hi)
        } else {
            at.clamp(half, 1.0 - half)
        };
    }

    /// Magnifies about a point, keeping the frequency under it in place.
    fn apply_zoom(&mut self, at: f32, notches: f32) {
        let axis = self.axis();
        let full_low = axis.full_low_hz();
        let full_span = axis.full_span_hz();

        let hz = axis.audio_of_fraction(at);
        // Position within the view, with the mirror removed. Everything below
        // works in that frame, so the direction of the axis does not appear.
        let u = if axis.mirrored() { 1.0 - at } else { at };

        let zoom = (self.settings.waterfall.zoom * ZOOM_PER_NOTCH.powf(notches))
            .clamp(1.0, ZOOM_MAX);
        let span = full_span / zoom;
        let low = hz - u * span;
        let centre = (low + span * 0.5 - full_low) / full_span;

        let half = 0.5 / zoom;
        self.settings.waterfall.zoom = zoom;
        self.settings.waterfall.view_centre = centre.clamp(half, 1.0 - half);
    }

    /// Moves the view under a drag, keeping the frequency grabbed at the press.
    fn apply_pan(&mut self, at: f32, started: bool) {
        let axis = self.axis();
        if started {
            self.pan_anchor_hz = axis.audio_of_fraction(at);
            return;
        }

        let full_low = axis.full_low_hz();
        let full_span = axis.full_span_hz();
        let zoom = self.settings.waterfall.zoom;
        let span = full_span / zoom;
        let u = if axis.mirrored() { 1.0 - at } else { at };

        let low = self.pan_anchor_hz - u * span;
        let centre = (low + span * 0.5 - full_low) / full_span;
        let half = 0.5 / zoom;
        self.settings.waterfall.view_centre = centre.clamp(half, 1.0 - half);
    }

    // --------------------------------------------------------------- rig

    /// Rebuilds the two lists the panel offers and the diagnostics beside them.
    fn refresh_rig_lists(&mut self) {
        self.rig_profile_names.clear();
        self.rig_profile_labels.clear();
        for entry in self.rig.profiles() {
            self.rig_profile_names.push(entry.name.clone());
            // An unusable description is listed rather than hidden, and the
            // marker is what tells the operator to look at the diagnostics
            // underneath instead of at the cable.
            self.rig_profile_labels.push(if entry.usable {
                entry.name.clone()
            } else {
                format!("{}  (!)", entry.name)
            });
        }
        self.sel.profile = self
            .rig_profile_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&self.settings.rig.profile))
            .unwrap_or(0);

        self.rig_port_names.clear();
        self.rig_port_labels.clear();
        for port in self.rig.ports() {
            self.rig_port_names.push(port.name.clone());
            self.rig_port_labels.push(if port.description.is_empty() {
                port.name.clone()
            } else {
                format!("{}  {}", port.name, port.description)
            });
        }
        self.sel.port = self
            .rig_port_names
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&self.settings.rig.port))
            .unwrap_or(0);

        self.refresh_rig_issues();
    }

    fn refresh_rig_issues(&mut self) {
        // The buffer is moved out because the reader borrows the link and the
        // destination lives in the same structure.
        let mut issues = std::mem::take(&mut self.rig_issues);
        self.rig.profile_issues(&self.settings.rig.profile, &mut issues);
        self.rig_issues = issues;
    }

    fn apply_rig_profile(&mut self) {
        if let Some(name) = self.rig_profile_names.get(self.sel.profile) {
            self.settings.rig.profile = name.clone();
        }
        self.refresh_rig_issues();
    }

    fn apply_rig_port(&mut self) {
        if let Some(name) = self.rig_port_names.get(self.sel.port) {
            self.settings.rig.port = name.clone();
        }
    }

    /// Audio frequency a signal should be brought to.
    fn tune_target_hz(&self, mapping: &Mapping) -> f32 {
        if self.settings.sdr_mode() {
            return self.settings.receiver.listen_hz();
        }
        if mapping.zero_hz > 0.0 {
            return mapping.zero_hz;
        }
        if self.decode.status().cw_channels > 0 {
            self.decode.status().cw_tone_hz
        } else {
            self.settings.morse.tone_hz
        }
    }

    /// Bounds of the receiver tuning point, in hertz.
    fn tune_bounds(&self) -> (f32, f32) {
        if !self.settings.complex_signal() || !self.settings.receiver.tune_enabled {
            return (0.0, 0.0);
        }
        let nyquist = self.dsp.spectrum.nyquist_hz();
        (-nyquist, nyquist)
    }

    /// Bounds of a filter edge, relative to the tuning point.
    fn filter_bounds(&self) -> (f32, f32) {
        let nyquist = self.dsp.spectrum.nyquist_hz();
        if self.settings.complex_signal() {
            (-nyquist, nyquist)
        } else {
            (0.0, nyquist)
        }
    }

    /// Audio frequency of the point the readout names.
    fn receiver_reference_hz(&self) -> f32 {
        let complex = self.settings.complex_signal();
        let zero = self
            .rig
            .mapping(&self.settings.rig, complex)
            .map(|m| m.zero_hz)
            .unwrap_or(0.0);
        zero + self.settings.receiver.tune_hz
    }

    /// Moves the receiver tuning point so a signal lands where it listens.
    fn apply_receiver_tune(&mut self, hz: f32) {
        if !self.settings.complex_signal() {
            crate::log_warn!(
                "app",
                "the input is demodulated audio, so there is no tuning point to move"
            );
            return;
        }
        if !self.settings.receiver.tune_enabled {
            crate::log_warn!(
                "app",
                "independent tuning is locked, the receiver follows the transceiver"
            );
            return;
        }
        let (lo, hi) = self.tune_bounds();
        let wanted = (hz - self.settings.receiver.relative_centre_hz()).clamp(lo, hi);
        self.settings.receiver.tune_hz = wanted;
        crate::log_info!(
            "app",
            "receiver tuned to {:.0} Hz, passband centre {:.0} Hz",
            wanted,
            self.settings.receiver.listen_hz()
        );
    }

    /// Frequency the readout names.
    ///
    /// The point being received rather than the dial, because the grid is drawn
    /// through the same mapping and two statements about one point that disagree
    /// are worse than either alone.
    ///
    /// Read by the drawing and by the hit test. A second derivation would be a
    /// second chance for the two to disagree about which decade a character
    /// carries, and the symptom of that is a digit that steps its neighbour.
    fn readout_hz(&self) -> Option<i64> {
        self.axis().reference_rf().or_else(|| self.rig.display_hz())
    }

    /// True while the software oscillator can move the point being received.
    ///
    /// Demodulated audio has no such oscillator, a locked tuning point is an
    /// operator decision that the receiver follows the dial, and a recording was
    /// received through whatever oscillator was in force then.
    fn local_tuning(&self) -> bool {
        self.settings.complex_signal()
            && self.settings.receiver.tune_enabled
            && !self.replaying()
    }

    /// Moves the point the readout names by a stated amount.
    ///
    /// Which oscillator carries the move follows from the mode rather than from
    /// the gesture. With independent tuning the software one carries it, so the
    /// dial, the front panel of the transceiver and the far end all stay where
    /// the operator left them; locked, the dial is the only oscillator there is.
    ///
    /// A step the captured span cannot hold falls through to the dial, and the
    /// receiver keeps its place inside the span. The point moves by the stated
    /// amount on either path, which is what the readout claims, and that is the
    /// property the two have to share.
    fn nudge_reference(&mut self, delta_hz: i64, prefer_rig: bool) {
        if delta_hz == 0 {
            return;
        }
        // A modifier that silently does nothing is worse than one that is
        // ignored, so the request only reaches the transceiver when the
        // description can carry it.
        let use_local = self.local_tuning() && !(prefer_rig && self.rig.can_tune());

        if !use_local {
            self.rig.tune_by(delta_hz);
            return;
        }

        let (lo, hi) = self.tune_bounds();
        let wanted = self.settings.receiver.tune_hz + delta_hz as f32;
        if wanted >= lo && wanted <= hi {
            self.settings.receiver.tune_hz = wanted;
            return;
        }
        if self.rig.tune_by(delta_hz) {
            return;
        }
        // The span is exhausted and the dial cannot move, so the point stops at
        // the edge rather than reporting an arrival that did not happen.
        self.settings.receiver.tune_hz = wanted.clamp(lo, hi);
    }

    /// Reads the pointer over the frequency readout and acts on it.
    ///
    /// Nothing happens during replay: the frequency of a recording is fixed, and
    /// a gesture that moved the dial would put the picture and the markers out of
    /// step with each other.
    fn probe_readout(&mut self) {
        if !self.settings.rig.show_readout || self.replaying() {
            return;
        }
        // Tested once rather than per notch, so a wheel over a readout nothing
        // can move does not fill the log.
        if !self.local_tuning() && !self.rig.can_tune() {
            return;
        }
        let hz = match self.readout_hz() {
            Some(v) => v,
            None => return,
        };
        let probe = match self.gui.probe(self.readout_rect) {
            Some(p) => p,
            None => return,
        };
        if self.readout_char_w <= 0.0 {
            return;
        }

        let prefer_rig = self.gui.mods().ctrl;
        let lowest = if self.settings.rig.readout_fine { 0 } else { 1 };
        let readout = Readout::new(hz, lowest);
        let index = (probe.x / self.readout_char_w) as usize;
        // A separator carries no decade, so a press on one falls through to the
        // plain step rather than being rounded to a neighbouring digit.
        let step = readout.step_at(index);

        // Held, a digit repeats after a pause, which is what a front panel does.
        // The timer runs on the digit rather than on the button, so sliding across
        // the readout while held restarts it on the decade now under the pointer.
        let mut repeats = 0i64;
        let holding = self.settings.rig.readout_repeat && probe.right_down && step.is_some();
        if holding && index == self.readout_index {
            self.readout_hold += self.frame_dt;
            while self.readout_hold >= self.readout_next {
                self.readout_next += READOUT_PERIOD_S;
                repeats += 1;
                if repeats >= READOUT_MAX_BURST {
                    self.readout_next = self.readout_hold + READOUT_PERIOD_S;
                    break;
                }
            }
        } else if holding {
            self.readout_index = index;
            self.readout_hold = 0.0;
            self.readout_next = READOUT_DELAY_S;
        } else {
            self.readout_index = usize::MAX;
            self.readout_hold = 0.0;
        }

        if probe.wheel != 0.0 {
            let unit = step.unwrap_or(self.settings.rig.tune_step_hz as i64);
            let notches = probe.wheel.round() as i64;
            if notches != 0 {
                self.nudge_reference(unit * notches, prefer_rig);
            }
        }
        if let Some(unit) = step {
            // Clearing everything below the digit is what a front panel does,
            // and it is the one action that cannot be reached by stepping. The
            // remainder of the shown value rather than of the dial, which is the
            // whole point of deriving both from one place.
            if probe.left {
                self.nudge_reference(-hz.rem_euclid(unit), prefer_rig);
            }
            if probe.right {
                self.nudge_reference(unit, prefer_rig);
            }
            if repeats > 0 {
                self.nudge_reference(unit * repeats, prefer_rig);
            }
        }
    }

    /// Hertz one pixel of the data area covers.
    ///
    /// Every grab radius is stated through this rather than in hertz. A radius
    /// in hertz is a radius in pixels that changes with the span: half a
    /// detector width is a hundred hertz, which at full span on a quadrature
    /// input is well under one pixel, so a marker could only be grabbed by
    /// landing exactly on its centre.
    fn hz_per_pixel(&self) -> f32 {
        let width = self
            .gui
            .custom_rect(TAG_SPECTRUM)
            .or_else(|| self.gui.custom_rect(TAG_WATERFALL))
            .map(|r| (r.w - self.gutter_left()).max(1.0))
            .unwrap_or(1.0);
        self.axis().span_hz() / width
    }

    /// Channel whose marker covers a frequency.
    ///
    /// The larger of half the channel width, which is the band the marker draws,
    /// and a fixed distance in pixels, which is what makes the marker a target a
    /// pointer can actually hit at any span.
    fn channel_at(&self, hz: f32) -> Option<u32> {
        /// Pixels either side of a marker that count as the marker.
        ///
        /// Ten is about the width of a fingertip on a pointing device and is the
        /// distance every other grab handle in this interface uses.
        const GRAB_PIXELS: f32 = 10.0;
        let slack = self.hz_per_pixel() * GRAB_PIXELS;

        let mut best: Option<(u32, f32)> = None;
        for channel in &self.channel_view {
            let radius = (channel.width_hz * 0.5).max(slack);
            let distance = (channel.hz - hz).abs();
            if distance > radius {
                continue;
            }
            if best.map(|(_, held)| distance < held).unwrap_or(true) {
                best = Some((channel.id, distance));
            }
        }
        best.map(|(id, _)| id)
    }

    /// One frame of the keying gesture.
    ///
    /// ## What the press means
    ///
    /// A press on a marker selects that channel and holds it in place, so the
    /// detailed readout, the monitor and the drag all refer to it. A press on
    /// the marker that is already selected releases it, which is the only way
    /// back: with a selection held every press moves it, so without a release
    /// there is no gesture left to open a second channel.
    ///
    /// A press elsewhere moves the selected channel there when one is selected,
    /// and opens a channel there when none is. Moving rather than opening is
    /// what an operator means once they have chosen a channel: the alternative
    /// fills the bank with detectors nobody asked for and retires them one by
    /// one twelve seconds later.
    ///
    /// ## Why the target is latched
    ///
    /// The rest of the drag moves whatever the press decided. Recomputing it per
    /// frame would hand the gesture to whichever marker the pointer crossed on
    /// the way, so a move across a crowded band would drag three channels a
    /// short distance each rather than one channel the whole way.
    fn apply_channel_gesture(&mut self, hz: f32, started: bool) {
        let _ = self.hz_per_pixel();
        if started {
            let focus = self.decode.status().cw_focus;
            match self.channel_at(hz) {
                Some(id) if id == focus => {
                    self.decode.set_focus(0);
                    self.decode.pin_channel(id, false);
                    self.channel_drag = None;
                    crate::log_debug!("app", "channel {} released", id);
                    return;
                }
                Some(id) => {
                    self.decode.set_focus(id);
                    self.decode.pin_channel(id, true);
                    self.channel_drag = Some(id);
                    crate::log_debug!("app", "channel {} selected", id);
                    return;
                }
                None if focus != 0 => {
                    self.channel_drag = Some(focus);
                }
                None => {
                    // The one path that snaps to a peak. A click on bare
                    // spectrum states a neighbourhood rather than a frequency,
                    // and the spectrum states where inside it the carrier is; a
                    // drag must not snap, or it would jump away from the pointer.
                    self.apply_manual_tune(hz);
                    self.channel_drag = Some(self.decode.status().cw_focus);
                    return;
                }
            }
        }

        let id = match self.channel_drag {
            Some(id) => id,
            None => return,
        };
        let placed = self.decode.move_channel(id, hz);

        // A single channel has nowhere else to go, so the stated tone follows it
        // and the automatic search is switched off: left running it would move
        // the channel away on the next pass.
        if !self.settings.morse.multi_channel || self.settings.morse.max_channels <= 1 {
            self.settings.morse.auto_tone = false;
            self.settings.morse.tone_hz = placed;
        }
    }

    /// Points a keying channel at a frequency the operator picked.
    fn apply_manual_tune(&mut self, hz: f32) {
        let bin_hz = self.dsp.spectrum.bin_hz();
        let detector = self.decode.status().cw_bandwidth_hz.max(bin_hz * 4.0);
        let radius = self
            .settings
            .morse
            .capture_range_hz
            .min(detector * 0.5)
            .max(bin_hz * 3.0);

        // The bank reports where the channel was placed, which is not always
        // what was asked for: a request within half a detector width of an edge
        // is moved inside the band the detector can measure.
        //
        // The frequency is handed over as it is. An earlier floor of one hertz
        // here silently discarded the sign, so every request below the tuning
        // point landed at the bottom of the span whatever the two sided bounds
        // downstream permitted.
        let placed = self.decode.tune_to(hz, bin_hz, radius, &self.settings);

        if !self.settings.morse.multi_channel || self.settings.morse.max_channels <= 1 {
            // A single channel has nowhere else to go, so automatic tracking is
            // switched off: left running it would move away on the next pass.
            self.settings.morse.auto_tone = false;
            self.settings.morse.tone_hz = placed;
        }
        // The mark tone of a shifted pair is the higher one by convention, so a
        // click is read as the mark and the shift setting is kept.
        self.settings.rtty.auto_shift = false;
        self.settings.rtty.mark_hz = placed;

        crate::log_info!(
            "app",
            "channel placed at {:.1} Hz from a click at {:.0} Hz, search radius {:.0} Hz",
            placed,
            hz,
            radius
        );
    }

    /// Moves the transceiver to a frequency picked from the band list.
    fn apply_station_tune(&mut self, rf_hz: i64) {
        let complex = self.settings.complex_signal();
        let mapping = match self.rig.mapping(&self.settings.rig, complex) {
            Some(m) => m,
            None => {
                crate::log_warn!("app", "no frequency mapping, the station cannot be reached");
                return;
            }
        };
        let target = self.tune_target_hz(&mapping);
        if self.rig.tune_to_rf(rf_hz, target, &self.settings.rig, complex) {
            crate::log_info!("app", "moving to {} Hz from the band list", rf_hz);
        } else {
            crate::log_warn!("app", "the description defines no way to set the frequency");
        }
    }

    /// Moves the transceiver so a signal in the spectrum lands on the target.
    fn apply_rig_tune(&mut self, audio_hz: f32) {
        let complex = self.settings.complex_signal();
        let mapping = match self.rig.mapping(&self.settings.rig, complex) {
            Some(m) => m,
            None => {
                // Nothing to convert the click into: no reading yet, or a real
                // input in a mode that places the carrier on the dial and the
                // audio at baseband, where one audio frequency answers to two on
                // the air.
                crate::log_warn!(
                    "app",
                    "no frequency mapping, a click cannot be turned into a dial position"
                );
                return;
            }
        };
        let target = self.tune_target_hz(&mapping);
        if self
            .rig
            .tune_audio_to(audio_hz, target, &self.settings.rig, complex)
        {
            crate::log_info!(
                "app",
                "moving {:.0} Hz to {:.0} Hz on the dial",
                audio_hz,
                target
            );
        } else {
            crate::log_warn!("app", "the description defines no way to set the frequency");
        }
    }

    /// Applies a detector width the operator dragged on the display.
    ///
    /// To the focused channel alone. Two stations on one band are rarely the same
    /// width, and a bank wide setting also rebuilt every detector on every step
    /// of the gesture, which discarded the level and timing estimates of the
    /// channels the operator was not adjusting.
    ///
    /// The stated default follows it, so the next channel opens at the width the
    /// operator last chose and the control in the panel agrees with the display.
    fn apply_bandwidth(&mut self, hz: f32) {
        let wanted = ((hz / BANDWIDTH_STEP_HZ).round() * BANDWIDTH_STEP_HZ)
            .clamp(BANDWIDTH_MIN_HZ, BANDWIDTH_MAX_HZ);

        let focus = self.decode.status().cw_focus;
        let held = self
            .decode
            .channel_width(focus)
            .unwrap_or(self.settings.morse.filter_bandwidth_hz);
        if (wanted - held).abs() < BANDWIDTH_STEP_HZ * 0.5 {
            return;
        }

        self.settings.morse.filter_bandwidth_hz = wanted;
        if focus != 0 {
            self.decode.set_channel_width(focus, wanted);
        }
    }

    /// Copies the decoded text the filter admits.
    ///
    /// Everything the filter admits rather than only what is on screen. The
    /// operator set the filter to say which traffic they wanted, and the panel
    /// height is not part of that statement.
    ///
    /// Lines are terminated the way this platform terminates them, because the
    /// destination is another application on it and half of them treat a bare
    /// line feed as no break at all.
    fn copy_decode(&mut self) {
        let needle = self.decode_view.filter.trim().to_ascii_uppercase();
        let mut out = String::with_capacity(4096);
        let mut lines = 0usize;

        for line in self.decode.log.lines() {
            if !needle.is_empty() && !line.text.to_ascii_uppercase().contains(&needle) {
                continue;
            }
            out.push_str(&line.stamp_full());
            out.push_str("  ");
            out.push_str(line.tag().trim());
            out.push_str(" Hz  ");
            out.push_str(&line.text);
            out.push_str("\r\n");
            lines += 1;
        }

        if out.is_empty() {
            crate::log_info!("app", "nothing matches the filter, the clipboard is untouched");
            return;
        }
        if self.window.set_clipboard_text(&out) {
            crate::log_info!("app", "{} lines copied", lines);
        }
    }

    fn apply_language(&mut self, code: &str) {
        self.settings.ui.language = code.to_string();
        let catalog = Catalog::load(&self.language_dir, code);
        self.gui.set_catalog(catalog);
        crate::log_info!("app", "language set to '{}'", code);
    }

    // -------------------------------------------------------------- loop

    
    /// Main loop. Returns the process exit code.
    pub fn run(&mut self) -> i32 {
        let mut last_title_update = crate::core::Instant::now();

        while self.running {
            // The event vector is moved out and back so the window can append
            // into it without the application holding a borrow of itself.
            let mut queued = std::mem::take(&mut self.events);
            queued.clear();
            let alive = self.window.pump(&mut queued);
            for ev in queued.drain(..) {
                self.handle_event(ev);
            }
            self.events = queued;
            if !alive {
                self.running = false;
                break;
            }

            let dt = self.clock.tick();
            self.time += dt;
            self.frame_dt = dt;

            // Nothing to draw while minimized; yield instead of spinning. The
            // capture thread keeps running and the queue trimming below absorbs
            // whatever piled up.
            if self.minimized {
                platform::sleep_ms(50);
                continue;
            }

            self.audio_status = match self.audio.as_ref() {
                Some(s) => s.status(),
                None => self.audio_status.clone(),
            };

            // Before anything reads the rate, so a stream brought back on this
            // frame is the one the chain is planned against rather than the one
            // that just died.
            self.sync_recovery(dt);

            // The chain follows whichever source is feeding it. A recording was
            // made at whatever rate the chain settled on then, which is not
            // necessarily the rate the device delivers now.
            let rate = self.source_rate();
            // Asked once and before either is synchronized, because both borrow
            // the shell mutably and the answer belongs to the source rather than
            // to them.
            let complex = self.signal_is_complex();
            self.dsp.sync(&self.settings, rate);
            self.decode.sync(&self.settings, rate, complex);

            // The two sided arrangement follows the source rather than the
            // settings, and is therefore applied after the synchronization that
            // took it from them. A recording states for itself whether it holds
            // a quadrature pair, and no switch can change that after the fact.
            self.dsp.spectrum.set_complex(self.signal_is_complex());

            {
                // Destructured because the call holds the engine mutably and the
                // renderer mutably, and they are disjoint members.
                let App { dsp, settings, renderer, .. } = self;
                dsp.sync_display(settings, renderer);
            }

            // The link runs its own thread and publishes into a queue, so it is
            // drained here for the same reason the capture queue is: nothing
            // moves until somebody looks.
            self.rig.sync(&self.settings.rig);
            self.rig.poll();
            self.sync_mode_link();
            self.sync_display_anchor();

            // The recorder is told what the reception is before the audio it
            // describes is drained, so a block opened this frame carries the
            // state of this frame rather than of the previous one.
            self.sync_record_meta();
            self.sync_replay(dt);

            if let Err(e) = self.pump_audio() {
                crate::log_error!("app", "processing failed: {}", e);
                self.running = false;
                break;
            }

            self.sync_band_stack();
            self.sync_auto_notch(dt);
            self.sync_callsigns(dt);
            self.sync_deviations(dt);

            let level = self.meter_level_db();
            self.dsp.update_meter(level, dt, &self.settings);
            self.sync_monitor();

            self.build_frame();

            if let Err(e) = self.fonts.flush(&mut self.renderer) {
                crate::log_error!("app", "glyph upload failed: {}", e);
                self.running = false;
                break;
            }
            if let Err(e) = self.renderer.render(&self.draw_list) {
                crate::log_error!("app", "render failed: {}", e);
                self.running = false;
                break;
            }

            // Frame pacing. With vertical synchronization on, present blocks and
            // this limiter is a safety net for drivers that ignore the mode.
            let target_fps = self.settings.ui.target_fps;
            if target_fps > 0 {
                self.clock.limit(target_fps);
            }

            if last_title_update.elapsed_secs() >= 0.5 {
                last_title_update = crate::core::Instant::now();
                let title = format!(
                    "RXScope {} - {}x{} - {:.0} fps",
                    env!("CARGO_PKG_VERSION"),
                    self.surface_size.0,
                    self.surface_size.1,
                    self.clock.fps()
                );
                self.window.set_title(&title);
            }
        }

        self.shutdown();
        0
    }

    fn handle_event(&mut self, ev: Event) {
        // The interface sees every event; it decides what it consumes.
        self.gui.on_event(&ev);

        match ev {
            Event::CloseRequested => {
                crate::log_info!("app", "close requested");
                self.running = false;
            }

            Event::Resized { width, height } => {
                if (width, height) != self.surface_size {
                    self.surface_size = (width, height);
                    self.renderer.resize(width, height);
                }
            }

            Event::Minimized(state) => {
                self.minimized = state;
            }

            Event::DpiChanged { scale } => {
                self.dpi_scale = scale;
                self.ui_scale = scale * self.settings.ui.scale;
                crate::log_info!("app", "dpi scale {:.2}, ui scale {:.2}", scale, self.ui_scale);
            }

            Event::ModalResize(active) => {
                self.in_modal_resize = active;
            }

            Event::Key { key, pressed, .. } => {
                // Global shortcuts stay quiet while a text field has focus.
                if pressed && !self.gui.wants_keyboard() && !self.gui.popup_open() {
                    match key {
                        Key::F(1) => {
                            self.settings.ui.show_debug_overlay =
                                !self.settings.ui.show_debug_overlay;
                        }
                        Key::F(2) => {
                            self.settings.ui.vsync = !self.settings.ui.vsync;
                            self.renderer.set_vsync(self.settings.ui.vsync);
                        }
                        Key::F(3) => {
                            self.settings.ui.show_settings_panel =
                                !self.settings.ui.show_settings_panel;
                        }
                        Key::F(5) => {
                            self.start_audio();
                        }
                        Key::F(6) => {
                            self.focus_next_channel();
                        }
                        Key::Space if self.replaying() && !self.gui.wants_activation() => {
                            // The transport of a replay is what the space bar
                            // means everywhere, and it is the one control an
                            // operator reaches for without looking.
                            if let Some(r) = self.replay.as_ref() {
                                let playing = r.status().playing;
                                r.set_playing(!playing);
                            }
                        }
                        // The tab strip is reachable from the keyboard because
                        // switching view is the one action taken while both
                        // hands are on the receiver rather than the pointer.
                        Key::Digit(d) if (1..=6).contains(&d) => {
                            self.tab = (d - 1) as usize;
                            self.settings.ui.show_settings_panel = true;
                        }
                        _ => {}
                    }
                }
            }

            _ => {}
        }
    }

    /// Moves the detailed readout to the next channel in frequency order.
    fn focus_next_channel(&mut self) {
        if self.channel_view.is_empty() {
            return;
        }
        let current = self.decode.status().cw_focus;
        let at = self.channel_view.iter().position(|c| c.id == current);
        let next = match at {
            Some(i) => (i + 1) % self.channel_view.len(),
            None => 0,
        };
        self.decode.set_focus(self.channel_view[next].id);
    }

    /// Largest side panel width the current window allows, in logical units.
    fn side_panel_max(&self) -> f32 {
        let logical = self.surface_size.0 as f32 / self.ui_scale.max(0.1);
        (logical * SIDE_PANEL_MAX_FRACTION)
            .min(SIDE_PANEL_MAX)
            .max(SIDE_PANEL_MIN)
    }

        fn build_frame(&mut self) {
        let (w, h) = (self.surface_size.0 as f32, self.surface_size.1 as f32);
        let white = self.renderer.white_texture();
        self.draw_list.begin(w, h, white);

        // The channel view is refreshed once, before anything reads it: both the
        // interface declaration and the marker drawing use the same snapshot, so
        // a channel cannot appear in one and not in the other.
        self.decode.channels(&mut self.channel_view);
        self.refresh_spot_rows();

        // Interface scale follows the operator setting, which the panel can
        // change in the same frame; recomputing here keeps them in step. The
        // style is pushed once per frame rather than cached, so a control in the
        // appearance section takes effect on the frame it was moved in.
        self.ui_scale = self.dpi_scale * self.settings.ui.scale;
        self.accent = Color::hex(self.settings.ui.accent_rgb);
        self.gui.theme.apply(self.accent, &self.settings.appearance);
        
        let scale = self.ui_scale;
        let ui_px = (self.settings.ui.font_size_pt * PT_TO_PX * scale).round().max(8.0);
        let mono_px = (self.settings.ui.decode_font_size_pt * PT_TO_PX * scale).round().max(8.0);
        let time = self.time;

        // The stored width is clamped every frame rather than only on a drag: a
        // window that shrank, or a display factor that grew, can put a value
        // written earlier outside what the current geometry allows.
        //
        // The floor is the width the current wording needs, measured by the
        // widget system on the previous frame. The panel grows to it rather than
        // cutting the labels: a translation that runs half again as long as the
        // reference otherwise loses the end of every caption, which is the part
        // that names the control. Bounded by the ceiling, so a wording that
        // cannot fit at all leaves the panel usable instead of filling the
        // window.
        let side_max = self.side_panel_max();
        let side_min = SIDE_PANEL_MIN.max(self.gui.panel_demand().min(side_max));
        self.settings.ui.side_panel_width =
            self.settings.ui.side_panel_width.clamp(side_min, side_max);

        // The view is clamped every frame rather than only on a gesture: the
        // magnification slider writes the value directly, and a rate change
        // moves what a fraction of the span refers to.
        self.settings.waterfall.zoom = self.settings.waterfall.zoom.clamp(1.0, ZOOM_MAX);
        let half = 0.5 / self.settings.waterfall.zoom;
        self.settings.waterfall.view_centre =
            self.settings.waterfall.view_centre.clamp(half, 1.0 - half);

        // The tuning point is clamped every frame for the same reason, and a
        // real input has no tuning point at all, which the empty range below
        // expresses by forcing it to nought.
        {
            let (lo, hi) = self.tune_bounds();
            self.settings.receiver.tune_hz = self.settings.receiver.tune_hz.clamp(lo, hi);
        }

        self.gui.starts_frame(Rect::new(0.0, 0.0, w, h), scale, ui_px, mono_px, time);

        // The column ceilings are shares of the row, so the row width has to be
        // known before anything is declared. Stated here rather than derived
        // inside the widget system, because the panel width is an operator
        // decision the widget system does not own.
        self.gui.set_panel_width(self.settings.ui.side_panel_width);

        // The readout is drawn over everything, so it is probed before the tree
        // exists. Doing it afterwards would let a widget underneath act on the
        // same press.
        self.probe_readout();

        let stats = self.renderer.stats();
        // The rate the path settled on rather than the one that was asked for.
        // The reduction divides it and the resampler clamps it, so the two differ
        // and only the first describes what the decoders are measuring against.
        let rate = self.source_rate();
        let queue_fill = match self.audio.as_ref() {
            Some(s) => s.available() as f32 / s.capacity().max(1) as f32,
            None => 0.0,
        };
        let meter_text = self.dsp.meter.text(&self.settings.meter);
        let decoder_status = self.decode.status();
        let nyquist_hz = self.dsp.spectrum.nyquist_hz();
        let loopback = self.selected_is_loopback();
        let default_device = self.settings.audio.device_id.is_empty();
        let receiver_listen_hz = self.settings.receiver.listen_hz();
        let receiver_reference_hz = self.receiver_reference_hz();
        let window_maximized = self.window.is_maximized();

        // The source decides three things the panel has to state consistently,
        // so it is asked once here rather than at each call site.
        let complex_signal = self.signal_is_complex();
        let replay_active = self.replaying();
        let replay_status = self.replay_status.clone();
        let replay_block_seconds = self
            .replay
            .as_ref()
            .map(|r| r.block_seconds())
            .unwrap_or(0.0);
        let recorder_status = self.recorder_status.clone();

        // The readout is taken before the declaration, because the areas it
        // refers to are the ones the previous frame laid out. The gutter is
        // removed here so a pointer resting in the label strip does not report a
        // frequency it is not over.
        let display_axis = self.axis();
        self.cursor_fraction = self
            .gui
            .custom_hover(TAG_SPECTRUM)
            .or_else(|| self.gui.custom_hover(TAG_WATERFALL))
            .map(|(x, _)| self.axis_fraction(x));
        let cursor_hz = if self.settings.waterfall.show_cursor_readout {
            self.cursor_fraction.map(|t| display_axis.audio_of_fraction(t))
        } else {
            None
        };

        // Level under the pointer, from the same bins the trace is drawn from.
        // Already computed and sitting beside the frequency, and it answers the
        // one question the palette can otherwise only be squinted at for.
        let cursor_db = cursor_hz.and_then(|hz| {
            let bin_hz = self.dsp.spectrum.bin_hz();
            let bins = self.dsp.spectrum.bins();
            if bin_hz <= 0.0 || bins.is_empty() {
                return None;
            }
            let index = ((hz - self.dsp.spectrum.low_hz()) / bin_hz).round();
            if index < 0.0 {
                return None;
            }
            bins.get(index as usize).copied()
        });

        let view_span_hz = display_axis.span_hz();
        let view_full = display_axis.is_full();

        // The difference is computed here rather than in the panel, because both
        // ends of it are audio frequencies this frame produced and the panel has
        // neither the axis nor the pointer.
        let reference_hz = self.reference_hz;
        let reference_delta_hz = match (cursor_hz, reference_hz) {
            (Some(cursor), Some(reference)) => Some(cursor - reference),
            _ => None,
        };

        // Reported so a display that looks wrong can be told apart from a signal
        // that is. Both were asked about in the same breath once.
        let mirrored = display_axis.mirrored();
        let working_hz = self.working_reference_hz();

        let language_index = self
            .languages
            .iter()
            .position(|l| l.as_str() == self.settings.ui.language.as_str())
            .unwrap_or(0);

        // Named here rather than by the control library, see the note beside the
        // function: the two keyed modes are bound to the sideband entries the
        // other way round, so a label taken from the sideband reports each of
        // them as the other.
        self.rig_mode = self.rig.mode().map(crate::rig::mode_label).unwrap_or("");

        // Visible stations, as indices into the catalogue. Indices rather than
        // references because the drawing pass borrows the catalogue immutably
        // while the shell is held mutably, and a reference would tie the two
        // borrows together for no benefit.
        //
        // Filled once, from the axis, so the markers and the list cannot
        // disagree about which entries are in view.
        self.visible_stations.clear();
        self.station_rows.clear();
        self.rig_segment.clear();
        let mut rig_band: Option<&'static str> = None;

        if self.settings.waterfall.show_stations {
            if let Some((lo, hi)) = display_axis.rf_range() {
                for (index, station) in self.stations.all().iter().enumerate() {
                    if station.hz >= lo && station.hz <= hi {
                        self.visible_stations.push(index);
                    }
                }
            }
        }

        if let Some(m) = display_axis.mapping().copied() {
            if let Some(segment) = crate::stations::segment_of(m.dial_hz) {
                rig_band = Some(segment.name);
                self.rig_segment = format!(
                    "{}  {:.0} - {:.0} kHz",
                    segment.usage,
                    segment.low_hz as f64 / 1000.0,
                    segment.high_hz as f64 / 1000.0
                );
            }

            // The passband, on the air. What separates a station being worked
            // from one merely in view.
            let p0 = m.rf_of(self.settings.receiver.filter_low_hz);
            let p1 = m.rf_of(self.settings.receiver.filter_high_hz);
            let (pass_lo, pass_hi) = if p0 < p1 { (p0, p1) } else { (p1, p0) };

            for &index in &self.visible_stations {
                if let Some(station) = self.stations.all().get(index) {
                    self.station_rows.push(panel::StationRow {
                        hz: station.hz,
                        // Grouped the way the readout groups it, to two decimal
                        // places of a kilohertz: finer than a marker can be
                        // aimed at and coarse enough to read at a glance.
                        frequency: Readout::new(station.hz, 2).text(),
                        text: station.describe(),
                        inside: station.hz >= pass_lo && station.hz <= pass_hi,
                    });
                }
            }
        }

        // Monitor and link state are read out here rather than inside the block
        // below. Both readers take the application by reference, and the block
        // holds several of its fields mutably, so a call made from inside it
        // would overlap with borrows already in force.
        let feedback = self.monitor_feedback();
        // Read out here for the same reason the monitor state is: the block below
        // holds several fields of this structure mutably, and a read taken from
        // inside it would overlap with borrows already in force.
        let audio_recoveries = self.audio_recoveries;
        let rig_recoveries = self.rig_recoveries;
        let monitor_status = self.monitor_status.clone();
        let listen_band = self.listen_band;
        let rig_status = self.rig.status();
        let rig_frequency = self.rig.display_hz();
        let rig_mode = self.rig_mode;
        // Refused during replay, which disables the band list, the readout and
        // the click gesture in one place rather than in three. The frequency of
        // a recording is fixed, and a dial moved from here would put the picture
        // and the markers out of step with each other.
        let rig_can_tune = self.rig.can_tune() && !replay_active;

        let mut tab = self.tab;
        let mut editing_tab = self.editing_tab;
        let mut add_section = self.add_section;
        let mut sel = self.sel;
        let mut cmd = UiCommands::default();
        let vsync_before = self.settings.ui.vsync;
        let audio_running = self.audio.is_some() && self.audio_status.running;

        {
            let App {
                gui,
                fonts,
                settings,
                gpu_name,
                font_name,
                devices,
                monitor_devices,
                audio_status,
                channel_view,
                languages,
                clock,
                dsp,
                decode,
                rig_profile_labels,
                rig_port_labels,
                rig_issues,
                station_rows,
                rig_segment,
                spot_rows,
                book,
                band_list,
                entry,
                segment_rows,
                segments_total,
                deviations,
                decode_view,
                ..
            } = self;
            let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
            let monitor_names: Vec<&str> =
                monitor_devices.iter().map(|d| d.name.as_str()).collect();
            let language_names: Vec<&str> = languages.iter().map(|s| s.as_str()).collect();
            let profile_names: Vec<&str> =
                rig_profile_labels.iter().map(|s| s.as_str()).collect();
            let port_names: Vec<&str> = rig_port_labels.iter().map(|s| s.as_str()).collect();
            let band_names: Vec<&str> = band_list.iter().map(|b| b.name).collect();

            let status = StatusInfo {
                fps: clock.fps(),
                worst_ms: clock.worst_frame_ms(),
                draw_calls: stats.draw_calls,
                uploads: stats.uploads,
                clears: stats.clears,
                gpu_ms: stats.gpu_ms,
                gpu_worst_ms: stats.gpu_worst_ms,
                glyphs: fonts.cached_glyphs(),
                atlas: fonts.atlas_used(),
                gpu: gpu_name.as_str(),
                font: font_name.as_str(),
                dpi: scale / settings.ui.scale.max(0.1),
                monitor: monitor_status,
                monitor_devices: monitor_names.as_slice(),
                listen_band,
                feedback,
                window_maximized,
                audio: audio_status,
                decoder: decoder_status,
                channels: channel_view.as_slice(),
                languages: language_names.as_slice(),
                language_index,
                loopback,
                default_device,
                queue_fill,
                lines: dsp.waterfall.lines(),
                waterfall_columns: dsp.waterfall.width(),
                source_rate: rate,
                blanked: dsp.blanker.events(),
                worker_threads: settings.dsp.resolved_worker_threads(),
                audio_recoveries,
                rig_recoveries,
                log_lines: decode.log.len(),
                decoded_chars: decode.log.total_chars(),
                noise_floor_db: decode.noise_floor_db(),
                meter_text,
                nyquist_hz,
                bin_hz: dsp.spectrum.bin_hz(),
                line_rate: dsp.line_rate(),
                cursor_hz,
                cursor_db,
                view_span_hz,
                view_full,
                mirrored,
                working_hz,
                side_panel_max: side_max,
                side_panel_min: side_min,
                height: h,
                audio_running,
                recorder: recorder_status,
                replay: replay_status,
                replay_active,
                replay_block_seconds,
                segments: segment_rows.as_slice(),
                segments_total: segments_total.as_str(),
                deviations: deviations.as_slice(),
                rig: rig_status,
                rig_profiles: profile_names.as_slice(),
                rig_issues: rig_issues.as_slice(),
                rig_ports: port_names.as_slice(),
                rig_frequency,
                rig_mode,
                rig_can_tune,
                rig_band,
                rig_segment: rig_segment.as_str(),
                stations: station_rows.as_slice(),
                spots: spot_rows.as_slice(),
                prefix_count: book.prefix_count(),
                country_count: book.country_count(),
                bands: band_names.as_slice(),
                reference_hz,
                reference_delta_hz,
                device_channels: audio_status.channels,
                complex_signal,
                receiver_listen_hz,
                receiver_reference_hz,
            };

            let mut frame = Frame::new(gui, fonts);
            panel::build(
                &mut frame,
                settings,
                &status,
                &names,
                &mut sel,
                decode_view,
                entry,
                &mut tab,
                &mut editing_tab,
                &mut add_section,
                &mut cmd,
            );
            frame.ui.ends_frame();
        }

        self.tab = tab;
        self.editing_tab = editing_tab;
        self.add_section = add_section;
        self.sel = sel;
        if self.settings.ui.vsync != vsync_before {
            self.renderer.set_vsync(self.settings.ui.vsync);
        }
        self.window.set_cursor(self.gui.cursor());

        // The scrub bar is a control the application draws, so its gestures are
        // read here rather than in the declaration. A drag as well as a click:
        // scrubbing is a continuous action, and a click at a time would make it
        // a sequence of jumps through a recording rather than a sweep across it.
        // Decoded text. The wheel walks back through the history and a click
        // brings the receiver to the frequency the line came from, which is the
        // shortest path from reading a call sign to hearing the station again.
        //
        // The control key multiplies the step, because a skimmer produces a few
        // thousand lines an hour and a wheel that moved one at a time would need
        // a hundred turns to reach anything.
        if let Some((_, notches)) = self.gui.custom_wheel(TAG_DECODE) {
            let step = if self.gui.mods().ctrl { 10 } else { 1 };
            let by = notches.round() as i64 * step;
            let limit = self.decode.log.len().saturating_sub(1) as i64;
            let wanted = self.decode_view.scroll as i64 + by;
            self.decode_view.scroll = wanted.clamp(0, limit.max(0)) as usize;
        }
        if let Some((_, y)) = self.gui.custom_click(TAG_DECODE) {
            if let Some(rect) = self.gui.custom_rect(TAG_DECODE) {
                let at = rect.y + y * rect.h;
                // The frequency is tested rather than assumed: a status note
                // carries none, and tuning to nought would move the receiver to
                // the edge of the span for no reason the operator could see.
                if let Some(hit) = self.decode_hits.iter().find(|h| at >= h.top && at < h.bottom) {
                    if hit.hz > 0.0 {
                        cmd.decode_tune_hz = Some(hit.hz);
                    }
                }
            }
        }

        if let Some((t, _)) = self.gui.custom_click(TAG_TIMELINE) {
            cmd.replay_seek = Some(t);
        }
        if let Some(drag) = self
            .gui
            .custom_drag(TAG_TIMELINE, crate::platform::MouseButton::Left)
        {
            cmd.replay_seek = Some(drag.x);
        }

        // Four passes, in this order and for one reason. The reserved areas are
        // opaque and are drawn by the application rather than by the widget
        // system, so they have to land between the tree that reserved them and
        // the layers that must cover everything. The readout sits above them and
        // below an open list: a list is modal, and a modal element that cannot
        // be seen still owns the pointer.
        {
            let App { gui, fonts, draw_list, .. } = self;
            gui.draw_tree(fonts, draw_list);
        }

        self.draw_reserved_areas(mono_px);
        self.draw_readout(mono_px);

        {
            let App { gui, fonts, draw_list, .. } = self;
            gui.draw_top(fonts, draw_list);
        }

                // Window edge. Drawn last and over everything, because it is the edge: a
        // popup or a readout reaching past it would read as a window without
        // one. Dropped when maximized, where the window meets the screen and a
        // line there reads as a gap rather than as a boundary.
        //
        // The colour states focus. That is the only signal left after the system
        // caption is gone, and it is the one an operator running two receivers
        // needs. Thickness is not a setting: one pixel and two are the only
        // sane values, and the second is a border rather than an edge.
        if self.settings.appearance.custom_frame && !window_maximized {
            let thickness = self.gui.line(1.0);
            let color = if self.gui.window_focused() {
                self.accent
            } else {
                self.gui.theme.border_strong
            };
            self.draw_list
                .stroke_rect(Rect::new(0.0, 0.0, w, h), thickness, color);
        }

        self.draw_list.end();

        // Commands run after the declaration so the settings the layout read are
        // not mutated underneath it.
        if cmd.clear_log {
            self.decode.log.clear();
            self.decode_view.scroll = 0;
        }
        if cmd.decode_newest {
            self.decode_view.scroll = 0;
        }
        if cmd.copy_decode {
            self.copy_decode();
        }
        if let Some(hz) = cmd.decode_tune_hz {
            // The same dispatch a click on the display takes, because the
            // gesture means the same thing: bring the receiver to this
            // frequency. Which of the two things moves follows from the mode
            // rather than from where the click landed.
            if self.settings.sdr_mode() {
                self.apply_receiver_tune(hz);
            } else {
                self.apply_manual_tune(hz);
            }
        }
        if cmd.rescan_devices {
            self.rescan_devices();
        }
        if cmd.select_device {
            self.apply_device_selection();
            cmd.start_audio = true;
        }

        if cmd.select_monitor {
            self.apply_monitor_selection();
        }
        if cmd.restart_monitor {
            self.start_monitor();
        }

        if cmd.rescan_rig {
            self.rig.rescan_profiles(&self.settings.rig);
            self.rig.rescan_ports();
            self.refresh_rig_lists();
        }
        if cmd.select_rig_profile {
            self.apply_rig_profile();
        }
        if cmd.select_rig_port {
            self.apply_rig_port();
        }
        if cmd.stop_rig {
            self.rig_wanted = false;
            self.rig.stop();
        }
        if cmd.start_rig {
            self.rig_wanted = true;
            self.rig_backoff = RECOVER_FIRST_S;
            self.rig_retry = 0.0;
            self.rig.start(&self.settings.rig);
        }

        if let Some(index) = cmd.reset_tab {
            self.settings.panel.reset_tab(index);
        }

        if cmd.stop_audio {
            self.stop_audio();
        }
        if cmd.start_audio {
            self.start_audio();
        }
        if let Some(id) = cmd.focus_channel {
            self.decode.set_focus(id);
        }
        if let Some(id) = cmd.drop_channel {
            self.decode.drop_channel(id);
        }
        if let Some(code) = cmd.language {
            self.apply_language(&code);
        }
        if let Some(level) = cmd.log_level {
            crate::core::log::set_level(level);
        }

        // Recording. Stopping precedes starting so a restart is one gesture
        // rather than two, and the segment directory is reread by both.
        if cmd.stop_record {
            self.stop_recording();
        }
        if cmd.start_record {
            self.start_recording();
        }
        if cmd.rescan_segments {
            self.rescan_segments();
        }
        if let Some(index) = cmd.export_segment {
            self.export_segment(index);
        }

        // Replay. Closing precedes opening for the same reason, and both are
        // applied before the transport: a transport command names a stream, and
        // the one it names is whichever is open after these two.
        if cmd.close_replay {
            self.close_replay();
        }
        if cmd.open_replay {
            self.open_replay();
        }
        if let Some(stream) = self.replay.as_ref() {
            if let Some(play) = cmd.replay_play {
                stream.set_playing(play);
            }
            // The rate and the two switches are applied before any movement,
            // because a seek empties the queue and whatever refills it should
            // already be produced under the settings the operator just chose.
            if let Some(speed) = cmd.replay_speed {
                stream.set_speed(speed);
            }
            if let Some(on) = cmd.replay_follow {
                stream.set_follow(on);
            }
            if let Some(on) = cmd.replay_loop {
                stream.set_looping(on);
            }
            if let Some(t) = cmd.replay_seek {
                stream.seek_fraction(t);
            }
            if let Some(blocks) = cmd.replay_step {
                stream.step(blocks);
            }
            if cmd.replay_live {
                stream.seek_live();
            }
        }

        // Window chrome. The frame switch goes first: the other three act on the
        // window as it is, and changing the frame recomputes its geometry.
        if cmd.frame_changed {
            self.window.set_custom_frame(self.settings.appearance.custom_frame);
        }
        if cmd.window_close {
            self.window.close();
        } else if cmd.window_minimize {
            self.window.minimize();
        } else if cmd.window_toggle_max {
            self.window.toggle_maximize();
        } else if cmd.window_drag {
            // The system move loop takes the capture and consumes the release,
            // so the interface would otherwise carry a held button into the next
            // frame and a widget would act on a press that has already ended.
            self.window.begin_move();
            self.gui.release_pointer();
        }

        // The view is moved before anything is turned into a frequency, so a
        // click and a wheel arriving in the same frame agree about which axis
        // they mean.
        if let Some((at, notches)) = cmd.zoom_at {
            self.apply_zoom(self.axis_fraction(at), notches);
        }
        if let Some((at, started)) = cmd.pan_drag {
            self.apply_pan(self.axis_fraction(at), started);
        }
        if cmd.reset_zoom {
            self.settings.waterfall.zoom = 1.0;
            self.settings.waterfall.view_centre = 0.5;
        }
        if cmd.store_view {
            self.store_view();
        }
        if cmd.recall_view {
            self.recall_view();
        }
        if cmd.clear_reference {
            self.reference_hz = None;
        }
        if let Some(text) = cmd.tune_typed.take() {
            self.apply_typed_frequency(&text);
        }
        if let Some(index) = cmd.tune_band {
            self.go_to_band(index);
        }
        if cmd.mark_station {
            self.mark_station();
        }
        if let Some(hz) = cmd.tune_rf {
            self.apply_station_tune(hz);
        }
        if cmd.clear_spots {
            self.book.clear();
            crate::log_info!("app", "spot list cleared");
        }
        if cmd.reload_callsigns {
            self.book.reload(&self.settings.callsign);
        }
        if let Some(index) = cmd.tune_spot {
            if let Some(&rf) = self.spot_targets.get(index) {
                if rf != 0 {
                    self.apply_station_tune(rf);
                }
            }
        }

        // Positions arrive as fractions of the reserved area and are turned into
        // frequencies here, which is the only place the axis exists. Each is
        // applied exactly once.
        if let Some(t) = cmd.mark_reference {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(t));
            self.reference_hz = Some(hz);
            crate::log_debug!("app", "measurement reference at {:.0} Hz", hz);
        }
        if let Some((t, started)) = cmd.channel_drag {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(t));
            self.apply_channel_gesture(hz, started);
        }
        if let Some(t) = cmd.rig_tune_fraction {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(t));
            self.apply_rig_tune(hz);
        }
        if let Some(t) = cmd.receiver_tune_fraction {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(t));
            self.apply_receiver_tune(hz);
        }
        if let Some(drag) = cmd.filter_drag {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(drag.fraction));
            self.apply_filter_drag(hz, drag.started, drag.whole_band);
        }
        if let Some(t) = cmd.bandwidth_fraction {
            let hz = display_axis.audio_of_fraction(self.axis_fraction(t));
            // The focused channel is the one being adjusted, so its own centre
            // is what the pointer distance is measured from. Asked of the bank
            // rather than tested against nought: on a two sided spectrum a
            // channel below the tuning point has a negative centre and is not
            // an absent one.
            let centre = if self.decode.status().cw_channels > 0 {
                self.decode.status().cw_tone_hz
            } else {
                self.settings.morse.tone_hz
            };
            self.apply_bandwidth((hz - centre).abs() * 2.0);
        }
        if let Some(hz) = cmd.channel_width_hz {
            let focus = self.decode.status().cw_focus;
            if focus != 0 {
                self.decode.set_channel_width(focus, hz);
            }
        }
    }

    // --------------------------------------------------------- rendering

        // --------------------------------------------------------- rendering

    /// Fills the areas the layout reserved for the application.
    ///
    /// Every colour and every switch the drawing needs is copied out before the
    /// structure is destructured. The alternative is reading the theme from
    /// inside the block, which cannot be done: the block holds the draw list
    /// mutably and the theme lives beside it.
    fn draw_reserved_areas(&mut self, mono_px: f32) {
        let spectrum_full = self.gui.custom_rect(TAG_SPECTRUM);
        let waterfall_full = self.gui.custom_rect(TAG_WATERFALL);
        let decode_rect = self.gui.custom_rect(TAG_DECODE);
        let meter_rect = self.gui.custom_rect(TAG_METER);
        let timeline_rect = self.gui.custom_rect(TAG_TIMELINE);

        let axis = self.axis();
        let scale = self.ui_scale;

        let look = DataLook {
            scale,
            // Smaller than the interface font. An axis is read by position and
            // the number only confirms it, so a label at the same size as a
            // control caption competes with the data for attention.
            label_px: (mono_px * 0.72).round().max(8.0),
            accent: self.accent,
            text: self.gui.theme.text,
            dim: self.gui.theme.text_dim,
            faint: self.gui.theme.text_faint,
            grid_major: self.gui.theme.grid_major,
            grid_minor: self.gui.theme.grid_minor,
            background: self.gui.theme.data_background,
            held: self.gui.theme.text_disabled,
            monitor: self.gui.theme.monitor,
            station: Color::hex(0xE8C84A),
            // Light enough to read the data through. At any useful strength the
            // shade covers half the width on a real input, where the passband
            // ends at three kilohertz and the display runs to the Nyquist
            // frequency, and a dark half reads as a fault rather than as a
            // setting. The boundary carries the meaning instead.
            shade: self.gui.theme.background.with_alpha(0.22),
            shade_edge: self.gui.theme.border_strong,
            edge_grab: self.gui.m(self.gui.theme.edge_grab),
            call: Color::hex(0x6FD08C),
            notch: Color::hex(0xC08080),
            reference: Color::hex(0xF0F0F0),
            call_highlight: self.settings.callsign.lookup_enabled
                && self.settings.callsign.highlight_in_text,
            call_min: self.settings.callsign.min_callsign_length as usize,
            major_every: self.settings.appearance.grid_major_every,
            labels: self.settings.waterfall.show_labels && self.settings.waterfall.show_grid,
            trace_fill: self.settings.appearance.trace_fill,
            trace_fill_alpha: self.settings.appearance.trace_fill_alpha,
            trace_thickness: self.settings.appearance.trace_thickness,
            crosshair: self.settings.appearance.crosshair,
        };

        // The two colours the meter needs and the data area does not. Copied
        // separately rather than added to the structure above, so the structure
        // stays a description of the data area alone.
        let meter_well = self.gui.theme.surface_dark;
        let meter_border = self.gui.theme.border;

        let gutter_left = self.gutter_left();
        let gutter_bottom = self.gutter_bottom();
        let cursor = self.cursor_fraction;

        let show_grid = self.settings.waterfall.show_grid;
        let show_level_grid = self.settings.waterfall.show_level_grid;
        let peak_hold = self.settings.waterfall.peak_hold;
        let show_held = self.settings.waterfall.show_held_trace;
        let min_db = self.settings.waterfall.min_db;
        let max_db = self.settings.waterfall.max_db;
        let mark_decoders = self.settings.waterfall.mark_decoders;
        let sdr = self.settings.sdr_mode();
        let cw_enabled = self.settings.morse.enabled && !sdr;
        let decoder = self.decode.status();
        let mode = decoder.mode;
        let estimate = self.decode.estimate();
        // The width is per channel now, so the marker reads it from the channel
        // rather than from one figure describing the bank.
        let focus = decoder.cw_focus;
        let listen = self.listen_band;
        let replaying = self.replaying();

        // The band the receiver filter passes, and the point the readout names.
        // Two marks rather than one, because for a sideband mode they are
        // different frequencies and only the second is the one an operator logs.
        //
        // Neither is drawn during replay. The receiver chain is not in the path
        // then, so a band drawn from its settings would claim a filter that is
        // not filtering anything.
        let (receiver_band, receiver_vfo) = if sdr && !replaying {
            (
                Some(self.settings.receiver.absolute_band()),
                Some(self.receiver_reference_hz()),
            )
        } else {
            (None, None)
        };

        // Frequencies the notch removes, in the display frame. Two on a
        // quadrature input: the notch runs after the detector and the detector
        // output is real, so one setting removes a component on each side of the
        // reference. Drawing only the stated one would leave the operator
        // hunting for why a signal on the other side vanished.
        let mut notch_marks: [f32; 2] = [0.0, 0.0];
        let mut notch_count = 0usize;
        let mut notch_width = 0.0f32;
        if sdr && !replaying && self.settings.receiver.notch_enabled {
            let complex = self.settings.complex_signal();
            let base = self.settings.receiver.tune_hz
                - self.settings.receiver.detector_offset_hz(complex);
            let n = self.settings.receiver.notch_hz;
            notch_width = self.settings.receiver.notch_width_hz;
            notch_marks[0] = base + n;
            notch_count = 1;
            if complex {
                notch_marks[1] = base - n;
                notch_count = 2;
            }
        }

        let reference = self.reference_hz;
        let time_axis = self.settings.waterfall.time_axis;
        let waterfall_rows = self.dsp.waterfall.rows();
        // The rate the lines are actually produced at rather than the one that
        // was asked for. The hop is clamped by the transform size, so the two
        // differ at the extremes and the axis has to describe what happened.
        let lines_per_second = {
            let hop = self.dsp.spectrum.hop().max(1);
            self.dsp.spectrum.sample_rate() as f32 / hop as f32
        };
        let show_average =
            self.settings.waterfall.show_average_trace && self.settings.waterfall.spectrum_visible;

        // The decoder passband bounds where a channel may be opened, which is a
        // skimmer question. Held under the same switch as every other overlay,
        // so one control removes everything the application draws over the data.
        let passband = if sdr || !mark_decoders {
            None
        } else {
            Some((
                self.settings.dsp.passband_low_hz,
                self.settings.dsp.passband_high_hz,
            ))
        };

        let show_stations = self.settings.waterfall.show_stations;
        let station_indices = self.visible_stations.clone();

        let App {
            dsp,
            decode,
            fonts,
            draw_list,
            settings,
            channel_view,
            stations,
            replay_timeline,
            replay_status,
            decode_view,
            decode_hits,
            ..
        } = self;
        let channels: &[ChannelInfo] = channel_view;

        if let Some(full) = waterfall_full {
            draw_list.push_clip(full);
            // The gutters carry the same background as the data, so the two read
            // as one instrument rather than as a picture inside a frame.
            draw_list.fill_rect(full, look.background);

            let data = Rect::from_min_max(
                full.x + gutter_left,
                full.y,
                full.right(),
                full.bottom() - gutter_bottom,
            );
            if !data.is_empty() {
                let (a0, a1) = axis.view_fraction();
                dsp.waterfall.draw(draw_list, data, a0, a1, axis.mirrored());
                Self::draw_passband(draw_list, data, &axis, passband, &look);
                if show_grid {
                    Self::draw_frequency_grid(
                        draw_list, fonts, data, &axis, &look, gutter_bottom,
                    );
                }
                if show_stations {
                    Self::draw_stations(
                        draw_list, fonts, data, &axis, stations, &station_indices, &look, false,
                    );
                }
                if mark_decoders {
                    if cw_enabled {
                        Self::draw_channel_markers(
                            draw_list, fonts, data, &axis, channels, focus, &look, false,
                        );
                    }
                    Self::draw_fsk_markers(draw_list, data, &axis, mode, &estimate, &look);
                    Self::draw_listen_band(draw_list, data, &axis, listen, &look);
                    Self::draw_receiver_band(
                        draw_list, data, &axis, receiver_band, receiver_vfo, &look,
                    );
                }
                if notch_count > 0 {
                    Self::draw_notch(
                        draw_list,
                        data,
                        &axis,
                        &notch_marks[..notch_count],
                        notch_width,
                        &look,
                    );
                }
                Self::draw_reference(draw_list, data, &axis, reference, &look);
                if look.crosshair {
                    Self::draw_crosshair(draw_list, data, cursor, &look);
                }
                // Last, and into the gutter rather than the data, so nothing
                // above has to be aware of it.
                if time_axis {
                    Self::draw_time_axis(
                        draw_list,
                        fonts,
                        data,
                        gutter_left,
                        waterfall_rows,
                        lines_per_second,
                        &look,
                    );
                }
            }
            draw_list.pop_clip();
        }

        if let Some(full) = spectrum_full {
            draw_list.push_clip(full);
            draw_list.fill_rect(full, look.background);

            let data = Rect::from_min_max(full.x + gutter_left, full.y, full.right(), full.bottom());
            if !data.is_empty() {
                // Both grids go under everything else. A trace crossing a grid
                // line has to be the thing that is read, and a line drawn over
                // it would break the trace into segments at exactly the levels
                // an operator is comparing against.
                if show_grid {
                    Self::draw_frequency_grid(draw_list, fonts, data, &axis, &look, 0.0);
                }
                if show_level_grid {
                    Self::draw_level_grid(
                        draw_list, fonts, data, min_db, max_db, &look, gutter_left,
                    );
                }

                // The tracker trace is what the channel allocator actually
                // searches: a decayed peak hold, which keeps an intermittent
                // keyed carrier at its real level instead of at whatever the
                // current frame caught.
                if show_held && !sdr {
                    Self::draw_trace(
                        draw_list,
                        data,
                        decode.held_spectrum(),
                        &axis,
                        min_db,
                        max_db,
                        &look,
                        look.held,
                        false,
                    );
                }
                // Under everything else, because it is the weakest statement:
                // what has been there for several seconds, which on a busy band
                // is the noise floor plus whatever nobody has switched off.
                if show_average {
                    Self::draw_trace(
                        draw_list,
                        data,
                        dsp.spectrum.average(),
                        &axis,
                        min_db,
                        max_db,
                        &look,
                        look.faint,
                        false,
                    );
                }
                if peak_hold {
                    Self::draw_trace(
                        draw_list,
                        data,
                        dsp.spectrum.peaks(),
                        &axis,
                        min_db,
                        max_db,
                        &look,
                        look.dim,
                        false,
                    );
                }
                Self::draw_trace(
                    draw_list,
                    data,
                    dsp.spectrum.bins(),
                    &axis,
                    min_db,
                    max_db,
                    &look,
                    look.accent,
                    look.trace_fill,
                );

                Self::draw_passband(draw_list, data, &axis, passband, &look);
                if show_stations {
                    Self::draw_stations(
                        draw_list, fonts, data, &axis, stations, &station_indices, &look,
                        look.labels,
                    );
                }
                if mark_decoders {
                    if cw_enabled {
                        Self::draw_channel_markers(
                            draw_list, fonts, data, &axis, channels, focus, &look, look.labels,
                        );
                    }
                    Self::draw_fsk_markers(draw_list, data, &axis, mode, &estimate, &look);
                    Self::draw_listen_band(draw_list, data, &axis, listen, &look);
                    Self::draw_receiver_band(
                        draw_list, data, &axis, receiver_band, receiver_vfo, &look,
                    );
                }
                if notch_count > 0 {
                    Self::draw_notch(
                        draw_list,
                        data,
                        &axis,
                        &notch_marks[..notch_count],
                        notch_width,
                        &look,
                    );
                }
                Self::draw_reference(draw_list, data, &axis, reference, &look);
                if look.crosshair {
                    Self::draw_crosshair(draw_list, data, cursor, &look);
                }
            }
            draw_list.pop_clip();
        }

        if let Some(r) = decode_rect {
            Self::draw_decode(
                draw_list,
                fonts,
                &decode.log,
                r,
                mono_px,
                focus,
                &look,
                decode_view,
                decode_hits,
            );
        } else {
            // The panel is not declared this frame, so the rectangles from the
            // last one describe a place nothing is drawn. A click against them
            // would tune to a line the operator cannot see.
            decode_hits.clear();
        }

        if let Some(r) = meter_rect {
            let meter_look = MeterLook {
                scale,
                ui_px: (mono_px * 0.85).round().max(8.0),
                label_px: look.label_px,
                accent: look.accent,
                text: look.text,
                dim: look.dim,
                faint: look.faint,
                well: meter_well,
                border: meter_border,
                segmented: settings.appearance.meter_segmented,
                segment_px: settings.appearance.meter_segment_px,
                segment_gap_px: settings.appearance.meter_segment_gap_px,
                scale_labels: settings.appearance.meter_scale_labels,
            };
            Self::draw_meter(draw_list, fonts, &dsp.meter, &settings.meter, &meter_look, r);
        }

        if let Some(r) = timeline_rect {
            Self::draw_timeline(draw_list, r, replay_timeline, replay_status.as_ref(), &look);
        }
    }

    /// Marks the frequencies the notch removes.
    ///
    /// Dashed, and in a colour of its own. Both are needed: the colour is the
    /// faster read and the dashes are what keep it apart from the passband and
    /// the monitor band for a reader whose colour vision differs. The shade is
    /// light, because the trace underneath still shows what is being removed and
    /// erasing it would be a claim about the signal rather than about the filter.
    fn draw_notch(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        marks: &[f32],
        width_hz: f32,
        look: &DataLook,
    ) {
        if r.is_empty() {
            return;
        }
        let thickness = (1.0 * look.scale).max(1.0);
        let dash = (4.0 * look.scale).max(3.0);

        for &hz in marks {
            let t = axis.fraction_of_audio(hz);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = (r.x + r.w * t).round();

            if width_hz > 1.0 {
                let a = r.x + r.w * axis.fraction_of_audio(hz - width_hz * 0.5).clamp(0.0, 1.0);
                let b = r.x + r.w * axis.fraction_of_audio(hz + width_hz * 0.5).clamp(0.0, 1.0);
                let (x0, x1) = if a < b { (a, b) } else { (b, a) };
                if x1 - x0 >= 1.0 {
                    list.fill_rect(
                        Rect::from_min_max(x0, r.y, x1, r.bottom()),
                        look.notch.with_alpha(0.16),
                    );
                }
            }

            let mut y = r.y;
            while y < r.bottom() {
                let end = (y + dash).min(r.bottom());
                list.vline(x, y, end, thickness, look.notch);
                y = end + dash;
            }
            // A short solid bar at the top, so the mark is findable where the
            // dashes fall between two bright rows of a busy picture.
            list.fill_rect(
                Rect::new(
                    x,
                    r.y,
                    (thickness * 3.0).round(),
                    (r.h * 0.08).max(4.0 * look.scale).round(),
                ),
                look.notch,
            );
        }
    }

    /// Marks the point a measurement is taken from.
    ///
    /// One line and no label. The number belongs in the status line, where a
    /// number belongs; a label here would have to be placed somewhere, and every
    /// placement is wrong for some position of the marker.
    fn draw_reference(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        hz: Option<f32>,
        look: &DataLook,
    ) {
        let hz = match hz {
            Some(v) => v,
            None => return,
        };
        if r.is_empty() {
            return;
        }
        let t = axis.fraction_of_audio(hz);
        if !(0.0..=1.0).contains(&t) {
            return;
        }
        let x = (r.x + r.w * t).round();
        let thickness = (1.0 * look.scale).max(1.0);
        list.vline(x, r.y, r.bottom(), thickness, look.reference);
        let flag = (r.h * 0.10).max(5.0 * look.scale).round();
        list.fill_rect(Rect::new(x, r.y, (thickness * 3.0).round(), flag), look.reference);
    }

    /// Age of the history down the left gutter of the waterfall.
    ///
    /// The waterfall says what was received and not when. The line rate is known
    /// exactly, so the age of a row is arithmetic rather than a measurement, and
    /// it is what an operator needs when they are waiting for a transmission at
    /// a stated time or judging how long a station has been working.
    ///
    /// Stated as an age rather than as a clock. What is being asked is how long
    /// ago, and a wall clock forces the reader to subtract.
    ///
    /// The tick reaches a few pixels into the data, because a label with nothing
    /// beside it has to be read across an empty gutter and the eye loses the
    /// row. A full line across the picture would be worse: the picture is the
    /// thing being read.
    #[allow(clippy::too_many_arguments)]
    fn draw_time_axis(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        gutter: f32,
        rows: u32,
        lines_per_second: f32,
        look: &DataLook,
    ) {
        if r.is_empty() || gutter <= 1.0 || rows == 0 || lines_per_second <= 0.01 {
            return;
        }
        let span = rows as f32 / lines_per_second;
        if span < 2.0 {
            return;
        }

        let min_spacing = (look.label_px * 2.2).max(22.0 * look.scale);
        let mut step = 600.0f32;
        for candidate in [5.0f32, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0] {
            step = candidate;
            if r.h * (candidate / span) >= min_spacing {
                break;
            }
        }

        let thickness = (1.0 * look.scale).max(1.0);
        let reach = (4.0 * look.scale).max(3.0);
        let mut age = step;
        while age < span {
            let y = (r.bottom() - r.h * (age / span)).round();
            if y < r.y {
                break;
            }
            list.hline(r.x - reach, r.x + reach, y, thickness, look.grid_minor);

            let caption = if age >= 60.0 {
                format!("{:.0}:{:02.0}", (age / 60.0).floor(), age % 60.0)
            } else {
                format!("{:.0}s", age)
            };
            let w = fonts.measure(&caption, FontId::Mono, look.label_px);
            // Right aligned against the tick, so the numbers form a column
            // whatever their width.
            let x = (r.x - reach - 2.0 * look.scale - w).max(r.x - gutter);
            fonts.draw_text(
                list,
                x.round(),
                (y + look.label_px * 0.36).round(),
                &caption,
                FontId::Mono,
                look.label_px,
                look.faint,
            );
            age += step;
        }
    }

    /// Vertical line under the pointer, across the data area.
    ///
    /// Two lines rather than a full crosshair: the horizontal one would state a
    /// level, and a level under the pointer is not a fact about the signal, only
    /// about where the pointer happens to be. The vertical one states a
    /// frequency, which is what the whole display is indexed by, and it is the
    /// only way to compare a peak of the trace against a stripe in the history
    /// without a straight edge.
    fn draw_crosshair(list: &mut DrawList, r: Rect, cursor: Option<f32>, look: &DataLook) {
        let t = match cursor {
            Some(t) => t,
            None => return,
        };
        if r.is_empty() {
            return;
        }
        let x = (r.x + r.w * t.clamp(0.0, 1.0)).round();
        list.vline(x, r.y, r.bottom(), (1.0 * look.scale).max(1.0), look.accent.with_alpha(0.40));
    }

    /// Marks the part of the display the channel allocator does not look at.
    fn draw_passband(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        band: Option<(f32, f32)>,
        look: &DataLook,
    ) {
        let (low_hz, high_hz) = match band {
            Some(b) => b,
            None => return,
        };
        if r.is_empty() {
            return;
        }
        let to_x = |hz: f32| r.x + r.w * axis.fraction_of_audio(hz).clamp(0.0, 1.0);
        let a = to_x(low_hz);
        let b = to_x(high_hz);
        let (x0, x1) = if a < b { (a, b) } else { (b, a) };
        let thickness = (1.0 * look.scale).max(1.0);

        if x0 > r.x + 0.5 {
            list.fill_rect(Rect::from_min_max(r.x, r.y, x0, r.bottom()), look.shade);
            list.vline(x0.round(), r.y, r.bottom(), thickness, look.shade_edge);
        }
        if x1 < r.right() - 0.5 {
            list.fill_rect(Rect::from_min_max(x1, r.y, r.right(), r.bottom()), look.shade);
            list.vline(x1.round(), r.y, r.bottom(), thickness, look.shade_edge);
        }
    }

    /// Vertical lines at a round step, with the labels in the bottom gutter.
    ///
    /// Two strengths rather than one. A grid of uniform lines forces the eye to
    /// count them to find a position; a grid where every fifth is stronger is
    /// read directly, which is the whole reason a printed scale is marked that
    /// way.
    ///
    /// A major line is a multiple of the step times the interval rather than the
    /// n-th line drawn, so the pattern stays fixed to the frequencies as the
    /// view pans instead of walking with it.
    fn draw_frequency_grid(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        axis: &Axis,
        look: &DataLook,
        gutter: f32,
    ) {
        if r.is_empty() {
            return;
        }
        let min_spacing = 60.0 * look.scale;
        let every = look.major_every.max(1) as i64;
        let label_y = if gutter > 0.0 {
            // Inside the reserved strip, so no label ever crosses the data.
            (r.bottom() + gutter - 3.0 * look.scale).round()
        } else {
            (r.bottom() - 4.0 * look.scale).round()
        };

        match axis.rf_range() {
            Some((lo, hi)) => {
                let span = (hi - lo).max(1) as f32;

                // Round kilohertz, so a line lands where a band plan does.
                const STEPS: [i64; 8] = [500, 1000, 2000, 5000, 10_000, 20_000, 50_000, 100_000];
                let mut step = STEPS[STEPS.len() - 1];
                for candidate in STEPS {
                    step = candidate;
                    if r.w * (candidate as f32 / span) >= min_spacing {
                        break;
                    }
                }

                let decimals = if step < 1000 { 1 } else { 0 };
                let mut f = (lo / step + 1) * step;
                while f <= hi {
                    if let Some(t) = axis.fraction_of_rf(f) {
                        let x = (r.x + r.w * t).round();
                        let major = (f / step).rem_euclid(every) == 0;
                        let color = if major { look.grid_major } else { look.grid_minor };
                        list.vline(x, r.y, r.bottom(), 1.0, color);
                        if look.labels && major {
                            let caption = format!("{:.*}", decimals, f as f64 / 1000.0);
                            fonts.draw_text(
                                list,
                                (x + 3.0 * look.scale).round(),
                                label_y,
                                &caption,
                                FontId::Mono,
                                look.label_px,
                                look.faint,
                            );
                        }
                    }
                    f += step;
                }
            }
            None => {
                // Audio axis. The smallest round step that keeps the lines apart
                // at the current width.
                let span = axis.span_hz();
                let mut step_hz = 100.0f32;
                for candidate in [100.0f32, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0] {
                    step_hz = candidate;
                    if r.w * (candidate / span) >= min_spacing {
                        break;
                    }
                }

                let mut hz = (axis.low_hz() / step_hz).ceil() * step_hz;
                while hz < axis.high_hz() {
                    let x = (r.x + r.w * axis.fraction_of_audio(hz)).round();
                    let index = (hz / step_hz).round() as i64;
                    let major = index.rem_euclid(every) == 0;
                    let color = if major { look.grid_major } else { look.grid_minor };
                    list.vline(x, r.y, r.bottom(), 1.0, color);
                    if look.labels && major {
                        let caption = if step_hz >= 1000.0 {
                            format!("{:.0}k", hz / 1000.0)
                        } else {
                            format!("{:.0}", hz)
                        };
                        fonts.draw_text(
                            list,
                            (x + 3.0 * look.scale).round(),
                            label_y,
                            &caption,
                            FontId::Mono,
                            look.label_px,
                            look.faint,
                        );
                    }
                    hz += step_hz;
                }
            }
        }
    }

    /// Horizontal lines at round levels, with the scale in the left gutter.
    ///
    /// The frequency grid answers where a signal is; without this one there is
    /// no way to answer how strong it is except from a meter, and a meter reads
    /// the whole passband rather than the one trace being looked at.
    fn draw_level_grid(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        min_db: f32,
        max_db: f32,
        look: &DataLook,
        gutter: f32,
    ) {
        if r.is_empty() {
            return;
        }
        let span = (max_db - min_db).max(1.0);
        let min_spacing = (look.label_px * 2.0).max(24.0 * look.scale);
        let every = look.major_every.max(1) as i64;

        // The smallest round step that keeps two lines from touching. Ten
        // decibels is what a receiver scale uses; the others cover a display
        // range set very wide or very narrow.
        let mut step = 50.0f32;
        for candidate in [5.0f32, 10.0, 20.0, 50.0] {
            step = candidate;
            if r.h * (candidate / span) >= min_spacing {
                break;
            }
        }

        let mut db = (min_db / step).ceil() * step;
        while db < max_db {
            let t = (db - min_db) / span;
            let y = (r.bottom() - r.h * t).round();
            let major = (db / step).round() as i64 % every == 0;
            let color = if major { look.grid_major } else { look.grid_minor };
            list.hline(r.x, r.right(), y, 1.0, color);

            if look.labels && major && gutter > 0.0 {
                let caption = format!("{:.0}", db);
                let w = fonts.measure(&caption, FontId::Mono, look.label_px);
                // Right aligned against the data edge, so the numbers form a
                // column whatever their width.
                let x = (r.x - 3.0 * look.scale - w).max(r.x - gutter);
                let baseline = y + look.label_px * 0.36;
                fonts.draw_text(
                    list,
                    x.round(),
                    baseline.round(),
                    &caption,
                    FontId::Mono,
                    look.label_px,
                    look.faint,
                );
            } else if look.labels && major {
                // No gutter: the label has to sit on the data, so it is kept
                // clear of the two edges where the other axis puts its own.
                if y > r.y + look.label_px && y < r.bottom() - look.label_px {
                    let caption = format!("{:.0}", db);
                    fonts.draw_text(
                        list,
                        (r.x + 3.0 * look.scale).round(),
                        (y - 2.0 * look.scale).round(),
                        &caption,
                        FontId::Mono,
                        look.label_px,
                        look.faint,
                    );
                }
            }
            db += step;
        }
    }

    /// Spectrum trace, with an optional shade under it.
    ///
    /// Only the bins the view covers are drawn, one segment per horizontal
    /// pixel, taking the strongest bin of each column so a narrow carrier is
    /// never skipped by the reduction. When the view is narrower than the pixel
    /// count the same bin serves several columns, which is the honest picture:
    /// the transform has no more detail to show.
    ///
    /// The shade is emitted in a pass of its own. Interleaving it with the line
    /// would put the fill of one column over the segment that ends in it, and
    /// the trace would come out dashed.
    #[allow(clippy::too_many_arguments)]
    fn draw_trace(
        list: &mut DrawList,
        r: Rect,
        bins: &[f32],
        axis: &Axis,
        min_db: f32,
        max_db: f32,
        look: &DataLook,
        color: Color,
        fill: bool,
    ) {
        if bins.is_empty() || r.is_empty() {
            return;
        }
        let (lo, hi) = axis.bin_range(bins.len());
        let view = &bins[lo..hi];
        if view.is_empty() {
            return;
        }

        let span = (max_db - min_db).max(1.0);
        let columns = (r.w as usize).clamp(2, 4096);
        let n = view.len();
        let thickness = (look.trace_thickness * look.scale).max(1.0);
        let mirrored = axis.mirrored();
        let step = r.w / (columns - 1) as f32;

        // Column peak, in screen coordinates. Computed twice rather than stored
        // in a scratch buffer: the cost is a few thousand comparisons and the
        // alternative is an allocation on the drawing path.
        let point = |c: usize| -> (f32, f32) {
            let source = if mirrored { columns - 1 - c } else { c };
            let a = source * n / columns;
            let b = ((source + 1) * n / columns).max(a + 1).min(n);
            let mut peak = f32::MIN;
            for &v in &view[a..b] {
                if v > peak {
                    peak = v;
                }
            }
            let t = ((peak - min_db) / span).clamp(0.0, 1.0);
            (r.x + r.w * (c as f32 / (columns - 1) as f32), r.bottom() - r.h * t)
        };

        if fill && look.trace_fill_alpha > 0.001 {
            let top = color.with_alpha(look.trace_fill_alpha);
            let bottom = color.with_alpha(0.0);
            for c in 0..columns {
                let (x, y) = point(c);
                let h = r.bottom() - y;
                if h <= 0.5 {
                    continue;
                }
                // One pixel of overlap keeps a seam from appearing between two
                // adjacent columns after rounding.
                list.gradient_v(
                    Rect::new(x.round(), y.round(), step.max(1.0).ceil(), h),
                    top,
                    bottom,
                );
            }
        }

        let mut previous: Option<(f32, f32)> = None;
        for c in 0..columns {
            let (x, y) = point(c);
            if let Some((px, py)) = previous {
                list.line(px, py, x, y, thickness, color);
            }
            previous = Some((x, y));
        }
    }

    /// Marks every keying channel: a shaded band the width of the detector, a
    /// centre line and, on the focused channel, a handle at each edge.
    #[allow(clippy::too_many_arguments)]
    fn draw_channel_markers(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        axis: &Axis,
        channels: &[ChannelInfo],
        focus: u32,
        look: &DataLook,
        labels: bool,
    ) {
        if r.is_empty() || channels.is_empty() {
            return;
        }
        let to_x = |hz: f32| r.x + r.w * axis.fraction_of_audio(hz).clamp(0.0, 1.0);
        let thickness = (1.0 * look.scale).max(1.0);

        for ch in channels {
            // No test against nought. On a two sided spectrum every frequency is
            // a real position and the tuning point itself is nought, so a guard
            // there hid every channel below the dial: the detector ran, decoded
            // and reported, and nothing on screen said where it was.
            let strong = ch.focused || (focus == 0 && channels.len() == 1);
            let band_alpha = if strong { 0.14 } else { 0.06 };
            let line = if strong {
                look.accent
            } else if ch.present {
                look.accent.with_alpha(0.55)
            } else {
                look.dim
            };

            let bandwidth_hz = ch.width_hz;
            let mut x0 = 0.0f32;
            let mut x1 = 0.0f32;
            if bandwidth_hz > 0.0 {
                x0 = to_x(ch.hz - bandwidth_hz * 0.5);
                x1 = to_x(ch.hz + bandwidth_hz * 0.5);
                list.fill_rect(
                    Rect::from_min_max(x0.min(x1), r.y, x0.max(x1), r.bottom()),
                    look.accent.with_alpha(band_alpha),
                );
            }
            let cx = to_x(ch.hz).round();
            list.vline(cx, r.y, r.bottom(), thickness, line);

            // A grip at the top of the centre line. At full span the shaded band
            // is under a pixel wide, so without it there is nothing on screen to
            // aim a press at and the channel can only be grabbed by luck.
            let grip_w = look.edge_grab.max(3.0).round();
            let grip_h = (r.h * 0.10).max(6.0 * look.scale).round();
            list.fill_rect(
                Rect::new((cx - grip_w * 0.5).round(), r.y, grip_w, grip_h),
                if strong { look.accent } else { line },
            );
            if ch.pinned {
                // A second mark below the grip, so a channel the operator is
                // holding is distinguishable from one the allocator opened.
                list.fill_rect(
                    Rect::new(
                        (cx - grip_w * 0.5).round(),
                        r.y + grip_h + (2.0 * look.scale).round(),
                        grip_w,
                        (2.0 * look.scale).max(2.0).round(),
                    ),
                    look.accent,
                );
            }

            if strong && bandwidth_hz > 0.0 {
                let handle_h = (r.h * 0.22).max(6.0 * look.scale);
                let handle_w = look.edge_grab.max(2.0);
                for x in [x0, x1] {
                    let rect =
                        Rect::new((x - handle_w * 0.5).round(), r.y, handle_w, handle_h.round());
                    list.fill_rect(rect, look.accent.with_alpha(0.55));
                    list.vline(
                        x.round(),
                        r.y,
                        r.bottom(),
                        thickness,
                        look.accent.with_alpha(0.45),
                    );
                }
            }

            if labels {
                let caption = format!("{:.0}", ch.hz);
                fonts.draw_text(
                    list,
                    (cx + 3.0 * look.scale).round(),
                    (r.y + look.label_px + 2.0 * look.scale).round(),
                    &caption,
                    FontId::Mono,
                    look.label_px,
                    line,
                );
            }
        }
    }

    /// Marks the tone pair of the teleprinter demodulator.
    fn draw_fsk_markers(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        mode: Mode,
        est: &crate::decode::classify::Estimate,
        look: &DataLook,
    ) {
        if r.is_empty() || !matches!(mode, Mode::Rtty | Mode::Navtex) {
            return;
        }
        // Existence rather than magnitude, for the reason the keying markers
        // give: a pair below the tuning point has a negative mark tone and is
        // not an absent one.
        if !est.tone_valid || est.shift_hz <= 0.0 {
            return;
        }

        let to_x = |hz: f32| r.x + r.w * axis.fraction_of_audio(hz).clamp(0.0, 1.0);
        let thickness = (1.0 * look.scale).max(1.0);
        let x1 = to_x(est.mark_hz);
        let x0 = to_x(est.mark_hz - est.shift_hz);
        list.fill_rect(
            Rect::from_min_max(x0.min(x1), r.y, x0.max(x1), r.bottom()),
            look.accent.with_alpha(0.10),
        );
        list.vline(x0.round(), r.y, r.bottom(), thickness, look.accent);
        list.vline(x1.round(), r.y, r.bottom(), thickness, look.accent);
    }

    /// Marks the band the headphone monitor is passing.
    ///
    /// A bar above the display with a bracket at each end rather than a filled
    /// band. The decoder already owns the filled band, and the whole value of
    /// showing this one is that a mismatch between the two is obvious: two bands
    /// of the same shape in two colours would have to be told apart by hue
    /// alone, which fails at a glance and fails entirely for an operator whose
    /// colour vision differs.
    fn draw_listen_band(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        band: Option<(f32, f32)>,
        look: &DataLook,
    ) {
        let (lo, hi) = match band {
            Some(b) => b,
            None => return,
        };
        if r.is_empty() || hi <= lo {
            return;
        }

        let to_x = |hz: f32| r.x + r.w * axis.fraction_of_audio(hz).clamp(0.0, 1.0);
        let a = to_x(lo);
        let b = to_x(hi);
        let (x0, x1) = if a < b { (a, b) } else { (b, a) };
        if x1 - x0 < 1.0 {
            return;
        }

        let thickness = (2.0 * look.scale).max(2.0).round();
        let drop = (5.0 * look.scale).max(4.0).round();
        let top = r.y.round();

        list.fill_rect(
            Rect::from_min_max(x0.round(), top, x1.round(), top + thickness),
            look.monitor,
        );
        // The end brackets reach down into the trace so the edges stay locatable
        // when the bar itself is short.
        list.vline(x0.round(), top, top + drop, thickness, look.monitor);
        list.vline(x1.round(), top, top + drop, thickness, look.monitor);
    }

    /// Marks the band the receiver filter passes and the point it is tuned to.
    ///
    /// The band uses the same shape the keying channels use, and deliberately
    /// so: a filter edge that can be grabbed has to look grabbable. The two
    /// never appear together, because the decoders are not fed in the receiver
    /// mode.
    ///
    /// The tuning point is marked separately, because for a sideband mode it is
    /// not the middle of the band: it is the suppressed carrier, which is the
    /// frequency that goes in a log.
    fn draw_receiver_band(
        list: &mut DrawList,
        r: Rect,
        axis: &Axis,
        band: Option<(f32, f32)>,
        vfo_hz: Option<f32>,
        look: &DataLook,
    ) {
        if r.is_empty() {
            return;
        }
        let to_x = |hz: f32| r.x + r.w * axis.fraction_of_audio(hz).clamp(0.0, 1.0);
        let thickness = (1.0 * look.scale).max(1.0);

        if let Some((lo, hi)) = band {
            if hi > lo {
                let a = to_x(lo);
                let b = to_x(hi);
                let (x0, x1) = if a < b { (a, b) } else { (b, a) };
                if x1 - x0 >= 1.0 {
                    list.fill_rect(
                        Rect::from_min_max(x0, r.y, x1, r.bottom()),
                        look.accent.with_alpha(0.14),
                    );

                    let handle_h = (r.h * 0.22).max(6.0 * look.scale);
                    let handle_w = look.edge_grab.max(2.0);
                    for x in [x0, x1] {
                        let rect =
                            Rect::new((x - handle_w * 0.5).round(), r.y, handle_w, handle_h.round());
                        list.fill_rect(rect, look.accent.with_alpha(0.55));
                        list.vline(
                            x.round(),
                            r.y,
                            r.bottom(),
                            thickness,
                            look.accent.with_alpha(0.45),
                        );
                    }
                }
            }
        }

        if let Some(hz) = vfo_hz {
            let x = to_x(hz).round();
            list.vline(x, r.y, r.bottom(), thickness, look.accent.with_alpha(0.85));
            // A short flag at the top separates the tuning point from the band
            // edges without a second colour, which is what keeps the picture
            // readable for an operator whose colour vision differs.
            let flag = (r.h * 0.12).max(5.0 * look.scale).round();
            list.fill_rect(
                Rect::new(x, r.y, (look.edge_grab * 0.6).max(2.0).round(), flag),
                look.accent,
            );
        }
    }

    /// Marks known frequencies.
    ///
    /// A full height line rather than a band, because a station list states a
    /// frequency and not a width, and drawing a width would be inventing one.
    #[allow(clippy::too_many_arguments)]
    fn draw_stations(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        axis: &Axis,
        catalog: &crate::stations::Catalog,
        indices: &[usize],
        look: &DataLook,
        labels: bool,
    ) {
        if r.is_empty() || indices.is_empty() {
            return;
        }
        let thickness = (1.0 * look.scale).max(1.0);
        // Labels are stacked when two markers are close, so neither is buried
        // under the other. Three rows is enough for any list an operator keeps
        // by hand and bounds the clutter when it is not.
        let row_h = look.label_px + 2.0 * look.scale;
        let mut occupied: [f32; 3] = [f32::MIN; 3];

        for &index in indices {
            let station = match catalog.all().get(index) {
                Some(s) => s,
                None => continue,
            };
            let t = match axis.fraction_of_rf(station.hz) {
                Some(t) if (0.0..=1.0).contains(&t) => t,
                _ => continue,
            };
            let x = (r.x + r.w * t).round();
            list.vline(x, r.y, r.bottom(), thickness, look.station.with_alpha(0.65));

            if !labels {
                continue;
            }
            let width = fonts.measure(&station.label, FontId::Mono, look.label_px);
            let mut row = 0usize;
            while row < occupied.len() && x < occupied[row] {
                row += 1;
            }
            if row >= occupied.len() {
                continue;
            }
            occupied[row] = x + width + 6.0 * look.scale;

            let y = (r.y + look.label_px + 2.0 * look.scale + row as f32 * row_h).round();
            fonts.draw_text(
                list,
                (x + 3.0 * look.scale).round(),
                y,
                &station.label,
                FontId::Mono,
                look.label_px,
                look.station,
            );
        }
    }

    /// Decode panel.
    ///
    /// Lines are laid out from the bottom upwards so the newest traffic is
    /// always visible and there is no scroll position to maintain. The lines
    /// still being assembled sit on the bottom rows, one per channel, prefixed
    /// with a dotted stamp instead of a time because they do not have a final
    /// one yet. Every line carries the frequency it came from, which is the only
    /// thing that separates two stations in a shared column of text.
    #[allow(clippy::too_many_arguments)]
    fn draw_decode(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        log: &DecodeLog,
        r: Rect,
        mono_px: f32,
        focus: u32,
        look: &DataLook,
        view: &DecodeView,
        hits: &mut Vec<DecodeHit>,
    ) {
        hits.clear();
        if r.is_empty() {
            return;
        }
        list.push_clip(r);

        let gap = (4.0 * look.scale).round();
        let line_h = fonts.line_height(FontId::Mono, mono_px);
        let metrics = fonts.metrics(FontId::Mono, mono_px);
        let x = (r.x + gap).round();
        let mut baseline = r.bottom() - gap - metrics.descent;

        // Both sides of the comparison are folded to one case, so a call sign
        // typed either way finds the traffic. The needle is folded once rather
        // than per line, which for a buffer of a few thousand is the difference
        // between one allocation and a few thousand.
        let needle = view.filter.trim().to_ascii_uppercase();
        let admits =
            |text: &str| -> bool { needle.is_empty() || text.to_ascii_uppercase().contains(&needle) };

        // Lines still being assembled sit at the bottom, and only while the view
        // is at the newest line. A view scrolled back is a view of the past, and
        // live text arriving underneath it would move the very lines the
        // operator is reading.
        if view.scroll == 0 {
            let mut pending: Vec<&crate::decode::log::PendingLine> = log
                .pending()
                .iter()
                .filter(|p| !p.text.is_empty() && admits(&p.text))
                .collect();
            pending.sort_by(|a, b| b.hz.partial_cmp(&a.hz).unwrap_or(std::cmp::Ordering::Equal));

            for p in pending {
                if baseline < r.y + metrics.ascent {
                    break;
                }
                let prefix = format!("....{}", p.tag());
                let advance = fonts.draw_text(
                    list,
                    x,
                    baseline.round(),
                    &prefix,
                    FontId::Mono,
                    mono_px,
                    look.faint,
                );
                let color = if p.channel == focus && focus != 0 { look.accent } else { look.text };
                Self::draw_decode_runs(
                    list,
                    fonts,
                    (x + advance + gap).round(),
                    baseline.round(),
                    &p.text,
                    mono_px,
                    color,
                    look,
                );
                hits.push(DecodeHit {
                    top: baseline - metrics.ascent,
                    bottom: baseline + metrics.descent,
                    hz: p.hz,
                });
                baseline -= line_h;
            }
        }

        // Walking the buffer backwards means only the visible lines are
        // formatted, which matters when it holds a few thousand of them.
        //
        // The scroll position counts admitted lines rather than stored ones. On
        // the other arrangement a filter would make the wheel appear to jump,
        // because most of what it stepped over would be lines the filter hides.
        let mut skipped = 0usize;
        for line in log.lines().rev() {
            if !admits(&line.text) {
                continue;
            }
            if skipped < view.scroll {
                skipped += 1;
                continue;
            }
            if baseline < r.y + metrics.ascent {
                break;
            }
            let prefix = format!("{}{}", line.stamp(), line.tag());
            let advance = fonts.draw_text(
                list,
                x,
                baseline.round(),
                &prefix,
                FontId::Mono,
                mono_px,
                look.faint,
            );
            Self::draw_decode_runs(
                list,
                fonts,
                (x + advance + gap).round(),
                baseline.round(),
                &line.text,
                mono_px,
                look.text,
                look,
            );
            hits.push(DecodeHit {
                top: baseline - metrics.ascent,
                bottom: baseline + metrics.descent,
                hz: line.hz,
            });
            baseline -= line_h;
        }

        list.pop_clip();
    }

    /// Draws one line of decoded text, marking the call signs in it.
    ///
    /// The marking is what makes a wall of text scannable. An operator reads a
    /// decode panel looking for one kind of token, and a colour that names that
    /// token turns the reading into a glance.
    ///
    /// Extraction runs per visible line per frame rather than being cached with
    /// the line. A cache would have to be invalidated when the length setting
    /// moves, and the scan is a state machine over ninety characters: for the
    /// thirty lines a panel holds that is a few thousand character tests, which
    /// is below the cost of formatting the time stamp beside them.
    #[allow(clippy::too_many_arguments)]
    fn draw_decode_runs(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        x: f32,
        baseline: f32,
        text: &str,
        px: f32,
        plain: Color,
        look: &DataLook,
    ) {
        if !look.call_highlight {
            fonts.draw_text(list, x, baseline, text, FontId::Mono, px, plain);
            return;
        }

        let mut found = Vec::new();
        crate::callsign::extract(text, look.call_min, false, &mut found);
        if found.is_empty() {
            fonts.draw_text(list, x, baseline, text, FontId::Mono, px, plain);
            return;
        }

        // The pen is carried rather than recomputed per run. The face is
        // monospaced, so the two agree, and carrying it keeps them agreeing if a
        // proportional face is ever used here.
        let mut pen = x;
        let mut at = 0usize;
        for item in &found {
            if item.start > at {
                pen += fonts.draw_text(
                    list,
                    pen.round(),
                    baseline,
                    &text[at..item.start],
                    FontId::Mono,
                    px,
                    plain,
                );
            }
            pen += fonts.draw_text(
                list,
                pen.round(),
                baseline,
                &text[item.start..item.end],
                FontId::Mono,
                px,
                look.call,
            );
            at = item.end;
        }
        if at < text.len() {
            fonts.draw_text(list, pen.round(), baseline, &text[at..], FontId::Mono, px, plain);
        }
    }

    /// Signal strength meter.
    ///
    /// Three rows inside one well: tick marks, the bar, and the scale values.
    /// Splitting them is what lets a value be read off the bar without a second
    /// look at a number, which is the whole point of a meter over a readout.
    ///
    /// The bar is solid by default and segmented on request. A solid bar
    /// resolves a change smaller than one block, which is what an operator
    /// adjusting an attenuator is watching for; a segmented one reads as an
    /// instrument at a distance where a solid one reads as a progress bar. The
    /// trade is real and the setting states which side of it the operator is on.
    fn draw_meter(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        meter: &SMeter,
        cfg: &MeterSettings,
        look: &MeterLook,
        r: Rect,
    ) {
        if r.is_empty() {
            return;
        }
        let border = (1.0 * look.scale).max(1.0).round();
        list.push_clip(r);
        list.fill_rect(r, look.well);
        list.stroke_rect(r, border, look.border);

        let inner = r.inset((2.0 * look.scale).round());
        if inner.is_empty() {
            list.pop_clip();
            return;
        }

        // The tick positions match the fraction mapping in the meter itself:
        // nine units below S9 take three fifths of the width and the sixty
        // decibels above it take the rest, which is the layout of a printed
        // receiver scale.
        const MARKS: [f32; 8] = [0.0, 0.15, 0.30, 0.45, 0.60, 0.733, 0.866, 1.0];
        const LABELS: [&str; 8] = ["1", "3", "5", "7", "9", "+20", "+40", "+60"];

        // The labels name S units, so they are only meaningful on that scale. On
        // a decibel scale the marks stay as an evenly divided reference and the
        // numbers would be wrong, so they are dropped.
        let s_units = cfg.scale == MeterScale::SUnits;
        let want_labels = look.scale_labels && s_units;

        let label_h = if want_labels {
            fonts.line_height(FontId::Mono, look.label_px)
        } else {
            0.0
        };
        let scale_h = ((inner.h - label_h) * 0.42).max(3.0);
        let bar_h = (inner.h - label_h - scale_h).max(3.0);

        for (i, &t) in MARKS.iter().enumerate() {
            let x = (inner.x + inner.w * t).round();
            // S9 and the top of the scale are the two positions an operator
            // reads against, so they are marked taller.
            let tall = i == 4 || i == 7;
            let height = if tall { scale_h } else { scale_h * 0.6 };
            let color = if tall { look.dim } else { look.faint };
            list.vline(x, inner.y, inner.y + height, border, color);
        }

        let bar_y = (inner.y + scale_h).round();
        let bar = Rect::new(inner.x, bar_y, inner.w, bar_h.round());
        let level = meter.fraction(meter.level_db(), cfg);
        let filled = (bar.w * level.clamp(0.0, 1.0)).round();

        if look.segmented {
            let block = (look.segment_px * look.scale).max(2.0).round();
            let gap = (look.segment_gap_px * look.scale).max(0.0).round();
            let pitch = block + gap;
            let mut x = bar.x;
            while x + block <= bar.x + filled {
                list.fill_rect(Rect::new(x.round(), bar.y, block, bar.h), look.accent);
                x += pitch;
            }
        } else if filled > 0.0 {
            list.fill_rect(Rect::new(bar.x, bar.y, filled, bar.h), look.accent);
        }

        if cfg.show_peak {
            let peak = meter.fraction(meter.peak_db(), cfg);
            let x = (bar.x + bar.w * peak.clamp(0.0, 1.0)).round();
            list.vline(
                x,
                bar.y - (1.0 * look.scale).round(),
                bar.bottom() + (1.0 * look.scale).round(),
                (2.0 * look.scale).max(1.0),
                look.text,
            );
        }

        if want_labels {
            let baseline = (inner.bottom() - look.label_px * 0.2).round();
            // The reading is the more precise statement, so a scale value that
            // would collide with it is dropped rather than overprinted.
            let reading = if cfg.show_numeric { meter.text(cfg) } else { String::new() };
            let reading_w = if reading.is_empty() {
                0.0
            } else {
                fonts.measure(&reading, FontId::Mono, look.label_px)
            };
            let reading_x = inner.right() - reading_w;

            for (i, &t) in MARKS.iter().enumerate() {
                let caption = LABELS[i];
                let w = fonts.measure(caption, FontId::Mono, look.label_px);
                // Centred on the tick, then pulled inside the well so the first
                // and the last stay whole.
                let mut x = inner.x + inner.w * t - w * 0.5;
                x = x.clamp(inner.x, inner.right() - w);
                if !reading.is_empty() && x + w > reading_x - 4.0 * look.scale {
                    continue;
                }
                fonts.draw_text(
                    list,
                    x.round(),
                    baseline,
                    caption,
                    FontId::Mono,
                    look.label_px,
                    look.faint,
                );
            }

            if !reading.is_empty() {
                fonts.draw_text(
                    list,
                    reading_x.round(),
                    baseline,
                    &reading,
                    FontId::Mono,
                    look.label_px,
                    look.text,
                );
            }
        } else if cfg.show_numeric {
            // No label row, so the reading goes at the right end of the tick
            // row, where the scale has nothing of its own.
            let caption = meter.text(cfg);
            let w = fonts.measure(&caption, FontId::Mono, look.ui_px);
            fonts.draw_text(
                list,
                (inner.right() - w).round(),
                fonts.baseline_centered(FontId::Mono, look.ui_px, inner.y, scale_h),
                &caption,
                FontId::Mono,
                look.ui_px,
                look.dim,
            );
        }

        list.pop_clip();
    }

    /// Scrub bar of the replay.
    ///
    /// The level of every block, drawn as a column. The levels come from the
    /// block markers rather than from the audio, so the whole timeline of an
    /// hour long recording is a quarter of a megabyte of markers and no samples
    /// are read at all: a scrub bar that had to decode would take seconds to
    /// appear and would touch the disk on every redraw.
    ///
    /// What it shows is where the traffic is. An operator returning to a
    /// recording is looking for the parts that carry something, and a flat bar
    /// with a position marker would leave them scrubbing at random.
    fn draw_timeline(
        list: &mut DrawList,
        r: Rect,
        timeline: &Timeline,
        status: Option<&ReplayStatus>,
        look: &DataLook,
    ) {
        if r.is_empty() {
            return;
        }
        list.push_clip(r);
        list.fill_rect(r, look.background);

        let total = timeline.len();
        if total == 0 {
            list.pop_clip();
            return;
        }

        // The range is the display range, so a level here means the same thing
        // it means on the spectrum and the two can be compared directly.
        let floor = -120.0f32;
        let span = 120.0f32;
        let columns = (r.w as usize).clamp(2, 4096);

        for c in 0..columns {
            let a = c * total / columns;
            let b = ((c + 1) * total / columns).max(a + 1).min(total);

            // The strongest block of the column rather than the mean. A short
            // transmission inside a quiet minute is exactly what the bar exists
            // to reveal, and an average would bury it.
            let mut peak = f32::MIN;
            let mut broken = false;
            for index in a..b {
                let marker = timeline.marker(index);
                if marker.peak_db > peak {
                    peak = marker.peak_db;
                }
                if timeline.is_break(index) {
                    broken = true;
                }
            }

            let t = ((peak - floor) / span).clamp(0.0, 1.0);
            let x = (r.x + r.w * (c as f32 / columns as f32)).round();
            let w = (r.w / columns as f32).max(1.0).ceil();
            let h = (r.h * t).round();
            if h > 0.0 {
                list.fill_rect(
                    Rect::new(x, (r.bottom() - h).round(), w, h),
                    look.accent.with_alpha(0.55),
                );
            }

            // A boundary between two segments that do not continue each other.
            // Without it a jump of an hour reads as a moment of silence.
            if broken {
                list.vline(x, r.y, r.bottom(), (1.0 * look.scale).max(1.0), look.station);
            }
        }

        if let Some(s) = status {
            let x = (r.x + r.w * s.fraction().clamp(0.0, 1.0)).round();
            list.vline(x, r.y, r.bottom(), (2.0 * look.scale).max(2.0), look.text);
            // A flag at the top, so the position stays locatable where the bar
            // itself is tall and bright.
            let flag = (r.h * 0.3).max(4.0 * look.scale).round();
            list.fill_rect(
                Rect::new(x, r.y, (3.0 * look.scale).max(2.0).round(), flag),
                look.text,
            );
        }

        list.stroke_rect(r, (1.0 * look.scale).max(1.0), look.grid_major);
        list.pop_clip();
    }

    /// Frequency readout over the display.
    ///
    /// Drawn in the top layer because the layout model has no absolute
    /// positioning, and a readout declared as a sibling would take real space
    /// away from the spectrum it belongs to. It sits at the right edge, opposite
    /// the level scale, so neither obscures the other.
    ///
    /// Leading zeros are dimmed rather than hidden. Hiding them would move every
    /// character sideways whenever the value crossed a decade, and a target that
    /// shifts under the pointer between one click and the next cannot be aimed
    /// at.
    ///
    /// Four lines, because four facts are read at different rates. The frequency
    /// changes constantly and is large. The mode changes a few times an hour and
    /// sits beside it as a tag. The signal strength changes continuously and is
    /// read at a glance, so it is a bar and not only a number: a bar is compared
    /// against its own previous position without being read at all. The passband
    /// and the chain state change when the operator changes them.
    fn draw_readout(&mut self, mono_px: f32) {
        if !self.settings.rig.show_readout {
            self.readout_rect = Rect::default();
            return;
        }
        // The frequency at the reference point rather than the raw dial. The
        // grid is drawn through the same mapping, so a correction that moves one
        // moves the other.
        let axis = self.axis();
        let hz = match self.readout_hz() {
            Some(v) => v,
            None => {
                self.readout_rect = Rect::default();
                return;
            }
        };
        let host = match self
            .gui
            .custom_rect(TAG_SPECTRUM)
            .or_else(|| self.gui.custom_rect(TAG_WATERFALL))
        {
            Some(r) if !r.is_empty() => r,
            _ => {
                self.readout_rect = Rect::default();
                return;
            }
        };

        let scale = self.ui_scale;
        let px = (mono_px * self.settings.rig.readout_scale).round().max(10.0);
        let tag_px = (px * 0.42).round().max(9.0);
        let sub_px = (px * 0.30).round().max(8.0);
        let pad = (8.0 * scale).round();
        let gap = (6.0 * scale).round();

        let lowest = if self.settings.rig.readout_fine { 0 } else { 1 };
        let readout = Readout::new(hz, lowest);

        // Monospaced, so one measurement describes every character and the hit
        // test is an index rather than a search.
        let char_w = self.fonts.measure("0", FontId::Mono, px);
        let digits_w = char_w * readout.len() as f32;
        let metrics = self.fonts.metrics(FontId::Mono, px);

        // The mode is the tag beside the frequency rather than a line of its
        // own: it qualifies the number, and a reading of the two together is
        // what an operator actually takes.
        let tag = if self.rig_mode.is_empty() {
            String::new()
        } else {
            format!("[{}]", self.rig_mode)
        };
        let tag_w = if tag.is_empty() {
            0.0
        } else {
            self.fonts.measure(&tag, FontId::Ui, tag_px) + gap
        };

        // Signal strength. Read from the same meter the panel draws, so the two
        // cannot disagree about the reading or about the scale it is on.
        let meter_text = self.dsp.meter.text(&self.settings.meter);
        let meter_level = self
            .dsp
            .meter
            .fraction(self.dsp.meter.level_db(), &self.settings.meter);
        let meter_peak = self
            .dsp
            .meter
            .fraction(self.dsp.meter.peak_db(), &self.settings.meter);
        let show_peak = self.settings.meter.show_peak;

        let bar_w = (px * 2.6).round();
        let bar_h = (sub_px * 0.65).round().max(3.0);
        let meter_text_w = self.fonts.measure(&meter_text, FontId::Mono, sub_px);
        let meter_w = bar_w + gap + meter_text_w;
        let sub_h = self.fonts.line_height(FontId::Mono, sub_px);
        let meter_h = sub_h.max(bar_h + (2.0 * scale).round());

        // Passband, on the air when the mapping allows it and in audio when it
        // does not. The receiver filter in the receiver mode, the keying
        // detector otherwise: in each case the band that decides what is being
        // worked on.
        let (band_lo, band_hi) = if self.settings.sdr_mode() {
            (
                self.settings.receiver.filter_low_hz,
                self.settings.receiver.filter_high_hz,
            )
        } else {
            let centre = self.decode.status().cw_tone_hz;
            let width = self.decode.status().cw_bandwidth_hz.max(50.0);
            (centre - width * 0.5, centre + width * 0.5)
        };
        let band = match axis.mapping() {
            Some(m) => {
                let a = m.rf_of(band_lo);
                let b = m.rf_of(band_hi);
                let (lo, hi) = if a < b { (a, b) } else { (b, a) };
                format!("{:.2} - {:.2} kHz", lo as f64 / 1000.0, hi as f64 / 1000.0)
            }
            None => format!("{:.0} - {:.0} Hz", band_lo, band_hi),
        };

        // Last line: what stands between the number above and the transceiver.
        //
        // In the receiver mode the offset the software oscillator is holding,
        // whenever it is holding one. That is the one figure that explains a
        // readout disagreeing with the front panel of the transceiver: without
        // it the two simply differ, and an operator has no way to tell a
        // deliberate offset from a correction that is wrong.
        //
        // The gain of the loop follows it, which says whether the input level is
        // sensible. Otherwise the step a wheel outside a digit moves, which is
        // what the readout itself responds to.
        //
        // Read before the status is borrowed, because the predicate takes the
        // whole structure and the status lives inside it.
        let offset_hz = if self.local_tuning() {
            self.settings.receiver.tune_hz
        } else {
            0.0
        };
        let detail = if self.settings.sdr_mode() {
            let s = &self.monitor_status;
            let gain = if s.receiver && s.running {
                format!(
                    "{:+.0} dB{}{}",
                    s.rx_gain_db,
                    if s.rx_open { "" } else { "  muted" },
                    if s.rx_locked { "  lock" } else { "" }
                )
            } else {
                String::new()
            };
            // A hertz is the resolution of the finest readout, so anything below
            // it is a rounding remainder rather than an offset.
            if offset_hz.abs() >= 1.0 && gain.is_empty() {
                format!("{:+.0} Hz from the dial", offset_hz)
            } else if offset_hz.abs() >= 1.0 {
                format!("{:+.0} Hz   {}", offset_hz, gain)
            } else {
                gain
            }
        } else {
            format!("step {} Hz", self.settings.rig.tune_step_hz)
        };

        let block_w = (tag_w + digits_w)
            .max(meter_w)
            .max(self.fonts.measure(&band, FontId::Mono, sub_px))
            .max(self.fonts.measure(&detail, FontId::Mono, sub_px));
        let block_h = metrics.ascent + metrics.descent + meter_h + sub_h * 2.0;

        let block_x = (host.right() - pad - block_w).round();
        let block_y = (host.y + pad).round();
        let baseline = (block_y + metrics.ascent).round();

        let bg = self.gui.theme.data_background.with_alpha(0.78);
        let bright = self.gui.theme.text;
        let faint = self.gui.theme.text_faint;
        let dim = self.gui.theme.text_dim;
        let dark = self.gui.theme.surface_dark;
        let accent = self.accent;

        {
            let App { fonts, draw_list, .. } = self;
            draw_list.fill_rect(
                Rect::new(
                    block_x - pad * 0.5,
                    block_y - pad * 0.5,
                    block_w + pad,
                    block_h + pad,
                ),
                bg,
            );

            if !tag.is_empty() {
                fonts.draw_text(draw_list, block_x, baseline, &tag, FontId::Ui, tag_px, dim);
            }

            // One character at a time, so the dim leading zeros and the live
            // digits can carry different colours without two passes over the
            // string and without the two disagreeing about where a glyph sits.
            let digits_x = (block_x + tag_w).round();
            let mut buffer = [0u8; 4];
            for index in 0..readout.len() {
                let ch = readout.char_at(index);
                let color = if readout.is_leading(index) {
                    faint
                } else if ch == '.' {
                    dim
                } else {
                    accent
                };
                let x = (digits_x + char_w * index as f32).round();
                let text = ch.encode_utf8(&mut buffer);
                fonts.draw_text(draw_list, x, baseline, text, FontId::Mono, px, color);
            }

            // Meter row.
            let meter_y = baseline + metrics.descent;
            let bar = Rect::new(
                block_x,
                (meter_y + (meter_h - bar_h) * 0.5).round(),
                bar_w,
                bar_h,
            );
            draw_list.fill_rect(bar, dark);
            let filled = (bar.w * meter_level.clamp(0.0, 1.0)).round();
            if filled > 0.0 {
                draw_list.fill_rect(Rect::new(bar.x, bar.y, filled, bar.h), accent);
            }
            if show_peak {
                let x = (bar.x + bar.w * meter_peak.clamp(0.0, 1.0)).round();
                draw_list.vline(
                    x,
                    bar.y - (1.0 * scale).round(),
                    bar.bottom() + (1.0 * scale).round(),
                    (1.0 * scale).max(1.0),
                    bright,
                );
            }
            fonts.draw_text(
                draw_list,
                (block_x + bar_w + gap).round(),
                fonts.baseline_centered(FontId::Mono, sub_px, meter_y, meter_h),
                &meter_text,
                FontId::Mono,
                sub_px,
                bright,
            );

            let mut y = meter_y + meter_h + sub_px;
            fonts.draw_text(
                draw_list,
                block_x,
                y.round(),
                &band,
                FontId::Mono,
                sub_px,
                bright,
            );
            y += sub_h;
            if !detail.is_empty() {
                fonts.draw_text(
                    draw_list,
                    block_x,
                    y.round(),
                    &detail,
                    FontId::Mono,
                    sub_px,
                    dim,
                );
            }
        }

        // Only the digits are a target. The tag, the meter and the two lines
        // underneath carry no decade, and a press on one of them must fall
        // through rather than be rounded onto whichever digit happens to be
        // nearest.
        self.readout_rect = Rect::new(
            (block_x + tag_w).round(),
            block_y,
            digits_w.round(),
            (metrics.ascent + metrics.descent).round(),
        );
        self.readout_char_w = char_w;
    }

    /// Moves the receiver filter under a drag.
    ///
    /// The edges are relative to the tuning point, so the pointer frequency is
    /// converted before anything is decided. On a real input the tuning point
    /// is nought and the two coincide, which is correct: there the edges are
    /// absolute audio.
    ///
    /// What the gesture grabbed is decided once, at the press. A band that
    /// narrows under the pointer would otherwise change which edge is nearest
    /// and the drag would jump across; a band being moved would swap into an
    /// edge drag the moment the pointer crossed the centre.
    fn apply_filter_drag(&mut self, hz: f32, started: bool, whole_band: bool) {
        let (floor, ceiling) = self.filter_bounds();
        let relative = hz - self.settings.receiver.tune_hz;
        let snap = |v: f32| (v / FILTER_STEP_HZ).round() * FILTER_STEP_HZ;

        if started {
            let low = self.settings.receiver.filter_low_hz;
            let high = self.settings.receiver.filter_high_hz;
            self.filter_base = (low, high);
            self.filter_anchor_hz = relative;
            self.filter_grab = if whole_band {
                FilterGrab::Band
            } else if (relative - low).abs() <= (relative - high).abs() {
                FilterGrab::Low
            } else {
                FilterGrab::High
            };
        }

        match self.filter_grab {
            FilterGrab::Low => {
                let top = self.settings.receiver.filter_high_hz - FILTER_MIN_WIDTH_HZ;
                self.settings.receiver.filter_low_hz =
                    snap(relative).clamp(floor, top.max(floor));
            }
            FilterGrab::High => {
                let bottom = self.settings.receiver.filter_low_hz + FILTER_MIN_WIDTH_HZ;
                self.settings.receiver.filter_high_hz =
                    snap(relative).clamp(bottom.min(ceiling), ceiling);
            }
            FilterGrab::Band => {
                let (base_low, base_high) = self.filter_base;
                let width = (base_high - base_low).max(FILTER_MIN_WIDTH_HZ);
                let shift = snap(relative - self.filter_anchor_hz);
                let low = (base_low + shift).clamp(floor, (ceiling - width).max(floor));
                self.settings.receiver.filter_low_hz = low;
                self.settings.receiver.filter_high_hz = (low + width).min(ceiling);
            }
            FilterGrab::None => {}
        }
    }

    fn shutdown(&mut self) {
        // The band being worked is recorded here as well as on a change, so a
        // session that ended without one is still where the operator left it
        // when the next one starts.
        if let (Some(band), Some(hz)) = (self.last_band, self.last_band_hz) {
            self.settings
                .bands
                .remember(band, hz, self.settings.receiver.detector);
        }

        self.close_replay();
        self.stop_recording();
        self.rig.stop();
        self.stop_audio();

        // Persist geometry so the next start restores the layout.
        if !self.window.is_minimized() {
            let (x, y, _w, _h) = self.window.outer_rect();
            self.settings.ui.window_x = x;
            self.settings.ui.window_y = y;
            let (cw, ch) = self.window.client_size();
            if cw > 0 && ch > 0 {
                self.settings.ui.window_width = cw;
                self.settings.ui.window_height = ch;
            }
        }

        if self.high_res_timer {
            platform::win32::end_high_resolution_timing();
        }
        crate::log_info!("app", "shutdown complete");
    }

    /// Moves the settings out for the final save.
    pub fn take_settings(&mut self) -> Settings {
        std::mem::take(&mut self.settings)
    }
}

/// Language codes the catalogue directory offers, reference wording first.
///
/// Scanned once at startup rather than per frame: the list changes only when a
/// file is added, which is not something that happens while the receiver runs.
fn scan_languages(directory: &std::path::Path) -> Vec<String> {
    let mut list = vec![crate::i18n::DEFAULT_LANGUAGE.to_string()];
    let entries = match std::fs::read_dir(directory) {
        Ok(e) => e,
        Err(_) => return list,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("lang") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        // The template is a starting point for a translator, not a language.
        if stem == "template" || stem.eq_ignore_ascii_case(crate::i18n::DEFAULT_LANGUAGE) {
            continue;
        }
        list.push(stem.to_string());
    }
    list
}

/// Fingerprint of everything that forces the call sign book to be reread.
///
/// The switches that only gate behaviour are absent: turning resolution off must
/// not discard a database that is about to be turned back on.
fn callsign_signature(settings: &crate::config::settings::CallsignSettings) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |bytes: &[u8]| {
        for b in bytes {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    mix(&[u8::from(settings.lookup_enabled)]);
    mix(&[settings.source as u8 + 1]);
    mix(settings.prefix_db_path.as_bytes());
    mix(settings.local_db_path.as_bytes());
    mix(settings.history_path.as_bytes());
    mix(&settings.history_limit.to_le_bytes());
    mix(&settings.cache_entries.to_le_bytes());
    h
}

/// Detector that corresponds to a transceiver mode.
///
/// The data modes map onto their own detectors rather than onto plain sideband,
/// because they differ in the filter preset and in what the decoders expect,
/// even though the demodulation is identical.
fn detector_of(mode: detent::Mode) -> Detector {
    match mode {
        detent::Mode::CwUpper | detent::Mode::CwLower => Detector::Cw,
        detent::Mode::SsbUpper => Detector::Usb,
        detent::Mode::SsbLower => Detector::Lsb,
        detent::Mode::DigUpper => Detector::DigU,
        detent::Mode::DigLower => Detector::DigL,
        detent::Mode::Am => Detector::Am,
        detent::Mode::Fm => Detector::Fm,
    }
}

/// Transceiver mode as a marker code.
///
/// A number rather than the enumeration itself, so the recording format does not
/// depend on the control library. Nought means unknown, which is what a
/// description that cannot read the mode produces.
fn mode_code(mode: detent::Mode) -> u8 {
    match mode {
        detent::Mode::CwUpper => 1,
        detent::Mode::CwLower => 2,
        detent::Mode::SsbUpper => 3,
        detent::Mode::SsbLower => 4,
        detent::Mode::DigUpper => 5,
        detent::Mode::DigLower => 6,
        detent::Mode::Am => 7,
        detent::Mode::Fm => 8,
    }
}

fn sideband_code(sideband: crate::rig::Sideband) -> u8 {
    match sideband {
        crate::rig::Sideband::Upper => 1,
        crate::rig::Sideband::Lower => 2,
    }
}

/// Transceiver mode that corresponds to a detector.
///
/// Synchronous detection has no counterpart: it is a way of receiving amplitude
/// modulation rather than a mode a transmitter can be put into, so it maps onto
/// the mode it is receiving.
fn rig_mode_of(detector: Detector) -> Option<detent::Mode> {
    Some(match detector {
        // The description binds the upper sideband entry to the reversed mode,
        // so the normal one is the lower sideband entry. Driving the reversed
        // mode places the audio above the dial, which leaves the part of the
        // band below it outside the captured span altogether.
        Detector::Cw => detent::Mode::CwLower,
        Detector::Usb => detent::Mode::SsbUpper,
        Detector::Lsb => detent::Mode::SsbLower,
        Detector::DigU => detent::Mode::DigUpper,
        Detector::DigL => detent::Mode::DigLower,
        Detector::Am | Detector::Sam => detent::Mode::Am,
        Detector::Fm => detent::Mode::Fm,
    })
}