//! Visual constants.
//!
//! Colours are literal sRGB values written straight into the vertex stream;
//! the swapchain is UNORM, so no transfer curve is applied on the way out.
//!
//! Metrics are logical units at ninety six dpi. Every consumer multiplies
//! them by the interface scale and rounds, which keeps one pixel borders
//! exactly one pixel wide at any display factor.
//!
//! The structure also carries the style switches the drawing pass consults.
//! Copying them out of the configuration once per frame keeps the draw code
//! free of a dependency on the settings tree, which is owned by the
//! application and is not available while the widget tree is being drawn.

#![allow(dead_code)]

use crate::config::settings::{AnimCurve, AppearanceSettings, TabStyle};
use crate::render::Color;

pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub panel_header: Color,
    pub surface: Color,
    pub surface_dark: Color,
    pub control: Color,
    pub control_hover: Color,
    pub control_active: Color,
    /// Fill of a control that refuses input. Distinct from the enabled fill so
    /// a dead control reads as dead at a glance rather than only through its
    /// text colour.
    pub control_disabled: Color,
    pub border: Color,
    pub border_strong: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_disabled: Color,
    /// Weakest text in the palette, for hint lines and unit suffixes. Without a
    /// fourth level a hint competes with the label above it.
    pub text_faint: Color,
    pub accent: Color,
    pub accent_dim: Color,
    /// Background of the spectrum, the waterfall and their axis gutters.
    pub data_background: Color,
    /// Grid line that carries a label.
    pub grid_major: Color,
    /// Grid line between two labelled ones.
    pub grid_minor: Color,
    /// Retained for callers that draw a single strength grid.
    pub grid: Color,
    pub separator: Color,

    /// Marks the listening band of the headphone monitor.
    ///
    /// Deliberately not the accent. The accent already marks where a decoder is
    /// listening, and the two bands are different claims about the same axis:
    /// one is what is being measured, the other is what reaches the ear. Sharing
    /// a colour would make a mismatch between them invisible, which is precisely
    /// the mismatch worth seeing.
    pub monitor: Color,
    /// Hover fill of the close button. The one place a warning colour is
    /// warranted, because the action cannot be undone.
    pub danger: Color,

    /// Height of a single control row.
    pub row_height: f32,
    /// Spacing between siblings.
    pub gap: f32,
    /// Inner spacing of containers.
    pub padding: f32,
    /// Inset of a scrolling panel from its own edges.
    pub panel_margin: f32,
    /// Inset of the content of a group from the box drawn around it.
    ///
    /// Applied to an inner container rather than to the box, so the header
    /// still spans the full width. A header inset by the content padding reads
    /// as a floating label rather than as a title strip.
    pub group_padding: f32,
    /// Inset of the whole side panel from the surfaces beside it.
    pub panel_inset: f32,
    /// Clear space between the scroll indicator and the content beside it.
    pub scrollbar_gap: f32,
    pub border_px: f32,
    pub checkbox: f32,
    pub scrollbar: f32,
    pub splitter: f32,
    pub header_height: f32,
    /// Height of the caption strip, which is also the toolbar row.
    pub caption_height: f32,
    /// Floor of the label column of a settings row.
    pub label_min: f32,
    /// Largest share of the row the label column may occupy.
    ///
    /// A share rather than a ceiling in logical units, because a ceiling is a
    /// bet on one wording. The reference text and a translation of it differ by
    /// half again in length, and only the first is known when a constant is
    /// written; a share leaves the control the rest of the row whatever the
    /// language does, and the panel widens to cover the difference.
    pub label_fraction: f32,
    /// Floor of the numeric column of a settings row.
    pub value_min: f32,
    /// Largest share of the row the numeric column may occupy.
    pub value_fraction: f32,
    /// Narrowest a control may become before the label gives way instead.
    ///
    /// The one thing a long caption must not do is erase the track it labels: a
    /// slider of twenty pixels states nothing and cannot be dragged.
    pub control_min: f32,
    /// Track of a toggle switch.
    pub toggle_width: f32,
    pub toggle_height: f32,
    /// Reciprocal time constant of the tracking animations, per second.
    ///
    /// Derived from the stated duration rather than set independently, so one
    /// number governs how fast the interface moves. The tab bar tracks a target
    /// that can move mid flight, which is what an exponential approach is for;
    /// a transition between two known endpoints uses the duration directly.
    pub toggle_speed: f32,
    /// Distance from the knob within which a slider press grabs it instead of
    /// jumping to the pointer.
    pub slider_grab: f32,
    /// Distance from a band edge within which a press grabs that edge.
    pub edge_grab: f32,
    /// Size of a hint line, as a fraction of the interface font.
    pub hint_scale: f32,

    /// Style switches, copied from the configuration once per frame.
    pub focus_ring: bool,
    pub accent_hover: bool,
    pub group_tick: bool,
    pub tab_underline: bool,
    pub value_column: bool,
    pub numeric_entry: bool,
    pub popup_shade: f32,
    pub group_activity: bool,
    pub keyboard_focus: bool,
    pub splitter_grip: bool,
    pub animate: bool,
    pub anim_ms: f32,
    pub anim_curve: AnimCurve,
}

impl Theme {
    /// Dark grey surfaces, white text, a single accent for focus, selection
    /// and data. The accent comes from the configuration so the operator can
    /// match it to the rest of the desk.
    pub fn dark(accent: Color) -> Theme {
        let mut theme = Theme {
            background: Color::hex(0x1E1E1E),
            panel: Color::hex(0x252526),
            panel_header: Color::hex(0x2D2D30),
            surface: Color::hex(0x1B1B1C),
            surface_dark: Color::hex(0x141414),
            control: Color::hex(0x2D2D30),
            control_hover: Color::hex(0x3E3E42),
            control_active: Color::hex(0x094771),
            control_disabled: Color::hex(0x232326),
            border: Color::hex(0x3F3F46),
            border_strong: Color::hex(0x555559),
            text: Color::hex(0xE6E6E6),
            text_dim: Color::hex(0x9A9A9E),
            text_disabled: Color::hex(0x6A6A6E),
            text_faint: Color::hex(0x57575B),
            accent,
            accent_dim: accent.with_alpha(0.35),
            data_background: Color::hex(0x0A0A0C),
            grid_major: Color::hex(0x33333A),
            grid_minor: Color::hex(0x33333A).with_alpha(0.45),
            grid: Color::hex(0x2A2A2D),
            separator: Color::hex(0x3F3F46).with_alpha(0.7),
            monitor: Color::hex(0xE0A040),
            danger: Color::hex(0xC42B1C),

            row_height: 22.0,
            gap: 4.0,
            padding: 6.0,
            panel_margin: 6.0,
            group_padding: 6.0,
            panel_inset: 2.0,
            scrollbar_gap: 3.0,
            border_px: 1.0,
            checkbox: 13.0,
            scrollbar: 8.0,
            splitter: 5.0,
            header_height: 20.0,
            caption_height: 28.0,
            label_min: 44.0,
            label_fraction: 0.55,
            value_min: 36.0,
            value_fraction: 0.30,
            control_min: 72.0,
            toggle_width: 26.0,
            toggle_height: 14.0,
            toggle_speed: 12.0,
            slider_grab: 9.0,
            edge_grab: 6.0,
            hint_scale: 0.85,

            focus_ring: true,
            accent_hover: false,
            group_tick: false,
            tab_underline: true,
            value_column: true,
            numeric_entry: true,
            popup_shade: 0.18,
            group_activity: true,
            keyboard_focus: true,
            splitter_grip: true,
            animate: true,
            anim_ms: 120.0,
            anim_curve: AnimCurve::EaseOut,
        };
        theme.apply(accent, &AppearanceSettings::default());
        theme
    }

    /// Pushes the appearance section into the palette and the metrics.
    ///
    /// Cheap enough to call every frame: the structure is a few dozen words and
    /// nothing downstream caches a value out of it.
    pub fn apply(&mut self, accent: Color, cfg: &AppearanceSettings) {
        self.accent = accent;
        self.accent_dim = accent.with_alpha(0.35);

        self.data_background = Color::hex(cfg.data_background_rgb);
        self.grid_minor = self.grid_major.with_alpha(cfg.grid_minor_alpha);
        self.separator = self.border.with_alpha(cfg.separator_alpha);

        self.row_height = cfg.row_height;
        self.gap = cfg.gap;
        self.panel_margin = cfg.panel_margin;
        self.group_padding = cfg.group_padding;
        self.caption_height = cfg.caption_height;
        self.hint_scale = cfg.hint_scale;

        self.focus_ring = cfg.focus_ring;
        self.accent_hover = cfg.accent_hover;
        self.group_tick = cfg.group_tick;
        self.tab_underline = cfg.tab_style == TabStyle::Underline;
        self.value_column = cfg.value_column;
        self.numeric_entry = cfg.numeric_entry;
        self.popup_shade = cfg.popup_shade;
        self.group_activity = cfg.group_activity;
        self.keyboard_focus = cfg.keyboard_focus;
        self.splitter_grip = cfg.splitter_grip;

        self.animate = cfg.animate;
        self.anim_ms = cfg.anim_ms.max(1.0);
        self.anim_curve = cfg.anim_curve;
        // The factor is what makes the stated duration and the tracker feel the
        // same: at the default it lands on the constant the tab bar used before
        // the duration became a setting.
        self.toggle_speed = 1500.0 / self.anim_ms;
    }

    /// Border colour of a control under the pointer.
    ///
    /// Held here rather than at each call site so the accent discipline is one
    /// decision instead of eight.
    pub fn hover_border(&self) -> Color {
        if self.accent_hover {
            self.accent
        } else {
            self.border_strong
        }
    }
}