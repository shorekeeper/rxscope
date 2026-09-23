//! TrueType font file parsing.
//!
//! Scope: the tables needed to place and rasterize glyphs, nothing else.
//!   head  units per em and the loca index format
//!   hhea  ascender, descender, line gap, horizontal metric count
//!   maxp  glyph count
//!   hmtx  advance widths
//!   loca  glyph data offsets
//!   glyf  outlines, simple and composite
//!   cmap  character to glyph mapping, formats 0, 4, 6 and 12
//!   kern  legacy pair kerning, format 0 horizontal subtables
//!
//! Not supported: CFF outlines (OpenType files with an OTTO signature),
//! hinting programs, GPOS and GSUB. Modern faces carry their kerning in GPOS,
//! so an empty kern map is the normal case and not an error.
//!
//! All multi byte values in a font file are big endian. Every read is bounds
//! checked; a truncated or malformed file produces an error or an empty
//! outline instead of a panic.

use std::collections::HashMap;

use crate::core::{Error, Result};

fn err(message: &str) -> Error {
    Error::font(message)
}

/// Packs a four character table tag into the integer form used in the file.
const fn tag(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

fn read_u16(data: &[u8], off: usize) -> Result<u16> {
    if off + 2 > data.len() {
        return Err(err("read past the end of the font file"));
    }
    Ok(((data[off] as u16) << 8) | data[off + 1] as u16)
}

fn read_i16(data: &[u8], off: usize) -> Result<i16> {
    Ok(read_u16(data, off)? as i16)
}

fn read_u32(data: &[u8], off: usize) -> Result<u32> {
    if off + 4 > data.len() {
        return Err(err("read past the end of the font file"));
    }
    Ok(((data[off] as u32) << 24)
        | ((data[off + 1] as u32) << 16)
        | ((data[off + 2] as u32) << 8)
        | (data[off + 3] as u32))
}

/// Sequential reader for the variable length parts of a glyph record.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn at(data: &'a [u8], pos: usize) -> Reader<'a> {
        Reader { data, pos }
    }

    fn skip(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    fn u8(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.pos)
            .ok_or_else(|| err("glyph record ended early"))?;
        self.pos += 1;
        Ok(b)
    }

    fn u16(&mut self) -> Result<u16> {
        let v = read_u16(self.data, self.pos)?;
        self.pos += 2;
        Ok(v)
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }
}

/// Outline point in font units. Off curve points are quadratic control
/// points; TrueType never uses cubic curves in glyf.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

/// Closed contours of one glyph, already expanded for composites.
#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub contours: Vec<Vec<Point>>,
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

/// Affine transform in the row vector convention used by composite glyphs:
/// x' = a*x + c*y + dx, y' = b*x + d*y + dy.
#[derive(Debug, Clone, Copy)]
struct Transform {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    dx: f32,
    dy: f32,
}

impl Transform {
    fn identity() -> Transform {
        Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, dx: 0.0, dy: 0.0 }
    }

    fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.dx, self.b * x + self.d * y + self.dy)
    }

    /// Returns the transform that applies self first and outer second.
    fn then(&self, outer: &Transform) -> Transform {
        Transform {
            a: self.a * outer.a + self.b * outer.c,
            b: self.a * outer.b + self.b * outer.d,
            c: self.c * outer.a + self.d * outer.c,
            d: self.c * outer.b + self.d * outer.d,
            dx: self.dx * outer.a + self.dy * outer.c + outer.dx,
            dy: self.dx * outer.b + self.dy * outer.d + outer.dy,
        }
    }
}

/// Selected cmap subtable. The offset is absolute inside the file.
#[derive(Debug, Clone, Copy)]
enum Cmap {
    None,
    Format0(usize),
    Format4(usize),
    Format6(usize),
    Format12(usize),
}

pub struct Font {
    data: Vec<u8>,
    pub units_per_em: f32,
    /// Vertical metrics in font units, taken from hhea. The descender is
    /// negative, matching the file convention.
    pub ascender: f32,
    pub descender: f32,
    pub line_gap: f32,
    pub num_glyphs: u16,
    pub name: String,

    num_h_metrics: u16,
    hmtx_off: usize,
    glyf_off: usize,
    glyf_len: usize,
    /// Byte offsets into glyf, num_glyphs + 1 entries.
    loca: Vec<u32>,
    cmap: Cmap,
    /// Symbol fonts map their glyphs into the private use area at 0xF000.
    cmap_symbol: bool,
    /// Key is the left glyph in the high half and the right glyph in the low
    /// half; value is the adjustment in font units.
    kern: HashMap<u32, i16>,
}

impl Font {
    /// Parses a font file image. The buffer is kept because glyph records are
    /// decoded lazily on first use.
    pub fn parse(data: Vec<u8>) -> Result<Font> {
        // A collection file starts with a ttcf header holding a table of
        // offsets; only the first face is used.
        let mut base = 0usize;
        if read_u32(&data, 0)? == tag(b"ttcf") {
            let count = read_u32(&data, 8)?;
            if count == 0 {
                return Err(err("font collection is empty"));
            }
            base = read_u32(&data, 12)? as usize;
        }

        let version = read_u32(&data, base)?;
        if version == tag(b"OTTO") {
            return Err(err("OpenType CFF outlines are not supported"));
        }
        if version != 0x0001_0000 && version != tag(b"true") {
            return Err(err("unrecognized font format"));
        }

        // The table directory holds sixteen bytes per record: tag, checksum,
        // offset and length.
        let num_tables = read_u16(&data, base + 4)? as usize;
        let mut tables: HashMap<u32, (usize, usize)> = HashMap::with_capacity(num_tables);
        for i in 0..num_tables {
            let rec = base + 12 + i * 16;
            let t = read_u32(&data, rec)?;
            let off = read_u32(&data, rec + 8)? as usize;
            let len = read_u32(&data, rec + 12)? as usize;
            // Truncated files are common in the wild; clamp instead of
            // rejecting the whole face.
            if off < data.len() {
                tables.insert(t, (off, len.min(data.len() - off)));
            }
        }

        let head = tables.get(&tag(b"head")).copied().ok_or_else(|| err("head table missing"))?;
        let hhea = tables.get(&tag(b"hhea")).copied().ok_or_else(|| err("hhea table missing"))?;
        let maxp = tables.get(&tag(b"maxp")).copied().ok_or_else(|| err("maxp table missing"))?;
        let hmtx = tables.get(&tag(b"hmtx")).copied().ok_or_else(|| err("hmtx table missing"))?;
        let loca_t = tables.get(&tag(b"loca")).copied().ok_or_else(|| err("loca table missing"))?;
        let glyf = tables.get(&tag(b"glyf")).copied().ok_or_else(|| err("glyf table missing"))?;

        let units_per_em = read_u16(&data, head.0 + 18)? as f32;
        if units_per_em <= 0.0 {
            return Err(err("head reports zero units per em"));
        }
        let index_to_loc = read_i16(&data, head.0 + 50)?;

        let ascender = read_i16(&data, hhea.0 + 4)? as f32;
        let descender = read_i16(&data, hhea.0 + 6)? as f32;
        let line_gap = read_i16(&data, hhea.0 + 8)? as f32;
        let num_h_metrics = read_u16(&data, hhea.0 + 34)?;

        let num_glyphs = read_u16(&data, maxp.0 + 4)?;

        // loca stores either halved sixteen bit offsets or plain thirty two
        // bit offsets, selected by indexToLocFormat.
        let entries = num_glyphs as usize + 1;
        let mut loca = Vec::with_capacity(entries);
        if index_to_loc == 0 {
            for i in 0..entries {
                let o = loca_t.0 + i * 2;
                if o + 2 > loca_t.0 + loca_t.1 {
                    break;
                }
                loca.push(read_u16(&data, o)? as u32 * 2);
            }
        } else {
            for i in 0..entries {
                let o = loca_t.0 + i * 4;
                if o + 4 > loca_t.0 + loca_t.1 {
                    break;
                }
                loca.push(read_u32(&data, o)?);
            }
        }
        if loca.len() < 2 {
            return Err(err("loca table is too short"));
        }

        let (cmap, cmap_symbol) = match tables.get(&tag(b"cmap")) {
            Some(&(off, _)) => select_cmap(&data, off)?,
            None => (Cmap::None, false),
        };
        if matches!(cmap, Cmap::None) {
            return Err(err("no usable cmap subtable"));
        }

        let kern = match tables.get(&tag(b"kern")) {
            Some(&(off, len)) => parse_kern(&data, off, len),
            None => HashMap::new(),
        };

        let name = match tables.get(&tag(b"name")) {
            Some(&(off, _)) => parse_name(&data, off).unwrap_or_else(|| "unnamed".to_string()),
            None => "unnamed".to_string(),
        };

        crate::log_info!(
            "font",
            "{}: {} glyphs, {} upem, ascent {}, descent {}, kern pairs {}",
            name,
            num_glyphs,
            units_per_em,
            ascender,
            descender,
            kern.len()
        );

        Ok(Font {
            data,
            units_per_em,
            ascender,
            descender,
            line_gap,
            num_glyphs,
            name,
            num_h_metrics,
            hmtx_off: hmtx.0,
            glyf_off: glyf.0,
            glyf_len: glyf.1,
            loca,
            cmap,
            cmap_symbol,
            kern,
        })
    }

    /// Maps a character to a glyph index, zero meaning notdef.
    pub fn glyph_index(&self, ch: char) -> u16 {
        let cp = ch as u32;
        let g = self.lookup(cp);
        if g != 0 {
            return g;
        }
        // Symbol encodings place the printable range at 0xF000 upwards.
        if self.cmap_symbol && cp < 0x100 {
            return self.lookup(0xF000 + cp);
        }
        0
    }

    fn lookup(&self, cp: u32) -> u16 {
        let r = match self.cmap {
            Cmap::None => Ok(0),
            Cmap::Format0(off) => lookup_format0(&self.data, off, cp),
            Cmap::Format4(off) => lookup_format4(&self.data, off, cp),
            Cmap::Format6(off) => lookup_format6(&self.data, off, cp),
            Cmap::Format12(off) => lookup_format12(&self.data, off, cp),
        };
        r.unwrap_or(0)
    }

    /// Advance width in font units. Glyphs past the metric array reuse the
    /// last entry, which is how monospaced tails are encoded.
    pub fn advance(&self, glyph: u16) -> f32 {
        let n = self.num_h_metrics as usize;
        if n == 0 {
            return 0.0;
        }
        let i = (glyph as usize).min(n - 1);
        read_u16(&self.data, self.hmtx_off + i * 4).unwrap_or(0) as f32
    }

    /// Pair adjustment in font units, zero when the face has no kern table.
    pub fn kerning(&self, left: u16, right: u16) -> f32 {
        if self.kern.is_empty() {
            return 0.0;
        }
        let key = ((left as u32) << 16) | right as u32;
        self.kern.get(&key).copied().unwrap_or(0) as f32
    }

    /// Decodes the outline of one glyph. Returns None for empty glyphs such
    /// as the space character and for records that fail validation.
    pub fn outline(&self, glyph: u16) -> Option<Outline> {
        let mut out = Outline::default();
        if self.load_outline(glyph, &mut out, Transform::identity(), 0).is_err() {
            return None;
        }
        if out.contours.is_empty() {
            return None;
        }

        // The bounding box is recomputed from the points rather than taken
        // from the glyph header: composite headers are unreliable and control
        // points may reach outside the stated box.
        let mut x_min = f32::MAX;
        let mut y_min = f32::MAX;
        let mut x_max = f32::MIN;
        let mut y_max = f32::MIN;
        for contour in &out.contours {
            for p in contour {
                x_min = x_min.min(p.x);
                y_min = y_min.min(p.y);
                x_max = x_max.max(p.x);
                y_max = y_max.max(p.y);
            }
        }
        if !x_min.is_finite() || !y_min.is_finite() || x_max <= x_min || y_max <= y_min {
            return None;
        }
        out.x_min = x_min;
        out.y_min = y_min;
        out.x_max = x_max;
        out.y_max = y_max;
        Some(out)
    }

    fn load_outline(
        &self,
        glyph: u16,
        out: &mut Outline,
        xf: Transform,
        depth: u32,
    ) -> Result<()> {
        // Composite glyphs may nest; five levels is far beyond what real
        // fonts use and stops a malformed file from recursing forever.
        if depth > 5 {
            return Ok(());
        }
        let gi = glyph as usize;
        if gi + 1 >= self.loca.len() {
            return Ok(());
        }
        let start = self.loca[gi] as usize;
        let end = self.loca[gi + 1] as usize;
        if end <= start {
            // Equal offsets mark a glyph without an outline.
            return Ok(());
        }
        if end > self.glyf_len {
            return Err(err("loca entry points outside glyf"));
        }

        let g_off = self.glyf_off + start;
        let g_len = end - start;
        let num_contours = read_i16(&self.data, g_off)?;
        if num_contours >= 0 {
            self.parse_simple(g_off, num_contours as usize, out, xf)
        } else {
            self.parse_composite(g_off, g_len, out, xf, depth)
        }
    }

    /// Simple glyph layout: contour end indices, then the instruction block,
    /// then run length encoded flags, then delta encoded coordinates.
    fn parse_simple(
        &self,
        g_off: usize,
        n: usize,
        out: &mut Outline,
        xf: Transform,
    ) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        let mut r = Reader::at(&self.data, g_off + 10);

        let mut ends = Vec::with_capacity(n);
        for _ in 0..n {
            ends.push(r.u16()? as usize);
        }
        let num_points = match ends.last() {
            Some(&e) => e + 1,
            None => return Ok(()),
        };
        // A sane upper bound, the format allows at most 65535 points.
        if num_points > 10_000 {
            return Err(err("glyph point count is out of range"));
        }

        let instruction_len = r.u16()? as usize;
        r.skip(instruction_len);

        // Flag bit 3 repeats the previous flag byte a given number of times.
        const ON_CURVE: u8 = 0x01;
        const X_SHORT: u8 = 0x02;
        const Y_SHORT: u8 = 0x04;
        const REPEAT: u8 = 0x08;
        const X_SAME_OR_POSITIVE: u8 = 0x10;
        const Y_SAME_OR_POSITIVE: u8 = 0x20;

        let mut flags = Vec::with_capacity(num_points);
        while flags.len() < num_points {
            let f = r.u8()?;
            flags.push(f);
            if f & REPEAT != 0 {
                let count = r.u8()?;
                for _ in 0..count {
                    if flags.len() >= num_points {
                        break;
                    }
                    flags.push(f);
                }
            }
        }

        // Coordinates are deltas. A short delta is one unsigned byte whose
        // sign comes from the SAME_OR_POSITIVE bit; a long delta is signed.
        // When the short bit is clear and SAME is set, the delta is zero.
        let mut xs = Vec::with_capacity(num_points);
        let mut x = 0i32;
        for &f in &flags {
            if f & X_SHORT != 0 {
                let d = r.u8()? as i32;
                x += if f & X_SAME_OR_POSITIVE != 0 { d } else { -d };
            } else if f & X_SAME_OR_POSITIVE == 0 {
                x += r.i16()? as i32;
            }
            xs.push(x);
        }

        let mut ys = Vec::with_capacity(num_points);
        let mut y = 0i32;
        for &f in &flags {
            if f & Y_SHORT != 0 {
                let d = r.u8()? as i32;
                y += if f & Y_SAME_OR_POSITIVE != 0 { d } else { -d };
            } else if f & Y_SAME_OR_POSITIVE == 0 {
                y += r.i16()? as i32;
            }
            ys.push(y);
        }

        let mut start = 0usize;
        for &e in &ends {
            if e < start || e >= num_points {
                // Malformed contour table, keep whatever was decoded so far.
                break;
            }
            let mut contour = Vec::with_capacity(e - start + 1);
            for i in start..=e {
                let (px, py) = xf.apply(xs[i] as f32, ys[i] as f32);
                contour.push(Point { x: px, y: py, on_curve: flags[i] & ON_CURVE != 0 });
            }
            if contour.len() >= 2 {
                out.contours.push(contour);
            }
            start = e + 1;
        }
        Ok(())
    }

    /// Composite glyph: a chain of component references with an optional
    /// scale, each contributing the contours of another glyph.
    fn parse_composite(
        &self,
        g_off: usize,
        g_len: usize,
        out: &mut Outline,
        xf: Transform,
        depth: u32,
    ) -> Result<()> {
        const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY_VALUES: u16 = 0x0002;
        const WE_HAVE_A_SCALE: u16 = 0x0008;
        const MORE_COMPONENTS: u16 = 0x0020;
        const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
        const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;

        let limit = g_off + g_len;
        let mut r = Reader::at(&self.data, g_off + 10);

        loop {
            if r.pos + 4 > limit {
                break;
            }
            let flags = r.u16()?;
            let index = r.u16()?;

            let (arg1, arg2) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                (r.i16()? as f32, r.i16()? as f32)
            } else {
                (r.u8()? as i8 as f32, r.u8()? as i8 as f32)
            };
            // The point matching form aligns two contours by point index and
            // is not implemented; such a component lands at the origin.
            let (dx, dy) = if flags & ARGS_ARE_XY_VALUES != 0 { (arg1, arg2) } else { (0.0, 0.0) };

            // Scale factors are F2Dot14, a signed fixed point value with
            // fourteen fraction bits.
            let f2dot14 = |v: i16| v as f32 / 16384.0;
            let mut component = Transform { a: 1.0, b: 0.0, c: 0.0, d: 1.0, dx, dy };
            if flags & WE_HAVE_A_SCALE != 0 {
                let s = f2dot14(r.i16()?);
                component.a = s;
                component.d = s;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                component.a = f2dot14(r.i16()?);
                component.d = f2dot14(r.i16()?);
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                component.a = f2dot14(r.i16()?);
                component.b = f2dot14(r.i16()?);
                component.c = f2dot14(r.i16()?);
                component.d = f2dot14(r.i16()?);
            }

            self.load_outline(index, out, component.then(&xf), depth + 1)?;

            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
        Ok(())
    }
}

/// Picks the most capable cmap subtable. Full Unicode tables win over the
/// basic plane, and a symbol table is the last resort.
fn select_cmap(data: &[u8], off: usize) -> Result<(Cmap, bool)> {
    let count = read_u16(data, off + 2)? as usize;
    let mut best: Option<(u32, usize, bool)> = None;

    for i in 0..count {
        let rec = off + 4 + i * 8;
        let platform = read_u16(data, rec)?;
        let encoding = read_u16(data, rec + 2)?;
        let sub = off + read_u32(data, rec + 4)? as usize;
        if sub + 4 > data.len() {
            continue;
        }
        let score: u32 = match (platform, encoding) {
            (3, 10) => 100, // Windows, full Unicode
            (0, 4) | (0, 6) => 95,
            (3, 1) => 90, // Windows, basic multilingual plane
            (0, _) => 85, // Unicode platform, any encoding
            (3, 0) => 70, // Windows symbol
            (1, 0) => 10, // Macintosh Roman
            _ => 1,
        };
        let symbol = platform == 3 && encoding == 0;
        if best.map(|(s, _, _)| score > s).unwrap_or(true) {
            best = Some((score, sub, symbol));
        }
    }

    let (_, sub, symbol) = match best {
        Some(b) => b,
        None => return Ok((Cmap::None, false)),
    };

    let format = read_u16(data, sub)?;
    let cmap = match format {
        0 => Cmap::Format0(sub),
        4 => Cmap::Format4(sub),
        6 => Cmap::Format6(sub),
        12 => Cmap::Format12(sub),
        other => {
            crate::log_warn!("font", "cmap format {} is not supported", other);
            Cmap::None
        }
    };
    Ok((cmap, symbol))
}

/// Byte encoding table, a flat array of 256 glyph indices.
fn lookup_format0(data: &[u8], off: usize, cp: u32) -> Result<u16> {
    if cp > 255 {
        return Ok(0);
    }
    Ok(data.get(off + 6 + cp as usize).copied().unwrap_or(0) as u16)
}

/// Segmented mapping. Layout after the twelve byte header:
/// endCode, one padding word, startCode, idDelta, idRangeOffset.
fn lookup_format4(data: &[u8], off: usize, cp: u32) -> Result<u16> {
    if cp > 0xFFFF {
        return Ok(0);
    }
    let cp = cp as u16;
    let seg_count = read_u16(data, off + 6)? as usize / 2;
    if seg_count == 0 {
        return Ok(0);
    }
    let end_codes = off + 14;
    let start_codes = end_codes + seg_count * 2 + 2;
    let id_deltas = start_codes + seg_count * 2;
    let id_ranges = id_deltas + seg_count * 2;

    // Find the first segment whose end code is not below the code point.
    let mut lo = 0usize;
    let mut hi = seg_count;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if read_u16(data, end_codes + mid * 2)? < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo >= seg_count {
        return Ok(0);
    }

    let start = read_u16(data, start_codes + lo * 2)?;
    if cp < start {
        return Ok(0);
    }
    let delta = read_u16(data, id_deltas + lo * 2)?;
    let range_offset = read_u16(data, id_ranges + lo * 2)?;

    // A zero range offset means the glyph is the code point plus the delta,
    // both wrapping at sixteen bits. Otherwise the offset is a byte distance
    // from its own slot into the glyph index array.
    if range_offset == 0 {
        return Ok(cp.wrapping_add(delta));
    }
    let slot = id_ranges + lo * 2 + range_offset as usize + (cp - start) as usize * 2;
    let g = read_u16(data, slot)?;
    if g == 0 {
        Ok(0)
    } else {
        Ok(g.wrapping_add(delta))
    }
}

/// Trimmed table mapping a contiguous code range.
fn lookup_format6(data: &[u8], off: usize, cp: u32) -> Result<u16> {
    let first = read_u16(data, off + 6)? as u32;
    let count = read_u16(data, off + 8)? as u32;
    if cp < first || cp >= first + count {
        return Ok(0);
    }
    read_u16(data, off + 10 + ((cp - first) as usize) * 2)
}

/// Segmented coverage for code points beyond the basic plane.
fn lookup_format12(data: &[u8], off: usize, cp: u32) -> Result<u16> {
    let groups = read_u32(data, off + 12)? as usize;
    let base = off + 16;
    let mut lo = 0usize;
    let mut hi = groups;
    while lo < hi {
        let mid = (lo + hi) / 2;
        let g = base + mid * 12;
        let start = read_u32(data, g)?;
        let end = read_u32(data, g + 4)?;
        if cp < start {
            hi = mid;
        } else if cp > end {
            lo = mid + 1;
        } else {
            let first_glyph = read_u32(data, g + 8)?;
            return Ok((first_glyph + (cp - start)) as u16);
        }
    }
    Ok(0)
}

/// Legacy kerning. Only version zero with format zero horizontal subtables is
/// read; Apple style version one tables and class based formats are skipped.
fn parse_kern(data: &[u8], off: usize, len: usize) -> HashMap<u32, i16> {
    let mut map = HashMap::new();
    if read_u16(data, off).unwrap_or(1) != 0 {
        return map;
    }
    let subtables = read_u16(data, off + 2).unwrap_or(0) as usize;
    let limit = off + len;
    let mut p = off + 4;

    for _ in 0..subtables {
        if p + 14 > limit {
            break;
        }
        let length = read_u16(data, p + 2).unwrap_or(0) as usize;
        let coverage = read_u16(data, p + 4).unwrap_or(0);
        let horizontal = coverage & 0x01 != 0;
        let format = coverage >> 8;

        if horizontal && format == 0 {
            let pairs = read_u16(data, p + 6).unwrap_or(0) as usize;
            for i in 0..pairs {
                let e = p + 14 + i * 6;
                if e + 6 > limit {
                    break;
                }
                let left = read_u16(data, e).unwrap_or(0);
                let right = read_u16(data, e + 2).unwrap_or(0);
                let value = read_i16(data, e + 4).unwrap_or(0);
                if value != 0 {
                    map.insert(((left as u32) << 16) | right as u32, value);
                }
            }
        }

        if length == 0 {
            break;
        }
        p += length;
    }
    map
}

/// Reads the family name, preferring the Windows UTF-16 record. Used only for
/// logging, so a failure downgrades to a placeholder.
fn parse_name(data: &[u8], off: usize) -> Option<String> {
    let count = read_u16(data, off + 2).ok()? as usize;
    let storage = off + read_u16(data, off + 4).ok()? as usize;
    let mut fallback: Option<String> = None;

    for i in 0..count {
        let rec = off + 6 + i * 12;
        let platform = read_u16(data, rec).ok()?;
        let name_id = read_u16(data, rec + 6).ok()?;
        let length = read_u16(data, rec + 8).ok()? as usize;
        let str_off = storage + read_u16(data, rec + 10).ok()? as usize;
        if name_id != 1 || str_off + length > data.len() {
            continue;
        }

        if platform == 3 {
            // UTF-16 big endian.
            let mut units = Vec::with_capacity(length / 2);
            for k in (0..length).step_by(2) {
                units.push(read_u16(data, str_off + k).ok()?);
            }
            return Some(String::from_utf16_lossy(&units));
        }
        if fallback.is_none() {
            fallback = Some(String::from_utf8_lossy(&data[str_off..str_off + length]).into_owned());
        }
    }
    fallback
}