//! Interface declaration.
//!
//! Split from the application shell because the two answer different questions:
//! the shell owns the window, timing and drawing, this module owns what the
//! operator sees and touches. Nothing here holds state of its own. Every value
//! it reads arrives through StatusInfo, every change it wants leaves through
//! UiCommands, and the shell applies those after the declaration has finished.
//!
//! The deferral is not stylistic. The layout solver walks the tree the
//! declaration built, so a setting mutated part way through would be read at two
//! different values inside one frame.
//!
//! The side panel is composed from sections listed per tab in the configuration.
//! Each section appears on exactly one tab by default, and the tab names say
//! what is on them. The settings tab is not composable and always carries the
//! layout editor, which is what keeps a composition recoverable however badly it
//! was edited.

use crate::audio::OutputStatus;
use crate::progress::CharRow;
use crate::session::SessionView;
use crate::config::settings::{
    DrillMode, DrillUnit, EnvelopeShape, LessonMethod, LogLevelCfg, MaterialSource, PaddleMode,
    PaddleSource, PanelSection, PANEL_TABS,
};
use crate::config::ConfigEnum;
use crate::config::Settings;
use crate::font::FontId;
use crate::gui::layout::{Align, Style};
use crate::gui::{Frame, TextAlign, WindowButton};
use crate::platform::MouseButton;
use crate::render::Color;

/// Tags of the areas the application draws itself. Declared here because this
/// module reserves them and the shell only fills them in.
pub const TAG_SCOPE: u32 = 1;
pub const TAG_PROMPT: u32 = 2;
/// Accuracy bars and the confusion matrix.
///
/// Inside the statistics group rather than in the data area, because it is
/// consulted between sessions rather than read during one: given a share of the
/// window it would take height from the keying picture, which is read while the
/// ear is busy.
pub const TAG_HEATMAP: u32 = 3;

/// The two levers of the key.
///
/// Three things at once, and each of them is a question an operator asks within
/// the first minute. Whether the contacts are reaching the machine at all, which
/// nothing else on the screen answers. Which lever sends which element, which
/// the wiring decides and the operator forgets. And a target for somebody with
/// no paddle soldered to anything, so the three modes can be heard before a
/// decision is made about which to wire for.
pub const TAG_PADDLE: u32 = 4;

/// The element being keyed, against the one expected.
///
/// Beside the prompt rather than in the panel, because it is read while the hand
/// is moving: a diagnostic that lives on another tab is a diagnostic consulted
/// after the mistake rather than during it. Declared only while the student is
/// the one sending, where there is something to decode.
pub const TAG_DECODER: u32 = 5;

/// Bounds of the side panel, in logical units.
///
/// The floor is what the widest settings row needs before its label column hits
/// its own floor and the control beside it collapses. The ceiling is a fraction
/// of the window rather than a constant, because the panel competes with the
/// keying picture for width.
pub const SIDE_PANEL_MIN: f32 = 240.0;
pub const SIDE_PANEL_MAX: f32 = 900.0;
pub const SIDE_PANEL_MAX_FRACTION: f32 = 0.5;

/// Tab strip.
///
/// The first four are composable and index the configuration lists directly.
/// The last is fixed: it configures the panel, including the composition of the
/// other four.
const TAB_KEYS: [&str; PANEL_TABS + 1] = [
    "panel.practice",
    "panel.lesson",
    "panel.sound",
    "panel.display",
    "panel.settings",
];

pub const TAB_SETTINGS: usize = PANEL_TABS;

/// One frame of a scope drag.
///
/// A named structure rather than a tuple because the button decides what the
/// fraction means, and a caller reading a bare pair would have to remember
/// which field was which.
#[derive(Debug, Clone, Copy)]
pub struct ScopeDrag {
    /// Position across the area, nought to one.
    pub fraction: f32,
    pub button: MouseButton,
    /// True only on the frame the drag began.
    ///
    /// A gesture that grabs something has to decide what it grabbed at the
    /// moment of the press and hold that decision: recomputing it per frame lets
    /// the target change underneath the pointer, which turns a grab into a jump.
    pub started: bool,
    /// Shift as of the press, held for the whole gesture.
    pub shift: bool,
}

/// Positions the operator picked in the lists.
///
/// Held together because they are interface position rather than configuration:
/// they say which entry is highlighted, and the configuration only changes once
/// the shell applies the corresponding command.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selections {
    pub device: usize,
}

/// Read only values handed to the interface builder.
pub struct StatusInfo<'a> {
    pub fps: f32,
    pub worst_ms: f32,
    pub draw_calls: u32,
    pub uploads: u32,
    /// Device side frame time, nought when not measured.
    pub gpu_ms: f32,
    pub gpu_worst_ms: f32,
    pub glyphs: usize,
    pub atlas: f32,
    pub gpu: &'a str,
    pub font: &'a str,
    pub dpi: f32,
    pub languages: &'a [&'a str],
    pub language_index: usize,
    /// Output endpoints, the system default first.
    pub devices: &'a [&'a str],
    pub audio: &'a OutputStatus,
    /// Times a failed endpoint was brought back on its own.
    ///
    /// Reported because an output that quietly restarts itself is a cable that
    /// wants replacing, and papering over it silently would hide exactly that.
    pub audio_recoveries: u32,
    pub session: &'a SessionView,
    /// Characters the generator is drawing from.
    pub character_pool: &'a str,
    /// Accuracy over the current set, nothing until it has been practised.
    pub set_accuracy: Option<f32>,
    /// Character of the set being missed most.
    pub weakest: Option<char>,
    /// One row per character of the set.
    pub rows: &'a [CharRow],
    /// Largest entry in the confusion matrix.
    pub matrix_peak: u32,
    pub sessions: u32,
    /// Name of the session log, so the operator knows what to open.
    pub sessions_file: &'a str,
    /// Characters keyed that the alphabet does not hold.
    pub sent_malformed: u32,
    /// Seconds of keying the picture holds.
    pub scope_history_s: f32,
    /// Seconds before the newest sample that the right edge shows.
    pub scope_end_s: f32,
    /// Cursor positions, as ages.
    pub cursor_a: Option<f32>,
    pub cursor_b: Option<f32>,
    /// Seconds between the two cursors.
    pub measurement_s: Option<f32>,
    /// Dot length at the current speed, for the measurement in units.
    pub dot_ms: f32,
    /// Settings that differ from the values the build ships with.
    pub deviations: &'a [String],
    /// Smallest width the current wording fits in.
    ///
    /// Measured rather than stated: the drag has to refuse a width that would
    /// cut the captions, and only the widget system knows how wide they came out
    /// in the language that is loaded.
    pub side_panel_min: f32,
    pub side_panel_max: f32,
    /// Height of the whole surface, for the splitter arithmetic.
    pub height: f32,
    pub window_maximized: bool,
}

/// Actions requested by the interface.
#[derive(Default)]
pub struct UiCommands {
    pub start_session: bool,
    pub stop_session: bool,
    pub skip_group: bool,
    /// Send the same material again before the answer is taken.
    pub repeat_group: bool,
    /// Take the answer now.
    pub submit: bool,
    pub test_tone: bool,
    pub rescan_devices: bool,
    pub select_device: bool,
    pub restart_output: bool,
    pub stop_output: bool,
    pub reset_progress: bool,
    /// Measurement cursors, cleared from the panel.
    pub clear_marks: bool,
    /// Cleared by the gesture, which is a different statement: the panel button
    /// is reachable while the pointer is elsewhere and the gesture is not.
    pub scope_clear_gesture: bool,
    /// Return the picture to the newest sample.
    pub scope_live: bool,
    /// Wheel over the picture: position and notches turned.
    pub scope_zoom: Option<(f32, f32)>,
    pub scope_drag: Option<ScopeDrag>,
    pub reset_tab: Option<usize>,
    pub language: Option<String>,
    pub log_level: Option<crate::core::log::Level>,
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
    sel: &mut Selections,
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
    data_area(f, settings, status, cmd, caption_h, status_h);
    if settings.ui.show_settings_panel {
        side_panel(f, settings, status, sel, *tab, editing_tab, add_section, cmd);
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
/// The drag zone is whatever the row has left over. That is a layout answer to a
/// question the window proc cannot answer: with a custom frame the whole client
/// area reports as client, so the only thing that separates a handle from a
/// button is which of them the layout put there.
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
        f.gap(f.ui.theme.padding);
        let color = f.ui.theme.text;
        f.label_styled("CWDrill / ", color, FontId::Ui, TextAlign::Left, Style::row());
        f.separator_vertical();
    }

    // Selecting a tab reveals the panel. The settings tab in particular has no
    // meaning without it: it is the tab that configures the panel.
    let keys: Vec<&str> = TAB_KEYS.to_vec();
    if f.tabs("toolbar", tab, &keys) {
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

    if f.button(if status.session.running { "action.stop" } else { "action.start" }) {
        if status.session.running {
            cmd.stop_session = true;
        } else {
            cmd.start_session = true;
        }
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

/// Keying picture above, session text below.
///
/// The picture is on top because it is read while the material plays and the
/// text is read after it: the eye travels down, which is the direction it
/// travels anyway.
#[allow(clippy::too_many_arguments)]
fn data_area(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
    caption_h: f32,
    status_h: f32,
) {
    f.begin(Style::column().grow(1.0));

    // True while the readout is drawn over the picture, which is the case where
    // the panel below it has nothing left to say.
    let overlaid = settings.scope.visible && settings.appearance.hud;

    if settings.scope.visible {
        f.begin_panel("panel.scope", Style::column().grow(1.0).min_h(f.ui.m(80.0)));
        f.custom(TAG_SCOPE, Style::row().grow(1.0));
        f.end_panel();

        // Four gestures over one surface, separated by button rather than by
        // region: a region a gesture belongs to would have to be visible, and
        // there is nothing to draw it on.
        //
        // The two buttons place the two measurement cursors, which is what turns
        // the picture from an illustration into an instrument: an element that is
        // plainly not three units long says nothing until the two edges have been
        // measured. The press snaps to the nearest edge, because a press states a
        // neighbourhood and the picture states where inside it the crossing is;
        // the drag does not, or it would jump away from the pointer. Shift
        // inverts the two, so both are reachable in either gesture.
        //
        // The middle button pans and the wheel magnifies about the pointer, which
        // is where an operator expects it and the only place that keeps the
        // element they are looking at under the cursor.
        if let Some((x, notches)) = f.ui.custom_wheel(TAG_SCOPE) {
            let speed = if f.ui.mods().ctrl {
                4.0
            } else if f.ui.mods().shift {
                0.25
            } else {
                1.0
            };
            cmd.scope_zoom = Some((x, notches * speed));
        }
        // The double click is tested before the drag, because the second press
        // of a pair arrives as both and clearing is the more specific reading.
        if let Some((_, _)) = f.ui.custom_double_click(TAG_SCOPE) {
            cmd.scope_clear_gesture = true;
        } else {
            for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
                if let Some(drag) = f.ui.custom_drag(TAG_SCOPE, button) {
                    cmd.scope_drag = Some(ScopeDrag {
                        fraction: drag.x,
                        button,
                        started: drag.started,
                        shift: drag.mods.shift,
                    });
                    break;
                }
            }
        }

        // Declared only where something sits on both sides of it. A handle that
        // separates one area from nothing does nothing, and it still takes a row
        // of height and still reports a drag against a fraction nobody reads.
        if !overlaid {
            let usable = (status.height - caption_h - status_h).max(1.0);
            let dy = f.splitter("prompt_split", false);
            if dy != 0.0 {
                let next = settings.ui.prompt_panel_fraction - dy / usable;
                settings.ui.prompt_panel_fraction = next.clamp(0.1, 0.9);
            }
        }
    }

    // ## Why this panel is conditional
    //
    // It carries the prompt, the answer and the pattern, and so does the frame
    // over the picture. Declared together the two said the same thing at
    // opposite edges of the window, and the second copy cost half the height of
    // the picture to say it.
    //
    // So the frame replaces this rather than joining it. What remains is the
    // arrangement the frame cannot serve: with the picture switched off there is
    // nothing to draw the frame over, and the text needs somewhere to be.
    if !overlaid {
        let style = if settings.scope.visible {
            Style::column()
                .basis_percent(settings.ui.prompt_panel_fraction)
                .min_h(f.ui.m(80.0))
        } else {
            Style::column().grow(1.0)
        };
        f.begin_panel("panel.prompt", style);

        // What the exchange is asking for, which copy is playing, and what the
        // token means once it has been answered. Above the text, because all
        // three are read in the same glance as it.
        context_line(f, status);

        f.custom(TAG_PROMPT, Style::row().grow(1.0));

        // Only while the student is the one keying: an empty pattern row in
        // every other arrangement is a permanent reminder of one nobody is in.
        if settings.practice.keying() {
            f.custom(TAG_DECODER, Style::row().height_px(f.ui.m(40.0)));
        }
        f.end_panel();
    }

    // Under whichever of the two carried the text, because that is where the
    // eyes are: a transport reachable only by looking away is a transport used
    // once and then replaced by the keyboard.
    if overlaid {
        // Its own surface, so the row reads as a bar rather than as buttons
        // loose on the background where the panel used to end.
        f.begin_frame(Style::column(), f.ui.theme.panel, Color::TRANSPARENT);
        transport(f, status, cmd);
        f.end();
    } else {
        transport(f, status, cmd);
    }

    f.end();
}

/// What is being asked for, above the text.
///
/// Nothing at all when there is nothing to say, which is the ordinary case for a
/// group: a row of dashes standing in for absent information is information
/// nobody can act on.
fn context_line(f: &mut Frame<'_>, status: &StatusInfo<'_>) {
    let s = status.session;
    let has_fields = s.exchange && !s.fields.is_empty();
    let has_copies = s.copies > 1 && s.copy > 0;
    let has_gloss = s.scored && s.gloss.is_some();
    let has_fallback = s.fallback.is_some();
    if !has_fields && !has_copies && !has_gloss && !has_fallback {
        return;
    }

    let gap = f.ui.m(f.ui.theme.gap);
    let height = f.ui.m(f.ui.theme.row_height);
    f.begin(Style::row().height_px(height).gap(gap).align(Align::Center)
        .padding_xy(f.ui.m(12.0), 0.0));

    if has_fields {
        // Each field named, and marked once it has been scored. The verdict is
        // beside the name rather than in the panel, because the two together are
        // one statement: this is what you were asked for and this is what you
        // got.
        for field in &s.fields {
            let colour = if !s.scored {
                f.ui.theme.text_dim
            } else if field.right {
                Color::hex(0x5FBF6A)
            } else {
                Color::hex(0xD05050)
            };
            let name = f.ui.tr(field.key).to_string();
            f.label_styled(&name, colour, FontId::Ui, TextAlign::Left, Style::row());
        }
    }

    if has_gloss {
        if let Some(text) = s.gloss {
            // After the answer rather than before it. A gloss before the answer
            // would be the answer.
            let colour = f.ui.theme.accent;
            f.label_styled(text, colour, FontId::Ui, TextAlign::Left, Style::row().shrink(1.0));
        }
    }

    if let Some(key) = s.fallback {
        f.hint(key);
    }

    f.spacer(1.0);

    if has_copies {
        let text = format!("{} / {}", s.copy, s.copies);
        let colour = f.ui.theme.text_dim;
        f.label_mono(&text, colour, TextAlign::Right, Style::row());
    }

    f.end();
}

/// The four commands, under the text.
///
/// Under the text and not in the side panel, because during a session the eyes
/// are on the text: a transport reachable only by looking away is a transport
/// used once and then replaced by the keyboard, and the keyboard shortcuts are
/// not written anywhere the student is looking either.
///
/// AGAIN sends the material a second time before the answer is taken, which is
/// what an operator receives after asking for it on the air. What has been typed
/// is kept, because a repetition is a second hearing rather than a second
/// question.
fn transport(f: &mut Frame<'_>, status: &StatusInfo<'_>, cmd: &mut UiCommands) {
    let s = status.session;
    let gap = f.ui.m(f.ui.theme.gap);
    let height = f.ui.m(f.ui.theme.row_height + 6.0);

    f.begin(Style::row().height_px(height).gap(gap).align(Align::Center)
        .padding_xy(f.ui.m(12.0), f.ui.m(3.0)));

    f.begin_disabled(!status.audio.running);
    if f.button(if s.running { "action.stop" } else { "action.start" }) {
        if s.running {
            cmd.stop_session = true;
        } else {
            cmd.start_session = true;
        }
    }
    f.end_disabled();

    f.separator_vertical();

    // Nothing to hear again while the student is the one sending, and nothing to
    // repeat before anything has been drawn.
    let can_repeat = s.running && !s.scored && s.characters > 0 && s.repeatable;
    f.begin_disabled(!can_repeat);
    if f.button("action.repeat") {
        cmd.repeat_group = true;
    }
    f.end_disabled();

    f.begin_disabled(!s.running);
    if f.button("action.skip") {
        cmd.skip_group = true;
    }
    if f.button("action.submit") {
        cmd.submit = true;
    }
    f.end_disabled();

    f.spacer(1.0);

    // The score, where it can be read without leaving the text. Only once
    // something has been answered, so an idle session shows a clean row.
    if s.characters > 0 {
        let text = format!("{:.0} %", s.accuracy * 100.0);
        let colour = f.ui.theme.text;
        f.label_mono(&text, colour, TextAlign::Right, Style::row());
    }

    f.end();
}

/// Side panel. Its contents are the sections listed for the active tab, or the
/// fixed application settings when the settings tab is selected.
#[allow(clippy::too_many_arguments)]
fn side_panel(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    sel: &mut Selections,
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
        settings.ui.side_panel_width = next.clamp(status.side_panel_min, status.side_panel_max);
    }

    let gap = f.ui.m(f.ui.theme.gap);
    let margin = f.ui.m(f.ui.theme.panel_margin);
    let inset = f.ui.m(f.ui.theme.panel_inset);
    let width = f.ui.m(settings.ui.side_panel_width);

    f.begin_frame(
        Style::column().width_px(width).padding_xy(inset, 0.0),
        f.ui.theme.panel,
        Color::TRANSPARENT,
    );
    f.begin_scroll("side_scroll", Style::column().grow(1.0).gap(gap).padding(margin));

    if tab == TAB_SETTINGS {
        application(f, settings, status, cmd);
        appearance(f, settings, cmd);
        data_look(f, settings);
        layout_editor(f, settings, editing_tab, add_section, cmd);
        deviations(f, status);
    } else {
        // The list is copied because the section builders take the settings
        // mutably, and the list lives inside them.
        let order: Vec<PanelSection> = settings.panel.sections(tab).to_vec();
        if order.is_empty() {
            f.hint("hint.layout_empty");
        }
        for which in order {
            section(f, which, settings, status, sel, cmd);
        }
    }

    f.end_scroll();
    f.end();
}

/// Dispatches one section of the composable panel.
fn section(
    f: &mut Frame<'_>,
    which: PanelSection,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    sel: &mut Selections,
    cmd: &mut UiCommands,
) {
    match which {
        PanelSection::Session => section_session(f, status, cmd),
        PanelSection::Input => section_input(f, settings),
        PanelSection::Paddle => section_paddle(f, settings, status),
        PanelSection::Progress => section_progress(f, settings, status, cmd),
        PanelSection::Lesson => section_lesson(f, settings, status),
        PanelSection::Material => section_material(f, settings),
        PanelSection::Timing => section_timing(f, settings),
        PanelSection::Tone => section_tone(f, settings, cmd),
        PanelSection::Conditions => section_conditions(f, settings, status),
        PanelSection::Device => section_device(f, settings, status, sel, cmd),
        PanelSection::Scope => section_scope(f, settings, status, cmd),
        PanelSection::Heatmap => section_heatmap(f, status),
    }
}

// ------------------------------------------------------------------ sections

/// The answers that have been scored.
///
/// ## Why this rather than a second copy of the figures
///
/// The phase, the clock and the score are read while a group is playing, so they
/// are drawn over the keying picture; repeating them here made two readouts of
/// one fact on opposite edges of the screen, and the fold made the second the
/// slower of the two to reach.
///
/// What is left is the thing nothing else holds. A scored answer disappears at
/// the moment it becomes worth reading: the reveal lasts a fraction of a second
/// and the next group begins. The lines below are the only place an operator can
/// look at the group they have just got wrong.
fn section_session(f: &mut Frame<'_>, status: &StatusInfo<'_>, cmd: &mut UiCommands) {
    let s = status.session;
    let open = f.begin_group("group.session");
    f.mark_active(s.running);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        f.readout(
            "field.session.sent",
            &format!("{} in {} groups", s.characters, s.groups),
        );

        if s.characters > 0 {
            // The figure a contact is judged on, which is a different question
            // from the share of characters copied: an operator who logged the
            // wrong serial made an error whatever else they heard.
            if s.fields_total > 0 {
                f.readout(
                    "field.session.field_score",
                    &format!("{} / {}", s.fields_right, s.fields_total),
                );
            }
            if s.reaction_ms > 0.0 {
                f.readout("field.session.reaction", &format!("{:.0} ms", s.reaction_ms));
            } else if s.copies > 1 {
                f.hint("hint.session_no_reaction");
            }
            if s.inserted > 0 {
                f.readout("field.session.inserted", &format!("{}", s.inserted));
                f.hint("hint.session_inserted");
            }
        }

        // Newest first, because that is the one being thought about. Sent on the
        // left and answered on the right, so the two are compared by running the
        // eye across one line rather than by remembering the line above.
        if s.log.is_empty() {
            f.hint("hint.session_log_empty");
        } else {
            f.separator();
            let dim = f.ui.theme.text_dim;
            for entry in s.log.iter().rev() {
                let colour = if entry.right {
                    Color::hex(0x5FBF6A)
                } else {
                    Color::hex(0xD05050)
                };
                f.begin(Style::row().gap(gap).align(Align::Center));
                f.label_mono(
                    &entry.sent,
                    dim,
                    TextAlign::Left,
                    Style::row().grow(1.0).shrink(1.0),
                );
                f.label_mono(
                    &entry.typed,
                    colour,
                    TextAlign::Left,
                    Style::row().grow(1.0).shrink(1.0),
                );
                f.end();
            }
            f.separator();
        }

        f.begin(Style::row().gap(gap));
        f.begin_disabled(!status.audio.running);
        if f.button(if s.running { "action.stop" } else { "action.start" }) {
            if s.running {
                cmd.stop_session = true;
            } else {
                cmd.start_session = true;
            }
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();
        if !status.audio.running {
            f.hint("hint.session_needs_output");
        }

        // The transport lives under the text, which is where the eyes are during
        // a session. This one is here because a session is also started from a
        // panel that may be the only thing open.
        f.hint("hint.session_transport");
        f.hint("hint.session_live");
        f.hint("hint.session_keys");
    }
    f.end_group();
}

fn section_input(f: &mut Frame<'_>, settings: &mut Settings) {
    if f.begin_group("group.input") {
        // First, and the mode is dead underneath it. Each drill states both what
        // is played and what answers it, so a mode left live would be a second
        // answer to a question already settled.
        f.enum_combo("field.practice.drill", &mut settings.practice.drill);
        let drilling = settings.practice.drill != DrillMode::Off;
        if drilling {
            f.enum_combo("field.practice.drill_unit", &mut settings.practice.drill_unit);
            // Only for the character exercise. A word is drawn from a list and
            // has no newest member for the weight to name.
            if settings.practice.drill_unit == DrillUnit::Character {
                f.slider(
                    "field.practice.drill_focus",
                    &mut settings.practice.drill_focus,
                    0.0,
                    8.0,
                    1,
                    "",
                );
                f.hint("hint.drill_focus");
            }
            f.hint(match settings.practice.drill {
                DrillMode::Recall => "hint.drill_recall",
                DrillMode::Echo => "hint.drill_echo",
                _ => "hint.drill_blind",
            });
        } else {
            f.hint("hint.drill");
        }

        f.begin_disabled(drilling);
        f.enum_combo("field.practice.mode", &mut settings.practice.mode);
        f.end_disabled();
        if drilling {
            f.hint("hint.drill_overrides");
        } else if settings.practice.keying() {
            f.hint("hint.practice_send");
        }
        f.enum_combo("field.practice.case", &mut settings.practice.case);

        // Nothing below applies while the student only listens: there is no
        // answer to reveal and nothing to time.
        let answering = !settings.practice.listening();
        f.begin_disabled(!answering);
        f.toggle("field.practice.reveal", &mut settings.practice.reveal);
        f.begin_disabled(!settings.practice.reveal);
        f.slider_u32(
            "field.practice.reveal_delay",
            &mut settings.practice.reveal_delay_ms,
            0,
            5000,
            "unit.ms",
        );
        f.end_disabled();
        f.hint("hint.practice_reveal");

        f.toggle("field.practice.backspace", &mut settings.practice.allow_backspace);
        f.hint("hint.practice_backspace");
        f.slider_u32(
            "field.practice.timeout",
            &mut settings.practice.answer_timeout_ms,
            500,
            60000,
            "unit.ms",
        );
        f.toggle("field.practice.strict", &mut settings.practice.strict);
        f.end_disabled();
    }
    f.end_group();
}

/// The key the student sends with.
///
/// Held apart from the answering section because the two describe different
/// halves of the exercise: that one decides what counts as an answer, this one
/// decides what the operator answers with.
fn section_paddle(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let sending = settings.practice.keying();
    let live = sending && status.session.running;
    let open = f.begin_group("group.paddle");
    f.mark_active(live);
    if open {
        // First, because it is the answer to whether any of the rest is doing
        // anything: a control that cannot be seen working is a control that gets
        // adjusted in the dark.
        f.custom(TAG_PADDLE, Style::row().height_px(f.ui.m(52.0)));
        f.hint("hint.paddle_levers");

        f.enum_combo("field.paddle.mode", &mut settings.paddle.mode);
        f.hint(if settings.paddle.mode == PaddleMode::Straight {
            "hint.paddle_straight"
        } else {
            "hint.paddle_iambic"
        });

        f.enum_combo("field.paddle.source", &mut settings.paddle.source);
        f.toggle("field.paddle.swap", &mut settings.paddle.swap);
        f.toggle("field.paddle.sidetone", &mut settings.paddle.sidetone);

        // The two letters are dead while the contacts come from the mouse: a
        // field that is read in one arrangement and ignored in the other is a
        // field that can only be misread.
        let keys = matches!(
            settings.paddle.source,
            PaddleSource::Keyboard | PaddleSource::Both
        );
        f.begin_disabled(!keys);
        f.text_edit("field.paddle.key_dit", &mut settings.paddle.key_dit);
        f.text_edit("field.paddle.key_dah", &mut settings.paddle.key_dah);
        f.end_disabled();
        if keys {
            f.hint("hint.paddle_letters");
        }

        // The two figures the key is sent at, restated here because they belong
        // to the timing section and an operator adjusting a key should not have
        // to find another tab to see what it will produce.
        let dot = settings.timing.dot_seconds();
        f.readout(
            "field.paddle.speed",
            &format!(
                "{:.0} {}   {:.0} ms",
                settings.timing.char_wpm,
                f.ui.tr("unit.wpm"),
                dot * 1000.0
            ),
        );
        f.readout(
            "field.paddle.dash",
            &format!("{:.1} {}", settings.timing.weight, f.ui.tr("unit.units")),
        );

        // The delay between the press and the tone, which is the one figure that
        // decides whether the key is usable. Measured rather than derived from
        // the setting: the endpoint hands back whatever buffer it likes, and the
        // difference between the two used to be invisible.
        if status.audio.running && status.audio.device_rate > 0 {
            let queued =
                status.audio.latency_frames as f32 * 1000.0 / status.audio.device_rate as f32;
            f.readout("field.paddle.latency", &format!("{:.0} ms", queued));
            if queued > 25.0 {
                f.hint("hint.paddle_slow");
            }
        }

        // Stated only once it has happened. A permanent nought would be noise,
        // and a figure that climbs is a hand that is running its elements
        // together or leaving gaps inside a character.
        if status.sent_malformed > 0 {
            f.readout("field.paddle.malformed", &format!("{}", status.sent_malformed));
            f.hint("hint.paddle_malformed");
        }

        if !sending {
            f.hint("hint.paddle_needs_mode");
        } else {
            f.hint("hint.paddle_capture");
            if settings.paddle.mode == PaddleMode::Straight {
                f.hint("hint.paddle_hold");
            }
            f.hint("hint.paddle_latency");
        }
    }
    f.end_group();
}

fn section_progress(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.progress") {
        let gap = f.ui.m(f.ui.theme.gap);

        f.readout("field.progress.level", &format!("{}", settings.lesson.level));
        // The one figure the level decision reads. Reported so the operator can
        // see why the level moved, or why it did not.
        match status.set_accuracy {
            Some(accuracy) => f.readout(
                "field.progress.accuracy",
                &format!(
                    "{:.0} %   {} / {}",
                    accuracy * 100.0,
                    (settings.lesson.advance_accuracy * 100.0) as u32,
                    (settings.lesson.regress_accuracy * 100.0) as u32
                ),
            ),
            None => f.hint("hint.progress_untested"),
        }
        if let Some(ch) = status.weakest {
            f.readout("field.progress.weakest", &ch.to_string());
        }
        f.readout("field.progress.sessions", &format!("{}", status.sessions));

        // Bounded by the storage rather than by taste: the window is bits in one
        // word, and a longer one would need a vector per character to hold
        // statistics that say nothing about the session in progress.
        f.slider_u32("field.progress.window", &mut settings.progress.window, 5, 64, "");
        f.hint("hint.progress_window");
        f.text_edit("field.progress.path", &mut settings.progress.path);
        f.toggle("field.progress.log", &mut settings.progress.log_sessions);
        f.begin_disabled(!settings.progress.log_sessions);
        f.readout("field.progress.log_file", status.sessions_file);
        f.slider_u32("field.progress.keep", &mut settings.progress.keep_sessions, 0, 10_000, "");
        f.hint(if settings.progress.keep_sessions == 0 {
            "hint.progress_keep_all"
        } else {
            "hint.progress_keep"
        });
        f.end_disabled();

        f.begin(Style::row().gap(gap));
        // Refused while a session is running: the history is written when the
        // session ends, so clearing it now would clear it and then have it
        // rewritten by the session that was already counting.
        f.begin_disabled(status.session.running);
        if f.button("action.reset_progress") {
            cmd.reset_progress = true;
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();
        if status.session.running {
            f.hint("hint.progress_running");
        }
    }
    f.end_group();
}

fn section_lesson(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    if f.begin_group("group.lesson") {
        f.enum_combo("field.lesson.method", &mut settings.lesson.method);
        f.hint("hint.lesson_koch");

        // The level counts characters of a generated order, so it means nothing
        // when the operator states the set by hand.
        let generated = settings.lesson.method != LessonMethod::Custom;
        f.begin_disabled(!generated);
        f.slider_u32("field.lesson.level", &mut settings.lesson.level, 2, 64, "unit.chars");
        f.end_disabled();

        f.begin_disabled(generated);
        f.text_edit("field.lesson.custom_set", &mut settings.lesson.custom_set);
        f.end_disabled();

        f.slider_u32(
            "field.lesson.session",
            &mut settings.lesson.session_seconds,
            30,
            3600,
            "unit.s",
        );

        f.toggle("field.lesson.auto", &mut settings.lesson.auto_advance);
        f.begin_disabled(!settings.lesson.auto_advance);
        f.slider("field.lesson.advance", &mut settings.lesson.advance_accuracy, 0.5, 1.0, 2, "");
        // Bounded below the advance threshold, otherwise the level would rise
        // and fall on the same reading.
        let ceiling = (settings.lesson.advance_accuracy - 0.05).max(0.0);
        f.slider("field.lesson.regress", &mut settings.lesson.regress_accuracy, 0.0, ceiling, 2, "");
        f.hint("hint.lesson_regress");
        f.end_disabled();

        f.slider("field.lesson.weak_weight", &mut settings.lesson.weak_weight, 0.0, 8.0, 1, "");

        // The pool rather than the level, because that is the question a student
        // asks: which characters am I hearing. Here rather than in the session,
        // because it is a property of the lesson and it changes when the level
        // does.
        f.readout("field.lesson.pool", status.character_pool);
    }
    f.end_group();
}

fn section_material(f: &mut Frame<'_>, settings: &mut Settings) {
    if f.begin_group("group.material") {
        f.enum_combo("field.material.source", &mut settings.material.source);
        f.hint("hint.material_groups");
        if matches!(
            settings.material.source,
            MaterialSource::QCodes | MaterialSource::Abbrev
        ) {
            f.hint("hint.material_vocab");
        }

        // Group length is a property of random groups. A word has the length the
        // word has, and stretching it would be inventing one.
        // A vocabulary token has the length it has, and a callsign has a shape.
        // Only the two sources that assemble something from nothing take a length.
        let grouped = settings.material.source == MaterialSource::Groups
            || settings.material.source == MaterialSource::Numbers;
        f.begin_disabled(!grouped);
        f.slider_u32("field.material.min_group", &mut settings.material.min_group, 1, 20, "");
        let floor = settings.material.min_group;
        f.slider_u32("field.material.max_group", &mut settings.material.max_group, floor, 20, "");
        f.end_disabled();

        f.toggle("field.material.numbers", &mut settings.material.include_numbers);
        f.toggle("field.material.punctuation", &mut settings.material.include_punctuation);
        f.toggle("field.material.prosigns", &mut settings.material.include_prosigns);

        // A word list serves two sources, so the field is live for both: one
        // reads a list of words and the other reads prose, and what a trainer
        // wants from either is the words.
        let needs_file = matches!(
            settings.material.source,
            MaterialSource::File | MaterialSource::Words
        );
        f.begin_disabled(!needs_file);
        f.text_edit("field.material.file", &mut settings.material.file_path);
        f.end_disabled();
        if needs_file {
            f.hint("hint.material_file");
            f.hint("hint.material_builtin");
        }
        if settings.material.source == MaterialSource::Qso {
            f.hint("hint.material_qso");
            f.hint("hint.material_qso_answer");
        }
        if settings.material.source == MaterialSource::Callsigns {
            f.hint("hint.material_callsign");
        }

        f.slider_u32("field.material.repeat", &mut settings.material.repeat, 1, 5, "");
        if settings.material.repeat > 1 {
            f.hint("hint.material_repeat");
        }
    }
    f.end_group();
}

fn section_timing(f: &mut Frame<'_>, settings: &mut Settings) {
    if f.begin_group("group.timing") {
        let t = &mut settings.timing;

        f.slider("field.timing.char_wpm", &mut t.char_wpm, 5.0, 60.0, 0, "unit.wpm");
        f.toggle("field.timing.farnsworth", &mut t.farnsworth);
        // The text speed cannot exceed the character speed: the gaps would have
        // to be negative, which is not a slower text but a different alphabet.
        f.begin_disabled(!t.farnsworth);
        let ceiling = t.char_wpm;
        f.slider("field.timing.text_wpm", &mut t.text_wpm, 3.0, ceiling, 0, "unit.wpm");
        f.end_disabled();
        f.hint("hint.timing_two_speeds");

        // Three readings that answer the one question the two speeds raise: how
        // long an element actually is, and how far apart the characters and the
        // words end up. Stated in units rather than seconds, because that is what
        // the definition states and what a printed exercise compares against.
        let dot = t.dot_seconds();
        let (_, char_gap, word_gap) = t.gaps();
        f.readout("field.timing.dot", &format!("{:.0} ms", dot * 1000.0));
        f.readout(
            "field.timing.gaps",
            &format!(
                "{:.1} / {:.1} {}",
                char_gap / dot.max(1e-6),
                word_gap / dot.max(1e-6),
                f.ui.tr("unit.units")
            ),
        );

        f.slider("field.timing.weight", &mut t.weight, 2.0, 4.5, 2, "");
        f.hint("hint.timing_weight");

        f.slider("field.timing.element_gap", &mut t.element_gap, 0.5, 2.0, 2, "unit.units");
        // With the two speeds apart the two gaps below are not settings: they
        // follow from the requirement that the text occupy the time the text
        // speed asks for. Greyed rather than silently overridden, because a
        // control that is read in one arrangement and ignored in the other is a
        // control that can only be misread.
        let stretched = t.farnsworth && t.text_wpm < t.char_wpm;
        f.begin_disabled(stretched);
        f.slider("field.timing.char_gap", &mut t.char_gap, 2.0, 12.0, 1, "unit.units");
        f.slider("field.timing.word_gap", &mut t.word_gap, 4.0, 24.0, 1, "unit.units");
        f.end_disabled();
        if stretched {
            f.hint("hint.timing_gaps_derived");
        }

        f.slider("field.timing.jitter", &mut t.jitter_percent, 0.0, 40.0, 0, "unit.percent");
        f.hint("hint.timing_jitter");
        f.slider("field.timing.swing", &mut t.swing_percent, 0.0, 30.0, 0, "unit.percent");
        f.hint("hint.timing_swing");
    }
    f.end_group();
}

fn section_tone(f: &mut Frame<'_>, settings: &mut Settings, cmd: &mut UiCommands) {
    if f.begin_group("group.tone") {
        let gap = f.ui.m(f.ui.theme.gap);
        let t = &mut settings.tone;

        f.slider("field.tone.pitch", &mut t.pitch_hz, 200.0, 1500.0, 0, "unit.hz");
        f.slider("field.tone.volume", &mut t.volume, 0.0, 1.0, 2, "");
        f.enum_combo("field.tone.shape", &mut t.shape);
        f.hint("hint.tone_shape");

        // A hard edge has no duration to state, which is what makes it hard.
        f.begin_disabled(t.shape == EnvelopeShape::Hard);
        f.slider("field.tone.rise", &mut t.rise_ms, 0.0, 20.0, 1, "unit.ms");
        f.slider("field.tone.fall", &mut t.fall_ms, 0.0, 20.0, 1, "unit.ms");
        f.end_disabled();

        f.slider("field.tone.pan", &mut t.pan, -1.0, 1.0, 2, "");
        f.hint("hint.tone_pan");

        f.begin(Style::row().gap(gap));
        if f.button("action.test_tone") {
            cmd.test_tone = true;
        }
        f.spacer(1.0);
        f.end();
        f.hint("hint.tone_test");
    }
    f.end_group();
}

fn section_conditions(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>) {
    let c = &settings.conditions;
    let active = c.noise || c.qsb || c.qrm || c.qrn || c.drift_hz_per_min != 0.0;
    let open = f.begin_group("group.conditions");
    f.mark_active(active);
    if open {
        f.hint("hint.conditions");
        let c = &mut settings.conditions;

        f.toggle("field.conditions.noise", &mut c.noise);
        f.begin_disabled(!c.noise);
        f.slider("field.conditions.snr", &mut c.snr_db, -10.0, 40.0, 0, "unit.db");
        f.end_disabled();

        f.toggle("field.conditions.qsb", &mut c.qsb);
        f.begin_disabled(!c.qsb);
        f.slider("field.conditions.qsb_rate", &mut c.qsb_rate_hz, 0.02, 2.0, 2, "unit.hz");
        f.slider("field.conditions.qsb_depth", &mut c.qsb_depth_db, 1.0, 40.0, 0, "unit.db");
        f.end_disabled();

        f.toggle("field.conditions.qrm", &mut c.qrm);
        f.begin_disabled(!c.qrm);
        f.slider("field.conditions.qrm_offset", &mut c.qrm_offset_hz, 20.0, 800.0, 0, "unit.hz");
        f.slider("field.conditions.qrm_level", &mut c.qrm_level_db, -30.0, 6.0, 0, "unit.db");
        f.hint("hint.conditions_qrm");
        f.end_disabled();
        if c.qrm && status.session.running {
            f.readout(
                "field.conditions.qrm_queued",
                &format!("{}", status.audio.qrm_pending),
            );
        }

        f.toggle("field.conditions.qrn", &mut c.qrn);
        f.begin_disabled(!c.qrn);
        f.slider("field.conditions.qrn_rate", &mut c.qrn_per_minute, 1.0, 600.0, 0, "unit.per_min");
        f.end_disabled();

        f.slider("field.conditions.drift", &mut c.drift_hz_per_min, -120.0, 120.0, 0, "unit.hz_per_min");
        f.hint("hint.conditions_drift");
        f.hint("hint.conditions_pan");
    }
    f.end_group();
}

fn section_device(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    sel: &mut Selections,
    cmd: &mut UiCommands,
) {
    let open = f.begin_group("group.device");
    f.mark_active(status.audio.running);
    if open {
        let gap = f.ui.m(f.ui.theme.gap);

        f.readout(
            "field.audio.state",
            if status.audio.running { "status.running" } else { "status.stopped" },
        );
        // The format the endpoint gave rather than one that was asked for. In
        // shared mode the mixer hands back its own whatever is requested, so
        // there is nothing to state and everything to report.
        if !status.audio.format.is_empty() {
            f.readout("field.audio.format", &status.audio.format);
        }
        if !status.audio.error.is_empty() {
            f.label_dim(&status.audio.error);
        }
        // Stated only once it has happened. A permanent nought would be noise,
        // and a figure that starts climbing is the one thing worth noticing.
        if status.audio_recoveries > 0 {
            f.readout("field.audio.recoveries", &format!("{}", status.audio_recoveries));
            f.hint("hint.audio_recovered");
        }
        if status.audio.underruns > 0 {
            f.readout("field.audio.underruns", &format!("{}", status.audio.underruns));
        }

        if status.devices.is_empty() {
            f.hint("hint.no_devices");
        } else if f.combo("field.audio.device", &mut sel.device, status.devices) {
            cmd.select_device = true;
        }

        f.begin(Style::row().gap(gap));
        if f.button(if status.audio.running { "action.stop" } else { "action.start" }) {
            if status.audio.running {
                cmd.stop_output = true;
            } else {
                cmd.restart_output = true;
            }
        }
        if f.button("action.rescan") {
            cmd.rescan_devices = true;
        }
        f.spacer(1.0);
        f.end();

        // Elements waiting to be sent. The one reading that says whether a timing
        // change has been heard yet: everything queued keeps the speed it was
        // made at, which is correct and is not obvious. Here rather than in the
        // session, because it is a property of the stream.
        f.readout("field.audio.queued", &format!("{}", status.audio.pending));

        // What the buffer setting actually produced. The endpoint offers whatever
        // buffer it likes and the request is a ceiling on how much of it is used,
        // so the two are different numbers and only one of them is audible.
        if status.audio.running && status.audio.device_rate > 0 {
            let queued =
                status.audio.latency_frames as f32 * 1000.0 / status.audio.device_rate as f32;
            f.readout("field.audio.latency", &format!("{:.0} ms", queued));
        }

        // Applied on the next open, because the buffer size is stated when the
        // endpoint is initialized and cannot be changed under a running stream.
        f.slider_u32("field.audio.buffer", &mut settings.audio.buffer_ms, 2, 200, "unit.ms");
        f.hint("hint.audio_buffer");
        f.hint("hint.audio_shared");
    }
    f.end_group();
}

fn section_scope(f: &mut Frame<'_>, settings: &mut Settings, status: &StatusInfo<'_>, cmd: &mut UiCommands) {
    if f.begin_group("group.scope") {
        let gap = f.ui.m(f.ui.theme.gap);

        f.toggle("field.scope.visible", &mut settings.scope.visible);
        f.begin_disabled(!settings.scope.visible);

        f.hint("hint.scope_gestures");
        f.slider("field.scope.seconds", &mut settings.scope.seconds, 0.05, 30.0, 2, "unit.s");
        f.slider("field.scope.height", &mut settings.scope.height_fraction, 0.1, 0.7, 2, "");
        f.readout(
            "field.scope.history",
            &format!("{:.1} {}", status.scope_history_s, f.ui.tr("unit.s")),
        );
        // Stated only while the picture is held back. A permanent nought would be
        // noise, and its absence is what says the picture is live.
        if status.scope_end_s > 0.001 {
            f.readout(
                "field.scope.held",
                &format!("{:.2} {}", status.scope_end_s, f.ui.tr("unit.s")),
            );
        }

        f.toggle("field.scope.unit_grid", &mut settings.scope.unit_grid);
        f.toggle("field.scope.ideal", &mut settings.scope.show_ideal);
        f.hint("hint.scope_ideal");
        f.toggle("field.scope.labels", &mut settings.scope.show_labels);
        f.hint("hint.scope_labels");
        f.toggle("field.scope.timing", &mut settings.scope.show_timing);
        f.hint("hint.scope_timing");

        // The measurement. Reported in both because the two answer different
        // questions: the milliseconds compare against a stopwatch and the units
        // compare against the definition.
        match status.measurement_s {
            Some(seconds) => {
                let ms = seconds * 1000.0;
                let units = if status.dot_ms > 0.0 { ms / status.dot_ms } else { 0.0 };
                f.readout(
                    "field.scope.measure",
                    &format!("{:.1} ms   {:.2} {}", ms, units, f.ui.tr("unit.units")),
                );
            }
            None => f.hint("hint.scope_measure"),
        }

        f.begin(Style::row().gap(gap));
        f.begin_disabled(status.cursor_a.is_none() && status.cursor_b.is_none());
        if f.button("action.clear_marks") {
            cmd.clear_marks = true;
        }
        f.end_disabled();
        f.begin_disabled(status.scope_end_s <= 0.001);
        if f.button("action.live") {
            cmd.scope_live = true;
        }
        f.end_disabled();
        f.spacer(1.0);
        f.end();

        f.end_disabled();
    }
    f.end_group();
}

fn section_heatmap(f: &mut Frame<'_>, status: &StatusInfo<'_>) {
    let open = f.begin_group("group.heatmap");
    f.mark_active(status.matrix_peak > 0);
    if open {
        // A count of the characters that have been answered enough to judge. The
        // one reading that says whether the picture below means anything yet.
        let tested = status.rows.iter().filter(|r| r.accuracy.is_some()).count();
        f.readout(
            "field.heatmap.tested",
            &format!("{} / {}", tested, status.rows.len()),
        );
        if status.matrix_peak > 0 {
            f.readout("field.heatmap.peak", &format!("{}", status.matrix_peak));
        }

        // The slowest character, which is a different question from the least
        // accurate: a character copied correctly after a second is one that is
        // being worked out rather than recognized.
        let slowest = status
            .rows
            .iter()
            .filter(|r| r.reaction_ms > 0.0)
            .max_by(|a, b| {
                a.reaction_ms
                    .partial_cmp(&b.reaction_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        if let Some(row) = slowest {
            f.readout(
                "field.heatmap.slowest",
                &format!("{}   {:.0} ms", row.ch, row.reaction_ms),
            );
        }

        // The character with the least practice behind it, which is where the
        // next reading of the picture is least trustworthy: a bar built from six
        // answers and one built from forty look the same and are not.
        if let Some(row) = status.rows.iter().min_by_key(|r| r.answers) {
            f.readout(
                "field.heatmap.least",
                &format!("{}   {}", row.ch, row.answers),
            );
        }

        f.custom(TAG_HEATMAP, Style::row().height_px(f.ui.m(260.0)));
        f.hint("hint.heatmap");
        f.hint("hint.heatmap_matrix");
    }
    f.end_group();
}

// -------------------------------------------------------- settings tab

/// Controls that configure the application rather than the training.
///
/// Kept off the composable tabs on purpose. Several of them decide how the panel
/// itself behaves, and one of them decides what the panel contains, so a
/// composition that omitted them would leave no way back.
fn application(
    f: &mut Frame<'_>,
    settings: &mut Settings,
    status: &StatusInfo<'_>,
    cmd: &mut UiCommands,
) {
    if f.begin_group("group.interface") {
        let mut language = status.language_index;
        if f.combo("field.ui.language", &mut language, status.languages) {
            if let Some(code) = status.languages.get(language) {
                cmd.language = Some((*code).to_string());
            }
        }

        f.slider("field.ui.scale", &mut settings.ui.scale, 0.5, 3.0, 2, "");
        f.slider("field.ui.font", &mut settings.ui.font_size_pt, 6.0, 32.0, 1, "unit.pt");
        f.slider("field.ui.prompt_font", &mut settings.ui.prompt_font_size_pt, 8.0, 96.0, 1, "unit.pt");
        // Coverage shaping is applied when a glyph is rasterized, and the cache
        // is keyed by size rather than by gamma, so a change reaches the glyphs
        // drawn next rather than the ones already in the atlas.
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
        f.text_edit("field.app.log_path", &mut settings.log.file_path);
        f.toggle("field.ui.debug_overlay", &mut settings.ui.show_debug_overlay);
    }
    f.end_group();
}

/// Chrome and density.
///
/// Nothing here changes what is sent. It changes how much of the drawing is
/// decoration and how the few marks that remain are used, which is a matter the
/// operator settles once and then stops thinking about.
fn appearance(f: &mut Frame<'_>, settings: &mut Settings, cmd: &mut UiCommands) {
    if f.begin_group("group.appearance") {
        let a = &mut settings.appearance;

        if f.toggle("field.look.custom_frame", &mut a.custom_frame) {
            cmd.frame_changed = true;
        }
        f.begin_disabled(!a.custom_frame);
        f.slider("field.look.caption_height", &mut a.caption_height, 18.0, 48.0, 0, "");
        f.end_disabled();

        f.toggle("field.look.focus_ring", &mut a.focus_ring);
        f.toggle("field.look.accent_hover", &mut a.accent_hover);
        f.toggle("field.look.group_tick", &mut a.group_tick);
        f.enum_combo("field.look.tab_style", &mut a.tab_style);

        f.toggle("field.look.animate", &mut a.animate);
        f.begin_disabled(!a.animate);
        f.slider("field.look.anim_ms", &mut a.anim_ms, 30.0, 500.0, 0, "unit.ms");
        f.enum_combo("field.look.anim_curve", &mut a.anim_curve);
        f.end_disabled();

        f.toggle("field.look.value_column", &mut a.value_column);
        f.toggle("field.look.numeric_entry", &mut a.numeric_entry);
        f.hint("hint.look_entry");
        f.toggle("field.look.group_activity", &mut a.group_activity);
        f.toggle("field.look.keyboard_focus", &mut a.keyboard_focus);
        f.slider("field.look.popup_shade", &mut a.popup_shade, 0.0, 0.6, 2, "");
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

/// Everything the application draws over the keying picture.
fn data_look(f: &mut Frame<'_>, settings: &mut Settings) {
    if f.begin_group("group.data_area") {
        let a = &mut settings.appearance;

        // The background is a colour and there is no colour control in this
        // interface, so it is stated in the configuration file. Saying so is
        // better than offering three sliders for one value nobody adjusts twice.
        f.readout("field.look.data_background", &format!("{:06X}", a.data_background_rgb));

        f.toggle("field.look.axis_gutters", &mut a.axis_gutters);
        f.begin_disabled(!a.axis_gutters);
        f.slider("field.look.gutter_left", &mut a.axis_gutter_left, 0.0, 90.0, 0, "");
        f.slider("field.look.gutter_bottom", &mut a.axis_gutter_bottom, 0.0, 40.0, 0, "");
        f.end_disabled();

        f.slider_u32("field.look.grid_major_every", &mut a.grid_major_every, 1, 20, "");
        f.slider("field.look.grid_minor_alpha", &mut a.grid_minor_alpha, 0.0, 1.0, 2, "");

        f.toggle("field.look.hud", &mut a.hud);
        f.begin_disabled(!a.hud);
        f.slider("field.look.hud_width", &mut a.hud_width, 0.2, 1.0, 2, "");
        f.slider("field.look.hud_height", &mut a.hud_height, 0.15, 0.9, 2, "");
        f.slider("field.look.hud_opacity", &mut a.hud_opacity, 0.3, 1.0, 2, "");
        f.end_disabled();
        f.hint("hint.hud");

        f.toggle("field.look.trace_fill", &mut a.trace_fill);
        f.begin_disabled(!a.trace_fill);
        f.slider("field.look.trace_fill_alpha", &mut a.trace_fill_alpha, 0.0, 0.8, 2, "");
        f.end_disabled();
        f.slider("field.look.trace_thickness", &mut a.trace_thickness, 1.0, 4.0, 1, "");
    }
    f.end_group();
}

/// Editor for the composition of the four composable tabs.
///
/// The tab is chosen with the same strip the toolbar uses, not with a list. A
/// list would name the tabs in one place while the strip names them in another,
/// and the operator would have to match the two before anything below made
/// sense.
///
/// Order matters as much as membership, so the list offers movement rather than
/// only presence. A section may be placed on several tabs; it carries the same
/// settings in each, because the settings belong to the training and the tab is
/// only a view onto them. The defaults do not do this, because a control in two
/// places invites the belief that they are two controls.
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
            // One scope per row: the rows are built from the same keys and would
            // otherwise share their identity, which is what makes state leak
            // from one row to the next.
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
        // holds everything, and the control says so by being dead rather than by
        // vanishing.
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
/// than about one section. A hundred settings mean a trainer can behave
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
            f.label_mono(line, color, TextAlign::Left, Style::row().grow(1.0).shrink(1.0));
        }
    }
    f.end_group();
}

// ------------------------------------------------------------------ chrome

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
    } else if status.session.running {
        f.ui.theme.accent
    } else if status.audio.running {
        f.ui.theme.text_dim
    } else {
        f.ui.theme.text_disabled
    };
    f.begin_frame(
        Style::row().width_px(led).height_px(led).align_self(Align::Center),
        led_color,
        Color::TRANSPARENT,
    );
    f.end();

    // The endpoint before the session, because a session that cannot start is
    // almost always a sound problem and the reason belongs where the eye goes
    // first.
    let state = if !status.audio.error.is_empty() {
        status.audio.error.clone()
    } else if status.session.running {
        format!(
            "{}   {:.0} s",
            f.ui.tr(status.session.phase),
            status.session.remaining
        )
    } else if status.audio.running {
        format!(
            "{}   {} {}",
            f.ui.tr("status.idle"),
            status.audio.device_rate,
            f.ui.tr("unit.hz")
        )
    } else {
        f.ui.tr("status.stopped").to_string()
    };
    f.label_dim(&state);

    f.separator_vertical();
    let dim = f.ui.theme.text_dim;
    let text = f.ui.theme.text;
    let text_color = text;

    // The two figures that describe what is being sent, and they are the two an
    // operator checks against a printed exercise.
    let speed = format!(
        "{:.0}/{:.0} {}",
        settings.timing.char_wpm,
        if settings.timing.farnsworth { settings.timing.text_wpm } else { settings.timing.char_wpm },
        f.ui.tr("unit.wpm")
    );
    f.label_mono(&speed, text, TextAlign::Left, Style::row());

    f.separator_vertical();
    let tone = format!("{:.0} {}", settings.tone.pitch_hz, f.ui.tr("unit.hz"));
    f.label_mono(&tone, dim, TextAlign::Left, Style::row());

    f.separator_vertical();
    let level = format!("{} {}", settings.lesson.level, f.ui.tr("unit.chars"));
    f.label_mono(&level, dim, TextAlign::Left, Style::row());

    // The drill decides both what is played and what answers it, so an operator
    // who forgot which one is running cannot work it out from anything else on
    // the bar. Absent while none is, which is the statement that the practice
    // mode means what it says.
    let drill = match settings.practice.drill {
        DrillMode::Off => "",
        DrillMode::Recall => "drill.recall",
        DrillMode::Echo => "drill.echo",
        DrillMode::Blind => "drill.blind",
    };
    if !drill.is_empty() {
        f.separator_vertical();
        let text = f.ui.tr(drill).to_string();
        f.label_mono(&text, f.ui.theme.accent, TextAlign::Left, Style::row());
    }

    // The one thing an operator has to be able to see with the panel folded: the
    // mouse buttons are contacts at the moment, and escape is what gives them
    // back. Stated only while that is true, so its absence is the statement that
    // the pointer is a pointer.
    if settings.practice.keying() && status.session.running {
        f.separator_vertical();
        let text = f.ui.tr("status.keying").to_string();
        f.label_mono(&text, f.ui.theme.accent, TextAlign::Left, Style::row());
    }

    // Stated only once something has been answered. A permanent nought would be
    // noise, and its absence is what says the session has not scored anything.
    if status.session.characters > 0 {
        f.separator_vertical();
        let text = format!(
            "{:.0} %  {:.0} ms",
            status.session.accuracy * 100.0,
            status.session.reaction_ms
        );
        f.label_mono(&text, text_color, TextAlign::Left, Style::row());
    }

    // Stated only while two cursors are placed. A permanent dash would be noise,
    // and its absence is what says nothing is being measured.
    if let Some(seconds) = status.measurement_s {
        f.separator_vertical();
        let ms = seconds * 1000.0;
        let units = if status.dot_ms > 0.0 { ms / status.dot_ms } else { 0.0 };
        let text = format!("{:.1} ms  {:.2}u", ms, units);
        f.label_mono(&text, f.ui.theme.accent, TextAlign::Left, Style::row());
    }

    f.spacer(1.0);

    // Fixed width fields. A count that grows by one digit would otherwise shift
    // everything to its left, and the right hand end of the bar is exactly where
    // an operator glances without reading.
    let sent = format!("{:>6} {}", status.session.characters, f.ui.tr("unit.chars"));
    f.label_mono(&sent, dim, TextAlign::Right, Style::row());
    f.separator_vertical();
    let fps_text = format!("{:>3.0} {}", status.fps, f.ui.tr("unit.fps"));
    f.label_mono(&fps_text, text, TextAlign::Right, Style::row());
    f.end();
}

/// Diagnostic block. Not part of the tree: the layout model has no absolute
/// positioning, and a sibling would take real space away from the data area.
fn overlay(f: &mut Frame<'_>, status: &StatusInfo<'_>) {
    let lines = vec![
        format!("fps {:.1}   worst {:.2} ms", status.fps, status.worst_ms),
        // Nought means the reading is absent rather than instantaneous, which is
        // why the two cases are worded differently.
        if status.gpu_ms > 0.0 {
            format!("gpu {:.2} ms   worst {:.2} ms", status.gpu_ms, status.gpu_worst_ms)
        } else {
            "gpu not measured".to_string()
        },
        format!(
            "draws {}   uploads {}   dpi {:.2}",
            status.draw_calls, status.uploads, status.dpi
        ),
        format!("glyphs {}   atlas {:.0} %", status.glyphs, status.atlas * 100.0),
        format!(
            "audio {}   {} ch   {} frames   {} underruns",
            if status.audio.format.is_empty() { "closed" } else { status.audio.format.as_str() },
            status.audio.channels,
            status.audio.frames,
            status.audio.underruns
        ),
        format!(
            "queued {} + {}   history {:.1} s   held {:.2} s",
            status.audio.pending,
            status.audio.qrm_pending,
            status.scope_history_s,
            status.scope_end_s
        ),
        format!(
            "session {}   {} groups   {} chars   {:.1} %   {:.0} ms",
            status.session.phase,
            status.session.groups,
            status.session.characters,
            status.session.accuracy * 100.0,
            status.session.reaction_ms
        ),
        format!("pool {}", status.character_pool),
        format!("font {}", status.font),
        format!("gpu {}", status.gpu),
    ];
    f.ui.set_overlay(lines);
}