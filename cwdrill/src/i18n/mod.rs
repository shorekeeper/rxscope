//! Localization catalogue.
//!
//! Interface strings are addressed by key rather than by their English text.
//! The key is also what the widget system hashes to derive widget identity, so
//! a change of language must not change it: were the displayed text used as the
//! identifier, switching language would reset focus, group folding and scroll
//! position across the whole panel.
//!
//! A key that has no entry resolves to itself. That single rule carries two
//! useful consequences. A run time string, such as a device name or a numeric
//! readout, can be passed through the same lookup without harm. And a key added
//! to the interface before it is added to the table renders as the key, which is
//! visible in testing rather than silently blank.
//!
//! Every key appears exactly once. A duplicate is not an error the compiler can
//! see: the later entry overwrites the earlier one, so the wording that ships is
//! whichever happens to be lower in the file.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

pub const DEFAULT_LANGUAGE: &str = "en";

const TABLE: &[(&str, &str)] = &[
    // Commands.
    ("action.start", "START"),
    ("action.stop", "STOP"),
    ("action.pause", "PAUSE"),
    ("action.submit", "ANSWER"),
    ("action.repeat", "AGAIN"),
    ("action.skip", "SKIP"),
    ("action.test_tone", "TEST TONE"),
    ("action.clear_marks", "CLEAR MARKS"),
    ("action.live", "LIVE"),
    ("action.rescan", "RESCAN"),
    ("action.reset", "RESET"),
    ("action.panel", "PANEL"),
    ("action.add", "ADD"),
    ("action.up", "^"),
    ("action.down", "v"),
    ("action.close", "X"),
    ("action.reset_progress", "CLEAR PROGRESS"),
    // Tabs and areas.
    ("panel.practice", "PRACTICE"),
    ("panel.lesson", "LESSON"),
    ("panel.sound", "SOUND"),
    ("panel.display", "DISPLAY"),
    ("panel.settings", "SETTINGS"),
    ("panel.prompt", "SESSION"),
    ("panel.scope", "KEYING"),
    // Groups.
    ("group.session", "SESSION"),
    ("group.input", "ANSWERING"),
    ("group.paddle", "KEY"),
    ("group.progress", "PROGRESS"),
    ("group.lesson", "LESSON PLAN"),
    ("group.material", "MATERIAL"),
    ("group.timing", "TIMING"),
    ("group.tone", "TONE"),
    ("group.conditions", "BAND CONDITIONS"),
    ("group.device", "OUTPUT"),
    ("group.scope", "KEYING PICTURE"),
    ("group.heatmap", "WEAK CHARACTERS"),
    ("group.interface", "INTERFACE"),
    ("group.application", "APPLICATION"),
    ("group.appearance", "APPEARANCE"),
    ("group.data_area", "DATA AREA"),
    ("group.layout", "PANEL LAYOUT"),
    ("group.deviations", "CHANGED SETTINGS"),
    // Tone.
    ("field.tone.pitch", "pitch"),
    ("field.tone.volume", "volume"),
    ("field.tone.shape", "edge shape"),
    ("field.tone.rise", "rise"),
    ("field.tone.fall", "fall"),
    ("field.tone.pan", "pan"),
    // Timing.
    ("field.timing.char_wpm", "character speed"),
    ("field.timing.text_wpm", "text speed"),
    ("field.timing.farnsworth", "stretch the gaps"),
    ("field.timing.weight", "dash weight"),
    ("field.timing.element_gap", "element gap"),
    ("field.timing.char_gap", "character gap"),
    ("field.timing.word_gap", "word gap"),
    ("field.timing.jitter", "jitter"),
    ("field.timing.swing", "swing"),
    ("field.timing.dot", "dot length"),
    ("field.timing.gaps", "character / word gap"),
    // Lesson.
    ("field.lesson.method", "method"),
    ("field.lesson.level", "characters"),
    ("field.lesson.set", "current set"),
    ("field.lesson.custom_set", "custom set"),
    ("field.lesson.session", "session length"),
    ("field.lesson.pool", "characters"),
    ("field.lesson.advance", "advance above"),
    ("field.lesson.regress", "withdraw below"),
    ("field.lesson.auto", "move the level itself"),
    ("field.lesson.weak_weight", "weight the weak"),
    // Material.
    ("field.material.source", "source"),
    ("field.material.min_group", "shortest group"),
    ("field.material.max_group", "longest group"),
    ("field.material.numbers", "numbers"),
    ("field.material.punctuation", "punctuation"),
    ("field.material.prosigns", "prosigns"),
    ("field.material.file", "text file"),
    ("field.material.repeat", "repeat"),
    // Practice.
    ("field.practice.drill", "drill"),
    ("field.practice.drill_unit", "one prompt is"),
    ("field.practice.drill_focus", "lean on the newest"),
    ("field.practice.mode", "mode"),
    ("field.practice.case", "case"),
    ("field.practice.reveal", "reveal the answer"),
    ("field.practice.reveal_delay", "reveal after"),
    ("field.practice.backspace", "allow backspace"),
    ("field.practice.timeout", "answer within"),
    ("field.practice.strict", "count every miss"),
    // Key.
    ("field.paddle.mode", "key"),
    ("field.paddle.source", "contacts"),
    ("field.paddle.swap", "left sends the dash"),
    ("field.paddle.sidetone", "hear yourself"),
    ("field.paddle.key_dit", "dot letter"),
    ("field.paddle.key_dah", "dash letter"),
    ("field.paddle.speed", "sending at"),
    ("field.paddle.dash", "dash"),
    ("field.paddle.latency", "delay to the tone"),
    ("field.paddle.malformed", "unreadable"),
    // Conditions.
    ("field.conditions.noise", "noise"),
    ("field.conditions.snr", "signal to noise"),
    ("field.conditions.qsb", "fading"),
    ("field.conditions.qsb_rate", "fading rate"),
    ("field.conditions.qsb_depth", "fading depth"),
    ("field.conditions.qrm", "second station"),
    ("field.conditions.qrm_offset", "offset"),
    ("field.conditions.qrm_level", "its level"),
    ("field.conditions.qrn", "impulse noise"),
    ("field.conditions.qrn_rate", "impulses"),
    ("field.conditions.drift", "drift"),
    // Device.
    ("field.audio.device", "device"),
    ("field.audio.buffer", "buffer"),
    ("field.audio.state", "state"),
    ("field.audio.format", "format"),
    ("field.audio.recoveries", "recovered"),
    ("field.audio.underruns", "gaps"),
    ("field.audio.queued", "queued"),
    ("field.audio.latency", "queued audio"),
    // Scope.
    ("field.scope.visible", "visible"),
    ("field.scope.seconds", "span"),
    ("field.scope.unit_grid", "unit grid"),
    ("field.scope.ideal", "mark the ideal edges"),
    ("field.scope.labels", "name each character"),
    ("field.scope.timing", "timing error"),
    ("field.scope.height", "height"),
    ("field.scope.history", "history"),
    ("field.scope.held", "held back"),
    ("field.scope.measure", "between the marks"),
    // Session and progress.
    ("field.session.state", "state"),
    ("field.session.remaining", "remaining"),
    ("field.session.sent", "sent"),
    ("field.session.pool", "characters"),
    ("field.session.queued", "queued"),
    ("field.session.accuracy", "accuracy"),
    ("field.session.reaction", "reaction"),
    ("field.session.inserted", "extra characters"),
    ("field.session.copy", "copy"),
    ("field.session.fields", "copy down"),
    ("field.session.verdict", "fields"),
    ("field.session.field_score", "fields right"),
    ("field.qso.filler", "wording"),
    ("field.qso.call", "call"),
    ("field.qso.rst", "report"),
    ("field.qso.serial", "serial"),
    ("field.qso.name", "name"),
    ("field.qso.qth", "location"),
    ("field.progress.level", "level"),
    ("field.progress.accuracy", "set accuracy"),
    ("field.progress.weakest", "weakest"),
    ("field.progress.sessions", "sessions"),
    ("field.progress.window", "measured over"),
    ("field.progress.path", "history file"),
    ("field.progress.log", "log sessions"),
    ("field.heatmap.tested", "characters judged"),
    ("field.heatmap.peak", "worst pair"),
    ("field.heatmap.slowest", "slowest"),
    ("field.heatmap.least", "least practised"),
    ("field.progress.log_file", "log file"),
    ("field.conditions.qrm_queued", "its queue"),
    // Interface and application.
    ("field.ui.language", "language"),
    ("field.ui.scale", "scale"),
    ("field.ui.font", "font"),
    ("field.ui.prompt_font", "prompt font"),
    ("field.ui.text_gamma", "text gamma"),
    ("field.ui.vsync", "vsync"),
    ("field.ui.target_fps", "frame limit"),
    ("field.ui.debug_overlay", "debug overlay"),
    ("field.app.present_mode", "present mode"),
    ("field.app.frames_in_flight", "frames in flight"),
    ("field.app.validation", "gpu validation"),
    ("field.app.gpu_timing", "gpu timing"),
    ("field.app.log_level", "log level"),
    ("field.app.log_path", "log file"),
    ("field.app.atlas", "glyph atlas"),
    ("field.layout.add", "add section"),
    ("field.deviations.count", "changed"),
    // Appearance.
    ("field.look.custom_frame", "own window frame"),
    ("field.look.caption_height", "caption height"),
    ("field.look.focus_ring", "focus outline"),
    ("field.look.accent_hover", "accent on hover"),
    ("field.look.group_tick", "group marker"),
    ("field.look.tab_style", "tab style"),
    ("field.look.animate", "animate"),
    ("field.look.anim_ms", "duration"),
    ("field.look.anim_curve", "curve"),
    ("field.look.hint_scale", "hint size"),
    ("field.look.value_column", "align values"),
    ("field.look.numeric_entry", "type values"),
    ("field.look.popup_shade", "shade under lists"),
    ("field.look.group_activity", "mark active groups"),
    ("field.look.keyboard_focus", "keyboard focus"),
    ("field.look.splitter_grip", "splitter grip"),
    ("field.look.separator_alpha", "separator"),
    ("field.look.panel_margin", "panel margin"),
    ("field.look.group_padding", "group padding"),
    ("field.look.row_height", "row height"),
    ("field.look.gap", "gap"),
    ("field.look.data_background", "background"),
    ("field.look.grid_minor_alpha", "minor grid"),
    ("field.look.grid_major_every", "major every"),
    ("field.look.axis_gutters", "axis gutters"),
    ("field.look.gutter_left", "left gutter"),
    ("field.look.gutter_bottom", "bottom gutter"),
    ("field.look.hud", "prompt over the picture"),
    ("field.look.hud_width", "its width"),
    ("field.look.hud_height", "its height"),
    ("field.look.hud_opacity", "its opacity"),
    ("field.look.trace_fill", "fill under trace"),
    ("field.look.trace_fill_alpha", "fill opacity"),
    ("field.look.trace_thickness", "trace width"),
    // Hints.
    ("hint.tone_shape", "raised cosine is what a transmitter produces; hard keying teaches the ear to key on the click instead of the tone"),
    ("hint.tone_pan", "one ear is measurably easier, so moving towards the centre is how the difficulty is raised"),
    ("hint.timing_two_speeds", "the elements keep the character speed and only the gaps stretch, so nothing has to be unlearned when they close"),
    ("hint.timing_jitter", "machine timing is a different signal from a human one; at nought you learn to copy a machine"),
    ("hint.timing_swing", "a systematic bias rather than a spread, which is what a mechanical bug produces"),
    ("hint.timing_weight", "three is the definition, a hand key sits between two and a half and three and a half"),
    ("hint.lesson_koch", "two characters at full speed, one added at a time, and the level falls as well as rises"),
    ("hint.lesson_regress", "a level that only rises turns a bad evening into a wall"),
    ("hint.material_groups", "random groups train recognizing a character with nothing to help; everything below trains what an operator actually copies, which is a small vocabulary heard as whole shapes"),
    ("hint.material_vocab", "sent whole and with its meaning shown after the answer. The whole alphabet is used, because a Q code built out of two characters is not one"),
    ("hint.material_builtin", "with no file a list of common words is used, so the source works before anything is written"),
    ("hint.fallback_no_word", "nothing in the list can be spelled with the current characters, so a group was sent instead"),
    ("hint.fallback_no_file", "the word file could not be read, so a group was sent instead"),
    ("hint.session_transport", "start, again, skip and answer sit under the text, where the eyes already are"),
    ("hint.session_live", "the phase, the clock and the score are drawn over the keying picture, because they are read during a group rather than between two"),
    ("hint.material_file", "one word per line, or plain text: everything outside the current set is skipped"),
    ("hint.conditions", "a clean tone in silence is a signal nobody receives"),
    ("hint.practice_reveal", "a reveal that arrives with the keystroke removes the moment the ear commits"),
    ("hint.practice_backspace", "off is deliberate: on the air a character cannot be taken back"),
    ("hint.scope_ideal", "the difference between what was sent and what should have been sent is the lesson"),
    ("hint.scope_gestures", "left and right place the two marks and snap to the nearest edge, shift places freely, middle drags the picture, the wheel changes the span, double click clears"),
    ("hint.scope_labels", "the character each burst was, above the trace: a shape examined afterwards is otherwise a shape nobody can attribute"),
    ("hint.scope_timing", "how far every element was from the length it should have had, above the line for long and below for short. The corner states the speed the marks were actually sent at"),
    ("hint.scope_measure", "place both marks to measure an element"),
    ("hint.session_keys", "space starts and stops, enter takes the answer, escape ends the session"),
    ("hint.session_needs_output", "nothing to play through until the output is open"),
    ("hint.session_inserted", "a character written that was never sent, which is an element heard as a character"),
    ("hint.progress_window", "up to sixty four, which is a dozen groups: the horizon the level decision needs"),
    ("hint.progress_untested", "not enough answers yet to judge the set"),
    ("hint.progress_running", "the history is written when the session ends"),
    ("hint.hud", "the prompt, the answer and the score sit over the keying picture, where the eyes are during a group. The frame widens to hold a word or an exchange before the text starts shrinking"),
    ("hint.heatmap", "a bar is a character, its height the accuracy and an outline means untested"),
    ("hint.heatmap_matrix", "the matrix is sent down the side against received across the top; the last column is characters that were never written"),
    ("hint.practice_send", "the group is shown rather than sounded; key it, and what you sent is decoded and scored the same way"),
    ("hint.drill", "one character or one word at a time, in place of groups"),
    ("hint.drill_recall", "the character is shown and never sounded: you produce the pattern, which is the direction sending needs and the one groups never train"),
    ("hint.drill_echo", "the character is sounded, then keyed back. The pattern stays on screen while you send, so the two can be compared element by element"),
    ("hint.drill_blind", "the character is sounded and withheld, and the answer is typed. Copying narrowed to one unit, so a single character can be worked on"),
    ("hint.drill_overrides", "the mode is decided by the drill while one is running"),
    ("hint.drill_focus", "how much the character the lesson introduced last outweighs the rest. Nought draws evenly, which spends a drill on characters already known"),
    ("hint.session_log_empty", "answers appear here once a group has been scored"),
    ("hint.paddle_iambic", "the keyer keeps time, so a squeeze alternates; iambic_b sends one more element when both contacts are released during one"),
    ("hint.paddle_straight", "the hand decides every length, which is the harder exercise: the picture marks where each element should have ended"),
    ("hint.paddle_letters", "one letter each. Punctuation is not offered because its key depends on the layout"),
    ("hint.paddle_needs_mode", "set the practice mode to send before the key does anything"),
    ("hint.paddle_capture", "while the session runs the mouse buttons are contacts wherever the pointer is not over a control, so the panel and the window keep working; escape ends it"),
    ("hint.paddle_levers", "filled while the contact is closed. Press a lever to send that element, which is how the modes are compared without a paddle wired to anything"),
    ("hint.paddle_latency", "the delay is the queued audio plus one frame of the interface; lower audio.buffer_ms to close it, and turn vsync off if it is still long"),
    ("hint.paddle_slow", "over twenty five milliseconds is audible as a lag between the hand and the tone: lower audio.buffer_ms in the output section"),
    ("hint.paddle_malformed", "a character the alphabet does not hold, which is elements run together or a gap left inside one"),
    ("hint.conditions_qrm", "a real station with its own material, a tenth faster so the two do not lock together"),
    ("hint.conditions_drift", "a triangle rather than a walk: past ninety hertz a transmitter would be retuned"),
    ("hint.conditions_pan", "the interference and the noise go to the other ear, so the pan control is what sets the separation"),
    ("hint.timing_gaps_derived", "the two gaps follow from the text speed while it is stretched; switch that off to state them"),
    ("hint.tone_test", "the note is queued as an element, so it is exactly what a dash sounds like"),
    ("hint.audio_buffer", "applied on the next open: the size is stated when the endpoint is initialized"),
    ("hint.audio_shared", "the endpoint hands back its own format, so the rate is reported rather than chosen"),
    ("hint.audio_recovered", "the endpoint came back on its own; a figure that keeps climbing is a cable"),
    ("hint.material_callsign", "a callsign uses the whole alphabet rather than the current set, because a callsign made of two letters is not one"),
    ("hint.material_qso", "an exchange is fields wrapped in wording, so it is scored field by field and the wording is not scored at all"),
    ("hint.material_qso_answer", "type the fields in the order they are sent, separated by spaces; a token sent twice is written once"),
    ("hint.session_fields", "one answer per field, in order, separated by spaces; keying a word gap is what makes the separator"),
    ("hint.paddle_hold", "the amber bar is the element still being held and the tick is where it stops being a dot: nothing is judged until the contact opens"),
    ("hint.session_no_reaction", "not timed while the material is repeated: an interval measured across several hearings is not a reaction"),
    ("hint.material_repeat", "the same material is sent again before anything is asked, which is what AGN gets you"),
    ("hint.progress_keep", "the log is rewritten to its tail once it grows past this"),
    ("hint.progress_keep_all", "nought keeps every line, which is what a log another tool is following needs"),
    ("hint.no_devices", "no output endpoint found"),
    ("hint.layout", "sections of the tab selected above, in the order they appear"),
    ("hint.layout_empty", "tab is empty, add a section below"),
    ("hint.restart", "takes effect on the next start"),
    ("hint.deviations", "everything that differs from the shipped values, window geometry and the training level aside"),
    ("hint.deviations_none", "the configuration is as the build ships it"),
    ("hint.slider_speeds", "on any slider: shift is fine, control is coarse, both together is finest"),
    ("hint.look_entry", "double click a number to type it; enter applies, escape cancels"),
    ("hint.gpu_timing", "measures the device side frame time and feeds it to the debug overlay"),
    // Units.
    ("unit.hz", "Hz"),
    ("unit.db", "dB"),
    ("unit.ms", "ms"),
    ("unit.s", "s"),
    ("unit.wpm", "wpm"),
    ("unit.units", "units"),
    ("unit.percent", "%"),
    ("unit.pt", "pt"),
    ("unit.fps", "fps"),
    ("unit.chars", "chars"),
    ("unit.per_min", "per min"),
    ("unit.hz_per_min", "Hz per min"),
    ("unit.lines", "lines"),
    // Status.
    ("status.idle", "idle"),
    ("status.running", "running"),
    ("status.sending", "sending"),
    ("status.answering", "answer"),
    ("status.reveal", "result"),
    ("status.stopped", "stopped"),
    ("status.no_material", "nothing being sent"),
    ("status.keying", "KEY"),
    ("status.default_output", "default output"),
    // Fixed choices. The suffix matches the text written to the config file.
    ("enum.none", "none"),
    ("enum.auto", "auto"),
    ("enum.hard", "hard"),
    ("enum.raised_cosine", "raised cosine"),
    ("enum.gaussian", "gaussian"),
    ("enum.koch", "one at a time"),
    ("enum.alphabet", "alphabetical"),
    ("enum.frequency", "by frequency of use"),
    ("enum.custom", "custom set"),
    ("enum.groups", "random groups"),
    ("enum.words", "words"),
    ("enum.callsigns", "callsigns"),
    ("enum.numbers", "numbers"),
    ("enum.qcodes", "Q codes"),
    ("enum.abbrev", "abbreviations"),
    ("enum.qso", "exchanges"),
    ("enum.file", "text file"),
    ("enum.listen", "listen only"),
    ("enum.copy", "copy"),
    ("enum.head_copy", "head copy"),
    ("enum.send", "send"),
    ("enum.recall", "recall and key it"),
    ("enum.echo", "hear it, then key it"),
    ("enum.blind", "hear it, then write it"),
    ("enum.character", "one character"),
    ("enum.word", "one word"),
    ("drill.recall", "RECALL"),
    ("drill.echo", "ECHO"),
    ("drill.blind", "BLIND"),
    ("enum.straight", "straight key"),
    ("enum.iambic_a", "iambic A"),
    ("enum.iambic_b", "iambic B"),
    ("enum.mouse", "mouse buttons"),
    ("enum.keyboard", "keyboard"),
    ("enum.both", "both"),
    ("enum.upper", "upper case"),
    ("enum.lower", "lower case"),
    ("enum.linear", "linear"),
    ("enum.ease_out", "ease out"),
    ("enum.ease_in_out", "ease in out"),
    ("enum.underline", "underline"),
    ("enum.attached", "attached"),
    ("enum.fifo", "FIFO"),
    ("enum.fifo_relaxed", "FIFO relaxed"),
    ("enum.mailbox", "mailbox"),
    ("enum.immediate", "immediate"),
    ("enum.trace", "trace"),
    ("enum.debug", "debug"),
    ("enum.info", "info"),
    ("enum.warn", "warning"),
    ("enum.error", "error"),
    ("enum.off", "off"),
    ("enum.session", "session"),
    ("enum.input", "answering"),
    ("enum.paddle", "key"),
    ("enum.progress", "progress"),
    ("enum.lesson", "lesson plan"),
    ("enum.material", "material"),
    ("enum.timing", "timing"),
    ("enum.tone", "tone"),
    ("enum.conditions", "band conditions"),
    ("enum.device", "output"),
    ("enum.scope", "keying picture"),
    ("enum.heatmap", "weak characters"),
];

#[derive(Clone)]
pub struct Catalog {
    language: String,
    map: HashMap<String, String>,
}

impl Catalog {
    pub fn builtin() -> Catalog {
        let mut map = HashMap::with_capacity(TABLE.len() * 2);
        for (key, text) in TABLE {
            map.insert((*key).to_string(), (*text).to_string());
        }
        Catalog { language: DEFAULT_LANGUAGE.to_string(), map }
    }

    /// Loads a translation on top of the reference catalogue.
    ///
    /// A missing file is not an error: the reference wording is complete on its
    /// own, so an untranslated language degrades to English rather than to blank
    /// labels. A key the build does not know is kept and never looked up, which
    /// makes a translation file forward compatible with an older build.
    pub fn load(directory: &Path, language: &str) -> Catalog {
        let mut catalog = Catalog::builtin();
        if language.is_empty() || language.eq_ignore_ascii_case(DEFAULT_LANGUAGE) {
            return catalog;
        }
        catalog.language = language.to_string();

        let path = directory.join(format!("{}.lang", language));
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                crate::log_warn!("i18n", "{}: {}, using the reference wording", path.display(), e);
                return catalog;
            }
        };

        let mut loaded = 0usize;
        for (number, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            match line.find('=') {
                Some(at) => {
                    let key = line[..at].trim();
                    let value = line[at + 1..].trim();
                    if key.is_empty() || value.is_empty() {
                        continue;
                    }
                    catalog.map.insert(key.to_string(), value.to_string());
                    loaded += 1;
                }
                None => crate::log_warn!("i18n", "{} line {}: missing '='", path.display(), number + 1),
            }
        }

        crate::log_info!("i18n", "{}: {} entries for '{}'", path.display(), loaded, language);
        catalog
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        match self.map.get(key) {
            Some(text) => text.as_str(),
            None => key,
        }
    }

    /// Writes the reference wording as a translation template.
    pub fn write_template(directory: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join("template.lang");

        let mut out = String::with_capacity(TABLE.len() * 48);
        out.push_str("; CWDrill translation template\n");
        out.push_str("; Copy to <code>.lang and set ui.language to that code.\n");
        out.push_str("; Untranslated lines may be deleted, they fall back to English.\n");
        out.push_str("; A longer translation is not cut: the side panel widens to hold it.\n\n");

        let mut previous = "";
        for (key, text) in TABLE {
            let section = key.split('.').next().unwrap_or("");
            if section != previous {
                out.push('\n');
                previous = section;
            }
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(text);
            out.push('\n');
        }
        std::fs::write(&path, out)?;
        crate::log_info!("i18n", "template written to {}", path.display());
        Ok(())
    }
}

impl Default for Catalog {
    fn default() -> Catalog {
        Catalog::builtin()
    }
}

static ACTIVE: Mutex<Option<Catalog>> = Mutex::new(None);

pub fn install(catalog: Catalog) {
    let mut slot = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(catalog);
}

/// Copy of the installed catalogue. Called once per interface instance.
pub fn current() -> Catalog {
    let slot = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    slot.clone().unwrap_or_else(Catalog::builtin)
}