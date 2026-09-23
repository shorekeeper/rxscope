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
//! One catalogue is installed globally at startup and cloned into each interface
//! instance. The clone is a few hundred short strings, paid once.
//!
//! Every key appears exactly once. A duplicate is not an error the compiler can
//! see: the later entry simply overwrites the earlier one in the map, so the
//! wording that ships is whichever happens to be lower in the file, and the
//! template carries the same key twice for the translator to reconcile.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Language code used when none is configured.
pub const DEFAULT_LANGUAGE: &str = "en";

/// Built in table. The right hand side is the reference wording; a translation
/// file overrides individual entries and leaves the rest at these values.
///
/// Naming follows the path of the string through the interface:
///   action.*    operator commands, normally on a button
///   group.*     collapsible section titles
///   panel.*     titles of the large areas
///   field.*     labels of settings rows
///   status.*    the bottom bar and the diagnostic block
///   hint.*      explanatory lines shown inside a group
///   enum.*      values of a fixed choice, matching the configuration text
///   unit.*      measurement suffixes
///   gate.*      reasons the keying decision is held shut
///   mode.*      recognized transmission modes
const TABLE: &[(&str, &str)] = &[
    // Commands.
    ("action.start", "START"),
    ("action.stop", "STOP"),
    ("action.clear", "CLEAR"),
    ("action.copy", "COPY"),
    ("action.reload", "RELOAD"),
    ("action.mark", "MARK"),
    ("action.store_view", "STORE"),
    ("action.recall_view", "RECALL"),
    ("action.clear_reference", "CLEAR MARK"),
    ("action.newest", "NEWEST"),
    ("action.panel", "PANEL"),
    ("action.rescan", "RESCAN"),
    ("action.close", "X"),
    ("action.reset", "RESET"),
    ("action.apply", "APPLY"),
    ("action.up", "^"),
    ("action.down", "v"),
    ("action.add", "ADD"),
    ("action.preset", "PRESET"),
    ("action.reset_zoom", "FULL SPAN"),
    ("action.record", "RECORD"),
    ("action.open", "OPEN"),
    ("action.close_replay", "CLOSE"),
    ("action.play", "PLAY"),
    ("action.pause", "PAUSE"),
    ("action.back", "<"),
    ("action.forward", ">"),
    ("action.rewind", "<<"),
    ("action.live", "LIVE"),
    ("action.export", "EXPORT"),
    // Toolbar and large areas.
    ("panel.receive", "RECEIVE"),
    ("panel.waterfall", "WATERFALL"),
    ("panel.decode", "DECODE"),
    ("panel.settings", "SETTINGS"),
    ("panel.all", "ALL"),
    ("panel.band", "BAND"),
    ("panel.audio", "AUDIO"),
    ("panel.display", "DISPLAY"),
    ("panel.replay", "REPLAY"),
    // Group titles.
    ("group.meter", "S METER"),
    ("group.audio", "AUDIO INPUT"),
    ("group.spectrum", "SPECTRUM"),
    ("group.waterfall", "WATERFALL"),
    ("group.cw_channels", "CW CHANNELS"),
    ("group.cw_decoder", "CW DECODER"),
    ("group.rtty_decoder", "RTTY DECODER"),
    ("group.psk_decoder", "PSK31 DECODER"),
    ("group.classifier", "CLASSIFIER"),
    ("group.callsign", "CALL SIGN"),
    ("group.spots", "STATIONS HEARD"),
    ("group.interface", "INTERFACE"),
    ("group.application", "APPLICATION"),
    ("group.layout", "PANEL LAYOUT"),
    ("group.deviations", "CHANGED SETTINGS"),
    ("group.monitor", "MONITOR"),
    ("group.rig", "TRANSCEIVER"),
    ("group.receiver", "RECEIVER"),
    ("group.appearance", "APPEARANCE"),
    ("group.data_area", "DATA AREA"),
    ("group.record", "RECORDING"),
    ("group.replay", "PLAYBACK"),
    ("group.segments", "SEGMENTS"),
    // Transceiver.
    ("field.rig.enabled", "enabled"),
    ("field.rig.profile", "description"),
    ("field.rig.profiles_path", "directory"),
    ("field.rig.port", "port"),
    ("field.rig.baud", "baud"),
    ("field.rig.transport", "transport"),
    ("field.rig.dtr", "dtr"),
    ("field.rig.rts", "rts"),
    ("field.rig.sideband", "sideband"),
    ("field.rig.cw_pitch", "cw pitch"),
    ("field.rig.offset", "offset"),
    ("field.rig.show_readout", "readout"),
    ("field.rig.readout_scale", "readout size"),
    ("field.rig.readout_fine", "one hertz"),
    ("field.rig.readout_repeat", "repeat while held"),
    ("field.rig.rf_axis", "rf axis"),
    ("field.rig.tune_step", "tune step"),
    ("field.rig.entry", "go to"),
    ("field.rig.mark_label", "mark as"),
    ("field.rig.click_tunes", "click tunes"),
    ("field.rig.link", "link"),
    ("field.rig.exchanges", "exchanges"),
    ("field.rig.lines", "lines"),
    ("field.rig.recoveries", "recovered"),
    ("field.rig.frequency", "frequency"),
    ("field.rig.mode", "mode"),
    // Receiver chain.
    ("field.receiver.mode", "mode"),
    ("field.receiver.detector", "detector"),
    ("field.receiver.mode_link", "mode link"),
    ("field.receiver.tune_enabled", "independent tuning"),
    ("field.receiver.tune", "tuning"),
    ("field.receiver.reference", "reference"),
    ("field.receiver.bfo", "beat oscillator"),
    ("field.receiver.filter_low", "filter lo"),
    ("field.receiver.filter_high", "filter hi"),
    ("field.receiver.width", "width"),
    ("field.receiver.listening", "passband centre"),
    ("field.receiver.state", "chain"),
    ("field.receiver.blanked", "blanked"),
    ("field.receiver.carrier", "carrier"),
    ("field.receiver.iq_input", "i/q input"),
    ("field.receiver.iq_swap", "swap channels"),
    ("field.receiver.iq_gain", "i/q gain"),
    ("field.receiver.iq_phase", "i/q phase"),
    ("field.receiver.nb_wide", "wide blanker"),
    ("field.receiver.nb_wide_threshold", "wide threshold"),
    ("field.receiver.nb_narrow", "narrow blanker"),
    ("field.receiver.nb_narrow_threshold", "narrow threshold"),
    ("field.receiver.nr", "noise reduction"),
    ("field.receiver.nr_strength", "strength"),
    ("field.receiver.nr_method", "nr method"),
    ("field.receiver.notch", "notch"),
    ("field.receiver.notch_hz", "notch centre"),
    ("field.receiver.notch_width", "notch width"),
    ("field.receiver.auto_notch", "point it itself"),
    ("field.receiver.agc", "gain control"),
    ("field.receiver.agc_attack", "attack"),
    ("field.receiver.agc_hang", "hang"),
    ("field.receiver.agc_release", "release"),
    ("field.receiver.agc_target", "target"),
    ("field.receiver.squelch", "squelch"),
    ("field.receiver.squelch_db", "threshold"),
    // Meter.
    ("field.meter.reading", "reading"),
    ("field.meter.raw", "input level"),
    ("field.meter.scale", "scale"),
    ("field.meter.s9_reference", "s9 ref"),
    ("field.meter.calibration", "calibration"),
    ("field.meter.attack", "attack"),
    ("field.meter.release", "release"),
    ("field.meter.peak_hold", "peak hold"),
    ("field.meter.narrow", "measure the band only"),
    ("field.meter.peak_hold_time", "hold time"),
    // Audio input.
    ("field.audio.backend", "backend"),
    ("field.audio.device", "device"),
    ("field.audio.channel", "channel"),
    ("field.audio.gain", "gain"),
    ("field.audio.period", "period"),
    ("field.audio.ring", "ring"),
    ("field.audio.requested_rate", "device rate"),
    ("field.audio.dsp_rate", "dsp rate"),
    ("field.audio.dc_block", "dc block"),
    ("field.audio.exclusive", "exclusive mode"),
    ("field.audio.channels_in", "channels in"),
    ("field.audio.queue", "queue"),
    ("field.audio.effective_rate", "settled at"),
    ("field.audio.recoveries", "recovered"),
    ("field.audio.frames", "samples"),
    // Spectrum.
    ("field.spectrum.window", "window"),
    ("field.spectrum.fft_size", "fft size"),
    ("field.spectrum.zoom_resolution", "finer when zoomed"),
    ("field.spectrum.resolution", "resolution"),
    ("field.spectrum.average", "average"),
    ("field.spectrum.overlap", "overlap"),
    ("field.spectrum.passband_low", "passband lo"),
    ("field.spectrum.passband_high", "passband hi"),
    ("field.spectrum.search_span", "search span"),
    ("field.spectrum.blanker", "impulse blanker"),
    ("field.spectrum.blanker_threshold", "blanker threshold"),
    ("field.spectrum.blanked", "blanked"),
    ("field.spectrum.decimation", "reduce rate by"),
    ("field.spectrum.workers", "worker threads"),
    // Waterfall.
    ("field.waterfall.palette", "palette"),
    ("field.waterfall.style", "style"),
    ("field.waterfall.floor", "floor"),
    ("field.waterfall.ceiling", "ceiling"),
    ("field.waterfall.gamma", "gamma"),
    ("field.waterfall.speed", "speed"),
    ("field.waterfall.actual_speed", "actual"),
    ("field.waterfall.trace", "trace"),
    ("field.waterfall.trace_visible", "trace visible"),
    ("field.waterfall.grid", "grid"),
    ("field.waterfall.labels", "labels"),
    ("field.waterfall.level_grid", "level grid"),
    ("field.waterfall.peak_hold", "peak hold"),
    ("field.waterfall.held", "tracker trace"),
    ("field.waterfall.markers", "decoder markers"),
    ("field.waterfall.auto_range", "auto range"),
    ("field.waterfall.smoothing", "smoothing"),
    ("field.waterfall.cursor", "cursor readout"),
    ("field.waterfall.zoom", "zoom"),
    ("field.waterfall.span", "visible span"),
    ("field.waterfall.anchor", "follow the dial"),
    ("field.waterfall.columns", "history columns"),
    ("field.waterfall.smooth", "smooth"),
    ("field.waterfall.stations", "station markers"),
    ("field.waterfall.average", "long average"),
    ("field.waterfall.time_axis", "time axis"),
    ("field.waterfall.stored", "saved view"),
    ("field.waterfall.gpu_palette", "gpu palette"),
    // Keying channels.
    ("field.cw.multi_channel", "multi channel"),
    ("field.cw.max_channels", "max channels"),
    ("field.cw.spacing", "spacing"),
    ("field.cw.detected", "detected"),
    ("field.cw.snr", "snr"),
    ("field.cw.gate", "gate"),
    ("field.cw.centre", "centre"),
    ("field.cw.enabled", "enabled"),
    ("field.cw.auto_tone", "auto tone"),
    ("field.cw.afc", "afc"),
    ("field.cw.tone", "tone"),
    ("field.cw.capture", "capture"),
    ("field.cw.bandwidth", "detector width"),
    ("field.cw.effective", "effective"),
    ("field.cw.auto_speed", "auto speed"),
    ("field.cw.speed", "speed"),
    ("field.cw.tracking", "tracking"),
    ("field.cw.squelch", "squelch"),
    ("field.cw.min_snr", "min snr"),
    ("field.cw.print_above", "print above"),
    ("field.cw.quality", "quality"),
    ("field.cw.confidence", "confidence"),
    ("field.cw.case", "case"),
    ("field.cw.prosigns", "prosigns"),
    ("field.cw.farnsworth", "farnsworth spacing"),
    ("field.cw.char_gap", "character gap"),
    // Teleprinter.
    ("field.rtty.detected", "detected"),
    ("field.rtty.lock", "lock"),
    ("field.rtty.level", "level"),
    ("field.rtty.shift_state", "shift state"),
    ("field.rtty.enabled", "enabled"),
    ("field.rtty.alphabet", "alphabet"),
    ("field.rtty.baud", "baud"),
    ("field.rtty.shift", "shift"),
    ("field.rtty.mark", "mark"),
    ("field.rtty.data_bits", "data bits"),
    ("field.rtty.stop_bits", "stop bits"),
    ("field.rtty.parity", "parity"),
    ("field.rtty.atc", "atc"),
    ("field.rtty.squelch", "squelch"),
    ("field.rtty.usos", "unshift on space"),
    ("field.rtty.invert", "invert"),
    ("field.rtty.auto_shift", "auto shift"),
    ("field.rtty.afc", "afc"),
    ("field.rtty.afc_range", "afc range"),
    ("field.rtty.auto_invert", "find the polarity"),
    ("field.rtty.polarity", "polarity"),
    ("field.psk.enabled", "enabled"),
    ("field.psk.centre", "centre"),
    ("field.psk.centre_hz", "centre"),
    ("field.psk.auto_centre", "follow the carrier"),
    ("field.psk.afc", "afc"),
    ("field.psk.afc_range", "afc range"),
    ("field.psk.lock", "framing"),
    ("field.psk.level", "level"),
    ("field.psk.characters", "characters"),
    ("field.psk.squelch", "squelch"),
    ("field.psk.print_above", "print above"),
    // Classifier.
    ("field.classifier.mode", "mode"),
    ("field.classifier.floor", "noise floor"),
    ("field.classifier.enabled", "recognize the mode"),
    ("field.classifier.window", "window"),
    ("field.classifier.interval", "interval"),
    ("field.classifier.confidence", "confidence"),
    ("field.classifier.hold", "hold"),
    ("field.classifier.detect_cw", "detect cw"),
    ("field.classifier.detect_rtty", "detect rtty"),
    ("field.classifier.detect_navtex", "detect navtex"),
    ("field.classifier.detect_psk31", "detect psk31"),
    ("field.classifier.auto_switch", "auto switch"),
    ("field.classifier.announce", "announce"),
    // Call sign.
    ("field.callsign.lookup", "lookup"),
    ("field.callsign.source", "source"),
    ("field.callsign.prefix_db", "prefix db"),
    ("field.callsign.local_db", "local db"),
    ("field.callsign.highlight", "highlight"),
    ("field.callsign.loaded", "database"),
    ("field.callsign.auto_lookup", "resolve country"),
    ("field.callsign.min_length", "shortest call"),
    ("field.callsign.cache", "resolution cache"),
    ("field.callsign.history", "history file"),
    ("field.callsign.history_limit", "history limit"),
    ("field.spots.count", "heard"),
    // Interface and application.
    ("field.ui.decoded", "decoded"),
    ("field.ui.scale", "scale"),
    ("field.ui.font", "font"),
    ("field.ui.decode_font", "decode font"),
    ("field.ui.text_gamma", "text gamma"),
    ("field.ui.vsync", "vsync"),
    ("field.ui.target_fps", "frame limit"),
    ("field.ui.transcript", "transcript"),
    ("field.ui.debug_overlay", "debug overlay"),
    ("field.ui.show_meter", "meter strip"),
    ("field.ui.show_decode_log", "decode panel"),
    ("field.decode.filter", "filter"),
    ("field.ui.language", "language"),
    ("field.app.present_mode", "present mode"),
    ("field.app.log_level", "log level"),
    ("field.app.validation", "gpu validation"),
    ("field.app.gpu_timing", "gpu timing"),
    ("field.app.frames_in_flight", "frames in flight"),
    ("field.app.atlas", "glyph atlas"),
    ("field.app.transcript_path", "transcript file"),
    ("field.app.max_lines", "decode history"),
    ("field.layout.tab", "tab"),
    ("field.layout.add", "add section"),
    ("field.band.segment", "segment"),
    // Monitor.
    ("field.monitor.enabled", "listen"),
    ("field.monitor.device", "output"),
    ("field.monitor.volume", "volume"),
    ("field.monitor.filter", "filter"),
    ("field.monitor.width_mode", "width"),
    ("field.monitor.bandwidth", "listen width"),
    ("field.monitor.follow", "follow channel"),
    ("field.monitor.centre", "centre"),
    ("field.monitor.pitch", "pitch"),
    ("field.monitor.agc", "gain control"),
    ("field.monitor.agc_attack", "attack"),
    ("field.monitor.agc_release", "release"),
    ("field.monitor.agc_target", "target"),
    ("field.monitor.band", "listening"),
    ("field.monitor.state", "device"),
    ("field.monitor.buffer", "buffer"),
    // Appearance.
    ("field.look.custom_frame", "own window frame"),
    ("field.look.caption_height", "caption height"),
    ("field.look.focus_ring", "focus outline"),
    ("field.look.accent_hover", "accent on hover"),
    ("field.look.group_tick", "group marker"),
    ("field.look.tab_style", "tab style"),
    ("field.deviations.count", "changed"),
    ("field.look.keyboard_focus", "keyboard focus"),
    ("field.look.numeric_entry", "type values"),
    ("field.look.popup_shade", "shade under lists"),
    ("field.look.group_activity", "mark active groups"),
    ("field.look.animate", "animate"),
    ("field.look.anim_ms", "duration"),
    ("field.look.anim_curve", "curve"),
    ("field.look.hint_scale", "hint size"),
    ("field.look.value_column", "align values"),
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
    ("field.look.crosshair", "crosshair"),
    ("field.look.trace_fill", "fill under trace"),
    ("field.look.trace_fill_alpha", "fill opacity"),
    ("field.look.trace_thickness", "trace width"),
    ("field.look.meter_segmented", "segmented meter"),
    ("field.look.meter_segment", "segment"),
    ("field.look.meter_segment_gap", "segment gap"),
    ("field.look.meter_scale_labels", "scale labels"),
    // Record and replay.
    ("field.record.enabled", "enabled"),
    ("field.record.state", "state"),
    ("field.record.segment", "segment"),
    ("field.record.written", "written"),
    ("field.record.total", "on disk"),
    ("field.record.gaps", "gaps"),
    ("field.record.path", "directory"),
    ("field.record.budget", "budget"),
    ("field.record.format", "samples"),
    ("field.record.block", "block"),
    ("field.record.auto_start", "start with audio"),
    ("field.replay.position", "position"),
    ("field.replay.speed", "speed"),
    ("field.replay.follow", "follow the edge"),
    ("field.replay.loop", "loop"),
    ("field.replay.faults", "faults"),
    ("field.export.format", "format"),
    ("field.export.float", "float samples"),
    ("field.export.path", "directory"),
    // Hints.
    ("hint.cw_channels", "left press on a marker selects that channel and holds it, on the selected one releases it; with one selected a press elsewhere moves it, with none it opens a channel there"),
    ("hint.cw_drag", "dragging carries whatever the press grabbed, so a channel follows the pointer across the band"),
    ("hint.cw_gate", "gate names the setting that is blocking"),
    ("hint.cw_bandwidth", "right drag sets the width of the selected channel"),
    ("hint.cw_width", "the width belongs to the selected channel; this also becomes the width of the next channel opened"),
    ("hint.cw_squelch", "an absolute level, and a backstop rather than the control to use: it moves with the detector width and with the band noise, so a threshold set against it stops meaning the same thing"),
    ("hint.cw_min_snr", "the level above the measured noise floor, shown beside the reading above; this is the gate that keeps its meaning as the band and the width change"),
    ("hint.cw_quality", "the share of patterns the alphabet could not have held by chance; every pattern up to three elements is a letter, so a plain match rate reads a hundred per cent on noise and this reads nought"),
    ("hint.meter_calibrate", "set the reference from the input level above while a signal you call S9 is being received; a narrow measurement over a wide capture span reads far below the wide one, so a reference taken from one does not serve the other"),
    ("hint.passband", "channels are only found inside the passband; on a quadrature input the span below is the bound instead"),
    ("hint.search_span", "either side of the tuning point, nought meaning everything the converter captured; the passband above states three kilohertz, which on a span of two hundred hides every station but the few beside the dial"),
    ("hint.blanker", "ahead of the transform, so it serves the display and the decoders; the two under the receiver serve the ear only"),
    ("hint.decimation", "divides the rate above again; the audio section states what the path settled on"),
    ("hint.zoom_resolution", "widens the transform with the magnification, up to four times, and up to a window of a second and a half; the line rate is planned against the stated size, so it does not move"),
    ("hint.overlap", "the largest step between two lines, as a share of the window, so it is a floor under the speed rather than a ceiling: raise it and the waterfall cannot run slower than the rate the floor implies"),
    ("hint.speed_from_overlap", "the overlap is what sets the speed here, not the control above; lower it or lower the dsp rate to reach the requested rate"),
    ("hint.audio_shared_rate", "shared mode hands back the mix format, so the device chooses the rate"),
    ("hint.dsp_rate", "a quadrature pair is two sided, so this is the whole width the display can show"),
    ("hint.audio_iq_mono", "the device delivers one channel, so the pair is two copies of it and the display stays one sided"),
    ("hint.workers", "the path runs on one thread: every stage is orders below the frame budget, and a worker would add a queue for nothing"),
    ("hint.meter_narrow", "the receiver filter in the receiver mode, the keying detector otherwise; the calibration stays valid either way"),
    ("hint.cw_tracking", "nought holds the speed where it is, one follows every element"),
    ("hint.cw_farnsworth", "characters at speed, gaps stretched; without this every letter becomes a word"),
    ("hint.rtty_auto_invert", "reverses the polarity on suspicion and undoes it if the framing did not improve"),
    ("hint.psk_rate", "the symbol rate is stated by the format, so there is nothing to set"),
    ("hint.psk_lock", "framing is the share of codes that resolved, phase is how tightly it clusters at nought and pi"),
    ("hint.psk_afc", "read from the step between two symbols, which cannot exceed a quarter of the symbol rate"),
    ("hint.psk_print", "the framing accepts any run of bits, so noise assembles codes and a few resolve"),
    ("hint.audio_recovered", "the device came back on its own; a figure that keeps climbing is a cable"),
    ("hint.rig_recovered", "the link came back on its own; a figure that keeps climbing is a cable"),
    ("hint.no_carrier", "no carrier"),
    ("hint.callsign_offline", "resolved from a local file: an online service fails exactly when it is needed"),
    ("hint.callsign_no_db", "no cty.dat at that path, calls are listed without a country"),
    ("hint.callsign_auto", "off lists the call alone, which costs nothing per sighting"),
    ("hint.callsign_min", "three admits the rare short calls and lets more noise through"),
    ("hint.callsign_history", "a first sighting is appended, a repetition is not"),
    ("hint.spots_empty", "nothing recognized yet"),
    ("hint.spots_click", "press a frequency to bring the receiver to it; a dim row rests on one sighting"),
    ("hint.decode_scroll", "the wheel walks back, control multiplies the step, a click tunes to the line"),
    ("hint.layout", "sections of the tab selected above, in the order they appear"),
    ("hint.layout_empty", "tab is empty, add a section below"),
    ("hint.restart", "takes effect on the next start"),
    ("hint.default_device", "follows the system default"),
    ("hint.slider_speeds", "on any slider: shift is fine, control is coarse, both together is finest"),
    ("hint.monitor_feedback", "output matches the loopback source"),
    ("hint.monitor_band", "the bar above the trace is what reaches the ear"),
    ("hint.monitor_pitch", "the chosen band is brought down to this pitch, so a channel sounds the same wherever it sits in the span"),
    ("hint.monitor_is_receiver", "the receiver chain is the listening path"),
    ("hint.monitor_needed_in_sdr", "the chain runs here, so nothing is heard or filtered until listening is on"),
    ("hint.rig", "the dial frequency turns the audio spectrum into a band"),
    ("hint.rig_profile", "a file in the directory below, without the extension"),
    ("hint.rig_shared", "shared lets another application hold the same port"),
    ("hint.rig_lines", "low unless the cable keys the transmitter from one"),
    ("hint.rig_pitch", "used only when the transceiver cannot report its own"),
    ("hint.rig_cw_reverse", "CW puts the band below the dial into the captured span and CW-R puts the band above it, so the reversed mode leaves almost nothing below the dial in view"),
    ("hint.rig_unusable", "the description carries an error and cannot be used"),
    ("hint.rig_no_profiles", "no descriptions in the directory"),
    ("hint.rig_no_ports", "no serial ports present"),
    ("hint.rig_readout", "the wheel steps the digit under the pointer, right click steps up, left click clears everything below it; the receiver moves when it can, and control asks for the dial instead"),
    ("hint.rig_click", "control inverts the gesture, so both targets stay reachable"),
    ("hint.rig_entry", "14025 or 14.025 or 14025000, or a band such as 40m; enter applies it"),
    ("hint.rig_bands", "a band returns to where you last were on it, in the mode you were using"),
    ("hint.rig_mark", "adds the current frequency to stations.ini, which is otherwise edited by hand"),
    ("hint.receiver_auto_notch", "moves the notch onto the strongest steady tone; a keyed carrier is left alone, but the two are only a few decibels apart"),
    ("hint.receiver_notch_two_sided", "the notch runs after the detector, so it removes a tone on each side of the tuning point"),
    ("hint.average_trace", "finds a carrier below the noise of one frame and loses anything that changes"),
    ("hint.time_axis", "the age of the history, down the left edge"),
    ("hint.time_axis_needs_gutter", "needs the axis gutters, which is where the labels go"),
    ("hint.stored_view", "one bookmark beside the live view, which is already kept across a restart"),
    ("hint.reference", "shift and left click on the display places the mark; the status line then states the difference"),
    ("hint.rig_offset", "the control detents at nought; shift and control together are the fine speed"),
    ("hint.rig_no_sideband", "a complex signal carries the sign, so there is no sideband to choose"),
    ("hint.receiver_mode", "skimmer leaves the signal alone, sdr filters and demodulates one"),
    ("hint.receiver_mode_link", "follow takes the mode from the rig, drive sends it"),
    ("hint.receiver_needs_iq", "am and fm need a carrier, which audio from a rig has not"),
    ("hint.receiver_needs_monitor", "the chain runs in the monitor, switch listening on"),
    ("hint.receiver_needs_stereo", "a mono input carries no quadrature channel"),
    ("hint.receiver_real_input", "one channel carries no sideband to select"),
    ("hint.receiver_lower_negative", "the lower sideband lives below nought here, so both edges are negative"),
    ("hint.receiver_tune", "what is being received; the filter edges below are measured from it, and the readout digits step it without moving the dial"),
    ("hint.receiver_no_tune", "demodulated audio has no tuning point: the transceiver already tuned"),
    ("hint.receiver_tune_locked", "locked: the receiver sits on the dial and a click moves the transceiver"),
    ("hint.receiver_filter", "edges are set separately; absolute on audio, measured from the tuning point on i/q"),
    ("hint.receiver_filter_move", "shift and right drag moves both edges together"),
    ("hint.receiver_iq", "image rejection follows how well the paths match"),
    ("hint.receiver_iq_skimmer", "the display and the detectors are both two sided here, so a channel below the tuning point is a channel of its own rather than the mirror of one above it"),
    ("hint.receiver_blankers", "two blankers on two time scales"),
    ("hint.receiver_nr_method", "predictor suits keying, spectral suits voice"),
    ("hint.receiver_hang", "hang holds the gain through a pause"),
    ("hint.decoder_off_in_sdr", "not fed in the receiver mode"),
    ("hint.classifier_scope", "off leaves the carrier search running, so channels are still opened and tracked; only the decision about which mode the band is carrying stops"),
    ("hint.band_needs_rig", "a dial frequency is needed to place these"),
    ("hint.band_no_stations", "nothing listed inside the visible span"),
    ("hint.stations_click", "press a frequency to bring it to the receiver"),
    ("hint.zoom", "wheel over the display magnifies about the pointer, middle drag pans"),
    ("hint.span_real", "audio from a transceiver fills the low end of the span only; magnify and the view centres on what is being received"),
    ("hint.anchor", "audio holds the receiver on screen, band holds the panorama and lets the receiver travel"),
    ("hint.columns", "nought takes the transform width, below which the history is coarser than the trace"),
    ("hint.smooth", "interpolates between columns, which widens a narrow carrier"),
    ("hint.anchor_needs_rf_axis", "needs rf axis: a display labelled in audio is already correct in audio"),
    ("hint.gpu_palette", "one channel, mapped in the shader: a palette or gamma change repaints the whole history"),
    ("hint.look_frame", "the system frame is drawn in the system palette and cannot match"),
    ("hint.look_accent", "off keeps the accent for data and focus, which is what makes it mean anything"),
    ("hint.deviations", "everything that differs from the shipped values, window geometry and device choices aside"),
    ("hint.deviations_none", "the configuration is as the build ships it"),
    ("hint.look_keyboard", "tab walks the controls, the arrows step a slider, space operates one; off returns space to the replay transport"),
    ("hint.look_entry", "double click a number to type it; enter applies, escape cancels, clicking away applies"),
    ("hint.look_activity", "a dot on a folded group that holds something switched on"),
    ("hint.look_shade", "says the panel under an open list takes no presses"),
    ("hint.look_animate", "the list reveal, the switch travel and the tab bar; off snaps all three"),
    ("hint.look_curve", "applied to a phase that moves both ways, so an ease_out reveal is an ease_in dismissal"),
    ("hint.look_gutters", "reserves strips for the labels rather than drawing them over the trace"),
    ("hint.look_meter", "a solid bar resolves a change a block cannot"),
    ("hint.record", "the ring keeps the last recordings and discards the oldest"),
    ("hint.record_gaps", "a gap is a block the disk could not take in time"),
    ("hint.record_format", "16 bit holds more range than a sound card input carries"),
    ("hint.record_block", "also the seek granularity and the dial sampling interval"),
    ("hint.record_running", "geometry applies when the recorder next starts"),
    ("hint.record_needs_audio", "nothing to record until the capture is running"),
    ("hint.replay_closed", "no recording open, the display is fed from the air"),
    ("hint.replay_no_segments", "nothing recorded yet"),
    ("hint.replay_scrub", "press or drag the bar to move; the columns are the levels"),
    ("hint.replay_speed", "space plays and pauses"),
    ("hint.replay_follow", "wait at the newest block instead of stopping"),
    ("hint.export_format", "lossless keeps every sample, QOA loses the weak ones first"),
    // Units.
    ("unit.hz", "Hz"),
    ("unit.khz", "kHz"),
    ("unit.db", "dB"),
    ("unit.dbfs", "dBFS"),
    ("unit.ms", "ms"),
    ("unit.s", "s"),
    ("unit.percent", "%"),
    ("unit.wpm", "wpm"),
    ("unit.baud", "bd"),
    ("unit.lps", "lps"),
    ("unit.pt", "pt"),
    ("unit.fps", "fps"),
    ("unit.chars", "chars"),
    ("unit.channels", "ch"),
    ("unit.lines", "lines"),
    ("unit.spots", "heard"),
    ("unit.mb", "MB"),
    ("unit.ppm", "ppm"),
    ("unit.deg", "deg"),
    // Keying gate reasons.
    ("gate.open", "open"),
    ("gate.warm", "warm"),
    ("gate.off", "off"),
    ("gate.sqlch", "sqlch"),
    ("gate.snr", "snr"),
    ("gate.flat", "flat"),
    // Transceiver link.
    ("link.closed", "closed"),
    ("link.starting", "starting"),
    ("link.up", "up"),
    ("link.stalled", "stalled"),
    // Modes.
    ("mode.none", "no mode"),
    ("mode.cw", "CW"),
    ("mode.rtty", "RTTY"),
    ("mode.navtex", "NAVTEX"),
    ("mode.psk31", "PSK31"),
    // Status bar.
    ("status.stopped", "stopped"),
    ("status.held", "back"),
    ("status.silent", "silent"),
    ("status.default_input", "default input"),
    ("status.letters", "letters"),
    ("status.figures", "figures"),
    ("status.wide_open", "full passband"),
    ("status.no_link", "no link"),
    ("status.shared", "shared"),
    ("status.muted", "muted"),
    ("status.recording", "recording"),
    ("status.replaying", "replay"),
    // Fixed choices. The suffix matches the text written to the config file, so
    // a new variant needs one entry and nothing else.
    ("enum.none", "none"),
    ("enum.auto", "auto"),
    ("enum.follow_device", "follow the device"),
    ("enum.linear", "linear"),
    ("enum.ease_out", "ease out"),
    ("enum.ease_in_out", "ease in out"),
    ("enum.wav", "WAV"),
    ("enum.lossless", "lossless"),
    ("enum.qoa", "QOA"),
    ("enum.i16", "16 bit"),
    ("enum.f32", "float"),
    ("enum.underline", "underline"),
    ("enum.attached", "attached"),
    ("enum.wasapi", "WASAPI"),
    ("enum.wavein", "waveIn (legacy)"),
    ("enum.left", "left"),
    ("enum.right", "right"),
    ("enum.mix", "mix"),
    ("enum.difference", "difference"),
    ("enum.rectangular", "rectangular"),
    ("enum.hann", "Hann"),
    ("enum.hamming", "Hamming"),
    ("enum.blackman", "Blackman"),
    ("enum.blackman_harris", "Blackman Harris"),
    ("enum.nuttall", "Nuttall"),
    ("enum.flattop", "flat top"),
    ("enum.kaiser", "Kaiser"),
    ("enum.grayscale", "grayscale"),
    ("enum.blue_steel", "blue steel"),
    ("enum.inferno", "inferno"),
    ("enum.viridis", "viridis"),
    ("enum.turbo", "turbo"),
    ("enum.classic", "classic"),
    ("enum.skimmer", "skimmer"),
    ("enum.audio", "hold the receiver"),
    ("enum.band", "hold the panorama"),
    ("enum.sdr", "receiver"),
    ("enum.s_units", "S units"),
    ("enum.dbm", "dBm"),
    ("enum.dbfs", "dBFS"),
    ("enum.upper", "upper case"),
    ("enum.lower", "lower case"),
    ("enum.as_received", "as received"),
    ("enum.baudot", "Baudot"),
    ("enum.ascii", "ASCII"),
    ("enum.even", "even"),
    ("enum.odd", "odd"),
    ("enum.mark", "mark"),
    ("enum.space", "space"),
    ("enum.local_file", "local file"),
    ("enum.cty", "cty.dat"),
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
    ("enum.detector", "follow detector"),
    ("enum.independent", "independent"),
    ("enum.follow", "follow rig"),
    ("enum.drive", "drive rig"),
    ("enum.both", "both directions"),
    ("enum.predictor", "predictor"),
    ("enum.spectral", "spectral"),
    ("enum.cw", "CW"),
    ("enum.usb", "USB"),
    ("enum.lsb", "LSB"),
    ("enum.dig_u", "DIG-U"),
    ("enum.dig_l", "DIG-L"),
    ("enum.am", "AM"),
    ("enum.sam", "SAM"),
    ("enum.fm", "FM"),
    ("enum.direct", "direct"),
    ("enum.shared", "shared"),
    ("enum.low", "low"),
    ("enum.high", "high"),
    ("enum.sideband.auto", "auto"),
    ("enum.sideband.upper", "upper"),
    ("enum.sideband.lower", "lower"),
];

#[derive(Clone)]
pub struct Catalog {
    language: String,
    map: HashMap<String, String>,
}

impl Catalog {
    /// Reference catalogue, always complete.
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
    /// own, so a language that has not been translated yet degrades to English
    /// rather than to blank labels.
    ///
    /// The file format is one key and value per line separated by an equals
    /// sign, with semicolon and number sign starting a comment. Keys that the
    /// build does not know are kept in the map and simply never looked up, which
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
                None => {
                    crate::log_warn!("i18n", "{} line {}: missing '='", path.display(), number + 1);
                }
            }
        }

        crate::log_info!("i18n", "{}: {} entries for '{}'", path.display(), loaded, language);
        catalog
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    /// Resolves a key. An unknown key returns itself.
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        match self.map.get(key) {
            Some(text) => text.as_str(),
            None => key,
        }
    }

    /// Writes the reference wording as a translation template.
    ///
    /// Every key the build knows is listed with its English value, so a
    /// translator starts from a complete file rather than from the source.
    pub fn write_template(directory: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join("template.lang");

        let mut out = String::with_capacity(TABLE.len() * 48);
        out.push_str("; RXScope translation template\n");
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

/// Installs the catalogue every later interface instance starts from.
pub fn install(catalog: Catalog) {
    let mut slot = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    *slot = Some(catalog);
}

/// Copy of the installed catalogue, or the reference one when none was
/// installed. Called once per interface instance, not per frame.
pub fn current() -> Catalog {
    let slot = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    slot.clone().unwrap_or_else(Catalog::builtin)
}