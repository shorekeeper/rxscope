//! QOA encoder.
//!
//! Quite OK Audio: a lossy codec whose whole specification is a page long. It
//! exists in this application for one purpose, which is sending a recording to
//! somebody: it reaches a quarter of the uncompressed size with no dependency
//! and no options, and its decoder is thirty lines in any language.
//!
//! It is deliberately not the archival format. The method is a least mean
//! squares predictor followed by a three bit residual, and three bits of
//! residual is where the error goes: on receiver audio the loss lands on the
//! quietest part of the signal, which is exactly the part a second decoding
//! pass would be trying to recover.
//!
//! ## Method
//!
//! Per channel a four tap predictor whose weights adapt to the signal. The
//! residual is divided by one of sixteen scale factors, clamped to eight steps
//! and stored in three bits. Twenty samples share one scale factor and pack
//! into eight bytes; two hundred and fifty six such slices make a frame, and a
//! frame restates the predictor state so a decoder can start anywhere.
//!
//! The encoder tries all sixteen scale factors per slice and keeps the one with
//! the least squared error, which is what makes the result track a signal whose
//! level changes. The search abandons a candidate as soon as it is worse than
//! the best so far, so the cost is far below sixteen passes.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::core::{Error, Result};

const SLICE_LEN: usize = 20;
const SLICES_PER_FRAME: usize = 256;
const FRAME_LEN: usize = SLICE_LEN * SLICES_PER_FRAME;
const LMS_LEN: usize = 4;

/// Scale factors. The step between two of them is roughly a third of an octave,
/// which is fine enough that the search almost always finds one within a
/// fraction of a decibel of ideal.
const SCALEFACTOR: [i32; 16] = [
    1, 7, 21, 45, 84, 138, 211, 304, 421, 562, 731, 928, 1157, 1419, 1715, 2048,
];

/// Reciprocals of the scale factors in sixteen bit fixed point, so the division
/// in the inner loop is a multiply and a shift.
const RECIPROCAL: [i32; 16] = [
    65536, 9363, 3121, 1457, 781, 475, 311, 216, 156, 117, 90, 71, 57, 47, 39, 32,
];

/// Value each three bit code stands for, per scale factor.
const DEQUANT: [[i32; 8]; 16] = [
    [1, -1, 3, -3, 5, -5, 7, -7],
    [5, -5, 18, -18, 32, -32, 49, -49],
    [16, -16, 53, -53, 95, -95, 147, -147],
    [34, -34, 113, -113, 203, -203, 315, -315],
    [63, -63, 210, -210, 378, -378, 588, -588],
    [104, -104, 345, -345, 621, -621, 966, -966],
    [158, -158, 528, -528, 950, -950, 1477, -1477],
    [228, -228, 760, -760, 1368, -1368, 2128, -2128],
    [316, -316, 1053, -1053, 1895, -1895, 2947, -2947],
    [422, -422, 1405, -1405, 2529, -2529, 3934, -3934],
    [548, -548, 1828, -1828, 3290, -3290, 5117, -5117],
    [696, -696, 2320, -2320, 4176, -4176, 6496, -6496],
    [868, -868, 2893, -2893, 5207, -5207, 8099, -8099],
    [1064, -1064, 3548, -3548, 6386, -6386, 9933, -9933],
    [1286, -1286, 4288, -4288, 7718, -7718, 12005, -12005],
    [1536, -1536, 5120, -5120, 9216, -9216, 14336, -14336],
];

/// Code for a scaled residual in the range minus eight to plus eight.
const QUANT: [u8; 17] = [7, 7, 7, 5, 5, 3, 3, 1, 0, 0, 2, 2, 4, 4, 6, 6, 6];

/// Four tap predictor.
#[derive(Clone, Copy)]
struct Lms {
    history: [i32; LMS_LEN],
    weights: [i32; LMS_LEN],
}

impl Lms {
    /// Initial weights are not nought.
    ///
    /// A predictor starting from nought predicts silence, so the first frame of
    /// every recording would be coded at full residual. The pair below is the
    /// two tap difference the format specifies, which predicts a smooth signal
    /// immediately and lets the adaptation take over from there.
    fn new() -> Lms {
        Lms {
            history: [0; LMS_LEN],
            weights: [0, 0, -(1 << 13), 1 << 14],
        }
    }

    #[inline]
    fn predict(&self) -> i32 {
        let mut acc = 0i64;
        for i in 0..LMS_LEN {
            acc += self.history[i] as i64 * self.weights[i] as i64;
        }
        (acc >> 13) as i32
    }

    #[inline]
    fn update(&mut self, sample: i32, residual: i32) {
        let delta = residual >> 4;
        for i in 0..LMS_LEN {
            self.weights[i] += if self.history[i] < 0 { -delta } else { delta };
        }
        for i in 0..LMS_LEN - 1 {
            self.history[i] = self.history[i + 1];
        }
        self.history[LMS_LEN - 1] = sample;
    }
}

/// Divides a residual by a scale factor, rounding away from nought.
///
/// Rounding away rather than to nearest is what the format specifies, and it
/// matters: rounding towards nought biases the reconstruction low and the
/// predictor then chases the bias.
#[inline]
fn divide(v: i32, scalefactor: usize) -> i32 {
    let reciprocal = RECIPROCAL[scalefactor];
    let n = ((v as i64 * reciprocal as i64 + (1 << 15)) >> 16) as i32;
    n + (v > 0) as i32 - (v < 0) as i32 - (n > 0) as i32 + (n < 0) as i32
}

#[inline]
fn clamp_s16(v: i32) -> i32 {
    v.clamp(-32768, 32767)
}

pub struct Writer {
    file: BufWriter<File>,
    rate: u32,
    channels: usize,
    lms: [Lms; 2],
    /// Scale factor the previous slice chose, per channel.
    ///
    /// The search starts from it because the level of a signal changes slowly,
    /// so the previous choice is usually within a step or two of the next and
    /// the early abandon then triggers on the first few candidates.
    previous: [usize; 2],
    /// Frames waiting for a whole coding frame.
    pending: Vec<[f32; 2]>,
    /// Integer lanes, reused per frame.
    lanes: [Vec<i32>; 2],
    frames: u64,
    /// Whether the header has been written. The header carries the total sample
    /// count, which is not known until the end, so it is patched on close.
    started: bool,
}

impl Writer {
    pub fn create(path: &Path, rate: u32, channels: usize) -> Result<Writer> {
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }
        let channels = channels.clamp(1, 2);
        if rate == 0 || rate > 0x00FF_FFFF {
            return Err(Error::io("the rate does not fit the container"));
        }

        let mut file = File::create(path)?;
        // Magic and the sample count, the latter patched on close.
        file.write_all(b"qoaf")?;
        file.write_all(&0u32.to_be_bytes())?;

        Ok(Writer {
            file: BufWriter::with_capacity(64 * 1024, file),
            rate,
            channels,
            lms: [Lms::new(), Lms::new()],
            previous: [0, 0],
            pending: Vec::with_capacity(FRAME_LEN),
            lanes: [Vec::with_capacity(FRAME_LEN), Vec::with_capacity(FRAME_LEN)],
            frames: 0,
            started: true,
        })
    }

    pub fn push(&mut self, frames: &[[f32; 2]]) -> Result<()> {
        for &frame in frames {
            self.pending.push(frame);
            if self.pending.len() >= FRAME_LEN {
                self.flush_frame()?;
            }
        }
        Ok(())
    }

    fn flush_frame(&mut self) -> Result<()> {
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

        let slices = (n + SLICE_LEN - 1) / SLICE_LEN;
        let size = 8 + self.channels * (16 + slices * 8);

        // Frame header, one big endian word: channels, rate, samples, size.
        let head: u64 = ((self.channels as u64) << 56)
            | ((self.rate as u64 & 0x00FF_FFFF) << 32)
            | ((n as u64 & 0xFFFF) << 16)
            | (size as u64 & 0xFFFF);
        self.file.write_all(&head.to_be_bytes())?;

        // Predictor state, so a decoder may start at any frame.
        for c in 0..self.channels {
            for i in 0..LMS_LEN {
                self.file.write_all(&(self.lms[c].history[i] as i16).to_be_bytes())?;
            }
            for i in 0..LMS_LEN {
                self.file.write_all(&(self.lms[c].weights[i] as i16).to_be_bytes())?;
            }
        }

        for slice in 0..slices {
            for c in 0..self.channels {
                let start = slice * SLICE_LEN;
                let end = (start + SLICE_LEN).min(n);
                let samples = &self.lanes[c][start..end];

                let mut best_error = u64::MAX;
                let mut best_slice = 0u64;
                let mut best_lms = self.lms[c];
                let mut best_sf = self.previous[c];

                for offset in 0..16usize {
                    // Starting from the previous choice makes the early abandon
                    // fire on the first candidates rather than the last.
                    let sf = (offset + self.previous[c]) % 16;
                    let mut lms = self.lms[c];
                    let mut packed: u64 = sf as u64;
                    let mut error = 0u64;

                    for &sample in samples {
                        let predicted = lms.predict();
                        let residual = sample - predicted;
                        let scaled = divide(residual, sf);
                        let clamped = scaled.clamp(-8, 8);
                        let quantized = QUANT[(clamped + 8) as usize];
                        let dequantized = DEQUANT[sf][quantized as usize];
                        let reconstructed = clamp_s16(predicted + dequantized);

                        let e = (sample - reconstructed) as i64;
                        error += (e * e) as u64;
                        if error > best_error {
                            break;
                        }
                        lms.update(reconstructed, dequantized);
                        packed = (packed << 3) | quantized as u64;
                    }

                    if error < best_error {
                        best_error = error;
                        best_slice = packed;
                        best_lms = lms;
                        best_sf = sf;
                    }
                }

                // A short final slice is left aligned, so a decoder reading a
                // full slice finds the samples where it expects them and the
                // trailing codes decode to whatever; the sample count in the
                // header is what bounds the output.
                let short = SLICE_LEN - samples.len();
                best_slice <<= short * 3;

                self.file.write_all(&best_slice.to_be_bytes())?;
                self.lms[c] = best_lms;
                self.previous[c] = best_sf;
            }
        }

        self.frames += n as u64;
        self.pending.clear();
        Ok(())
    }

    /// Patches the sample count and flushes.
    ///
    /// The count is per channel, which is what the container states, and it is
    /// what a decoder uses to stop rather than the file length: the last frame
    /// is padded to a whole slice.
    pub fn finish(mut self) -> Result<u64> {
        self.flush_frame()?;
        let frames = self.frames;
        if frames > u32::MAX as u64 {
            return Err(Error::io(
                "the recording exceeds what the container can describe, export it in parts",
            ));
        }
        let _ = self.started;

        let mut file = self
            .file
            .into_inner()
            .map_err(|e| Error::io(format!("cannot flush the export: {}", e)))?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(4))?;
        file.write_all(&(frames as u32).to_be_bytes())?;
        file.flush()?;
        Ok(frames)
    }
}