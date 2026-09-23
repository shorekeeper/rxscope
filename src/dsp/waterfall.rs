//! Waterfall history on a GPU texture.
//!
//! The texture is a ring in both directions, and the two rings answer two
//! different questions.
//!
//! Vertically it is a ring because a new line must not cost a shift of the
//! whole image: the head advances and the drawing walks from oldest to newest,
//! which puts the newest line at the bottom without any copy.
//!
//! Horizontally it is a ring because the audio span moves in frequency whenever
//! the dial does. A station keeps its place on the band and therefore changes
//! its place in the audio, so the record has to follow it or the picture stops
//! being a picture of the band.
//!
//! ## What the drawing is asked for
//!
//! The view arrives as two fractions of the stored span. They state what the
//! rectangle shows, not where inside it the picture goes, so a view narrower
//! than the span is stretched across the whole width. That is what
//! magnification means: placing the picture at its own fractions instead
//! divides it by the magnification and leaves it standing wherever the window
//! happens to sit.
//!
//! ## Why a retune copies nothing and erases nothing
//!
//! A retune moves the horizontal origin and nothing else. The origin alone is
//! enough for alignment: a line written at an earlier dial position, read
//! through the current origin, comes out at the frequencies it was captured
//! from.
//!
//! What the origin cannot express is how much of that line the current span
//! still reaches. A line holds the whole span as it was, so once the record has
//! slid by n columns the line describes audio columns n upwards and the
//! remainder of it is the far edge wrapped round rather than a reading. The
//! write position of every row is therefore recorded, and the drawing covers
//! only the part each row still describes.
//!
//! Erasing the exposed columns is the alternative and it is destructive: a dial
//! moved out and back erases at both edges and restores neither, so a few
//! minutes of ordinary tuning leaves the whole history blank. It also costs a
//! full height upload of zeros per retune, which for a kilohertz scale step is
//! megabytes through the staging buffer. Recording the position costs eight
//! bytes per row and one quad per group of rows that share it.
//!
//! ## Two storage arrangements
//!
//! The mapped one stores one byte per pixel holding the level above the bottom
//! of its own line, and the palette, the display span and the gamma are applied
//! by the shader. Four times less memory and four times less upload bandwidth,
//! and a change to any of those three repaints the whole history rather than
//! only the lines drawn after it.
//!
//! The direct one stores the colour itself. It costs four bytes per pixel and
//! cannot repaint history, and it exists because it needs nothing of the
//! fragment shader beyond a plain textured quad.
//!
//! The choice is made once at construction. Switching would mean a differently
//! formatted texture, and the bytes already stored cannot be reinterpreted.
//!
//! ## Level mapping
//!
//! Three arrangements, differing only in where the bottom of the palette sits.
//!
//! Absolute mapping puts it at the configured floor. The display then states
//! real signal strength, and a receiver whose gain is calibrated can be read
//! off the colours. The cost is that the picture follows the propagation: a
//! band that fades goes dark, and a band that lifts saturates.
//!
//! Tracked mapping puts it on a slow estimate of the noise floor. That removes
//! the drift over minutes while leaving the shorter variation intact, which is
//! what an operator watching one frequency wants.
//!
//! Per line mapping puts it on the floor measured from the line itself. Fading
//! is then removed entirely and every carrier reads at the brightness its own
//! signal to noise ratio earns, which is the arrangement a wide survey of keyed
//! carriers needs. Absolute level is no longer recoverable from the colour, and
//! that is the whole trade.
//!
//! The floor is a median rather than a mean. A mean is pulled up by every strong
//! carrier in the line, so on a busy band it would report a floor well above the
//! noise and the mapping would erase exactly the weak signals it exists to show.

use crate::config::settings::{ColorMap, WaterfallStyle};
use crate::core::Result;
use crate::render::{Color, DrawList, Mode, Rect, Renderer, TextureId};

use super::colormap;

/// Rate the tracked floor follows the measured one, per line. At the default
/// line rate this settles in about two seconds, which is longer than any keying
/// gap and shorter than a fade.
const FLOOR_TRACK: f32 = 0.02;

/// Decibels one stored step represents in the mapped arrangement.
///
/// Half a decibel. The palette holds two hundred and fifty six entries over a
/// display span that is typically a hundred decibels, so one palette entry is
/// four tenths of a decibel: a coarser step would be visible as banding on a
/// steady carrier, and a finer one cannot be shown.
const STORE_STEP_DB: f32 = 0.5;

/// Range a stored byte covers.
///
/// A display span beyond this is clipped at the top rather than rescaled. The
/// configuration permits a wider one in principle, but a receiver display of
/// more than a hundred and twenty seven decibels has no useful bottom: the
/// weakest quarter of it is below the noise of any sound card.
const STORE_SPAN_DB: f32 = 255.0 * STORE_STEP_DB;

/// Entries in the palette texture, which is what the shader interpolates over.
const PALETTE_ENTRIES: u32 = 256;

pub struct Waterfall {
    width: u32,
    height: u32,
    texture: TextureId,
    /// True while the level is stored and the colour is applied by the shader.
    mapped: bool,
    /// True while stored columns are interpolated rather than shown as blocks.
    smooth: bool,
    /// Bytes one pixel occupies in the texture.
    stride: usize,
    /// Row the next line goes to. Also the oldest row currently held.
    head: u32,
    /// Texture column holding the leftmost audio column.
    ///
    /// The horizontal ring. Audio column c lives at texture column
    /// (c + origin) modulo the width, so a retune moves this rather than the
    /// pixels.
    origin: u32,
    /// Columns the record has been slid since the texture was last cleared.
    ///
    /// Monotonic in the sense that it accumulates signed movement, so the
    /// difference against a stored value is the displacement between two
    /// moments and needs no modular interpretation.
    total: i64,
    /// Value of the counter above when each row was written.
    ///
    /// One entry per texture row rather than per line, because a row is reused
    /// and the entry has to describe whatever is in it now.
    written_at: Vec<i64>,
    /// Rows written since the last clear, for the initial fill.
    filled: u32,
    palette: Vec<[u8; 4]>,
    kind: ColorMap,
    /// True while the palette texture holds a stale table.
    palette_dirty: bool,
    row: Vec<u8>,
    /// Spectrum reduced to the texture width, in decibels.
    reduced: Vec<f32>,
    /// Temporally smoothed copy of the reduced row.
    smoothed: Vec<f32>,
    /// Sorting scratch for the median, so a line does not allocate.
    scratch: Vec<f32>,
    min_db: f32,
    max_db: f32,
    gamma: f32,
    style: WaterfallStyle,
    auto_range: bool,
    smoothing: f32,
    /// Slow noise floor estimate, in decibels.
    tracked_floor: f32,
    /// False until the first line has seeded the smoother and the tracker.
    primed: bool,
    lines: u64,
}

impl Waterfall {
    pub fn new(
        width: u32,
        height: u32,
        kind: ColorMap,
        mapped: bool,
        smooth: bool,
        renderer: &mut Renderer,
    ) -> Result<Waterfall> {
        let width = width.clamp(64, 16384);
        let height = height.clamp(64, 8192);
        let stride = if mapped { 1 } else { 4 };

        // The texture starts at nought so the empty part of the history is not
        // filled with whatever the allocation happened to contain. In the mapped
        // arrangement nought is the bottom of the palette, which reads the same
        // as the transparent black of the direct one.
        let zeros = vec![0u8; (width as usize) * (height as usize) * stride];
        let texture = if mapped {
            renderer.create_texture_r8(width, height, &zeros, !smooth)?
        } else {
            renderer.create_texture_rgba8(width, height, &zeros, !smooth)?
        };

        crate::log_info!(
            "dsp",
            "waterfall texture {}x{}, {}, {}, {:.1} MB",
            width,
            height,
            if mapped { "one channel mapped in the shader" } else { "colour" },
            if smooth { "interpolated" } else { "blocks" },
            zeros.len() as f32 / (1024.0 * 1024.0)
        );

        let mut waterfall = Waterfall {
            width,
            height,
            texture,
            mapped,
            smooth,
            stride,
            head: 0,
            origin: 0,
            total: 0,
            written_at: vec![0i64; height as usize],
            filled: 0,
            palette: colormap::build(kind),
            kind,
            palette_dirty: true,
            row: vec![0u8; (width as usize) * stride],
            reduced: vec![0.0; width as usize],
            smoothed: vec![0.0; width as usize],
            scratch: vec![0.0; width as usize],
            min_db: -120.0,
            max_db: -20.0,
            gamma: 1.0,
            style: WaterfallStyle::Classic,
            auto_range: false,
            smoothing: 0.0,
            tracked_floor: -120.0,
            primed: false,
            lines: 0,
        };

        // Uploaded here rather than left to the first line, so a display that
        // has not received audio yet still has a valid palette bound.
        waterfall.flush_palette(renderer)?;
        Ok(waterfall)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn lines(&self) -> u64 {
        self.lines
    }

    /// Rows the history texture holds.
    ///
    /// The whole of it reaches the screen, so this and the line rate are what the
    /// age of a row is computed from.
    pub fn rows(&self) -> u32 {
        self.height
    }

    /// True while the colour is applied by the shader.
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// Applies the interpolation setting.
    ///
    /// Nothing is rewritten: the setting names which sampler the descriptor
    /// points at, and the stored bytes mean the same either way.
    pub fn set_smooth(&mut self, smooth: bool, renderer: &mut Renderer) -> Result<()> {
        if smooth == self.smooth {
            return Ok(());
        }
        self.smooth = smooth;
        renderer.set_texture_filter(self.texture, !smooth)
    }

    /// Noise floor the mapping is currently anchored to, for a readout.
    pub fn floor_db(&self) -> f32 {
        self.tracked_floor
    }

    pub fn set_colormap(&mut self, kind: ColorMap) {
        if kind != self.kind {
            self.kind = kind;
            self.palette = colormap::build(kind);
            self.palette_dirty = true;
        }
    }

    /// Display range.
    ///
    /// In the mapped arrangement the ceiling and the gamma reach the shader and
    /// therefore repaint the whole history. The floor does so only under a per
    /// line or tracked mapping, where it is a span rather than an anchor; under
    /// absolute mapping it is subtracted at write time and applies to new lines
    /// alone.
    pub fn set_range(&mut self, min_db: f32, max_db: f32, gamma: f32) {
        self.min_db = min_db;
        self.max_db = max_db.max(min_db + 1.0);
        self.gamma = gamma.clamp(0.2, 4.0);
    }

    /// Mapping arrangement and the temporal smoothing applied before it.
    pub fn set_mapping(&mut self, style: WaterfallStyle, auto_range: bool, smoothing: f32) {
        self.style = style;
        self.auto_range = auto_range;
        self.smoothing = smoothing.clamp(0.0, 0.95);
    }

    /// Slides the record sideways by whole columns.
    ///
    /// A positive count means the content moves towards higher audio, which is
    /// what happens to a station when the dial moves the span underneath it.
    ///
    /// No pixel is touched. The origin carries the alignment and the per row
    /// write position carries how much of each row the span still reaches, so a
    /// dial moved out and back leaves the picture exactly as it was.
    ///
    /// A count at or beyond the width is the exception. Nothing stored describes
    /// a frequency inside the span any longer, and only the opposite move of the
    /// same size would bring any of it back, so the history is dropped rather
    /// than carried as an offset that reaches nothing.
    pub fn shift(&mut self, columns: i32, renderer: &mut Renderer) -> Result<()> {
        if columns == 0 {
            return Ok(());
        }
        if columns.unsigned_abs() >= self.width {
            self.origin = 0;
            self.total = 0;
            for slot in self.written_at.iter_mut() {
                *slot = 0;
            }
            // A whole texture is megabytes, so it is cleared by the device
            // rather than by uploading that many zeros.
            return renderer.queue_texture_clear(self.texture);
        }

        let width = self.width as i64;
        self.total += columns as i64;
        self.origin = ((self.origin as i64 - columns as i64).rem_euclid(width)) as u32;
        Ok(())
    }

    /// Adds one line. Bins are reduced to the texture width by taking the
    /// strongest bin of each column, which preserves a narrow carrier that
    /// averaging would wash out.
    pub fn push(&mut self, bins: &[f32], renderer: &mut Renderer) -> Result<()> {
        if bins.is_empty() {
            return Ok(());
        }
        if self.palette_dirty {
            self.flush_palette(renderer)?;
        }

        let w = self.width as usize;
        let n = bins.len();

        for x in 0..w {
            let lo = x * n / w;
            let hi = ((x + 1) * n / w).max(lo + 1).min(n);
            let mut peak = f32::MIN;
            for &v in &bins[lo..hi] {
                if v > peak {
                    peak = v;
                }
            }
            self.reduced[x] = peak;
        }

        // Temporal smoothing. Applied to the reduced row rather than to the
        // spectrum so its cost does not depend on the transform size, and after
        // the column maximum so it cannot hide a carrier that a single line
        // caught.
        let source: &[f32] = if self.smoothing > 0.01 {
            if !self.primed {
                self.smoothed.copy_from_slice(&self.reduced);
            } else {
                let alpha = 1.0 - self.smoothing;
                for (s, &v) in self.smoothed.iter_mut().zip(self.reduced.iter()) {
                    *s += alpha * (v - *s);
                }
            }
            &self.smoothed
        } else {
            &self.reduced
        };

        let relative = self.style == WaterfallStyle::Skimmer || self.auto_range;
        let mut bottom = self.min_db;
        if relative {
            self.scratch.copy_from_slice(source);
            self.scratch
                .sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let measured = self.scratch[self.scratch.len() / 2];

            if !self.primed {
                self.tracked_floor = measured;
            } else {
                self.tracked_floor += FLOOR_TRACK * (measured - self.tracked_floor);
            }
            bottom = if self.style == WaterfallStyle::Skimmer {
                measured
            } else {
                self.tracked_floor
            };
        }

        if self.mapped {
            // The level above the bottom, quantized on a scale the shader knows.
            // Nothing else is applied: the span, the gamma and the palette are
            // the shader's business precisely so a change to them repaints what
            // is already stored.
            for x in 0..w {
                let steps = ((source[x] - bottom) / STORE_STEP_DB).round();
                self.row[x] = steps.clamp(0.0, 255.0) as u8;
            }
        } else {
            let span = (self.max_db - self.min_db).max(1.0);
            let inv_gamma = 1.0 / self.gamma;
            for x in 0..w {
                let t = ((source[x] - bottom) / span)
                    .clamp(0.0, 1.0)
                    .powf(inv_gamma);
                let c = self.palette[(t * 255.0) as usize];
                let o = x * 4;
                self.row[o] = c[0];
                self.row[o + 1] = c[1];
                self.row[o + 2] = c[2];
                self.row[o + 3] = 255;
            }
        }

        // The row is built in audio order and stored in ring order, so it
        // reaches the texture as at most two contiguous pieces.
        let start = self.origin;
        let head = self.width - start;
        renderer.queue_texture_update(
            self.texture,
            start,
            self.head,
            head,
            1,
            &self.row[..head as usize * self.stride],
        )?;
        if head < self.width {
            renderer.queue_texture_update(
                self.texture,
                0,
                self.head,
                self.width - head,
                1,
                &self.row[head as usize * self.stride..],
            )?;
        }

        // The row covers the whole span as of now, which is what the counter
        // records. Written before the head advances, so it describes the row
        // that was just filled rather than the one that will be.
        self.written_at[self.head as usize] = self.total;

        self.head = (self.head + 1) % self.height;
        if self.filled < self.height {
            self.filled += 1;
        }
        self.lines += 1;
        self.primed = true;
        Ok(())
    }

    /// Draws the history into the rectangle, oldest at the top.
    ///
    /// The range is given as audio fractions of the full span, ascending, with
    /// the mirror stated separately. The two fractions say what the rectangle
    /// covers, so the view is stretched across the whole width; the mirror
    /// reverses the screen mapping and the texture range together.
    ///
    /// Rows are walked oldest first, which is also write order, so rows that
    /// share a dial position form one contiguous group and cost one quad. A
    /// static dial therefore emits the same one or two quads a plain image
    /// would, and a dial being turned emits one more group per movement.
    pub fn draw(&self, list: &mut DrawList, r: Rect, a0: f32, a1: f32, mirrored: bool) {
        if r.is_empty() {
            return;
        }
        let a0 = a0.clamp(0.0, 1.0);
        let a1 = a1.clamp(0.0, 1.0);
        let view = a1 - a0;
        if view <= 1e-6 {
            return;
        }

        let mode = if self.mapped {
            // The mapping travels as frame state rather than per quad, because
            // the quads below describe one picture and a per quad copy would be
            // one chance per quad for them to disagree.
            list.waterfall_store_span_db = STORE_SPAN_DB;
            list.waterfall_display_span_db = (self.max_db - self.min_db).max(1.0);
            list.waterfall_inv_gamma = 1.0 / self.gamma;
            Mode::Waterfall
        } else {
            Mode::Rgba
        };

        let columns = self.width as f32;
        let rows = self.height as f32;
        let origin = self.origin as f32 / columns;
        // Audio fraction at which the ring wraps back to the start of the
        // texture. With the origin at nought this sits at the right edge and no
        // split occurs, which is the case before any retune.
        let wrap = 1.0 - origin;

        // Screen position of an audio fraction. The mirror is applied here and
        // nowhere else, so the texture range and the destination cannot disagree
        // about which end of the view they describe. Rounded at the call site,
        // and deterministic, so two pieces meeting at one fraction land on the
        // same pixel and leave neither gap nor overlap.
        let to_x = |p: f32| -> f32 {
            let t = ((p - a0) / view).clamp(0.0, 1.0);
            if mirrored {
                r.x + r.w * (1.0 - t)
            } else {
                r.x + r.w * t
            }
        };

        let tint = Color::rgb(255, 255, 255);
        let height = self.height as usize;
        let head = self.head as usize;
        // Walk position at which the row index wraps. A group must not cross it,
        // because the two halves are not contiguous in the texture.
        let seam = height - head;

        let mut k = 0usize;
        while k < height {
            let first = (head + k) % height;
            let moved = self.total - self.written_at[first];

            let mut end = k + 1;
            while end < height && end != seam {
                let row = (head + end) % height;
                if self.total - self.written_at[row] != moved {
                    break;
                }
                end += 1;
            }

            // Audio the group still describes. A row holds the whole span as it
            // was written, so once the record has slid by `moved` columns the
            // data covers audio columns from there upwards and everything
            // outside that is the far edge wrapped round rather than a reading.
            let m = moved as f32 / columns;
            let lo = a0.max(m.max(0.0));
            let hi = a1.min((m + 1.0).min(1.0));
            if hi - lo > 1e-6 {
                let y0 = (r.y + r.h * (k as f32 / rows)).round();
                // One pixel of overlap into the group below, which is drawn
                // later and paints over it, so a boundary cannot fall between
                // two pixels and leave a transparent line.
                let y1 = (r.y + r.h * (end as f32 / rows) + 1.0)
                    .min(r.bottom())
                    .round();
                let v0 = first as f32 / rows;
                let v1 = (first + (end - k)) as f32 / rows;

                if y1 > y0 {
                    let mut pieces: [(f32, f32); 2] = [(lo, hi), (0.0, 0.0)];
                    let mut count = 1usize;
                    if lo < wrap && wrap < hi {
                        pieces[0] = (lo, wrap);
                        pieces[1] = (wrap, hi);
                        count = 2;
                    }

                    for pi in 0..count {
                        let (pa, pb) = pieces[pi];
                        if pb - pa <= 1e-6 {
                            continue;
                        }
                        let mut ua = pa + origin;
                        if ua >= 1.0 {
                            ua -= 1.0;
                        }
                        let ub = (ua + (pb - pa)).min(1.0);

                        let xa = to_x(pa).round();
                        let xb = to_x(pb).round();
                        let (x0, x1, tx0, tx1) = if xa <= xb {
                            (xa, xb, ua, ub)
                        } else {
                            (xb, xa, ub, ua)
                        };
                        if x1 <= x0 {
                            continue;
                        }
                        list.image(
                            Rect::from_min_max(x0, y0, x1, y1),
                            self.texture,
                            [tx0, v0],
                            [tx1, v1],
                            tint,
                            mode,
                        );
                    }
                }
            }

            k = end;
        }
    }

    /// Pushes the palette table into the texture the shader samples.
    ///
    /// A no operation in the direct arrangement, where the table is consulted on
    /// the processor and the texture holds colour already.
    fn flush_palette(&mut self, renderer: &mut Renderer) -> Result<()> {
        self.palette_dirty = false;
        if !self.mapped {
            return Ok(());
        }
        let target = renderer.palette_texture();
        let mut bytes = Vec::with_capacity(PALETTE_ENTRIES as usize * 4);
        for entry in self.palette.iter().take(PALETTE_ENTRIES as usize) {
            bytes.extend_from_slice(entry);
        }
        renderer.queue_texture_update(target, 0, 0, PALETTE_ENTRIES, 1, &bytes)
    }
}