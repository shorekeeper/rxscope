//! Single channel glyph atlas with shelf packing.
//!
//! Shelf packing keeps rows of a fixed height and appends glyphs left to
//! right. It wastes some vertical space compared to a full bin packer but the
//! insert is constant time and the layout never has to be rebuilt, which
//! matters because a rebuild would invalidate every cached texture coordinate.
//!
//! Uploads are tracked as a range of full rows. Full rows make the staging
//! copy a contiguous slice of the backing buffer, so no repacking is needed
//! before handing the data to the renderer.

pub struct Atlas {
    pub width: u32,
    pub height: u32,
    pixels: Vec<u8>,
    shelves: Vec<Shelf>,
    /// Half open row range waiting for upload; y1 <= y0 means clean.
    dirty_y0: u32,
    dirty_y1: u32,
}

struct Shelf {
    /// Top row of the shelf.
    y: u32,
    /// Fixed height, set by the first glyph placed on it.
    height: u32,
    /// Next free column.
    x: u32,
}

impl Atlas {
    pub fn new(width: u32, height: u32) -> Atlas {
        Atlas {
            width,
            height,
            pixels: vec![0u8; (width as usize) * (height as usize)],
            shelves: Vec::new(),
            dirty_y0: u32::MAX,
            dirty_y1: 0,
        }
    }

    /// Full backing buffer, used for the initial texture upload.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Reserves a rectangle and returns its top left corner.
    /// Returns None when the atlas is full.
    pub fn insert(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w == 0 || h == 0 || w > self.width || h > self.height {
            return None;
        }

        for shelf in self.shelves.iter_mut() {
            // Reuse a shelf only when the glyph is not far shorter than the
            // shelf height, otherwise a tall shelf fills up with small glyphs
            // and the wasted band never gets reclaimed.
            let fits_height = h <= shelf.height && h * 4 >= shelf.height * 3;
            if fits_height && shelf.x + w <= self.width {
                let position = (shelf.x, shelf.y);
                shelf.x += w;
                return Some(position);
            }
        }

        let y = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
        if y + h > self.height {
            return None;
        }
        self.shelves.push(Shelf { y, height: h, x: w });
        Some((0, y))
    }

    /// Copies a tightly packed glyph bitmap into the backing buffer and marks
    /// the affected rows for upload.
    pub fn write(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        let width = self.width as usize;
        for row in 0..h {
            let src = (row as usize) * (w as usize);
            let dst = ((y + row) as usize) * width + x as usize;
            let n = w as usize;
            if src + n <= data.len() && dst + n <= self.pixels.len() {
                self.pixels[dst..dst + n].copy_from_slice(&data[src..src + n]);
            }
        }
        self.dirty_y0 = self.dirty_y0.min(y);
        self.dirty_y1 = self.dirty_y1.max(y + h);
    }

    /// Returns the pending row range and clears the dirty marker.
    /// The tuple is the first row, the row count and the pixel data.
    pub fn take_dirty(&mut self) -> Option<(u32, u32, &[u8])> {
        if self.dirty_y1 <= self.dirty_y0 {
            return None;
        }
        let y0 = self.dirty_y0.min(self.height);
        let y1 = self.dirty_y1.min(self.height);
        self.dirty_y0 = u32::MAX;
        self.dirty_y1 = 0;
        if y1 <= y0 {
            return None;
        }

        let start = (y0 as usize) * (self.width as usize);
        let end = (y1 as usize) * (self.width as usize);
        Some((y0, y1 - y0, &self.pixels[start..end]))
    }

    /// Occupied fraction of the atlas height, for diagnostics.
    pub fn used_fraction(&self) -> f32 {
        let used = self.shelves.last().map(|s| s.y + s.height).unwrap_or(0);
        used as f32 / self.height as f32
    }
}