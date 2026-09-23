//! Decoded text buffer.
//!
//! Text arrives one character at a time and has no structure of its own, so the
//! log breaks it into lines on a line feed, on a width limit, and on a pause
//! long enough that the next character clearly belongs to a new transmission.
//! Each finished line keeps the wall clock time, the mode and the frequency that
//! produced it.
//!
//! Several carriers are decoded at once, so several lines are under construction
//! at once. A line is therefore keyed by the channel that feeds it: interleaving
//! two stations into one line would destroy both, and the frequency column is
//! what lets the operator tell them apart afterwards.
//!
//! The transcript file is written line by line and flushed immediately: an
//! operator who loses the application to a crash still wants the traffic.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use super::Mode;

/// Characters after which a line is broken even without a line feed. Chosen so
/// a line still fits a narrow panel at a large font.
const WRAP_WIDTH: usize = 96;

/// Seconds of silence that end the current line.
const IDLE_BREAK_S: f32 = 8.0;

/// Lines under construction at once. One more than the channel ceiling, so the
/// teleprinter path always has a slot of its own.
const MAX_PENDING: usize = 20;

#[derive(Debug, Clone)]
pub struct DecodeLine {
    /// Local time when the line started.
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub mode: Mode,
    /// Channel that produced the line, zero for the teleprinter path.
    pub channel: u32,
    pub hz: f32,
    pub text: String,
}

impl DecodeLine {
    /// Hours and minutes, with the marker that says which clock.
    ///
    /// The marker costs one character and removes the one ambiguity a stamp can
    /// carry. A reader who assumes the wrong clock is out by a whole number of
    /// hours, which is worse than no stamp because it looks correct.
    pub fn stamp(&self) -> String {
        format!("{:02}{:02}z", self.hour, self.minute)
    }

    pub fn stamp_full(&self) -> String {
        format!("{:02}:{:02}:{:02}Z", self.hour, self.minute, self.second)
    }

    /// Frequency column, fixed width so the text stays aligned.
    pub fn tag(&self) -> String {
        if self.hz > 0.0 {
            format!("{:>5.0}", self.hz)
        } else {
            "     ".to_string()
        }
    }
}

/// Line still being assembled.
#[derive(Debug, Clone)]
pub struct PendingLine {
    pub channel: u32,
    pub hz: f32,
    pub mode: Mode,
    pub text: String,
    stamp: (u16, u16, u16),
    idle: f32,
}

impl PendingLine {
    pub fn tag(&self) -> String {
        if self.hz > 0.0 {
            format!("{:>5.0}", self.hz)
        } else {
            "     ".to_string()
        }
    }
}

pub struct DecodeLog {
    lines: std::collections::VecDeque<DecodeLine>,
    limit: usize,
    pending: Vec<PendingLine>,
    transcript: Option<File>,
    total_chars: u64,
    /// Lines appended since the log was created.
    ///
    /// The buffer discards its oldest, so its length says nothing about how many
    /// lines have gone through it. A consumer that wants to see each finished
    /// line exactly once compares against this rather than against the length.
    appended: u64,
}

impl DecodeLog {
    pub fn new(limit: usize, transcript_path: Option<&Path>) -> DecodeLog {
        let transcript = transcript_path.and_then(|p| {
            if let Some(dir) = p.parent() {
                if !dir.as_os_str().is_empty() {
                    let _ = std::fs::create_dir_all(dir);
                }
            }
            match OpenOptions::new().create(true).append(true).open(p) {
                Ok(f) => {
                    crate::log_info!("decode", "transcript: {}", p.display());
                    Some(f)
                }
                Err(e) => {
                    crate::log_warn!("decode", "cannot open {}: {}", p.display(), e);
                    None
                }
            }
        });

        DecodeLog {
            lines: std::collections::VecDeque::with_capacity(limit.min(4096)),
            limit: limit.max(16),
            pending: Vec::with_capacity(MAX_PENDING),
            transcript,
            total_chars: 0,
            appended: 0,
        }
    }

    /// Finished lines, oldest first. The iterator is double ended so a view
    /// that lays out from the bottom can walk backwards without copying the
    /// buffer, which matters when it holds a few thousand lines.
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &DecodeLine> + '_ {
        self.lines.iter()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.pending.is_empty()
    }

    pub fn total_chars(&self) -> u64 {
        self.total_chars
    }

    /// Lines appended since the log was created.
    pub fn appended(&self) -> u64 {
        self.appended
    }

    /// Lines still being assembled, one per active channel. Shown at the bottom
    /// of the view so text appears as it decodes rather than a line at a time.
    pub fn pending(&self) -> &[PendingLine] {
        &self.pending
    }

    /// Appends decoded text produced by one channel.
    pub fn push_text(&mut self, channel: u32, hz: f32, text: &str, mode: Mode) {
        if text.is_empty() {
            return;
        }

        let slot = match self.pending.iter().position(|p| p.channel == channel) {
            Some(i) => i,
            None => {
                // A bank at its ceiling plus the teleprinter path cannot exceed
                // the capacity, so this only triggers after a configuration
                // change; the oldest line is finished to make room.
                if self.pending.len() >= MAX_PENDING {
                    let oldest = self
                        .pending
                        .iter()
                        .enumerate()
                        .max_by(|a, b| {
                            a.1.idle.partial_cmp(&b.1.idle).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    self.flush(oldest);
                }
                self.pending.push(PendingLine {
                    channel,
                    hz,
                    mode,
                    text: String::with_capacity(WRAP_WIDTH),
                    stamp: crate::platform::utc_time_hms(),
                    idle: 0.0,
                });
                self.pending.len() - 1
            }
        };

        // The frequency of a line follows the detector: it drifts while the line
        // is assembled and the last reading is the one worth recording.
        self.pending[slot].hz = hz;
        self.pending[slot].mode = mode;
        self.pending[slot].idle = 0.0;

        for ch in text.chars() {
            if self.pending[slot].text.is_empty() {
                self.pending[slot].stamp = crate::platform::utc_time_hms();
            }
            self.total_chars += 1;

            if ch == '\n' {
                self.flush(slot);
                continue;
            }
            // Control codes other than the line feed carry no meaning here and
            // would corrupt the display width.
            if (ch as u32) < 0x20 {
                continue;
            }
            self.pending[slot].text.push(ch);
            if self.pending[slot].text.chars().count() >= WRAP_WIDTH {
                self.flush(slot);
            }
        }

        self.drop_empty();
    }

    /// Advances the idle timers. A long pause closes a line so the next
    /// transmission on that channel starts with its own timestamp.
    pub fn tick(&mut self, dt: f32) {
        let mut i = 0usize;
        while i < self.pending.len() {
            self.pending[i].idle += dt;
            if self.pending[i].idle >= IDLE_BREAK_S && !self.pending[i].text.is_empty() {
                self.flush(i);
            }
            i += 1;
        }
        self.drop_empty();
    }

    /// Finishes the line of a channel that is going away.
    pub fn close_channel(&mut self, channel: u32) {
        if let Some(i) = self.pending.iter().position(|p| p.channel == channel) {
            self.flush(i);
            self.pending.remove(i);
        }
    }

    /// Finishes the lines of every channel that is no longer in the bank.
    pub fn retain_channels(&mut self, alive: &[u32]) {
        let mut i = 0usize;
        while i < self.pending.len() {
            // The teleprinter path uses channel zero and is not part of the bank.
            if self.pending[i].channel != 0 && !alive.contains(&self.pending[i].channel) {
                self.flush(i);
                self.pending.remove(i);
                continue;
            }
            i += 1;
        }
    }

    /// Records a status note rather than received traffic. Used for mode
    /// changes and for decoder faults.
    pub fn push_note(&mut self, text: &str) {
        let (h, m, s) = crate::platform::utc_time_hms();
        self.append(DecodeLine {
            hour: h,
            minute: m,
            second: s,
            mode: Mode::Unknown,
            channel: 0,
            hz: 0.0,
            text: format!("[{}]", text),
        });
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.pending.clear();
    }

    /// Moves the text of one slot into the finished buffer, leaving the slot in
    /// place so the channel keeps its position in the display.
    fn flush(&mut self, slot: usize) {
        if slot >= self.pending.len() || self.pending[slot].text.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.pending[slot].text);
        let (h, m, s) = self.pending[slot].stamp;
        let line = DecodeLine {
            hour: h,
            minute: m,
            second: s,
            mode: self.pending[slot].mode,
            channel: self.pending[slot].channel,
            hz: self.pending[slot].hz,
            text,
        };
        self.pending[slot].idle = 0.0;
        self.append(line);
    }

    /// Removes slots that carry nothing and have been quiet long enough that the
    /// channel is unlikely to add to them.
    fn drop_empty(&mut self) {
        self.pending
            .retain(|p| !p.text.is_empty() || p.idle < IDLE_BREAK_S);
    }

    fn append(&mut self, line: DecodeLine) {
        if let Some(f) = self.transcript.as_mut() {
            let _ = writeln!(
                f,
                "{} {:>6} {:>5} Hz  {}",
                line.stamp_full(),
                line.mode.as_str(),
                if line.hz > 0.0 { line.hz.round() } else { 0.0 },
                line.text
            );
            let _ = f.flush();
        }
        if self.lines.len() >= self.limit {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.appended += 1;
    }
}