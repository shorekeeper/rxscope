//! Widget system.
//!
//! The interface is declared every frame into a node arena, solved by the
//! flexbox engine and then drawn. State that cannot be derived from the
//! declaration, such as which control has focus, whether a group is folded, how
//! far a list is scrolled or where a slider was grabbed, lives in a retained map
//! keyed by widget identity.
//!
//! Hit testing uses the rectangle a widget occupied on the previous frame.
//! The alternative, solving the layout before interaction, would need two
//! layout passes per frame for no visible gain: a rectangle only changes when
//! the window is resized, and one frame of lag on a click is not perceptible.
//! The column widths are carried across frames for the same reason and with
//! the same justification.
//!
//! Identity is a hash of the widget key mixed with the enclosing scope, so the
//! same key may appear in several panels without colliding. The key is a stable
//! identifier and never the displayed text: were the two the same, changing
//! language would reset focus, folding and scroll position across the interface.
//!
//! Two things are drawn outside the tree because the layout model has no
//! absolute positioning: the open combo list and the diagnostic overlay. Both
//! are anchored to a rectangle that is already known when they are drawn.
//!
//! ## Width the wording needs
//!
//! Nothing in the layout model can grow a fixed width container to fit its
//! content, and the side panel is exactly that: its width is an operator
//! decision carried across sessions. So the panel is measured instead. Every
//! labelled row records the width of its caption and of its numeric readout,
//! and the widest row of the frame is published as a demand the application
//! uses as the floor of the panel width.
//!
//! That is the only arrangement that survives a translation. A ceiling stated
//! in logical units is a bet on one wording, and the wording is what changes.

pub mod input;
pub mod layout;
pub mod theme;
pub mod widgets;

use std::collections::HashMap;

use crate::font::{FontId, FontSystem};
use crate::i18n::Catalog;
use crate::platform::{CursorKind, Key, Modifiers, MouseButton};
use crate::render::{Color, DrawList, Rect};

use input::InputState;
use layout::{LayoutNode, Style};
use theme::Theme;

/// Text placement inside the node rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Visual variant of a clickable box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Normal,
    /// Tab strip entry.
    Tab,
    /// Flat entry without a border, used inside dense rows.
    Flat,
}

/// Command carried by a window chrome button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowButton {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Pointer state reported for a drag over an application drawn area.
#[derive(Debug, Clone, Copy)]
pub struct Drag {
    /// Position inside the area, normalized to its rectangle.
    pub x: f32,
    pub y: f32,
    /// True only on the frame the drag began.
    ///
    /// A gesture that grabs something has to decide what it grabbed at the
    /// moment of the press and hold that decision for the rest of the drag.
    /// Recomputing it per frame lets the target change underneath the pointer,
    /// which is what turns a grab into a jump.
    pub started: bool,
    /// Modifiers as of the press, held for the whole gesture.
    pub mods: Modifiers,
}

/// Pointer state over a rectangle the application draws in the top layer.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    /// Position inside the rectangle, in pixels from its top left corner.
    pub x: f32,
    pub y: f32,
    pub wheel: f32,
    pub left: bool,
    pub right: bool,
    /// True while the right button is held, press edge or not.
    ///
    /// Held apart from the edge because a repeat is a property of the hold and a
    /// step is a property of the press, and one field cannot say both.
    pub right_down: bool,
}

/// Result of a press on the caption strip.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptionHit {
    /// Begin a window move.
    pub drag: bool,
    /// Toggle the maximized state.
    pub double: bool,
}

/// Slice of the per frame string arena.
#[derive(Debug, Clone, Copy, Default)]
struct Span {
    start: u32,
    len: u32,
}

/// Drawing payload of a node. Plain containers carry Item::None.
enum Item {
    None,
    Frame { fill: Color, border: Color, border_px: f32 },
    /// Title strip. A collapsible header is also a click target, so it owns an
    /// identity and reports its folded state.
    Header { text: Span, id: u64, collapsible: bool, open: bool, active: bool },
    /// Numeric cell of a slider.
    ///
    /// Its own item rather than a label, because it has to carry an identity: a
    /// cell that can be typed into has to be hit tested, and a label records no
    /// rectangle.
    Value { text: Span, id: u64, editing: bool, cursor: usize, enabled: bool },
    Label { text: Span, color: Color, font: FontId, size: f32, align: TextAlign },
    Button { text: Span, id: u64, kind: ButtonKind, selected: bool, enabled: bool },
    /// Selection bar of a tab strip. Carries its animated geometry, so the
    /// drawing pass needs no access to the retained map, and is drawn after the
    /// children rather than before them.
    TabBar { id: u64, x: f32, w: f32 },
    Checkbox { text: Span, id: u64, checked: bool, enabled: bool },
    /// Sliding switch. The animation phase is captured at declaration time so
    /// the drawing pass needs no access to the retained map.
    Toggle { id: u64, on: bool, anim: f32, enabled: bool },
    SliderTrack { id: u64, fraction: f32, enabled: bool },
    Combo { text: Span, id: u64, open: bool, anim: f32, enabled: bool },
    TextEdit { text: Span, id: u64, cursor: usize, enabled: bool },
    Splitter { id: u64, vertical: bool },
    Separator { vertical: bool },
    /// Draggable region of the caption strip. Draws nothing.
    Caption { id: u64 },
    WindowChrome { id: u64, kind: WindowButton },
    /// Reserved area drawn by the application after the widget pass.
    Custom { tag: u32 },
}

/// Retained per widget state.
#[derive(Debug, Clone, Copy)]
struct Retained {
    scroll: f32,
    /// Main axis extent of the content, written back after the layout pass.
    content: f32,
    /// Caret position in bytes for a text field.
    cursor: usize,
    /// Open combo list or unfolded group.
    open: bool,
    /// False until the widget has been seen once, so a group can default to
    /// unfolded and a switch can start at its final phase without the caller
    /// having to seed the map.
    seen: bool,
    /// Animation phase, zero to one.
    anim: f32,
    /// Value a slider held when it was grabbed, and the pointer position at
    /// that moment. Dragging is relative to these rather than absolute, which
    /// is what keeps a control stable when its own geometry moves underneath it.
    grab: f32,
    grab_x: f32,
    /// Value units per pixel of pointer travel, frozen at grab time.
    units_px: f32,
    /// Tab selection bar, as an offset inside the strip and a width.
    bar_x: f32,
    bar_w: f32,
    /// Time the widget was last declared.
    ///
    /// A list dismissed by a click animates out over the following frames, which
    /// needs the phase to survive the close. A list abandoned because the panel
    /// holding it stopped being declared must not: the phase would sit where it
    /// stopped and flash a half open list on the frame the panel returns. The two
    /// are told apart by whether the widget was declared recently.
    last_seen: f32,
}

impl Default for Retained {
    fn default() -> Retained {
        Retained {
            scroll: 0.0,
            content: 0.0,
            cursor: 0,
            open: false,
            seen: false,
            anim: 0.0,
            grab: 0.0,
            grab_x: 0.0,
            units_px: 0.0,
            bar_x: 0.0,
            bar_w: 0.0,
            last_seen: 0.0,
        }
    }
}

/// Widths of the two aligned columns of one group.
///
/// Every labelled row in a group has to share both boundaries, otherwise the
/// controls between them form a ragged edge on the left and the numbers form
/// one on the right. Neither width can be known while the first row is being
/// declared, so both are measured over a frame and used on the next one.
/// Growth is applied at once so a newly unfolded group is correct immediately;
/// shrinking waits for the end of the frame, which is what stops a column from
/// oscillating when a row is alternately declared and skipped.
#[derive(Debug, Clone, Copy, Default)]
struct Columns {
    label_published: f32,
    label_pending: f32,
    value_published: f32,
    value_pending: f32,
}

/// Combo box list, drawn on top of the tree.
///
/// Present while the list is open and while it is still revealing or hiding, so
/// the closing frames have something to draw. The flag is what separates the
/// two: a closing list is visible and is not modal, because a click that
/// dismissed it must not be eaten for the duration of the animation.
struct Popup {
    owner: u64,
    items: Vec<Span>,
    selected: usize,
    open: bool,
    /// Reveal, nought to one, already eased.
    anim: f32,
}

pub struct Ui {
    pub theme: Theme,
    catalog: Catalog,

    nodes: Vec<LayoutNode>,
    items: Vec<Item>,
    strings: String,
    stack: Vec<usize>,
    root: usize,

    /// Rectangles of interactive widgets from the previous frame.
    rects: HashMap<u64, Rect>,
    retained: HashMap<u64, Retained>,
    /// Column widths per identity scope.
    columns: HashMap<u64, Columns>,
    /// Nodes that need their scroll extent written back after layout.
    scroll_nodes: Vec<(usize, u64)>,
    /// Rectangles of application drawn areas, filled after layout.
    customs: Vec<(u32, Rect)>,

    input: InputState,
    scopes: Vec<u64>,

    hot: u64,
    active: u64,
    focus: u64,
    /// True while the focused widget consumes character input.
    focus_is_text: bool,
    /// Focusable identities of the previous frame, in declaration order.
    ///
    /// Declaration order is reading order, so the traversal follows the panel
    /// down the way an operator reads it. Held across one frame for the reason the
    /// hit test rectangles are: a second layout pass per frame would buy nothing
    /// anybody can perceive.
    focus_order: Vec<u64>,
    /// Nesting depth of the enabling scopes, counted whether or not they
    /// disable anything.
    scope_depth: u32,
    /// Depth the innermost live disabling scope was opened at, if any.
    disabled_from: Option<u32>,

    popup: Option<Popup>,
    /// Full popup geometry from the previous frame.
    ///
    /// The row arithmetic is measured from it rather than from what is on screen:
    /// a list revealing upwards moves its top edge every frame, and an index
    /// computed from a moving origin names a different row each time.
    popup_rect: Rect,
    /// Part of it actually drawn.
    ///
    /// The containment test uses this, so a press on a row that has not appeared
    /// yet falls outside the list and dismisses it rather than selecting
    /// something the operator has not seen.
    popup_visible: Rect,
    /// Identity of the control that owns the open list.
    popup_owner: u64,
    /// Application drawn area that captured a drag, with the button that owns
    /// it. Held across frames because a drag outlives the press that started it.
    drag_owner: Option<(u32, MouseButton)>,
    /// True on the frame the current custom drag began.
    drag_started: bool,
    /// Modifiers latched when that drag began.
    drag_mods: Modifiers,
    /// Diagnostic lines, drawn over everything when not empty.
    overlay: Vec<String>,
    /// Cell being typed into, and what has been typed.
    ///
    /// One at a time by construction: opening a second cell commits the first, so
    /// a pair could never both be live. Held apart from the retained map because
    /// it is the only piece of widget state that is a string, and giving every
    /// widget one would allocate a few hundred of them per session for nothing.
    edit: Option<(u64, String)>,
    /// Node holding the header of the group being declared.
    ///
    /// Recorded so a section can mark itself active after its own header exists.
    /// The alternative is a flag passed into the header, which every section
    /// would have to compute before it knows what it contains.
    last_header: usize,

    /// Width one settings row has for its label, its control and its value.
    ///
    /// Pushed in by the application before the declaration, because the panel
    /// width is an operator decision the widget system does not own.
    field_px: f32,
    /// Widest row of the previous frame, label column plus value column.
    demand: f32,
    /// Scratch for the wrapped hint lines, so a panel of a few dozen hints does
    /// not allocate per frame.
    hint_text: String,
    wrap: Vec<(usize, usize)>,

    scale: f32,
    ui_px: f32,
    mono_px: f32,
    time: f32,
    /// Seconds since the previous frame, derived from the time stamp so the
    /// caller does not have to pass both.
    dt: f32,
    cursor: CursorKind,
    viewport: Rect,
}

impl Ui {
    pub fn new(theme: Theme) -> Ui {
        Ui {
            theme,
            catalog: crate::i18n::current(),
            nodes: Vec::with_capacity(512),
            items: Vec::with_capacity(512),
            strings: String::with_capacity(8192),
            stack: Vec::with_capacity(16),
            root: 0,
            rects: HashMap::with_capacity(256),
            retained: HashMap::with_capacity(128),
            columns: HashMap::with_capacity(32),
            scroll_nodes: Vec::with_capacity(4),
            customs: Vec::with_capacity(8),
            input: InputState::new(),
            scopes: Vec::with_capacity(8),
            hot: 0,
            active: 0,
            focus: 0,
            focus_is_text: false,
            focus_order: Vec::with_capacity(256),
            scope_depth: 0,
            disabled_from: None,
            popup: None,
            popup_rect: Rect::default(),
            popup_visible: Rect::default(),
            popup_owner: 0,
            drag_owner: None,
            drag_started: false,
            drag_mods: Modifiers::default(),
            overlay: Vec::new(),
            edit: None,
            last_header: usize::MAX,
            field_px: 0.0,
            demand: 0.0,
            hint_text: String::with_capacity(256),
            wrap: Vec::with_capacity(8),
            scale: 1.0,
            ui_px: 12.0,
            mono_px: 13.0,
            time: 0.0,
            dt: 0.0,
            cursor: CursorKind::Arrow,
            viewport: Rect::default(),
        }
    }

    /// Replaces the translation catalogue. Safe at any point between frames.
    pub fn set_catalog(&mut self, catalog: Catalog) {
        self.catalog = catalog;
        // Column widths were measured from the previous wording and would hold a
        // stale boundary for one frame; discarding them costs one frame of
        // ragged layout instead.
        self.columns.clear();
    }

    pub fn language(&self) -> &str {
        self.catalog.language()
    }

    /// Resolves a key for display. An unknown key returns itself, which is what
    /// lets a run time string be passed through unchanged.
    pub fn tr<'a>(&'a self, key: &'a str) -> &'a str {
        self.catalog.get(key)
    }

    pub fn on_event(&mut self, ev: &crate::platform::Event) {
        self.input.on_event(ev);
    }

    /// True while a text field has focus, so global shortcuts stay quiet.
    ///
    /// Character input only. A focused slider does not consume a digit, so the
    /// shortcuts that select a tab stay reachable while one is being adjusted.
    pub fn wants_keyboard(&self) -> bool {
        self.focus != 0 && self.focus_is_text
    }

    /// True while any widget holds focus.
    ///
    /// Asked by the shell about space and enter, which a focused control operates
    /// whether or not it holds text. Held apart from the predicate above because
    /// the two answer different questions and one field cannot say both.
    pub fn wants_activation(&self) -> bool {
        self.focus != 0
    }

    /// True while the pointer sits over something that would act on a press.
    ///
    /// Read by an input layer that wants the mouse buttons for itself. Answering
    /// with the whole window would take the close button and the transport with
    /// it; answering with one region would fail whenever the pointer drifted out
    /// of it. The boundary that works is the control: over one, the press belongs
    /// to the interface, and over anything else it does not.
    ///
    /// The value is the one the previous declaration left, which is the same lag
    /// the hit test rectangles carry and for the same reason. It is exact in the
    /// case that matters, because a pointer nobody is moving is a pointer that
    /// was in the same place when the frame was built.
    pub fn pointer_over_widget(&self) -> bool {
        self.hot != 0
    }

    /// True while a list is open.
    pub fn popup_open(&self) -> bool {
        self.popup_owner != 0
    }

    /// True while the window holds keyboard focus.
    ///
    /// Read by the application to colour the window edge. With the system
    /// caption gone that edge is the only thing left that states which window
    /// is active, and an operator with two receivers open needs it stated.
    pub fn window_focused(&self) -> bool {
        self.input.window_focused
    }

    pub fn cursor(&self) -> CursorKind {
        self.cursor
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Keyboard modifiers as of the current frame.
    pub fn mods(&self) -> Modifiers {
        self.input.mods
    }

    pub fn viewport(&self) -> Rect {
        self.viewport
    }

    /// Scales a logical metric and snaps it to a whole pixel.
    pub fn m(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    /// Same as m but never collapses to zero, for borders and hairlines.
    pub fn line(&self, v: f32) -> f32 {
        (v * self.scale).round().max(1.0)
    }

    /// Replaces the diagnostic block for this frame.
    pub fn set_overlay(&mut self, lines: Vec<String>) {
        self.overlay = lines;
    }

    // ------------------------------------------------------- panel geometry

    /// Everything a settings row loses to the chrome around it, in pixels.
    ///
    /// Derived from the same metrics the containers are built from rather than
    /// stated as a constant, so a change to the density does not leave the
    /// measurement describing a layout that no longer exists.
    ///
    /// The scroll indicator overlays the right edge and the container reserves
    /// the larger of the margin and the indicator, not their sum, which is why
    /// the two sides are not symmetric here.
    fn panel_chrome(&self) -> f32 {
        let inset = self.m(self.theme.panel_inset) * 2.0;
        let margin = self.m(self.theme.panel_margin);
        let reserved = self.m(self.theme.scrollbar) + self.m(self.theme.scrollbar_gap);
        let border = self.line(self.theme.border_px) * 2.0;
        let padding = self.m(self.theme.group_padding) * 2.0;
        inset + margin + margin.max(reserved) + border + padding
    }

    /// States how wide the side panel is, in logical units.
    ///
    /// Called before the declaration. Without it the column ceilings would be
    /// computed against a guess, and a narrow panel would keep a label column
    /// that leaves the control no room.
    pub fn set_panel_width(&mut self, logical: f32) {
        let px = self.m(logical) - self.panel_chrome();
        // A floor rather than a clamp against the request: a panel narrower than
        // this cannot hold a row at all, and reporting a negative width would
        // invert every ceiling derived from it.
        self.field_px = px.max(self.m(80.0));
    }

    /// Width one settings row has for its label, its control and its value.
    pub fn field_width(&self) -> f32 {
        if self.field_px > 0.0 {
            self.field_px
        } else {
            (self.m(320.0) - self.panel_chrome()).max(self.m(80.0))
        }
    }

    /// Panel width the current wording needs, in logical units.
    ///
    /// Measured on the previous frame, which is the same lag the hit test
    /// rectangles carry and for the same reason: a second layout pass per frame
    /// would buy nothing an operator can perceive.
    ///
    /// Zero before anything has been declared, so the caller falls back to its
    /// own floor rather than collapsing the panel.
    pub fn panel_demand(&self) -> f32 {
        if self.demand <= 0.0 {
            return 0.0;
        }
        let px = self.demand
            + self.m(self.theme.control_min)
            + self.m(self.theme.gap) * 2.0
            + self.panel_chrome();
        px / self.scale.max(0.1)
    }

    /// Drops whatever the pointer was doing.
    ///
    /// Needed after an action that hands the pointer to the system, such as a
    /// window move: the modal loop consumes the release, so without this the
    /// held state would survive into the next frame and a widget would see a
    /// press that has already ended.
    pub fn release_pointer(&mut self) {
        self.input.consume_left();
        self.active = 0;
        self.drag_owner = None;
    }

    pub fn starts_frame(&mut self, viewport: Rect, scale: f32, ui_px: f32, mono_px: f32, time: f32) {
        self.viewport = viewport;
        self.scale = scale;
        self.ui_px = ui_px;
        self.mono_px = mono_px;
        // The step is clamped so a stall does not snap an animation to its end.
        self.dt = (time - self.time).clamp(0.0, 0.1);
        self.time = time;
        self.cursor = CursorKind::Arrow;
        self.hot = 0;
        self.scope_depth = 0;
        self.disabled_from = None;

        // Before the declaration, so a move that lands this frame is drawn with
        // the outline in its new place rather than one frame behind it. Reads the
        // order of the previous frame, which is then discarded.
        self.advance_focus();
        self.focus_order.clear();

        self.nodes.clear();
        self.items.clear();
        self.strings.clear();
        self.stack.clear();
        self.scopes.clear();
        self.scroll_nodes.clear();
        self.popup = None;
        self.overlay.clear();
        // The index refers to a node list that has just been cleared. The buffer
        // beside it is not reset: a cell being typed into survives the frame.
        self.last_header = usize::MAX;

        // The root fills the viewport and stacks its children vertically.
        let style = Style::column().width_px(viewport.w).height_px(viewport.h);
        self.nodes.push(LayoutNode::new(style, None, [viewport.w, viewport.h]));
        self.items.push(Item::None);
        self.root = 0;
        self.stack.push(0);
    }

    /// Solves the layout and refreshes the caches the next frame reads.
    pub fn ends_frame(&mut self) {
        if self.stack.len() != 1 {
            crate::log_warn!("gui", "container stack depth {} at frame end", self.stack.len());
            self.stack.truncate(1);
        }
        if self.scope_depth != 0 {
            crate::log_warn!("gui", "{} disabled scopes left open", self.scope_depth);
            self.scope_depth = 0;
            self.disabled_from = None;
        }

        layout::solve(&mut self.nodes, self.root, self.viewport);

        // Interactive rectangles for the next frame.
        self.rects.clear();
        for i in 0..self.nodes.len() {
            let rect = self.nodes[i].rect;
            let id = Self::item_id(&self.items[i]);
            if id != 0 {
                self.rects.insert(id, rect);
            }
        }

        // Scroll extents, clamped so a shrinking list cannot leave the view
        // parked past the end of its content.
        for &(node, id) in &self.scroll_nodes {
            let view = self.nodes[node].rect.h;
            let content = self.nodes[node].content_main;
            let entry = self.retained.entry(id).or_default();
            entry.content = content;
            let max = (content - view).max(0.0);
            entry.scroll = entry.scroll.clamp(0.0, max);
            self.rects.insert(id, self.nodes[node].rect);
        }

        // Columns. Publishing at the end rather than on demand is what makes
        // shrinking wait a frame. An entry that saw nothing belongs to a scope
        // that is no longer declared and is dropped so the map stays bounded.
        //
        // The widest row of any group is published as the panel demand. The
        // pending values are the measured text rather than the laid out column,
        // so the demand does not depend on the width it is about to influence
        // and the two cannot oscillate.
        let mut demand = 0.0f32;
        self.columns.retain(|_, column| {
            let row = column.label_pending + column.value_pending;
            if row > demand {
                demand = row;
            }
            column.label_published = column.label_pending;
            column.value_published = column.value_pending;
            column.label_pending = 0.0;
            column.value_pending = 0.0;
            column.label_published > 0.0 || column.value_published > 0.0
        });
        self.demand = demand;

        self.customs.clear();
        for i in 0..self.nodes.len() {
            if let Item::Custom { tag } = self.items[i] {
                self.customs.push((tag, self.nodes[i].rect));
            }
        }

        // A widget that stopped being declared, because its group was folded or
        // its tab left, cannot be seen and cannot be reached. Keeping the focus on
        // it would send every key to nothing.
        if self.focus != 0 && !self.focus_order.contains(&self.focus) {
            self.focus = 0;
            self.focus_is_text = false;
        }

        self.input.end_frame();
    }

    fn item_id(item: &Item) -> u64 {
        match item {
            Item::Header { id, .. } => *id,
            Item::Value { id, .. } => *id,
            Item::Button { id, .. } => *id,
            Item::TabBar { id, .. } => *id,
            Item::Checkbox { id, .. } => *id,
            Item::Toggle { id, .. } => *id,
            Item::SliderTrack { id, .. } => *id,
            Item::Combo { id, .. } => *id,
            Item::TextEdit { id, .. } => *id,
            Item::Splitter { id, .. } => *id,
            Item::Caption { id } => *id,
            Item::WindowChrome { id, .. } => *id,
            _ => 0,
        }
    }

    /// Rectangle of an application drawn area.
    pub fn custom_rect(&self, tag: u32) -> Option<Rect> {
        self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)
    }

    /// Pointer position over an application drawn area, normalized.
    pub fn custom_hover(&self, tag: u32) -> Option<(f32, f32)> {
        if self.popup_owner != 0 || !self.input.inside {
            return None;
        }
        let rect = self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)?;
        if rect.is_empty() || !rect.contains(self.input.mouse.0, self.input.mouse.1) {
            return None;
        }
        let x = ((self.input.mouse.0 - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let y = ((self.input.mouse.1 - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
        Some((x, y))
    }

    /// Double click inside an application drawn area.
    ///
    /// Read before a drag, because the second press of a pair arrives as both
    /// and the more specific reading has to win: an area that acted on the drag
    /// first would never see the double.
    pub fn custom_double_click(&mut self, tag: u32) -> Option<(f32, f32)> {
        if !self.input.double_click || self.popup_owner != 0 || !self.input.inside {
            return None;
        }
        let rect = self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)?;
        if rect.is_empty() || !rect.contains(self.input.mouse.0, self.input.mouse.1) {
            return None;
        }
        let x = ((self.input.mouse.0 - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let y = ((self.input.mouse.1 - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
        // Both are consumed: the press that produced the double must not also
        // begin a drag, which would move a cursor on the frame it was cleared.
        self.input.consume_left();
        Some((x, y))
    }

    /// Click inside an application drawn area, normalized to its rectangle.
    pub fn custom_click(&mut self, tag: u32) -> Option<(f32, f32)> {
        if !self.input.left_pressed() || !self.pointer_free(0) {
            return None;
        }
        let rect = self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)?;
        if rect.is_empty() || !rect.contains(self.input.mouse.0, self.input.mouse.1) {
            return None;
        }
        let x = ((self.input.mouse.0 - rect.x) / rect.w).clamp(0.0, 1.0);
        let y = ((self.input.mouse.1 - rect.y) / rect.h).clamp(0.0, 1.0);
        self.input.consume_left();
        Some((x, y))
    }

    /// Pointer position inside an application drawn area while a button is
    /// held, normalized to the rectangle.
    pub fn custom_drag(&mut self, tag: u32, button: MouseButton) -> Option<Drag> {
        if let Some((owner_tag, owner_button)) = self.drag_owner {
            if owner_tag != tag || owner_button != button {
                return None;
            }
        }
        let rect = self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)?;
        if rect.is_empty() {
            return None;
        }

        let started = if self.drag_owner.is_some() {
            if !self.input.button_down(button) {
                self.drag_owner = None;
                self.drag_started = false;
                return None;
            }
            std::mem::take(&mut self.drag_started)
        } else {
            if self.popup_owner != 0 || !self.input.button_pressed(button) {
                return None;
            }
            if !self.input.inside || !rect.contains(self.input.mouse.0, self.input.mouse.1) {
                return None;
            }
            self.drag_owner = Some((tag, button));
            self.drag_mods = self.input.mods;
            true
        };

        self.cursor = CursorKind::ResizeHorizontal;
        let x = ((self.input.mouse.0 - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let y = ((self.input.mouse.1 - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
        Some(Drag { x, y, started, mods: self.drag_mods })
    }

    /// Wheel movement over an application drawn area.
    pub fn custom_wheel(&mut self, tag: u32) -> Option<(f32, f32)> {
        if self.popup_owner != 0 || !self.input.inside || self.input.wheel == 0.0 {
            return None;
        }
        let rect = self.customs.iter().find(|&&(t, _)| t == tag).map(|&(_, r)| r)?;
        if rect.is_empty() || !rect.contains(self.input.mouse.0, self.input.mouse.1) {
            return None;
        }
        let x = ((self.input.mouse.0 - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let notches = self.input.wheel;
        self.input.consume_wheel();
        Some((x, notches))
    }

    /// Reads the pointer over a rectangle the application owns.
    pub fn probe(&mut self, rect: Rect) -> Option<Probe> {
        if self.popup_owner != 0 || !self.input.inside || rect.is_empty() {
            return None;
        }
        if !rect.contains(self.input.mouse.0, self.input.mouse.1) {
            return None;
        }

        let out = Probe {
            x: self.input.mouse.0 - rect.x,
            y: self.input.mouse.1 - rect.y,
            wheel: self.input.wheel,
            left: self.input.left_pressed(),
            right: self.input.button_pressed(MouseButton::Right),
            right_down: self.input.button_down(MouseButton::Right),
        };

        self.cursor = CursorKind::Hand;
        self.input.consume_wheel();
        self.input.consume_press(MouseButton::Left);
        self.input.consume_press(MouseButton::Right);
        Some(out)
    }

    // ------------------------------------------------------------- identity

    fn scope(&self) -> u64 {
        *self.scopes.last().unwrap_or(&0xcbf2_9ce4_8422_2325)
    }

    fn id(&self, key: &str) -> u64 {
        mix(self.scope(), fnv(key))
    }

    fn push_scope(&mut self, name: &str) {
        let s = mix(self.scope(), fnv(name));
        self.scopes.push(s);
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Opens a named identity scope.
    pub fn begin_scope(&mut self, name: &str) {
        self.push_scope(name);
    }

    pub fn end_scope(&mut self) {
        self.pop_scope();
    }

    /// Records a label width and returns the column width to lay out with.
    fn label_width(&mut self, measured: f32) -> f32 {
        let scope = self.scope();
        let entry = self.columns.entry(scope).or_default();
        if measured > entry.label_pending {
            entry.label_pending = measured;
        }
        if entry.label_pending > entry.label_published {
            entry.label_published = entry.label_pending;
        }
        entry.label_published
    }

    /// Records a numeric width and returns the column width to lay out with.
    fn value_width(&mut self, measured: f32) -> f32 {
        let scope = self.scope();
        let entry = self.columns.entry(scope).or_default();
        if measured > entry.value_pending {
            entry.value_pending = measured;
        }
        if entry.value_pending > entry.value_published {
            entry.value_published = entry.value_pending;
        }
        entry.value_published
    }

    /// Width the label column of a settings row is laid out at.
    ///
    /// The ceiling is a share of the row and never leaves the control less than
    /// its own floor. A long caption therefore narrows the track rather than
    /// erasing it, and the panel widens on the next frame to cover what the
    /// share could not.
    fn label_column_width(&mut self, measured: f32) -> f32 {
        let available = self.field_width();
        let floor = self.m(self.theme.label_min);
        let control = self.m(self.theme.control_min);
        let cap = (available * self.theme.label_fraction)
            .min(available - control)
            .max(floor);
        self.label_width(measured).clamp(floor, cap)
    }

    /// Width the numeric column of a settings row is laid out at.
    fn value_column_width(&mut self, measured: f32) -> f32 {
        let available = self.field_width();
        let floor = self.m(self.theme.value_min);
        let cap = (available * self.theme.value_fraction).max(floor);
        self.value_width(measured).clamp(floor, cap)
    }

    // ------------------------------------------------------------- enabling

    /// Opens a subtree that does not accept input.
    pub fn begin_disabled(&mut self, disabled: bool) {
        self.scope_depth += 1;
        if disabled && self.disabled_from.is_none() {
            self.disabled_from = Some(self.scope_depth);
        }
    }

    pub fn end_disabled(&mut self) {
        if self.disabled_from == Some(self.scope_depth) {
            self.disabled_from = None;
        }
        self.scope_depth = self.scope_depth.saturating_sub(1);
    }

    fn enabled(&self) -> bool {
        self.disabled_from.is_none()
    }

    // ------------------------------------------------------------- building

    fn intern(&mut self, text: &str) -> Span {
        let start = self.strings.len() as u32;
        self.strings.push_str(text);
        Span { start, len: self.strings.len() as u32 - start }
    }

    /// Resolves a key and stores the result in the frame arena.
    fn intern_key(&mut self, key: &str) -> Span {
        let start = self.strings.len() as u32;
        let text = self.catalog.get(key);
        self.strings.push_str(text);
        Span { start, len: self.strings.len() as u32 - start }
    }

    fn span_text(strings: &str, span: Span) -> &str {
        let a = span.start as usize;
        let b = a + span.len as usize;
        if b <= strings.len() {
            &strings[a..b]
        } else {
            ""
        }
    }

    fn add(&mut self, style: Style, content: [f32; 2], item: Item) -> usize {
        let parent = *self.stack.last().unwrap_or(&self.root);
        let index = self.nodes.len();
        self.nodes.push(LayoutNode::new(style, Some(parent), content));
        self.items.push(item);
        self.nodes[parent].children.push(index);
        index
    }

    fn push(&mut self, style: Style, item: Item) -> usize {
        let index = self.add(style, [0.0, 0.0], item);
        self.stack.push(index);
        index
    }

    fn pop(&mut self) {
        if self.stack.len() > 1 {
            self.stack.pop();
        } else {
            crate::log_warn!("gui", "container end without a matching begin");
        }
    }

    /// Full width hairline between two stacked areas of the root column.
    pub fn add_separator_row(&mut self) {
        let h = self.line(self.theme.border_px);
        let color = self.theme.border_strong;
        self.add(
            Style::row().height_px(h),
            [0.0, h],
            Item::Frame { fill: color, border: Color::TRANSPARENT, border_px: 0.0 },
        );
    }

    // ---------------------------------------------------------- interaction

    /// Records a widget as a stop in the traversal.
    ///
    /// Called during declaration whatever the switch says, because the order is
    /// what tells a later pass whether the focused widget still exists. Only the
    /// traversal itself is optional; a stale focus is a defect in either case.
    ///
    /// A disabled widget is not a stop. A control that refuses input is a place
    /// the operator has to press past for nothing.
    fn register_focusable(&mut self, id: u64) {
        if self.enabled() {
            self.focus_order.push(id);
        }
    }

    /// Moves the focus on a tab press.
    ///
    /// Nothing happens while a list is open: a list is modal, so a move would put
    /// the focus behind something that owns every press.
    fn advance_focus(&mut self) {
        if !self.theme.keyboard_focus || self.focus_order.is_empty() || self.popup_owner != 0 {
            return;
        }

        let mut step = 0i32;
        for press in &self.input.keys {
            if press.key == Key::Tab {
                step += if self.input.mods.shift { -1 } else { 1 };
            }
        }
        if step == 0 {
            return;
        }

        let n = self.focus_order.len() as i32;
        let next = match self.focus_order.iter().position(|&id| id == self.focus) {
            Some(at) => (at as i32 + step).rem_euclid(n),
            // Nothing focused, so the two directions start at the two ends.
            None => {
                if step > 0 {
                    0
                } else {
                    n - 1
                }
            }
        };
        self.focus = self.focus_order[next as usize];
        // The widget states this for itself when it is declared, a few lines from
        // now. Cleared here so a move away from a text field does not leave the
        // shortcuts suppressed.
        self.focus_is_text = false;
    }

    /// True when the pointer is available to the given widget.
    fn pointer_free(&self, id: u64) -> bool {
        self.popup_owner == 0 || id == self.popup_owner
    }

    fn hovered(&self, id: u64, rect: Rect) -> bool {
        self.enabled()
            && self.input.inside
            && !rect.is_empty()
            && rect.contains(self.input.mouse.0, self.input.mouse.1)
            && self.pointer_free(id)
    }

    fn prev_rect(&self, id: u64) -> Rect {
        self.rects.get(&id).copied().unwrap_or_default()
    }

    /// Press and release cycle on one widget. The click only counts when the
    /// release lands inside, which is the standard cancel gesture.
    ///
    /// A widget that holds nothing takes no focus and clears whatever held it.
    /// The two are one statement: focus exists to route character input and to
    /// say which control an accent outline belongs to, and a button that was
    /// pressed answers neither question. Keeping it would leave the outline
    /// behind after the press, which reads as a control still waiting for
    /// something; clearing it is what stops typing from continuing into a field
    /// the operator has just clicked away from.
    fn clicked(&mut self, id: u64, cursor: CursorKind, takes_focus: bool) -> bool {
        let rect = self.prev_rect(id);
        let hovered = self.hovered(id, rect);
        if hovered {
            self.hot = id;
            self.cursor = cursor;
        }

        let mut clicked = false;
        if self.active == id {
            if self.input.left_released() {
                clicked = hovered;
                self.active = 0;
            }
        } else if hovered && self.input.left_pressed() {
            self.active = id;
            self.focus = if takes_focus { id } else { 0 };
            self.focus_is_text = false;
        }
        clicked
    }

    /// True while this cell is the one being typed into.
    fn editing(&self, id: u64) -> bool {
        matches!(self.edit.as_ref(), Some((owner, _)) if *owner == id)
    }

    fn retained_mut(&mut self, id: u64) -> &mut Retained {
        self.retained.entry(id).or_default()
    }

    fn retained_of(&self, id: u64) -> Retained {
        self.retained.get(&id).copied().unwrap_or_default()
    }

    /// Advances an animation phase toward its target and returns the eased value.
    ///
    /// The stored phase is linear and the curve is applied on the way out, which
    /// is what makes one curve serve both directions: read against a phase that
    /// is falling, a curve that is fast at the start becomes one that is slow at
    /// the start, which is the correct pair for an appearance and a dismissal.
    ///
    /// Stepped by the stated duration rather than approached exponentially, so
    /// the duration means what it says and the phase reaches its target exactly.
    fn animate(&mut self, id: u64, target: f32) -> f32 {
        let curve = self.theme.anim_curve;
        let step = if self.theme.animate {
            (self.dt * 1000.0 / self.theme.anim_ms.max(1.0)).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let slot = self.retained_mut(id);
        if !slot.seen {
            slot.seen = true;
            slot.anim = target;
        } else if slot.anim < target {
            slot.anim = (slot.anim + step).min(target);
        } else if slot.anim > target {
            slot.anim = (slot.anim - step).max(target);
        }
        curve.apply(slot.anim)
    }

    // -------------------------------------------------------------- drawing

    /// Draws the widget tree.
    pub fn draw_tree(&mut self, fonts: &mut FontSystem, list: &mut DrawList) {
        let nodes = std::mem::take(&mut self.nodes);
        let items = std::mem::take(&mut self.items);
        let strings = std::mem::take(&mut self.strings);

        self.draw_node(&nodes, &items, &strings, self.root, fonts, list);

        self.nodes = nodes;
        self.items = items;
        self.strings = strings;
    }

    /// Draws everything that sits above the tree and above the reserved areas.
    pub fn draw_top(&mut self, fonts: &mut FontSystem, list: &mut DrawList) {
        let strings = std::mem::take(&mut self.strings);

        let popup = self.popup.take();
        match popup {
            Some(p) => {
                let anchor = self.rects.get(&p.owner).copied().unwrap_or_default();
                // Faded in by the same phase that reveals the list, so the
                // statement of modality arrives with the thing that is modal.
                // Over everything including the data areas, because the list
                // really does take every press.
                if self.theme.popup_shade > 0.001 {
                    let alpha = self.theme.popup_shade * p.anim.clamp(0.0, 1.0);
                    if alpha > 0.002 {
                        list.fill_rect(self.viewport, self.theme.background.with_alpha(alpha));
                    }
                }
                let (full, visible) = self.draw_popup(&p, &strings, anchor, fonts, list);
                // A list on its way out is drawn and is not modal. Holding the
                // pointer for it would make the very click that dismissed it
                // unusable for the duration of the animation.
                if p.open {
                    self.popup_owner = p.owner;
                    self.popup_rect = full;
                    self.popup_visible = visible;
                } else {
                    self.popup_owner = 0;
                    self.popup_rect = Rect::default();
                    self.popup_visible = Rect::default();
                }
            }
            None => {
                self.popup_owner = 0;
                self.popup_rect = Rect::default();
                self.popup_visible = Rect::default();
            }
        }

        self.strings = strings;

        if !self.overlay.is_empty() {
            self.draw_overlay(fonts, list);
        }
    }

    fn draw_node(
        &self,
        nodes: &[LayoutNode],
        items: &[Item],
        strings: &str,
        index: usize,
        fonts: &mut FontSystem,
        list: &mut DrawList,
    ) {
        let node = &nodes[index];
        let r = node.rect;
        let t = &self.theme;
        let border = self.line(t.border_px);

        match items[index] {
            Item::None => {}

            Item::Frame { fill, border: bc, border_px } => {
                if fill.0[3] > 0 {
                    list.fill_rect(r, fill);
                }
                if border_px > 0.0 && bc.0[3] > 0 {
                    list.stroke_rect(r, self.line(border_px), bc);
                }
            }

            Item::Header { text, id, collapsible, open, active } => {
                let hot = collapsible && self.hot == id;
                list.fill_rect(r, if hot { t.control_hover } else { t.panel_header });
                list.hline(r.x, r.right(), r.bottom() - border, border, t.border);
                if t.group_tick {
                    list.fill_rect(Rect::new(r.x, r.y, self.line(2.0), r.h), t.accent);
                }

                let mut text_x = r.x + self.m(t.padding);
                if collapsible {
                    // Fold marker: a chevron pointing down when the group is
                    // open, to the right when it is folded.
                    let cx = r.x + self.m(t.padding) + self.m(3.0);
                    let cy = r.y + r.h * 0.5;
                    let a = self.m(3.0);
                    let th = self.line(1.0);
                    let color = if hot { t.text } else { t.text_dim };
                    if open {
                        list.line(cx - a, cy - a * 0.5, cx, cy + a * 0.5, th, color);
                        list.line(cx, cy + a * 0.5, cx + a, cy - a * 0.5, th, color);
                    } else {
                        list.line(cx - a * 0.5, cy - a, cx + a * 0.5, cy, th, color);
                        list.line(cx + a * 0.5, cy, cx - a * 0.5, cy + a, th, color);
                    }
                    text_x = cx + a + self.m(t.gap);
                }

                let baseline = fonts.baseline_centered(FontId::Ui, self.ui_px, r.y, r.h);
                list.push_clip(r);
                fonts.draw_text(
                    list,
                    text_x.round(),
                    baseline,
                    Self::span_text(strings, text),
                    FontId::Ui,
                    self.ui_px,
                    t.text,
                );
                list.pop_clip();

                // A dot at the trailing end rather than a stripe at the leading
                // one, which is where the decorative group marker already sits: a
                // statement about the contents and a statement about the frame
                // have to be distinguishable.
                if active {
                    let d = self.line(4.0);
                    let x = (r.right() - self.m(t.padding) - d).round();
                    let y = (r.y + (r.h - d) * 0.5).round();
                    list.fill_rect(Rect::new(x, y, d, d), t.accent);
                }
            }

            Item::Label { text, color, font, size, align } => {
                let s = Self::span_text(strings, text);
                let w = fonts.measure(s, font, size);
                let baseline = fonts.baseline_centered(font, size, r.y, r.h);
                if w > r.w + 0.5 {
                    list.push_clip(r);
                    Self::draw_elided(fonts, list, r.x.round(), baseline, s, font, size, r.w, color);
                    list.pop_clip();
                } else {
                    let x = match align {
                        TextAlign::Left => r.x,
                        TextAlign::Center => r.x + (r.w - w) * 0.5,
                        TextAlign::Right => r.right() - w,
                    };
                    fonts.draw_text(list, x.round(), baseline, s, font, size, color);
                }
            }

            Item::Value { text, id, editing, cursor, enabled } => {
                let s = Self::span_text(strings, text);
                let baseline = fonts.baseline_centered(FontId::Mono, self.ui_px, r.y, r.h);
                if editing {
                    // The same geometry the number occupied, so the field appears
                    // in place and nothing on the row moves. A cell that jumped
                    // would take the caret out from under the pointer that had
                    // just been aimed at it.
                    list.fill_rect(r, t.surface_dark);
                    list.stroke_rect(r, border, t.accent);
                    let pad = self.m(t.padding * 0.5);
                    let clip = Rect::from_min_max(
                        r.x + pad,
                        r.y,
                        (r.right() - pad).max(r.x + pad),
                        r.bottom(),
                    );
                    list.push_clip(clip);
                    let caret = cursor.min(s.len());
                    let caret_x = fonts.measure(&s[..caret], FontId::Mono, self.ui_px);
                    let shift = (caret_x - (clip.w - self.m(4.0))).max(0.0);
                    let x = (clip.x - shift).round();
                    fonts.draw_text(list, x, baseline, s, FontId::Mono, self.ui_px, t.text);
                    if ((self.time * 2.0) as i64) % 2 == 0 {
                        list.vline(
                            (x + caret_x).round(),
                            r.y + self.m(3.0),
                            r.bottom() - self.m(3.0),
                            self.line(1.0),
                            t.accent,
                        );
                    }
                    list.pop_clip();
                } else {
                    let hot = enabled && self.hot == id;
                    let w = fonts.measure(s, FontId::Mono, self.ui_px);
                    let color = if !enabled {
                        t.text_disabled
                    } else if hot {
                        t.accent
                    } else {
                        t.text
                    };
                    let x = (r.right() - w).round();
                    fonts.draw_text(list, x, baseline, s, FontId::Mono, self.ui_px, color);
                    // A hairline under the number, so a cell that accepts typing
                    // says so before it is double clicked. Nothing moves and no
                    // box appears, which is what keeps the row reading as a
                    // readout rather than as a field.
                    if hot {
                        list.hline(
                            x,
                            r.right(),
                            r.bottom() - self.line(1.0),
                            self.line(1.0),
                            t.accent.with_alpha(0.5),
                        );
                    }
                }
            }

            Item::Button { text, id, kind, selected, enabled } => {
                let hot = enabled && self.hot == id;
                let held = enabled && self.active == id;
                let (fill, bc) = match kind {
                    ButtonKind::Normal => {
                        let fill = if !enabled {
                            t.control_disabled
                        } else if held {
                            t.control_active
                        } else if hot {
                            t.control_hover
                        } else {
                            t.control
                        };
                        let edge = if !enabled {
                            t.border
                        } else if hot || held {
                            t.hover_border()
                        } else {
                            t.border_strong
                        };
                        (fill, edge)
                    }
                    ButtonKind::Tab => {
                        if t.tab_underline {
                            // One mark states the selection: the bar below.
                            // The surface stays flat so the strip reads as a
                            // row of labels rather than as a row of boxes.
                            let fill = if hot { t.control_hover } else { Color::TRANSPARENT };
                            (fill, Color::TRANSPARENT)
                        } else {
                            let fill = if selected {
                                t.surface_dark
                            } else if hot {
                                t.control_hover
                            } else {
                                t.control
                            };
                            let edge = if selected { t.border_strong } else { t.border };
                            (fill, edge)
                        }
                    }
                    ButtonKind::Flat => {
                        let fill = if hot { t.control_hover } else { Color::TRANSPARENT };
                        (fill, Color::TRANSPARENT)
                    }
                };
                if fill.0[3] > 0 {
                    list.fill_rect(r, fill);
                }
                if bc.0[3] > 0 {
                    list.stroke_rect(r, border, bc);
                }

                if kind == ButtonKind::Tab && selected && t.tab_underline {
                    list.fill_rect(
                        Rect::new(r.x, r.bottom() - border * 2.0, r.w, border * 2.0),
                        t.accent,
                    );
                }

                let s = Self::span_text(strings, text);
                let w = fonts.measure(s, FontId::Ui, self.ui_px);
                let baseline = fonts.baseline_centered(FontId::Ui, self.ui_px, r.y, r.h);
                let color = if !enabled {
                    t.text_disabled
                } else if selected || hot || held {
                    t.text
                } else {
                    t.text_dim
                };
                list.push_clip(r);
                if w > r.w + 0.5 {
                    // A caption that no longer fits is elided rather than
                    // centred past the edges: a tab strip narrowed by a long
                    // translation would otherwise show the middle of every
                    // word and the start of none.
                    Self::draw_elided(
                        fonts,
                        list,
                        r.x.round(),
                        baseline,
                        s,
                        FontId::Ui,
                        self.ui_px,
                        r.w,
                        color,
                    );
                } else {
                    fonts.draw_text(
                        list,
                        (r.x + (r.w - w) * 0.5).round(),
                        baseline,
                        s,
                        FontId::Ui,
                        self.ui_px,
                        color,
                    );
                }
                list.pop_clip();
            }

            Item::Checkbox { text, id, checked, enabled } => {
                let hot = enabled && self.hot == id;
                let side = self.m(t.checkbox);
                let box_rect = Rect::new(r.x, (r.y + (r.h - side) * 0.5).round(), side, side);
                list.fill_rect(box_rect, if enabled { t.surface_dark } else { t.control_disabled });
                let edge = if !enabled {
                    t.border
                } else if hot {
                    t.hover_border()
                } else {
                    t.border_strong
                };
                list.stroke_rect(box_rect, border, edge);
                if checked {
                    let core = if enabled { t.accent } else { t.text_disabled };
                    list.fill_rect(box_rect.inset(self.line(3.0)), core);
                }
                let s = Self::span_text(strings, text);
                if !s.is_empty() {
                    let baseline = fonts.baseline_centered(FontId::Ui, self.ui_px, r.y, r.h);
                    let text_x = box_rect.right() + self.m(t.gap);
                    let clip = Rect::from_min_max(text_x, r.y, r.right(), r.bottom());
                    let color = if !enabled {
                        t.text_disabled
                    } else if checked {
                        t.text
                    } else {
                        t.text_dim
                    };
                    list.push_clip(clip);
                    Self::draw_elided(
                        fonts,
                        list,
                        text_x.round(),
                        baseline,
                        s,
                        FontId::Ui,
                        self.ui_px,
                        clip.w,
                        color,
                    );
                    list.pop_clip();
                }
            }

            Item::Toggle { id, on, anim, enabled } => {
                let hot = enabled && self.hot == id;
                let tw = self.m(t.toggle_width);
                let th = self.m(t.toggle_height);
                let track = Rect::new(r.x, (r.y + (r.h - th) * 0.5).round(), tw, th);

                // The track carries the state as a colour and the knob carries
                // it as a position, so the switch reads correctly both at a
                // glance and for an operator who cannot separate the two hues.
                let on_fill = if enabled { t.accent.with_alpha(0.30) } else { t.border };
                let base = if enabled { t.surface_dark } else { t.control_disabled };
                let fill = base.lerp(on_fill, anim);
                list.fill_rect(track, fill);
                let edge = if !enabled {
                    t.border
                } else if hot {
                    t.hover_border()
                } else if on {
                    t.accent.with_alpha(0.70)
                } else {
                    t.border_strong
                };
                list.stroke_rect(track, border, edge);

                let inset = self.line(2.0);
                let knob_side = (th - inset * 2.0).max(2.0);
                let travel = (tw - inset * 2.0 - knob_side).max(0.0);
                let knob = Rect::new(
                    (track.x + inset + travel * anim).round(),
                    (track.y + inset).round(),
                    knob_side,
                    knob_side,
                );
                let knob_color = if !enabled {
                    t.text_disabled
                } else if on {
                    t.accent
                } else {
                    t.text_dim
                };
                list.fill_rect(knob, knob_color);
            }

            Item::SliderTrack { id, fraction, enabled } => {
                let hot = enabled && self.hot == id;
                let held = enabled && self.active == id;
                let thickness = self.line(4.0);
                let track = Rect::new(r.x, (r.y + (r.h - thickness) * 0.5).round(), r.w, thickness);
                list.fill_rect(track, if enabled { t.surface_dark } else { t.control_disabled });
                list.stroke_rect(track, border, t.border);

                let fill_color = if enabled { t.accent } else { t.text_disabled };
                let filled = (track.w * fraction.clamp(0.0, 1.0)).round();
                if filled > 0.0 {
                    list.fill_rect(Rect::new(track.x, track.y, filled, track.h), fill_color);
                }

                let knob_w = self.line(5.0);
                let knob_h = self.m(12.0).min(r.h);
                let knob = Rect::new(
                    (track.x + filled - knob_w * 0.5)
                        .round()
                        .clamp(r.x, (r.right() - knob_w).max(r.x)),
                    (r.y + (r.h - knob_h) * 0.5).round(),
                    knob_w,
                    knob_h,
                );
                let knob_color = if !enabled {
                    t.text_disabled
                } else if held || hot {
                    t.text
                } else {
                    t.text_dim
                };
                list.fill_rect(knob, knob_color);
                list.stroke_rect(knob, border, t.surface_dark);
            }

            Item::Combo { text, id, open, anim, enabled } => {
                let hot = enabled && self.hot == id;
                let fill = if !enabled {
                    t.control_disabled
                } else if hot {
                    t.control_hover
                } else {
                    t.control
                };
                list.fill_rect(r, fill);
                let edge = if !enabled {
                    t.border
                } else if open {
                    t.accent
                } else if hot {
                    t.hover_border()
                } else {
                    t.border_strong
                };
                list.stroke_rect(r, border, edge);

                let pad = self.m(t.padding * 0.5);
                let arrow_w = self.m(14.0);
                let inner =
                    Rect::from_min_max(r.x + pad, r.y, (r.right() - arrow_w).max(r.x + pad), r.bottom());
                list.push_clip(inner);
                let baseline = fonts.baseline_centered(FontId::Ui, self.ui_px, r.y, r.h);
                let color = if enabled { t.text } else { t.text_disabled };
                Self::draw_elided(
                    fonts,
                    list,
                    inner.x.round(),
                    baseline,
                    Self::span_text(strings, text),
                    FontId::Ui,
                    self.ui_px,
                    inner.w,
                    color,
                );
                list.pop_clip();

                let cx = r.right() - arrow_w * 0.5;
                let cy = r.y + r.h * 0.5;
                let a = self.m(3.0);
                let th = self.line(1.0);
                let arrow = if !enabled {
                    t.text_disabled
                } else if hot || open {
                    t.text
                } else {
                    t.text_dim
                };
                // Turned over by the same phase that reveals the list, so the
                // mark and the geometry state one thing rather than two. Halfway
                // through it is a flat line, which reads as a clean pass rather
                // than as a shape being crushed.
                let s = 1.0 - 2.0 * anim.clamp(0.0, 1.0);
                list.line(cx - a, cy - a * 0.5 * s, cx, cy + a * 0.5 * s, th, arrow);
                list.line(cx, cy + a * 0.5 * s, cx + a, cy - a * 0.5 * s, th, arrow);
            }

            Item::TextEdit { text, id, cursor, enabled } => {
                let focused = enabled && self.focus == id;
                let hot = enabled && self.hot == id;
                list.fill_rect(r, if enabled { t.surface_dark } else { t.control_disabled });
                list.stroke_rect(
                    r,
                    border,
                    if focused {
                        t.accent
                    } else if hot {
                        t.border_strong
                    } else {
                        t.border
                    },
                );

                let pad = self.m(t.padding * 0.5);
                let clip =
                    Rect::from_min_max(r.x + pad, r.y, (r.right() - pad).max(r.x + pad), r.bottom());
                list.push_clip(clip);

                let s = Self::span_text(strings, text);
                let baseline = fonts.baseline_centered(FontId::Mono, self.ui_px, r.y, r.h);
                let caret = cursor.min(s.len());
                let caret_x = fonts.measure(&s[..caret], FontId::Mono, self.ui_px);
                let shift = (caret_x - (clip.w - self.m(4.0))).max(0.0);
                let x = (clip.x - shift).round();
                let color = if enabled { t.text } else { t.text_disabled };
                fonts.draw_text(list, x, baseline, s, FontId::Mono, self.ui_px, color);

                if focused && ((self.time * 2.0) as i64) % 2 == 0 {
                    let cx = (x + caret_x).round();
                    list.vline(
                        cx,
                        r.y + self.m(3.0),
                        r.bottom() - self.m(3.0),
                        self.line(1.0),
                        t.accent,
                    );
                }
                list.pop_clip();
            }

            Item::Splitter { id, vertical } => {
                let hot = self.hot == id;
                let held = self.active == id;
                list.fill_rect(r, t.panel);
                let color = if held {
                    t.text_dim
                } else if hot {
                    t.border_strong
                } else {
                    t.border
                };
                if t.splitter_grip {
                    // Three short marks read as a handle at a glance, which a
                    // continuous line does not: a line is indistinguishable
                    // from a separator that happens to sit there.
                    let th = self.line(1.0);
                    let step = self.m(4.0);
                    let len = self.m(10.0);
                    if vertical {
                        let x = (r.x + r.w * 0.5).round();
                        let cy = r.y + r.h * 0.5;
                        for k in -1..=1 {
                            let y = cy + k as f32 * step;
                            list.hline(x - th, x + th, y.round(), th, color);
                        }
                        let _ = len;
                    } else {
                        let y = (r.y + r.h * 0.5).round();
                        let cx = r.x + r.w * 0.5;
                        for k in -1..=1 {
                            let x = cx + k as f32 * step;
                            list.vline(x.round(), y - th, y + th, th, color);
                        }
                    }
                } else if vertical {
                    list.vline(r.x + r.w * 0.5, r.y, r.bottom(), self.line(1.0), color);
                } else {
                    list.hline(r.x, r.right(), r.y + r.h * 0.5, self.line(1.0), color);
                }
            }

            Item::Separator { vertical } => {
                if vertical {
                    list.vline(r.x + r.w * 0.5, r.y, r.bottom(), border, t.separator);
                } else {
                    list.hline(r.x, r.right(), r.y + r.h * 0.5, border, t.separator);
                }
            }

            Item::Caption { .. } => {}

            Item::TabBar { .. } => {}

            Item::WindowChrome { id, kind } => {
                let hot = self.hot == id;
                let close = kind == WindowButton::Close;
                if hot {
                    list.fill_rect(r, if close { t.danger } else { t.control_hover });
                }
                let color = if hot && close {
                    Color::hex(0xFFFFFF)
                } else if hot {
                    t.text
                } else {
                    t.text_dim
                };
                let cx = (r.x + r.w * 0.5).round();
                let cy = (r.y + r.h * 0.5).round();
                let a = self.m(4.0);
                let th = self.line(1.0);
                match kind {
                    WindowButton::Minimize => list.hline(cx - a, cx + a, cy, th, color),
                    WindowButton::Maximize => {
                        list.stroke_rect(Rect::new(cx - a, cy - a, a * 2.0, a * 2.0), th, color)
                    }
                    WindowButton::Restore => {
                        let o = self.m(2.0);
                        list.stroke_rect(
                            Rect::new(cx - a, cy - a + o, a * 2.0 - o, a * 2.0 - o),
                            th,
                            color,
                        );
                        list.stroke_rect(
                            Rect::new(cx - a + o, cy - a, a * 2.0 - o, a * 2.0 - o),
                            th,
                            color,
                        );
                    }
                    WindowButton::Close => {
                        list.line(cx - a, cy - a, cx + a, cy + a, th, color);
                        list.line(cx - a, cy + a, cx + a, cy - a, th, color);
                    }
                }
            }

            Item::Custom { .. } => {}
        }

        // Focus outline. One mark, one colour, the same on every control: a
        // focus indicator that differs per widget is not an indicator, it is a
        // decoration that happens to correlate with focus.
        if t.focus_ring && self.focus != 0 {
            // A button and a group header are included because the traversal
            // reaches them, and a focus nobody can see is a focus nobody can use.
            // The text field and the numeric cell are absent: both already carry
            // an accent border while they hold focus, and a second mark on top of
            // it would say the same thing twice.
            let ringed = matches!(
                items[index],
                Item::Checkbox { .. }
                    | Item::Toggle { .. }
                    | Item::SliderTrack { .. }
                    | Item::Combo { .. }
                    | Item::Button { .. }
                    | Item::Header { .. }
            );
            if ringed && Self::item_id(&items[index]) == self.focus {
                list.stroke_rect(r, border, t.accent);
            }
        }

        if node.clip {
            list.push_clip(r);
        }
        for &child in &node.children {
            self.draw_node(nodes, items, strings, child, fonts, list);
        }
        if node.clip {
            list.pop_clip();
        }

        // Tab selection bar. After the children because a hover fill spans the
        // whole tab and the bar sits inside its bottom edge; before it the fill
        // would cover it.
        if let Item::TabBar { x, w, .. } = items[index] {
            if t.tab_underline && w > 0.5 {
                let thickness = self.line(2.0);
                list.fill_rect(
                    Rect::new(
                        (r.x + x).round(),
                        (r.bottom() - thickness).round(),
                        w.round(),
                        thickness,
                    ),
                    t.accent,
                );
            }
        }

        // Scroll indicator sits on top of the clipped content.
        if node.scrollable && node.content_main > r.h + 0.5 && r.h > 0.0 {
            let bar_w = self.m(t.scrollbar);
            let track = Rect::new(r.right() - bar_w, r.y, bar_w, r.h);
            list.fill_rect(track, t.surface_dark);
            let visible = (r.h / node.content_main).clamp(0.05, 1.0);
            let offset = (node.scroll / (node.content_main - r.h)).clamp(0.0, 1.0);
            let thumb_h = (track.h * visible).max(self.m(16.0)).min(track.h);
            let thumb_y = track.y + (track.h - thumb_h) * offset;
            list.fill_rect(
                Rect::new(
                    track.x + self.line(1.0),
                    thumb_y.round(),
                    (bar_w - self.line(2.0)).max(1.0),
                    thumb_h.round(),
                ),
                t.border_strong,
            );
        }
    }

    /// Draws text, replacing the tail with a marker when it does not fit.
    #[allow(clippy::too_many_arguments)]
    fn draw_elided(
        fonts: &mut FontSystem,
        list: &mut DrawList,
        x: f32,
        baseline: f32,
        text: &str,
        font: FontId,
        px: f32,
        width: f32,
        color: Color,
    ) {
        if text.is_empty() || width <= 0.0 {
            return;
        }
        if fonts.measure(text, font, px) <= width {
            fonts.draw_text(list, x, baseline, text, font, px, color);
            return;
        }

        const MARKER: &str = "...";
        let marker_w = fonts.measure(MARKER, font, px);
        let room = (width - marker_w).max(0.0);
        let cut = fonts.fit(text, font, px, room);
        let head = &text[..cut];
        let advance = fonts.draw_text(list, x, baseline, head, font, px, color);
        fonts.draw_text(list, (x + advance).round(), baseline, MARKER, font, px, color);
    }

    /// Draws the combo list.
    ///
    /// Returns the full geometry and the part of it drawn, which during the
    /// reveal are different rectangles and are read by different things: the row
    /// arithmetic by the first, the containment test by the second.
    ///
    /// The reveal grows from the edge the list is anchored to, so the edge
    /// touching the control stays put and the free one moves. Rows are drawn at
    /// their final positions and clipped rather than sliding with the edge, which
    /// is a shade rising rather than a drawer being pulled: the calmer of the two
    /// and the one that does not move text an operator is already reading.
    fn draw_popup(
        &self,
        popup: &Popup,
        strings: &str,
        anchor: Rect,
        fonts: &mut FontSystem,
        list: &mut DrawList,
    ) -> (Rect, Rect) {
        if anchor.is_empty() || popup.items.is_empty() {
            return (Rect::default(), Rect::default());
        }
        let t = &self.theme;
        let border = self.line(t.border_px);
        let pad = self.m(t.padding);
        let row = self.m(t.row_height);

        let mut widest = 0.0f32;
        for &span in &popup.items {
            let s = Self::span_text(strings, span);
            widest = widest.max(fonts.measure(s, FontId::Ui, self.ui_px));
        }
        let wanted = (widest + pad * 2.0 + border * 2.0).max(anchor.w);
        let w = wanted.min(self.viewport.w - self.m(t.gap) * 2.0).max(self.m(48.0));

        let height = (row * popup.items.len() as f32 + border * 2.0).min(self.viewport.h);

        let mut y = anchor.bottom();
        if y + height > self.viewport.bottom() {
            y = (anchor.y - height).max(self.viewport.y);
        }
        let mut x = anchor.x;
        if x + w > self.viewport.right() {
            x = self.viewport.right() - w;
        }
        x = x.max(self.viewport.x);

        let rect = Rect::new(x.round(), y.round(), w.round(), height.round());

        // Which edge is anchored follows from where the list was placed: below
        // the control the top edge is fixed, above it the bottom one is.
        let upward = y < anchor.y;
        let shown = (rect.h * popup.anim.clamp(0.0, 1.0)).round();
        // Below a couple of pixels there is nothing to draw but a sliver, which
        // reads as a defect rather than as a beginning.
        if shown < 2.0 {
            return (rect, Rect::default());
        }
        let visible = if upward {
            Rect::new(rect.x, rect.bottom() - shown, rect.w, shown)
        } else {
            Rect::new(rect.x, rect.y, rect.w, shown)
        };

        list.fill_rect(visible, t.surface);
        list.stroke_rect(visible, border, t.accent);
        list.push_clip(visible);

        for (i, &span) in popup.items.iter().enumerate() {
            let item = Rect::new(
                rect.x + border,
                (rect.y + border + row * i as f32).round(),
                rect.w - border * 2.0,
                row,
            );
            if item.y > rect.bottom() {
                break;
            }
            // Only while the list accepts input. A row lighting up under the
            // pointer as the list disappears claims the press is still going
            // somewhere.
            let hovered = popup.open
                && self.input.inside
                && item.contains(self.input.mouse.0, self.input.mouse.1);
            if i == popup.selected {
                list.fill_rect(item, t.control_active);
            } else if hovered {
                list.fill_rect(item, t.control_hover);
            }
            let baseline = fonts.baseline_centered(FontId::Ui, self.ui_px, item.y, item.h);
            Self::draw_elided(
                fonts,
                list,
                (item.x + pad).round(),
                baseline,
                Self::span_text(strings, span),
                FontId::Ui,
                self.ui_px,
                item.w - pad * 2.0,
                if i == popup.selected { t.text } else { t.text_dim },
            );
        }

        list.pop_clip();
        (rect, visible)
    }

    /// Diagnostic block, pinned to the top right corner of the viewport.
    fn draw_overlay(&self, fonts: &mut FontSystem, list: &mut DrawList) {
        let t = &self.theme;
        let pad = self.m(t.padding);
        let line_h = fonts.line_height(FontId::Mono, self.mono_px);

        let mut widest = 0.0f32;
        for line in &self.overlay {
            widest = widest.max(fonts.measure(line, FontId::Mono, self.mono_px));
        }
        let w = widest + pad * 2.0;
        let h = line_h * self.overlay.len() as f32 + pad * 2.0;
        let top = self.viewport.y + self.m(t.caption_height * 2.0);
        let rect = Rect::new(
            (self.viewport.right() - w - pad).round(),
            top.round(),
            w.round(),
            h.round(),
        );

        list.fill_rect(rect, t.surface_dark);
        list.stroke_rect(rect, self.line(t.border_px), t.accent);
        list.push_clip(rect);

        let metrics = fonts.metrics(FontId::Mono, self.mono_px);
        let mut y = rect.y + pad + metrics.ascent;
        for line in &self.overlay {
            fonts.draw_text(
                list,
                (rect.x + pad).round(),
                y.round(),
                line,
                FontId::Mono,
                self.mono_px,
                t.text,
            );
            y += line_h;
        }
        list.pop_clip();
    }
}

/// Frame scoped builder. Holds the interface state and the font system, the
/// two things every widget needs, so the call sites stay short.
pub struct Frame<'a> {
    pub ui: &'a mut Ui,
    pub fonts: &'a mut FontSystem,
}

impl<'a> Frame<'a> {
    pub fn new(ui: &'a mut Ui, fonts: &'a mut FontSystem) -> Frame<'a> {
        Frame { ui, fonts }
    }
}

/// Fowler Noll Vo hash, chosen because it is short, has no dependencies and
/// distributes short ASCII keys well enough for an identity map.
fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn mix(a: u64, b: u64) -> u64 {
    let mut h = a ^ b.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 29;
    h
}