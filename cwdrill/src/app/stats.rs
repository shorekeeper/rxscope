//! Statistics picture.
//!
//! ## Why the matrix is a texture
//!
//! Forty characters against forty is sixteen hundred cells. Drawn as rectangles
//! that is sixteen hundred quads on every frame to show data that changes once a
//! group; as a texture it is one quad and one upload when a group is scored.
//! That is what a texture is for, and it is the only place in this application
//! where the distinction is measurable.
//!
//! ## Why the palette rather than a brightness
//!
//! The counts span orders of magnitude: a pair confused twice and a pair confused
//! forty times both matter, and a linear brightness shows only the second. A heat
//! ramp separates them, and a gamma below one lifts the low end so a pair
//! confused twice is visible rather than being the same black as a pair never
//! confused at all.
//!
//! The mapping happens in the shader, which is what lets the gamma move without
//! rewriting the texture.

use crate::core::Result;
use crate::progress::{Progress, OMITTED};
use crate::render::{Renderer, TextureId};

/// Characters each axis holds.
///
/// The whole alphabet plus the digits and the punctuation the material offers,
/// which is what the widest set reaches. A texture is allocated once at this size
/// and the used part is addressed by its texture coordinates, because reallocating
/// on a level change would mean draining the device to do it.
pub const MAX_SIDE: u32 = 64;

/// Entries the palette holds.
const PALETTE_ENTRIES: usize = 256;

/// Gamma applied to the matrix.
///
/// Below one, which lifts the low end: a pair confused twice out of a peak of
/// forty is a twentieth, and at unity that is indistinguishable from never.
pub const MATRIX_GAMMA: f32 = 0.45;

pub struct Stats {
    matrix: TextureId,
    /// Characters on each axis, sent along one and received along the other.
    chars: Vec<char>,
    pixels: Vec<u8>,
    /// True while the texture holds something other than what the history says.
    dirty: bool,
    /// Set the axes were built for, so a level change is noticed.
    built_for: String,
    peak: u32,
}

impl Stats {
    pub fn new(renderer: &mut Renderer) -> Result<Stats> {
        let side = MAX_SIDE as usize;
        let pixels = vec![0u8; side * side];
        // Nearest rather than linear: a cell is a count and interpolating two of
        // them would invent a pair that was never confused.
        let matrix = renderer.create_texture_r8(MAX_SIDE, MAX_SIDE, &pixels, true)?;

        let stats = Stats {
            matrix,
            chars: Vec::with_capacity(side),
            pixels,
            dirty: true,
            built_for: String::new(),
            peak: 0,
        };
        stats.upload_palette(renderer)?;
        Ok(stats)
    }

    pub fn matrix(&self) -> TextureId {
        self.matrix
    }

    /// Characters of the axes, in order.
    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    /// Largest count in the matrix, which is what the picture is scaled against.
    pub fn peak(&self) -> u32 {
        self.peak
    }

    /// Fraction of the texture the axes occupy.
    ///
    /// The used part rather than the whole, so the unused remainder is not drawn
    /// as a black margin that reads as data.
    pub fn extent(&self) -> f32 {
        if self.chars.is_empty() {
            0.0
        } else {
            self.chars.len() as f32 / MAX_SIDE as f32
        }
    }

    /// Marks the picture as behind the history.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Rebuilds the texture when the history moved.
    ///
    /// The set is taken rather than derived, because the axes have to be the
    /// characters the student is working on: the whole alphabet would put thirty
    /// empty rows above the four that matter.
    ///
    /// The omission column is appended to the received axis. It is the most
    /// informative entry in the matrix, because a character never written is one
    /// the ear did not hear rather than one it heard as something else.
    pub fn refresh(
        &mut self,
        set: &str,
        progress: &Progress,
        renderer: &mut Renderer,
    ) -> Result<()> {
        if !self.dirty && self.built_for == set {
            return Ok(());
        }
        self.dirty = false;
        self.built_for = set.to_string();

        self.chars.clear();
        for ch in set.chars() {
            if self.chars.len() >= MAX_SIDE as usize - 1 {
                break;
            }
            self.chars.push(ch);
        }
        // Last, so the diagonal of the square part still means what it looks
        // like: sent against received of the same character.
        self.chars.push(OMITTED);

        for v in self.pixels.iter_mut() {
            *v = 0;
        }

        self.peak = 0;
        let side = MAX_SIDE as usize;
        for (row, &sent) in self.chars.iter().enumerate() {
            if sent == OMITTED {
                continue;
            }
            for (column, &typed) in self.chars.iter().enumerate() {
                let count = progress.confusion_of(sent, typed);
                if count == 0 {
                    continue;
                }
                if count > self.peak {
                    self.peak = count;
                }
                self.pixels[row * side + column] = count.min(255) as u8;
            }
        }

        // Scaled after the peak is known, so the brightest cell reaches the top
        // of the ramp whatever the counts happen to be: a fixed scale would leave
        // a fresh history entirely black and a long one entirely saturated.
        if self.peak > 0 {
            let scale = 255.0 / self.peak as f32;
            for v in self.pixels.iter_mut() {
                if *v > 0 {
                    *v = ((*v as f32) * scale).round().min(255.0) as u8;
                }
            }
        }

        renderer.queue_texture_update(self.matrix, 0, 0, MAX_SIDE, MAX_SIDE, &self.pixels)
    }

    /// Writes the heat ramp the shader samples.
    ///
    /// Uploaded once. The ramp is dark blue through red to yellow, which is the
    /// convention every instrument uses for a count and is read without a legend.
    fn upload_palette(&self, renderer: &mut Renderer) -> Result<()> {
        let stops: [(f32, [f32; 3]); 5] = [
            (0.00, [0.05, 0.05, 0.08]),
            (0.25, [0.15, 0.15, 0.45]),
            (0.55, [0.60, 0.18, 0.30]),
            (0.80, [0.90, 0.45, 0.10]),
            (1.00, [1.00, 0.92, 0.45]),
        ];

        let mut bytes = Vec::with_capacity(PALETTE_ENTRIES * 4);
        for i in 0..PALETTE_ENTRIES {
            let t = i as f32 / (PALETTE_ENTRIES - 1) as f32;
            let mut colour = stops[stops.len() - 1].1;
            for pair in stops.windows(2) {
                let (t0, c0) = pair[0];
                let (t1, c1) = pair[1];
                if t <= t1 {
                    let f = ((t - t0) / (t1 - t0).max(1e-6)).clamp(0.0, 1.0);
                    colour = [
                        c0[0] + (c1[0] - c0[0]) * f,
                        c0[1] + (c1[1] - c0[1]) * f,
                        c0[2] + (c1[2] - c0[2]) * f,
                    ];
                    break;
                }
            }
            bytes.push((colour[0] * 255.0) as u8);
            bytes.push((colour[1] * 255.0) as u8);
            bytes.push((colour[2] * 255.0) as u8);
            bytes.push(255);
        }

        let target = renderer.palette_texture();
        renderer.queue_texture_update(target, 0, 0, PALETTE_ENTRIES as u32, 1, &bytes)
    }
}