//! Text rendering: glyph cache, atlas management and string placement.
//!
//! Rasterization happens once per glyph and size, on demand, on the thread
//! that draws. A cache miss costs a few tens of microseconds; after the first
//! frames the working set is stable and no rasterization happens at all.
//!
//! Positioning is integer aligned. Glyph bitmaps are cut at pixel boundaries
//! and the pen is rounded before each quad, so an atlas texel maps one to one
//! onto a framebuffer pixel and the nearest neighbour sampler reproduces the
//! rasterizer output exactly. Subpixel positioning would need one cached
//! bitmap per phase and is not worth it for a fixed layout interface.

pub mod atlas;
pub mod raster;
pub mod system;
pub mod ttf;

use std::collections::HashMap;

use crate::core::{Error, Result};
use crate::render::{Color, DrawList, Mode, Rect, Renderer, TextureId};

use atlas::Atlas;
use raster::Rasterizer;
use ttf::Font;

/// Logical font slots. The interface face carries labels and values, the
/// monospaced face carries decoded text, frequencies and timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontId {
    Ui = 0,
    Mono = 1,
}

/// Vertical metrics in pixels at a given size.
#[derive(Debug, Clone, Copy)]
pub struct FontMetrics {
    /// Distance from the baseline to the top of the ascenders.
    pub ascent: f32,
    /// Distance from the baseline down to the bottom of the descenders.
    pub descent: f32,
    pub line_gap: f32,
    /// Baseline to baseline distance, rounded up to whole pixels so rows of
    /// text stay aligned to the pixel grid.
    pub line_height: f32,
}

/// Cache key. The size is stored in sixty fourths of a pixel so nearby sizes
/// share entries only when they are truly identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: u8,
    glyph: u16,
    size: u32,
}

/// Placement of a rasterized glyph inside the atlas.
#[derive(Debug, Clone, Copy)]
struct CachedGlyph {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    /// Bitmap size in pixels.
    width: f32,
    height: f32,
    /// Horizontal offset from the pen to the left edge of the bitmap.
    bearing_x: f32,
    /// Vertical offset from the baseline up to the top edge of the bitmap.
    bearing_y: f32,
}

pub struct FontSystem {
    fonts: Vec<Font>,
    /// Maps a logical slot to an index in fonts. Both slots point at the same
    /// face when only one file could be loaded.
    slots: [usize; 2],
    atlas: Atlas,
    raster: Rasterizer,
    /// A None value marks a glyph without a bitmap, such as the space, or one
    /// that could not be placed. It is cached so the failure is not retried
    /// on every frame.
    cache: HashMap<GlyphKey, Option<CachedGlyph>>,
    /// Reused destination for the rasterizer output.
    scratch: Vec<u8>,
    gamma: [u8; 256],
    texture: TextureId,
    /// Set once when the atlas runs out of room, to keep the log quiet.
    atlas_exhausted: bool,
}

/// Largest glyph bitmap accepted, in pixels per side. Guards against a
/// corrupt outline that scales to an absurd box.
const MAX_GLYPH_SIDE: usize = 512;

impl FontSystem {
    pub fn new(
        ui_path: &str,
        mono_path: &str,
        atlas_size: u32,
        gamma: f32,
        renderer: &mut Renderer,
    ) -> Result<FontSystem> {
        let mut fonts = Vec::with_capacity(2);

        let ui_file = system::find_font(ui_path, system::UI_CANDIDATES)
            .ok_or_else(|| Error::font("no interface font found"))?;
        crate::log_info!("font", "interface face: {}", ui_file.display());
        let ui_data = std::fs::read(&ui_file)
            .map_err(|e| Error::font(format!("{}: {}", ui_file.display(), e)))?;
        fonts.push(Font::parse(ui_data)?);

        // The monospaced face is optional; without it both slots share the
        // interface face and decoded text loses its column alignment but the
        // application still runs.
        let mut slots = [0usize, 0usize];
        if let Some(mono_file) = system::find_font(mono_path, system::MONO_CANDIDATES) {
            match std::fs::read(&mono_file) {
                Ok(data) => match Font::parse(data) {
                    Ok(f) => {
                        crate::log_info!("font", "monospaced face: {}", mono_file.display());
                        fonts.push(f);
                        slots[1] = 1;
                    }
                    Err(e) => crate::log_warn!("font", "{}: {}", mono_file.display(), e),
                },
                Err(e) => crate::log_warn!("font", "{}: {}", mono_file.display(), e),
            }
        } else {
            crate::log_warn!("font", "no monospaced face found, using the interface face");
        }

        let atlas = Atlas::new(atlas_size, atlas_size);
        // The full buffer is uploaded once so every texel starts at zero;
        // later uploads only touch the rows that changed.
        let texture = renderer.create_texture_r8(atlas_size, atlas_size, atlas.pixels(), true)?;

        crate::log_info!(
            "font",
            "atlas {}x{} texture {}, gamma {:.2}",
            atlas_size,
            atlas_size,
            texture.0,
            gamma
        );

        Ok(FontSystem {
            fonts,
            slots,
            atlas,
            raster: Rasterizer::new(),
            cache: HashMap::with_capacity(512),
            scratch: Vec::with_capacity(4096),
            gamma: build_gamma_table(gamma),
            texture,
            atlas_exhausted: false,
        })
    }

    pub fn texture(&self) -> TextureId {
        self.texture
    }

    pub fn cached_glyphs(&self) -> usize {
        self.cache.len()
    }

    /// Family name of the interface face, for the diagnostic block. A missing
    /// name table leaves the parser reporting a placeholder rather than failing,
    /// so this is never empty.
    pub fn family_name(&self) -> &str {
        &self.fonts[self.slots[FontId::Ui as usize]].name
    }

    pub fn atlas_used(&self) -> f32 {
        self.atlas.used_fraction()
    }

    fn font(&self, id: FontId) -> &Font {
        &self.fonts[self.slots[id as usize]]
    }

    pub fn metrics(&self, id: FontId, px: f32) -> FontMetrics {
        let f = self.font(id);
        let scale = px / f.units_per_em;
        // The hhea descender is negative, the reported descent is positive.
        let ascent = f.ascender * scale;
        let descent = -f.descender * scale;
        let line_gap = f.line_gap * scale;
        FontMetrics {
            ascent,
            descent,
            line_gap,
            line_height: (ascent + descent + line_gap).ceil(),
        }
    }

    pub fn line_height(&self, id: FontId, px: f32) -> f32 {
        self.metrics(id, px).line_height
    }

    /// Baseline that centres one line of text vertically in a box.
    /// Derived from top = y + (h - ascent - descent) / 2 and
    /// baseline = top + ascent.
    pub fn baseline_centered(&self, id: FontId, px: f32, rect_y: f32, rect_h: f32) -> f32 {
        let m = self.metrics(id, px);
        (rect_y + (rect_h + m.ascent - m.descent) * 0.5).round()
    }

    /// Advance width of a single line. Control characters are ignored, a tab
    /// advances four spaces.
    pub fn measure(&self, text: &str, id: FontId, px: f32) -> f32 {
        let f = self.font(id);
        let scale = px / f.units_per_em;
        let mut x = 0.0f32;
        let mut previous = 0u16;

        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            if ch == '\t' {
                let space = f.glyph_index(' ');
                x += f.advance(space) * scale * 4.0;
                previous = 0;
                continue;
            }
            let glyph = f.glyph_index(ch);
            if previous != 0 {
                x += f.kerning(previous, glyph) * scale;
            }
            x += f.advance(glyph) * scale;
            previous = glyph;
        }
        x
    }

    /// Longest prefix that fits into the given width, returned as a byte
    /// offset so it can be used to slice the input directly.
    pub fn fit(&self, text: &str, id: FontId, px: f32, max_width: f32) -> usize {
        let f = self.font(id);
        let scale = px / f.units_per_em;
        let mut x = 0.0f32;
        let mut previous = 0u16;

        for (offset, ch) in text.char_indices() {
            if ch == '\n' || ch == '\r' {
                return offset;
            }
            let advance = if ch == '\t' {
                previous = 0;
                f.advance(f.glyph_index(' ')) * scale * 4.0
            } else {
                let glyph = f.glyph_index(ch);
                let kern = if previous != 0 { f.kerning(previous, glyph) * scale } else { 0.0 };
                previous = glyph;
                kern + f.advance(glyph) * scale
            };
            if x + advance > max_width {
                return offset;
            }
            x += advance;
        }
        text.len()
    }

    /// Emits one line of text with the baseline at the given y.
    /// Returns the advance width actually consumed.
    pub fn draw_text(
        &mut self,
        list: &mut DrawList,
        x: f32,
        baseline: f32,
        text: &str,
        id: FontId,
        px: f32,
        color: Color,
    ) -> f32 {
        if text.is_empty() || color.0[3] == 0 {
            return 0.0;
        }

        let slot = self.slots[id as usize];
        let scale = px / self.fonts[slot].units_per_em;
        let size_key = (px * 64.0).round() as u32;
        let texture = self.texture;

        let mut pen = x;
        let mut previous = 0u16;

        // The character loop reads the font through an index rather than a
        // reference because the cache lookup needs the whole system mutably.
        let chars: Vec<char> = text.chars().collect();
        for ch in chars {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            if ch == '\t' {
                let space = self.fonts[slot].glyph_index(' ');
                pen += self.fonts[slot].advance(space) * scale * 4.0;
                previous = 0;
                continue;
            }

            let glyph = self.fonts[slot].glyph_index(ch);
            if previous != 0 {
                pen += self.fonts[slot].kerning(previous, glyph) * scale;
            }

            let key = GlyphKey { font: slot as u8, glyph, size: size_key };
            let entry = match self.cache.get(&key) {
                Some(e) => *e,
                None => {
                    let built = self.build_glyph(slot, glyph, px);
                    self.cache.insert(key, built);
                    built
                }
            };

            if let Some(g) = entry {
                // Both the pen and the bearings are snapped, so the quad lands
                // on whole pixels and the atlas maps one to one.
                let qx = (pen + g.bearing_x).round();
                let qy = (baseline - g.bearing_y).round();
                list.image(
                    Rect::new(qx, qy, g.width, g.height),
                    texture,
                    g.uv_min,
                    g.uv_max,
                    color,
                    Mode::Alpha,
                );
            }

            pen += self.fonts[slot].advance(glyph) * scale;
            previous = glyph;
        }

        pen - x
    }

    /// Rasterizes one glyph and places it in the atlas.
    fn build_glyph(&mut self, slot: usize, glyph: u16, px: f32) -> Option<CachedGlyph> {
        // The outline is copied out so the font borrow ends before the
        // rasterizer and the atlas are touched.
        let (outline, scale) = {
            let f = &self.fonts[slot];
            let scale = px / f.units_per_em;
            (f.outline(glyph)?, scale)
        };

        // Pixel aligned bounding box. One extra column and row of margin
        // absorb the coverage that the edge routine deposits just past the
        // right and bottom edges.
        let x0 = (outline.x_min * scale).floor();
        let y0 = (outline.y_min * scale).floor();
        let x1 = (outline.x_max * scale).ceil();
        let y1 = (outline.y_max * scale).ceil();

        let w = (x1 - x0) as i32 + 1;
        let h = (y1 - y0) as i32 + 1;
        if w <= 0 || h <= 0 {
            return None;
        }
        let w = w as usize;
        let h = h as usize;
        if w > MAX_GLYPH_SIDE || h > MAX_GLYPH_SIDE {
            crate::log_warn!("font", "glyph {} is {}x{} pixels, skipped", glyph, w, h);
            return None;
        }

        self.raster.begin(w, h);
        self.raster.fill(&outline, scale, x0, y1);
        self.raster.finish(&self.gamma, &mut self.scratch);

        // One pixel of padding on the right and bottom keeps neighbouring
        // glyphs from bleeding into each other under linear filtering.
        let (ax, ay) = match self.atlas.insert(w as u32 + 1, h as u32 + 1) {
            Some(p) => p,
            None => {
                if !self.atlas_exhausted {
                    self.atlas_exhausted = true;
                    crate::log_error!(
                        "font",
                        "glyph atlas is full at {:.0} percent, increase ui.glyph_atlas_size",
                        self.atlas.used_fraction() * 100.0
                    );
                }
                return None;
            }
        };
        self.atlas.write(ax, ay, w as u32, h as u32, &self.scratch);

        let aw = self.atlas.width as f32;
        let ah = self.atlas.height as f32;
        Some(CachedGlyph {
            uv_min: [ax as f32 / aw, ay as f32 / ah],
            uv_max: [(ax as usize + w) as f32 / aw, (ay as usize + h) as f32 / ah],
            width: w as f32,
            height: h as f32,
            bearing_x: x0,
            bearing_y: y1,
        })
    }

    /// Pushes the rows changed since the last call to the GPU. Runs once per
    /// frame; after the working set is cached it does nothing. The queued path
    /// records the copy into the frame command buffer, so a new glyph costs no
    /// stall.
    pub fn flush(&mut self, renderer: &mut Renderer) -> Result<()> {
        let texture = self.texture;
        let width = self.atlas.width;
        if let Some((y, rows, data)) = self.atlas.take_dirty() {
            renderer.queue_texture_update(texture, 0, y, width, rows, data)?;
        }
        Ok(())
    }
}

/// Maps linear coverage to stored alpha. A gamma above one lifts the partial
/// coverage values, which compensates for the perceived thinning of light
/// glyphs on a dark background.
fn build_gamma_table(gamma: f32) -> [u8; 256] {
    let inverse = 1.0 / gamma.clamp(0.5, 3.0);
    let mut table = [0u8; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let coverage = i as f32 / 255.0;
        *slot = (coverage.powf(inverse) * 255.0 + 0.5) as u8;
    }
    table
}