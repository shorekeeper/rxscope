//! WAV writer.
//!
//! Uncompressed, because the format exists for the one case where the recording
//! has to open in something else. Any compression at all narrows that, and the
//! two other export formats are there precisely to trade size against reach.
//!
//! Sixteen bit integer or thirty two bit float. Twenty four bit is offered by
//! neither: it is a three byte container that half the tools in the field read
//! as silence, and the size it saves over float is the size lossless saves
//! twice over.
//!
//! The header is written with a length of nought and patched on close, which is
//! what lets the writer stream rather than buffer the whole recording.

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::core::{Error, Result};

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;

pub struct Writer {
    file: File,
    channels: u16,
    /// Sixteen or thirty two. Thirty two means float.
    bits: u16,
    frames: u64,
}

impl Writer {
    pub fn create(path: &Path, rate: u32, channels: u16, bits: u32) -> Result<Writer> {
        if let Some(directory) = path.parent() {
            if !directory.as_os_str().is_empty() {
                std::fs::create_dir_all(directory)?;
            }
        }
        let bits: u16 = match bits {
            16 => 16,
            32 => 32,
            other => {
                return Err(Error::io(format!(
                    "{} bits per sample is not offered, use 16 or 32",
                    other
                )))
            }
        };
        let channels = channels.clamp(1, 2);

        let mut file = File::create(path)?;
        let tag = if bits == 32 { FORMAT_FLOAT } else { FORMAT_PCM };
        let block_align = channels * bits / 8;
        let byte_rate = rate * block_align as u32;

        // A sixteen byte format chunk with the float tag is what every tool in
        // the field reads. The extensible form is more correct on paper and is
        // refused by enough of them to be the worse choice.
        let mut header = Vec::with_capacity(44);
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(b"WAVE");
        header.extend_from_slice(b"fmt ");
        header.extend_from_slice(&16u32.to_le_bytes());
        header.extend_from_slice(&tag.to_le_bytes());
        header.extend_from_slice(&channels.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&byte_rate.to_le_bytes());
        header.extend_from_slice(&block_align.to_le_bytes());
        header.extend_from_slice(&bits.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&0u32.to_le_bytes());
        file.write_all(&header)?;

        Ok(Writer { file, channels, bits, frames: 0 })
    }

    /// Appends frames. The pair is truncated to the channel count, so a mono
    /// export takes the first channel rather than the reduction: which of the
    /// two is wanted is a decision for the caller and not for a file writer.
    pub fn push(&mut self, frames: &[[f32; 2]]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        let channels = self.channels as usize;
        let mut out = Vec::with_capacity(frames.len() * channels * (self.bits as usize / 8));

        if self.bits == 16 {
            for frame in frames {
                for c in 0..channels {
                    let v = (frame[c].clamp(-1.0, 1.0) * 32767.0) as i16;
                    out.extend_from_slice(&v.to_le_bytes());
                }
            }
        } else {
            for frame in frames {
                for c in 0..channels {
                    out.extend_from_slice(&frame[c].to_le_bytes());
                }
            }
        }

        self.file.write_all(&out)?;
        self.frames += frames.len() as u64;
        Ok(())
    }

    /// Patches the two lengths.
    ///
    /// A recording past four gigabytes cannot be described by this container at
    /// all, so the length is saturated and the caller is told: a file with a
    /// wrapped length reads as a few seconds of audio, which is worse than a
    /// refusal.
    pub fn finish(mut self) -> Result<u64> {
        let bytes_per_frame = self.channels as u64 * (self.bits as u64 / 8);
        let data = self.frames * bytes_per_frame;
        if data + 36 > u32::MAX as u64 {
            return Err(Error::io(
                "the recording exceeds what a WAV container can describe, export it in parts",
            ));
        }

        self.file.seek(SeekFrom::Start(4))?;
        self.file.write_all(&((data + 36) as u32).to_le_bytes())?;
        self.file.seek(SeekFrom::Start(40))?;
        self.file.write_all(&(data as u32).to_le_bytes())?;
        self.file.flush()?;
        Ok(self.frames)
    }
}