//! Control implementations.
//!
//! Each control declares its own subtree, so a caller cannot leave the
//! container stack unbalanced. Values are passed by mutable reference and the
//! return value reports whether the frame changed them, which is what the
//! caller needs in order to react without diffing the settings.
//!
//! Every text argument is a localization key. An unknown key resolves to itself,
//! so a run time string may be passed through the same call without harm, and
//! widget identity is derived from the key rather than from the resolved text:
//! changing language must not reset focus, folding or scroll position.
//!
//! Fixed size chrome is declared without shrink, which is the layout default.
//! Only the label column of a field opts into shrinking, so a narrow settings
//! panel compresses the text and never the control next to it.

#![allow(dead_code)]

use crate::config::ConfigEnum;
use crate::font::{FontId, FontSystem};
use crate::platform::{CursorKind, Key};
use crate::render::Color;

use super::layout::{Align, Direction, Edges, Style};
use super::{ButtonKind, CaptionHit, Frame, Item, Span, TextAlign, WindowButton};

/// What one frame of a text field produced.
///
/// Two facts rather than one, because the callers want different ones and a
/// single boolean would force the field to guess which.
struct FieldResult {
    changed: bool,
    submitted: bool,
}

/// Quantum a slider snaps to, derived from the displayed precision. Snapping to
/// what is shown removes the case where a value reads as one number and behaves
/// as another.
fn step_for(decimals: usize) -> f32 {
    match decimals {
        0 => 1.0,
        1 => 0.1,
        2 => 0.01,
        3 => 0.001,
        _ => 0.0001,
    }
}

fn quantize(value: f32, origin: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return value;
    }
    origin + ((value - origin) / step).round() * step
}

/// The value alone, without its unit.
///
/// What a field is seeded with, because the unit is a label rather than part of
/// the number and typing over it would only have to be parsed away again.
fn value_text(value: f32, decimals: usize) -> String {
    format!("{:.*}", decimals, value)
}

/// Reads a number typed into a slider cell.
///
/// The stated range and quantum are applied, so a value typed and a value
/// dragged are the same set: an entry between two steps is snapped and one
/// outside the range is clamped. Text that is not a number is refused rather
/// than guessed at, which is what makes committing on a press elsewhere safe.
fn parse_value(text: &str, min: f32, max: f32, step: f32) -> Option<f32> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    // A comma is what most keyboard layouts in Europe produce for a decimal
    // point, and the two cannot be confused here: a cell holds one number and
    // no group separators.
    let mut cleaned = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        match ch {
            ',' => cleaned.push('.'),
            ' ' | '\'' | '_' => {}
            other => cleaned.push(other),
        }
    }
    let value: f32 = cleaned.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(quantize(value.clamp(min, max), min, step))
}

/// What one frame of typing produced.
struct EditOutcome {
    changed: bool,
    submitted: bool,
    cancelled: bool,
}

/// Amount one wheel notch moves a slider.
fn wheel_step(range: f32, step: f32) -> f32 {
    let wanted = range * 0.01;
    let snapped = (wanted / step).round() * step;
    snapped.max(step)
}

/// Pulls a value onto nought when it lands within a detent of it.
fn detent_zero(value: f32, min: f32, max: f32, width: f32) -> f32 {
    if min < 0.0 && max > 0.0 && value.abs() < width {
        0.0
    } else {
        value
    }
}

/// Travel of the zero detent, in pixels of pointer movement.
const DETENT_PIXELS: f32 = 1.5;

/// Breaks text into lines at word boundaries, as byte ranges into the input.
///
/// Ranges rather than owned strings, so a panel of a few dozen hints does not
/// allocate one string per line on every frame.
///
/// The character fit is the hard bound and the break is then moved back to the
/// last space inside it: a word split in half is harder to read than a short
/// line, and the whole reason for wrapping is that the alternative loses the
/// end of the sentence altogether.
fn wrap_words(
    fonts: &mut FontSystem,
    text: &str,
    font: FontId,
    px: f32,
    width: f32,
    out: &mut Vec<(usize, usize)>,
) {
    out.clear();
    if text.is_empty() {
        return;
    }
    if width <= 1.0 || fonts.measure(text, font, px) <= width {
        out.push((0, text.len()));
        return;
    }

    let mut start = 0usize;
    while start < text.len() {
        let rest = &text[start..];
        let cut = fonts.fit(rest, font, px, width);
        if cut >= rest.len() {
            out.push((start, text.len()));
            break;
        }

        let mut end = cut;
        if cut > 0 {
            if let Some(space) = rest[..cut].rfind(' ') {
                if space > 0 {
                    end = space;
                }
            }
        }
        if end == 0 {
            // A single word wider than the line, or a line narrower than one
            // character. One character is taken so the loop cannot stall.
            end = rest.chars().next().map(|c| c.len_utf8()).unwrap_or(rest.len());
        }
        out.push((start, start + end));

        // The break consumes the space it landed on, so a line never starts
        // with one.
        let mut next = start + end;
        while next < text.len() && text.as_bytes()[next] == b' ' {
            next += 1;
        }
        if next <= start {
            break;
        }
        start = next;
    }
}

impl<'a> Frame<'a> {
    // ------------------------------------------------------------ containers

    /// Plain container. The caller must call end.
    pub fn begin(&mut self, style: Style) {
        self.ui.push(style, Item::None);
    }

    /// Container with a background and a border.
    pub fn begin_frame(&mut self, style: Style, fill: Color, border: Color) {
        let border_px = self.ui.theme.border_px;
        self.ui.push(style, Item::Frame { fill, border, border_px });
    }

    /// Container that clips whatever its children draw outside its bounds.
    pub fn begin_clipped(&mut self, style: Style, fill: Color, border: Color) {
        let border_px = self.ui.theme.border_px;
        let index = self.ui.push(style, Item::Frame { fill, border, border_px });
        self.ui.nodes[index].clip = true;
    }

    pub fn end(&mut self) {
        self.ui.pop();
    }

    /// Empty node of a stated width, for aligning a row against a gutter.
    pub fn strut(&mut self, width_px: f32) {
        let w = width_px.max(0.0);
        self.ui.add(Style::row().width_px(w), [w, 0.0], Item::None);
    }

    /// Surface with a fixed title strip. The title key also scopes widget
    /// identity, so the same key can repeat in another panel.
    pub fn begin_panel(&mut self, title: &str, style: Style) {
        // A data panel takes no inner padding: the spectrum has to reach the
        // frame, otherwise the axis gutters sit inside a second margin.
        let content = Style::column().grow(1.0);
        self.begin_titled(title, style, false, true, content);
    }

    pub fn end_panel(&mut self) {
        self.ui.pop();
        self.ui.pop();
        self.ui.pop_scope();
    }

    /// Boxed group with a foldable header. Returns true while the group is
    /// unfolded; end_group must be called either way.
    pub fn begin_group(&mut self, title: &str) -> bool {
        let gap = self.ui.m(self.ui.theme.gap);
        let pad = self.ui.m(self.ui.theme.group_padding);
        let content = Style::column().gap(gap).padding(pad);
        self.begin_titled(title, Style::column(), true, true, content)
    }

    pub fn end_group(&mut self) {
        self.ui.pop();
        self.ui.pop();
        self.ui.pop_scope();
    }

    /// Collapsible surface with a caller supplied size.
    pub fn begin_group_titled(&mut self, title: &str, style: Style, open: bool) -> bool {
        let content = Style::column().grow(1.0);
        self.begin_titled(title, style, true, open, content)
    }

    /// Shared body of the titled containers.
    ///
    /// Two nodes are pushed: the box, whose padding is the border alone so the
    /// header spans the full width, and a content container that carries the
    /// inset the children need. A single node cannot do both.
    fn begin_titled(
        &mut self,
        title: &str,
        style: Style,
        collapsible: bool,
        default_open: bool,
        content: Style,
    ) -> bool {
        let fill = self.ui.theme.surface_dark;
        let border = self.ui.theme.border_strong;
        let border_px = self.ui.theme.border_px;
        let header_h = self.ui.m(self.ui.theme.header_height);

        self.ui.push_scope(title);
        let id = if collapsible { self.ui.id("##header") } else { 0 };

        let mut open = default_open;
        if collapsible {
            self.ui.register_focusable(id);
            let entry = self.ui.retained_of(id);
            open = if entry.seen { entry.open } else { default_open };
            if self.ui.clicked(id, CursorKind::Hand, false) || self.activated(id) {
                open = !open;
            }
            let slot = self.ui.retained_mut(id);
            slot.open = open;
            slot.seen = true;
        }

        let outer = Style {
            direction: Direction::Column,
            padding: Edges::all(self.ui.line(border_px)),
            ..style
        };
        self.ui.push(outer, Item::Frame { fill, border, border_px });

        let span = self.ui.intern_key(title);
        let header = self.ui.add(
            Style::row().height_px(header_h),
            [0.0, header_h],
            Item::Header { text: span, id, collapsible, open, active: false },
        );
        self.ui.last_header = header;

        self.ui.push(content, Item::None);
        open
    }

    /// Marks the group being declared as holding something switched on.
    ///
    /// Called after the header rather than passed into it, because a section
    /// knows what counts as active and the container does not. Called outside the
    /// test on the folded state as well, since the folded case is the only one
    /// where the mark carries information.
    pub fn mark_active(&mut self, active: bool) {
        if !active || !self.ui.theme.group_activity {
            return;
        }
        let at = self.ui.last_header;
        if let Some(Item::Header { active: flag, .. }) = self.ui.items.get_mut(at) {
            *flag = true;
        }
    }

    /// Vertically scrolling container.
    pub fn begin_scroll(&mut self, name: &str, mut style: Style) {
        let id = self.ui.id(name);
        let rect = self.ui.prev_rect(id);

        let wheel = self.ui.input.wheel;
        if wheel != 0.0 && self.ui.hovered(id, rect) {
            let step = self.ui.m(self.ui.theme.row_height * 3.0);
            let content = self.ui.retained_of(id).content;
            let max = (content - rect.h).max(0.0);
            let entry = self.ui.retained_mut(id);
            entry.scroll = (entry.scroll - wheel * step).clamp(0.0, max);
        }

        // The indicator overlays the right edge, so the content is kept clear of
        // it. The reservation is the bar plus a gutter rather than the bar
        // alone: with the two touching, a value right aligned against that edge
        // reads as truncated whether or not it is.
        let reserved = self.ui.m(self.ui.theme.scrollbar) + self.ui.m(self.ui.theme.scrollbar_gap);
        style.padding.right = style.padding.right.max(reserved);
        style.direction = Direction::Column;
        style.scroll = true;

        let offset = self.ui.retained_of(id).scroll;
        let index = self.ui.push(style, Item::None);
        self.ui.nodes[index].clip = true;
        self.ui.nodes[index].scrollable = true;
        self.ui.nodes[index].scroll = offset;
        self.ui.scroll_nodes.push((index, id));
    }

    pub fn end_scroll(&mut self) {
        self.ui.pop();
    }

    /// Opens a subtree whose controls are shown but refuse input.
    pub fn begin_disabled(&mut self, disabled: bool) {
        self.ui.begin_disabled(disabled);
    }

    pub fn end_disabled(&mut self) {
        self.ui.end_disabled();
    }

    // ---------------------------------------------------------------- atoms

    pub fn label(&mut self, key: &str) {
        let color = self.ui.theme.text;
        self.label_styled(key, color, FontId::Ui, TextAlign::Left, Style::row().shrink(1.0));
    }

    pub fn label_dim(&mut self, key: &str) {
        let color = self.ui.theme.text_dim;
        self.label_styled(key, color, FontId::Ui, TextAlign::Left, Style::row().shrink(1.0));
    }

    /// Explanatory line inside a group.
    ///
    /// Smaller and weaker than a label. A hint set in the same size competes
    /// with the caption above it for the same attention, and the operator has
    /// to read both to find out which is which.
    ///
    /// Broken across lines rather than cut. A hint is the longest string in the
    /// panel and is the first thing a translation makes longer; a sentence with
    /// its tail replaced by a marker states nothing at all, whereas three short
    /// lines state the whole of it.
    pub fn hint(&mut self, key: &str) {
        let color = self.ui.theme.text_faint;
        let size = self.hint_px();
        let width = self.ui.field_width();

        // The text is copied out so the node list and the string arena may be
        // written while it is being measured.
        let mut text = std::mem::take(&mut self.ui.hint_text);
        text.clear();
        text.push_str(self.ui.tr(key));

        let mut lines = std::mem::take(&mut self.ui.wrap);
        wrap_words(self.fonts, &text, FontId::Ui, size, width, &mut lines);

        if lines.len() <= 1 {
            self.label_sized(
                &text,
                color,
                FontId::Ui,
                size,
                TextAlign::Left,
                Style::row().shrink(1.0),
            );
        } else {
            self.begin(Style::column());
            for index in 0..lines.len() {
                let (a, b) = lines[index];
                self.label_sized(
                    &text[a..b],
                    color,
                    FontId::Ui,
                    size,
                    TextAlign::Left,
                    Style::row().shrink(1.0),
                );
            }
            self.end();
        }

        self.ui.wrap = lines;
        self.ui.hint_text = text;
    }

    pub fn label_mono(&mut self, key: &str, color: Color, align: TextAlign, style: Style) {
        self.label_styled(key, color, FontId::Mono, align, style);
    }

    pub fn label_styled(
        &mut self,
        key: &str,
        color: Color,
        font: FontId,
        align: TextAlign,
        style: Style,
    ) {
        let size = self.ui.ui_px;
        self.label_sized(key, color, font, size, align, style);
    }

    fn label_sized(
        &mut self,
        key: &str,
        color: Color,
        font: FontId,
        size: f32,
        align: TextAlign,
        style: Style,
    ) {
        let (w, h) = {
            let text = self.ui.tr(key);
            (self.fonts.measure(text, font, size), self.fonts.line_height(font, size))
        };
        let span = self.ui.intern_key(key);
        self.ui.add(style, [w, h], Item::Label { text: span, color, font, size, align });
    }

    fn hint_px(&self) -> f32 {
        (self.ui.ui_px * self.ui.theme.hint_scale).round().max(8.0)
    }

    /// Flexible empty cell, used to push the following widgets to the end.
    pub fn spacer(&mut self, grow: f32) {
        self.ui.add(Style::row().grow(grow), [0.0, 0.0], Item::None);
    }

    pub fn gap(&mut self, logical: f32) {
        let v = self.ui.m(logical);
        self.ui.add(Style::row().width_px(v).height_px(v), [v, v], Item::None);
    }

    pub fn separator(&mut self) {
        let h = self.ui.m(self.ui.theme.gap * 2.0);
        self.ui.add(Style::row().height_px(h), [0.0, h], Item::Separator { vertical: false });
    }

    pub fn separator_vertical(&mut self) {
        let w = self.ui.m(self.ui.theme.gap * 2.0);
        self.ui.add(Style::row().width_px(w), [w, 0.0], Item::Separator { vertical: true });
    }

    /// Reserves an area for the application to draw into after the widget
    /// pass. The tag identifies it in the rectangle lookup.
    pub fn custom(&mut self, tag: u32, style: Style) {
        self.ui.add(style, [0.0, 0.0], Item::Custom { tag });
    }

    // -------------------------------------------------------------- chrome

    /// Draggable region of the caption strip.
    ///
    /// The window proc reports the whole client area as client, so which part
    /// of it moves the window is a layout question and is answered here: the
    /// zone occupies whatever the toolbar has left over, so a button placed on
    /// the strip is a button and the space between buttons is a handle.
    pub fn caption(&mut self, name: &str, style: Style) -> CaptionHit {
        let id = self.ui.id(name);
        let rect = self.ui.prev_rect(id);
        let hovered = self.ui.hovered(id, rect);
        let mut hit = CaptionHit::default();

        if hovered {
            // Recorded as hot although nothing is drawn, because an input layer
            // that takes the mouse buttons asks that question to decide whether
            // the press is the interface's. A drag zone that answered no would
            // be a window that cannot be moved.
            self.ui.hot = id;
            // The second click of a pair arrives as a press and a double click
            // in the same frame, so the double is tested first.
            if self.ui.input.double_click {
                hit.double = true;
                self.ui.input.consume_left();
            } else if self.ui.input.left_pressed() {
                hit.drag = true;
                self.ui.input.consume_left();
            }
        }

        self.ui.add(style, [0.0, 0.0], Item::Caption { id });
        hit
    }

    /// Window command button. Carries no text, only a drawn mark.
    pub fn window_button(&mut self, name: &str, kind: WindowButton) -> bool {
        let id = self.ui.id(name);
        let clicked = self.ui.clicked(id, CursorKind::Hand, false);
        let w = self.ui.m(self.ui.theme.caption_height * 1.6);
        self.ui.add(
            Style::row().width_px(w).align_self(Align::Stretch),
            [w, 0.0],
            Item::WindowChrome { id, kind },
        );
        clicked
    }

    // -------------------------------------------------------------- controls

    pub fn button(&mut self, key: &str) -> bool {
        self.button_kind(key, ButtonKind::Normal, false)
    }

    /// Borderless button, for dense rows where a frame would add noise.
    pub fn button_flat(&mut self, key: &str) -> bool {
        self.button_kind(key, ButtonKind::Flat, false)
    }

    fn button_kind(&mut self, key: &str, kind: ButtonKind, selected: bool) -> bool {
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        // Reachable by keyboard and not by pointer, which is not a contradiction:
        // a press has already stated its target, so focus there would only leave
        // an outline behind, whereas a button the traversal cannot reach is a
        // command that needs a hand off the transceiver.
        //
        // A tab is not a stop. The strip has its own shortcuts, and six of them
        // before every panel is six presses the operator repeats each time.
        if kind != ButtonKind::Tab {
            self.ui.register_focusable(id);
        }
        let clicked = self.ui.clicked(id, CursorKind::Arrow, false) || self.activated(id);

        let px = self.ui.ui_px;
        let text_w = {
            let text = self.ui.tr(key);
            self.fonts.measure(text, FontId::Ui, px)
        };
        let pad = self.ui.m(self.ui.theme.padding);
        let h = self.ui.m(self.ui.theme.row_height);
        let span = self.ui.intern_key(key);
        let w = text_w + pad * 2.0;

        // A tab strip has to hold every entry, and a translation can make the
        // set wider than the window. Letting the entries compress elides the
        // captions; refusing to compress pushes the last of them off the edge,
        // where it cannot be reached at all.
        let style = match kind {
            ButtonKind::Tab => Style::row()
                .width_px(w)
                .height_px(h)
                .shrink(1.0)
                .min_w(self.ui.m(28.0)),
            _ => Style::row().width_px(w).height_px(h),
        };
        self.ui.add(style, [w, h], Item::Button { text: span, id, kind, selected, enabled });
        clicked
    }

    /// Tab strip. Returns true when the selection changed.
    ///
    /// The selection is stated by one bar that slides and stretches between
    /// positions rather than by a mark appearing under each tab in turn. A mark
    /// that jumps states where the selection is; one that travels states where
    /// it came from, which is what an operator switching between two views is
    /// actually tracking.
    pub fn tabs(&mut self, name: &str, index: &mut usize, items: &[&str]) -> bool {
        if items.is_empty() {
            return false;
        }
        let gap = self.ui.m(self.ui.theme.gap);
        let pad = self.ui.m(self.ui.theme.padding);

        self.ui.push_scope(name);

        // The target is the selected tab as it was laid out on the previous
        // frame, held as an offset inside the strip so that moving the whole
        // strip does not displace the bar. Both rectangles come from the same
        // frame, which is what makes the offset self consistent across a resize.
        let strip_id = self.ui.id("##strip");
        let strip_rect = self.ui.prev_rect(strip_id);
        let target = {
            let key = items[(*index).min(items.len() - 1)];
            self.ui.prev_rect(self.ui.id(key))
        };

        let mut bar_x = 0.0f32;
        let mut bar_w = 0.0f32;
        if !strip_rect.is_empty() && !target.is_empty() {
            let want_x = target.x - strip_rect.x;
            let want_w = target.w;
            // The same time constant the toggle uses. Both are a mark of a
            // control moving to a new position, so one constant is enough, and
            // two would invite the operator to make them disagree.
            //
            // A full step is the unanimated case: the bar lands on the selected
            // tab within the frame of the press.
            let step = if self.ui.theme.animate {
                (self.ui.dt * self.ui.theme.toggle_speed).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let slot = self.ui.retained_mut(strip_id);
            if !slot.seen {
                slot.seen = true;
                slot.bar_x = want_x;
                slot.bar_w = want_w;
            } else {
                slot.bar_x += (want_x - slot.bar_x) * step;
                slot.bar_w += (want_w - slot.bar_w) * step;
                // Snapped once the remainder is below a pixel, otherwise the bar
                // approaches its target forever and the geometry changes on
                // every frame for no visible reason.
                if (want_x - slot.bar_x).abs() < 0.5 {
                    slot.bar_x = want_x;
                }
                if (want_w - slot.bar_w).abs() < 0.5 {
                    slot.bar_w = want_w;
                }
            }
            bar_x = slot.bar_x;
            bar_w = slot.bar_w;
        }

        self.ui.push(
            Style::row().gap(gap).padding_xy(pad, 0.0).align(Align::Stretch),
            Item::TabBar { id: strip_id, x: bar_x, w: bar_w },
        );

        let mut changed = false;
        for (i, item) in items.iter().enumerate() {
            if self.button_kind(item, ButtonKind::Tab, i == *index) && *index != i {
                *index = i;
                changed = true;
            }
        }
        self.end();
        self.ui.pop_scope();
        changed
    }

    pub fn checkbox(&mut self, key: &str, value: &mut bool) -> bool {
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        let clicked = self.ui.clicked(id, CursorKind::Hand, true);
        self.ui.register_focusable(id);
        let clicked = clicked || self.activated(id);
        if clicked {
            *value = !*value;
        }

        let px = self.ui.ui_px;
        let text_w = {
            let text = self.ui.tr(key);
            self.fonts.measure(text, FontId::Ui, px)
        };
        let side = self.ui.m(self.ui.theme.checkbox);
        let gap = self.ui.m(self.ui.theme.gap);
        let h = self.ui.m(self.ui.theme.row_height);
        let span = self.ui.intern_key(key);

        let w = side + gap + text_w;
        self.ui.add(
            Style::row().height_px(h).min_w(side).shrink(1.0),
            [w, h],
            Item::Checkbox { text: span, id, checked: *value, enabled },
        );
        clicked
    }

    /// Sliding switch laid out as a labelled row.
    pub fn toggle(&mut self, key: &str, value: &mut bool) -> bool {
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        let clicked = self.ui.clicked(id, CursorKind::Hand, true);
        self.ui.register_focusable(id);
        let clicked = clicked || self.activated(id);
        if clicked {
            *value = !*value;
        }
        let anim = self.ui.animate(id, if *value { 1.0 } else { 0.0 });

        let h = self.ui.m(self.ui.theme.row_height);
        let tw = self.ui.m(self.ui.theme.toggle_width);
        self.begin_field(key);
        self.ui.add(
            Style::row().width_px(tw).height_px(h),
            [tw, h],
            Item::Toggle { id, on: *value, anim, enabled },
        );
        self.spacer(1.0);
        self.end();
        clicked
    }

    /// Opens a labelled row.
    ///
    /// The label column is as wide as the widest label of the enclosing scope,
    /// measured over the previous frame, and bounded by a share of the row
    /// rather than by a constant. A constant is a bet on one wording: a
    /// translation that runs half again as long loses the end of every caption,
    /// and on a wide panel the constant wastes the space that would have held
    /// it. The share leaves the control its own floor whatever the language
    /// does, and the panel widens on the next frame to cover the rest.
    fn begin_field(&mut self, key: &str) {
        let gap = self.ui.m(self.ui.theme.gap);
        let h = self.ui.m(self.ui.theme.row_height);
        let px = self.ui.ui_px;

        let measured = {
            let text = self.ui.tr(key);
            self.fonts.measure(text, FontId::Ui, px)
        };
        let column = self.ui.label_column_width(measured);

        self.begin(Style::row().height_px(h).gap(gap).align(Align::Center));
        let color = if self.ui.enabled() {
            self.ui.theme.text_dim
        } else {
            self.ui.theme.text_disabled
        };
        self.label_styled(
            key,
            color,
            FontId::Ui,
            TextAlign::Left,
            Style::row()
                .width_px(column)
                .shrink(1.0)
                .min_w(self.ui.m(24.0))
                .height_px(h),
        );
    }

    fn format_value(&self, value: f32, decimals: usize, unit: &str) -> String {
        if unit.is_empty() {
            format!("{:.*}", decimals, value)
        } else {
            format!("{:.*} {}", decimals, value, self.ui.tr(unit))
        }
    }

    /// Width of the numeric cell of a slider.
    ///
    /// Taken from the two endpoints rather than from the value on screen. With
    /// a fixed decimal count no intermediate value can be longer than the
    /// longer endpoint, so this is exact, and it is the only way the track
    /// stops changing length while the control is being dragged.
    fn value_cell_width(&mut self, min: f32, max: f32, decimals: usize, unit: &str) -> f32 {
        let px = self.ui.ui_px;
        let low = self.format_value(min, decimals, unit);
        let high = self.format_value(max, decimals, unit);
        let measured = self
            .fonts
            .measure(&low, FontId::Mono, px)
            .max(self.fonts.measure(&high, FontId::Mono, px));

        if self.ui.theme.value_column {
            self.ui.value_column_width(measured)
        } else {
            measured
        }
    }

    /// Continuous value with a track, a knob and a numeric readout.
    pub fn slider(
        &mut self,
        key: &str,
        value: &mut f32,
        min: f32,
        max: f32,
        decimals: usize,
        unit: &str,
    ) -> bool {
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        let range = (max - min).max(1e-9);
        let step = step_for(decimals);
        let rect = self.ui.prev_rect(id);
        let hovered = self.ui.hovered(id, rect);
        let mut changed = false;

        if hovered {
            self.ui.hot = id;
            self.ui.cursor = CursorKind::ResizeHorizontal;
        }

        let mods = self.ui.input.mods;
        let speed = if mods.shift && mods.ctrl {
            0.02
        } else if mods.shift {
            0.20
        } else if mods.ctrl {
            4.0
        } else {
            1.0
        };

        if self.ui.active == id {
            if self.ui.input.left_down() {
                let state = self.ui.retained_of(id);
                let travel = self.ui.input.mouse.0 - state.grab_x;
                let per_pixel = state.units_px * speed;
                let raw = state.grab + travel * per_pixel;
                let mut next = quantize(raw.clamp(min, max), min, step);
                next = detent_zero(next, min, max, per_pixel * DETENT_PIXELS);
                if (next - *value).abs() > step * 0.5 {
                    *value = next;
                    changed = true;
                }
            } else {
                self.ui.active = 0;
            }
        } else if hovered && self.ui.input.left_pressed() {
            self.ui.active = id;
            self.ui.focus = id;
            self.ui.focus_is_text = false;

            let pointer = self.ui.input.mouse.0;
            let track = rect.w.max(1.0);
            let knob_x = rect.x + track * ((*value - min) / range).clamp(0.0, 1.0);
            let slack = self.ui.m(self.ui.theme.slider_grab);

            let mut anchor = *value;
            if (pointer - knob_x).abs() > slack {
                let t = ((pointer - rect.x) / track).clamp(0.0, 1.0);
                anchor = quantize(min + t * range, min, step).clamp(min, max);
                if (anchor - *value).abs() > step * 0.5 {
                    *value = anchor;
                    changed = true;
                }
            }

            let units_px = range / track;
            let slot = self.ui.retained_mut(id);
            slot.grab = anchor;
            slot.grab_x = pointer;
            slot.units_px = units_px;
        }

        let wheel = self.ui.input.wheel;
        if hovered && wheel != 0.0 {
            let notch = wheel_step(range, step) * speed;
            let mut next = quantize((*value + wheel * notch).clamp(min, max), min, step);
            next = detent_zero(next, min, max, notch * 0.5);
            if (next - *value).abs() > step * 0.5 {
                *value = next;
                changed = true;
            }
        }

        // The arrows step by the quantum the value is displayed at, which is the
        // finest move that changes what the operator reads. Page is ten of them,
        // so crossing a range is a dozen presses rather than a hundred.
        //
        // The extremes are deliberately not bound. The keys carry no label in the
        // interface, so they are found by accident, and finding them on the
        // monitor volume is loud.
        self.ui.register_focusable(id);
        if self.ui.theme.keyboard_focus && self.ui.focus == id && enabled {
            let mut delta = 0.0f32;
            for press in self.ui.input.keys.clone() {
                match press.key {
                    Key::Left | Key::Down => delta -= step,
                    Key::Right | Key::Up => delta += step,
                    Key::PageDown => delta -= step * 10.0,
                    Key::PageUp => delta += step * 10.0,
                    _ => {}
                }
            }
            if delta != 0.0 {
                let next = quantize((*value + delta).clamp(min, max), min, step);
                if (next - *value).abs() > step * 0.5 {
                    *value = next;
                    changed = true;
                }
            }
        }

        *value = value.clamp(min, max);

        // The numeric cell is a target as well as a statement. The identity is
        // derived from the slider rather than from a key of its own, so the two
        // cannot drift apart when the caller renames one of them.
        let cell_id = super::mix(id, super::fnv("##cell"));
        let entry = self.ui.theme.numeric_entry && enabled;
        let mut editing = entry && self.ui.editing(cell_id);
        let mut buffer = String::new();

        if entry {
            let rect = self.ui.prev_rect(cell_id);
            let hovered = self.ui.hovered(cell_id, rect);
            if hovered {
                self.ui.hot = cell_id;
                self.ui.cursor = CursorKind::Text;
            }

            if !editing && hovered && self.ui.input.double_click {
                // Seeded from the shown value without its unit, so the field
                // opens on what the operator was reading and a value committed
                // unchanged is the value that was already there.
                buffer = value_text(*value, decimals);
                let at = buffer.len();
                self.ui.retained_mut(cell_id).cursor = at;
                self.ui.focus = cell_id;
                self.ui.focus_is_text = true;
                editing = true;
                // Consumed, so a second cell the pointer happens to cover does
                // not open as well.
                self.ui.input.double_click = false;
            } else if editing {
                buffer = self.ui.edit.take().map(|(_, text)| text).unwrap_or_default();
                // A cell being typed into is a stop, so the traversal leaves it
                // for the control after its own slider rather than for the top of
                // the panel. Only while it is live: two stops on every row is one
                // too many.
                self.ui.register_focusable(cell_id);

                // The keys are read only while the focus is still here. Something
                // else took it, most often the traversal, and processing them as
                // well would have one press act on two controls.
                let mine = self.ui.focus == cell_id;
                let outcome = if mine {
                    self.ui.focus_is_text = true;
                    self.edit_keys(cell_id, &mut buffer)
                } else {
                    EditOutcome { changed: false, submitted: false, cancelled: false }
                };
                // A press elsewhere commits, and so does a focus that moved on. An
                // operator who typed a number and left meant the number, and text
                // that is not a number leaves the setting alone, so neither is
                // destructive.
                let commit = outcome.submitted
                    || !mine
                    || (self.ui.input.left_pressed() && !hovered);
                if commit || outcome.cancelled {
                    editing = false;
                    if self.ui.focus == cell_id {
                        self.ui.focus = 0;
                        self.ui.focus_is_text = false;
                    }
                }
                if commit {
                    match parse_value(&buffer, min, max, step) {
                        Some(next) => {
                            if (next - *value).abs() > step * 0.5 {
                                *value = next;
                                changed = true;
                            }
                        }
                        None => crate::log_debug!(
                            "gui",
                            "'{}' is not a value for {}",
                            buffer.trim(),
                            key
                        ),
                    }
                }
            }
        }
        if editing {
            self.ui.edit = Some((cell_id, buffer.clone()));
        } else if self.ui.editing(cell_id) {
            // The cell was live and is no longer, which includes the case of the
            // setting being switched off while a field was open.
            self.ui.edit = None;
        }

        // After the typing rather than before it, so a value committed this frame
        // moves the knob on this frame.
        let fraction = (*value - min) / range;
        let readout = self.format_value(*value, decimals, unit);
        let cell = self.value_cell_width(min, max, decimals, unit);
        let gap = self.ui.m(self.ui.theme.gap);
        let h = self.ui.m(self.ui.theme.row_height);

        self.begin_field(key);
        self.ui.add(
            Style::row().grow(1.0).shrink(1.0).min_w(self.ui.m(32.0)).height_px(h),
            [0.0, h],
            Item::SliderTrack { id, fraction, enabled },
        );
        if entry || editing {
            let text = if editing { buffer.as_str() } else { readout.as_str() };
            let span = self.ui.intern(text);
            let cursor = self.ui.retained_of(cell_id).cursor;
            self.ui.add(
                Style::row().width_px(cell + gap).height_px(h),
                [cell, h],
                Item::Value { text: span, id: cell_id, editing, cursor, enabled },
            );
        } else {
            let color = if enabled { self.ui.theme.text } else { self.ui.theme.text_disabled };
            self.label_styled(
                &readout,
                color,
                FontId::Mono,
                TextAlign::Right,
                Style::row().width_px(cell + gap).height_px(h),
            );
        }
        self.end();
        changed
    }

    /// Integer variant, so counts and sizes do not need a float detour.
    pub fn slider_u32(
        &mut self,
        key: &str,
        value: &mut u32,
        min: u32,
        max: u32,
        unit: &str,
    ) -> bool {
        let mut v = *value as f32;
        if self.slider(key, &mut v, min as f32, max as f32, 0, unit) {
            let rounded = v.round().clamp(min as f32, max as f32) as u32;
            if rounded != *value {
                *value = rounded;
                return true;
            }
        }
        false
    }

    /// Drop down list.
    pub fn combo(&mut self, key: &str, index: &mut usize, items: &[&str]) -> bool {
        if items.is_empty() {
            return false;
        }
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        let mut open = self.ui.retained_of(id).open && enabled;
        let mut changed = false;

        let box_rect = self.ui.prev_rect(id);
        let popup_full = self.ui.popup_rect;
        let popup_seen = self.ui.popup_visible;
        let over_box = enabled
            && self.ui.input.inside
            && !box_rect.is_empty()
            && box_rect.contains(self.ui.input.mouse.0, self.ui.input.mouse.1);

        if open {
            let row = self.ui.m(self.ui.theme.row_height);
            let border = self.ui.line(self.ui.theme.border_px);
            // Containment against what is on screen, so a press on a row that
            // has not appeared yet dismisses the list rather than choosing
            // something unseen. The row itself is measured from the full
            // geometry, which does not move while the list reveals.
            let over_popup = !popup_seen.is_empty()
                && popup_seen.contains(self.ui.input.mouse.0, self.ui.input.mouse.1);

            if over_popup || over_box {
                self.ui.hot = id;
                self.ui.cursor = CursorKind::Hand;
            }
            self.ui.input.consume_wheel();

            if self.ui.input.left_pressed() {
                if over_popup {
                    let row_index =
                        ((self.ui.input.mouse.1 - popup_full.y - border) / row).floor();
                    if row_index >= 0.0 && (row_index as usize) < items.len() {
                        let pick = row_index as usize;
                        if pick != *index {
                            *index = pick;
                            changed = true;
                        }
                    }
                }
                open = false;
                self.ui.input.consume_left();
            }

            if self.ui.input.key_pressed(Key::Escape) {
                open = false;
            }
        } else if over_box && self.ui.input.left_pressed() {
            open = true;
            self.ui.focus = id;
            self.ui.focus_is_text = false;
            self.ui.input.consume_left();
        } else if over_box {
            self.ui.hot = id;
            self.ui.cursor = CursorKind::Hand;
        }

        // Selection moves without opening, which is the whole point of a list
        // being a control rather than a menu: the next device is one press away.
        // No wrap, because a list of a few dozen entries that jumps from the end
        // to the start reads as having lost the place.
        self.ui.register_focusable(id);
        if self.ui.theme.keyboard_focus && self.ui.focus == id && enabled {
            for press in self.ui.input.keys.clone() {
                match press.key {
                    Key::Space | Key::Enter if !press.repeat => open = !open,
                    Key::Down => {
                        if *index + 1 < items.len() {
                            *index += 1;
                            changed = true;
                        }
                    }
                    Key::Up => {
                        if *index > 0 {
                            *index -= 1;
                            changed = true;
                        }
                    }
                    _ => {}
                }
            }
        }

        {
            // A phase left over from a list that stopped being declared belongs
            // to a panel that has since been away, not to a dismissal in
            // progress. Letting it decay would flash a half open list on the
            // frame the panel returns.
            let now = self.ui.time;
            let stale = (self.ui.dt * 2.0).max(0.05);
            let slot = self.ui.retained_mut(id);
            if !open && now - slot.last_seen > stale {
                slot.anim = 0.0;
            }
            slot.last_seen = now;
            slot.open = open;
            slot.seen = true;
        }
        let anim = self.ui.animate(id, if open { 1.0 } else { 0.0 });

        let current = items[(*index).min(items.len() - 1)];
        let span = self.ui.intern_key(current);
        let h = self.ui.m(self.ui.theme.row_height);

        self.begin_field(key);
        self.ui.add(
            Style::row().grow(1.0).shrink(1.0).min_w(self.ui.m(40.0)).height_px(h),
            [0.0, h],
            Item::Combo { text: span, id, open, anim, enabled },
        );
        self.end();

        // Emitted while anything is still moving, which is what gives the
        // dismissal frames something to draw.
        if open || anim > 1e-4 {
            let spans: Vec<Span> = items.iter().map(|s| self.ui.intern_key(s)).collect();
            self.ui.popup = Some(super::Popup {
                owner: id,
                items: spans,
                selected: *index,
                open,
                anim,
            });
        }
        changed
    }

    /// Combo bound to a configuration enum.
    pub fn enum_combo<T: ConfigEnum>(&mut self, key: &str, value: &mut T) -> bool {
        let variants = T::variants();
        if variants.is_empty() {
            return false;
        }
        let current = value.to_config();
        let mut index = variants.iter().position(|v| *v == current).unwrap_or(0);

        let labels: Vec<String> = variants.iter().map(|v| format!("enum.{}", v)).collect();
        let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();

        if self.combo(key, &mut index, &refs) {
            if let Some(next) = T::from_config(variants[index]) {
                *value = next;
                return true;
            }
        }
        false
    }

    /// True when the focused widget was operated from the keyboard.
    ///
    /// Space and enter both, because the two are the same gesture on a control
    /// that holds no text and an operator arriving from either convention presses
    /// one of them. A repeat is ignored: a held key must not flip a switch several
    /// dozen times.
    fn activated(&self, id: u64) -> bool {
        if !self.ui.theme.keyboard_focus || self.ui.focus != id || !self.ui.enabled() {
            return false;
        }
        self.ui
            .input
            .keys
            .iter()
            .any(|p| !p.repeat && matches!(p.key, Key::Space | Key::Enter))
    }

    /// Applies one frame of typing to a buffer.
    ///
    /// Shared by the text field and by the numeric cell, because the two differ
    /// in what they do with the result rather than in how a caret moves: a second
    /// implementation would be a second place for the caret and the drawing to
    /// disagree about where it sits.
    ///
    /// Focus is released on both endings, so a caller does not have to remember
    /// which of the two it is looking at.
    fn edit_keys(&mut self, id: u64, buffer: &mut String) -> EditOutcome {
        let mut out = EditOutcome { changed: false, submitted: false, cancelled: false };
        let mut cursor = self.ui.retained_of(id).cursor.min(buffer.len());

        for ch in self.ui.input.text.clone() {
            if ch >= ' ' {
                buffer.insert(cursor, ch);
                cursor += ch.len_utf8();
                out.changed = true;
            }
        }
        for press in self.ui.input.keys.clone() {
            match press.key {
                Key::Backspace => {
                    if cursor > 0 {
                        let mut prev = cursor - 1;
                        while prev > 0 && !buffer.is_char_boundary(prev) {
                            prev -= 1;
                        }
                        buffer.replace_range(prev..cursor, "");
                        cursor = prev;
                        out.changed = true;
                    }
                }
                Key::Delete => {
                    if cursor < buffer.len() {
                        let mut next = cursor + 1;
                        while next < buffer.len() && !buffer.is_char_boundary(next) {
                            next += 1;
                        }
                        buffer.replace_range(cursor..next, "");
                        out.changed = true;
                    }
                }
                Key::Left => {
                    if cursor > 0 {
                        cursor -= 1;
                        while cursor > 0 && !buffer.is_char_boundary(cursor) {
                            cursor -= 1;
                        }
                    }
                }
                Key::Right => {
                    if cursor < buffer.len() {
                        cursor += 1;
                        while cursor < buffer.len() && !buffer.is_char_boundary(cursor) {
                            cursor += 1;
                        }
                    }
                }
                Key::Home => cursor = 0,
                Key::End => cursor = buffer.len(),
                Key::Enter => out.submitted = true,
                Key::Escape => out.cancelled = true,
                _ => {}
            }
        }

        self.ui.retained_mut(id).cursor = cursor;
        if out.submitted || out.cancelled {
            self.ui.focus = 0;
            self.ui.focus_is_text = false;
        }
        out
    }

    /// Single line text field.
    ///
    /// Reports whether the buffer changed.
    pub fn text_edit(&mut self, key: &str, buffer: &mut String) -> bool {
        self.text_field(key, buffer).changed
    }

    /// Single line field that reports the enter key.
    ///
    /// Held apart from the plain field because the two mean different things to
    /// the caller. A path is applied as it is typed and a frequency is not: a
    /// receiver that retuned on every keystroke would visit four wrong
    /// frequencies on the way to the right one, and the last of them would be
    /// whatever the operator happened to have typed when they stopped.
    pub fn text_submit(&mut self, key: &str, buffer: &mut String) -> bool {
        self.text_field(key, buffer).submitted
    }

    fn text_field(&mut self, key: &str, buffer: &mut String) -> FieldResult {
        let id = self.ui.id(key);
        let enabled = self.ui.enabled();
        self.ui.register_focusable(id);
        let rect = self.ui.prev_rect(id);
        let hovered = self.ui.hovered(id, rect);
        if hovered {
            self.ui.hot = id;
            self.ui.cursor = CursorKind::Text;
        }
        if hovered && self.ui.input.left_pressed() {
            self.ui.focus = id;
            self.ui.focus_is_text = true;
            let len = buffer.len();
            self.ui.retained_mut(id).cursor = len;
        } else if !hovered && self.ui.input.left_pressed() && self.ui.focus == id {
            self.ui.focus = 0;
            self.ui.focus_is_text = false;
        }

        let mut changed = false;
        let mut submitted = false;
        if enabled && self.ui.focus == id {
            // Stated on every frame rather than only on the click that took the
            // focus, because the traversal can hand it over as well and a field
            // reached by tab has to suppress the shortcuts just the same.
            self.ui.focus_is_text = true;
            let outcome = self.edit_keys(id, buffer);
            changed = outcome.changed;
            submitted = outcome.submitted;
        }
        let cursor = self.ui.retained_of(id).cursor.min(buffer.len());

        let span = self.ui.intern(buffer);
        let h = self.ui.m(self.ui.theme.row_height);
        self.begin_field(key);
        self.ui.add(
            Style::row().grow(1.0).shrink(1.0).min_w(self.ui.m(40.0)).height_px(h),
            [0.0, h],
            Item::TextEdit { text: span, id, cursor, enabled },
        );
        self.end();
        FieldResult { changed, submitted }
    }

    /// Drag handle between two areas.
    pub fn splitter(&mut self, name: &str, vertical: bool) -> f32 {
        let id = self.ui.id(name);
        let rect = self.ui.prev_rect(id);
        let hovered = self.ui.hovered(id, rect);
        let cursor_kind = if vertical {
            CursorKind::ResizeHorizontal
        } else {
            CursorKind::ResizeVertical
        };
        if hovered {
            self.ui.hot = id;
            self.ui.cursor = cursor_kind;
        }

        let mut delta = 0.0f32;
        if self.ui.active == id {
            self.ui.cursor = cursor_kind;
            if self.ui.input.left_down() {
                delta = if vertical { self.ui.input.delta.0 } else { self.ui.input.delta.1 };
            } else {
                self.ui.active = 0;
            }
        } else if hovered && self.ui.input.left_pressed() {
            self.ui.active = id;
        }

        let thickness = self.ui.m(self.ui.theme.splitter);
        let style = if vertical {
            Style::row().width_px(thickness)
        } else {
            Style::row().height_px(thickness)
        };
        self.ui.add(style, [thickness, thickness], Item::Splitter { id, vertical });
        delta
    }

    /// Value shown as text without a control, for read only readouts.
    ///
    /// Deliberately outside the numeric column: a readout carries free text
    /// such as a device name, and letting it set the column width would take
    /// the track width away from every slider in the group.
    pub fn readout(&mut self, key: &str, value: &str) {
        let h = self.ui.m(self.ui.theme.row_height);
        self.begin_field(key);
        let color = self.ui.theme.text;
        self.label_styled(
            value,
            color,
            FontId::Mono,
            TextAlign::Left,
            Style::row().grow(1.0).shrink(1.0).height_px(h),
        );
        self.end();
    }
}