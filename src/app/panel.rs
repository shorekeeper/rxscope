//! Interface declaration.
//!
//! Split from the application shell because the two answer different questions:
//! the shell owns devices, timing and drawing, this module owns what the
//! operator sees and touches. Nothing here holds state of its own. Every value
//! it reads arrives through StatusInfo, every change it wants leaves through
//! UiCommands, and the shell applies those after the declaration has finished.
//!
//! The deferral is not stylistic. The layout solver walks the tree the
//! declaration built, so a setting mutated part way through would be read at two
//! different values inside one frame, and a command that rebuilt a decoder would
//! invalidate the status the rest of the panel is still formatting.
//!
//! The side panel is composed from sections listed per tab in the configuration.
//! Each section appears on exactly one tab by default, and the tab names say
//! what is on them. The settings tab is not composable and always carries the
//! layout editor, which is what keeps a composition recoverable however badly it
//! was edited.

use crate::config::settings::{
    AnchorMode, AudioBackend, CallsignSource, LogLevelCfg, MeterScale, PanelSection,
    SidebandMode, WaterfallStyle, WindowFn, PANEL_TABS,
};
use crate::config::ConfigEnum;
use crate::config::Settings;
use crate::decode::channels::ChannelInfo;
use crate::decode::{DecoderStatus, Mode};
use crate::font::FontId;
use crate::gui::layout::{Align, Style};
use crate::gui::{Frame, TextAlign, WindowButton};
use crate::platform::MouseButton;
use crate::render::Color;
use crate::rig::RigStatus;
use crate::record::replay::{ReplayStatus, SPEED_MAX, SPEED_MIN};
use crate::record::RecorderStatus;

use super::{DecodeView, Entry};

use detent::{LinkState, Signal};

/// Tags of the areas the application draws itself. Declared here because this
/// module reserves them and the shell only fills them in.
pub const TAG_SPECTRUM: u32 = 1;
pub const TAG_WATERFALL: u32 = 2;
pub const TAG_DECODE: u32 = 3;
pub const TAG_METER: u32 = 4;
/// Scrub bar of the replay tab.
pub const TAG_TIMELINE: u32 = 5;

/// Bounds of the keying detector width, shared with the drag gesture so the
/// slider and the pointer cannot disagree about the limits.
pub const BANDWIDTH_MIN_HZ: f32 = 20.0;
pub const BANDWIDTH_MAX_HZ: f32 = 1000.0;

/// Largest magnification of the display.
///
/// Sixty four brings a twelve kilohertz span down to under two hundred hertz,
/// which separates two carriers a few hertz apart. Beyond that the transform
/// resolution is reached and further magnification only widens the pixels.
pub const ZOOM_MAX: f32 = 64.0;

/// Rates a converter is likely to accept.
///
/// Nought is a request to follow the device rather than a rate, which is the
/// ordinary case on the shared path where the mix format is handed back
/// whatever was asked for.
const DEVICE_RATES: [u32; 12] = [
    0, 8_000, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000,
];

/// Rates the processing path is offered.
///
/// The top of the list matches the top of the device list, because a quadrature
/// input is two sided and a path running at half the converter rate throws away
/// half the band before anything sees it.
const DSP_RATES: [u32; 10] = [
    8_000, 12_000, 16_000, 24_000, 32_000, 44_100, 48_000, 96_000, 176_400, 192_000,
];

/// Transform sizes, every one a power of two.
const FFT_SIZES: [u32; 8] = [256, 512, 1_024, 2_048, 4_096, 8_192, 16_384, 32_768];

/// Bounds of the side panel, in logical units.
///
/// The floor is what the widest settings row needs before its label column hits
/// its own floor and the control beside it collapses. The ceiling is a fraction
/// of the window rather than a constant, because the panel competes with the
/// spectrum for width and the spectrum is the reason the application exists.
pub const SIDE_PANEL_MIN: f32 = 240.0;
pub const SIDE_PANEL_MAX: f32 = 900.0;
pub const SIDE_PANEL_MAX_FRACTION: f32 = 0.45;

/// Tab strip.
///
/// The first four are composable and index the configuration lists directly.
/// The last two are fixed: one configures the panel, including the composition
/// of the other four, and one owns the recording, which is a mode of the
/// application rather than a group of settings.
const TAB_KEYS: [&str; PANEL_TABS + 2] = [
    "panel.receive",
    "panel.audio",
    "panel.decode",
    "panel.display",
    "panel.settings",
    "panel.replay",
];

/// Positions of the two fixed tabs.
pub const TAB_SETTINGS: usize = PANEL_TABS;
pub const TAB_REPLAY: usize = PANEL_TABS + 1;

/// Positions the operator picked in the lists.
///
/// Held together because they are interface position rather than configuration:
/// they say which entry is highlighted, and the configuration only changes once
/// the shell applies the corresponding command.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selections {
    pub device: usize,
    pub monitor: usize,
    pub profile: usize,
    pub port: usize,
}

/// One row of the band panel.
///
/// Formatted by the shell rather than here, because the frequency has to be
/// grouped the way the readout groups it and that arithmetic already lives
/// there. The raw value travels with it so the row can be a target as well as a
/// statement.
#[derive(Debug, Clone)]
pub struct StationRow {
    pub hz: i64,
    pub frequency: String,
    pub text: String,
    /// True while the entry falls inside the receiver passband, which is what
    /// separates a station being worked from one merely visible.
    pub inside: bool,
}

/// One station as the spot list shows it.
///
/// Formatted by the shell for the same reason a station row is: the frequency
/// has to be grouped the way the readout groups it, and the age has to be
/// computed against the same clock the retirement uses.
#[derive(Debug, Clone)]
pub struct SpotRow {
    pub frequency: String,
    /// Call, country, signal and age, already laid out in columns.
    pub detail: String,
    /// True when the entry rests on more than a single sighting of a token.
    ///
    /// Colour rather than a printed figure. The row already carries four
    /// numbers, and what the operator needs from the fifth is a yes or a no.
    pub confident: bool,
    /// True when a frequency on the air is known, without which there is
    /// nowhere to tune to.
    pub tunable: bool,
}

/// One segment as the replay list shows it.
///
/// Formatted by the shell for the same reason a station row is: the arithmetic
/// that turns blocks into a duration belongs beside the format that defines a
/// block, and the panel is declared every frame.
#[derive(Debug, Clone)]
pub struct SegmentRow {
    pub name: String,
    /// Duration and size, already formatted.
    pub detail: String,
    /// False when the file could not be read as a segment. Listed rather than
    /// hidden, so an operator whose recording will not open is told which file
    /// is at fault instead of finding it missing.
    pub usable: bool,
}

/// One frame of a receiver filter drag.
///
/// A named structure rather than a tuple because the third field decides what
/// the first two mean, and a caller reading a bare pair of booleans would have
/// to remember which is which.
#[derive(Debug, Clone, Copy)]
pub struct FilterDrag {
    /// Position on the display, nought to one.
    pub fraction: f32,
    pub started: bool,
    /// True when the whole band moves and keeps its width, rather than one edge
    /// moving and changing it.
    pub whole_band: bool,
}

/// Read only values handed to the interface builder.
pub struct StatusInfo<'a> {
    pub fps: f32,
    pub worst_ms: f32,
    pub draw_calls: u32,
    pub uploads: u32,
    pub clears: u32,
    /// Device side frame time, nought when not measured.
    pub gpu_ms: f32,
    pub gpu_worst_ms: f32,
    pub glyphs: usize,
    pub atlas: f32,
    pub gpu: &'a str,
    pub font: &'a str,
    pub dpi: f32,
    pub audio: &'a crate::audio::AudioStatus,
    pub decoder: DecoderStatus,
    pub channels: &'a [ChannelInfo],
    pub languages: &'a [&'a str],
    pub language_index: usize,
    /// True when the selected endpoint is recorded through the loopback path,
    /// which rules out exclusive mode.
    pub loopback: bool,
    /// True when the selected entry is the one that follows the system default.
    pub default_device: bool,
    pub queue_fill: f32,
    pub lines: u64,
    /// Columns the history texture actually holds.
    pub waterfall_columns: u32,
    /// Rate the processing path settled on, after the reduction.
    pub source_rate: u32,
    /// Impulses the blanker ahead of the transform acted on.
    pub blanked: u64,
    /// Threads the transform section resolved to.
    pub worker_threads: u32,
    /// Times a failed device or link was brought back on its own.
    ///
    /// Reported because a receiver that quietly restarts itself is a cable that
    /// wants replacing, and papering over it silently would hide exactly that.
    pub audio_recoveries: u32,
    pub rig_recoveries: u32,
    pub log_lines: usize,
    pub decoded_chars: u64,
    pub noise_floor_db: f32,
    pub meter_text: String,
    pub nyquist_hz: f32,
    pub bin_hz: f32,
    /// Lines the transform is producing, per second.
    ///
    /// The rate that resulted rather than the one that was requested. The two
    /// differ whenever the overlap setting is the binding constraint, and from
    /// the two controls alone there is no way to tell which of them is acting.
    pub line_rate: f32,
    /// Frequency under the pointer, when it rests on the display.
    pub cursor_hz: Option<f32>,
    /// Level of the bin under it, absent while the transform holds nothing.
    pub cursor_db: Option<f32>,
    /// Span the display currently covers, in hertz.
    pub view_span_hz: f32,
    /// True when the whole representable span is on screen.
    pub view_full: bool,
    /// True when the audio is drawn right to left, which is what the lower
    /// sideband needs for the frequency axis to ascend.
    pub mirrored: bool,
    /// Audio frequency the view is kept around, which is not the dial.
    pub working_hz: f32,
    /// Largest side panel width the window allows, in logical units.
    pub side_panel_max: f32,
    /// Smallest width the current wording fits in.
    ///
    /// Measured rather than stated: the drag has to refuse a width that would
    /// cut the captions, and only the widget system knows how wide they came
    /// out in the language that is loaded.
    pub side_panel_min: f32,
    /// Height of the whole surface, for the splitter arithmetic.
    pub height: f32,
    pub audio_running: bool,
    pub monitor: crate::audio::MonitorStatus,
    pub monitor_devices: &'a [&'a str],
    /// Band the monitor is actually passing, when it is filtering.
    pub listen_band: Option<(f32, f32)>,
    /// True when the selected pair would feed the output back into the input.
    pub feedback: bool,
    /// True while the window occupies the work area, so the chrome button can
    /// show the correct command.
    pub window_maximized: bool,
    /// Condition of the recorder.
    pub recorder: RecorderStatus,
    /// Condition of the replay, absent when no recording is open.
    pub replay: Option<ReplayStatus>,
    /// True while the display is fed from a recording rather than from the air.
    pub replay_active: bool,
    /// Duration of one block, which is the granularity every seek lands on.
    pub replay_block_seconds: f64,
    /// Segments on disk, oldest first.
    pub segments: &'a [SegmentRow],
    /// Whole directory, formatted. A borrow like every other text in this
    /// structure, so the shell keeps the buffer and the declaration does not
    /// allocate one per frame.
    pub segments_total: &'a str,
    /// Condition of the transceiver link.
    pub rig: RigStatus,
    /// Descriptions in the catalogue, unusable ones marked.
    pub rig_profiles: &'a [&'a str],
    /// Diagnostics of the selected description, empty when it is sound.
    pub rig_issues: &'a [String],
    pub rig_ports: &'a [&'a str],
    /// Frequency to display, absent while nothing has been read.
    pub rig_frequency: Option<i64>,
    /// Mode the transceiver reports, empty when it reports none.
    pub rig_mode: &'static str,
    /// True when the description defines a way to set the frequency.
    pub rig_can_tune: bool,
    /// Amateur band the dial sits in, absent when it sits outside one.
    pub rig_band: Option<&'a str>,
    /// Segment inside that band, as usage and edges. Empty when unknown.
    pub rig_segment: &'a str,
    /// Known frequencies inside the visible span.
    pub stations: &'a [StationRow],
    /// Stations heard, oldest sighting last.
    pub spots: &'a [SpotRow],
    /// Bands the plan lists, ascending.
    pub bands: &'a [&'a str],
    /// Point a measurement is taken from, absent when none is placed.
    pub reference_hz: Option<f32>,
    /// Difference between the pointer and that point.
    pub reference_delta_hz: Option<f32>,
    /// Settings that differ from the values the build ships with.
    ///
    /// Refreshed on a timer by the shell and only while the tab that shows it is
    /// open, so the comparison never lands on a frame that is drawing a spectrum.
    pub deviations: &'a [String],
    /// Prefixes and countries the database holds, nought when none is loaded.
    pub prefix_count: usize,
    pub country_count: usize,
    /// Channels the capture device delivers.
    pub device_channels: u32,
    /// True when the signal is complex from the input to the ear.
    ///
    /// Decides three things the panel has to state consistently: whether a
    /// filter edge may go below nought, whether the lower sideband preset is
    /// mirrored, and whether the sideband setting means anything at all.
    pub complex_signal: bool,
    /// Audio frequency of the middle of the receiver passband.
    pub receiver_listen_hz: f32,
    /// Audio frequency of the point the readout names.
    pub receiver_reference_hz: f32,
}

/// Actions requested by the interface.
#[derive(Default)]
pub struct UiCommands {
    pub rescan_devices: bool,
    pub select_device: bool,
    pub start_audio: bool,
    pub stop_audio: bool,
    pub clear_log: bool,
    /// Audio frequency of a decoded line the operator clicked.
    ///
    /// A frequency rather than a fraction, unlike every other tuning request
    /// here: a decoded line records where it came from, so nothing has to be
    /// converted through the axis and nothing can be lost if the view moved
    /// between the reception and the click.
    pub decode_tune_hz: Option<f32>,
    /// Copy the decoded text the filter admits.
    pub copy_decode: bool,
    /// Return the decoded text panel to the newest line.
    pub decode_newest: bool,
    /// Bring the receiver to a station in the spot list, by its position.
    pub tune_spot: Option<usize>,
    pub clear_spots: bool,
    /// Reread the prefix database and the operator maintained list.
    pub reload_callsigns: bool,
    /// Frequency or band name the operator typed.
    ///
    /// The text rather than a parsed value, because a band name is not a
    /// frequency and deciding what to do with one needs the band stack.
    pub tune_typed: Option<String>,
    /// Band the operator pressed, by its position in the list.
    pub tune_band: Option<usize>,
    /// Record the current frequency in the station list.
    pub mark_station: bool,
    /// Place the measurement reference, as a fraction of the display.
    pub mark_reference: Option<f32>,
    pub clear_reference: bool,
    /// The one saved view.
    pub store_view: bool,
    pub recall_view: bool,
    /// Position on the display the operator is pointing at, nought to one.
    ///
    /// A fraction rather than a frequency, because the conversion needs the axis
    /// and the axis needs the transceiver, neither of which the panel has.
    ///
    /// The keying gesture carries whether the press just happened, because what
    /// it grabbed is decided then and held for the rest of the drag.
    pub channel_drag: Option<(f32, bool)>,
    pub rig_tune_fraction: Option<f32>,
    /// Position the operator picked as the receiver tuning point.
    pub receiver_tune_fraction: Option<f32>,
    pub bandwidth_fraction: Option<f32>,
    /// Width the operator typed or dragged in the panel, for the focused
    /// channel. Separate from the fraction above, which is a position on the
    /// display and has to be turned into a width through the axis.
    pub channel_width_hz: Option<f32>,
    /// Receiver filter dragged on the display.
    pub filter_drag: Option<FilterDrag>,
    /// Wheel over the display: position and notches turned.
    pub zoom_at: Option<(f32, f32)>,
    /// Middle drag over the display.
    pub pan_drag: Option<(f32, bool)>,
    pub reset_zoom: bool,
    /// Frequency on the air the operator picked from the band list.
    pub tune_rf: Option<i64>,
    pub focus_channel: Option<u32>,
    pub drop_channel: Option<u32>,
    pub language: Option<String>,
    pub log_level: Option<crate::core::log::Level>,
    pub select_monitor: bool,
    pub restart_monitor: bool,
    pub reset_tab: Option<usize>,
    pub rescan_rig: bool,
    pub select_rig_profile: bool,
    pub select_rig_port: bool,
    pub start_rig: bool,
    pub stop_rig: bool,
    /// Recording.
    pub start_record: bool,
    pub stop_record: bool,
    /// Reread the segment directory.
    pub rescan_segments: bool,
    /// Export one segment, by its position in the list.
    pub export_segment: Option<usize>,

    /// Replay. The whole directory is opened rather than one file: the
    /// timeline spans the segments, and a seek across a boundary is what makes
    /// a cyclic recording browsable at all.
    pub open_replay: bool,
    pub close_replay: bool,
    pub replay_play: Option<bool>,
    /// Position as a fraction of the whole recording.
    pub replay_seek: Option<f32>,
    /// Movement in whole blocks.
    pub replay_step: Option<i64>,
    /// Jump to the newest block.
    pub replay_live: bool,
    /// Playback rate, as a multiple of real time.
    ///
    /// Deliberately not carried into the settings. A playback rate is a
    /// property of the session rather than of the receiver, and one restored on
    /// the next start would surprise an operator who set it once to pick apart
    /// a single character.
    pub replay_speed: Option<f32>,
    /// Wait at the newest block instead of stopping.
    pub replay_follow: Option<bool>,
    /// Return to the start instead of stopping.
    pub replay_loop: Option<bool>,
    /// Window chrome. Only meaningful while the application draws its own
    /// frame; with the system frame these are never raised.
    pub window_drag: bool,
    pub window_minimize: bool,
    pub window_toggle_max: bool,
    pub window_close: bool,
    /// The frame setting moved, so the window has to be told.
    pub frame_changed: bool,
}

/// Declares the whole interface for one frame.
#[allow(clippy::too_many_arguments)]
pub fn build(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    devices: &[&str],
    sel: &mut Selections,
    view: &mut DecodeView,
    entry: &mut Entry,
    tab: &mut usize,
    editing_tab: &mut usize,
    add_section: &mut usize,
    cmd: &mut UiCommands,
) {
    let status_h = f.ui.m(f.ui.theme.row_height + 2.0);
    let caption_h = f.ui.m(f.ui.theme.caption_height);

    f.begin_frame(Style::column().grow(1.0), f.ui.theme.background, Color::TRANSPARENT);

    caption_bar(f, settings, status, tab, cmd, caption_h);
    f.ui.add_separator_row();

    f.begin(Style::row().grow(1.0));
    display_area(f, settings, status, view, cmd, caption_h, status_h);
    if settings.ui.show_settings_panel {
        side_panel(
            f, settings, status, devices, sel, entry, *tab, editing_tab, add_section, cmd,
        );
    }
    f.end();

    f.ui.add_separator_row();
    status_bar(f, settings, status, status_h);

    f.end();

    if settings.ui.show_debug_overlay {
        overlay(f, status);
    }
}

/// Caption strip.
///
/// One row carries three things that would otherwise need three: the tab strip,
/// the commands, and the window chrome. Merging them is what removes the system
/// caption without spending a second row of height on a replacement.
///
/// The drag zone is whatever the row has left over. That is a layout answer to
/// a question the window proc cannot answer: with a custom frame the whole
/// client area reports as client, so the only thing that separates a handle
/// from a button is which of them the layout put there.
///
/// The zone shrinks as well as grows. A translation can make the tab captions
/// wider than the window, and without the shrink the free space goes negative
/// and the last button is pushed off the edge where nothing can reach it.
fn caption_bar(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    tab: &mut usize,
    cmd: &mut UiCommands,
    height: f32,
) {
    let custom = settings.appearance.custom_frame;

    f.begin_clipped(
        Style::row().height_px(height).align(Align::Center),
        f.ui.theme.panel_header,
        Color::TRANSPARENT,
    );

    if custom {
        // Application name, where the system caption used to put it. Not a
        // localization key: a proper noun is the same in every language, and
        // routing it through the catalogue would invite a translation of it.
        //
        // Fixed width rather than shrinking. Seven characters cost less than the
        // gap beside them, and it is the one thing on the strip that never
        // changes, so it is the wrong thing to compress when a translation makes
        // the tabs wide.
        f.gap(f.ui.theme.padding);
        let color = f.ui.theme.text;
        f.label_styled("RXScope v1.35 / ", color, FontId::Ui, TextAlign::Left, Style::row());
        f.separator_vertical();
    }

    // Selecting a tab reveals the panel. The settings tab in particular has no
    // meaning without it: it is the tab that configures the panel.
    if f.tabs("toolbar", tab, &TAB_KEYS) {
        settings.ui.show_settings_panel = true;
    }

    if custom {
        let hit = f.caption(
            "caption",
            Style::row().grow(1.0).shrink(1.0).align_self(Align::Stretch),
        );
        if hit.double {
            cmd.window_toggle_max = true;
        } else if hit.drag {
            cmd.window_drag = true;
        }
    } else {
        f.spacer(1.0);
    }

    if f.button(if status.audio_running { "action.stop" } else { "action.start" }) {
        if status.audio_running {
            cmd.stop_audio = true;
        } else {
            cmd.start_audio = true;
        }
    }
    if f.button("action.clear") {
        cmd.clear_log = true;
    }
    if f.button("action.panel") {
        settings.ui.show_settings_panel = !settings.ui.show_settings_panel;
    }

    if custom {
        // Flush against the right edge: a gap there reads as a misalignment
        // rather than as spacing, because the window border used to be exactly
        // where the gap now is.
        f.gap(f.ui.theme.gap);
        if f.window_button("win.min", WindowButton::Minimize) {
            cmd.window_minimize = true;
        }
        let restore = if status.window_maximized {
            WindowButton::Restore
        } else {
            WindowButton::Maximize
        };
        if f.window_button("win.max", restore) {
            cmd.window_toggle_max = true;
        }
        if f.window_button("win.close", WindowButton::Close) {
            cmd.window_close = true;
        }
    } else {
        f.gap(f.ui.theme.gap);
    }

    f.end();
}

/// Spectrum, waterfall and whatever occupies the area underneath.
#[allow(clippy::too_many_arguments)]
fn display_area(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    view: &mut DecodeView,
    cmd: &mut UiCommands,
    caption_h: f32,
    status_h: f32,
) {
    f.begin(Style::column().grow(1.0));

    // Trace above history, both inside one panel so the frequency axis is
    // shared and a click means the same thing in either.
    f.begin_panel("panel.waterfall", Style::column().grow(1.0).min_h(f.ui.m(120.0)));
    if settings.waterfall.spectrum_visible {
        f.custom(
            TAG_SPECTRUM,
            Style::row().height_percent(settings.waterfall.spectrum_height_fraction),
        );
    }
    f.custom(TAG_WATERFALL, Style::row().grow(1.0));
    f.end_panel();

    // Four gestures over one surface, separated by button and by modifier
    // rather than by region, because a region a gesture belongs to would have
    // to be visible and there is nothing to draw it on.
    //
    // Left: selecting a signal. In the receiver mode that means the tuning
    // point, which is the only control that reaches it; in the skimmer it means
    // pointing a keying detector, which is the only way to place one when
    // several channels are allowed. Either way the control key swaps in moving
    // the transceiver instead, and click_tunes states which of the two a plain
    // click carries. A request the transceiver cannot carry out falls through
    // to the local intention rather than being dropped: a display that ignores
    // a click cannot be told from one that is not listening.
    //
    // Right: the passband, which is a different thing in each mode. One
    // detector width in the skimmer, two edges around the tuning point in the
    // receiver, and shift there moves both together, which changes what
    // survives without changing the pitch of what does.
    //
    // Middle: panorama. Wheel: magnification about the pointer, which is where
    // an operator expects it and the only place that keeps the signal they are
    // looking at under the cursor.
    //
    // Positions travel as fractions. Turning one into a frequency needs the
    // axis, which needs the transceiver and the quadrature setting, and neither
    // belongs here.
    for tag in [TAG_SPECTRUM, TAG_WATERFALL] {
        if let Some((x, notches)) = f.ui.custom_wheel(tag) {
            cmd.zoom_at = Some((x, notches));
        }
        if let Some(drag) = f.ui.custom_drag(tag, MouseButton::Middle) {
            cmd.pan_drag = Some((drag.x, drag.started));
        }
        // The left button is read as a drag rather than as a click, and a click
        // is the first frame of one. That is what lets the same gesture select a
        // channel and then move it: the press decides what was grabbed and the
        // frames after it carry the pointer.
        if let Some(drag) = f.ui.custom_drag(tag, MouseButton::Left) {
            // Shift places the measurement reference instead of tuning. The
            // fifth gesture on one surface, and the only modifier left: shift
            // with the right button already moves the whole passband, and the
            // two cannot be confused because they are different buttons.
            //
            // On the press alone: a reference that followed the pointer would
            // state a difference against wherever the pointer is, which is
            // nought.
            if drag.mods.shift {
                if drag.started {
                    cmd.mark_reference = Some(drag.x);
                }
                continue;
            }

            // The local target differs by mode, and in the receiver mode it
            // exists only for a complex signal and only while independent
            // tuning is allowed. Demodulated audio has no tuning point, because
            // the transceiver already tuned; a locked tuning point is an
            // operator decision that the receiver follows the dial. Where there
            // is no local target the gesture goes to the transceiver instead of
            // vanishing, and where there is no transceiver either it still
            // travels, so the application can say why nothing happened.
            let ctrl = drag.mods.ctrl;
            let wants_rig = settings.rig.click_tunes != ctrl;
            let local_possible = if settings.sdr_mode() {
                status.complex_signal && settings.receiver.tune_enabled
            } else {
                true
            };

            if (wants_rig || !local_possible) && status.rig_can_tune {
                // The dial moves on the press alone. A transceiver answers one
                // request per polling pass, so a drag would queue a frequency
                // per frame and the radio would still be arriving at the first
                // of them when the gesture ended.
                if drag.started {
                    cmd.rig_tune_fraction = Some(drag.x);
                }
            } else if settings.sdr_mode() {
                cmd.receiver_tune_fraction = Some(drag.x);
            } else {
                cmd.channel_drag = Some((drag.x, drag.started));
            }
        }

        if let Some(drag) = f.ui.custom_drag(tag, MouseButton::Right) {
            if settings.sdr_mode() {
                cmd.filter_drag = Some(FilterDrag {
                    fraction: drag.x,
                    started: drag.started,
                    whole_band: drag.mods.shift,
                });
            } else {
                cmd.bandwidth_fraction = Some(drag.x);
            }
        }
    }

    // Two occupants of one area, chosen by the mode, and neither when the
    // operator has handed the space back to the display. Decoded text is what
    // the skimmer produces and there is none in the receiver mode, where the
    // decoders are not fed at all; an empty panel there would reserve a third
    // of the window for nothing.
    let lower = settings.sdr_mode() || settings.ui.show_decode_log;

    // The splitter is declared only when something sits below it. A handle that
    // separates one area from nothing is a handle that does nothing, and it
    // still takes a row of height and still reports a drag.
    if lower {
        // The drag is a fraction of the usable height, which is the window
        // minus the two fixed strips.
        let usable = (status.height - caption_h - status_h).max(1.0);
        let dy = f.splitter("decode_split", false);
        if dy != 0.0 {
            let next = settings.ui.decode_panel_fraction - dy / usable;
            settings.ui.decode_panel_fraction = next.clamp(0.1, 0.8);
        }
    }

    if settings.sdr_mode() {
        band_panel(f, settings, status, cmd);
    } else if settings.ui.show_decode_log {
        decode_panel(f, settings, status, view, cmd);
    }

    f.end();
}

/// Decoded text, with the two controls that make a few thousand lines usable.
///
/// A filter and a scroll position. Without them the panel is a window onto the
/// newest dozen lines and everything above them is unreachable, which for a
/// skimmer is almost everything it produced.
///
/// The chrome sits above the text rather than below it, because the text is laid
/// out from the bottom upwards: controls under it would move every time a line
/// arrived.
fn decode_panel(
    f: &mut Frame<'_>,
    settings: &Settings,
    status: &StatusInfo<'_>,
    view: &mut DecodeView,
    cmd: &mut UiCommands,
) {
    let gap = f.ui.m(f.ui.theme.gap);
    let title = decode_title(f, status);

    f.begin_panel(
        &title,
        Style::column()
            .basis_percent(settings.ui.decode_panel_fraction)
            .min_h(f.ui.m(80.0)),
    );

    f.text_edit("field.decode.filter", &mut view.filter);

    f.begin(Style::row().gap(gap).align(Align::Center));
    if f.button("action.copy") {
        cmd.copy_decode = true;
    }
    f.begin_disabled(view.scroll == 0);
    if f.button("action.newest") {
        cmd.decode_newest = true;
    }
    f.end_disabled();
    f.spacer(1.0);
    // The held position is stated only while it is held. A permanent reading of
    // nought would be noise, and its absence is what says the panel is showing
    // the newest line.
    if view.scroll > 0 {
        let held = format!("{} {}", view.scroll, f.ui.tr("status.held"));
        f.label_dim(&held);
    }
    f.end();

    // Shown while there is nothing to read, which is the one moment the space
    // costs nothing and the one moment an operator is looking for what the
    // panel does.
    if status.log_lines == 0 {
        f.hint("hint.decode_scroll");
    }

    f.custom(TAG_DECODE, Style::row().grow(1.0));
    f.end_panel();
}

/// Band information, where the decoded text goes in the other mode.
///
/// Collapsible, because it is reference rather than traffic: an operator
/// consults it on arriving at a band and then wants the height back for the
/// waterfall. Folded it costs one row.
fn band_panel(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    let style = if settings.ui.band_panel_open {
        Style::column()
            .basis_percent(settings.ui.decode_panel_fraction)
            .min_h(f.ui.m(80.0))
    } else {
        // Folded to the header alone. Stated as a height rather than by
        // omitting the panel, so the splitter above keeps something to separate
        // and does not jump to the window edge.
        Style::column().height_px(f.ui.m(f.ui.theme.header_height + 4.0))
    };

    let title = match status.rig_band {
        Some(band) => format!("{}   {}", f.ui.tr("panel.band"), band),
        None => f.ui.tr("panel.band").to_string(),
    };

    let open = f.begin_group_titled(&title, style, settings.ui.band_panel_open);
    settings.ui.band_panel_open = open;

    if open {
        let gap = f.ui.m(f.ui.theme.gap);
        let pad = f.ui.m(f.ui.theme.group_padding);
        f.begin_scroll("band_scroll", Style::column().grow(1.0).gap(gap).padding(pad));

        if status.rig_frequency.is_none() {
            f.hint("hint.band_needs_rig");
        }
        if !status.rig_segment.is_empty() {
            f.readout("field.band.segment", status.rig_segment);
        }

        if status.stations.is_empty() {
            f.hint("hint.band_no_stations");
        } else if status.rig_can_tune {
            f.hint("hint.stations_click");
        }

        for (index, entry) in status.stations.iter().enumerate() {
            // One scope per row: the rows are built from the same keys and
            // would otherwise share their identity, which is what makes state
            // leak from one row to the next.
            f.ui.begin_scope(&format!("station{}", index));
            f.begin(Style::row().gap(gap).align(Align::Center));

            // The frequency is a target rather than a statement. An entry is
            // there because an operator returns to it, and the shortest path
            // from wanting to hear it to hearing it is one press.
            f.begin_disabled(!status.rig_can_tune);
            if f.button(&entry.frequency) {
                cmd.tune_rf = Some(entry.hz);
            }
            f.end_disabled();

            let color = if entry.inside {
                f.ui.theme.text
            } else {
                f.ui.theme.text_disabled
            };
            f.label_mono(
                &entry.text,
                color,
                TextAlign::Left,
                Style::row().grow(1.0).shrink(1.0),
            );

            f.end();
            f.ui.end_scope();
        }

        f.end_scroll();
    }
    f.end_group();
}

fn decode_title(f: &Frame<'_>, status: &StatusInfo<'_>) -> String {
    let head = f.ui.tr("panel.decode");
    let channels = f.ui.tr("unit.channels");
    match status.decoder.mode {
        Mode::Unknown => format!("{}   {} {}", head, status.decoder.cw_channels, channels),
        Mode::Cw => format!(
            "{}   {}   {} {}   {:.0} {}   {:.0} {}",
            head,
            f.ui.tr("mode.cw"),
            status.decoder.cw_channels,
            channels,
            status.decoder.wpm,
            f.ui.tr("unit.wpm"),
            status.decoder.cw_tone_hz,
            f.ui.tr("unit.hz")
        ),
        other => format!(
            "{}   {}   {:.2} {}   {:.0} {}",
            head,
            f.ui.tr(other.key()),
            status.decoder.baud,
            f.ui.tr("unit.baud"),
            status.decoder.shift_hz,
            f.ui.tr("unit.hz")
        ),
    }
}

/// Side panel. Its contents are the sections listed for the active tab, or the
/// fixed application settings when the settings tab is selected.
#[allow(clippy::too_many_arguments)]
fn side_panel(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    devices: &[&str],
    sel: &mut Selections,
    entry: &mut Entry,
    tab: usize,
    editing_tab: &mut usize,
    add_section: &mut usize,
    cmd: &mut UiCommands,
) {
    let dx = f.splitter("side_split", true);
    if dx != 0.0 {
        // The handle sits on the left edge, so moving it right narrows the panel.
        //
        // The floor is the width the current wording needs rather than a
        // constant. A drag that went below it would cut the captions, and the
        // caption is what names the control beside it.
        let next = settings.ui.side_panel_width - dx / f.ui.scale();
        settings.ui.side_panel_width =
            next.clamp(status.side_panel_min, status.side_panel_max);
    }

    let gap = f.ui.m(f.ui.theme.gap);
    let margin = f.ui.m(f.ui.theme.panel_margin);
    let inset = f.ui.m(f.ui.theme.panel_inset);
    let width = f.ui.m(settings.ui.side_panel_width);

    // The inset keeps the panel surface off the splitter on one side and off
    // the window frame on the other. It is deliberately small: the scroll
    // container inside already provides the clear space, and adding the two
    // together pushes every group away from the edge twice over.
    f.begin_frame(
        Style::column().width_px(width).padding_xy(inset, 0.0),
        f.ui.theme.panel,
        Color::TRANSPARENT,
    );
    f.begin_scroll("side_scroll", Style::column().grow(1.0).gap(gap).padding(margin));

    if tab == TAB_SETTINGS {
        application(f, settings, status, cmd);
        appearance(f, settings, cmd);
        data_area(f, settings);
        layout_editor(f, settings, editing_tab, add_section, cmd);
        deviations(f, status);
    } else if tab == TAB_REPLAY {
        section_record(f, settings, status, cmd);
        section_replay(f, status, cmd);
        section_segments(f, settings, status, cmd);
    } else {
        // The list is copied because the section builders take the settings
        // mutably, and the list lives inside them.
        let order: Vec<PanelSection> = settings.panel.sections(tab).to_vec();
        if order.is_empty() {
            f.hint("hint.layout_empty");
        }
        for which in order {
            section(f, which, settings, status, devices, sel, entry, cmd);
        }
    }

    f.end_scroll();
    f.end();
}

/// Dispatches one section of the composable panel.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn section(
    f: &mut Frame<'_>,
    which: PanelSection,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    devices: &[&str],
    sel: &mut Selections,
    entry: &mut Entry,
    cmd: &mut UiCommands,
) {
    match which {
        PanelSection::Rig => section_rig(f, settings, status, sel, entry, cmd),
        PanelSection::Receiver => section_receiver(f, settings, status),
        PanelSection::Meter => section_meter(f, settings, status),
        PanelSection::Audio => section_audio(f, settings, status, devices, sel, cmd),
        PanelSection::Spectrum => section_spectrum(f, settings, status),
        PanelSection::Waterfall => section_waterfall(f, settings, status, cmd),
        PanelSection::CwChannels => section_cw_channels(f, settings, status, cmd),
        PanelSection::CwDecoder => section_cw_decoder(f, settings, status, cmd),
        PanelSection::RttyDecoder => section_rtty(f, settings, status),
        PanelSection::PskDecoder => section_psk(f, settings, status),
        PanelSection::Classifier => section_classifier(f, settings, status),
        PanelSection::Callsign => section_callsign(f, settings, status, cmd),
        PanelSection::Spots => section_spots(f, status, cmd),
        PanelSection::Monitor => section_monitor(f, settings, status, sel, cmd),
        // Placed last because the two arms above it are the ones that read.
    }
}

// ------------------------------------------------------------------ sections

/// Localization key of a link condition.
fn link_key(link: LinkState) -> &'static str {
    match link {
        LinkState::Closed => "link.closed",
        LinkState::Starting => "link.starting",
        LinkState::Up => "link.up",
        LinkState::Stalled => "link.stalled",
    }
}

/// Compact view of the six modem control lines.
///
/// A line with no reading shows a dash rather than a nought, because a
/// transport that cannot report on it and one that reports it low are different
/// facts, and only the first is true of a port that is not open.
fn signal_text(status: &RigStatus) -> String {
    let mut out = String::with_capacity(48);
    for signal in [
        Signal::Dtr,
        Signal::Rts,
        Signal::Cts,
        Signal::Dsr,
        Signal::Ring,
        Signal::Carrier,
    ] {
        if !out.is_empty() {
            out.push(' ');
        }
        let state = match status.signals.get(signal) {
            Some(true) => '1',
            Some(false) => '0',
            None => '-',
        };
        out.push_str(signal.name());
        out.push(state);
    }
    out
}

/// Transceiver control.
///
/// The dial frequency is what turns the audio spectrum into a picture of a
/// band. Everything below serves that: which description to speak, which port
/// to speak it on, and how to read what comes back.
#[allow(clippy::too_many_arguments)]
fn section_rig(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    sel: &mut Selections,
    entry: &mut Entry,
    cmd: &mut UiCommands,
) {
    /// Rates a transceiver menu offers. A list rather than a continuum, because
    /// that is what the far end accepts.
    const BAUD_RATES: [u32; 8] = [1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200];

    let active = settings.rig.enabled;
    let open = f.begin_group("group.rig");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        f.hint("hint.rig");
        f.toggle("field.rig.enabled", &mut settings.rig.enabled);
        f.begin_disabled(!settings.rig.enabled);

        // Readings first. An operator opening this section is normally checking
        // whether the link is alive, not reconfiguring it.
        let link = if status.rig.running {
            f.ui.tr(link_key(status.rig.link)).to_string()
        } else {
            f.ui.tr("status.no_link").to_string()
        };
        let link = if status.rig.shared {
            format!("{}  ({})", link, f.ui.tr("status.shared"))
        } else {
            link
        };
        f.readout("field.rig.link", &link);

        let frequency = match status.rig_frequency {
            Some(hz) => crate::rig::Readout::new(hz, 0).text(),
            None => "-".to_string(),
        };
        f.readout("field.rig.frequency", &frequency);
        f.readout(
            "field.rig.mode",
            if status.rig_mode.is_empty() { "-" } else { status.rig_mode },
        );
        f.readout(
            "field.rig.exchanges",
            &format!(
                "{}  err {}  stray {}",
                status.rig.counters.completed,
                status.rig.counters.faults,
                status.rig.counters.strays
            ),
        );
        f.readout("field.rig.lines", &signal_text(&status.rig));
        if status.rig_recoveries > 0 {
            f.readout("field.rig.recoveries", &format!("{}", status.rig_recoveries));
            f.hint("hint.rig_recovered");
        }
        if !status.rig.error.is_empty() {
            f.label_dim(&status.rig.error);
        }

        f.begin(Style::row().gap(gap));
        if f.button(if status.rig.running { "action.stop" } else { "action.start" }) {
            if status.rig.running {
                cmd.stop_rig = true;
            } else {
                cmd.start_rig = true;
            }
        }
        if f.button("action.rescan") {
            cmd.rescan_rig = true;
        }
        f.end();

        // Arriving at a frequency rather than moving towards one. The digits of
        // the readout are stepped and clicked, which is the wrong gesture for a
        // number read off a cluster or a schedule: it takes a dozen actions and
        // passes through eleven frequencies nobody asked to hear.
        f.begin_disabled(!status.rig_can_tune);
        if f.text_submit("field.rig.entry", &mut entry.frequency) {
            cmd.tune_typed = Some(entry.frequency.clone());
        }
        f.hint("hint.rig_entry");

        // Band stack. Laid out in fixed rows rather than by wrapping, because a
        // wrapping container reports the height of one line and the rows below
        // it would be drawn over.
        if !status.bands.is_empty() {
            const PER_ROW: usize = 4;
            for chunk in status.bands.chunks(PER_ROW) {
                f.begin(Style::row().gap(gap));
                for band in chunk {
                    // The name is the key as well as the caption, which is sound
                    // because the plan states each band once.
                    if f.button(band) {
                        cmd.tune_band = status.bands.iter().position(|b| b == band);
                    }
                }
                f.spacer(1.0);
                f.end();
            }
            f.hint("hint.rig_bands");
        }
        f.end_disabled();

        // Recording a frequency. The list is otherwise edited by hand, so an
        // operator who finds something worth returning to either interrupts the
        // session or loses it.
        f.begin_disabled(status.rig_frequency.is_none());
        if f.text_submit("field.rig.mark_label", &mut entry.label) {
            cmd.mark_station = true;
        }
        f.begin(Style::row().gap(gap));
        if f.button("action.mark") {
            cmd.mark_station = true;
        }
        f.spacer(1.0);
        f.end();
        f.end_disabled();
        f.hint("hint.rig_mark");

        // Catalogue. An unusable description is listed rather than hidden, so
        // an operator whose transceiver stopped appearing is told which file is
        // wrong instead of left to guess.
        if status.rig_profiles.is_empty() {
            f.hint("hint.rig_no_profiles");
        } else if f.combo("field.rig.profile", &mut sel.profile, status.rig_profiles) {
            cmd.select_rig_profile = true;
        }
        for issue in status.rig_issues.iter().take(4) {
            f.hint(issue);
        }
        f.text_edit("field.rig.profiles_path", &mut settings.rig.profiles_path);
        f.hint("hint.rig_profile");

        if status.rig_ports.is_empty() {
            f.hint("hint.rig_no_ports");
        } else if f.combo("field.rig.port", &mut sel.port, status.rig_ports) {
            cmd.select_rig_port = true;
        }

        // A rate the list does not carry is added to it, so a configuration
        // written by hand is displayed rather than silently replaced.
        let mut rates: Vec<u32> = BAUD_RATES.to_vec();
        if !rates.contains(&settings.rig.baud) {
            rates.push(settings.rig.baud);
            rates.sort_unstable();
        }
        let labels: Vec<String> = rates.iter().map(|r| r.to_string()).collect();
        let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
        let mut baud_index = rates.iter().position(|&r| r == settings.rig.baud).unwrap_or(0);
        if f.combo("field.rig.baud", &mut baud_index, &refs) {
            settings.rig.baud = rates[baud_index];
        }

        f.enum_combo("field.rig.transport", &mut settings.rig.transport);
        f.hint("hint.rig_shared");

        f.enum_combo("field.rig.dtr", &mut settings.rig.dtr);
        f.enum_combo("field.rig.rts", &mut settings.rig.rts);
        f.hint("hint.rig_lines");

        // A complex input carries the sign of the offset in the samples, so
        // there is no sideband to choose and the control does nothing. Greyed
        // rather than hidden, because the dependency is only visible next to
        // the setting that caused it.
        f.begin_disabled(status.complex_signal);
        let mut sideband = match settings.rig.sideband {
            SidebandMode::Auto => 0usize,
            SidebandMode::Upper => 1,
            SidebandMode::Lower => 2,
        };
        let sidebands = ["enum.sideband.auto", "enum.sideband.upper", "enum.sideband.lower"];
        if f.combo("field.rig.sideband", &mut sideband, &sidebands) {
            settings.rig.sideband = match sideband {
                1 => SidebandMode::Upper,
                2 => SidebandMode::Lower,
                _ => SidebandMode::Auto,
            };
        }
        f.end_disabled();
        if status.complex_signal {
            f.hint("hint.rig_no_sideband");
        }

        f.begin_disabled(status.complex_signal);
        f.slider("field.rig.cw_pitch", &mut settings.rig.cw_pitch_hz, 100.0, 3000.0, 0, "unit.hz");
        f.end_disabled();
        f.hint("hint.rig_pitch");
        f.hint("hint.rig_cw_reverse");

        f.slider(
            "field.rig.offset",
            &mut settings.rig.offset_hz,
            -100_000.0,
            100_000.0,
            0,
            "unit.hz",
        );
        f.hint("hint.rig_offset");

        f.toggle("field.rig.show_readout", &mut settings.rig.show_readout);
        f.begin_disabled(!settings.rig.show_readout);
        f.slider("field.rig.readout_scale", &mut settings.rig.readout_scale, 1.0, 8.0, 1, "");
        f.toggle("field.rig.readout_fine", &mut settings.rig.readout_fine);
        f.toggle("field.rig.readout_repeat", &mut settings.rig.readout_repeat);
        f.hint("hint.rig_readout");
        f.end_disabled();

        f.toggle("field.rig.rf_axis", &mut settings.rig.rf_axis);

        // Steps are decades, which is what a wheel outside a digit is expected
        // to move. A value the list does not carry is added, as above.
        let mut steps: Vec<u32> = vec![1, 10, 100, 1000, 10_000];
        if !steps.contains(&settings.rig.tune_step_hz) {
            steps.push(settings.rig.tune_step_hz);
            steps.sort_unstable();
        }
        let step_labels: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
        let step_refs: Vec<&str> = step_labels.iter().map(|s| s.as_str()).collect();
        let mut step_index = steps
            .iter()
            .position(|&s| s == settings.rig.tune_step_hz)
            .unwrap_or(0);
        if f.combo("field.rig.tune_step", &mut step_index, &step_refs) {
            settings.rig.tune_step_hz = steps[step_index];
        }

        // Clicking to move the transceiver and clicking to point a decoder are
        // different intentions, so only one of them can be the meaning of a
        // plain click. A description that cannot set a frequency leaves the
        // choice dead rather than silently ignored.
        f.begin_disabled(!status.rig_can_tune);
        f.toggle("field.rig.click_tunes", &mut settings.rig.click_tunes);
        f.end_disabled();
        f.hint("hint.rig_click");

        f.end_disabled();
    }
    f.end_group();
}

/// Receiver chain.
///
/// Everything below the mode is dead outside the receiver mode, and greyed
/// rather than hidden: several of these settings destroy what the keying
/// detectors depend on, and the dependency is only visible beside the control
/// that carries it.
fn section_receiver(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let active = settings.sdr_mode();
    let open = f.begin_group("group.receiver");
    f.mark_active(active);
    if open {
        f.hint("hint.receiver_mode");
        f.enum_combo("field.receiver.mode", &mut settings.receiver.mode);

        // The quadrature front end sits above the mode gate deliberately. It
        // decides whether the display has two sides at all, which is a property
        // of the wiring rather than of what the audio is being used for, and a
        // control unreachable in the skimmer would make the two sided waterfall
        // unreachable there with it.
        f.toggle("field.receiver.iq_input", &mut settings.receiver.iq_input);
        f.begin_disabled(!settings.receiver.iq_input);
        // A mono device duplicates its single channel, so the pair is two copies
        // of one thing: the correction has nothing to correct and the spectrum
        // comes out symmetric about nought whatever is set here.
        if settings.receiver.iq_input && status.device_channels < 2 {
            f.hint("hint.receiver_needs_stereo");
        }
        f.toggle("field.receiver.iq_swap", &mut settings.receiver.iq_swap);
        f.slider("field.receiver.iq_gain", &mut settings.receiver.iq_gain_db, -12.0, 12.0, 2, "unit.db");
        f.slider(
            "field.receiver.iq_phase",
            &mut settings.receiver.iq_phase_deg,
            -45.0,
            45.0,
            2,
            "unit.deg",
        );
        f.end_disabled();
        f.hint("hint.receiver_iq");

        let sdr = settings.sdr_mode();
        let iq = settings.receiver.iq_input;
        let complex = status.complex_signal;
        let nyquist = status.nyquist_hz.max(400.0);

        if iq && !sdr {
            f.hint("hint.receiver_iq_skimmer");
        }
        // A complex signal has a two sided spectrum, so a filter edge below the
        // tuning point is meaningful. Demodulated audio is entirely positive,
        // and an edge below nought there would only pass the mirror image of
        // what is already present.
        let floor = if complex { -nyquist } else { 0.0 };

        f.begin_disabled(!sdr);

        // The full list, always. The detector is local processing and the
        // transceiver mode is a command on a wire; they are coupled by the
        // setting below when the operator asks for it, and are otherwise two
        // separate facts.
        f.enum_combo("field.receiver.detector", &mut settings.receiver.detector);
        f.enum_combo("field.receiver.mode_link", &mut settings.receiver.mode_link);
        f.hint("hint.receiver_mode_link");

        // Amplitude and frequency detection operate on a modulated carrier.
        // Audio from a transceiver has none, because the transceiver detected
        // it already, and detecting the result a second time produces noise.
        if !iq && settings.receiver.detector.needs_carrier() {
            f.hint("hint.receiver_needs_iq");
        }
        // Selecting a sideband means discarding one half of a two sided
        // spectrum, and a single channel input has one half. The setting is not
        // refused, because it still drives the preset and the transceiver, but
        // it changes nothing in the audio.
        if !iq && settings.receiver.detector.is_sideband() {
            f.hint("hint.receiver_real_input");
        }
        // The lower sideband lives below nought on a complex signal, so the
        // preset places the filter there. Worth saying, because a filter whose
        // edges are both negative reads as a mistake until the reason is known.
        if complex && settings.receiver.detector.is_lower_sideband() {
            f.hint("hint.receiver_lower_negative");
        }

        // The tuning point, and it exists only for a complex signal. Audio a
        // transceiver demodulated has none: that transceiver tuned, and the
        // chain here is a filter, which by construction leaves the pitch of
        // what it passes alone.
        //
        // The switch above the slider is the lock. Off, the oscillator is held
        // at nought and the receiver sits on the dial, so the readout, the
        // display and the far end name one frequency; a click on the display
        // then moves the transceiver, which is what keeps the gesture useful.
        // On, the two are free to move apart, which is what a panoramic
        // receiver wants and is also how they stop agreeing.
        f.begin_disabled(!complex);
        f.toggle("field.receiver.tune_enabled", &mut settings.receiver.tune_enabled);
        f.end_disabled();

        f.begin_disabled(!complex || !settings.receiver.tune_enabled);
        f.slider(
            "field.receiver.tune",
            &mut settings.receiver.tune_hz,
            -nyquist,
            nyquist,
            0,
            "unit.hz",
        );
        f.end_disabled();

        if !complex {
            f.hint("hint.receiver_no_tune");
        } else if settings.receiver.tune_enabled {
            f.hint("hint.receiver_tune");
        } else {
            f.hint("hint.receiver_tune_locked");
        }
        f.readout(
            "field.receiver.reference",
            &format!("{:.0} {}", status.receiver_reference_hz, f.ui.tr("unit.hz")),
        );

        // The beat oscillator. Keying on a complex signal is symmetric about
        // the tuning point, so a signal sitting there emerges at nought hertz;
        // on a real input the transceiver already supplied a pitch.
        f.begin_disabled(!complex || !settings.receiver.detector.is_keyed());
        f.slider(
            "field.receiver.bfo",
            &mut settings.receiver.cw_pitch_hz,
            200.0,
            1500.0,
            0,
            "unit.hz",
        );
        f.end_disabled();

        // The preset is applied automatically when the mode changes; the button
        // is for returning to it after the edges have been moved by hand.
        if f.button("action.preset") {
            let complex_now = settings.complex_signal();
            let (low, high) = settings.receiver.detector.filter_preset(complex_now);
            settings.receiver.filter_low_hz = low;
            settings.receiver.filter_high_hz = high;
        }

        // Absolute audio on a real input, measured from the tuning point on a
        // complex one, which is why the floor differs.
        f.slider(
            "field.receiver.filter_low",
            &mut settings.receiver.filter_low_hz,
            floor,
            nyquist - 50.0,
            0,
            "unit.hz",
        );
        f.slider(
            "field.receiver.filter_high",
            &mut settings.receiver.filter_high_hz,
            floor + 50.0,
            nyquist,
            0,
            "unit.hz",
        );
        f.hint("hint.receiver_filter");
        f.hint("hint.receiver_filter_move");
        let (band_lo, band_hi) = settings.receiver.absolute_band();
        f.readout(
            "field.receiver.width",
            &format!(
                "{:.0} {}   {:.0} .. {:.0}",
                settings.receiver.filter_high_hz - settings.receiver.filter_low_hz,
                f.ui.tr("unit.hz"),
                band_lo,
                band_hi
            ),
        );
        f.readout(
            "field.receiver.listening",
            &format!("{:.0} {}", status.receiver_listen_hz, f.ui.tr("unit.hz")),
        );

        // What the chain is doing, so the settings above can be judged against
        // something rather than adjusted blind. The chain runs inside the
        // monitor, so with listening switched off there is nothing to report and
        // nothing to hear; saying so is more useful than a row of zeros.
        let m = &status.monitor;
        if m.receiver && m.running {
            f.readout(
                "field.receiver.state",
                &format!(
                    "{:+.0} dB  {}{}",
                    m.rx_gain_db,
                    if m.rx_open { f.ui.tr("gate.open") } else { f.ui.tr("status.muted") },
                    if m.rx_locked { "  lock" } else { "" }
                ),
            );
            f.readout("field.receiver.blanked", &format!("{}", m.rx_blanked));
            if settings.receiver.detector == crate::config::settings::Detector::Sam {
                f.readout(
                    "field.receiver.carrier",
                    &format!("{:+.1} {}", m.rx_offset_hz, f.ui.tr("unit.hz")),
                );
            }
        } else {
            f.hint("hint.receiver_needs_monitor");
        }

        f.toggle("field.receiver.nb_wide", &mut settings.receiver.nb_wide);
        f.begin_disabled(!settings.receiver.nb_wide);
        f.slider(
            "field.receiver.nb_wide_threshold",
            &mut settings.receiver.nb_wide_threshold,
            1.0,
            40.0,
            1,
            "",
        );
        f.end_disabled();
        f.toggle("field.receiver.nb_narrow", &mut settings.receiver.nb_narrow);
        f.begin_disabled(!settings.receiver.nb_narrow);
        f.slider(
            "field.receiver.nb_narrow_threshold",
            &mut settings.receiver.nb_narrow_threshold,
            1.0,
            40.0,
            1,
            "",
        );
        f.end_disabled();
        f.hint("hint.receiver_blankers");

        f.toggle("field.receiver.nr", &mut settings.receiver.nr_enabled);
        f.begin_disabled(!settings.receiver.nr_enabled);
        f.slider("field.receiver.nr_strength", &mut settings.receiver.nr_strength, 0.0, 1.0, 2, "");
        f.enum_combo("field.receiver.nr_method", &mut settings.receiver.nr_method);
        f.hint("hint.receiver_nr_method");
        f.end_disabled();

        f.toggle("field.receiver.notch", &mut settings.receiver.notch_enabled);
        f.begin_disabled(!settings.receiver.notch_enabled);
        // The switch lives under the transform section in the configuration
        // because that is where it was written, and moving it would change what
        // an existing file means. It is shown here because this is the control
        // it acts on, and a switch shown apart from what it does is a switch
        // nobody connects to anything.
        f.toggle("field.receiver.auto_notch", &mut settings.dsp.auto_notch);
        f.hint("hint.receiver_auto_notch");
        f.begin_disabled(settings.dsp.auto_notch);
        f.slider("field.receiver.notch_hz", &mut settings.receiver.notch_hz, 50.0, nyquist, 0, "unit.hz");
        f.end_disabled();
        f.slider(
            "field.receiver.notch_width",
            &mut settings.receiver.notch_width_hz,
            10.0,
            500.0,
            0,
            "unit.hz",
        );
        if settings.complex_signal() {
            f.hint("hint.receiver_notch_two_sided");
        }
        f.end_disabled();

        f.toggle("field.receiver.agc", &mut settings.receiver.agc_enabled);
        f.begin_disabled(!settings.receiver.agc_enabled);
        f.slider("field.receiver.agc_attack", &mut settings.receiver.agc_attack_ms, 0.1, 200.0, 1, "unit.ms");
        f.slider("field.receiver.agc_hang", &mut settings.receiver.agc_hang_ms, 0.0, 5000.0, 0, "unit.ms");
        f.slider(
            "field.receiver.agc_release",
            &mut settings.receiver.agc_release_ms,
            10.0,
            8000.0,
            0,
            "unit.ms",
        );
        f.slider("field.receiver.agc_target", &mut settings.receiver.agc_target_db, -60.0, 0.0, 0, "unit.db");
        f.end_disabled();
        f.hint("hint.receiver_hang");

        f.toggle("field.receiver.squelch", &mut settings.receiver.squelch_enabled);
        f.begin_disabled(!settings.receiver.squelch_enabled);
        f.slider("field.receiver.squelch_db", &mut settings.receiver.squelch_db, -140.0, 0.0, 0, "unit.db");
        f.end_disabled();

        f.end_disabled();
    }
    f.end_group();
}

fn section_meter(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    if f.begin_group("group.meter") {
        // The strip stretches to the content width of the group, which is what
        // the drawing pass measures its own scale against; the group padding is
        // what keeps the end labels clear of the frame.
        if settings.ui.show_meter {
            f.custom(TAG_METER, Style::row().height_px(f.ui.m(30.0)));
        }
        f.readout("field.meter.reading", &status.meter_text);
        // The number the reference is set against. Without it the calibration is
        // a search: the scale below states a level in dBFS and nothing on screen
        // says what the input is delivering, which at a wide capture span and a
        // narrow measurement differ by thirty decibels.
        f.readout(
            "field.meter.raw",
            &format!("{:.1} {}", status.audio.rms_db, f.ui.tr("unit.dbfs")),
        );
        f.enum_combo("field.meter.scale", &mut settings.meter.scale);
        // The reference only means anything on a scale that is calibrated
        // against it; on a full scale readout there is nothing to refer to.
        f.begin_disabled(settings.meter.scale == MeterScale::DbFs);
        f.slider(
            "field.meter.s9_reference",
            &mut settings.meter.s9_reference_dbfs,
            -120.0,
            0.0,
            0,
            "unit.dbfs",
        );
        f.end_disabled();
        f.slider(
            "field.meter.calibration",
            &mut settings.meter.calibration_db,
            -60.0,
            60.0,
            1,
            "unit.db",
        );
        f.slider("field.meter.attack", &mut settings.meter.attack_ms, 0.5, 200.0, 0, "unit.ms");
        f.slider("field.meter.release", &mut settings.meter.release_ms, 10.0, 2000.0, 0, "unit.ms");
        // The band being worked rather than the whole passband. Taken as a ratio
        // against that passband, so the calibration above stays valid: a reading
        // narrowed by a different scale would need calibrating again.
        f.toggle("field.meter.narrow", &mut settings.meter.narrow_band_measure);
        f.hint("hint.meter_narrow");
        f.hint("hint.meter_calibrate");
        f.toggle("field.meter.peak_hold", &mut settings.meter.show_peak);
        f.begin_disabled(!settings.meter.show_peak);
        f.slider(
            "field.meter.peak_hold_time",
            &mut settings.meter.peak_hold_ms,
            0.0,
            5000.0,
            0,
            "unit.ms",
        );
        f.end_disabled();
    }
    f.end_group();
}

/// Whole number chosen from a list.
///
/// A rate and a transform size are each one of a handful of values the hardware
/// or the arithmetic accepts, and everything between two of them is refused
/// outright or rounded on load. A track cannot state that: it offers a continuum
/// and resolves to the width of one pixel, so at a hundred and ninety two
/// thousand over a few hundred pixels the useful values cannot be landed on at
/// all and the control can only be set wrong.
///
/// A value the list does not carry is added to it, so a configuration written by
/// hand is displayed rather than silently replaced.
fn choice_u32(
    f: &mut Frame<'_>,
    key: &str,
    value: &mut u32,
    offered: &[u32],
    zero_key: &str,
) -> bool {
    let mut list: Vec<u32> = offered.to_vec();
    if !list.contains(value) {
        list.push(*value);
    }
    list.sort_unstable();
    list.dedup();

    // Nought is a request and not a rate, so it is named rather than printed: a
    // list holding a plain zero reads as a rate nothing can produce.
    let labels: Vec<String> = list
        .iter()
        .map(|&v| if v == 0 { zero_key.to_string() } else { v.to_string() })
        .collect();
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

    let mut index = list.iter().position(|&v| v == *value).unwrap_or(0);
    if f.combo(key, &mut index, &refs) {
        *value = list[index];
        return true;
    }
    false
}

fn section_audio(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    devices: &[&str],
    sel: &mut Selections,
    cmd: &mut UiCommands,
) {
    let active = status.audio_running;
    let open = f.begin_group("group.audio");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);
        let wasapi = settings.audio.backend == AudioBackend::Wasapi;

        if f.enum_combo("field.audio.backend", &mut settings.audio.backend) {
            cmd.rescan_devices = true;
            cmd.start_audio = true;
        }
        if f.combo("field.audio.device", &mut sel.device, devices) {
            cmd.select_device = true;
        }
        if status.default_device {
            f.hint("hint.default_device");
        }

        f.begin(Style::row().gap(gap));
        if f.button(if status.audio_running { "action.stop" } else { "action.start" }) {
            if status.audio_running {
                cmd.stop_audio = true;
            } else {
                cmd.start_audio = true;
            }
        }
        if f.button("action.rescan") {
            cmd.rescan_devices = true;
        }
        f.end();

        f.enum_combo("field.audio.channel", &mut settings.audio.channel_mode);
        f.slider("field.audio.gain", &mut settings.audio.input_gain_db, -40.0, 40.0, 1, "unit.db");
        f.slider_u32("field.audio.period", &mut settings.audio.capture_buffer_ms, 2, 200, "unit.ms");
        f.slider("field.audio.ring", &mut settings.audio.ring_seconds, 0.25, 60.0, 2, "unit.s");

        // The device rate is a request the legacy interface honours or refuses
        // outright. Shared mode always hands back the mix format instead, so
        // the control would be inert and is shown as such.
        f.begin_disabled(wasapi);
        choice_u32(
            f,
            "field.audio.requested_rate",
            &mut settings.audio.sample_rate,
            &DEVICE_RATES,
            "enum.follow_device",
        );
        f.end_disabled();
        if wasapi {
            f.hint("hint.audio_shared_rate");
        }

        choice_u32(
            f,
            "field.audio.dsp_rate",
            &mut settings.audio.dsp_sample_rate,
            &DSP_RATES,
            "enum.auto",
        );
        f.hint("hint.dsp_rate");
        // What the path settled on. The reduction under the transform section
        // divides it and the resampler clamps it, so a stated rate and an actual
        // one are two different numbers and only the second is what the decoders
        // measure against.
        if status.source_rate != settings.audio.dsp_sample_rate {
            f.readout(
                "field.audio.effective_rate",
                &format!("{} {}", status.source_rate, f.ui.tr("unit.hz")),
            );
        }
        f.toggle("field.audio.dc_block", &mut settings.audio.dc_block);

        // Exclusive mode takes the endpoint away from the mixer, which the
        // loopback path depends on, so the two cannot coexist. The legacy
        // interface has no such mode at all.
        f.begin_disabled(!wasapi || status.loopback);
        f.toggle("field.audio.exclusive", &mut settings.audio.exclusive_mode);
        f.end_disabled();

        // Stated only once it has happened. A permanent nought would be noise,
        // and a figure that starts climbing is the one thing worth noticing here.
        if status.audio_recoveries > 0 {
            f.readout("field.audio.recoveries", &format!("{}", status.audio_recoveries));
            f.hint("hint.audio_recovered");
        }
        // The one reading that separates a two sided display from a folded one.
        // A mono endpoint duplicates its channel, and the duplicate cannot be
        // told from a quadrature pair by anything downstream.
        f.readout("field.audio.channels_in", &format!("{}", status.device_channels));
        if settings.receiver.iq_input && status.device_channels < 2 {
            f.hint("hint.audio_iq_mono");
        }
        f.readout("field.audio.queue", &format!("{:.0} %", status.queue_fill * 100.0));
        // Sample count and discontinuities together separate a stream that
        // never started from one that keeps losing its place.
        f.readout(
            "field.audio.frames",
            &format!("{}  gaps {}", status.audio.frames, status.audio.discontinuities),
        );
    }
    f.end_group();
}

fn section_spectrum(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    if f.begin_group("group.spectrum") {
        f.enum_combo("field.spectrum.window", &mut settings.dsp.fft_window);
        // Beta shapes the Kaiser window and means nothing to any other.
        f.begin_disabled(settings.dsp.fft_window != WindowFn::Kaiser);
        f.slider("enum.kaiser", &mut settings.dsp.kaiser_beta, 0.1, 20.0, 1, "");
        f.end_disabled();
        // A power of two or nothing: a value between two of them is rounded up
        // on load, so a track offers positions that cannot be reached.
        choice_u32(f, "field.spectrum.fft_size", &mut settings.dsp.fft_size, &FFT_SIZES, "enum.auto");
        f.toggle("field.spectrum.zoom_resolution", &mut settings.dsp.zoom_resolution);
        f.hint("hint.zoom_resolution");
        // The width the path settled on rather than the one that was stated, so
        // the effect of the widening is visible where the setting is.
        f.readout(
            "field.spectrum.resolution",
            &format!("{:.2} {}", status.bin_hz, f.ui.tr("unit.hz")),
        );
        f.slider_u32("field.spectrum.average", &mut settings.dsp.average_frames, 1, 64, "");
        f.slider_u32("field.spectrum.overlap", &mut settings.dsp.overlap_percent, 0, 90, "unit.percent");
        f.hint("hint.overlap");

        // The passband bounds where a decoder channel may be opened, which is a
        // skimmer question. Greyed in the receiver mode rather than hidden: it
        // is the same setting and it takes effect again the moment the mode
        // changes back.
        f.begin_disabled(settings.sdr_mode());
        f.slider(
            "field.spectrum.passband_low",
            &mut settings.dsp.passband_low_hz,
            0.0,
            3000.0,
            0,
            "unit.hz",
        );
        f.slider(
            "field.spectrum.passband_high",
            &mut settings.dsp.passband_high_hz,
            300.0,
            status.nyquist_hz.max(400.0),
            0,
            "unit.hz",
        );
        f.end_disabled();
        f.hint("hint.passband");

        // The bound that applies to a quadrature input, where the pair above
        // describes nothing: it is stated in the terms of an audio path three
        // kilohertz wide.
        f.begin_disabled(settings.sdr_mode() || !status.complex_signal);
        f.slider(
            "field.spectrum.search_span",
            &mut settings.dsp.search_span_hz,
            0.0,
            status.nyquist_hz.max(1000.0),
            0,
            "unit.hz",
        );
        f.end_disabled();
        if status.complex_signal && !settings.sdr_mode() {
            f.hint("hint.search_span");
        }

        // Ahead of the transform, so it serves the display and the decoders. The
        // two under the receiver section are on the monitor thread and serve the
        // ear; the samples this path carries never meet them.
        f.toggle("field.spectrum.blanker", &mut settings.dsp.noise_blanker);
        f.begin_disabled(!settings.dsp.noise_blanker);
        f.slider(
            "field.spectrum.blanker_threshold",
            &mut settings.dsp.noise_blanker_threshold,
            1.5,
            40.0,
            1,
            "",
        );
        if status.blanked > 0 {
            f.readout("field.spectrum.blanked", &format!("{}", status.blanked));
        }
        f.end_disabled();
        f.hint("hint.blanker");

        // The reduction composes with the rate above rather than replacing it,
        // which is what makes it worth having: halving is one step of a small
        // integer and retyping a rate is not.
        f.slider_u32("field.spectrum.decimation", &mut settings.dsp.decimation, 1, 16, "");
        f.hint("hint.decimation");
        f.hint("hint.restart");

        f.readout("field.spectrum.workers", &format!("{}", status.worker_threads));
        f.hint("hint.workers");
    }
    f.end_group();
}

fn section_waterfall(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.waterfall") {
        let gap = f.ui.m(f.ui.theme.gap);

        // Magnification first, because it is the control an operator reaches
        // for and the one that is otherwise invisible: the gesture works
        // without it, and a gesture nobody knows about is a feature nobody has.
        f.slider("field.waterfall.zoom", &mut settings.waterfall.zoom, 1.0, ZOOM_MAX, 1, "");
        let span = if status.view_span_hz >= 2000.0 {
            format!("{:.2} {}", status.view_span_hz / 1000.0, f.ui.tr("unit.khz"))
        } else {
            format!("{:.0} {}", status.view_span_hz, f.ui.tr("unit.hz"))
        };
        f.readout("field.waterfall.span", &span);
        f.begin(Style::row().gap(gap));
        f.begin_disabled(status.view_full);
        if f.button("action.reset_zoom") {
            cmd.reset_zoom = true;
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();
        f.hint("hint.zoom");

        // One saved view beside the live one. The magnification and the centre
        // above are already carried across a restart, so this is a bookmark
        // rather than a second statement of the same thing: somewhere to return
        // to after looking elsewhere.
        f.begin(Style::row().gap(gap));
        if f.button("action.store_view") {
            cmd.store_view = true;
        }
        if f.button("action.recall_view") {
            cmd.recall_view = true;
        }
        f.spacer(1.0);
        f.end();
        f.readout(
            "field.waterfall.stored",
            &format!(
                "{:.2} kHz at {:.0} Hz",
                settings.waterfall.span_hz / 1000.0,
                settings.waterfall.center_hz
            ),
        );

        // The measurement. Cleared from here rather than by a second gesture on
        // the display, because a gesture that removes a marker has to be
        // distinguishable from one that moves it and there is no button left.
        f.begin_disabled(status.reference_hz.is_none());
        if f.button("action.clear_reference") {
            cmd.clear_reference = true;
        }
        f.end_disabled();
        f.hint("hint.reference");
        if !status.complex_signal {
            f.hint("hint.span_real");
        }

        f.enum_combo("field.waterfall.palette", &mut settings.waterfall.colormap);
        f.enum_combo("field.waterfall.style", &mut settings.waterfall.style);

        let skimmer = settings.waterfall.style == WaterfallStyle::Skimmer;
        f.slider("field.waterfall.floor", &mut settings.waterfall.min_db, -180.0, -20.0, 0, "unit.db");
        f.slider("field.waterfall.ceiling", &mut settings.waterfall.max_db, -140.0, 20.0, 0, "unit.db");
        f.slider("field.waterfall.gamma", &mut settings.waterfall.gamma, 0.2, 4.0, 2, "");
        // Per line mapping already rides the noise, so the slow tracker has
        // nothing left to correct.
        f.begin_disabled(skimmer);
        f.toggle("field.waterfall.auto_range", &mut settings.waterfall.auto_range);
        f.end_disabled();
        f.slider("field.waterfall.smoothing", &mut settings.waterfall.smoothing, 0.0, 0.95, 2, "");
        f.slider(
            "field.waterfall.speed",
            &mut settings.waterfall.scroll_lines_per_second,
            1.0,
            200.0,
            0,
            "unit.lps",
        );
        // The rate that resulted, because the overlap sets a floor under it and
        // the two controls together do not say which of them is acting.
        f.readout(
            "field.waterfall.actual_speed",
            &format!("{:.0} {}", status.line_rate, f.ui.tr("unit.lps")),
        );
        if (status.line_rate - settings.waterfall.scroll_lines_per_second).abs() > 1.0 {
            f.hint("hint.speed_from_overlap");
        }
        f.slider(
            "field.waterfall.trace",
            &mut settings.waterfall.spectrum_height_fraction,
            0.1,
            0.8,
            2,
            "",
        );
        f.toggle("field.waterfall.trace_visible", &mut settings.waterfall.spectrum_visible);
        // The held trace and the level grid are only drawn over the live trace,
        // so they need it.
        f.begin_disabled(!settings.waterfall.spectrum_visible);
        f.toggle("field.waterfall.level_grid", &mut settings.waterfall.show_level_grid);
        f.toggle("field.waterfall.held", &mut settings.waterfall.show_held_trace);
        f.toggle("field.waterfall.peak_hold", &mut settings.waterfall.peak_hold);
        f.toggle("field.waterfall.average", &mut settings.waterfall.show_average_trace);
        f.hint("hint.average_trace");
        f.end_disabled();
        f.toggle("field.waterfall.grid", &mut settings.waterfall.show_grid);
        f.begin_disabled(!settings.waterfall.show_grid);
        f.toggle("field.waterfall.labels", &mut settings.waterfall.show_labels);
        f.end_disabled();
        f.toggle("field.waterfall.markers", &mut settings.waterfall.mark_decoders);
        f.enum_combo("field.waterfall.anchor", &mut settings.waterfall.anchor);
        let anchored = settings.waterfall.anchor != AnchorMode::Off;
        if anchored && !settings.rig.rf_axis {
            f.hint("hint.anchor_needs_rf_axis");
        } else {
            f.hint("hint.anchor");
        }

        f.toggle("field.waterfall.smooth", &mut settings.waterfall.smooth);
        f.hint("hint.smooth");

        // The stored width is a property of the texture, which cannot be resized
        // without discarding the history it holds.
        f.begin_disabled(true);
        let mut columns = status.waterfall_columns;
        f.slider_u32("field.waterfall.columns", &mut columns, 0, 16384, "");
        f.end_disabled();
        f.hint("hint.columns");
        f.hint("hint.restart");
        f.toggle("field.waterfall.stations", &mut settings.waterfall.show_stations);
        f.toggle("field.waterfall.cursor", &mut settings.waterfall.show_cursor_readout);
        f.begin_disabled(!settings.appearance.axis_gutters);
        f.toggle("field.waterfall.time_axis", &mut settings.waterfall.time_axis);
        f.end_disabled();
        if settings.appearance.axis_gutters {
            f.hint("hint.time_axis");
        } else {
            f.hint("hint.time_axis_needs_gutter");
        }

        // Storage rather than presentation, so it sits below the overlay
        // switches rather than among them. Dead on a running session because
        // switching means a differently formatted texture and the history
        // already stored cannot be reinterpreted; shown because the operator has
        // to be able to see which arrangement is in force.
        f.toggle("field.waterfall.gpu_palette", &mut settings.waterfall.gpu_palette);
        f.hint("hint.gpu_palette");
        f.hint("hint.restart");
    }
    f.end_group();
}

fn section_cw_channels(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.cw_channels") {
        let gap = f.ui.m(f.ui.theme.gap);

        // The keying detectors are not fed in the receiver mode. That is not a
        // convenience: they derive a threshold from the envelope and measure
        // element durations against it, and the receiver chain filters, reduces
        // noise and applies a gain loop, each of which destroys the measurement.
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.decoder_off_in_sdr");
        }
        f.begin_disabled(sdr);

        f.hint("hint.cw_channels");
        f.hint("hint.cw_drag");
        f.toggle("field.cw.multi_channel", &mut settings.morse.multi_channel);
        f.begin_disabled(!settings.morse.multi_channel);
        f.slider_u32("field.cw.max_channels", &mut settings.morse.max_channels, 1, 8, "");
        f.slider("field.cw.spacing", &mut settings.morse.channel_spacing_hz, 40.0, 1000.0, 0, "unit.hz");
        f.end_disabled();

        if status.channels.is_empty() {
            f.hint("hint.no_carrier");
        }
        for ch in status.channels {
            // Every row is built from the same keys, so it needs an identity
            // scope of its own; without one the buttons of all rows would share
            // their pressed state, and the label column would be measured across
            // rows that do not belong together.
            f.ui.begin_scope(&format!("cwch{}", ch.id));
            f.begin(Style::row().gap(gap).align(Align::Center));

            if f.button(&format!("{:.0} {}", ch.hz, f.ui.tr("unit.hz"))) {
                cmd.focus_channel = Some(ch.id);
            }
            let color = if ch.focused {
                f.ui.theme.accent
            } else if ch.present {
                f.ui.theme.text
            } else {
                f.ui.theme.text_disabled
            };
            // Elements against rejects is the pair that separates a detector
            // hearing nothing from one hearing the wrong thing, and the
            // confidence beside them is what the print threshold gates on. The
            // two suppressions are different: an open gate with a low figure is
            // a channel assembling patterns the table does not recognize, which
            // is not the same condition as one hearing nothing at all.
            let row = format!(
                "{:>4.0}Hz {:>3.0}w {:>4.1}dB {:>3}el {:>3}rj {:>3.0}% {:<5}{}",
                ch.width_hz,
                ch.wpm,
                ch.snr_db,
                ch.elements.min(999),
                ch.rejects.min(999),
                ch.confidence * 100.0,
                f.ui.tr(ch.gate.key()),
                if ch.pinned { "*" } else { "" }
            );
            f.label_mono(&row, color, TextAlign::Left, Style::row().grow(1.0).shrink(1.0));
            if f.button_flat("action.close") {
                cmd.drop_channel = Some(ch.id);
            }

            f.end();
            f.ui.end_scope();
        }

        f.end_disabled();
    }
    f.end_group();
}

fn section_cw_decoder(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    let active = settings.morse.enabled && !settings.sdr_mode();
    let open = f.begin_group("group.cw_decoder");
    f.mark_active(active);
    if open {
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.decoder_off_in_sdr");
        }
        f.begin_disabled(sdr);

        f.hint("hint.cw_gate");
        f.hint("hint.cw_bandwidth");
        f.readout("field.cw.detected", &format!("{:.1} {}", status.decoder.wpm, f.ui.tr("unit.wpm")));
        // Both ends of the ratio. The level above the floor is what the gate
        // reads and what keeps its meaning; the floor beside it is what says
        // whether a reading of eight decibels is a weak signal or the spread of
        // the noise itself.
        f.readout(
            "field.cw.snr",
            &format!(
                "{:.1} dB over {:.1}  duty {:.2}",
                status.decoder.cw_snr_db, status.decoder.cw_floor_db, status.decoder.cw_duty
            ),
        );
        f.readout(
            "field.cw.gate",
            &format!(
                "{}  {:.1} dB",
                f.ui.tr(status.decoder.cw_gate.key()),
                status.decoder.cw_level_db
            ),
        );
        f.readout(
            "field.cw.centre",
            &format!(
                "{:.1} Hz  afc {:+.1} Hz",
                status.decoder.cw_tone_hz, status.decoder.cw_afc_hz
            ),
        );

        f.toggle("field.cw.enabled", &mut settings.morse.enabled);
        f.begin_disabled(!settings.morse.enabled);

        // Automatic tone selection drives the single channel case only; with a
        // bank the allocator decides and the control would do nothing.
        let multi = settings.morse.multi_channel && settings.morse.max_channels > 1;
        f.begin_disabled(multi);
        f.toggle("field.cw.auto_tone", &mut settings.morse.auto_tone);
        f.end_disabled();

        f.toggle("field.cw.afc", &mut settings.morse.afc);
        f.begin_disabled(multi || settings.morse.auto_tone);
        // A quadrature input has a band below the tuning point as well, and it
        // is half of what the receiver captured.
        let ceiling = status.nyquist_hz.max(200.0);
        let floor = if status.complex_signal { -ceiling } else { 50.0 };
        f.slider("field.cw.tone", &mut settings.morse.tone_hz, floor, ceiling, 0, "unit.hz");
        f.end_disabled();
        f.slider("field.cw.capture", &mut settings.morse.capture_range_hz, 10.0, 500.0, 0, "unit.hz");
        if f.slider(
            "field.cw.bandwidth",
            &mut settings.morse.filter_bandwidth_hz,
            BANDWIDTH_MIN_HZ,
            BANDWIDTH_MAX_HZ,
            0,
            "unit.hz",
        ) {
            cmd.channel_width_hz = Some(settings.morse.filter_bandwidth_hz);
        }
        f.hint("hint.cw_width");
        f.readout(
            "field.cw.effective",
            &format!("{:.0} Hz  bin {:.1} Hz", status.decoder.cw_bandwidth_hz, status.bin_hz),
        );

        f.toggle("field.cw.auto_speed", &mut settings.morse.auto_speed);
        let (lo, hi) = (settings.morse.wpm_min, settings.morse.wpm_max);
        f.slider("field.cw.speed", &mut settings.morse.wpm, lo, hi, 1, "unit.wpm");
        f.begin_disabled(!settings.morse.auto_speed);
        f.slider("field.cw.tracking", &mut settings.morse.speed_tracking, 0.0, 1.0, 2, "");
        f.hint("hint.cw_tracking");
        f.end_disabled();

        // Farnsworth stretches the gaps and leaves the elements alone, so the
        // speed estimate stays right and the word boundary does not. The observed
        // character gap is what says which is arriving, so it is stated beside the
        // switch rather than left to be inferred.
        f.toggle("field.cw.farnsworth", &mut settings.morse.farnsworth_aware);
        if status.decoder.cw_char_gap_units > 0.0 {
            f.readout(
                "field.cw.char_gap",
                &format!("{:.1} units", status.decoder.cw_char_gap_units),
            );
        }
        f.hint("hint.cw_farnsworth");

        f.slider("field.cw.squelch", &mut settings.morse.squelch_db, -140.0, 0.0, 0, "unit.db");
        f.hint("hint.cw_squelch");
        f.slider("field.cw.min_snr", &mut settings.morse.min_snr_db, -10.0, 40.0, 1, "unit.db");
        f.hint("hint.cw_min_snr");
        f.slider("field.cw.print_above", &mut settings.morse.print_threshold, 0.0, 1.0, 2, "");
        f.readout(
            "field.cw.quality",
            &format!(
                "{:.0} %  singles {:.0} %",
                status.decoder.cw_quality * 100.0,
                status.decoder.cw_singles * 100.0
            ),
        );
        f.hint("hint.cw_quality");
        f.readout("field.cw.confidence", &format!("{:.0} %", status.decoder.cw_confidence * 100.0));
        f.enum_combo("field.cw.case", &mut settings.morse.output_case);
        f.toggle("field.cw.prosigns", &mut settings.morse.show_prosigns);

        f.end_disabled();
        f.end_disabled();
    }
    f.end_group();
}

fn section_rtty(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let active = settings.rtty.enabled && !settings.sdr_mode();
    let open = f.begin_group("group.rtty_decoder");
    f.mark_active(active);
    if open {
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.decoder_off_in_sdr");
        }
        f.begin_disabled(sdr);

        f.readout(
            "field.rtty.detected",
            &format!("{:.1} bd  {:.0} Hz", status.decoder.detected_baud, status.decoder.shift_hz),
        );
        f.readout(
            "field.rtty.lock",
            &format!(
                "{:.0} %  err {}",
                status.decoder.fsk_lock * 100.0,
                status.decoder.framing_errors
            ),
        );
        f.readout(
            "field.rtty.level",
            &format!(
                "{:.1} dB  mark {:.0} Hz",
                status.decoder.fsk_level_db, status.decoder.mark_hz
            ),
        );
        // The value is a key rather than resolved text, so a fixed choice is
        // translated without holding a reference into the catalogue across a
        // call that takes the interface mutably.
        f.readout(
            "field.rtty.shift_state",
            if status.decoder.figures { "status.figures" } else { "status.letters" },
        );

        f.toggle("field.rtty.enabled", &mut settings.rtty.enabled);
        f.begin_disabled(!settings.rtty.enabled);

        f.enum_combo("field.rtty.alphabet", &mut settings.rtty.alphabet);
        f.begin_disabled(settings.rtty.auto_baud);
        f.slider("field.rtty.baud", &mut settings.rtty.baud, 10.0, 1200.0, 2, "unit.baud");
        f.end_disabled();
        f.begin_disabled(settings.rtty.auto_shift);
        f.slider("field.rtty.shift", &mut settings.rtty.shift_hz, 20.0, 2000.0, 0, "unit.hz");
        let ceiling = status.nyquist_hz.max(200.0);
        let floor = if status.complex_signal { -ceiling } else { 50.0 };
        f.slider("field.rtty.mark", &mut settings.rtty.mark_hz, floor, ceiling, 0, "unit.hz");
        f.end_disabled();
        // The five bit alphabet has a fixed frame; the data and stop lengths
        // only mean something on a character oriented link.
        f.begin_disabled(settings.rtty.alphabet == crate::config::settings::RttyAlphabet::Baudot);
        f.slider_u32("field.rtty.data_bits", &mut settings.rtty.data_bits, 5, 8, "");
        f.end_disabled();
        f.slider("field.rtty.stop_bits", &mut settings.rtty.stop_bits, 1.0, 2.0, 1, "");
        f.enum_combo("field.rtty.parity", &mut settings.rtty.parity);
        f.slider("field.rtty.atc", &mut settings.rtty.atc, 0.0, 1.0, 2, "");
        f.slider("field.rtty.squelch", &mut settings.rtty.squelch_db, -140.0, 0.0, 0, "unit.db");
        f.toggle("field.rtty.usos", &mut settings.rtty.usos);
        f.toggle("field.rtty.invert", &mut settings.rtty.invert);
        // Tried rather than predicted, which is what the setting says: with the
        // sense reversed the start bit is a mark and the receiver never arms, so
        // there is nothing to compare against without a second framing machine.
        f.toggle("field.rtty.auto_invert", &mut settings.rtty.bit_inversion_retry);
        if status.decoder.fsk_auto_inverted {
            f.readout("field.rtty.polarity", "reversed");
        }
        f.hint("hint.rtty_auto_invert");
        f.toggle("field.rtty.auto_shift", &mut settings.rtty.auto_shift);
        f.toggle("field.rtty.afc", &mut settings.rtty.afc);
        f.begin_disabled(!settings.rtty.afc);
        f.slider("field.rtty.afc_range", &mut settings.rtty.afc_range_hz, 5.0, 500.0, 0, "unit.hz");
        f.end_disabled();

        f.end_disabled();
        f.end_disabled();
    }
    f.end_group();
}

/// Phase shift keying at thirty one and a quarter baud.
///
/// Two readings carry the section and they answer different questions. The
/// clustering says whether the carrier is phase modulated at all, which is the
/// one thing a spectrum cannot show; the framing says whether that modulation
/// carries this alphabet, which is what separates it from every other two phase
/// format and from noise that happened to assemble a code.
fn section_psk(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let active = settings.psk.enabled && !settings.sdr_mode();
    let open = f.begin_group("group.psk_decoder");
    f.mark_active(active);
    if open {
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.decoder_off_in_sdr");
        }
        f.begin_disabled(sdr);

        f.hint("hint.psk_rate");
        f.readout(
            "field.psk.centre",
            &format!(
                "{:.1} Hz  afc {:+.1} Hz",
                status.decoder.psk_centre_hz, status.decoder.psk_afc_hz
            ),
        );
        f.readout(
            "field.psk.lock",
            &format!(
                "{:.0} %  phase {:.0} %",
                status.decoder.psk_lock * 100.0,
                status.decoder.psk_quality * 100.0
            ),
        );
        f.hint("hint.psk_lock");
        f.readout(
            "field.psk.level",
            &format!("{:.1} {}", status.decoder.psk_level_db, f.ui.tr("unit.db")),
        );
        f.readout(
            "field.psk.characters",
            &format!("{}  rejected {}", status.decoder.psk_characters, status.decoder.psk_rejects),
        );

        f.toggle("field.psk.enabled", &mut settings.psk.enabled);
        f.begin_disabled(!settings.psk.enabled);

        f.toggle("field.psk.auto_centre", &mut settings.psk.auto_centre);
        // A stated centre is only reachable when nothing is being followed.
        f.begin_disabled(settings.psk.auto_centre);
        let ceiling = status.nyquist_hz.max(200.0);
        let floor = if status.complex_signal { -ceiling } else { 50.0 };
        f.slider("field.psk.centre_hz", &mut settings.psk.centre_hz, floor, ceiling, 0, "unit.hz");
        f.end_disabled();

        f.toggle("field.psk.afc", &mut settings.psk.afc);
        f.begin_disabled(!settings.psk.afc);
        f.slider("field.psk.afc_range", &mut settings.psk.afc_range_hz, 1.0, 7.8, 1, "unit.hz");
        f.end_disabled();
        f.hint("hint.psk_afc");

        f.slider("field.psk.squelch", &mut settings.psk.squelch_db, -140.0, 0.0, 0, "unit.db");
        f.slider("field.psk.print_above", &mut settings.psk.print_threshold, 0.0, 1.0, 2, "");
        f.hint("hint.psk_print");

        f.end_disabled();
        f.end_disabled();
    }
    f.end_group();
}

fn section_classifier(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let active = settings.classifier.enabled && !settings.sdr_mode();
    let open = f.begin_group("group.classifier");
    f.mark_active(active);
    if open {
        // The classifier reads the spectrum and the decoder statistics. The
        // spectrum survives the receiver mode; the statistics do not, because
        // the decoders are not fed, so its decision would rest on half its
        // evidence and would be worse than none.
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.decoder_off_in_sdr");
        }
        f.begin_disabled(sdr);

        f.readout(
            "field.classifier.mode",
            &format!(
                "{} {:.0} %",
                f.ui.tr(status.decoder.mode.key()),
                status.decoder.confidence * 100.0
            ),
        );
        f.readout("field.classifier.floor", &format!("{:.1} dB", status.noise_floor_db));
        f.toggle("field.classifier.enabled", &mut settings.classifier.enabled);
        f.hint("hint.classifier_scope");
        f.begin_disabled(!settings.classifier.enabled);
        f.slider_u32(
            "field.classifier.window",
            &mut settings.classifier.analysis_window_ms,
            200,
            20000,
            "unit.ms",
        );
        f.slider_u32(
            "field.classifier.interval",
            &mut settings.classifier.update_interval_ms,
            50,
            5000,
            "unit.ms",
        );
        f.slider("field.classifier.confidence", &mut settings.classifier.min_confidence, 0.1, 1.0, 2, "");
        f.slider_u32("field.classifier.hold", &mut settings.classifier.hold_time_ms, 0, 30000, "unit.ms");
        f.toggle("field.classifier.detect_cw", &mut settings.classifier.detect_cw);
        f.toggle("field.classifier.detect_rtty", &mut settings.classifier.detect_rtty);
        // The maritime format is a teleprinter signal with fixed parameters, so
        // it can only be recognized once the shifted pair itself is.
        f.begin_disabled(!settings.classifier.detect_rtty);
        f.toggle("field.classifier.detect_navtex", &mut settings.classifier.detect_navtex);
        f.end_disabled();
        f.toggle("field.classifier.detect_psk31", &mut settings.classifier.detect_psk31);
        f.toggle("field.classifier.auto_switch", &mut settings.classifier.auto_switch_decoder);
        f.toggle("field.classifier.announce", &mut settings.classifier.announce_in_log);
        f.end_disabled();

        f.end_disabled();
    }
    f.end_group();
}

/// Call sign resolution.
///
/// The database is the only part an operator has to provide, so what was loaded
/// is stated first: a path that resolves nothing looks exactly like resolution
/// being switched off, and the two call for opposite actions.
fn section_callsign(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    let active = settings.callsign.lookup_enabled;
    let open = f.begin_group("group.callsign");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        f.hint("hint.callsign_offline");
        f.toggle("field.callsign.lookup", &mut settings.callsign.lookup_enabled);
        f.begin_disabled(!settings.callsign.lookup_enabled);

        if status.prefix_count > 0 {
            f.readout(
                "field.callsign.loaded",
                &format!("{} pfx  {} dxcc", status.prefix_count, status.country_count),
            );
        } else if settings.callsign.source == CallsignSource::Cty {
            f.hint("hint.callsign_no_db");
        }

        f.enum_combo("field.callsign.source", &mut settings.callsign.source);
        f.begin_disabled(settings.callsign.source != CallsignSource::Cty);
        f.text_edit("field.callsign.prefix_db", &mut settings.callsign.prefix_db_path);
        f.end_disabled();
        // The operator list annotates whatever the prefix database resolved, so
        // it is useful alongside it rather than only as its replacement.
        f.text_edit("field.callsign.local_db", &mut settings.callsign.local_db_path);

        f.begin(Style::row().gap(gap));
        if f.button("action.reload") {
            cmd.reload_callsigns = true;
        }
        f.spacer(1.0);
        f.end();

        f.toggle("field.callsign.highlight", &mut settings.callsign.highlight_in_text);
        f.toggle("field.callsign.auto_lookup", &mut settings.callsign.auto_lookup_on_decode);
        f.hint("hint.callsign_auto");
        f.slider_u32(
            "field.callsign.min_length",
            &mut settings.callsign.min_callsign_length,
            3,
            12,
            "",
        );
        f.hint("hint.callsign_min");
        f.slider_u32(
            "field.callsign.cache",
            &mut settings.callsign.cache_entries,
            64,
            65536,
            "",
        );

        f.text_edit("field.callsign.history", &mut settings.callsign.history_path);
        f.slider_u32(
            "field.callsign.history_limit",
            &mut settings.callsign.history_limit,
            0,
            100_000,
            "unit.lines",
        );
        f.hint("hint.callsign_history");

        f.end_disabled();
    }
    f.end_group();
}

/// Stations heard.
///
/// The product of a skimmer rather than a convenience. A text panel says what
/// was sent; this says who is on the band, where, how strongly and how recently,
/// which is what an operator consults before deciding where to point the
/// receiver.
///
/// Ordered by frequency ascending, so the list reads the way a dial turns and a
/// row does not move under the pointer when a station is heard again.
fn section_spots(f: &mut Frame<'_>, status: &StatusInfo<'_>, cmd: &mut UiCommands) {
    let active = !status.spots.is_empty();
    let open = f.begin_group("group.spots");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        f.readout("field.spots.count", &format!("{}", status.spots.len()));

        f.begin(Style::row().gap(gap));
        f.begin_disabled(status.spots.is_empty());
        if f.button("action.clear") {
            cmd.clear_spots = true;
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();

        if status.spots.is_empty() {
            f.hint("hint.spots_empty");
        } else {
            f.hint("hint.spots_click");
        }

        for (index, row) in status.spots.iter().enumerate() {
            // One scope per row: the rows are built from the same keys and would
            // otherwise share their identity, which is what makes state leak
            // from one row to the next.
            f.ui.begin_scope(&format!("spot{}", index));
            f.begin(Style::row().gap(gap).align(Align::Center));

            // The frequency is a target rather than a statement, which is the
            // shortest path from reading a call to hearing the station.
            f.begin_disabled(!row.tunable);
            if f.button(&row.frequency) {
                cmd.tune_spot = Some(index);
            }
            f.end_disabled();

            // An entry resting on one sighting of one token may well be a
            // misread, and dimming it says so without spending a column.
            let color = if row.confident {
                f.ui.theme.text
            } else {
                f.ui.theme.text_disabled
            };
            f.label_mono(
                &row.detail,
                color,
                TextAlign::Left,
                Style::row().grow(1.0).shrink(1.0),
            );

            f.end();
            f.ui.end_scope();
        }
    }
    f.end_group();
}

/// Headphone monitor.
///
/// Two arrangements and they do not share a control.
///
/// In the skimmer mode the monitor is a listening aid for the decoders: it
/// narrows onto the focused keyed carrier and follows it as the tracker moves,
/// because the reason to listen is to judge whether the detector is centred.
///
/// In the receiver mode the receiver chain is the listening path. Its filter,
/// its detector and its gain loop replace everything here, so the controls that
/// belong to the narrow filter are dead: leaving them live would let a filter
/// follow the keying tracker while the operator works a voice signal, which
/// reproduces the tracker in the headphones as a tone sweeping up the band.
fn section_monitor(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    sel: &mut Selections,
    cmd: &mut UiCommands,
) {
    let active = settings.audio.monitor_enabled && status.monitor.running;
    let open = f.begin_group("group.monitor");
    f.mark_active(active);
    if open {
        // Feedback is refused rather than merely warned about, so the control
        // that would arm it is dead while the pair is dangerous.
        if status.feedback {
            f.hint("hint.monitor_feedback");
        }
        f.begin_disabled(status.feedback);
        if f.toggle("field.monitor.enabled", &mut settings.audio.monitor_enabled) {
            cmd.restart_monitor = true;
        }
        f.end_disabled();

        // Listening is where the receiver chain runs, so in that mode it is not
        // a convenience at all: with it off the chain is not built and none of
        // its settings do anything.
        let sdr = settings.sdr_mode();
        if sdr {
            f.hint("hint.monitor_is_receiver");
            if !settings.audio.monitor_enabled {
                f.hint("hint.monitor_needed_in_sdr");
            }
        }

        f.begin_disabled(!settings.audio.monitor_enabled || status.feedback);

        if f.combo("field.monitor.device", &mut sel.monitor, status.monitor_devices) {
            cmd.select_monitor = true;
            cmd.restart_monitor = true;
        }

        f.slider("field.monitor.volume", &mut settings.audio.monitor_volume, 0.0, 1.0, 2, "");

        // Everything from here to the gain control belongs to the narrow filter
        // of the skimmer arrangement, which the receiver chain replaces.
        f.begin_disabled(sdr);

        f.toggle("field.monitor.filter", &mut settings.audio.monitor_filter);
        f.begin_disabled(!settings.audio.monitor_filter);

        f.enum_combo("field.monitor.width_mode", &mut settings.audio.monitor_width_mode);
        // The stated width is only reachable when it is the one in use.
        f.begin_disabled(
            settings.audio.monitor_width_mode != crate::config::settings::MonitorWidth::Independent,
        );
        f.slider(
            "field.monitor.bandwidth",
            &mut settings.audio.monitor_bandwidth_hz,
            50.0,
            3000.0,
            0,
            "unit.hz",
        );
        f.end_disabled();

        f.toggle("field.monitor.follow", &mut settings.audio.monitor_follow);
        // A stated centre is only reachable when nothing is being followed.
        f.begin_disabled(settings.audio.monitor_follow);
        let ceiling = status.nyquist_hz.max(200.0);
        let floor = if status.complex_signal { -ceiling } else { 50.0 };
        f.slider(
            "field.monitor.centre",
            &mut settings.audio.monitor_centre_hz,
            floor,
            ceiling,
            0,
            "unit.hz",
        );
        f.end_disabled();

        f.slider(
            "field.monitor.pitch",
            &mut settings.audio.monitor_pitch_hz,
            200.0,
            1500.0,
            0,
            "unit.hz",
        );
        f.hint("hint.monitor_pitch");

        f.end_disabled();

        // The gain loop of the skimmer arrangement. In the receiver mode the
        // chain has its own, with a hang the loop here does not have.
        f.toggle("field.monitor.agc", &mut settings.dsp.agc_enabled);
        f.begin_disabled(!settings.dsp.agc_enabled);
        f.slider("field.monitor.agc_attack", &mut settings.dsp.agc_attack_ms, 0.1, 500.0, 1, "unit.ms");
        f.slider("field.monitor.agc_release", &mut settings.dsp.agc_release_ms, 1.0, 5000.0, 0, "unit.ms");
        f.slider("field.monitor.agc_target", &mut settings.dsp.agc_target_db, -60.0, 0.0, 0, "unit.db");
        f.end_disabled();

        f.end_disabled();

        f.end_disabled();

        // Readings. Outside every disabled scope, because they state what is
        // happening rather than offering to change it, and a dimmed reading of
        // a live value is a lie about whether it is live.
        //
        // The band is reported as edges rather than as a width: edges are what
        // can be compared against the display without arithmetic, and in the
        // receiver mode they are the filter edges themselves.
        let band = match status.listen_band {
            Some((lo, hi)) => format!("{:.0} - {:.0} {}", lo, hi, f.ui.tr("unit.hz")),
            None => f.ui.tr("status.wide_open").to_string(),
        };
        f.readout("field.monitor.band", &band);
        f.hint("hint.monitor_band");

        // Drift is the difference between the two device clocks, expressed as
        // the correction being applied. A reading pinned at the limit means the
        // rates are not what the endpoints reported.
        f.readout(
            "field.monitor.state",
            &format!(
                "{} Hz {}ch  {:+.0} ppm",
                status.monitor.device_rate, status.monitor.channels, status.monitor.drift_ppm
            ),
        );
        f.readout(
            "field.monitor.buffer",
            &format!("{:.0} %  gaps {}", status.monitor.fill * 100.0, status.monitor.underruns),
        );

        if !status.monitor.error.is_empty() {
            f.label_dim(&status.monitor.error);
        }
    }
    f.end_group();
}

// ---------------------------------------------------------- replay tab

/// Cyclic recording.
///
/// The controls that decide the shape of the ring are separated from the one
/// that starts it, because the first group takes effect on the next start and
/// the second is what an operator reaches for while listening.
fn section_record(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    let active = status.recorder.running;
    let open = f.begin_group("group.record");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);
        let r = &status.recorder;

        f.hint("hint.record");
        f.toggle("field.record.enabled", &mut settings.record.enabled);

        f.begin_disabled(!settings.record.enabled);

        // Readings first: an operator opening this section is normally checking
        // whether the ring is still turning, not reconfiguring it.
        f.readout(
            "field.record.state",
            if r.running { "status.recording" } else { "status.stopped" },
        );
        if !r.current.is_empty() {
            f.readout("field.record.segment", &r.current);
        }
        // The closed count sits beside the two live figures because it is what
        // says the ring is turning. A segment that keeps growing while none has
        // ever closed means the rotation is not happening, and the size alone
        // cannot distinguish that from a long segment length.
        f.readout(
            "field.record.written",
            &format!(
                "{:.0} s   {:.1} MB   {} closed",
                r.seconds,
                r.current_bytes as f64 / (1024.0 * 1024.0),
                r.segments
            ),
        );
        f.readout("field.record.total", &status.segments_total);
        // A gap is a block the queue could not take, which is a hole in the
        // file. Counted rather than hidden: a recording with holes and one
        // without look identical until somebody tries to decode from it.
        if r.gaps > 0 {
            f.readout("field.record.gaps", &format!("{}", r.gaps));
            f.hint("hint.record_gaps");
        }
        if !r.error.is_empty() {
            f.label_dim(&r.error);
        }

        f.begin(Style::row().gap(gap));
        f.begin_disabled(!status.audio_running);
        if f.button(if r.running { "action.stop" } else { "action.record" }) {
            if r.running {
                cmd.stop_record = true;
            } else {
                cmd.start_record = true;
            }
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();
        if !status.audio_running {
            f.hint("hint.record_needs_audio");
        }

        // Geometry. Applied when the recorder next starts, because the segment
        // being written was planned from the previous values and a block that
        // changed size in the middle of a file would be unreadable.
        f.begin_disabled(r.running);
        f.text_edit("field.record.path", &mut settings.record.path);
        f.slider_u32(
            "field.record.segment",
            &mut settings.record.segment_seconds,
            5,
            3600,
            "unit.s",
        );
        f.slider_u32("field.record.budget", &mut settings.record.budget_mb, 16, 262_144, "unit.mb");
        f.enum_combo("field.record.format", &mut settings.record.format);
        f.hint("hint.record_format");
        f.slider(
            "field.record.block",
            &mut settings.record.block_seconds,
            0.05,
            5.0,
            2,
            "unit.s",
        );
        f.hint("hint.record_block");
        f.end_disabled();
        if r.running {
            f.hint("hint.record_running");
        }

        f.toggle("field.record.auto_start", &mut settings.record.auto_start);

        f.end_disabled();
    }
    f.end_group();
}

/// Playback of the ring.
///
/// The transport sits above the timeline because it is what an operator uses
/// most: scrubbing is occasional, stepping back over a call sign is not.
fn section_replay(f: &mut Frame<'_>, status: &StatusInfo<'_>, cmd: &mut UiCommands) {
    let active = status.replay_active;
    let open = f.begin_group("group.replay");
    f.mark_active(active);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        match status.replay.as_ref() {
            None => {
                f.hint("hint.replay_closed");
                f.begin(Style::row().gap(gap));
                f.begin_disabled(status.segments.is_empty());
                if f.button("action.open") {
                    cmd.open_replay = true;
                }
                f.end_disabled();
                f.spacer(1.0);
                f.end();
                if status.segments.is_empty() {
                    f.hint("hint.replay_no_segments");
                }
            }
            Some(r) => {
                // Position first. It is the one reading an operator checks
                // constantly, and burying it under the transport would mean
                // reading past the buttons every time.
                let seconds = r.seconds(status.replay_block_seconds);
                let total = r.blocks as f64 * status.replay_block_seconds;
                f.readout(
                    "field.replay.position",
                    &format!("{}  /  {}", clock(seconds), clock(total)),
                );
                if r.faults > 0 {
                    f.readout("field.replay.faults", &format!("{}", r.faults));
                }
                if !r.error.is_empty() {
                    f.label_dim(&r.error);
                }

                // The timeline is a control rather than a picture, so it sits
                // among the transport rather than at the end of the group.
                f.custom(TAG_TIMELINE, Style::row().height_px(f.ui.m(34.0)));
                f.hint("hint.replay_scrub");

                f.begin(Style::row().gap(gap));
                if f.button("action.rewind") {
                    // Ten blocks, which at the default is five seconds: far
                    // enough to catch a call sign that was just missed, short
                    // enough that two presses do not overshoot the exchange.
                    cmd.replay_step = Some(-10);
                }
                if f.button("action.back") {
                    cmd.replay_step = Some(-1);
                }
                if f.button(if r.playing { "action.pause" } else { "action.play" }) {
                    cmd.replay_play = Some(!r.playing);
                }
                if f.button("action.forward") {
                    cmd.replay_step = Some(1);
                }
                if f.button("action.live") {
                    cmd.replay_live = true;
                }
                f.spacer(1.0);
                if f.button("action.close_replay") {
                    cmd.close_replay = true;
                }
                f.end();

                // Every control below reads its value out of the stream and
                // reports a change as a command. The stream is the one place the
                // transport state lives, so a control cannot come to disagree
                // with what is actually playing.
                //
                // The slider bounds are the ones the stream clamps to rather
                // than literals of their own: a slider offering a rate the
                // stream refuses would show a value nothing is playing at.
                let mut speed = r.speed;
                if f.slider("field.replay.speed", &mut speed, SPEED_MIN, SPEED_MAX, 2, "") {
                    cmd.replay_speed = Some(speed);
                }
                f.hint("hint.replay_speed");

                let mut follow = r.follow;
                if f.toggle("field.replay.follow", &mut follow) {
                    cmd.replay_follow = Some(follow);
                }
                f.hint("hint.replay_follow");

                let mut looping = r.looping;
                if f.toggle("field.replay.loop", &mut looping) {
                    cmd.replay_loop = Some(looping);
                }
            }
        }
    }
    f.end_group();
}

/// Segments on disk, and the export.
fn section_segments(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.segments") {
        let gap = f.ui.m(f.ui.theme.gap);

        f.enum_combo("field.export.format", &mut settings.record.export_format);
        f.hint("hint.export_format");
        // Bit depth applies to the uncompressed container alone. The other two
        // define their own sample layout, so the control would be inert.
        f.begin_disabled(
            settings.record.export_format != crate::config::settings::ExportFormat::Wav,
        );
        let mut float = settings.record.export_bits >= 32;
        if f.toggle("field.export.float", &mut float) {
            settings.record.export_bits = if float { 32 } else { 16 };
        }
        f.end_disabled();
        f.text_edit("field.export.path", &mut settings.record.export_path);

        f.begin(Style::row().gap(gap));
        if f.button("action.rescan") {
            cmd.rescan_segments = true;
        }
        f.spacer(1.0);
        f.end();

        if status.segments.is_empty() {
            f.hint("hint.replay_no_segments");
        }

        for (index, entry) in status.segments.iter().enumerate() {
            // One scope per row: the rows are built from the same keys and
            // would otherwise share their identity, which is what makes state
            // leak from one row to the next.
            f.ui.begin_scope(&format!("seg{}", index));
            f.begin(Style::row().gap(gap).align(Align::Center));

            let color = if entry.usable {
                f.ui.theme.text
            } else {
                f.ui.theme.text_disabled
            };
            f.label_mono(
                &entry.name,
                color,
                TextAlign::Left,
                Style::row().grow(1.0).shrink(1.0),
            );
            f.label_mono(&entry.detail, f.ui.theme.text_dim, TextAlign::Right, Style::row());
            f.begin_disabled(!entry.usable);
            if f.button_flat("action.export") {
                cmd.export_segment = Some(index);
            }
            f.end_disabled();

            f.end();
            f.ui.end_scope();
        }
    }
    f.end_group();
}

/// Seconds as hours, minutes and seconds.
///
/// Fixed width, so a position readout does not shift as it crosses a minute.
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

// -------------------------------------------------------- settings tab

/// Controls that configure the application rather than the receiver.
///
/// Kept off the composable tabs on purpose. Several of them decide how the
/// panel itself behaves, and one of them decides what the panel contains, so a
/// composition that omitted them would leave no way back.
fn application(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.interface") {
        f.readout(
            "field.ui.decoded",
            &format!("{} {}", status.decoded_chars, f.ui.tr("unit.chars")),
        );

        let mut language = status.language_index;
        if f.combo("field.ui.language", &mut language, status.languages) {
            if let Some(code) = status.languages.get(language) {
                cmd.language = Some((*code).to_string());
            }
        }

        f.slider("field.ui.scale", &mut settings.ui.scale, 0.5, 3.0, 2, "");
        f.slider("field.ui.font", &mut settings.ui.font_size_pt, 6.0, 32.0, 1, "unit.pt");
        f.slider("field.ui.decode_font", &mut settings.ui.decode_font_size_pt, 6.0, 40.0, 1, "unit.pt");
        // Coverage shaping is applied when a glyph is rasterized, and the cache
        // is keyed by size rather than by gamma, so a change reaches the glyphs
        // that are drawn next rather than the ones already in the atlas.
        f.slider("field.ui.text_gamma", &mut settings.ui.text_gamma, 0.5, 3.0, 2, "");
        f.hint("hint.restart");
        f.readout(
            "field.app.atlas",
            &format!("{} px  {:.0} %", settings.ui.glyph_atlas_size, status.atlas * 100.0),
        );
        f.hint("hint.slider_speeds");
    }
    f.end_group();

    if f.begin_group("group.application") {
        f.toggle("field.ui.vsync", &mut settings.ui.vsync);
        // With vertical synchronization on the present call already paces the
        // loop; a second limiter can only make it slower.
        f.begin_disabled(settings.ui.vsync);
        f.slider_u32("field.ui.target_fps", &mut settings.ui.target_fps, 0, 480, "unit.fps");
        f.end_disabled();

        // The presentation mode is chosen when the swapchain is built, and the
        // automatic setting derives it from the synchronization toggle above.
        f.enum_combo("field.app.present_mode", &mut settings.render.present_mode);
        f.slider_u32("field.app.frames_in_flight", &mut settings.render.frames_in_flight, 1, 4, "");
        f.toggle("field.app.validation", &mut settings.render.validation);
        f.toggle("field.app.gpu_timing", &mut settings.render.gpu_timing);
        f.hint("hint.gpu_timing");
        f.hint("hint.restart");

        let mut level = LogLevelCfg::from_level(settings.log.level);
        if f.enum_combo("field.app.log_level", &mut level) {
            settings.log.level = level.to_level();
            // Applied immediately rather than at the next start: the reason to
            // raise the level is almost always a fault happening right now.
            cmd.log_level = Some(settings.log.level);
        }

        f.toggle("field.ui.transcript", &mut settings.log.transcript_enabled);
        f.begin_disabled(!settings.log.transcript_enabled);
        f.text_edit("field.app.transcript_path", &mut settings.log.transcript_path);
        f.end_disabled();
        f.readout(
            "field.app.max_lines",
            &format!("{} {}", status.log_lines, f.ui.tr("unit.lines")),
        );
        // Two areas the operator can hand back to the display. The strip and
        // the text panel are the largest things in the window that are not the
        // spectrum, and an operator watching one frequency wants neither.
        f.toggle("field.ui.show_meter", &mut settings.ui.show_meter);
        f.toggle("field.ui.show_decode_log", &mut settings.ui.show_decode_log);
        f.toggle("field.ui.debug_overlay", &mut settings.ui.show_debug_overlay);
    }
    f.end_group();
}

/// Chrome and density.
///
/// Nothing here changes what the application measures. It changes how much of
/// the drawing is decoration and how the few marks that remain are used, which
/// is a matter the operator settles once and then stops thinking about.
fn appearance(f: &mut Frame<'_>, settings: &mut Settings, cmd: &mut UiCommands) {
    if f.begin_group("group.appearance") {
        let a = &mut settings.appearance;

        if f.toggle("field.look.custom_frame", &mut a.custom_frame) {
            cmd.frame_changed = true;
        }
        f.hint("hint.look_frame");
        f.begin_disabled(!a.custom_frame);
        f.slider("field.look.caption_height", &mut a.caption_height, 18.0, 48.0, 0, "");
        f.end_disabled();

        f.toggle("field.look.focus_ring", &mut a.focus_ring);
        f.toggle("field.look.accent_hover", &mut a.accent_hover);
        f.hint("hint.look_accent");
        f.toggle("field.look.group_tick", &mut a.group_tick);
        f.enum_combo("field.look.tab_style", &mut a.tab_style);

        f.toggle("field.look.animate", &mut a.animate);
        f.hint("hint.look_animate");
        f.begin_disabled(!a.animate);
        f.slider("field.look.anim_ms", &mut a.anim_ms, 30.0, 500.0, 0, "unit.ms");
        f.enum_combo("field.look.anim_curve", &mut a.anim_curve);
        f.hint("hint.look_curve");
        f.end_disabled();

        f.toggle("field.look.value_column", &mut a.value_column);
        f.toggle("field.look.numeric_entry", &mut a.numeric_entry);
        f.hint("hint.look_entry");
        f.toggle("field.look.group_activity", &mut a.group_activity);
        f.hint("hint.look_activity");
        f.toggle("field.look.keyboard_focus", &mut a.keyboard_focus);
        f.hint("hint.look_keyboard");
        f.slider("field.look.popup_shade", &mut a.popup_shade, 0.0, 0.6, 2, "");
        f.hint("hint.look_shade");
        f.toggle("field.look.splitter_grip", &mut a.splitter_grip);
        f.slider("field.look.hint_scale", &mut a.hint_scale, 0.6, 1.0, 2, "");
        f.slider("field.look.separator_alpha", &mut a.separator_alpha, 0.1, 1.0, 2, "");

        f.slider("field.look.row_height", &mut a.row_height, 16.0, 40.0, 0, "");
        f.slider("field.look.gap", &mut a.gap, 0.0, 16.0, 0, "");
        f.slider("field.look.panel_margin", &mut a.panel_margin, 0.0, 24.0, 0, "");
        f.slider("field.look.group_padding", &mut a.group_padding, 0.0, 20.0, 0, "");
    }
    f.end_group();
}

/// Everything the application draws over the spectrum and the waterfall.
fn data_area(f: &mut Frame<'_>, settings: &mut Settings) {
    if f.begin_group("group.data_area") {
        let a = &mut settings.appearance;

        // The background is a colour and there is no colour control in this
        // interface, so it is stated in the configuration file. Saying so is
        // better than offering three sliders for one value nobody adjusts twice.
        f.readout("field.look.data_background", &format!("{:06X}", a.data_background_rgb));

        f.toggle("field.look.axis_gutters", &mut a.axis_gutters);
        f.hint("hint.look_gutters");
        f.begin_disabled(!a.axis_gutters);
        f.slider("field.look.gutter_left", &mut a.axis_gutter_left, 0.0, 90.0, 0, "");
        f.slider("field.look.gutter_bottom", &mut a.axis_gutter_bottom, 0.0, 40.0, 0, "");
        f.end_disabled();

        f.slider_u32("field.look.grid_major_every", &mut a.grid_major_every, 1, 20, "");
        f.slider("field.look.grid_minor_alpha", &mut a.grid_minor_alpha, 0.0, 1.0, 2, "");

        f.toggle("field.look.crosshair", &mut a.crosshair);
        f.toggle("field.look.trace_fill", &mut a.trace_fill);
        f.begin_disabled(!a.trace_fill);
        f.slider("field.look.trace_fill_alpha", &mut a.trace_fill_alpha, 0.0, 0.8, 2, "");
        f.end_disabled();
        f.slider("field.look.trace_thickness", &mut a.trace_thickness, 1.0, 4.0, 1, "");

        f.toggle("field.look.meter_segmented", &mut a.meter_segmented);
        f.hint("hint.look_meter");
        f.begin_disabled(!a.meter_segmented);
        f.slider("field.look.meter_segment", &mut a.meter_segment_px, 2.0, 12.0, 0, "");
        f.slider("field.look.meter_segment_gap", &mut a.meter_segment_gap_px, 0.0, 6.0, 0, "");
        f.end_disabled();
        f.toggle("field.look.meter_scale_labels", &mut a.meter_scale_labels);
    }
    f.end_group();
}

/// Editor for the composition of the four composable tabs.
///
/// The tab is chosen with the same strip the toolbar uses, not with a list. A
/// list would name the tabs in one place while the strip names them in another,
/// and the operator would have to match the two before anything below made
/// sense; the strip simply shows the set and marks the one being edited.
///
/// Order matters as much as membership, so the list offers movement rather than
/// only presence. A section may be placed on several tabs; it carries the same
/// settings in each, because the settings belong to the receiver and the tab is
/// only a view onto them. The defaults do not do this, because a control in two
/// places invites the belief that they are two controls.
///
/// A restore is offered per tab. Without one an emptied tab is indistinguishable
/// from a lost one, and the only way back would be through the configuration
/// file, which is the wrong place to recover from an interface action.
fn layout_editor(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    editing_tab: &mut usize,
    add_section: &mut usize,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.layout") {
        let gap = f.ui.m(f.ui.theme.gap);

        let tab_keys: Vec<&str> = TAB_KEYS[..PANEL_TABS].to_vec();
        f.tabs("layout_tabs", editing_tab, &tab_keys);
        *editing_tab = (*editing_tab).min(PANEL_TABS - 1);
        f.hint("hint.layout");

        // The order is edited through an intent recorded here and applied after
        // the rows are declared: mutating the list while iterating it would
        // shift the rows still to come.
        let present: Vec<PanelSection> = settings.panel.sections(*editing_tab).to_vec();
        let mut move_up: Option<usize> = None;
        let mut move_down: Option<usize> = None;
        let mut remove: Option<usize> = None;

        if present.is_empty() {
            f.hint("hint.layout_empty");
        }
        for (index, which) in present.iter().enumerate() {
            f.ui.begin_scope(&format!("layout{}", which.to_config()));
            f.begin(Style::row().gap(gap).align(Align::Center));

            let color = f.ui.theme.text;
            f.label_styled(
                which.key(),
                color,
                FontId::Ui,
                TextAlign::Left,
                Style::row().grow(1.0).shrink(1.0),
            );
            f.begin_disabled(index == 0);
            if f.button_flat("action.up") {
                move_up = Some(index);
            }
            f.end_disabled();
            f.begin_disabled(index + 1 >= present.len());
            if f.button_flat("action.down") {
                move_down = Some(index);
            }
            f.end_disabled();
            if f.button_flat("action.close") {
                remove = Some(index);
            }

            f.end();
            f.ui.end_scope();
        }

        // Sections not already on this tab. An empty remainder means the tab
        // holds everything, and the control says so by being dead rather than
        // by vanishing.
        let missing: Vec<PanelSection> = PanelSection::all()
            .into_iter()
            .filter(|s| !present.contains(s))
            .collect();
        let missing_keys: Vec<&str> = missing.iter().map(|s| s.key()).collect();

        let mut append: Option<PanelSection> = None;
        if missing.is_empty() {
            f.begin_disabled(true);
            // The combo needs at least one entry to lay out; a placeholder keeps
            // the row the same height as it is when the list has content.
            let none = ["enum.none"];
            let mut zero = 0usize;
            f.combo("field.layout.add", &mut zero, &none);
            f.end_disabled();
        } else {
            *add_section = (*add_section).min(missing.len() - 1);
            f.combo("field.layout.add", add_section, &missing_keys);
        }

        f.begin(Style::row().gap(gap));
        if f.button("action.reset") {
            cmd.reset_tab = Some(*editing_tab);
        }
        f.spacer(1.0);
        f.begin_disabled(missing.is_empty());
        if f.button("action.add") {
            append = missing.get(*add_section).copied();
        }
        f.end_disabled();
        f.end();

        if let Some(list) = settings.panel.sections_mut(*editing_tab) {
            if let Some(i) = move_up {
                list.swap(i - 1, i);
            }
            if let Some(i) = move_down {
                if i + 1 < list.len() {
                    list.swap(i, i + 1);
                }
            }
            if let Some(i) = remove {
                if i < list.len() {
                    list.remove(i);
                }
            }
            if let Some(s) = append {
                list.push(s);
                // The selection index refers to a list that just shrank.
                *add_section = 0;
            }
        }
    }
    f.end_group();
}

/// Settings that differ from the values the build ships with.
///
/// The one group that answers a question about the whole configuration rather
/// than about one section. Two hundred settings mean a receiver can behave
/// strangely for a reason nothing on any panel states, and the list of what has
/// been touched is the shortest path to it.
///
/// Read only. A control that reset an entry from here would be a second way to
/// change a setting, and the section that owns it is where it belongs.
fn deviations(f: &mut Frame<'_>, status: &StatusInfo<'_>) {
    let count = status.deviations.len();
    let open = f.begin_group("group.deviations");
    // Folded, the dot is the whole statement: something has been changed.
    f.mark_active(count > 0);
    if open {
        f.readout("field.deviations.count", &format!("{}", count));
        if count == 0 {
            f.hint("hint.deviations_none");
        } else {
            f.hint("hint.deviations");
        }

        let color = f.ui.theme.text_dim;
        for line in status.deviations {
            f.label_mono(
                line,
                color,
                TextAlign::Left,
                Style::row().grow(1.0).shrink(1.0),
            );
        }
    }
    f.end_group();
}

// ---------------------------------------------------------------- chrome

fn status_bar(f: &mut Frame<'_>, settings: &Settings, status: &StatusInfo<'_>, height: f32) {
    let gap = f.ui.m(f.ui.theme.gap);
    f.begin_clipped(
        Style::row().height_px(height).align(Align::Center).gap(gap).padding_xy(gap, 0.0),
        f.ui.theme.panel_header,
        Color::TRANSPARENT,
    );

    let led = f.ui.m(8.0);
    let led_color = if !status.audio.error.is_empty() {
        Color::hex(0xD05050)
    } else if status.audio_running {
        f.ui.theme.accent
    } else {
        f.ui.theme.text_disabled
    };
    f.begin_frame(
        Style::row().width_px(led).height_px(led).align_self(Align::Center),
        led_color,
        Color::TRANSPARENT,
    );
    f.end();

    let state = if !status.audio.error.is_empty() {
        status.audio.error.clone()
    } else if status.audio_running {
        // The silent marker matters for a loopback stream: an endpoint with no
        // playback looks exactly like a dead device otherwise.
        format!(
            "{}   {} {} {}{} {}{}",
            if settings.audio.device_name.is_empty() {
                f.ui.tr("status.default_input").to_string()
            } else {
                settings.audio.device_name.clone()
            },
            status.audio.device_rate,
            f.ui.tr("unit.hz"),
            status.audio.channels,
            f.ui.tr("unit.channels"),
            status.audio.format,
            if status.audio.silent {
                format!("   {}", f.ui.tr("status.silent"))
            } else {
                String::new()
            }
        )
    } else {
        f.ui.tr("status.stopped").to_string()
    };
    f.label_dim(&state);

    // The transceiver gets an indicator of its own rather than a share of the
    // audio one. The two fail independently, and a link that stalled while the
    // sound card keeps running is precisely the case worth seeing.
    if settings.rig.enabled {
        f.separator_vertical();
        let rig_color = if !status.rig.error.is_empty() {
            Color::hex(0xD05050)
        } else {
            match status.rig.link {
                LinkState::Up => f.ui.theme.accent,
                LinkState::Starting => f.ui.theme.text_dim,
                _ => f.ui.theme.text_disabled,
            }
        };
        let rig_text = match status.rig_frequency {
            Some(hz) => crate::rig::Readout::new(hz, 1).text(),
            None => f.ui.tr(link_key(status.rig.link)).to_string(),
        };
        f.label_mono(&rig_text, rig_color, TextAlign::Left, Style::row());
    }

    f.separator_vertical();

    // The recorder gets an indicator of its own. A ring that stopped because
    // the disk filled is exactly the condition an operator has to notice
    // without opening a tab.
    if status.recorder.running || status.replay_active {
        f.separator_vertical();
        let (text, color) = if status.replay_active {
            (
                f.ui.tr("status.replaying").to_string(),
                f.ui.theme.monitor,
            )
        } else if !status.recorder.error.is_empty() {
            (status.recorder.error.clone(), Color::hex(0xD05050))
        } else {
            (
                format!(
                    "{} {:.0} MB",
                    f.ui.tr("status.recording"),
                    status.recorder.total_bytes as f64 / (1024.0 * 1024.0)
                ),
                f.ui.theme.accent,
            )
        };
        f.label_mono(&text, color, TextAlign::Left, Style::row());
    }

    f.separator_vertical();

    let mode_text = if status.decoder.mode == Mode::Unknown {
        f.ui.tr("mode.none").to_string()
    } else {
        format!(
            "{} {:.0} %",
            f.ui.tr(status.decoder.mode.key()),
            status.decoder.confidence * 100.0
        )
    };
    let mode_color = if status.decoder.mode == Mode::Unknown {
        f.ui.theme.text_dim
    } else {
        f.ui.theme.accent
    };
    f.label_mono(&mode_text, mode_color, TextAlign::Left, Style::row());

    f.separator_vertical();
    let dim = f.ui.theme.text_dim;
    let text = f.ui.theme.text;
    let channels_text = format!("{} {}", status.decoder.cw_channels, f.ui.tr("unit.channels"));
    f.label_mono(&channels_text, dim, TextAlign::Left, Style::row());

    // Stated only while there is something in the list. A permanent nought would
    // be noise, and its absence says nothing has been recognized yet.
    if !status.spots.is_empty() {
        f.separator_vertical();
        let spots = format!("{} {}", status.spots.len(), f.ui.tr("unit.spots"));
        f.label_mono(&spots, f.ui.theme.accent, TextAlign::Left, Style::row());
    }

    // The magnification is only stated when it is in force. A permanent reading
    // of one would be noise; an absent one says the whole span is on screen.
    if !status.view_full {
        f.separator_vertical();
        let span = if status.view_span_hz >= 2000.0 {
            format!("{:.2} kHz", status.view_span_hz / 1000.0)
        } else {
            format!("{:.0} Hz", status.view_span_hz)
        };
        f.label_mono(&span, f.ui.theme.accent, TextAlign::Left, Style::row());
    }

    if let Some(hz) = status.cursor_hz {
        f.separator_vertical();
        let cursor_text = format!("{:.0} {}", hz, f.ui.tr("unit.hz"));
        f.label_mono(&cursor_text, f.ui.theme.accent, TextAlign::Left, Style::row());

        // Dimmer than the frequency. The frequency is what the pointer is aimed
        // at and the level is what happens to be there, so they are not two
        // statements of equal weight.
        if let Some(db) = status.cursor_db {
            let level = format!("{:.0} {}", db, f.ui.tr("unit.db"));
            f.label_mono(&level, f.ui.theme.text_dim, TextAlign::Left, Style::row());
        }

        // The difference against the reference, which is the whole reason to
        // place one: a shift is measured rather than read, and reading two
        // absolute figures and subtracting them is what the marker removes.
        if let Some(delta) = status.reference_delta_hz {
            let text = format!("{:+.0} {}", delta, f.ui.tr("unit.hz"));
            f.label_mono(&text, f.ui.theme.text, TextAlign::Left, Style::row());
        }
    }

    f.spacer(1.0);

    // Fixed width fields. A count that grows by one digit would otherwise shift
    // everything to its left, and the right hand end of the bar is exactly
    // where an operator glances without reading.
    let rate = format!(
        "{:>6} {}  FFT {:>5}  xrun {:>4}",
        status.source_rate,
        f.ui.tr("unit.hz"),
        settings.dsp.fft_size,
        status.audio.overruns.min(9999)
    );
    f.label_mono(&rate, dim, TextAlign::Right, Style::row());
    f.separator_vertical();
    let fps_text = format!("{:>3.0} {}", status.fps, f.ui.tr("unit.fps"));
    f.label_mono(&fps_text, text, TextAlign::Right, Style::row());
    f.end();
}

/// Diagnostic block. Not part of the tree: the layout model has no absolute
/// positioning, and a sibling would take real space away from the display.
fn overlay(f: &mut Frame<'_>, status: &StatusInfo<'_>) {
    let mut lines = vec![
        format!("fps {:.1}   worst {:.2} ms", status.fps, status.worst_ms),
        // Nought means the reading is absent rather than instantaneous, which is
        // why the two cases are worded differently.
        if status.gpu_ms > 0.0 {
            format!(
                "gpu {:.2} ms   worst {:.2} ms",
                status.gpu_ms, status.gpu_worst_ms
            )
        } else {
            "gpu not measured".to_string()
        },
        format!(
            "draws {}   uploads {}   clears {}   dpi {:.2}",
            status.draw_calls, status.uploads, status.clears, status.dpi
        ),
        format!("glyphs {}   atlas {:.0} %", status.glyphs, status.atlas * 100.0),
        format!(
            "queue {:.0} %   lines {}   log {}",
            status.queue_fill * 100.0,
            status.lines,
            status.log_lines
        ),
        format!("peak {:.1} dB   rms {:.1} dB", status.audio.peak_db, status.audio.rms_db),
        format!(
            "samples {}   gaps {}   xrun {}",
            status.audio.frames, status.audio.discontinuities, status.audio.overruns
        ),
        format!(
            "cw bw {:.0} Hz   floor {:.1} dB   fsk lock {:.0} %",
            status.decoder.cw_bandwidth_hz,
            status.noise_floor_db,
            status.decoder.fsk_lock * 100.0
        ),
        format!(
            "complex {}   nyquist {:.0} Hz   bin {:.2} Hz   view {:.0} Hz",
            status.complex_signal, status.nyquist_hz, status.bin_hz, status.view_span_hz
        ),
        // Here rather than in the panel because all three answer one question:
        // whether a picture that looks wrong is a display fault or a signal.
        // The mirror says which way the axis runs, the working point says which
        // frequency the view is being held around, and the anchor says where the
        // keying detector was pointed before its own loop moved it.
        format!(
            "mirrored {}   working {:.0} Hz   cw anchor {:.0} Hz",
            status.mirrored, status.working_hz, status.decoder.tone_hz
        ),
        format!(
            "rig {:?}   {}   ok {} err {} stray {}",
            status.rig.link,
            match status.rig_frequency {
                Some(hz) => format!("{} Hz", hz),
                None => "no reading".to_string(),
            },
            status.rig.counters.completed,
            status.rig.counters.faults,
            status.rig.counters.strays
        ),
    ];
    for ch in status.channels {
        lines.push(format!(
            "ch {:<2} {:>5.0} Hz  {:>4.1} wpm  {:>5.1}/{:>6.1} dB  {:>3.0} %  {:>3} el  {:>3} rj  {:>4} ch  {}",
            ch.id,
            ch.hz,
            ch.wpm,
            ch.snr_db,
            ch.level_db,
            ch.quality * 100.0,
            ch.elements.min(999),
            ch.rejects.min(999),
            ch.chars.min(9999),
            ch.gate.as_str()
        ));
    }
    lines.push(format!(
        "chars {}   framing err {}   fsk chars {}",
        status.decoded_chars, status.decoder.framing_errors, status.decoder.characters
    ));
    lines.push(format!("font {}", status.font));
    lines.push(format!("gpu {}", status.gpu));
    f.ui.set_overlay(lines);
}