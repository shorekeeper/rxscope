//! Lossless codec.
//!
//! ## Why a codec of its own
//!
//! Archiving a reception means keeping every sample: the reason to return to a
//! recording is almost always to decode something that was too weak the first
//! time, and a lossy codec spends its error budget on exactly those samples.
//! The alternative is an established lossless container, and the two candidates
//! both cost more than they are worth here: one needs a metadata model and a
//! subframe search this application has no use for, the other is a decade of
//! accumulated framing rules. What is left after removing both is a fixed
//! predictor and an entropy coder, which is the part that does the compressing.
//!
//! ## Method
//!
//! Per block of frames and per channel: choose one of five fixed polynomial
//! predictors, subtract it, and Rice code the residual.
//!
//! The predictors are the successive differences of the sample sequence. Order
//! nought is the sample itself, order one the first difference, and so on. On
//! noise the low orders win because differencing amplifies noise; on a steady
//! tone the high orders win because a smooth sequence differences towards
//! nought. Trying all five and taking the smallest costs five passes over the
//! block and removes the need to model the signal at all.
//!
//! Rice coding is a unary quotient and a fixed length remainder. It is optimal
//! for a two sided geometric distribution, which is what a prediction residual
//! is, and it needs no table: one parameter per block per channel.
//!
//! The result on receiver audio is roughly half the size of the integer
//! original. That is what lossless compression of noise is worth; a claim of
//! more would mean the input was not noise.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

use crate::core::{Error, Result};

pub const MAGIC: [u8; 4] = *b"RXL1";
pub const VERSION: u16 = 1;

/// Frames in one block.
///
/// Long enough that one predictor and one Rice parameter describe the block
/// well, short enough that a change in the signal is followed within a fraction
/// of a second. Four thousand at the decoder rate is a third of a second.
const BLOCK: usize = 4096;

/// Highest predictor order tried.
const MAX_ORDER: usize = 4;

/// Quotient beyond which a residual is escaped.
///
/// Without a bound one pathological sample would emit tens of thousands of bits
/// of unary. The escape costs forty bits on a residual that would otherwise
/// cost more, and it bounds the worst case block to a fixed size.
const ESCAPE: u32 = 40;

// ------------------------------------------------------------------ bits

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    bits: u32,
}

impl BitWriter {
    fn new() -> BitWriter {
        BitWriter { out: Vec::with_capacity(BLOCK * 2), acc: 0, bits: 0 }
    }

    /// Writes the low count bits of value, most significant first.
    #[inline]
    fn write(&mut self, value: u32, count: u32) {
        if count == 0 {
            return;
        }
        let masked = if count >= 32 { value as u64 } else { (value as u64) & ((1u64 << count) - 1) };
        self.acc = (self.acc << count) | masked;
        self.bits += count;
        while self.bits >= 8 {
            self.bits -= 8;
            self.out.push((self.acc >> self.bits) as u8);
        }
    }

    /// Writes a run of zeros followed by a one, which is the unary part of a
    /// Rice code. Written in chunks so a long run does not overflow the
    /// accumulator.
    #[inline]
    fn unary(&mut self, zeros: u32) {
        let mut left = zeros;
        while left >= 24 {
            self.write(0, 24);
            left -= 24;
        }
        self.write(1, left + 1);
    }

    /// Pads to a byte boundary and returns the buffer.
    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            let pad = 8 - self.bits;
            self.write(0, pad);
        }
        self.out
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
    acc: u64,
    bits: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, at: 0, acc: 0, bits: 0 }
    }

    #[inline]
    fn fill(&mut self) {
        while self.bits <= 56 && self.at < self.data.len() {
            self.acc = (self.acc << 8) | self.data[self.at] as u64;
            self.at += 1;
            self.bits += 8;
        }
    }

    #[inline]
    fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        self.fill();
        if self.bits < count {
            // Past the end. Zeros are returned rather than an error because the
            // caller already knows how many samples the block holds, so a
            // truncated block is caught by the frame count rather than here.
            let have = self.bits;
            let value = (self.acc << (count - have)) as u32;
            self.bits = 0;
            self.acc = 0;
            return value & if count >= 32 { u32::MAX } else { (1u32 << count) - 1 };
        }
        self.bits -= count;
        let value = (self.acc >> self.bits) as u32;
        self.acc &= if self.bits == 0 { 0 } else { (1u64 << self.bits) - 1 };
        value & if count >= 32 { u32::MAX } else { (1u32 << count) - 1 }
    }

    /// Counts zeros up to and including the terminating one.
    #[inline]
    fn unary(&mut self) -> u32 {
        let mut zeros = 0u32;
        loop {
            if self.read(1) == 1 {
                return zeros;
            }
            zeros += 1;
            if zeros > ESCAPE + 1 {
                // A run longer than the escape cannot occur in a well formed
                // stream, so this is a corrupt file rather than a long value.
                return zeros;
            }
        }
    }
}

/// Maps a signed residual to an unsigned one without losing the sign.
///
/// Interleaving rather than an offset, so a small negative residual stays small.
#[inline]
fn zigzag(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

#[inline]
fn unzigzag(u: u32) -> i32 {
    ((u >> 1) as i32) ^ -((u & 1) as i32)
}

/// Applies one of the fixed predictors in place, leaving the residual.
///
/// The first `order` samples cannot be predicted and are left as they are,
/// which is why the decoder reconstructs from the same index.
fn difference(order: usize, samples: &mut [i32]) {
    for _ in 0..order {
        for i in (1..samples.len()).rev() {
            samples[i] = samples[i].wrapping_sub(samples[i - 1]);
        }
    }
}

fn integrate(order: usize, samples: &mut [i32]) {
    for _ in 0..order {
        for i in 1..samples.len() {
            samples[i] = samples[i].wrapping_add(samples[i - 1]);
        }
    }
}

/// Bits a block of residuals costs at a given parameter.
fn cost(residuals: &[i32], k: u32) -> u64 {
    let mut total = 0u64;
    for &r in residuals {
        let u = zigzag(r);
        let q = u >> k;
        total += if q >= ESCAPE {
            (ESCAPE + 1 + 32) as u64
        } else {
            (q + 1 + k) as u64
        };
    }
    total
}

/// Parameter that costs the fewest bits.
///
/// Searched exhaustively over the useful range rather than estimated from the
/// mean. Twenty five passes over four thousand values is a hundred thousand
/// operations per block, which on an export is nothing, and it removes the one
/// place an estimate could be wrong on an unusual block.
fn best_k(residuals: &[i32]) -> (u32, u64) {
    let mut best = (0u32, u64::MAX);
    for k in 0..=24u32 {
        let bits = cost(residuals, k);
        if bits < best.1 {
            best = (k, bits);
        }
    }
    best
}

// ---------------------------------------------------------------- writer

pub struct Writer {
    file: BufWriter<File>,
    channels: usize,
    frames: u64,
    /// Pending frames, flushed once a block is full.
    pending: Vec<[f32; 2]>,
    /// One integer lane per channel, reused per block.
    lanes: [Vec<i32>; 2],
    scratch: Vec<i32>,
}

impl Writer {
    pub fn create(path: &Path, rate: u32, channels: usize) -> Result<Writer> {
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }
        let channels = channels.clamp(1, 2);

        let mut file = File::create(path)?;
        let mut header = Vec::with_capacity(24);
        header.extend_from_slice(&MAGIC);
        header.extend_from_slice(&VERSION.to_le_bytes());
        header.extend_from_slice(&(channels as u16).to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&(BLOCK as u32).to_le_bytes());
        // Frame count, patched on close. A reader that finds nought decodes
        // until the blocks run out, so a truncated file still plays.
        header.extend_from_slice(&0u64.to_le_bytes());
        file.write_all(&header)?;

        Ok(Writer {
            file: BufWriter::with_capacity(64 * 1024, file),
            channels,
            frames: 0,
            pending: Vec::with_capacity(BLOCK),
            lanes: [Vec::with_capacity(BLOCK), Vec::with_capacity(BLOCK)],
            scratch: Vec::with_capacity(BLOCK),
        })
    }

    pub fn push(&mut self, frames: &[[f32; 2]]) -> Result<()> {
        for &frame in frames {
            self.pending.push(frame);
            if self.pending.len() >= BLOCK {
                self.flush_block()?;
            }
        }
        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        let n = self.pending.len();
        if n == 0 {
            return Ok(());
        }

        for c in 0..self.channels {
            self.lanes[c].clear();
            for frame in &self.pending {
                self.lanes[c].push((frame[c].clamp(-1.0, 1.0) * 32767.0) as i32);
            }
        }

        // Block header: the frame count, then one predictor and one parameter
        // per channel. The count is per block rather than implied so the last
        // block does not need padding.
        let mut head = Vec::with_capacity(8);
        head.extend_from_slice(&(n as u32).to_le_bytes());

        let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(self.channels);
        for c in 0..self.channels {
            let mut chosen_order = 0usize;
            let mut chosen_k = 0u32;
            let mut chosen_bits = u64::MAX;
            let mut chosen: Vec<i32> = Vec::new();

            for order in 0..=MAX_ORDER.min(n.saturating_sub(1)) {
                self.scratch.clear();
                self.scratch.extend_from_slice(&self.lanes[c]);
                difference(order, &mut self.scratch);
                // The first samples are the seed the decoder integrates from,
                // and they are stored raw at the head of the residual run, so
                // they take part in the parameter search like everything else.
                let (k, bits) = best_k(&self.scratch);
                if bits < chosen_bits {
                    chosen_bits = bits;
                    chosen_order = order;
                    chosen_k = k;
                    chosen.clear();
                    chosen.extend_from_slice(&self.scratch);
                }
            }

            head.push(chosen_order as u8);
            head.push(chosen_k as u8);

            let mut bits = BitWriter::new();
            for &r in &chosen {
                let u = zigzag(r);
                let q = u >> chosen_k;
                if q >= ESCAPE {
                    bits.unary(ESCAPE);
                    bits.write(u, 32);
                } else {
                    bits.unary(q);
                    bits.write(u, chosen_k);
                }
            }
            bodies.push(bits.finish());
        }

        // Lengths follow the parameters, so a decoder can find the second
        // channel without decoding the first.
        for body in &bodies {
            head.extend_from_slice(&(body.len() as u32).to_le_bytes());
        }
        self.file.write_all(&head)?;
        for body in &bodies {
            self.file.write_all(body)?;
        }

        self.frames += n as u64;
        self.pending.clear();
        Ok(())
    }

    pub fn finish(mut self) -> Result<u64> {
        self.flush_block()?;
        let frames = self.frames;
        let mut file = self
            .file
            .into_inner()
            .map_err(|e| Error::io(format!("cannot flush the export: {}", e)))?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(16))?;
        file.write_all(&frames.to_le_bytes())?;
        file.flush()?;
        Ok(frames)
    }
}

// ---------------------------------------------------------------- reader

/// Decodes a whole file into memory.
///
/// The whole file rather than a stream, because the one consumer is a
/// verification pass: an export that cannot be read back is a corrupt archive,
/// and the only way to know is to read it back. A recording is replayed from
/// the segment format, which is seekable and needs no decoding at all.
pub fn decode(path: &Path) -> Result<(u32, usize, Vec<[f32; 2]>)> {
    let mut raw = Vec::new();
    File::open(path)?.read_to_end(&mut raw)?;
    if raw.len() < 24 || raw[0..4] != MAGIC {
        return Err(Error::io(format!("{}: not a lossless export", path.display())));
    }
    let version = u16::from_le_bytes(raw[4..6].try_into().unwrap());
    if version != VERSION {
        return Err(Error::io(format!("{}: version {} is not readable", path.display(), version)));
    }
    let channels = u16::from_le_bytes(raw[6..8].try_into().unwrap()) as usize;
    let rate = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    if channels == 0 || channels > 2 {
        return Err(Error::io(format!("{}: bad channel count", path.display())));
    }

    let mut out: Vec<[f32; 2]> = Vec::new();
    let mut at = 24usize;
    let mut lanes: [Vec<i32>; 2] = [Vec::new(), Vec::new()];

    while at + 4 <= raw.len() {
        let n = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
        at += 4;
        if n == 0 || n > BLOCK {
            break;
        }

        let params_bytes = channels * 2;
        let lengths_bytes = channels * 4;
        if at + params_bytes + lengths_bytes > raw.len() {
            break;
        }

        let mut orders = [0usize; 2];
        let mut ks = [0u32; 2];
        for c in 0..channels {
            orders[c] = raw[at] as usize;
            ks[c] = raw[at + 1] as u32;
            at += 2;
        }
        let mut lengths = [0usize; 2];
        for c in 0..channels {
            lengths[c] = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
            at += 4;
        }

        let mut ok = true;
        for c in 0..channels {
            if at + lengths[c] > raw.len() {
                ok = false;
                break;
            }
            let body = &raw[at..at + lengths[c]];
            at += lengths[c];

            let mut bits = BitReader::new(body);
            let lane = &mut lanes[c];
            lane.clear();
            lane.reserve(n);
            for _ in 0..n {
                let q = bits.unary();
                let u = if q >= ESCAPE {
                    bits.read(32)
                } else {
                    (q << ks[c]) | bits.read(ks[c])
                };
                lane.push(unzigzag(u));
            }
            integrate(orders[c], lane);
        }
        if !ok {
            break;
        }

        for i in 0..n {
            let mut frame = [0.0f32; 2];
            for c in 0..channels {
                frame[c] = (lanes[c][i] as f32 / 32768.0).clamp(-1.0, 1.0);
            }
            if channels == 1 {
                frame[1] = frame[0];
            }
            out.push(frame);
        }
    }

    Ok((rate, channels, out))
}