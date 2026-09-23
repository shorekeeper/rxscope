//! Sample format conversion and channel selection.
//!
//! Capture buffers arrive as interleaved frames in whatever format the device
//! negotiated. Everything downstream works on a single channel of f32 in the
//! range minus one to plus one, so this is the only place that has to know
//! about container sizes and integer scaling.

use crate::config::settings::ChannelMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// Unsigned eight bit, midpoint at 128.
    U8,
    I16,
    /// Three byte packed signed integer, little endian.
    I24,
    I32,
    F32,
}

impl SampleFormat {
    pub fn bytes(self) -> usize {
        match self {
            SampleFormat::U8 => 1,
            SampleFormat::I16 => 2,
            SampleFormat::I24 => 3,
            SampleFormat::I32 => 4,
            SampleFormat::F32 => 4,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SampleFormat::U8 => "u8",
            SampleFormat::I16 => "i16",
            SampleFormat::I24 => "i24",
            SampleFormat::I32 => "i32",
            SampleFormat::F32 => "f32",
        }
    }
}

pub struct Converter {
    pub format: SampleFormat,
    pub channels: usize,
    pub mode: ChannelMode,
    /// Distance between two frames in bytes. Taken from the device format
    /// rather than computed, because a driver may pad a frame.
    pub frame_bytes: usize,
}

impl Converter {
    pub fn new(format: SampleFormat, channels: usize, frame_bytes: usize, mode: ChannelMode) -> Converter {
        let channels = channels.max(1);
        // A frame must hold at least one sample per channel; a smaller stride
        // would mean the format description is inconsistent.
        let minimum = channels * format.bytes();
        Converter {
            format,
            channels,
            mode,
            frame_bytes: frame_bytes.max(minimum),
        }
    }

    /// Appends the two channels as a pair, unreduced.
    ///
    /// Both are carried because a receiver fed quadrature needs them apart, and
    /// which of the two arrangements is in force is decided far downstream. A
    /// queue that carried the reduction would settle that question in the
    /// capture thread, where the receiver settings are not known and where a
    /// change would cost a stream restart.
    ///
    /// A mono device duplicates its single channel. That is not a quadrature
    /// pair and cannot become one; the caller is told the channel count and
    /// says so.
    pub fn to_pairs(&self, bytes: &[u8], frames: usize, out: &mut Vec<[f32; 2]>) {
        let available = bytes.len() / self.frame_bytes;
        let frames = frames.min(available);
        out.reserve(frames);

        let right = if self.channels > 1 { 1 } else { 0 };
        for f in 0..frames {
            let base = f * self.frame_bytes;
            out.push([self.sample(bytes, base, 0), self.sample(bytes, base, right)]);
        }
    }

    /// Reduces a pair to one channel.
    ///
    /// Applied wherever a mono view is wanted rather than once in the capture
    /// thread, so the setting takes effect without a restart and so the
    /// receiver path can decline the reduction entirely.
    ///
    /// The difference mode subtracts the channels, which cancels whatever is
    /// common to both and suppresses hum picked up by the sound card wiring.
    #[inline]
    pub fn reduce(mode: ChannelMode, pair: [f32; 2]) -> f32 {
        match mode {
            ChannelMode::Left => pair[0],
            ChannelMode::Right => pair[1],
            ChannelMode::Mix => (pair[0] + pair[1]) * 0.5,
            ChannelMode::Difference => pair[0] - pair[1],
        }
    }

    /// Channels the device delivers. One means a quadrature pair is impossible.
    pub fn channels(&self) -> usize {
        self.channels
    }

    #[inline]
    fn sample(&self, bytes: &[u8], frame_base: usize, channel: usize) -> f32 {
        let offset = frame_base + channel * self.format.bytes();
        match self.format {
            SampleFormat::U8 => {
                let v = bytes[offset] as f32;
                (v - 128.0) / 128.0
            }
            SampleFormat::I16 => {
                let v = i16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                v as f32 / 32768.0
            }
            SampleFormat::I24 => {
                // Sign extension by shifting the value into the top of an i32
                // and back down again.
                let raw = (bytes[offset] as u32)
                    | ((bytes[offset + 1] as u32) << 8)
                    | ((bytes[offset + 2] as u32) << 16);
                let v = ((raw << 8) as i32) >> 8;
                v as f32 / 8_388_608.0
            }
            SampleFormat::I32 => {
                let v = i32::from_le_bytes([
                    bytes[offset],
                    bytes[offset + 1],
                    bytes[offset + 2],
                    bytes[offset + 3],
                ]);
                v as f32 / 2_147_483_648.0
            }
            SampleFormat::F32 => f32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]),
        }
    }
}