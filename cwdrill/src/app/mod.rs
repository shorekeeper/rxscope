//! Application shell.
//!
//! Owns the window, the renderer, the font system, the widget system, the output
//! stream, the session and the history. What the operator sees is declared in the
//! panel module; this one supplies it with state and applies what it asks for.
//!
//! Frame order is fixed and every step depends on the previous one:
//!   1. drain the message queue and fold the events into the input snapshot;
//!   2. advance the clock;
//!   3. bring the output stream back if it went away, and publish the sound;
//!   4. drain the picture queues, telling the session what has begun to sound;
//!   5. advance the session, which tops up the queue and scores answers;
//!   6. declare the interface, which solves the layout and records commands;
//!   7. draw the widget tree, the reserved areas and the top layer;
//!   8. apply the commands, queue pending glyph rows and submit.
//!
//! Step four precedes step five because the session decides whether a group has
//! finished sounding, and that is what the drained boundaries tell it.

pub mod clock;
pub mod panel;
pub mod scope;
pub mod stats;

use std::path::PathBuf;

use crate::audio::{DeviceInfo, OutputConfig, OutputStatus, OutputStream};
use crate::config::settings::{DrillMode, PaddleMode, PaddleSettings, PaddleSource};
use crate::config::Settings;
use crate::core::Result;
use crate::font::{FontId, FontSystem};
use crate::gui::theme::Theme;
use crate::gui::{Frame, Ui};
use crate::i18n::Catalog;
use crate::lesson::{character_set, Material};
use crate::platform::{self, Event, Key, MouseButton, Window, WindowConfig};
use crate::progress::{CharRow, Progress};
use crate::render::{Color, DrawList, Mode, Rect, Renderer};
use crate::session::{Mark, Session};
use crate::synth::{Edge, Element, Keyed, Keyer};

use clock::FrameClock;
use panel::{
    Selections, StatusInfo, UiCommands, SIDE_PANEL_MAX, SIDE_PANEL_MAX_FRACTION, SIDE_PANEL_MIN,
    TAB_SETTINGS, TAG_DECODER, TAG_HEATMAP, TAG_PADDLE, TAG_PROMPT, TAG_SCOPE,
};
use scope::{Scope, MAX_SECONDS, SNAP_PIXELS};
use stats::{Stats, MATRIX_GAMMA};

/// Point size to pixels at the ninety six dpi baseline.
const PT_TO_PX: f32 = 4.0 / 3.0;

/// Interval between two configuration comparisons, in seconds.
const DEVIATION_POLL_S: f32 = 0.5;

/// Lines the report shows before it states a remainder.
const MAX_DEVIATIONS: usize = 30;

/// Interval between two rebuilds of the readout rows, in seconds.
///
/// The rows are forty entries of a hash lookup, which is nothing, and the data
/// changes once a group. Rebuilding per frame would be paying sixty times a
/// second for a figure that moves once every ten.
const ROWS_POLL_S: f32 = 0.25;

/// Delay before the first attempt to bring the output back, in seconds.
const RECOVER_FIRST_S: f32 = 1.0;

/// Longest the delay grows to.
const RECOVER_MAX_S: f32 = 20.0;

/// Duration of the test tone, in seconds.
const TEST_TONE_S: f32 = 0.6;

/// Magnification of the time span per wheel notch.
const SPAN_PER_NOTCH: f32 = 1.2;

/// Elements below which the interfering station is topped up.
///
/// Shallower than the primary queue, because nothing is scored against it and a
/// gap in interference is a gap in interference rather than a lost character.
const QRM_LOW: u32 = 32;

/// What a scope drag grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeGrab {
    None,
    CursorA,
    CursorB,
    Pan,
}

/// Colours and switches the data area drawing reads.
struct DataLook {
    scale: f32,
    /// Size of an axis label. Smaller than the interface font: an axis is read by
    /// position and the number only confirms it.
    label_px: f32,
    accent: Color,
    text: Color,
    dim: Color,
    faint: Color,
    grid_major: Color,
    grid_minor: Color,
    background: Color,
    /// Colour of the ideal element boundaries.
    ///
    /// Its own, because it is the one mark that states what should have happened
    /// rather than what did. Sharing a colour with the grid would make the
    /// difference between the two invisible, which is the whole reading.
    ideal: Color,
    cursor: Color,
    /// Verdict colours. Green and red are the convention and are kept, with the
    /// omission drawn as neither: it is a character that was not answered rather
    /// than answered wrongly, and the distinction is the whole of the diagnosis.
    right: Color,
    wrong: Color,
    missed: Color,
    major_every: u32,
    trace_fill: bool,
    trace_fill_alpha: f32,
    trace_thickness: f32,
}

pub struct App {
    settings: Settings,
    window: Window,
    renderer: Renderer,
    fonts: FontSystem,
    gui: Ui,
    draw_list: DrawList,
    clock: FrameClock,
    events: Vec<Event>,
    gpu_name: String,
    font_name: String,

    languages: Vec<String>,
    language_dir: PathBuf,

    output: Option<OutputStream>,
    output_status: OutputStatus,
    /// True while the stream is meant to be open.
    output_wanted: bool,
    output_retry: f32,
    output_backoff: f32,
    output_recoveries: u32,
    devices: Vec<DeviceInfo>,
    sel: Selections,

    session: Session,
    progress: Progress,
    stats: Stats,
    /// Readout rows, refreshed on a timer.
    rows: Vec<CharRow>,
    rows_poll: f32,

    /// The interfering station. Its own keyer and material, because it is a
    /// different station: sharing either would make it the same text at another
    /// pitch, which the ear separates trivially and learns nothing from.
    qrm_keyer: Keyer,
    qrm_material: Material,
    qrm_elements: Vec<Element>,

    /// Contact each mouse button is holding, the dot being true.
    ///
    /// Latched at the press so the release reaches the same contact whatever has
    /// happened since. Without it a press that wandered off the picture would
    /// leave the tone on with nothing in reach to stop it, and a swap toggled
    /// mid press would open the contact the operator did not close.
    paddle_held: [Option<bool>; 2],

    /// What the paddle produced, drained once per frame.
    keyed_scratch: Vec<Keyed>,
    /// Elements of the character being assembled.
    ///
    /// Held here rather than in the session because it is a property of the key
    /// rather than of the exercise: the elements accumulate whether or not a
    /// session is running, and only a completed character is an answer.
    pattern: String,
    /// Answer the elements above belong to.
    ///
    /// Elements accumulate towards a character, and a character belongs to the
    /// answer it was keyed into. Without this the tail of a group that was scored
    /// or skipped becomes the head of the first character of the next one.
    answer_epoch: u32,
    /// Characters keyed that the alphabet does not hold.
    ///
    /// Counted rather than shown as a marker, because a marker is itself a
    /// character and would have to be scored as one. An unreadable character is
    /// a missed character, which is what the alignment already reports.
    sent_malformed: u32,

    scope: Scope,
    trace_scratch: Vec<f32>,
    edge_scratch: Vec<Edge>,
    ideal_marks: Vec<f32>,
    visible_edges: Vec<(f32, Edge)>,
    visible_labels: Vec<(f32, char)>,
    scope_grab: ScopeGrab,
    /// Age the pan grabbed, in seconds before the newest sample.
    ///
    /// Latched at the press so the picture follows the pointer rather than
    /// accumulating the movement, which would make a slow drag travel further
    /// than a fast one over the same distance.
    scope_pan_anchor: f32,

    deviations: Vec<String>,
    deviation_poll: f32,

    tab: usize,
    editing_tab: usize,
    add_section: usize,

    surface_size: (u32, u32),
    dpi_scale: f32,
    ui_scale: f32,
    accent: Color,

    minimized: bool,
    running: bool,
    high_res_timer: bool,
    time: f32,
}

/// Keys the application writes without being asked.
///
/// A difference in one of these says nothing about a decision. The geometry, the
/// endpoint, the composition of the panel and the training level are written by
/// the application itself, and every one differs from the shipped value on any
/// real installation; left in, they would bury the lines that matter.
const SESSION_KEYS: &[(&str, &str)] = &[
    ("ui", "window_x"),
    ("ui", "window_y"),
    ("ui", "window_width"),
    ("ui", "window_height"),
    ("ui", "maximized"),
    ("ui", "side_panel_width"),
    ("ui", "prompt_panel_fraction"),
    ("ui", "show_settings_panel"),
    ("audio", "device_id"),
    ("audio", "device_name"),
    ("lesson", "level"),
    ("scope", "seconds"),
    ("panel", "practice"),
    ("panel", "lesson"),
    ("panel", "sound"),
    ("panel", "display"),
    ("panel", "known"),
];

impl App {
    pub fn new(settings: Settings) -> Result<App> {
        let cfg = WindowConfig {
            title: format!("CWDrill {}", env!("CARGO_PKG_VERSION")),
            width: settings.ui.window_width,
            height: settings.ui.window_height,
            x: if settings.ui.window_x >= 0 { Some(settings.ui.window_x) } else { None },
            y: if settings.ui.window_y >= 0 { Some(settings.ui.window_y) } else { None },
            min_width: 760,
            min_height: 480,
            maximized: settings.ui.maximized,
            custom_frame: settings.appearance.custom_frame,
        };
        let window = Window::new(&cfg)?;

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
        let gpu_name = renderer.device_name().to_string();
        let stats = Stats::new(&mut renderer)?;

        let accent = Color::hex(settings.ui.accent_rgb);
        let mut theme = Theme::dark(accent);
        theme.apply(accent, &settings.appearance);
        let gui = Ui::new(theme);

        let language_dir = settings.language_dir();
        let languages = scan_languages(&language_dir);
        let progress = Progress::load(&settings.progress);

        crate::log_info!(
            "app",
            "surface {}x{} ui_scale {:.2} vsync {} gpu {}",
            surface_size.0,
            surface_size.1,
            ui_scale,
            settings.ui.vsync,
            gpu_name
        );

        let mut app = App {
            settings,
            window,
            renderer,
            fonts,
            gui,
            draw_list: DrawList::new(),
            clock: FrameClock::new(),
            events: Vec::with_capacity(128),
            gpu_name,
            font_name,
            languages,
            language_dir,
            output: None,
            output_status: OutputStatus::idle(),
            output_wanted: false,
            output_retry: 0.0,
            output_backoff: RECOVER_FIRST_S,
            output_recoveries: 0,
            devices: Vec::new(),
            sel: Selections::default(),
            session: Session::new(),
            progress,
            stats,
            rows: Vec::with_capacity(64),
            rows_poll: 0.0,
            qrm_keyer: Keyer::new(),
            qrm_material: Material::new(),
            qrm_elements: Vec::with_capacity(128),
            paddle_held: [None; 2],
            keyed_scratch: vec![Keyed::default(); 128],
            pattern: String::with_capacity(8),
            answer_epoch: 0,
            sent_malformed: 0,
            scope: Scope::new(),
            trace_scratch: vec![0.0; 4096],
            edge_scratch: vec![Edge::default(); 256],
            ideal_marks: Vec::with_capacity(256),
            visible_edges: Vec::with_capacity(256),
            visible_labels: Vec::with_capacity(128),
            scope_grab: ScopeGrab::None,
            scope_pan_anchor: 0.0,
            deviations: Vec::new(),
            deviation_poll: 0.0,
            tab: 0,
            editing_tab: 0,
            add_section: 0,
            surface_size,
            dpi_scale,
            ui_scale,
            accent,
            minimized: false,
            running: true,
            high_res_timer,
            time: 0.0,
        };

        app.rescan_devices();
        app.start_output();
        Ok(app)
    }

    // ------------------------------------------------------------- output

    fn rescan_devices(&mut self) {
        self.devices = crate::audio::enumerate();
        let wanted = self.settings.audio.device_id.clone();
        self.sel.device = self.devices.iter().position(|d| d.id == wanted).unwrap_or(0);
    }

    fn apply_device_selection(&mut self) {
        if let Some(d) = self.devices.get(self.sel.device) {
            self.settings.audio.device_id = d.id.clone();
            self.settings.audio.device_name = d.name.clone();
        }
    }

    fn stop_output(&mut self) {
        self.output_wanted = false;
        if let Some(mut stream) = self.output.take() {
            stream.stop();
        }
        self.output_status = OutputStatus::idle();
        self.stop_session();
        self.scope.clear();
    }

    fn start_output(&mut self) {
        self.stop_output();
        self.output_wanted = true;
        let cfg = OutputConfig::from_settings(&self.settings.audio);
        match OutputStream::start(cfg, &self.settings.tone, &self.settings.conditions) {
            Ok(stream) => {
                self.scope.set_rate(stream.rate(), stream.scope_step());
                self.output = Some(stream);
            }
            Err(e) => {
                crate::log_error!("app", "cannot open the output: {}", e);
                let mut status = OutputStatus::idle();
                status.error = e.to_string();
                self.output_status = status;
            }
        }
    }

    /// Brings a failed endpoint back on its own.
    ///
    /// The failure this exists for is a cable: headphones unplugged, a dock
    /// disconnected, a machine that woke from sleep. The delay grows and is never
    /// abandoned, and is reset on success rather than decayed: the next fault is a
    /// new fault, and inheriting the delay would make a second knock take twenty
    /// seconds to recover from.
    fn sync_output(&mut self, dt: f32) {
        self.output_status = match self.output.as_ref() {
            Some(s) => s.status(),
            None => self.output_status.clone(),
        };

        let down = self.output_wanted && (self.output.is_none() || !self.output_status.running);
        if down {
            self.output_retry -= dt;
            if self.output_retry <= 0.0 {
                self.output_recoveries += 1;
                crate::log_warn!(
                    "app",
                    "the output is down ({}), attempt {} after {:.0} s",
                    if self.output_status.error.is_empty() {
                        "no reason given"
                    } else {
                        self.output_status.error.as_str()
                    },
                    self.output_recoveries,
                    self.output_backoff
                );
                self.output_backoff = (self.output_backoff * 2.0).min(RECOVER_MAX_S);
                self.output_retry = self.output_backoff;
                self.start_output();
            }
        } else {
            self.output_backoff = RECOVER_FIRST_S;
            self.output_retry = 0.0;
        }

        if let Some(stream) = self.output.as_ref() {
            stream.publish(&self.settings.tone, &self.settings.conditions);
            // The sidetone follows the session rather than the setting alone: a
            // paddle that keyed a tone with no exercise running would be a tone
            // the operator cannot stop without finding the switch.
            let sending = self.session.is_running()
                && self.settings.practice.keying()
                && self.settings.paddle.sidetone;
            stream.publish_paddle(&self.settings.paddle, &self.settings.timing, sending);
        }
    }

    // ------------------------------------------------------------ session

    fn start_session(&mut self) {
        if self.output.is_none() {
            crate::log_warn!("app", "nothing to play through");
            return;
        }
        self.session.start(&self.settings);
    }

    fn stop_session(&mut self) {
        if !self.session.is_running() {
            return;
        }
        self.session.stop(&mut self.settings, &mut self.progress);
        self.stats.invalidate();
        self.pattern.clear();
        self.paddle_held = [None; 2];
        if let Some(stream) = self.output.as_ref() {
            stream.flush();
            // A button held at this moment receives no release the paddle would
            // see, so the contact would stay closed and the tone with it.
            stream.paddle_release();
        }
    }

    /// Drains the picture queues and tells the session what has begun to sound.
    ///
    /// The draining and the notification are two passes because they need the
    /// application differently: draining borrows the stream and writes fields
    /// beside it, which the borrow checker splits, and the notification touches
    /// the session and the settings together.
    fn sync_scope(&mut self) {
        let mut starts: Vec<u64> = Vec::new();

        {
            let stream = match self.output.as_ref() {
                Some(s) => s,
                None => return,
            };

            loop {
                let n = stream.read_scope(&mut self.trace_scratch);
                if n == 0 {
                    break;
                }
                self.scope.push_trace(&self.trace_scratch[..n]);
            }
            self.scope.set_newest(stream.frames());

            loop {
                let n = stream.read_edges(&mut self.edge_scratch);
                if n == 0 {
                    break;
                }
                self.scope.push_edges(&self.edge_scratch[..n]);
                for edge in &self.edge_scratch[..n] {
                    if edge.sync {
                        starts.push(edge.at);
                    }
                }
            }
        }

        if starts.is_empty() {
            return;
        }

        // The sample index becomes a session time. The frame counter says what
        // has been rendered and what is audible is that less the buffer, so a
        // character at index `at` is heard that many samples from now. The
        // endpoint adds a latency this cannot know, which is a constant and
        // therefore cancels in any comparison between two characters.
        let (rate, played) = match self.output.as_ref() {
            Some(s) => (
                s.rate().max(1) as f32,
                s.frames().saturating_sub(s.buffer_frames() as u64),
            ),
            None => return,
        };
        let now = self.session.view(&self.settings).elapsed;

        for at in starts {
            let ahead = at.saturating_sub(played) as f32 / rate;
            if let Some(ch) = self.session.on_character(now + ahead, &self.settings) {
                self.scope.push_label(at, ch);
            }
        }
    }

    /// Advances the session and keeps the interfering station fed.
    fn sync_session(&mut self, dt: f32) {
        // Fields are destructured so the session can be held mutably beside the
        // stream and the history, which are disjoint members.
        let App { session, output, settings, progress, .. } = self;
        if let Some(stream) = output.as_ref() {
            session.update(dt, stream, settings, progress);
        }

        // The session ended on its own, which the update cannot record: the level
        // move needs the settings mutably.
        if self.session.expired() && !self.session.is_running() {
            self.session.finish(&mut self.settings, &mut self.progress);
            self.stats.invalidate();
            if let Some(stream) = self.output.as_ref() {
                stream.flush();
            }
        }

        self.feed_interference();
    }

    /// Keeps the interfering station sending.
    ///
    /// Its own material, and deliberately from the whole alphabet: what makes
    /// interference hard is that it is a plausible signal, and a second station
    /// restricted to the two characters the student has met would be a rhythm the
    /// ear separates at once.
    fn feed_interference(&mut self) {
        if !self.settings.conditions.qrm || !self.session.is_running() {
            return;
        }
        let stream = match self.output.as_ref() {
            Some(s) => s,
            None => return,
        };
        if stream.interference_pending() >= QRM_LOW {
            return;
        }

        let mut lesson = self.settings.lesson.clone();
        lesson.method = crate::config::settings::LessonMethod::Alphabet;
        lesson.level = 36;
        let text = self.qrm_material.next(
            &lesson,
            &self.settings.material,
            None,
            self.settings.progress.window,
            0.0,
        );
        if text.is_empty() {
            return;
        }

        self.qrm_elements.clear();
        // Its own timing, one part in ten faster: two stations at exactly the
        // same speed drift in and out of phase, and the ear then hears one
        // signal that stutters rather than two it has to separate.
        let mut timing = self.settings.timing.clone();
        timing.char_wpm = (timing.char_wpm * 1.1).clamp(5.0, 60.0);
        timing.text_wpm = timing.text_wpm.min(timing.char_wpm);
        self.qrm_keyer
            .encode(&format!(" {}", text), &timing, &mut self.qrm_elements);
        stream.push_interference(&self.qrm_elements);
    }

    /// Plays a note so the pitch and the level can be judged.
    ///
    /// Queued as an element rather than sent through a path of its own, so what is
    /// heard is exactly what a dash sounds like: the same envelope, the same
    /// balance, the same conversion, the same band underneath it.
    fn test_tone(&mut self) {
        if let Some(stream) = self.output.as_ref() {
            stream.push(&[Element {
                on: true,
                seconds: TEST_TONE_S,
                ideal_seconds: TEST_TONE_S,
                sync: true,
            }]);
        }
    }

    // ------------------------------------------------------------- readout

    fn sync_rows(&mut self, dt: f32) {
        self.rows_poll -= dt;
        if self.rows_poll > 0.0 {
            return;
        }
        self.rows_poll = ROWS_POLL_S;
        let set = character_set(&self.settings.lesson);
        self.progress
            .rows(&set, self.settings.progress.window, &mut self.rows);
    }

    // -------------------------------------------------------------- scope

    fn scope_span(&self) -> f32 {
        self.settings.scope.seconds.clamp(0.05, MAX_SECONDS)
    }

    /// Seconds one pixel covers.
    ///
    /// Every grab radius is stated through this rather than in seconds. A radius
    /// in seconds is a radius in pixels that changes with the span, so a cursor
    /// that snapped reliably at four seconds could only be placed by landing
    /// exactly on the edge at thirty.
    fn seconds_per_pixel(&self) -> f32 {
        let width = self
            .gui
            .custom_rect(TAG_SCOPE)
            .map(|r| (r.w - self.gutter_left()).max(1.0))
            .unwrap_or(1.0);
        self.scope_span() / width
    }

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

    /// Converts a position over the reserved area into one on the axis.
    ///
    /// The widget system reports a fraction of the whole rectangle, and the axis
    /// occupies only what the gutters left over. A caller that skipped this would
    /// place a press a gutter width to the left of where it was aimed.
    fn axis_fraction(&self, t: f32) -> f32 {
        let gutter = self.gutter_left();
        if gutter <= 0.0 {
            return t;
        }
        let width = self.gui.custom_rect(TAG_SCOPE).map(|r| r.w).unwrap_or(0.0);
        let usable = width - gutter;
        if usable <= 1.0 {
            return t;
        }
        ((t * width - gutter) / usable).clamp(0.0, 1.0)
    }

    fn apply_span_zoom(&mut self, at: f32, notches: f32) {
        let span = self.scope_span();
        let age = self.scope.age_at(at, span);
        let next = (span * SPAN_PER_NOTCH.powf(-notches)).clamp(0.05, MAX_SECONDS);
        // The anchor is kept under the pointer: the moment being examined must not
        // travel across the picture as the span changes, or the operator loses the
        // element they were looking at.
        self.scope.end = (age - next * (1.0 - at)).max(0.0);
        self.settings.scope.seconds = next;
        self.scope.clamp(next);
    }

    /// One frame of a scope gesture.
    ///
    /// What the press grabbed is latched: recomputing it per frame would hand the
    /// gesture to whichever button was pressed last, so a pan that crossed a
    /// cursor would start moving the cursor instead.
    ///
    /// The snap applies to the press and not to the drag. A press states a
    /// neighbourhood and the picture states where inside it the crossing is;
    /// a drag that snapped would jump away from the pointer.
    fn apply_scope_drag(&mut self, which: ScopeGrab, at: f32, started: bool, shift: bool) {
        if started {
            self.scope_grab = which;
        }
        if self.scope_grab != which {
            return;
        }

        let span = self.scope_span();
        let age = self.scope.age_at(at, span);

        match which {
            ScopeGrab::CursorA | ScopeGrab::CursorB => {
                // Shift inverts the snap rather than switching it off, so both
                // behaviours are reachable in either gesture.
                let snap = started != shift;
                let tolerance = self.seconds_per_pixel() * SNAP_PIXELS;
                self.scope
                    .place(which == ScopeGrab::CursorA, age, snap, tolerance);
            }
            ScopeGrab::Pan => {
                if started {
                    self.scope_pan_anchor = age;
                } else {
                    self.scope.end = (self.scope_pan_anchor - span * (1.0 - at)).max(0.0);
                    self.scope.clamp(span);
                }
            }
            ScopeGrab::None => {}
        }
    }

    // -------------------------------------------------------- diagnostics

    /// Compares the configuration against the one the build would write.
    fn sync_deviations(&mut self, dt: f32) {
        if self.tab != TAB_SETTINGS || !self.settings.ui.show_settings_panel {
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
                None => continue,
            };
            if value != stated {
                self.deviations
                    .push(format!("{}.{} = {}   was {}", section, key, value, stated));
            }
        }

        let total = self.deviations.len();
        if total > MAX_DEVIATIONS {
            self.deviations.truncate(MAX_DEVIATIONS);
            self.deviations.push(format!("and {} more", total - MAX_DEVIATIONS));
        }
    }

    fn side_panel_max(&self) -> f32 {
        let logical = self.surface_size.0 as f32 / self.ui_scale.max(0.1);
        (logical * SIDE_PANEL_MAX_FRACTION)
            .min(SIDE_PANEL_MAX)
            .max(SIDE_PANEL_MIN)
    }

    fn apply_language(&mut self, code: &str) {
        self.settings.ui.language = code.to_string();
        let catalog = Catalog::load(&self.language_dir, code);
        self.gui.set_catalog(catalog);
        crate::log_info!("app", "language set to '{}'", code);
    }

    // --------------------------------------------------------------- loop

    pub fn run(&mut self) -> i32 {
        let mut last_title_update = crate::core::Instant::now();

        while self.running {
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

            // Nothing to draw while minimized; yield instead of spinning. The
            // output keeps playing, which is deliberate: a trainer that stopped
            // because its window was minimized would be one nobody could listen
            // to while reading something else.
            if self.minimized {
                platform::sleep_ms(50);
                continue;
            }

            self.sync_output(dt);
            self.sync_scope();
            // Before the session advances, because a character completed on this
            // frame is an answer the session may be about to score.
            self.sync_keyed();
            self.sync_session(dt);
            self.sync_rows(dt);
            self.sync_deviations(dt);

            {
                let set = character_set(&self.settings.lesson);
                let App { stats, progress, renderer, .. } = self;
                if let Err(e) = stats.refresh(&set, progress, renderer) {
                    crate::log_warn!("app", "cannot refresh the matrix: {}", e);
                }
            }

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

            let target_fps = self.settings.ui.target_fps;
            if target_fps > 0 {
                self.clock.limit(target_fps);
            }

            if last_title_update.elapsed_secs() >= 0.5 {
                last_title_update = crate::core::Instant::now();
                let title = format!(
                    "CWDrill {} - {}x{} - {:.0} fps",
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

    /// Hands a contact to the paddle before the interface sees it.
    ///
    /// ## Where the boundary runs
    ///
    /// Not around the window and not around one region. Around the control: a
    /// press over something that would act on it belongs to the interface, and a
    /// press over anything else belongs to the key.
    ///
    /// Taking the whole window was tried and is wrong, because it takes the close
    /// button, the caption and the transport with it, and the only way out is a
    /// key nothing on the screen mentions. Taking one region is wrong the other
    /// way: a paddle that stopped working because the pointer had drifted would
    /// be a key that fails for a reason nothing states.
    ///
    /// The control is the boundary that survives both. A pointer nobody is moving
    /// stays where it was, so a paddle wired across the switches keeps working;
    /// a pointer moved onto a button is a pointer aimed at a button, and the
    /// button is lit under it.
    ///
    /// Escape ends the session and gives the buttons back, and the status bar
    /// says so while they are taken.
    fn route_paddle(&mut self, ev: &Event) -> bool {
        // A release is honoured whatever has happened since the press, including
        // the session ending underneath it: the contact was closed by this
        // button, so it has to be opened by it.
        if let Event::MouseButton { button, pressed: false, .. } = *ev {
            if let Some(index) = mouse_index(button) {
                let held = self.paddle_held[index].take();
                if let Some(dit) = held {
                    if let Some(stream) = self.output.as_ref() {
                        if dit {
                            stream.paddle_dit(false);
                        } else {
                            stream.paddle_dah(false);
                        }
                    }
                    return true;
                }
            }
        }

        if !self.session.is_running() || !self.settings.practice.keying() {
            return false;
        }
        // A list is modal and a field is taking characters. Either one is the
        // operator using the interface rather than the key, and a contact that
        // swallowed the press would leave them unable to leave.
        if self.gui.popup_open() || self.gui.wants_keyboard() {
            return false;
        }
        if self.output.is_none() {
            return false;
        }

        let source = self.settings.paddle.source;
        let mouse = matches!(source, PaddleSource::Mouse | PaddleSource::Both);
        let keyboard = matches!(source, PaddleSource::Keyboard | PaddleSource::Both);
        let swap = self.settings.paddle.swap;
        let straight = self.settings.paddle.mode == PaddleMode::Straight;

        match *ev {
            Event::MouseButton { button, pressed: true, x, y, .. } if mouse => {
                self.close_contact(button, x, y, swap, straight)
            }
            // A rapid second tap arrives as a double click *instead of* a press,
            // because the window class asks for them. Swallowing it silently was
            // the whole of the reported fault: every other tap of a fast run
            // closed no contact at all and vanished.
            //
            // Treated as the press it stands for. The release that follows is an
            // ordinary one and finds the latch this set.
            Event::MouseDoubleClick { button, x, y } if mouse => {
                self.close_contact(button, x, y, swap, straight)
            }
            Event::Key { key: Key::Letter(code), pressed, repeat, .. } if keyboard => {
                // A repeat is the keyboard retriggering a key that is already
                // down, which is not a second closure of the contact. Swallowed
                // rather than passed on, because the contact is closed and the
                // interface must not act on a key the operator is holding as a
                // lever.
                if repeat {
                    return true;
                }
                let stream = match self.output.as_ref() {
                    Some(s) => s,
                    None => return false,
                };
                if code == PaddleSettings::letter(&self.settings.paddle.key_dit) {
                    stream.paddle_dit(pressed);
                    true
                } else if code == PaddleSettings::letter(&self.settings.paddle.key_dah) {
                    stream.paddle_dah(pressed);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Closes whichever contact a press at this position means.
    ///
    /// False when the press belongs to the interface, which is a press over a
    /// control: the boundary runs round the widget rather than round the window,
    /// so the close button and the transport keep working while the key has the
    /// rest.
    fn close_contact(
        &mut self,
        button: MouseButton,
        x: f32,
        y: f32,
        swap: bool,
        straight: bool,
    ) -> bool {
        let index = match mouse_index(button) {
            Some(i) => i,
            // The middle button still pans the picture, which is a thing an
            // operator does between characters rather than during one.
            None => return false,
        };
        let dit = match self.lever_at(x, y) {
            // Over the drawn levers the position decides the contact rather than
            // the button, because that is what pointing at a lever means. It is
            // also the only way somebody with one button can reach both.
            Some(left) => straight || (left != swap),
            None => {
                if self.gui.pointer_over_widget() {
                    return false;
                }
                match button {
                    MouseButton::Left => !swap,
                    MouseButton::Right => swap,
                    _ => return false,
                }
            }
        };

        // A second press on a button already held is the double click arriving
        // after its own press, which closes nothing new.
        if self.paddle_held[index] == Some(dit) {
            return true;
        }
        self.paddle_held[index] = Some(dit);
        if let Some(stream) = self.output.as_ref() {
            if dit {
                stream.paddle_dit(true);
            } else {
                stream.paddle_dah(true);
            }
        }
        true
    }

    /// Which drawn lever a position is over, the left one being true.
    ///
    /// The geometry comes from the previous frame, which is the same lag every
    /// hit test in this application carries.
    fn lever_at(&self, x: f32, y: f32) -> Option<bool> {
        let rect = self.gui.custom_rect(TAG_PADDLE)?;
        if rect.is_empty() || !rect.contains(x, y) {
            return None;
        }
        Some(x < rect.x + rect.w * 0.5)
    }

    /// Drains what the paddle produced and assembles characters from it.
    ///
    /// The classification arrives rather than a duration, because the machine
    /// that decided the element is the one that knows what it was; a second
    /// threshold here could disagree with the first and would then report a
    /// character the operator did not send.
    fn sync_keyed(&mut self) {
        // Elements keyed towards a character that was never finished belong to
        // the answer they were keyed into. Carrying them over would make the
        // first character of the next group begin with the tail of the last.
        let epoch = self.session.answer_epoch();
        if epoch != self.answer_epoch {
            self.answer_epoch = epoch;
            self.pattern.clear();
        }

        loop {
            let read = match self.output.as_ref() {
                Some(s) => s.read_keyed(&mut self.keyed_scratch),
                None => return,
            };
            if read == 0 {
                return;
            }
            for index in 0..read {
                match self.keyed_scratch[index] {
                    Keyed::Dit => self.pattern.push('.'),
                    Keyed::Dah => self.pattern.push('-'),
                    Keyed::CharGap => self.flush_pattern(),
                    Keyed::WordGap => {
                        self.flush_pattern();
                        let App { session, settings, .. } = self;
                        session.on_keyed(' ', settings);
                    }
                }
            }
        }
    }

    fn flush_pattern(&mut self) {
        if self.pattern.is_empty() {
            return;
        }
        match crate::morse::char_of(&self.pattern) {
            Some(ch) => {
                let App { session, settings, .. } = self;
                session.on_keyed(ch, settings);
            }
            None => self.sent_malformed += 1,
        }
        self.pattern.clear();
    }

    fn handle_event(&mut self, ev: Event) {
        // Before the interface, see the note on the routing.
        if self.route_paddle(&ev) {
            return;
        }
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

            Event::Minimized(state) => self.minimized = state,

            Event::DpiChanged { scale } => {
                self.dpi_scale = scale;
                self.ui_scale = scale * self.settings.ui.scale;
            }

            Event::Text(ch) => {
                // Typed characters reach the session only while nothing in the
                // interface is taking them. A path being edited would otherwise
                // be scored as an answer.
                if self.session.is_running() && !self.gui.wants_keyboard() {
                    self.session.on_char(ch, &self.settings);
                }
            }

            Event::Key { key, pressed, .. } => {
                if !pressed {
                    return;
                }
                // The session takes precedence over the shortcuts while it is
                // running, because the keys it wants are the keys a student is
                // pressing: enter and backspace are answers rather than commands.
                if self.session.is_running() && !self.gui.wants_keyboard() {
                    match key {
                        Key::Enter => {
                            let App { session, settings, progress, .. } = self;
                            session.on_submit(settings, progress);
                            self.stats.invalidate();
                            return;
                        }
                        Key::Backspace => {
                            self.session.on_backspace(&self.settings);
                            return;
                        }
                        Key::Escape => {
                            self.stop_session();
                            return;
                        }
                        _ => {}
                    }
                }

                if self.gui.wants_keyboard() || self.gui.popup_open() {
                    return;
                }
                match key {
                    Key::F(1) => {
                        self.settings.ui.show_debug_overlay = !self.settings.ui.show_debug_overlay;
                    }
                    Key::F(2) => {
                        self.settings.ui.vsync = !self.settings.ui.vsync;
                        self.renderer.set_vsync(self.settings.ui.vsync);
                    }
                    Key::F(3) => {
                        self.settings.ui.show_settings_panel =
                            !self.settings.ui.show_settings_panel;
                    }
                    Key::F(5) => self.start_output(),
                    Key::Space if !self.gui.wants_activation() => {
                        if self.session.is_running() {
                            self.stop_session();
                        } else {
                            self.start_session();
                        }
                    }
                    Key::Digit(d) if (1..=5).contains(&d) => {
                        self.tab = (d - 1) as usize;
                        self.settings.ui.show_settings_panel = true;
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }

    fn build_frame(&mut self) {
        let (w, h) = (self.surface_size.0 as f32, self.surface_size.1 as f32);
        let white = self.renderer.white_texture();
        self.draw_list.begin(w, h, white);

        self.ui_scale = self.dpi_scale * self.settings.ui.scale;
        self.accent = Color::hex(self.settings.ui.accent_rgb);
        self.gui.theme.apply(self.accent, &self.settings.appearance);

        let scale = self.ui_scale;
        let ui_px = (self.settings.ui.font_size_pt * PT_TO_PX * scale).round().max(8.0);
        let prompt_px = (self.settings.ui.prompt_font_size_pt * PT_TO_PX * scale)
            .round()
            .max(10.0);
        let time = self.time;

        // The stored width is clamped every frame rather than only on a drag: a
        // window that shrank, or a display factor that grew, can put a value
        // written earlier outside what the current geometry allows.
        let side_max = self.side_panel_max();
        let side_min = SIDE_PANEL_MIN.max(self.gui.panel_demand().min(side_max));
        self.settings.ui.side_panel_width =
            self.settings.ui.side_panel_width.clamp(side_min, side_max);

        let span = self.scope_span();
        self.scope.clamp(span);

        self.gui.starts_frame(Rect::new(0.0, 0.0, w, h), scale, ui_px, prompt_px, time);
        self.gui.set_panel_width(self.settings.ui.side_panel_width);

        let stats = self.renderer.stats();
        let window_maximized = self.window.is_maximized();
        let language_index = self
            .languages
            .iter()
            .position(|l| l.as_str() == self.settings.ui.language.as_str())
            .unwrap_or(0);

        let measurement = self.scope.measurement();
        let dot_ms = self.settings.timing.dot_seconds() * 1000.0;
        let history = self.scope.history_seconds();
        let set = character_set(&self.settings.lesson);
        let set_accuracy = self
            .progress
            .set_accuracy(&set, self.settings.progress.window);
        let weakest = self.progress.weakest(&set, self.settings.progress.window);
        let view = self.session.view(&self.settings);
        let pool = self.session.pool();
        let matrix_peak = self.stats.peak();
        let sessions = self.progress.sessions();
        let sessions_file = self
            .progress
            .sessions_path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut tab = self.tab;
        let mut editing_tab = self.editing_tab;
        let mut add_section = self.add_section;
        let mut sel = self.sel;
        let mut cmd = UiCommands::default();
        let vsync_before = self.settings.ui.vsync;

        {
            let App {
                gui,
                fonts,
                settings,
                gpu_name,
                font_name,
                languages,
                devices,
                deviations,
                clock,
                output_status,
                scope,
                rows,
                output_recoveries,
                sent_malformed,
                ..
            } = self;
            let language_names: Vec<&str> = languages.iter().map(|s| s.as_str()).collect();
            let device_names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();

            let status = StatusInfo {
                fps: clock.fps(),
                worst_ms: clock.worst_frame_ms(),
                draw_calls: stats.draw_calls,
                uploads: stats.uploads,
                gpu_ms: stats.gpu_ms,
                gpu_worst_ms: stats.gpu_worst_ms,
                glyphs: fonts.cached_glyphs(),
                atlas: fonts.atlas_used(),
                gpu: gpu_name.as_str(),
                font: font_name.as_str(),
                dpi: scale / settings.ui.scale.max(0.1),
                languages: language_names.as_slice(),
                language_index,
                devices: device_names.as_slice(),
                audio: output_status,
                audio_recoveries: *output_recoveries,
                session: &view,
                character_pool: pool.as_str(),
                set_accuracy,
                weakest,
                rows: rows.as_slice(),
                matrix_peak,
                sessions,
                sessions_file: sessions_file.as_str(),
                sent_malformed: *sent_malformed,
                scope_history_s: history,
                scope_end_s: scope.end,
                cursor_a: scope.cursor_a.map(|c| c.age),
                cursor_b: scope.cursor_b.map(|c| c.age),
                measurement_s: measurement,
                dot_ms,
                deviations: deviations.as_slice(),
                side_panel_min: side_min,
                side_panel_max: side_max,
                height: h,
                window_maximized,
            };

            let mut frame = Frame::new(gui, fonts);
            panel::build(
                &mut frame,
                settings,
                &status,
                &mut sel,
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

        // Three passes, in this order and for one reason. The reserved areas are
        // opaque and are drawn by the application rather than by the widget
        // system, so they have to land between the tree that reserved them and
        // the layer that must cover everything: a list is modal, and a modal
        // element that cannot be seen still owns the pointer.
        {
            let App { gui, fonts, draw_list, .. } = self;
            gui.draw_tree(fonts, draw_list);
        }

        self.draw_reserved_areas(prompt_px, &view);

        {
            let App { gui, fonts, draw_list, .. } = self;
            gui.draw_top(fonts, draw_list);
        }

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
        if cmd.stop_session {
            self.stop_session();
        }
        if cmd.start_session {
            self.start_session();
        }
        if cmd.skip_group {
            self.session.skip();
        }
        if cmd.repeat_group {
            let App { session, settings, .. } = self;
            session.again(settings);
        }
        if cmd.submit {
            let App { session, settings, progress, .. } = self;
            session.on_submit(settings, progress);
            self.stats.invalidate();
        }
        if cmd.test_tone {
            self.test_tone();
        }
        if cmd.rescan_devices {
            self.rescan_devices();
        }
        if cmd.select_device {
            self.apply_device_selection();
            cmd.restart_output = true;
        }
        if cmd.restart_output {
            self.start_output();
        }
        if cmd.stop_output {
            self.stop_output();
        }
        if cmd.reset_progress {
            self.progress.clear();
            if let Err(e) = self.progress.save() {
                crate::log_warn!("app", "cannot save the history: {}", e);
            }
            self.stats.invalidate();
            self.rows_poll = 0.0;
        }
        if cmd.clear_marks {
            self.scope.clear_cursors();
        }
        if cmd.scope_live {
            self.scope.end = 0.0;
        }
        if let Some(code) = cmd.language {
            self.apply_language(&code);
        }
        if let Some(level) = cmd.log_level {
            crate::core::log::set_level(level);
        }
        if let Some(index) = cmd.reset_tab {
            self.settings.panel.reset_tab(index);
        }
        if cmd.frame_changed {
            self.window
                .set_custom_frame(self.settings.appearance.custom_frame);
        }
        if cmd.window_close {
            self.window.close();
        } else if cmd.window_minimize {
            self.window.minimize();
        } else if cmd.window_toggle_max {
            self.window.toggle_maximize();
        } else if cmd.window_drag {
            // The system move loop takes the capture and consumes the release, so
            // the interface would otherwise carry a held button into the next
            // frame and a widget would act on a press that has already ended.
            self.window.begin_move();
            self.gui.release_pointer();
        }

        if let Some((at, notches)) = cmd.scope_zoom {
            self.apply_span_zoom(self.axis_fraction(at), notches);
        }
        if let Some(drag) = cmd.scope_drag {
            let which = match drag.button {
                MouseButton::Left => ScopeGrab::CursorA,
                MouseButton::Right => ScopeGrab::CursorB,
                _ => ScopeGrab::Pan,
            };
            self.apply_scope_drag(
                which,
                self.axis_fraction(drag.fraction),
                drag.started,
                drag.shift,
            );
        } else {
            self.scope_grab = ScopeGrab::None;
        }
        if cmd.scope_clear_gesture {
            self.scope.clear_cursors();
        }
    }

    // ---------------------------------------------------------- rendering

    fn draw_reserved_areas(&mut self, prompt_px: f32, view: &crate::session::SessionView) {
        let scope_rect = self.gui.custom_rect(TAG_SCOPE);
        let prompt_rect = self.gui.custom_rect(TAG_PROMPT);
        let heatmap_rect = self.gui.custom_rect(TAG_HEATMAP);
        let paddle_rect = self.gui.custom_rect(TAG_PADDLE);
        let decoder_rect = self.gui.custom_rect(TAG_DECODER);
        // Copied because the drawing borrows the application, and the elements
        // keyed so far are a field of it.
        let pattern = self.pattern.clone();
        let scale = self.ui_scale;

        let look = DataLook {
            scale,
            label_px: (prompt_px * 0.40).round().max(8.0),
            accent: self.accent,
            text: self.gui.theme.text,
            dim: self.gui.theme.text_dim,
            faint: self.gui.theme.text_faint,
            grid_major: self.gui.theme.grid_major,
            grid_minor: self.gui.theme.grid_minor,
            background: self.gui.theme.data_background,
            ideal: Color::hex(0xE0A040),
            cursor: Color::hex(0xF0F0F0),
            right: Color::hex(0x5FBF6A),
            wrong: Color::hex(0xD05050),
            missed: Color::hex(0x7A7A80),
            major_every: self.settings.appearance.grid_major_every,
            trace_fill: self.settings.appearance.trace_fill,
            trace_fill_alpha: self.settings.appearance.trace_fill_alpha,
            trace_thickness: self.settings.appearance.trace_thickness,
        };

        // Built here because the drawing has no catalogue: everything below the
        // declaration takes the application apart and the wording lives in the
        // half that is left behind.
        let hud = self.settings.appearance.hud;
        let hud_width = self.settings.appearance.hud_width;
        let hud_height = self.settings.appearance.hud_height;
        let hud_opacity = self.settings.appearance.hud_opacity;
        let hud_keying = self.settings.practice.keying();
        let hud_header = if hud && view.running {
            let mut text = String::with_capacity(64);
            let drill = match view.drill {
                DrillMode::Off => "",
                DrillMode::Recall => "drill.recall",
                DrillMode::Echo => "drill.echo",
                DrillMode::Blind => "drill.blind",
            };
            if !drill.is_empty() {
                text.push_str(self.gui.tr(drill));
                text.push_str("   ");
            }
            text.push_str(self.gui.tr(view.phase));
            text.push_str(&format!("   {:.0} s", view.remaining));
            if view.characters > 0 {
                text.push_str(&format!("   {:.0} %   {}", view.accuracy * 100.0, view.characters));
            }
            if view.copies > 1 {
                text.push_str(&format!("   {} / {}", view.copy, view.copies));
            }
            text
        } else {
            String::new()
        };

        // Everything the panel below used to carry beside the text: the fields
        // an exchange asks for, what a token means once it has been answered,
        // and why a source produced something other than what it promises.
        let hud_context = if hud && view.running {
            let mut text = String::with_capacity(96);
            if view.exchange {
                for field in &view.fields {
                    if !text.is_empty() {
                        text.push_str("   ");
                    }
                    text.push_str(self.gui.tr(field.key));
                }
            }
            // After the answer rather than before it. A gloss before the answer
            // would be the answer.
            if view.scored {
                if let Some(gloss) = view.gloss {
                    if !text.is_empty() {
                        text.push_str("   ");
                    }
                    text.push_str(gloss);
                }
            }
            if let Some(key) = view.fallback {
                if !text.is_empty() {
                    text.push_str("   ");
                }
                text.push_str(self.gui.tr(key));
            }
            text
        } else {
            String::new()
        };

        let gutter_left = self.gutter_left();
        let gutter_bottom = self.gutter_bottom();
        let span = self.scope_span();
        let dot_seconds = self.settings.timing.dot_seconds();
        let unit_grid = self.settings.scope.unit_grid;
        let show_ideal = self.settings.scope.show_ideal;
        let show_labels = self.settings.scope.show_labels;
        let show_timing = self.settings.scope.show_timing;

        let older = self.scope.end + span;
        let newer = self.scope.end;
        {
            let App { scope, visible_edges, visible_labels, ideal_marks, .. } = self;
            scope.edges_between(older, newer, visible_edges);
            if show_labels {
                scope.labels_between(older, newer, visible_labels);
            } else {
                visible_labels.clear();
            }
            if show_ideal {
                scope.ideal_marks(ideal_marks);
            } else {
                ideal_marks.clear();
            }
        }

        // Read from the machine rather than from what was last written to it:
        // the two would agree, and a second copy of one fact is a second chance
        // for them not to.
        let (dit_down, dah_down) = self
            .output
            .as_ref()
            .map(|s| s.paddle_contacts())
            .unwrap_or((false, false));
        let (live_mark, live_unit) = self
            .output
            .as_ref()
            .map(|s| s.paddle_progress())
            .unwrap_or((0, 1));
        let paddle_live = self.session.is_running() && self.settings.practice.keying();
        let paddle_mode = self.settings.paddle.mode;
        let paddle_swap = self.settings.paddle.swap;
        let paddle_keys = matches!(
            self.settings.paddle.source,
            PaddleSource::Keyboard | PaddleSource::Both
        );
        let letter = |text: &str| -> Option<char> {
            match PaddleSettings::letter(text) {
                0 => None,
                code => Some(code as char),
            }
        };
        let key_dit = if paddle_keys { letter(&self.settings.paddle.key_dit) } else { None };
        let key_dah = if paddle_keys { letter(&self.settings.paddle.key_dah) } else { None };

        let idle = self.scope.is_empty();
        let waiting = self.gui.tr("status.no_material").to_string();
        let matrix = self.stats.matrix();
        let matrix_extent = self.stats.extent();
        let matrix_chars: Vec<char> = self.stats.chars().to_vec();
        let matrix_peak = self.stats.peak();
        let rows: Vec<CharRow> = self.rows.clone();

        let App { fonts, draw_list, scope, visible_edges, visible_labels, ideal_marks, .. } =
            self;

        if let Some(full) = scope_rect {
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
                // Three lanes rather than one. The envelope states the shape and
                // spends most of the height carrying nothing; the ribbon says
                // what each burst was, which a picture read after the fact
                // cannot otherwise answer, and the lane below says how far each
                // element was from the length it should have had.
                let ribbon = if show_labels {
                    (look.label_px + look.scale * 6.0).min(data.h * 0.2)
                } else {
                    0.0
                };
                let lane = if show_timing {
                    (data.h * 0.26).min(look.scale * 64.0)
                } else {
                    0.0
                };
                let trace = Rect::from_min_max(
                    data.x,
                    data.y + ribbon,
                    data.right(),
                    data.bottom() - lane,
                );

                App::draw_unit_grid(
                    draw_list, fonts, data, dot_seconds, span, scope.end, unit_grid,
                    gutter_bottom, &look,
                );
                App::draw_envelope(draw_list, trace, scope, span, &look);
                App::draw_characters(draw_list, trace, scope, span, visible_edges, &look);
                if ribbon > 0.0 {
                    App::draw_labels(
                        draw_list,
                        fonts,
                        Rect::new(data.x, data.y, data.w, ribbon),
                        scope,
                        span,
                        visible_labels,
                        &look,
                    );
                }
                if show_ideal {
                    App::draw_ideal(draw_list, trace, scope, span, ideal_marks, &look);
                }
                if lane > 0.0 {
                    App::draw_timing(
                        draw_list,
                        fonts,
                        Rect::from_min_max(data.x, data.bottom() - lane, data.right(), data.bottom()),
                        scope,
                        span,
                        visible_edges,
                        dot_seconds,
                        &look,
                    );
                }
                App::draw_cursors(draw_list, fonts, data, scope, span, &look);

                if idle {
                    let w = fonts.measure(&waiting, FontId::Ui, look.label_px);
                    fonts.draw_text(
                        draw_list,
                        (data.x + (data.w - w) * 0.5).round(),
                        (data.y + data.h * 0.4).round(),
                        &waiting,
                        FontId::Ui,
                        look.label_px,
                        look.faint,
                    );
                }

                // Last, so nothing is drawn over it: it is opaque and it is the
                // one thing on this surface that is read rather than examined.
                if hud && view.running {
                    App::draw_hud(
                        draw_list, fonts, data, view, &hud_header, &hud_context, &pattern,
                        hud_keying, prompt_px, hud_width, hud_height, hud_opacity, &look,
                    );
                }
            }
            draw_list.pop_clip();
        }

        if let Some(full) = prompt_rect {
            App::draw_prompt(draw_list, fonts, full, view, prompt_px, &look);
        }

        if let Some(full) = decoder_rect {
            App::draw_decoder(
                draw_list, fonts, full, view, &pattern, live_mark, live_unit, prompt_px,
                &look,
            );
        }

        if let Some(full) = heatmap_rect {
            App::draw_stats(
                draw_list, fonts, full, &rows, matrix, matrix_extent, &matrix_chars,
                matrix_peak, &look,
            );
        }

        if let Some(full) = paddle_rect {
            App::draw_paddle(
                draw_list, fonts, full, paddle_mode, paddle_swap, dit_down, dah_down,
                paddle_live, key_dit, key_dah, &look,
            );
        }
    }

    /// The two levers of the key.
    ///
    /// Filled while the contact is closed and outlined while it is open, which
    /// is the whole of what the operator cannot otherwise find out: a paddle that
    /// is not reaching the machine and a paddle that is reaching it look
    /// identical everywhere else on the screen.
    ///
    /// The mark inside each lever is the element that lever sends, and it follows
    /// the swap rather than the wiring convention. A picture that showed the
    /// convention would be a second thing to remember instead of a reminder.
    #[allow(clippy::too_many_arguments)]
    fn draw_paddle(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        mode: PaddleMode,
        swap: bool,
        dit: bool,
        dah: bool,
        live: bool,
        key_dit: Option<char>,
        key_dah: Option<char>,
        look: &DataLook,
    ) {
        list.push_clip(r);
        list.fill_rect(r, look.background);

        let pad = (look.scale * 6.0).round();
        let inner = r.inset(pad);
        if inner.is_empty() {
            list.pop_clip();
            return;
        }

        if mode == PaddleMode::Straight {
            // One contact, so one lever. Two would invite the operator to press
            // them in turn and wonder why the result is the same.
            App::draw_lever(list, fonts, inner, true, dit || dah, live, key_dit, look);
        } else {
            let gap = (look.scale * 6.0).round();
            let half = ((inner.w - gap) * 0.5).floor().max(1.0);
            let left = Rect::new(inner.x, inner.y, half, inner.h);
            let right = Rect::new(inner.right() - half, inner.y, half, inner.h);
            App::draw_lever(
                list, fonts, left, swap, if swap { dah } else { dit }, live,
                if swap { key_dah } else { key_dit }, look,
            );
            App::draw_lever(
                list, fonts, right, !swap, if swap { dit } else { dah }, live,
                if swap { key_dit } else { key_dah }, look,
            );
        }

        list.pop_clip();
    }

    /// One lever, with the element it sends drawn inside it.
    #[allow(clippy::too_many_arguments)]
    fn draw_lever(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        dash: bool,
        closed: bool,
        live: bool,
        key: Option<char>,
        look: &DataLook,
    ) {
        let border = (look.scale * 1.5).max(1.0);
        let edge = if !live {
            look.faint
        } else if closed {
            look.accent
        } else {
            look.dim
        };
        if closed {
            // Weaker when the key is not taking contacts, so a press that reaches
            // the machine and a press that would have is told apart.
            list.fill_rect(r, look.accent.with_alpha(if live { 0.80 } else { 0.25 }));
        }
        list.stroke_rect(r, border, edge);

        let unit = (r.h * 0.16).max(look.scale * 3.0).round();
        let width = if dash { unit * 3.0 } else { unit };
        let mark = Rect::new(
            (r.x + (r.w - width) * 0.5).round(),
            (r.y + (r.h - unit) * 0.5).round(),
            width,
            unit,
        );
        list.fill_rect(mark, if closed { look.background } else { edge });

        // The letter, only when the keyboard is one of the sources. Under the
        // mark rather than beside it, so a narrow panel loses the letter before
        // it loses the element.
        if let Some(ch) = key {
            let mut buffer = [0u8; 4];
            let text = ch.encode_utf8(&mut buffer);
            let w = fonts.measure(text, FontId::Mono, look.label_px);
            if w < r.w {
                fonts.draw_text(
                    list,
                    (r.x + (r.w - w) * 0.5).round(),
                    (r.bottom() - look.scale * 4.0).round(),
                    text,
                    FontId::Mono,
                    look.label_px,
                    if closed { look.background } else { look.faint },
                );
            }
        }
    }

    /// Prompt, answer and score, over the keying picture.
    ///
    /// ## Why it is here rather than on a panel
    ///
    /// It is read while the ear is busy, and a figure behind a fold is a figure
    /// consulted after the group instead of during it.
    ///
    /// ## The arrangement
    ///
    /// The session figures on one line across the top. Below them the material,
    /// large and on the right, because it is the thing being answered and the
    /// eye has to find it without searching. The pattern sits on the left at the
    /// same height, with what has been keyed under it, and the typed answer runs
    /// along the bottom of that column.
    ///
    /// ## Sizing
    ///
    /// The material is sized to fill the band it occupies rather than drawn at a
    /// stated size and shrunk when it does not fit. The face is monospaced, so
    /// both the height and the width scale exactly with the size, and the
    /// largest size that fits follows from two probes instead of a search. A
    /// single character therefore fills the frame, and a callsign lands inside
    /// the same box without either of them moving it.
    #[allow(clippy::too_many_arguments)]
    fn draw_hud(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        area: Rect,
        view: &crate::session::SessionView,
        header: &str,
        context: &str,
        keyed: &str,
        keying: bool,
        px: f32,
        fraction: f32,
        height_fraction: f32,
        opacity: f32,
        look: &DataLook,
    ) {
        if area.is_empty() {
            return;
        }

        let pad = (look.scale * 11.0).round();
        let gap = (look.scale * 7.0).round();
        let width = (area.w * fraction.clamp(0.2, 1.0)).min(area.w - pad * 2.0);
        let height = (area.h * height_fraction.clamp(0.15, 0.9)).min(area.h - pad * 2.0);
        // Below this there is nothing a frame could state that the panel does
        // not state better, and a box of forty pixels reads as a defect.
        if width < look.scale * 150.0 || height < look.scale * 56.0 {
            return;
        }

        let r = Rect::new(
            (area.right() - pad - width).round(),
            (area.y + pad).round(),
            width.round(),
            height.round(),
        );
        list.fill_rect(r, look.background.with_alpha(opacity.clamp(0.2, 1.0)));
        list.stroke_rect(r, (look.scale * 1.5).max(1.0), look.accent);
        list.push_clip(r);

        let inner = r.inset(pad);
        let head_px = (inner.h * 0.12).clamp(look.label_px, px * 0.7);
        let head_h = fonts.line_height(FontId::Ui, head_px);
        fonts.draw_text(
            list,
            inner.x.round(),
            (inner.y + fonts.metrics(FontId::Ui, head_px).ascent).round(),
            header,
            FontId::Ui,
            head_px,
            look.dim,
        );
        let mut top = inner.y + head_h;

        // A line of its own, and only when there is something to say. Three
        // figures and a sentence read badly on one row, and an empty second row
        // would take height from the material for nothing.
        if !context.is_empty() {
            fonts.draw_text(
                list,
                inner.x.round(),
                (top + fonts.metrics(FontId::Ui, head_px).ascent).round(),
                context,
                FontId::Ui,
                head_px,
                look.faint,
            );
            top += head_h;
        }

        let body = Rect::from_min_max(inner.x, top + gap, inner.right(), inner.bottom());
        if body.h < look.scale * 26.0 {
            list.pop_clip();
            return;
        }

        // The material, right hand side, as large as the band allows.
        let target = if view.target_hidden {
            "?".to_string()
        } else if !view.target.is_empty() {
            view.target.clone()
        } else {
            view.sent.clone()
        };
        let chars: Vec<char> = target.chars().collect();
        let mut size = 0.0f32;
        let mut advance = 0.0f32;
        let mut target_w = 0.0f32;
        if !chars.is_empty() {
            // One probe of each dimension. A monospaced face scales linearly in
            // both, so the two ratios give the fitting size exactly.
            const PROBE: f32 = 100.0;
            let per_height = fonts.line_height(FontId::Mono, PROBE) / PROBE;
            let per_width = fonts.measure(&target, FontId::Mono, PROBE) / PROBE;
            let by_height = body.h * 0.94 / per_height.max(0.01);
            // Just over half the band, so the pattern and the answer keep a
            // column of their own however short the material is.
            let by_width = body.w * 0.56 / per_width.max(0.01);
            size = by_height.min(by_width).clamp(look.label_px, px * 5.0);
            advance = fonts.measure("0", FontId::Mono, size).max(1.0);
            target_w = advance * chars.len() as f32;
        }

        if size > 0.0 {
            let base = fonts.baseline_centered(FontId::Mono, size, body.y, body.h);
            let start = (body.right() - target_w).round();
            let plain = if view.target_hidden { look.ideal } else { look.text };
            let mut buffer = [0u8; 4];
            // The verdicts index sounding characters and the text holds spaces
            // as well, so the two are walked with separate cursors.
            let mut mark = 0usize;
            for (index, &ch) in chars.iter().enumerate() {
                let colour = if ch == ' ' {
                    look.dim
                } else {
                    let verdict = view.marks.get(mark).copied();
                    mark += 1;
                    match verdict {
                        Some(Mark::Right) => look.right,
                        Some(Mark::Wrong) => look.wrong,
                        Some(Mark::Missed) => look.missed,
                        Some(Mark::Unscored) => look.dim,
                        None => plain,
                    }
                };
                fonts.draw_text(
                    list,
                    (start + advance * index as f32).round(),
                    base,
                    ch.encode_utf8(&mut buffer),
                    FontId::Mono,
                    size,
                    colour,
                );
            }

            // Where the answer has reached. A word states how much is left by
            // its own length and says nothing about which character is due.
            if !view.scored && !view.target_hidden {
                let typed = view.typed.chars().filter(|&c| c != ' ').count();
                let mut remaining = typed;
                let mut at: Option<usize> = None;
                for (index, &ch) in chars.iter().enumerate() {
                    if ch == ' ' {
                        continue;
                    }
                    if remaining == 0 {
                        at = Some(index);
                        break;
                    }
                    remaining -= 1;
                }
                if let Some(index) = at {
                    list.fill_rect(
                        Rect::new(
                            (start + advance * index as f32).round(),
                            (base + look.scale * 4.0).round(),
                            advance.round().max(1.0),
                            (look.scale * 2.5).round().max(1.0),
                        ),
                        look.accent,
                    );
                }
            }
        }

        let left = Rect::from_min_max(
            body.x,
            body.y,
            (body.right() - target_w - gap * 2.0).max(body.x),
            body.bottom(),
        );
        if left.w < look.scale * 34.0 {
            list.pop_clip();
            return;
        }

        // The pattern only where there is a key to compare it against. In the
        // blind exercise the answer is typed, so a row of elements would be a
        // diagnostic of an instrument nobody is holding.
        let answer = if keying {
            let rows = Rect::new(left.x, left.y, left.w, left.h * 0.62);
            App::draw_hud_pattern(list, fonts, rows, view, keyed, look);
            Rect::from_min_max(left.x, rows.bottom(), left.right(), left.bottom())
        } else {
            left
        };

        let typed: Vec<char> = view.typed.chars().collect();
        if answer.h > look.scale * 12.0 {
            const PROBE: f32 = 100.0;
            let per_height = fonts.line_height(FontId::Mono, PROBE) / PROBE;
            let input_px =
                (answer.h * 0.80 / per_height.max(0.01)).clamp(look.label_px, px * 1.6);
            let step_x = fonts.measure("0", FontId::Mono, input_px).max(1.0);
            let holds = (answer.w / step_x) as usize;
            // The tail rather than the head: the newest characters are the ones
            // being written, and a line drawn from the start pushes them off.
            let head = typed.len().saturating_sub(holds.max(1));
            let text: String = typed[head..].iter().collect();
            let base = fonts.baseline_centered(FontId::Mono, input_px, answer.y, answer.h);
            fonts.draw_text(
                list,
                answer.x.round(),
                base,
                &text,
                FontId::Mono,
                input_px,
                look.accent,
            );
            if !view.scored {
                let metrics = fonts.metrics(FontId::Mono, input_px);
                list.vline(
                    (answer.x + step_x * (typed.len() - head) as f32).round(),
                    base - metrics.ascent,
                    base + metrics.descent,
                    (look.scale * 1.5).max(1.0),
                    look.accent.with_alpha(0.6),
                );
            }
        }

        list.pop_clip();
    }

    /// The pattern being asked for, above what has been keyed.
    ///
    /// Withheld in the recall exercise, where the pattern is the answer and
    /// showing it would be showing the answer. A question mark stands in for it,
    /// so the slot is plainly a thing being withheld rather than a thing that
    /// failed to draw.
    fn draw_hud_pattern(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        view: &crate::session::SessionView,
        keyed: &str,
        look: &DataLook,
    ) {
        if r.is_empty() {
            return;
        }
        let expected = view.next_char.and_then(crate::morse::pattern_of).unwrap_or("");
        if expected.is_empty() && keyed.is_empty() && !view.pattern_hidden {
            return;
        }
        let upper = Rect::new(r.x, r.y, r.w, r.h * 0.5);
        let lower = Rect::from_min_max(r.x, upper.bottom(), r.right(), r.bottom());

        // Sized from the height first and cut back only when the row would run
        // past the edge. Taken from the width, a two element character occupies
        // a quarter of the band and reads as a mark in the corner of an empty
        // box rather than as the pattern the box is for.
        //
        // Four and a bit units per column: a dash is three and the gap after it
        // is one, so nothing overlaps and the last dash still lands inside.
        let columns = expected.chars().count().max(keyed.chars().count()).max(1);
        let mut unit = (upper.h * 0.42).max(look.scale * 2.0);
        let mut step = unit * 4.4;
        if step * columns as f32 > r.w {
            step = r.w / columns as f32;
            unit = (step / 4.4).max(look.scale * 2.0);
        }

        if view.pattern_hidden {
            // A question mark rather than an empty row, so the slot is plainly
            // a thing being withheld and not a thing that failed to draw.
            const PROBE: f32 = 100.0;
            let per_height = fonts.line_height(FontId::Mono, PROBE) / PROBE;
            let size = (upper.h * 1.5 / per_height.max(0.01)).max(look.label_px);
            let base = fonts.baseline_centered(FontId::Mono, size, upper.y, upper.h);
            fonts.draw_text(list, upper.x.round(), base, "?", FontId::Mono, size, look.ideal);
        } else {
            let y = (upper.y + (upper.h - unit) * 0.5).round();
            for (index, symbol) in expected.chars().enumerate() {
                let w = if symbol == '-' { unit * 3.0 } else { unit };
                list.stroke_rect(
                    Rect::new((r.x + step * index as f32).round(), y, w.round(), unit.round()),
                    (look.scale * 1.5).max(1.0),
                    look.ideal,
                );
            }
        }

        if keyed.is_empty() {
            return;
        }
        let y = (lower.y + (lower.h - unit) * 0.5).round();
        // Green while it still matches, red from the first element that does
        // not: a divergence is one moment, and colouring everything after it
        // would hide which element was the mistake.
        let mut diverged = false;
        for (index, symbol) in keyed.chars().enumerate() {
            let matches = !diverged && expected.chars().nth(index) == Some(symbol);
            if !matches {
                diverged = true;
            }
            let w = if symbol == '-' { unit * 3.0 } else { unit };
            list.fill_rect(
                Rect::new((r.x + step * index as f32).round(), y, w.round(), unit.round()),
                if matches { look.right } else { look.wrong },
            );
        }
    }

    /// What was sent and what was typed.
    ///
    /// Two lines, because they are read at two different moments, and the sent one
    /// is marked per character rather than as a whole: a group with one wrong
    /// character and a group with five are the same summary and different
    /// diagnoses, and the mark is what separates them.
    fn draw_prompt(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        view: &crate::session::SessionView,
        px: f32,
        look: &DataLook,
    ) {
        list.push_clip(r);
        list.fill_rect(r, look.background);

        let metrics = fonts.metrics(FontId::Mono, px);
        let line = fonts.line_height(FontId::Mono, px);
        let pad = (look.scale * 12.0).round();
        let x = (r.x + pad).round();
        let width = (r.w - pad * 2.0).max(1.0);
        // Two lines centred as a block, so the pair sits in the middle of the
        // area whatever the font size does.
        let first = (r.y + r.h * 0.5 - line + metrics.ascent).round();

        // The sent line, one character at a time so the verdict can be carried.
        // The face is monospaced, so one measurement describes every character
        // and the pen advances by it rather than by a per glyph query.
        let advance = fonts.measure("0", FontId::Mono, px);
        if view.sent.is_empty() {
            let placeholder = if view.running { "-- -- --" } else { "" };
            if !placeholder.is_empty() {
                fonts.draw_text(list, x, first, placeholder, FontId::Mono, px, look.dim);
            }
        } else {
            // The tail rather than the head: the newest characters are what the
            // student is reading, and a line drawn from the start would push them
            // off the right edge within a group.
            //
            // The verdicts are indexed by sounding character and the string holds
            // spaces as well, so the two are walked with separate cursors. A
            // single index would shift every colour after the first space, which
            // is every field of an exchange after the first.
            let fits = (width / advance.max(1.0)) as usize;
            let sent: Vec<char> = view.sent.chars().collect();
            let from = sent.len().saturating_sub(fits.max(1));
            let mut mark = sent[..from].iter().filter(|&&c| c != ' ').count();
            let mut buffer = [0u8; 4];
            for (index, &ch) in sent[from..].iter().enumerate() {
                let colour = if ch == ' ' {
                    look.dim
                } else {
                    let verdict = view.marks.get(mark).copied();
                    mark += 1;
                    match verdict {
                        Some(Mark::Right) => look.right,
                        Some(Mark::Wrong) => look.wrong,
                        Some(Mark::Missed) => look.missed,
                        // Sent and not asked for, which is the wording of a
                        // contact. Dim rather than absent, because it was heard
                        // and the student has to see what surrounded the fields.
                        Some(Mark::Unscored) => look.dim,
                        // No verdict yet, which is the running transcript and the
                        // moment before the answer is taken.
                        None => look.text,
                    }
                };
                let at = (x + advance * index as f32).round();
                fonts.draw_text(list, at, first, ch.encode_utf8(&mut buffer), FontId::Mono, px, colour);
            }
        }

        // The typed line, with a caret while it is being written.
        let second = (first + line).round();
        let typed: Vec<char> = view.typed.chars().collect();
        let fits = (width / advance.max(1.0)) as usize;
        let from = typed.len().saturating_sub(fits.max(1));
        let shown: String = typed[from..].iter().collect();
        fonts.draw_text(list, x, second, &shown, FontId::Mono, px, look.accent);

        if view.running && !view.scored {
            let caret = (x + advance * (typed.len() - from) as f32).round();
            list.vline(
                caret,
                second - metrics.ascent,
                second + metrics.descent,
                (look.scale * 1.5).max(1.0),
                look.accent.with_alpha(0.6),
            );
        }

        list.pop_clip();
    }

    /// What the key is producing, against what it should produce.
    ///
    /// ## Why this is not optional
    ///
    /// Without it a student keys and sees nothing until a character completes,
    /// and then sees the wrong character with no way to tell what went wrong.
    /// Was the third element long, or was the gap between two of them wide enough
    /// to split the character in half? The two produce the same wrong letter and
    /// have opposite remedies.
    ///
    /// ## What each row is
    ///
    /// The upper row is the pattern being asked for, drawn as an outline: a
    /// statement of what should happen rather than a record of what did. The
    /// lower row is what the hand produced, filled. They share a column per
    /// element, so a missing element leaves a hole rather than sliding everything
    /// left.
    ///
    /// ## The element still being held
    ///
    /// Drawn in the colour of neither verdict, growing as it is held, with a mark
    /// at the point where it stops being a dot.
    ///
    /// That distinction is the whole of what makes a hand key usable here. Its
    /// length is the operator's to choose and is not known until they let go, so
    /// colouring it while the contact is closed says a dot is wrong before it has
    /// finished being a dot. It is also the only place the dot and dash boundary
    /// is visible at all: with a paddle the machine keeps it, and with a hand key
    /// it is a decision the operator has to make without being told where it is.
    ///
    /// ## Overflow
    ///
    /// Elements past the end of the expected pattern shade the region rather than
    /// being ruled off with a line. A bare line between two rows is
    /// indistinguishable from a fault in the drawing, which is what the previous
    /// form was taken for.
    #[allow(clippy::too_many_arguments)]
    fn draw_decoder(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        view: &crate::session::SessionView,
        keyed: &str,
        live_mark: u32,
        live_unit: u32,
        px: f32,
        look: &DataLook,
    ) {
        list.push_clip(r);
        list.fill_rect(r, look.background);

        let pad = (look.scale * 10.0).round();
        let inner = Rect::from_min_max(r.x + pad, r.y, r.right() - pad, r.bottom());
        if inner.is_empty() {
            list.pop_clip();
            return;
        }

        let expected = view
            .next_char
            .and_then(crate::morse::pattern_of)
            .unwrap_or("");
        let label_px = (px * 0.45).round().max(9.0);

        // The character being asked for, in the interface size, so the eye finds
        // it without leaving the row.
        let mut buffer = [0u8; 4];
        let target = match view.next_char {
            Some(ch) => ch.encode_utf8(&mut buffer),
            None => "",
        };
        let target_w = fonts.measure(target, FontId::Mono, px);
        let baseline = fonts.baseline_centered(FontId::Mono, px, inner.y, inner.h);
        fonts.draw_text(
            list,
            inner.x.round(),
            baseline,
            target,
            FontId::Mono,
            px,
            if view.running { look.text } else { look.faint },
        );

        let gap = (look.scale * 14.0).round();
        let field = Rect::from_min_max(
            inner.x + target_w + gap,
            inner.y,
            inner.right(),
            inner.bottom(),
        );
        if field.is_empty() {
            list.pop_clip();
            return;
        }

        let want = expected.chars().count();
        let done = keyed.chars().count();
        // One spare column beyond what has been keyed, so the element in progress
        // has somewhere to be drawn, and a floor so a two element character does
        // not occupy half the row.
        let columns = want.max(done + 1).max(5);
        let step = (field.w / columns as f32).min(look.scale * 26.0);
        let unit = (step * 0.26).max(look.scale * 2.0);
        let top = (field.y + field.h * 0.32).round();
        let bottom = (field.y + field.h * 0.72).round();
        let width_of = |dash: bool| if dash { unit * 3.0 } else { unit };

        // Everything past the end of what was asked for, shaded. First, so the
        // elements sit on top of their own background rather than beside it.
        if done > want && want > 0 {
            let from = (field.x + step * want as f32).round();
            if from < field.right() {
                list.fill_rect(
                    Rect::from_min_max(from, field.y, field.right(), field.bottom()),
                    look.wrong.with_alpha(0.12),
                );
            }
        }

        for (index, symbol) in expected.chars().enumerate() {
            let x = field.x + step * index as f32;
            let w = width_of(symbol == '-');
            list.stroke_rect(
                Rect::new(x.round(), (top - unit * 0.5).round(), w.round(), unit.round()),
                (look.scale).max(1.0),
                look.ideal,
            );
        }

        // Green while it still matches, red from the first element that does not:
        // a divergence is a single moment, and colouring everything after it
        // would hide which element was the mistake.
        let mut diverged = false;
        for (index, symbol) in keyed.chars().enumerate() {
            let matches = !diverged && expected.chars().nth(index) == Some(symbol);
            if !matches {
                diverged = true;
            }
            let x = field.x + step * index as f32;
            let w = width_of(symbol == '-');
            list.fill_rect(
                Rect::new(x.round(), (bottom - unit * 0.5).round(), w.round(), unit.round()),
                if matches { look.right } else { look.wrong },
            );
        }

        if live_mark > 0 {
            let units = (live_mark as f32 / live_unit.max(1) as f32).clamp(0.2, 4.0);
            let x = field.x + step * done as f32;
            list.fill_rect(
                Rect::new(
                    x.round(),
                    (bottom - unit * 0.5).round(),
                    (unit * units).round().max(1.0),
                    unit.round(),
                ),
                look.ideal,
            );
            // Two units, which is where the hand key stops calling it a dot. The
            // classifier uses the midpoint between one and three, and this is the
            // same number drawn.
            let threshold = x + unit * 2.0;
            if threshold < field.right() {
                list.vline(
                    threshold.round(),
                    bottom - unit * 1.4,
                    bottom + unit * 1.4,
                    (look.scale).max(1.0),
                    look.dim,
                );
            }
        }

        if view.running && keyed.is_empty() && live_mark == 0 {
            let text = "waiting for the key";
            let w = fonts.measure(text, FontId::Ui, label_px);
            if w < field.w {
                fonts.draw_text(
                    list,
                    (field.right() - w).round(),
                    (bottom + unit * 2.6).round(),
                    text,
                    FontId::Ui,
                    label_px,
                    look.faint,
                );
            }
        }

        list.pop_clip();
    }

    /// Per character accuracy above, the confusion matrix below.
    ///
    /// Two pictures of the same data and both are needed. The bars say which
    /// characters are weak, which is what decides what to practise; the matrix
    /// says what they are being heard as, which is what decides how. A student
    /// who confuses two characters with each other has a different problem from
    /// one who cannot hear either.
    #[allow(clippy::too_many_arguments)]
    fn draw_stats(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        rows: &[CharRow],
        matrix: crate::render::TextureId,
        extent: f32,
        chars: &[char],
        peak: u32,
        look: &DataLook,
    ) {
        list.push_clip(r);
        list.fill_rect(r, look.background);

        if rows.is_empty() {
            list.pop_clip();
            return;
        }

        let pad = (look.scale * 4.0).round();
        let label_h = (look.label_px + look.scale * 2.0).round();
        // The bars take a third and the matrix the rest: the matrix is square and
        // needs the room, and the bars are read by height rather than by area.
        let bars_h = ((r.h - pad * 3.0) * 0.33).max(label_h * 2.0);
        let bars = Rect::new(r.x + pad, r.y + pad, r.w - pad * 2.0, bars_h);

        let step = bars.w / rows.len() as f32;
        let bar_w = (step - look.scale).max(1.0);
        for (index, row) in rows.iter().enumerate() {
            let x = bars.x + step * index as f32;
            let bottom = bars.bottom() - label_h;
            let height = (bottom - bars.y).max(1.0);

            match row.accuracy {
                Some(accuracy) => {
                    let h = (height * accuracy.clamp(0.0, 1.0)).max(1.0);
                    // Green through red by accuracy, which is the one place a
                    // colour carries the value rather than the category: the bar
                    // already carries the value by height, and the colour is what
                    // makes the worst one findable without reading forty heights.
                    let colour = look.wrong.lerp(look.right, accuracy);
                    list.fill_rect(
                        Rect::new(x.round(), (bottom - h).round(), bar_w.round(), h.round()),
                        colour,
                    );
                }
                None => {
                    // Untested is drawn as an outline rather than as a nought
                    // height: an empty column and a failing one look the same,
                    // and they mean opposite things.
                    list.stroke_rect(
                        Rect::new(x.round(), bars.y.round(), bar_w.round(), height.round()),
                        (look.scale).max(1.0),
                        look.faint,
                    );
                }
            }

            let mut buffer = [0u8; 4];
            let text = row.ch.encode_utf8(&mut buffer);
            let w = fonts.measure(text, FontId::Mono, look.label_px);
            fonts.draw_text(
                list,
                (x + (bar_w - w) * 0.5).round(),
                (bars.bottom() - look.scale).round(),
                text,
                FontId::Mono,
                look.label_px,
                look.dim,
            );
        }

        // The matrix. Square, so a diagonal is a diagonal: stretched to the area
        // it would be a different shape from the one the axes describe.
        let below = Rect::from_min_max(r.x + pad, bars.bottom() + pad, r.right() - pad, r.bottom() - pad);
        if below.is_empty() || extent <= 0.0 {
            list.pop_clip();
            return;
        }
        let side = below.w.min(below.h);
        let square = Rect::new(
            (below.x + (below.w - side) * 0.5).round(),
            below.y.round(),
            side.round(),
            side.round(),
        );

        if peak == 0 {
            let text = "no confusions recorded";
            let w = fonts.measure(text, FontId::Ui, look.label_px);
            fonts.draw_text(
                list,
                (square.x + (square.w - w) * 0.5).round(),
                (square.y + square.h * 0.5).round(),
                text,
                FontId::Ui,
                look.label_px,
                look.faint,
            );
            list.pop_clip();
            return;
        }

        // The mapping travels as frame state rather than per quad, because one
        // surface uses it and it is drawn once.
        list.palette_scale = 1.0;
        list.palette_span = 1.0;
        list.palette_gamma = MATRIX_GAMMA;
        list.image(
            square,
            matrix,
            [0.0, 0.0],
            [extent, extent],
            Color::rgb(255, 255, 255),
            Mode::Palette,
        );
        list.stroke_rect(square, (look.scale).max(1.0), look.grid_major);

        // Labels only when a cell is wide enough to hold one. Below that the
        // matrix is read as a shape rather than by coordinate, and a row of
        // overlapping characters would be worse than none.
        let cell = square.w / chars.len().max(1) as f32;
        if cell >= look.label_px * 0.9 {
            let mut buffer = [0u8; 4];
            for (index, &ch) in chars.iter().enumerate() {
                let text = ch.encode_utf8(&mut buffer);
                let w = fonts.measure(text, FontId::Mono, look.label_px);
                let x = square.x + cell * (index as f32 + 0.5);
                fonts.draw_text(
                    list,
                    (x - w * 0.5).round(),
                    (square.y - look.scale * 2.0).round(),
                    text,
                    FontId::Mono,
                    look.label_px,
                    look.faint,
                );
                let y = square.y + cell * (index as f32 + 0.5) + look.label_px * 0.36;
                fonts.draw_text(
                    list,
                    (square.x - w - look.scale * 3.0).round(),
                    y.round(),
                    text,
                    FontId::Mono,
                    look.label_px,
                    look.faint,
                );
            }
        }

        list.pop_clip();
    }

    /// Vertical lines at dot boundaries, with the labels in the bottom gutter.
    ///
    /// The pattern is fixed to the units rather than to the picture, so panning
    /// moves the picture past the grid instead of dragging the grid with it.
    #[allow(clippy::too_many_arguments)]
    fn draw_unit_grid(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        dot_seconds: f32,
        span: f32,
        end: f32,
        units: bool,
        gutter: f32,
        look: &DataLook,
    ) {
        if r.is_empty() || span <= 0.0 {
            return;
        }
        let base = if units && dot_seconds > 0.0 { dot_seconds } else { 0.1 };
        // A grid finer than a few pixels is a shaded rectangle rather than a
        // grid, so the step is coarsened until the lines are separable.
        let mut step = base;
        let mut multiple = 1u32;
        while r.w * (step / span) < 4.0 * look.scale {
            step *= 2.0;
            multiple *= 2;
        }
        let label_every = look.major_every.max(1) * multiple;

        let label_y = if gutter > 0.0 {
            (r.bottom() + gutter - 3.0 * look.scale).round()
        } else {
            (r.bottom() - 4.0 * look.scale).round()
        };

        let first = (end / step).floor() as i64;
        let last = ((end + span) / step).ceil() as i64;
        for index in first..=last {
            let age = index as f32 * step;
            let t = 1.0 - (age - end) / span;
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = (r.x + r.w * t).round();
            let major = (index as u32).rem_euclid(label_every) == 0;
            let color = if major { look.grid_major } else { look.grid_minor };
            list.vline(x, r.y, r.bottom(), 1.0, color);

            if major && gutter > 0.0 && index > 0 {
                let caption = if units && dot_seconds > 0.0 {
                    format!("{}", index * multiple as i64)
                } else {
                    format!("{:.1}s", age)
                };
                let w = fonts.measure(&caption, FontId::Mono, look.label_px);
                fonts.draw_text(
                    list,
                    (x - w * 0.5).round(),
                    label_y,
                    &caption,
                    FontId::Mono,
                    look.label_px,
                    look.faint,
                );
            }
        }
    }

    /// The keying envelope, one segment per column.
    ///
    /// The peak of each column rather than a sample, because at a wide span one
    /// column covers many entries and a dot that occupied one of them would
    /// otherwise be invisible exactly when the operator zoomed out to find it.
    fn draw_envelope(list: &mut DrawList, r: Rect, scope: &Scope, span: f32, look: &DataLook) {
        if r.is_empty() || scope.is_empty() || span <= 0.0 {
            return;
        }
        let columns = (r.w as usize).clamp(2, 4096);
        let thickness = (look.trace_thickness * look.scale).max(1.0);
        let step = r.w / (columns - 1) as f32;
        // The trace sits above a baseline rather than filling the box, so the
        // ideal marks and the cursors have somewhere to be drawn that is not on
        // top of it.
        let floor = (r.bottom() - r.h * 0.12).round();
        let height = (floor - r.y - r.h * 0.10).max(1.0);

        let point = |c: usize| -> (f32, f32) {
            let t = c as f32 / (columns - 1) as f32;
            let half = span / columns as f32 * 0.5;
            let centre = scope.end + span * (1.0 - t);
            let v = scope.peak_between((centre + half).max(0.0), (centre - half).max(0.0));
            (r.x + r.w * t, floor - height * v.clamp(0.0, 1.0))
        };

        if look.trace_fill && look.trace_fill_alpha > 0.001 {
            let top = look.accent.with_alpha(look.trace_fill_alpha);
            let bottom = look.accent.with_alpha(0.0);
            for c in 0..columns {
                let (x, y) = point(c);
                let h = floor - y;
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

        list.hline(r.x, r.right(), floor, thickness, look.dim);
        let mut previous: Option<(f32, f32)> = None;
        for c in 0..columns {
            let (x, y) = point(c);
            if let Some((px, py)) = previous {
                list.line(px, py, x, y, thickness, look.accent);
            }
            previous = Some((x, y));
        }
    }

    /// Marks where each character began.
    ///
    /// Worth drawing because the gap between two characters is the one part of
    /// the picture that has no shape of its own: without a mark, a stretched gap
    /// and a run of silence at the end of the material look identical.
    fn draw_characters(
        list: &mut DrawList,
        r: Rect,
        scope: &Scope,
        span: f32,
        edges: &[(f32, Edge)],
        look: &DataLook,
    ) {
        if r.is_empty() || edges.is_empty() || span <= 0.0 {
            return;
        }
        let thickness = (1.0 * look.scale).max(1.0);
        let reach = (r.h * 0.08).max(4.0 * look.scale).round();

        for &(age, edge) in edges {
            if !edge.sync {
                continue;
            }
            let t = scope.fraction_of(age, span);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = (r.x + r.w * t).round();
            list.vline(x, r.y, r.y + reach, thickness, look.dim);
        }
    }

    /// Characters, above the trace, where they sounded.
    ///
    /// The one thing the envelope cannot state. A picture examined after a group
    /// shows an element that was plainly too long and gives no way to tell which
    /// character it belonged to, so the diagnosis stops at the shape.
    fn draw_labels(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        scope: &Scope,
        span: f32,
        labels: &[(f32, char)],
        look: &DataLook,
    ) {
        if r.is_empty() || labels.is_empty() || span <= 0.0 {
            return;
        }
        let px = (r.h * 0.8).clamp(look.label_px * 0.8, look.label_px * 1.6);
        let baseline = (r.bottom() - look.scale * 2.0).round();
        let mut last = f32::MIN;
        let mut buffer = [0u8; 4];

        for &(age, ch) in labels {
            let t = scope.fraction_of(age, span);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = r.x + r.w * t;
            let text = ch.encode_utf8(&mut buffer);
            let w = fonts.measure(text, FontId::Mono, px);
            // Dropped rather than overlapped: at a wide span the characters run
            // into one another and the row reads as a smear.
            if x < last + w * 0.7 {
                continue;
            }
            last = x;
            fonts.draw_text(list, x.round(), baseline, text, FontId::Mono, px, look.dim);
        }
    }

    /// Element lengths against the lengths they should have had.
    ///
    /// A bar above the line is an element sent long and one below it is short,
    /// with the colour stating how far. The envelope says what happened and this
    /// says what it cost, which is the difference between watching a hand and
    /// correcting one.
    ///
    /// The measured speed sits in the corner. It comes from the marks the
    /// classifier took for dots, so it is what the hand is doing rather than
    /// what the setting asks for, and the two are worth comparing.
    #[allow(clippy::too_many_arguments)]
    fn draw_timing(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        scope: &Scope,
        span: f32,
        edges: &[(f32, Edge)],
        dot_seconds: f32,
        look: &DataLook,
    ) {
        if r.is_empty() || span <= 0.0 {
            return;
        }
        let mid = (r.y + r.h * 0.58).round();
        list.hline(r.x, r.right(), mid, 1.0, look.grid_major);
        if edges.len() < 2 {
            return;
        }

        let reach = (r.h * 0.42).max(2.0);
        let mut dots = 0u32;
        let mut dot_total = 0.0f32;
        let mut error = 0.0f32;
        let mut counted = 0u32;

        // The list is oldest first, so an element runs from one entry to the
        // next and the newest has no end yet.
        for pair in edges.windows(2) {
            let (start_age, edge) = pair[0];
            let (end_age, _) = pair[1];
            let actual = start_age - end_age;
            let ideal = scope.seconds_of(edge.ideal_samples);
            if actual <= 0.0 || ideal <= 0.0 {
                continue;
            }

            let t0 = scope.fraction_of(start_age, span);
            let t1 = scope.fraction_of(end_age, span);
            if t1 < 0.0 || t0 > 1.0 {
                continue;
            }
            let x0 = r.x + r.w * t0.clamp(0.0, 1.0);
            let x1 = r.x + r.w * t1.clamp(0.0, 1.0);
            let width = (x1 - x0).max(1.0).round();

            // Whether the tone was on is not carried by the boundary, and asking
            // the envelope is cheaper than a second field on every edge.
            let on = scope.peak_between(start_age, end_age) > 0.5;
            let deviation = (actual / ideal - 1.0).clamp(-1.0, 1.0);
            error += deviation.abs();
            counted += 1;
            if on && dot_seconds > 0.0 && actual < dot_seconds * 2.0 {
                dot_total += actual;
                dots += 1;
            }

            let colour = if deviation.abs() < 0.08 {
                look.right
            } else if deviation.abs() < 0.20 {
                look.ideal
            } else {
                look.wrong
            };
            let height = (reach * deviation.abs().max(0.03)).round().max(1.0);
            let bar = if deviation >= 0.0 {
                Rect::new(x0.round(), (mid - height).round(), width, height)
            } else {
                Rect::new(x0.round(), mid, width, height)
            };
            // The gaps are weaker than the marks: both are timing and only one
            // of them is what the operator is listening to.
            list.fill_rect(bar, if on { colour } else { colour.with_alpha(0.35) });
        }

        if counted == 0 {
            return;
        }
        let mut text = format!("{:.0} % off", error / counted as f32 * 100.0);
        if dots > 0 {
            let measured = 1.2 / (dot_total / dots as f32).max(1e-4);
            text = format!("{:.0} wpm   {}", measured, text);
        }
        let w = fonts.measure(&text, FontId::Mono, look.label_px);
        if w < r.w * 0.5 {
            fonts.draw_text(
                list,
                (r.x + look.scale * 4.0).round(),
                (r.y + look.label_px).round(),
                &text,
                FontId::Mono,
                look.label_px,
                look.faint,
            );
        }
    }

    /// Marks where an element boundary should have been.
    ///
    /// Ticks rather than a second envelope. The information is the same and the
    /// reading is easier: the eye compares a tick against the edge beside it
    /// rather than separating two overlapping shapes.
    fn draw_ideal(
        list: &mut DrawList,
        r: Rect,
        scope: &Scope,
        span: f32,
        marks: &[f32],
        look: &DataLook,
    ) {
        if r.is_empty() || marks.is_empty() || span <= 0.0 {
            return;
        }
        let thickness = (1.0 * look.scale).max(1.0);
        let floor = (r.bottom() - r.h * 0.12).round();
        let reach = (r.h * 0.10).max(4.0 * look.scale).round();

        for &age in marks {
            let t = scope.fraction_of(age, span);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = (r.x + r.w * t).round();
            // Below the baseline rather than across the trace: a mark drawn over
            // the envelope would have to be told apart from the envelope, and the
            // whole point is comparing the two.
            list.vline(x, floor, floor + reach, thickness, look.ideal);
        }
    }

    /// The two measurement cursors and what lies between them.
    fn draw_cursors(
        list: &mut DrawList,
        fonts: &mut FontSystem,
        r: Rect,
        scope: &Scope,
        span: f32,
        look: &DataLook,
    ) {
        if r.is_empty() || span <= 0.0 {
            return;
        }
        let thickness = (1.0 * look.scale).max(1.0);

        // The band first, so the two lines sit on top of their own shading.
        if let (Some(a), Some(b)) = (scope.cursor_a, scope.cursor_b) {
            let x0 = r.x + r.w * scope.fraction_of(a.age, span).clamp(0.0, 1.0);
            let x1 = r.x + r.w * scope.fraction_of(b.age, span).clamp(0.0, 1.0);
            let (lo, hi) = if x0 < x1 { (x0, x1) } else { (x1, x0) };
            if hi - lo >= 1.0 {
                list.fill_rect(
                    Rect::from_min_max(lo, r.y, hi, r.bottom()),
                    look.cursor.with_alpha(0.10),
                );
            }
        }

        for (cursor, label) in [(scope.cursor_a, "A"), (scope.cursor_b, "B")] {
            let cursor = match cursor {
                Some(c) => c,
                None => continue,
            };
            let t = scope.fraction_of(cursor.age, span);
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let x = (r.x + r.w * t).round();
            list.vline(x, r.y, r.bottom(), thickness, look.cursor);

            // A flag at the top, so a cursor is findable where the trace behind
            // it is bright, and filled only when the position came from an edge:
            // that is the difference between a measurement and an estimate.
            let flag = (r.h * 0.12).max(5.0 * look.scale).round();
            let width = (thickness * 3.0).round();
            let mark = Rect::new(x, r.y, width, flag);
            if cursor.snapped {
                list.fill_rect(mark, look.cursor);
            } else {
                list.stroke_rect(mark, thickness, look.cursor);
            }

            fonts.draw_text(
                list,
                (x + width + 2.0 * look.scale).round(),
                (r.y + look.label_px).round(),
                label,
                FontId::Mono,
                look.label_px,
                look.cursor,
            );
        }
    }

    fn shutdown(&mut self) {
        self.stop_session();
        self.stop_output();
        if let Err(e) = self.progress.save() {
            crate::log_warn!("app", "cannot save the history: {}", e);
        }

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
        self.settings.ui.maximized = self.window.is_maximized();

        if self.high_res_timer {
            platform::win32::end_high_resolution_timing();
        }
        crate::log_info!("app", "shutdown complete");
    }

    pub fn take_settings(&mut self) -> Settings {
        std::mem::take(&mut self.settings)
    }
}

/// Slot a mouse button occupies in the contact latch.
///
/// Only the two main buttons are contacts. The middle one pans the picture and
/// the side ones are not present on every device, so binding either would be
/// binding a contact some operators cannot close.
fn mouse_index(button: MouseButton) -> Option<usize> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Right => Some(1),
        _ => None,
    }
}

/// Language codes the catalogue directory offers, reference wording first.
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