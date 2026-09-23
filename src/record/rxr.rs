//! Ring segment format.
//!
//! ## What the format has to do
//!
//! Three requirements, and the third is the one that rules out every existing
//! container.
//!
//! It must be writable from a thread that must not stall. That rules out
//! compression on the recording path: an encoder that occasionally takes a
//! millisecond longer than usual is an encoder that occasionally loses a block,
//! and a hole in a recording cannot be recovered from. The samples are
//! therefore stored raw and the compression happens on export, where a stall
//! costs nothing.
//!
//! It must be seekable by arithmetic. Scrubbing a recording like live audio
//! means landing on a position in constant time, so every block is the same
//! size and the offset of block n is a multiplication.
//!
//! It must carry what turns audio back into a picture of a band. A recording
//! that holds only samples can be played; one that holds the dial frequency
//! beside them can be replayed, with the spectrum labelled on the air and the
//! markers where they were. That is the whole difference between a sound file
//! and a record of a reception.
//!
//! ## Layout
//!
//! ```text
//!   header, 64 bytes
//!   block 0: marker, 32 bytes, then block_len frames
//!   block 1: ...
//! ```
//!
//! Everything is little endian, stated rather than inherited: the file may be
//! copied to another machine, and a reader that assumed the host order would
//! read a frequency of several terabytes rather than failing.
//!
//! The block count in the header is patched when the segment closes and is a
//! cross check rather than the truth. The truth is the file length, because a
//! process that died holds a header written before the samples were and a
//! reader that trusted it would report an empty recording.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::core::{Error, Result};

/// Four bytes at the head of every segment. Version is separate so a later
/// layout is refused rather than misread.
pub const MAGIC: [u8; 4] = *b"RXR1";
pub const VERSION: u16 = 1;

pub const HEADER_BYTES: usize = 64;
pub const MARKER_BYTES: usize = 32;

/// Sample layout inside a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    I16 = 0,
    F32 = 1,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::I16 => 2,
            SampleFormat::F32 => 4,
        }
    }

    fn from_code(code: u8) -> Option<SampleFormat> {
        match code {
            0 => Some(SampleFormat::I16),
            1 => Some(SampleFormat::F32),
            _ => None,
        }
    }
}

/// Flags in the header.
///
/// The quadrature flag is not cosmetic: a replay that treated a pair as two
/// copies of one channel would fold the spectrum about nought and every signal
/// below the tuning point would appear above it.
pub const FLAG_COMPLEX: u8 = 0x01;

/// Flags in a block marker.
pub const MARK_DIAL_VALID: u8 = 0x01;
pub const MARK_SDR_MODE: u8 = 0x02;

/// What was true at the start of one block.
///
/// Sampled per block rather than per sample, because none of it moves faster
/// than an operator can turn a dial, and per sample it would outweigh the audio.
#[derive(Debug, Clone, Copy, Default)]
pub struct Marker {
    /// Frequency the transceiver reported, meaningful only with the flag set.
    pub dial_hz: i64,
    /// Receiver oscillator, so a replay reproduces where it was listening.
    pub tune_hz: f32,
    /// Levels, so a scrub bar can be drawn without decoding the audio.
    pub peak_db: f32,
    pub rms_db: f32,
    /// Transceiver mode code, zero when unknown. The mapping is the one the
    /// control library uses, kept as a number so this format does not depend on
    /// its enumeration.
    pub mode: u8,
    /// Which side of the dial the audio sits on, zero when unknown.
    pub sideband: u8,
    pub flags: u8,
}

impl Marker {
    pub fn dial(&self) -> Option<i64> {
        if self.flags & MARK_DIAL_VALID != 0 {
            Some(self.dial_hz)
        } else {
            None
        }
    }

    fn encode(&self, out: &mut [u8; MARKER_BYTES]) {
        out.fill(0);
        out[0..8].copy_from_slice(&self.dial_hz.to_le_bytes());
        out[8..12].copy_from_slice(&self.tune_hz.to_le_bytes());
        out[12..16].copy_from_slice(&self.peak_db.to_le_bytes());
        out[16..20].copy_from_slice(&self.rms_db.to_le_bytes());
        out[20] = self.mode;
        out[21] = self.sideband;
        out[22] = self.flags;
    }

    fn decode(raw: &[u8; MARKER_BYTES]) -> Marker {
        Marker {
            dial_hz: i64::from_le_bytes(raw[0..8].try_into().unwrap()),
            tune_hz: f32::from_le_bytes(raw[8..12].try_into().unwrap()),
            peak_db: f32::from_le_bytes(raw[12..16].try_into().unwrap()),
            rms_db: f32::from_le_bytes(raw[16..20].try_into().unwrap()),
            mode: raw[20],
            sideband: raw[21],
            flags: raw[22],
        }
    }
}

/// Everything the header states.
#[derive(Debug, Clone, Copy)]
pub struct Info {
    pub channels: usize,
    pub rate: u32,
    pub format: SampleFormat,
    pub complex: bool,
    /// Frames per block, per channel.
    pub block_len: usize,
    /// Wall clock of the first sample, seconds since the epoch.
    pub start_unix: i64,
    /// Monotonic counter at the same instant.
    ///
    /// Held beside the wall clock because the two answer different questions. A
    /// wall clock names the moment for an operator; a monotonic counter tells
    /// whether two segments are contiguous, which a wall clock cannot after a
    /// clock adjustment.
    pub start_ticks: i64,
}

impl Info {
    /// Bytes one block occupies, marker included.
    pub fn block_bytes(&self) -> usize {
        MARKER_BYTES + self.block_len * self.channels * self.format.bytes()
    }

    pub fn block_seconds(&self) -> f64 {
        self.block_len as f64 / self.rate.max(1) as f64
    }
}

// ------------------------------------------------------------------ writer

pub struct Writer {
    file: File,
    info: Info,
    blocks: u32,
    /// Encoding scratch, sized once so a write never allocates.
    scratch: Vec<u8>,
}

impl Writer {
    /// Creates a segment and writes its header.
    ///
    /// The header is written up front with a block count of nought. A reader
    /// derives the count from the file length, so a segment left behind by a
    /// process that died is still readable up to its last whole block.
    pub fn create(path: &Path, info: Info) -> Result<Writer> {
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }
        if info.channels == 0 || info.channels > 2 {
            return Err(Error::io("a segment holds one or two channels"));
        }
        if info.block_len == 0 {
            return Err(Error::io("a segment block holds at least one frame"));
        }

        let mut file = File::create(path)?;
        let mut header = [0u8; HEADER_BYTES];
        header[0..4].copy_from_slice(&MAGIC);
        header[4..6].copy_from_slice(&VERSION.to_le_bytes());
        header[6..8].copy_from_slice(&(info.channels as u16).to_le_bytes());
        header[8..12].copy_from_slice(&info.rate.to_le_bytes());
        header[12] = info.format as u8;
        header[13] = if info.complex { FLAG_COMPLEX } else { 0 };
        header[16..20].copy_from_slice(&(info.block_len as u32).to_le_bytes());
        header[20..24].copy_from_slice(&0u32.to_le_bytes());
        header[24..32].copy_from_slice(&info.start_unix.to_le_bytes());
        header[32..40].copy_from_slice(&info.start_ticks.to_le_bytes());
        file.write_all(&header)?;

        let scratch = vec![0u8; info.block_bytes()];
        Ok(Writer { file, info, blocks: 0, scratch })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    pub fn blocks(&self) -> u32 {
        self.blocks
    }

    pub fn seconds(&self) -> f64 {
        self.blocks as f64 * self.info.block_seconds()
    }

    /// Bytes the file occupies so far.
    pub fn bytes(&self) -> u64 {
        HEADER_BYTES as u64 + self.blocks as u64 * self.info.block_bytes() as u64
    }

    /// Writes one whole block.
    ///
    /// A short slice is refused rather than padded. Padding would put silence in
    /// the middle of a recording and nothing downstream could tell it from a
    /// transmitter that stopped; the caller accumulates until it has a block.
    pub fn write_block(&mut self, marker: &Marker, frames: &[[f32; 2]]) -> Result<()> {
        if frames.len() != self.info.block_len {
            return Err(Error::io("a block write must carry exactly one block"));
        }

        let mut raw = [0u8; MARKER_BYTES];
        marker.encode(&mut raw);
        self.scratch[..MARKER_BYTES].copy_from_slice(&raw);

        let body = &mut self.scratch[MARKER_BYTES..];
        let channels = self.info.channels;
        match self.info.format {
            SampleFormat::I16 => {
                for (i, frame) in frames.iter().enumerate() {
                    for c in 0..channels {
                        // Clipped rather than wrapped. A wrap turns an overload
                        // into full scale noise of the opposite sign, which on a
                        // spectrum reads as a wideband burst that was never on
                        // the air.
                        let v = (frame[c].clamp(-1.0, 1.0) * 32767.0) as i16;
                        let at = (i * channels + c) * 2;
                        body[at..at + 2].copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            SampleFormat::F32 => {
                for (i, frame) in frames.iter().enumerate() {
                    for c in 0..channels {
                        let at = (i * channels + c) * 4;
                        body[at..at + 4].copy_from_slice(&frame[c].to_le_bytes());
                    }
                }
            }
        }

        self.file.write_all(&self.scratch)?;
        self.blocks += 1;
        Ok(())
    }

    /// Patches the block count and flushes.
    ///
    /// The count is a cross check rather than the truth, so a failure here
    /// leaves a usable file and is reported instead of being retried.
    pub fn finish(mut self) -> Result<u32> {
        let count = self.blocks;
        self.file.seek(SeekFrom::Start(20))?;
        self.file.write_all(&count.to_le_bytes())?;
        self.file.flush()?;
        Ok(count)
    }
}

// ------------------------------------------------------------------ reader

pub struct Reader {
    file: File,
    info: Info,
    /// Blocks the file length actually contains.
    blocks: usize,
    scratch: Vec<u8>,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Reader> {
        let mut file = File::open(path)?;
        let mut header = [0u8; HEADER_BYTES];
        file.read_exact(&mut header)
            .map_err(|_| Error::io(format!("{}: header is truncated", path.display())))?;

        if header[0..4] != MAGIC {
            return Err(Error::io(format!("{}: not a segment", path.display())));
        }
        let version = u16::from_le_bytes(header[4..6].try_into().unwrap());
        if version != VERSION {
            return Err(Error::io(format!(
                "{}: segment version {} is not readable by this build",
                path.display(),
                version
            )));
        }

        let channels = u16::from_le_bytes(header[6..8].try_into().unwrap()) as usize;
        let rate = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let format = SampleFormat::from_code(header[12])
            .ok_or_else(|| Error::io(format!("{}: unknown sample format", path.display())))?;
        let complex = header[13] & FLAG_COMPLEX != 0;
        let block_len = u32::from_le_bytes(header[16..20].try_into().unwrap()) as usize;
        let start_unix = i64::from_le_bytes(header[24..32].try_into().unwrap());
        let start_ticks = i64::from_le_bytes(header[32..40].try_into().unwrap());

        if channels == 0 || channels > 2 || rate == 0 || block_len == 0 {
            return Err(Error::io(format!("{}: header is inconsistent", path.display())));
        }

        let info = Info {
            channels,
            rate,
            format,
            complex,
            block_len,
            start_unix,
            start_ticks,
        };

        // Derived rather than read. A segment left by a process that died holds
        // a count of nought, and trusting it would report an empty recording of
        // a file that plainly has content.
        let len = file.metadata()?.len();
        let body = len.saturating_sub(HEADER_BYTES as u64);
        let blocks = (body / info.block_bytes() as u64) as usize;

        let scratch = vec![0u8; info.block_bytes()];
        Ok(Reader { file, info, blocks, scratch })
    }

    pub fn info(&self) -> Info {
        self.info
    }

    pub fn blocks(&self) -> usize {
        self.blocks
    }

    pub fn seconds(&self) -> f64 {
        self.blocks as f64 * self.info.block_seconds()
    }

    /// Reads one block into the destination, which is cleared first.
    ///
    /// The seek is a multiplication because every block is the same size, which
    /// is the whole reason the format has no variable length anything.
    pub fn read_block(
        &mut self,
        index: usize,
        marker: &mut Marker,
        out: &mut Vec<[f32; 2]>,
    ) -> Result<usize> {
        out.clear();
        if index >= self.blocks {
            return Ok(0);
        }

        let offset = HEADER_BYTES as u64 + index as u64 * self.info.block_bytes() as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut self.scratch)?;

        let raw: &[u8; MARKER_BYTES] = self.scratch[..MARKER_BYTES].try_into().unwrap();
        *marker = Marker::decode(raw);

        let body = &self.scratch[MARKER_BYTES..];
        let channels = self.info.channels;
        out.reserve(self.info.block_len);

        match self.info.format {
            SampleFormat::I16 => {
                for i in 0..self.info.block_len {
                    let mut frame = [0.0f32; 2];
                    for c in 0..channels {
                        let at = (i * channels + c) * 2;
                        let v = i16::from_le_bytes(body[at..at + 2].try_into().unwrap());
                        frame[c] = v as f32 / 32768.0;
                    }
                    // A single channel segment duplicates rather than leaving
                    // the second slot silent: everything downstream reduces the
                    // pair, and a silent slot would halve the level under the
                    // mix setting.
                    if channels == 1 {
                        frame[1] = frame[0];
                    }
                    out.push(frame);
                }
            }
            SampleFormat::F32 => {
                for i in 0..self.info.block_len {
                    let mut frame = [0.0f32; 2];
                    for c in 0..channels {
                        let at = (i * channels + c) * 4;
                        frame[c] = f32::from_le_bytes(body[at..at + 4].try_into().unwrap());
                    }
                    if channels == 1 {
                        frame[1] = frame[0];
                    }
                    out.push(frame);
                }
            }
        }

        Ok(out.len())
    }

    /// Reads only the marker of a block.
    ///
    /// Used to build a timeline: the levels and the dial of a whole segment are
    /// a few kilobytes of markers rather than several megabytes of audio, so a
    /// scrub bar can be drawn without touching the samples at all.
    pub fn read_marker(&mut self, index: usize) -> Result<Marker> {
        if index >= self.blocks {
            return Ok(Marker::default());
        }
        let offset = HEADER_BYTES as u64 + index as u64 * self.info.block_bytes() as u64;
        self.file.seek(SeekFrom::Start(offset))?;
        let mut raw = [0u8; MARKER_BYTES];
        self.file.read_exact(&mut raw)?;
        Ok(Marker::decode(&raw))
    }
}