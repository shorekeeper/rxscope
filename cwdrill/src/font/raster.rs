//! Glyph rasterizer.
//!
//! The method is signed area accumulation. Every edge deposits a signed
//! coverage delta into the cells it crosses; a prefix sum over the whole
//! buffer then turns those deltas into per pixel coverage. Compared to a
//! sorted edge list this needs no sorting, handles self intersecting outlines
//! reasonably and produces exact analytic antialiasing for straight edges.
//!
//! The accumulation runs across row boundaries on purpose: a closed contour
//! contributes a net zero per row, so the running sum returns to zero at the
//! end of each row as long as the bitmap is wide enough to contain the glyph.
//!
//! Coordinates are device pixels with y pointing down. The caller converts
//! from font units and flips the axis.

use crate::font::ttf::{Outline, Point};

pub struct Rasterizer {
    width: usize,
    height: usize,
    /// Signed coverage deltas. Four extra cells absorb the writes one column
    /// past the right edge that the edge routine can produce.
    area: Vec<f32>,
    /// Reused buffer for transformed contour points.
    scratch: Vec<Point>,
}

impl Rasterizer {
    pub fn new() -> Rasterizer {
        Rasterizer { width: 0, height: 0, area: Vec::new(), scratch: Vec::new() }
    }

    /// Prepares a clean canvas of the requested size.
    pub fn begin(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        let needed = width * height + 4;
        if self.area.len() < needed {
            self.area.resize(needed, 0.0);
        }
        for cell in self.area[..needed].iter_mut() {
            *cell = 0.0;
        }
    }

    /// Draws all contours of an outline.
    ///
    /// The mapping is x_device = x_font * scale - origin_x and
    /// y_device = origin_y - y_font * scale, where origin_x is the left edge
    /// of the bitmap in scaled font space and origin_y is its top edge.
    pub fn fill(&mut self, outline: &Outline, scale: f32, origin_x: f32, origin_y: f32) {
        // The scratch buffer is moved out so the emit routine can borrow the
        // rasterizer mutably while reading the points.
        let mut points = std::mem::take(&mut self.scratch);
        for contour in &outline.contours {
            points.clear();
            points.reserve(contour.len());
            for p in contour {
                points.push(Point {
                    x: p.x * scale - origin_x,
                    y: origin_y - p.y * scale,
                    on_curve: p.on_curve,
                });
            }
            self.emit_contour(&points);
        }
        self.scratch = points;
    }

    /// Walks one contour and converts it into line and quadratic segments.
    ///
    /// TrueType contours may start on an off curve point and may place two
    /// control points in a row, in which case an on curve point is implied
    /// halfway between them.
    fn emit_contour(&mut self, pts: &[Point]) {
        let n = pts.len();
        if n < 2 {
            return;
        }

        // Choose the start point and the index the walk begins at.
        let (start_x, start_y, first) = if pts[0].on_curve {
            (pts[0].x, pts[0].y, 1usize)
        } else if pts[n - 1].on_curve {
            (pts[n - 1].x, pts[n - 1].y, 0usize)
        } else {
            ((pts[0].x + pts[n - 1].x) * 0.5, (pts[0].y + pts[n - 1].y) * 0.5, 0usize)
        };

        let mut cur_x = start_x;
        let mut cur_y = start_y;
        let mut control: Option<(f32, f32)> = None;

        for k in 0..n {
            let p = pts[(first + k) % n];
            if p.on_curve {
                match control.take() {
                    Some((cx, cy)) => self.quad(cur_x, cur_y, cx, cy, p.x, p.y),
                    None => self.line(cur_x, cur_y, p.x, p.y),
                }
                cur_x = p.x;
                cur_y = p.y;
            } else {
                if let Some((cx, cy)) = control {
                    let mx = (cx + p.x) * 0.5;
                    let my = (cy + p.y) * 0.5;
                    self.quad(cur_x, cur_y, cx, cy, mx, my);
                    cur_x = mx;
                    cur_y = my;
                }
                control = Some((p.x, p.y));
            }
        }

        // Close the loop. A zero length segment is rejected by the edge
        // routine, so closing an already closed contour is harmless.
        match control {
            Some((cx, cy)) => self.quad(cur_x, cur_y, cx, cy, start_x, start_y),
            None => self.line(cur_x, cur_y, start_x, start_y),
        }
    }

    /// Flattens a quadratic segment into straight edges.
    ///
    /// The subdivision count comes from the deviation of the control point
    /// from the chord midpoint. The fourth root keeps the flattening error
    /// proportional to the tolerance rather than to the curve size.
    fn quad(&mut self, x0: f32, y0: f32, cx: f32, cy: f32, x1: f32, y1: f32) {
        let dev_x = x0 - 2.0 * cx + x1;
        let dev_y = y0 - 2.0 * cy + y1;
        let dev_sq = dev_x * dev_x + dev_y * dev_y;
        if dev_sq < 0.333 {
            self.line(x0, y0, x1, y1);
            return;
        }

        const TOLERANCE: f32 = 3.0;
        let steps = (1 + (TOLERANCE * dev_sq).sqrt().sqrt().floor() as usize).min(64);
        let step = 1.0 / steps as f32;

        let mut px = x0;
        let mut py = y0;
        let mut t = 0.0f32;
        for _ in 0..steps {
            t += step;
            let mt = 1.0 - t;
            let qx = mt * mt * x0 + 2.0 * mt * t * cx + t * t * x1;
            let qy = mt * mt * y0 + 2.0 * mt * t * cy + t * t * y1;
            self.line(px, py, qx, qy);
            px = qx;
            py = qy;
        }
    }

    /// Accumulates the signed coverage of one straight edge.
    ///
    /// The edge is processed one scanline at a time. Within a scanline the
    /// exact trapezoid area to the left of the edge is distributed over the
    /// touched cells, which is what makes the antialiasing analytic.
    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        if (y0 - y1).abs() <= f32::EPSILON {
            // Horizontal edges contribute nothing to the vertical sweep.
            return;
        }
        // Direction carries the winding sign; the edge is walked downwards.
        let (dir, sx, sy, ex, ey) =
            if y0 < y1 { (1.0f32, x0, y0, x1, y1) } else { (-1.0f32, x1, y1, x0, y0) };

        let dxdy = (ex - sx) / (ey - sy);
        let mut x = sx;
        if sy < 0.0 {
            // Advance to the top of the canvas before the loop starts.
            x -= sy * dxdy;
        }

        let y_start = sy.max(0.0) as usize;
        let y_end = (ey.ceil().max(0.0) as usize).min(self.height);

        for y in y_start..y_end {
            let row = y * self.width;
            // Height of the edge inside this scanline.
            let dy = ((y + 1) as f32).min(ey) - (y as f32).max(sy);
            let x_next = x + dxdy * dy;
            let d = dy * dir;

            let (left, right) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let left_floor = left.floor();
            let left_i = left_floor as i32;
            let right_ceil = right.ceil();
            let right_i = right_ceil as i32;

            if right_i <= left_i + 1 {
                // The edge stays inside a single cell; split the area between
                // that cell and its right neighbour by the horizontal centre.
                let mid = 0.5 * (x + x_next) - left_floor;
                self.add(row, left_i, d - d * mid);
                self.add(row, left_i + 1, d * mid);
            } else {
                // The edge spans several cells. The first and last cells get
                // triangular pieces, the middle cells get equal slices.
                let inv_span = (right - left).recip();
                let first_frac = left - left_floor;
                let first_area = 0.5 * inv_span * (1.0 - first_frac) * (1.0 - first_frac);
                let last_frac = right - right_ceil + 1.0;
                let last_area = 0.5 * inv_span * last_frac * last_frac;

                self.add(row, left_i, d * first_area);
                if right_i == left_i + 2 {
                    self.add(row, left_i + 1, d * (1.0 - first_area - last_area));
                } else {
                    let a1 = inv_span * (1.5 - first_frac);
                    self.add(row, left_i + 1, d * (a1 - first_area));
                    for xi in left_i + 2..right_i - 1 {
                        self.add(row, xi, d * inv_span);
                    }
                    let a2 = a1 + (right_i - left_i - 3) as f32 * inv_span;
                    self.add(row, right_i - 1, d * (1.0 - a2 - last_area));
                }
                self.add(row, right_i, d * last_area);
            }

            x = x_next;
        }
    }

    /// Adds a delta to one cell. Columns left of the canvas are folded into
    /// column zero so the running sum stays correct for a shape that starts
    /// outside the bitmap; columns past the end land in the slack cells.
    #[inline]
    fn add(&mut self, row: usize, column: i32, value: f32) {
        let column = column.max(0) as usize;
        if let Some(cell) = self.area.get_mut(row + column) {
            *cell += value;
        }
    }

    /// Runs the prefix sum and writes an eight bit coverage mask.
    /// The gamma table is applied here so the atlas holds final values.
    pub fn finish(&self, gamma: &[u8; 256], out: &mut Vec<u8>) {
        let count = self.width * self.height;
        out.clear();
        out.reserve(count);

        let mut acc = 0.0f32;
        for i in 0..count {
            acc += self.area[i];
            // The absolute value implements a non zero style fill; overlapping
            // contours saturate instead of cancelling.
            let coverage = acc.abs().min(1.0);
            out.push(gamma[(coverage * 255.0) as usize]);
        }
    }
}

impl Default for Rasterizer {
    fn default() -> Self {
        Rasterizer::new()
    }
}