//! Draw list construction.
//!
//! The GUI layer emits geometry into a DrawList and the renderer replays it.
//! Commands are merged as long as the texture and the clip rectangle stay the
//! same, so a full panel of solid rectangles ends up as one draw call.
//!
//! All coordinates are framebuffer pixels with the origin at the top left.

use crate::render::texture::TextureId;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn from_min_max(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    }

    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }

    /// Shrinks on all sides, used for borders and padding.
    pub fn inset(&self, amount: f32) -> Rect {
        Rect {
            x: self.x + amount,
            y: self.y + amount,
            w: (self.w - amount * 2.0).max(0.0),
            h: (self.h - amount * 2.0).max(0.0),
        }
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        Rect { x: x0, y: y0, w: (x1 - x0).max(0.0), h: (y1 - y0).max(0.0) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const TRANSPARENT: Color = Color([0, 0, 0, 0]);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color([r, g, b, a])
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color([r, g, b, 255])
    }

    /// Accepts 0xRRGGBB, alpha is opaque.
    pub const fn hex(v: u32) -> Color {
        Color([((v >> 16) & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, (v & 0xFF) as u8, 255])
    }

    pub fn with_alpha(self, a: f32) -> Color {
        let mut c = self;
        c.0[3] = (a.clamp(0.0, 1.0) * 255.0) as u8;
        c
    }

    /// Linear interpolation in 8 bit space, good enough for hover states.
    pub fn lerp(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let mut out = [0u8; 4];
        for i in 0..4 {
            out[i] = (self.0[i] as f32 + (other.0[i] as f32 - self.0[i] as f32) * t) as u8;
        }
        Color(out)
    }
}

/// Fragment shader branch selector, must match ui.frag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Mode {
    Solid = 0,
    Alpha = 1,
    Rgba = 2,
    Luma = 3,
    /// Single channel holding a level, mapped through the palette.
    Waterfall = 4,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [u8; 4],
    pub mode: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct DrawCmd {
    pub index_offset: u32,
    pub index_count: u32,
    pub texture: TextureId,
    /// Scissor rectangle as x0, y0, x1, y1 in integer pixels.
    pub clip: [i32; 4],
}

pub struct DrawList {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub commands: Vec<DrawCmd>,

    clip_stack: Vec<Rect>,
    current_clip: Rect,
    current_texture: TextureId,
    /// First index of the command currently being accumulated.
    open_index: u32,
    /// Full surface, used as the base clip and to reset between frames.
    surface: Rect,
    /// Waterfall mapping, written into the push constants.
    ///
    /// Frame state rather than per command state, because there is one waterfall
    /// and it is drawn once. Carrying it per command would put three floats in
    /// every draw command to describe something only one of them reads.
    pub waterfall_store_span_db: f32,
    pub waterfall_display_span_db: f32,
    pub waterfall_inv_gamma: f32,
}

impl DrawList {
    pub fn new() -> DrawList {
        DrawList {
            vertices: Vec::with_capacity(4096),
            indices: Vec::with_capacity(6144),
            commands: Vec::with_capacity(64),
            clip_stack: Vec::with_capacity(16),
            current_clip: Rect::new(0.0, 0.0, 0.0, 0.0),
            current_texture: TextureId(0),
            open_index: 0,
            surface: Rect::new(0.0, 0.0, 0.0, 0.0),
            waterfall_store_span_db: 1.0,
            waterfall_display_span_db: 1.0,
            waterfall_inv_gamma: 1.0,
        }
    }

    /// Resets for a new frame. The white texture id is the default binding.
    pub fn begin(&mut self, width: f32, height: f32, white: TextureId) {
        self.vertices.clear();
        self.indices.clear();
        self.commands.clear();
        self.clip_stack.clear();
        self.surface = Rect::new(0.0, 0.0, width, height);
        self.current_clip = self.surface;
        self.current_texture = white;
        self.open_index = 0;
        // Reset to a mapping that is arithmetically harmless, so a frame drawn
        // before the waterfall has stated its own does not divide by nought.
        self.waterfall_store_span_db = 1.0;
        self.waterfall_display_span_db = 1.0;
        self.waterfall_inv_gamma = 1.0;
    }

    /// Closes the trailing command. Must be called before the list is handed
    /// to the renderer.
    pub fn end(&mut self) {
        self.flush_command();
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    fn flush_command(&mut self) {
        let count = self.indices.len() as u32 - self.open_index;
        if count > 0 {
            self.commands.push(DrawCmd {
                index_offset: self.open_index,
                index_count: count,
                texture: self.current_texture,
                clip: [
                    self.current_clip.x.floor() as i32,
                    self.current_clip.y.floor() as i32,
                    self.current_clip.right().ceil() as i32,
                    self.current_clip.bottom().ceil() as i32,
                ],
            });
            self.open_index = self.indices.len() as u32;
        }
    }

    /// Intersects with the active clip, so nested panels cannot draw outside
    /// their parent.
    pub fn push_clip(&mut self, rect: Rect) {
        self.flush_command();
        self.clip_stack.push(self.current_clip);
        self.current_clip = self.current_clip.intersect(&rect);
    }

    pub fn pop_clip(&mut self) {
        self.flush_command();
        self.current_clip = self.clip_stack.pop().unwrap_or(self.surface);
    }

    pub fn clip(&self) -> Rect {
        self.current_clip
    }

    pub fn set_texture(&mut self, id: TextureId) {
        if id != self.current_texture {
            self.flush_command();
            self.current_texture = id;
        }
    }

    /// Emits a quad. Vertices are given clockwise starting at the top left.
    fn quad(
        &mut self,
        p0: [f32; 2],
        p1: [f32; 2],
        p2: [f32; 2],
        p3: [f32; 2],
        uv0: [f32; 2],
        uv1: [f32; 2],
        color: Color,
        mode: Mode,
    ) {
        let base = self.vertices.len() as u32;
        let c = color.0;
        let m = mode as u32;
        self.vertices.push(Vertex { pos: p0, uv: [uv0[0], uv0[1]], color: c, mode: m });
        self.vertices.push(Vertex { pos: p1, uv: [uv1[0], uv0[1]], color: c, mode: m });
        self.vertices.push(Vertex { pos: p2, uv: [uv1[0], uv1[1]], color: c, mode: m });
        self.vertices.push(Vertex { pos: p3, uv: [uv0[0], uv1[1]], color: c, mode: m });
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn fill_rect(&mut self, r: Rect, color: Color) {
        if r.is_empty() || color.0[3] == 0 {
            return;
        }
        self.quad(
            [r.x, r.y],
            [r.right(), r.y],
            [r.right(), r.bottom()],
            [r.x, r.bottom()],
            [0.0, 0.0],
            [1.0, 1.0],
            color,
            Mode::Solid,
        );
    }

    /// Sharp one pixel style border drawn as four rectangles. Corners are not
    /// mitred, which matches the flat visual style.
    pub fn stroke_rect(&mut self, r: Rect, thickness: f32, color: Color) {
        if r.is_empty() || thickness <= 0.0 {
            return;
        }
        let t = thickness.min(r.w * 0.5).min(r.h * 0.5);
        self.fill_rect(Rect::new(r.x, r.y, r.w, t), color);
        self.fill_rect(Rect::new(r.x, r.bottom() - t, r.w, t), color);
        self.fill_rect(Rect::new(r.x, r.y + t, t, r.h - t * 2.0), color);
        self.fill_rect(Rect::new(r.right() - t, r.y + t, t, r.h - t * 2.0), color);
    }

    /// Axis aligned separator. Rounded to whole pixels so a one pixel line
    /// never lands between two texels and turns grey.
    pub fn hline(&mut self, x0: f32, x1: f32, y: f32, thickness: f32, color: Color) {
        let y = y.round();
        self.fill_rect(Rect::new(x0.round(), y, (x1 - x0).round(), thickness), color);
    }

    pub fn vline(&mut self, x: f32, y0: f32, y1: f32, thickness: f32, color: Color) {
        let x = x.round();
        self.fill_rect(Rect::new(x, y0.round(), thickness, (y1 - y0).round()), color);
    }

    /// Arbitrary line segment, used by the spectrum trace.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: Color) {
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0.0001 {
            return;
        }
        // Offset perpendicular to the segment by half the thickness.
        let nx = -dy / len * thickness * 0.5;
        let ny = dx / len * thickness * 0.5;
        self.quad(
            [x0 + nx, y0 + ny],
            [x1 + nx, y1 + ny],
            [x1 - nx, y1 - ny],
            [x0 - nx, y0 - ny],
            [0.0, 0.0],
            [1.0, 1.0],
            color,
            Mode::Solid,
        );
    }

    /// Textured quad. uv is in normalized texture coordinates.
    pub fn image(&mut self, r: Rect, texture: TextureId, uv_min: [f32; 2], uv_max: [f32; 2], tint: Color, mode: Mode) {
        if r.is_empty() {
            return;
        }
        self.set_texture(texture);
        self.quad(
            [r.x, r.y],
            [r.right(), r.y],
            [r.right(), r.bottom()],
            [r.x, r.bottom()],
            uv_min,
            uv_max,
            tint,
            mode,
        );
    }

    /// Vertical two colour gradient, used for subtle panel headers.
    pub fn gradient_v(&mut self, r: Rect, top: Color, bottom: Color) {
        if r.is_empty() {
            return;
        }
        let base = self.vertices.len() as u32;
        let m = Mode::Solid as u32;
        self.vertices.push(Vertex { pos: [r.x, r.y], uv: [0.0, 0.0], color: top.0, mode: m });
        self.vertices.push(Vertex { pos: [r.right(), r.y], uv: [1.0, 0.0], color: top.0, mode: m });
        self.vertices.push(Vertex { pos: [r.right(), r.bottom()], uv: [1.0, 1.0], color: bottom.0, mode: m });
        self.vertices.push(Vertex { pos: [r.x, r.bottom()], uv: [0.0, 1.0], color: bottom.0, mode: m });
        self.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

impl Default for DrawList {
    fn default() -> Self {
        DrawList::new()
    }
}