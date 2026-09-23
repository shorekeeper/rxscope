//! Flexbox layout over a flat node arena.
//!
//! Supported: row and column direction, wrapping, grow, shrink, basis, fixed
//! and percentage sizes, minimum and maximum constraints, margin, padding,
//! gap, justification along the main axis and alignment along the cross axis
//! including stretch. Absolute positioning is not part of the model; the two
//! floating elements the interface needs, the combo box list and the debug
//! overlay, are drawn outside the tree.
//!
//! One deliberate deviation from CSS: shrink defaults to zero instead of one.
//! In a browser the viewport scrolls, so shrinking everything to fit is a
//! reasonable default. Here the window is the viewport and a settings column
//! is always taller than it, so a shrink of one propagates a negative free
//! space through the whole tree and compresses unrelated chrome. With a
//! default of zero an oversized subtree simply overflows and is clipped or
//! scrolled, and compression happens only where it was asked for.
//!
//! The solver runs in two passes. The measure pass walks bottom up and stores
//! an intrinsic size for every node: leaves report the size handed in at build
//! time, containers report the sum of their children along the main axis and
//! the largest child along the cross axis. The arrange pass walks top down,
//! distributes the free space of each line and writes final rectangles.
//!
//! Rectangles are rounded to whole pixels as they are assigned, so a one pixel
//! border never lands between two pixels and turns grey.

#![allow(dead_code)]

use crate::render::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

/// Size specification. Percentages resolve against the content box of the
/// parent, which is known before any child is placed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dim {
    Auto,
    Px(f32),
    Percent(f32),
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Edges {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Edges {
    pub fn all(v: f32) -> Edges {
        Edges { left: v, top: v, right: v, bottom: v }
    }

    pub fn xy(x: f32, y: f32) -> Edges {
        Edges { left: x, top: y, right: x, bottom: y }
    }

    /// Total extent consumed along the main axis of the given direction.
    fn main(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Row => self.left + self.right,
            Direction::Column => self.top + self.bottom,
        }
    }

    fn cross(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Row => self.top + self.bottom,
            Direction::Column => self.left + self.right,
        }
    }

    fn main_lead(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Row => self.left,
            Direction::Column => self.top,
        }
    }

    fn main_trail(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Row => self.right,
            Direction::Column => self.bottom,
        }
    }

    fn cross_lead(&self, dir: Direction) -> f32 {
        match dir {
            Direction::Row => self.top,
            Direction::Column => self.left,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub direction: Direction,
    pub wrap: bool,
    pub justify: Justify,
    pub align_items: Align,
    /// Overrides the alignment the parent would apply to this node.
    pub align_self: Option<Align>,
    pub grow: f32,
    /// Zero by default, see the module note.
    pub shrink: f32,
    pub basis: Dim,
    pub width: Dim,
    pub height: Dim,
    pub min_width: f32,
    pub min_height: f32,
    pub max_width: f32,
    pub max_height: f32,
    pub margin: Edges,
    pub padding: Edges,
    pub gap: f32,
    /// Marks a viewport that scrolls its content along the main axis. Such a
    /// node reports no intrinsic main extent, so the tree above it is sized by
    /// the window rather than by the length of the list inside.
    pub scroll: bool,
}

impl Default for Style {
    fn default() -> Style {
        Style {
            direction: Direction::Row,
            wrap: false,
            justify: Justify::Start,
            align_items: Align::Stretch,
            align_self: None,
            grow: 0.0,
            shrink: 0.0,
            basis: Dim::Auto,
            width: Dim::Auto,
            height: Dim::Auto,
            min_width: 0.0,
            min_height: 0.0,
            max_width: f32::INFINITY,
            max_height: f32::INFINITY,
            margin: Edges::default(),
            padding: Edges::default(),
            gap: 0.0,
            scroll: false,
        }
    }
}

impl Style {
    pub fn row() -> Style {
        Style { direction: Direction::Row, ..Style::default() }
    }

    pub fn column() -> Style {
        Style { direction: Direction::Column, ..Style::default() }
    }

    /// Row that centres its children on the cross axis instead of stretching
    /// them, which is what a line of controls needs.
    pub fn row_centered() -> Style {
        Style { direction: Direction::Row, align_items: Align::Center, ..Style::default() }
    }

    pub fn grow(mut self, v: f32) -> Style {
        self.grow = v;
        self
    }
    pub fn shrink(mut self, v: f32) -> Style {
        self.shrink = v;
        self
    }
    pub fn basis_px(mut self, v: f32) -> Style {
        self.basis = Dim::Px(v);
        self
    }
    pub fn basis_percent(mut self, v: f32) -> Style {
        self.basis = Dim::Percent(v);
        self
    }
    pub fn width_px(mut self, v: f32) -> Style {
        self.width = Dim::Px(v);
        self
    }
    pub fn width_percent(mut self, v: f32) -> Style {
        self.width = Dim::Percent(v);
        self
    }
    pub fn height_px(mut self, v: f32) -> Style {
        self.height = Dim::Px(v);
        self
    }
    pub fn height_percent(mut self, v: f32) -> Style {
        self.height = Dim::Percent(v);
        self
    }
    pub fn min_w(mut self, v: f32) -> Style {
        self.min_width = v;
        self
    }
    pub fn min_h(mut self, v: f32) -> Style {
        self.min_height = v;
        self
    }
    pub fn max_w(mut self, v: f32) -> Style {
        self.max_width = v;
        self
    }
    pub fn max_h(mut self, v: f32) -> Style {
        self.max_height = v;
        self
    }
    pub fn padding(mut self, v: f32) -> Style {
        self.padding = Edges::all(v);
        self
    }
    pub fn padding_xy(mut self, x: f32, y: f32) -> Style {
        self.padding = Edges::xy(x, y);
        self
    }
    pub fn padding_edges(mut self, e: Edges) -> Style {
        self.padding = e;
        self
    }
    pub fn margin(mut self, v: f32) -> Style {
        self.margin = Edges::all(v);
        self
    }
    pub fn margin_edges(mut self, e: Edges) -> Style {
        self.margin = e;
        self
    }
    pub fn gap(mut self, v: f32) -> Style {
        self.gap = v;
        self
    }
    pub fn justify(mut self, v: Justify) -> Style {
        self.justify = v;
        self
    }
    pub fn align(mut self, v: Align) -> Style {
        self.align_items = v;
        self
    }
    pub fn align_self(mut self, v: Align) -> Style {
        self.align_self = Some(v);
        self
    }
    pub fn wrap(mut self) -> Style {
        self.wrap = true;
        self
    }
    pub fn scroll(mut self) -> Style {
        self.scroll = true;
        self
    }
}

pub struct LayoutNode {
    pub style: Style,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Intrinsic content size of a leaf, excluding padding.
    pub content: [f32; 2],
    /// Result of the measure pass, including padding and constraints.
    pub intrinsic: [f32; 2],
    pub rect: Rect,
    /// Offset applied to children along the main axis, for scroll containers.
    pub scroll: f32,
    /// Main axis extent the children actually needed, used to clamp scroll.
    pub content_main: f32,
    pub clip: bool,
    pub scrollable: bool,
}

impl LayoutNode {
    pub fn new(style: Style, parent: Option<usize>, content: [f32; 2]) -> LayoutNode {
        LayoutNode {
            style,
            parent,
            children: Vec::new(),
            content,
            intrinsic: [0.0, 0.0],
            rect: Rect::default(),
            scroll: 0.0,
            content_main: 0.0,
            clip: false,
            scrollable: false,
        }
    }
}

/// Main and cross axis indices for a direction.
fn axes(dir: Direction) -> (usize, usize) {
    match dir {
        Direction::Row => (0, 1),
        Direction::Column => (1, 0),
    }
}

pub fn solve(nodes: &mut Vec<LayoutNode>, root: usize, viewport: Rect) {
    if nodes.is_empty() {
        return;
    }
    measure(nodes, root);
    arrange(nodes, root, viewport);
}

/// Bottom up intrinsic sizing.
fn measure(nodes: &mut Vec<LayoutNode>, id: usize) -> [f32; 2] {
    let style = nodes[id].style;
    // The child list is copied because the recursion needs the arena mutably.
    // Trees here hold a few hundred nodes, so the copy is not a concern.
    let kids = nodes[id].children.clone();
    let (ma, ca) = axes(style.direction);

    let mut main = 0.0f32;
    let mut cross = 0.0f32;

    if kids.is_empty() {
        main = nodes[id].content[ma];
        cross = nodes[id].content[ca];
    } else {
        for (i, &k) in kids.iter().enumerate() {
            let child = measure(nodes, k);
            let cm = nodes[k].style.margin;
            if i > 0 {
                main += style.gap;
            }
            main += child[ma] + cm.main(style.direction);
            cross = cross.max(child[ca] + cm.cross(style.direction));
        }
    }

    // A scrolling viewport hides the length of its content from the parent.
    // Reporting the real extent would make every ancestor grow to fit the
    // list, which is exactly what the scroll bar exists to avoid.
    if style.scroll {
        main = 0.0;
    }

    let mut size = [0.0f32; 2];
    size[ma] = main + style.padding.main(style.direction);
    size[ca] = cross + style.padding.cross(style.direction);

    // An explicit size replaces the measured one; percentages cannot be
    // resolved yet and fall back to the content size.
    if let Dim::Px(v) = style.width {
        size[0] = v;
    }
    if let Dim::Px(v) = style.height {
        size[1] = v;
    }
    size[0] = clamp(size[0], style.min_width, style.max_width);
    size[1] = clamp(size[1], style.min_height, style.max_height);

    nodes[id].intrinsic = size;
    size
}

/// Top down placement.
fn arrange(nodes: &mut Vec<LayoutNode>, id: usize, rect: Rect) {
    let snapped = Rect::new(
        rect.x.round(),
        rect.y.round(),
        rect.w.max(0.0).round(),
        rect.h.max(0.0).round(),
    );
    nodes[id].rect = snapped;

    let style = nodes[id].style;
    let kids = nodes[id].children.clone();
    if kids.is_empty() {
        nodes[id].content_main = 0.0;
        return;
    }

    let (ma, ca) = axes(style.direction);
    let origin = [snapped.x + style.padding.left, snapped.y + style.padding.top];
    let avail = [
        (snapped.w - style.padding.left - style.padding.right).max(0.0),
        (snapped.h - style.padding.top - style.padding.bottom).max(0.0),
    ];

    let n = kids.len();
    let mut main_size = vec![0.0f32; n];
    let mut cross_size = vec![0.0f32; n];

    // Base size along the main axis: the basis wins, then an explicit size on
    // that axis, then the measured intrinsic.
    for (i, &k) in kids.iter().enumerate() {
        let ks = nodes[k].style;
        let explicit = if ma == 0 { ks.width } else { ks.height };
        main_size[i] = match ks.basis {
            Dim::Px(v) => v,
            Dim::Percent(p) => avail[ma] * p,
            Dim::Auto => match explicit {
                Dim::Px(v) => v,
                Dim::Percent(p) => avail[ma] * p,
                Dim::Auto => nodes[k].intrinsic[ma],
            },
        };
    }

    // Break the children into lines. Without wrapping there is exactly one.
    let mut lines: Vec<(usize, usize)> = Vec::with_capacity(1);
    if style.wrap {
        let mut start = 0usize;
        let mut used = 0.0f32;
        for i in 0..n {
            let m = nodes[kids[i]].style.margin.main(style.direction);
            let lead = if i > start { style.gap } else { 0.0 };
            if i > start && used + lead + main_size[i] + m > avail[ma] {
                lines.push((start, i));
                start = i;
                used = main_size[i] + m;
            } else {
                used += lead + main_size[i] + m;
            }
        }
        lines.push((start, n));
    } else {
        lines.push((0, n));
    }

    let line_count = lines.len();
    let mut cross_cursor = origin[ca];
    let mut total_main_extent = 0.0f32;

    for (li, &(s0, s1)) in lines.iter().enumerate() {
        let count = s1 - s0;
        if count == 0 {
            continue;
        }

        // Free space along the main axis drives grow and shrink.
        let mut occupied = style.gap * (count.saturating_sub(1)) as f32;
        let mut grow_total = 0.0f32;
        let mut shrink_total = 0.0f32;
        for i in s0..s1 {
            let ks = nodes[kids[i]].style;
            occupied += main_size[i] + ks.margin.main(style.direction);
            grow_total += ks.grow;
            shrink_total += ks.shrink * main_size[i];
        }
        let free = avail[ma] - occupied;

        if free > 0.0 && grow_total > 0.0 {
            for i in s0..s1 {
                let g = nodes[kids[i]].style.grow;
                if g > 0.0 {
                    main_size[i] += free * (g / grow_total);
                }
            }
        } else if free < 0.0 && shrink_total > 0.0 {
            // Shrink is weighted by the base size, matching the flexbox rule
            // that a large item gives up more space than a small one. Only
            // nodes that opted in take part, so fixed chrome keeps its size
            // and the overflow lands on the element that can absorb it.
            for i in s0..s1 {
                let ks = nodes[kids[i]].style;
                let weight = ks.shrink * main_size[i] / shrink_total;
                main_size[i] = (main_size[i] + free * weight).max(0.0);
            }
        }

        for i in s0..s1 {
            let ks = nodes[kids[i]].style;
            let (lo, hi) = if ma == 0 {
                (ks.min_width, ks.max_width)
            } else {
                (ks.min_height, ks.max_height)
            };
            main_size[i] = clamp(main_size[i], lo, hi);
        }

        // Cross axis sizing. A single line stretches into the full content
        // box; wrapped lines stretch into the tallest item of the line.
        let mut line_cross = 0.0f32;
        for i in s0..s1 {
            let ks = nodes[kids[i]].style;
            let explicit = if ca == 0 { ks.width } else { ks.height };
            let align = ks.align_self.unwrap_or(style.align_items);
            let margin_cross = ks.margin.cross(style.direction);
            let size = match explicit {
                Dim::Px(v) => v,
                Dim::Percent(p) => avail[ca] * p,
                Dim::Auto => {
                    if align == Align::Stretch && !style.wrap {
                        (avail[ca] - margin_cross).max(0.0)
                    } else {
                        nodes[kids[i]].intrinsic[ca]
                    }
                }
            };
            let (lo, hi) = if ca == 0 {
                (ks.min_width, ks.max_width)
            } else {
                (ks.min_height, ks.max_height)
            };
            cross_size[i] = clamp(size, lo, hi);
            line_cross = line_cross.max(cross_size[i] + margin_cross);
        }
        if !style.wrap {
            line_cross = avail[ca];
        } else {
            for i in s0..s1 {
                let ks = nodes[kids[i]].style;
                let explicit = if ca == 0 { ks.width } else { ks.height };
                let align = ks.align_self.unwrap_or(style.align_items);
                if matches!(explicit, Dim::Auto) && align == Align::Stretch {
                    cross_size[i] = (line_cross - ks.margin.cross(style.direction)).max(0.0);
                }
            }
        }

        // Justification works on whatever space is left after sizing.
        let mut used = style.gap * (count.saturating_sub(1)) as f32;
        for i in s0..s1 {
            used += main_size[i] + nodes[kids[i]].style.margin.main(style.direction);
        }
        total_main_extent = total_main_extent.max(used);
        let slack = (avail[ma] - used).max(0.0);
        let fcount = count as f32;
        let (mut cursor, extra) = match style.justify {
            Justify::Start => (0.0, 0.0),
            Justify::Center => (slack * 0.5, 0.0),
            Justify::End => (slack, 0.0),
            Justify::SpaceBetween => {
                (0.0, if count > 1 { slack / (fcount - 1.0) } else { 0.0 })
            }
            Justify::SpaceAround => {
                let per = slack / fcount;
                (per * 0.5, per)
            }
        };
        cursor += origin[ma];
        // Scrolling shifts the whole run of children against the main axis.
        cursor -= nodes[id].scroll;

        for i in s0..s1 {
            let k = kids[i];
            let ks = nodes[k].style;
            cursor += ks.margin.main_lead(style.direction);

            let align = ks.align_self.unwrap_or(style.align_items);
            let room = (line_cross - cross_size[i] - ks.margin.cross(style.direction)).max(0.0);
            let cross_off = match align {
                Align::Start | Align::Stretch => 0.0,
                Align::Center => room * 0.5,
                Align::End => room,
            };

            let mut pos = [0.0f32; 2];
            pos[ma] = cursor;
            pos[ca] = cross_cursor + ks.margin.cross_lead(style.direction) + cross_off;
            let mut size = [0.0f32; 2];
            size[ma] = main_size[i];
            size[ca] = cross_size[i];

            arrange(nodes, k, Rect::new(pos[0], pos[1], size[0], size[1]));

            cursor += main_size[i] + ks.margin.main_trail(style.direction) + style.gap + extra;
        }

        cross_cursor += line_cross;
        if li + 1 < line_count {
            cross_cursor += style.gap;
        }
    }

    nodes[id].content_main = total_main_extent;
}

/// Clamp that tolerates an inverted range instead of panicking, which the f32
/// method does.
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if hi < lo {
        return lo;
    }
    v.max(lo).min(hi)
}