//! Replay of a recorded ring.
//!
//! ## Why a thread rather than a read on the interface thread
//!
//! A recording read as fast as the disk allows would pass a minute of audio
//! through the decoders in one frame. Every timing estimate in them is expressed
//! in samples and is compared against wall clock nowhere, so the decoding would
//! be correct and the display would be a blur; the waterfall would scroll a
//! thousand lines between two frames and the operator would see the last of
//! them.
//!
//! So the replay is paced, and pacing on the interface thread would tie the
//! playback rate to the frame rate. A thread of its own with a queue between
//! them is the same arrangement the capture path already uses, which is what
//! lets the shell swap one for the other with a branch rather than a rewrite.
//!
//! ## Why the queue is shallow
//!
//! The queue depth is the distance between what the operator hears and what the
//! markers say the dial was. A deep queue absorbs a stalled frame and lies about
//! the frequency by its own depth; three blocks is enough for the jitter of a
//! frame and bounds the error to a second and a half at the default block, which
//! only matters while a dial is being swept.
//!
//! A block that does not fit is retried rather than dropped. Dropping is right
//! for a monitor, where a late sample is worse than a missing one, and wrong
//! here: the whole reason to replay is to decode something that was missed, and
//! a decoder cannot decode a hole.
//!
//! ## Following the live edge
//!
//! The newest segment grows while the recorder writes it. A replay positioned at
//! its end waits rather than stopping, and the timeline is refreshed to pick up
//! the blocks that arrived meanwhile. That is what makes a recording browsable
//! the way live audio is: the operator scrubs back, listens, and lets the
//! position run forward until it catches up with the present.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::core::ring::{self, Consumer, Producer};
use crate::core::time::{ticks, ticks_per_second};
use crate::core::{Error, Result};

use super::rxr;
use super::{scan, SegmentInfo};

/// Blocks the queue holds. See the note on depth above.
const RING_BLOCKS: usize = 3;

/// Time the thread waits when it has nothing to do.
///
/// Short enough that a seek or an unpause is acted on within a frame, long
/// enough that a paused replay costs nothing measurable.
const IDLE_MS: u32 = 5;

/// Periods the deadline may fall behind before it is resynchronized.
///
/// A stall longer than this means the wall clock has moved past what the queue
/// can absorb, and catching up would deliver a burst of audio the display cannot
/// follow. Resynchronizing loses no samples, only the claim that playback is
/// still in step with the clock it started against.
const RESYNC_PERIODS: f64 = 8.0;

/// Slowest and fastest playback.
///
/// The floor is where an operator picks apart one character of keying; the
/// ceiling is where the waterfall still shows a stripe rather than a smear. Past
/// either the display stops being informative, which is the only reason a bound
/// exists at all.
pub const SPEED_MIN: f32 = 0.25;
pub const SPEED_MAX: f32 = 8.0;

// ---------------------------------------------------------------- timeline

/// One segment as the timeline sees it.
#[derive(Debug, Clone)]
pub struct Segment {
    pub path: PathBuf,
    pub name: String,
    pub blocks: usize,
    /// Wall clock of the first sample.
    pub start_unix: i64,
    /// Monotonic counter at the same instant.
    pub start_ticks: i64,
    /// True when this segment does not continue the previous one.
    ///
    /// Derived from the monotonic counter rather than from the wall clock,
    /// because a clock adjustment between two segments would make contiguous
    /// audio look discontinuous and a discontinuity look like an overlap.
    pub gap_before: bool,
}

/// One block, as a position in the whole recording.
#[derive(Debug, Clone, Copy)]
pub struct BlockRef {
    pub segment: u32,
    /// Index inside that segment.
    pub local: u32,
    pub marker: rxr::Marker,
}

/// Every block of every segment, in order.
///
/// Flattened on purpose. Seeking then costs one index rather than a search
/// through the segments, and a scrub bar is an array walk rather than a nested
/// loop over files.
///
/// The markers are held in memory and the samples are not. A marker is thirty
/// two bytes and a block is tens of kilobytes, so an hour of recording is a
/// quarter of a megabyte of markers: the whole timeline, the levels for the
/// scrub bar and the dial frequency of every moment, without touching the audio.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    pub segments: Vec<Segment>,
    pub blocks: Vec<BlockRef>,
    /// Geometry taken from the first readable segment.
    pub rate: u32,
    pub channels: usize,
    pub complex: bool,
    pub block_len: usize,
    pub block_seconds: f64,
    /// Segments that could not be read as such, for a diagnostic.
    pub rejected: usize,
}

impl Timeline {
    /// Reads a directory into a timeline.
    ///
    /// A segment whose geometry differs from the first is skipped rather than
    /// adapted: the replay feeds one chain at one rate, and a mid recording rate
    /// change would need the chain rebuilt in the middle of playback. Such a
    /// segment is still listed by the catalogue, so an operator can export it on
    /// its own.
    pub fn build(directory: &Path) -> Timeline {
        let mut out = Timeline::default();
        let found = scan(directory);

        for entry in &found {
            let info = match entry.info {
                Some(i) => i,
                None => {
                    out.rejected += 1;
                    continue;
                }
            };
            if entry.blocks == 0 {
                continue;
            }

            if out.segments.is_empty() {
                out.rate = info.rate;
                out.channels = info.channels;
                out.complex = info.complex;
                out.block_len = info.block_len;
                out.block_seconds = info.block_seconds();
            } else if info.rate != out.rate
                || info.channels != out.channels
                || info.block_len != out.block_len
            {
                out.rejected += 1;
                continue;
            }

            // Contiguity. The tolerance is one block, which covers the interval
            // between closing one segment and opening the next.
            let gap_before = match out.segments.last() {
                None => false,
                Some(previous) => {
                    let per_second = ticks_per_second();
                    let expected = previous.start_ticks
                        + (previous.blocks as f64 * out.block_seconds * per_second) as i64;
                    let slack = (out.block_seconds * per_second) as i64;
                    (info.start_ticks - expected).abs() > slack
                }
            };

            let index = out.segments.len() as u32;
            out.segments.push(Segment {
                path: entry.path.clone(),
                name: entry.name.clone(),
                blocks: entry.blocks,
                start_unix: info.start_unix,
                start_ticks: info.start_ticks,
                gap_before,
            });

            out.read_markers(index, 0, entry.blocks);
        }

        crate::log_info!(
            "replay",
            "{} segments, {} blocks, {:.1} seconds, {} rejected",
            out.segments.len(),
            out.blocks.len(),
            out.seconds(),
            out.rejected
        );
        out
    }

    /// Appends the markers of a range of one segment.
    fn read_markers(&mut self, segment: u32, from: usize, to: usize) {
        let path = match self.segments.get(segment as usize) {
            Some(s) => s.path.clone(),
            None => return,
        };
        let mut reader = match rxr::Reader::open(&path) {
            Ok(r) => r,
            Err(e) => {
                crate::log_warn!("replay", "cannot read {}: {}", path.display(), e);
                return;
            }
        };
        for local in from..to.min(reader.blocks()) {
            let marker = reader.read_marker(local).unwrap_or_default();
            self.blocks.push(BlockRef { segment, local: local as u32, marker });
        }
    }

    /// Picks up blocks the recorder appended to the newest segment.
    ///
    /// Only the last segment is examined. An earlier one cannot grow: the
    /// recorder closes a segment before opening the next and never returns to
    /// it, so rescanning them would be reading the whole directory to learn
    /// nothing.
    ///
    /// A segment that appeared since the last build is not picked up here. That
    /// needs a full rebuild, because a new segment may be discontinuous with the
    /// previous one and the contiguity of the whole chain has to be recomputed.
    /// The caller decides when to pay for that.
    pub fn refresh_tail(&mut self) -> usize {
        let index = match self.segments.len().checked_sub(1) {
            Some(i) => i,
            None => return 0,
        };
        let path = self.segments[index].path.clone();
        let known = self.segments[index].blocks;

        let now = match rxr::Reader::open(&path) {
            Ok(r) => r.blocks(),
            Err(_) => return 0,
        };
        if now <= known {
            return 0;
        }

        self.segments[index].blocks = now;
        self.read_markers(index as u32, known, now);
        now - known
    }

    /// True when a new segment has appeared or the tail grew.
    ///
    /// Cheap: a directory listing and one header read, no markers. Called on a
    /// timer so the interface notices a recorder that rotated without paying for
    /// a rebuild every frame.
    pub fn is_stale(&self, directory: &Path) -> bool {
        let found = scan(directory);
        let usable = found.iter().filter(|s| s.info.is_some() && s.blocks > 0).count();
        if usable != self.segments.len() {
            return true;
        }
        match (found.last(), self.segments.last()) {
            (Some(newest), Some(last)) => newest.blocks > last.blocks,
            _ => false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    pub fn seconds(&self) -> f64 {
        self.blocks.len() as f64 * self.block_seconds
    }

    pub fn marker(&self, index: usize) -> rxr::Marker {
        self.blocks.get(index).map(|b| b.marker).unwrap_or_default()
    }

    /// Wall clock of a block, seconds since the epoch.
    ///
    /// Derived from the segment start rather than stored per block: a block
    /// carries what changes and the start time does not.
    pub fn unix_of(&self, index: usize) -> i64 {
        match self.blocks.get(index) {
            Some(b) => match self.segments.get(b.segment as usize) {
                Some(s) => s.start_unix + (b.local as f64 * self.block_seconds) as i64,
                None => 0,
            },
            None => 0,
        }
    }

    pub fn block_at_fraction(&self, t: f32) -> usize {
        if self.blocks.is_empty() {
            return 0;
        }
        let last = self.blocks.len() - 1;
        ((t.clamp(0.0, 1.0) * self.blocks.len() as f32) as usize).min(last)
    }

    pub fn fraction_of_block(&self, index: usize) -> f32 {
        if self.blocks.is_empty() {
            return 0.0;
        }
        index as f32 / self.blocks.len() as f32
    }

    /// True when the block does not continue the previous one.
    pub fn is_break(&self, index: usize) -> bool {
        match self.blocks.get(index) {
            Some(b) => {
                b.local == 0
                    && self
                        .segments
                        .get(b.segment as usize)
                        .map(|s| s.gap_before)
                        .unwrap_or(false)
            }
            None => false,
        }
    }
}

// ------------------------------------------------------------------ stream

struct Shared {
    running: AtomicBool,
    playing: AtomicBool,
    /// Playback rate as float bits.
    speed_bits: AtomicU32,
    /// Requested position, or minus one when none is pending.
    ///
    /// A request rather than a direct write, because the thread has to clear the
    /// queue as part of the move: audio already in it belongs to the old
    /// position and would be heard after the jump.
    seek: AtomicI64,
    /// Block the thread last delivered.
    position: AtomicU64,
    /// Raised on every seek, so the consumer knows the chain holds audio from a
    /// discontinuity and must be reset.
    generation: AtomicU32,
    /// Wait at the end instead of stopping.
    follow: AtomicBool,
    /// Return to the start instead of stopping.
    looping: AtomicBool,
    /// Set when playback reached the end and neither of the two above applies.
    ended: AtomicBool,
    /// Blocks that could not be read, which is a fault in the file.
    faults: AtomicU64,
    error: Mutex<String>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            playing: AtomicBool::new(false),
            speed_bits: AtomicU32::new(1.0f32.to_bits()),
            seek: AtomicI64::new(-1),
            position: AtomicU64::new(0),
            generation: AtomicU32::new(0),
            follow: AtomicBool::new(true),
            looping: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            faults: AtomicU64::new(0),
            error: Mutex::new(String::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplayStatus {
    pub running: bool,
    pub playing: bool,
    pub speed: f32,
    /// Block being delivered.
    pub position: usize,
    pub blocks: usize,
    pub generation: u32,
    pub follow: bool,
    pub looping: bool,
    pub ended: bool,
    pub faults: u64,
    pub error: String,
}

impl ReplayStatus {
    pub fn idle() -> ReplayStatus {
        ReplayStatus {
            running: false,
            playing: false,
            speed: 1.0,
            position: 0,
            blocks: 0,
            generation: 0,
            follow: true,
            looping: false,
            ended: false,
            faults: 0,
            error: String::new(),
        }
    }

    pub fn seconds(&self, block_seconds: f64) -> f64 {
        self.position as f64 * block_seconds
    }

    pub fn fraction(&self) -> f32 {
        if self.blocks == 0 {
            0.0
        } else {
            self.position as f32 / self.blocks as f32
        }
    }
}

pub struct ReplayStream {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    consumer: Consumer<[f32; 2]>,
    /// Timeline shared with the thread.
    ///
    /// A lock rather than a copy per side. The thread takes it once per block,
    /// which at the fastest speed is a few dozen times a second, and the
    /// interface takes it when it redraws the scrub bar; neither is on a path
    /// where a lock is measurable.
    timeline: Arc<Mutex<Timeline>>,
    rate: u32,
    block_len: usize,
    block_seconds: f64,
}

impl ReplayStream {
    /// Opens a recording and starts the thread, paused.
    ///
    /// Paused rather than playing: opening a recording is a step towards
    /// deciding where to listen, and a replay that started at the beginning
    /// would have to be stopped before it could be positioned.
    pub fn open(timeline: Timeline) -> Result<ReplayStream> {
        if timeline.is_empty() {
            return Err(Error::io("the recording holds no readable audio"));
        }

        let rate = timeline.rate;
        let block_len = timeline.block_len;
        let block_seconds = timeline.block_seconds;

        let capacity = (block_len * RING_BLOCKS).max(4096);
        let (producer, consumer) = ring::channel::<[f32; 2]>(capacity);

        let shared = Arc::new(Shared::new());
        let stop = Arc::new(AtomicBool::new(false));
        let timeline = Arc::new(Mutex::new(timeline));

        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread_timeline = timeline.clone();

        let handle = std::thread::Builder::new()
            .name("rxscope-replay".to_string())
            .spawn(move || {
                let result = run(producer, thread_timeline, &thread_shared, &thread_stop);
                thread_shared.running.store(false, Ordering::Release);
                if let Err(e) = result {
                    crate::log_error!("replay", "stopped: {}", e);
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = e.to_string();
                    }
                }
            })
            .map_err(|e| Error::io(format!("cannot start the replay thread: {}", e)))?;

        crate::log_info!(
            "replay",
            "opened, {} Hz {} channel, {} frames per block, queue {} frames",
            rate,
            {
                let t = timeline.lock().unwrap_or_else(|e| e.into_inner());
                t.channels
            },
            block_len,
            capacity
        );

        Ok(ReplayStream {
            shared,
            stop,
            handle: Some(handle),
            consumer,
            timeline,
            rate,
            block_len,
            block_seconds,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn block_len(&self) -> usize {
        self.block_len
    }

    pub fn block_seconds(&self) -> f64 {
        self.block_seconds
    }

    pub fn available(&self) -> usize {
        self.consumer.len()
    }

    pub fn capacity(&self) -> usize {
        self.consumer.capacity()
    }

    pub fn read(&self, dst: &mut [[f32; 2]]) -> usize {
        self.consumer.read(dst)
    }

    pub fn skip(&self, count: usize) -> usize {
        self.consumer.skip(count)
    }

    /// Copy of the timeline, for drawing.
    ///
    /// A copy rather than a guard, because the caller draws with it while the
    /// thread keeps playing, and holding the lock across a frame would stall
    /// playback at every redraw.
    pub fn timeline(&self) -> Timeline {
        self.timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Marker of the block being delivered.
    ///
    /// What turns the replayed audio back into a picture of a band. Read per
    /// frame, so it takes the lock rather than copying the timeline.
    pub fn marker(&self) -> rxr::Marker {
        let index = self.shared.position.load(Ordering::Relaxed) as usize;
        self.timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .marker(index)
    }

    /// Picks up blocks the recorder appended.
    ///
    /// Called on a timer by the shell rather than by the thread, because a
    /// rebuild reads the whole directory and the thread must not stall.
    pub fn refresh_tail(&self) -> usize {
        self.timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refresh_tail()
    }

    /// Replaces the timeline outright, keeping the position.
    ///
    /// Used when a new segment appeared: the contiguity of the whole chain has
    /// to be recomputed, so the timeline is rebuilt outside the lock and swapped
    /// in here.
    pub fn replace_timeline(&self, next: Timeline) {
        if next.is_empty() {
            return;
        }
        let blocks = next.len();
        {
            let mut held = self.timeline.lock().unwrap_or_else(|e| e.into_inner());
            *held = next;
        }
        // A position past the end of the new timeline would leave the thread
        // waiting on a block that does not exist.
        let position = self.shared.position.load(Ordering::Relaxed) as usize;
        if position >= blocks {
            self.seek_block(blocks.saturating_sub(1));
        }
    }

    pub fn status(&self) -> ReplayStatus {
        let blocks = self
            .timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        let error = self
            .shared
            .error
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        ReplayStatus {
            running: self.shared.running.load(Ordering::Acquire),
            playing: self.shared.playing.load(Ordering::Relaxed),
            speed: f32::from_bits(self.shared.speed_bits.load(Ordering::Relaxed)),
            position: self.shared.position.load(Ordering::Relaxed) as usize,
            blocks,
            generation: self.shared.generation.load(Ordering::Relaxed),
            follow: self.shared.follow.load(Ordering::Relaxed),
            looping: self.shared.looping.load(Ordering::Relaxed),
            ended: self.shared.ended.load(Ordering::Relaxed),
            faults: self.shared.faults.load(Ordering::Relaxed),
            error,
        }
    }

    pub fn set_playing(&self, on: bool) {
        if on {
            self.shared.ended.store(false, Ordering::Relaxed);
        }
        self.shared.playing.store(on, Ordering::Relaxed);
    }

    pub fn set_speed(&self, speed: f32) {
        self.shared
            .speed_bits
            .store(speed.clamp(SPEED_MIN, SPEED_MAX).to_bits(), Ordering::Relaxed);
    }

    pub fn set_follow(&self, on: bool) {
        self.shared.follow.store(on, Ordering::Relaxed);
    }

    pub fn set_looping(&self, on: bool) {
        self.shared.looping.store(on, Ordering::Relaxed);
    }

    pub fn seek_block(&self, index: usize) {
        self.shared.ended.store(false, Ordering::Relaxed);
        self.shared.seek.store(index as i64, Ordering::Release);
    }

    pub fn seek_fraction(&self, t: f32) {
        let index = self
            .timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .block_at_fraction(t);
        self.seek_block(index);
    }

    /// Moves by whole blocks, which is the granularity the format seeks at.
    pub fn step(&self, blocks: i64) {
        let held = self.timeline.lock().unwrap_or_else(|e| e.into_inner());
        let last = held.len().saturating_sub(1) as i64;
        drop(held);
        let now = self.shared.position.load(Ordering::Relaxed) as i64;
        let wanted = (now + blocks).clamp(0, last);
        self.seek_block(wanted as usize);
    }

    /// Jumps to the newest block, which is where a following replay converges.
    pub fn seek_live(&self) {
        let last = self
            .timeline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
            .saturating_sub(1);
        self.seek_block(last);
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ReplayStream {
    fn drop(&mut self) {
        self.stop();
    }
}

// ------------------------------------------------------------------- thread

fn run(
    producer: Producer<[f32; 2]>,
    timeline: Arc<Mutex<Timeline>>,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<()> {
    shared.running.store(true, Ordering::Release);

    let mut block: Vec<[f32; 2]> = Vec::with_capacity(8192);
    let mut marker = rxr::Marker::default();
    // The reader is kept across blocks: consecutive blocks of one segment are
    // the common case and reopening the file for each of them would be a system
    // call per block for nothing.
    let mut open: Option<(u32, rxr::Reader)> = None;
    let mut position = 0usize;
    let mut next_due = ticks();
    let mut waiting = false;

    while !stop.load(Ordering::Relaxed) {
        // A seek is honoured before anything else. The queue is emptied as part
        // of it: what is in it belongs to the old position and would otherwise
        // be heard after the jump.
        let requested = shared.seek.swap(-1, Ordering::AcqRel);
        if requested >= 0 {
            position = requested as usize;
            shared.position.store(position as u64, Ordering::Relaxed);
            shared.generation.fetch_add(1, Ordering::Release);
            next_due = ticks();
            waiting = false;
            // Not through the consumer, which lives on the other side. The
            // producer cannot drain, so the queue is left to be consumed and the
            // generation tells the consumer to discard the chain state instead.
        }

        if !shared.playing.load(Ordering::Relaxed) {
            crate::platform::sleep_ms(IDLE_MS);
            next_due = ticks();
            continue;
        }

        let (total, block_seconds) = {
            let held = timeline.lock().unwrap_or_else(|e| e.into_inner());
            (held.len(), held.block_seconds)
        };

        if position >= total {
            // The end. Three answers and the operator chose which.
            if shared.looping.load(Ordering::Relaxed) && total > 0 {
                position = 0;
                shared.position.store(0, Ordering::Relaxed);
                shared.generation.fetch_add(1, Ordering::Release);
                next_due = ticks();
                continue;
            }
            if shared.follow.load(Ordering::Relaxed) {
                // Waiting rather than stopping is what makes the recording
                // browsable the way live audio is: the position sits at the edge
                // and moves on as the recorder appends.
                if !waiting {
                    waiting = true;
                    crate::log_debug!("replay", "at the live edge, waiting");
                }
                crate::platform::sleep_ms(IDLE_MS * 4);
                next_due = ticks();
                continue;
            }
            shared.ended.store(true, Ordering::Relaxed);
            shared.playing.store(false, Ordering::Relaxed);
            crate::log_info!("replay", "reached the end");
            continue;
        }
        waiting = false;

        // Pacing. The deadline is advanced by the period rather than measured
        // from the previous block, so a block that took longer than its period
        // does not add its own duration to the schedule.
        let speed = f32::from_bits(shared.speed_bits.load(Ordering::Relaxed))
            .clamp(SPEED_MIN, SPEED_MAX) as f64;
        let period = (block_seconds / speed * ticks_per_second()) as i64;
        let now = ticks();
        if now < next_due {
            crate::platform::sleep_ms(IDLE_MS);
            continue;
        }
        if now - next_due > (period as f64 * RESYNC_PERIODS) as i64 {
            next_due = now;
        }

        // Where the block lives. Taken per block because the timeline may have
        // grown or been replaced since the last one.
        let (segment, local, path) = {
            let held = timeline.lock().unwrap_or_else(|e| e.into_inner());
            match held.blocks.get(position) {
                Some(b) => {
                    let path = held
                        .segments
                        .get(b.segment as usize)
                        .map(|s| s.path.clone());
                    match path {
                        Some(p) => (b.segment, b.local as usize, p),
                        None => {
                            shared.faults.fetch_add(1, Ordering::Relaxed);
                            position += 1;
                            continue;
                        }
                    }
                }
                None => continue,
            }
        };

        let needs_open = match &open {
            Some((index, _)) => *index != segment,
            None => true,
        };
        if needs_open {
            match rxr::Reader::open(&path) {
                Ok(reader) => open = Some((segment, reader)),
                Err(e) => {
                    crate::log_warn!("replay", "cannot open {}: {}", path.display(), e);
                    shared.faults.fetch_add(1, Ordering::Relaxed);
                    position += 1;
                    continue;
                }
            }
        }

        let read = match open.as_mut() {
            Some((_, reader)) => reader.read_block(local, &mut marker, &mut block),
            None => Ok(0),
        };
        match read {
            Ok(0) => {
                // The segment shrank, which means the file was replaced under
                // the replay. The reader is dropped so the next pass reopens it.
                open = None;
                shared.faults.fetch_add(1, Ordering::Relaxed);
                position += 1;
                continue;
            }
            Ok(_) => {}
            Err(e) => {
                crate::log_warn!("replay", "block {} of {}: {}", local, path.display(), e);
                open = None;
                shared.faults.fetch_add(1, Ordering::Relaxed);
                position += 1;
                continue;
            }
        }

        // A block that does not fit is retried rather than dropped: the whole
        // reason to replay is to decode what was missed, and a decoder cannot
        // decode a hole. The deadline is not advanced either, so the retry does
        // not lose its place in the schedule.
        if !producer.write(&block) {
            crate::platform::sleep_ms(IDLE_MS);
            continue;
        }

        shared.position.store(position as u64, Ordering::Relaxed);
        position += 1;
        next_due += period;
    }

    crate::log_info!("replay", "thread ended at block {}", position);
    Ok(())
}