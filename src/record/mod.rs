//! Cyclic recording and export.
//!
//! ## Where the audio is taken from
//!
//! The capture chain, at the decoder rate, through a queue of its own. Three
//! places were possible and the other two are wrong.
//!
//! The device rate is wrong because it changes with the device: a segment
//! recorded through one sound card would replay through a chain planned for
//! another, and every timing estimate in the decoders is expressed in samples.
//!
//! The interface thread is wrong because it drops blocks under backlog. That is
//! correct behaviour for a display, which must stay in real time, and it is the
//! one behaviour a recording cannot have: a gap in a file is not recoverable and
//! nothing downstream can tell it from a transmitter that stopped.
//!
//! So the recorder owns a tap of its own, four seconds deep, and a thread of its
//! own. A disk that stalls costs queue depth rather than samples, and if the
//! queue does overflow the capture thread counts it.
//!
//! ## Why segments
//!
//! A ring on disk could be one file with a wrapping write position. Segments
//! win on three counts, all of them about failure rather than about speed. A
//! process that dies loses the segment it was writing rather than the whole
//! ring. Discarding the oldest is a file delete instead of a rewrite of
//! everything after it. And the operator can copy one out with the tools they
//! already have.
//!
//! There is no manifest. The start time is in the file name, so sorting by name
//! sorts by time, and a directory that survived a crash still scans correctly.
//! A manifest would be one more thing that can disagree with the files.

pub mod lossless;
pub mod qoa;
pub mod rxr;
pub mod wav;
pub mod replay;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::config::settings::{ExportFormat, RecordFormat, RecordSettings};
use crate::core::ring::Consumer;
use crate::core::{Error, Result};

/// Samples read from the tap in one call.
const CHUNK: usize = 4096;

/// Time the thread waits when the tap is empty.
///
/// Long enough that an idle recorder costs nothing, short enough that a stop
/// request closes the segment promptly.
const IDLE_MS: u32 = 20;

/// Extension every segment carries.
pub const EXTENSION: &str = "rxr";

// ------------------------------------------------------------ shared state

/// What the interface thread tells the recorder about the reception.
///
/// Two atomics rather than five. The mode and the sideband qualify each other,
/// so a reader that caught one of the new pair and one of the old would write a
/// marker describing a state the receiver was never in; packing them into one
/// word removes the possibility rather than making it unlikely.
#[derive(Debug)]
pub struct SharedMeta {
    dial: AtomicI64,
    /// Bits 0 to 31 the tuning point as float bits, 32 to 39 the mode, 40 to 47
    /// the sideband, 48 to 55 the flags.
    packed: AtomicU64,
}

impl SharedMeta {
    pub fn new() -> Arc<SharedMeta> {
        Arc::new(SharedMeta {
            dial: AtomicI64::new(0),
            packed: AtomicU64::new(0),
        })
    }

    /// Publishes the state of the reception. Called once per frame.
    pub fn publish(
        &self,
        dial: Option<i64>,
        tune_hz: f32,
        mode: u8,
        sideband: u8,
        sdr_mode: bool,
    ) {
        let mut flags = 0u8;
        if dial.is_some() {
            flags |= rxr::MARK_DIAL_VALID;
        }
        if sdr_mode {
            flags |= rxr::MARK_SDR_MODE;
        }
        let packed = (tune_hz.to_bits() as u64)
            | ((mode as u64) << 32)
            | ((sideband as u64) << 40)
            | ((flags as u64) << 48);
        self.dial.store(dial.unwrap_or(0), Ordering::Relaxed);
        self.packed.store(packed, Ordering::Release);
    }

    fn read(&self) -> rxr::Marker {
        let packed = self.packed.load(Ordering::Acquire);
        rxr::Marker {
            dial_hz: self.dial.load(Ordering::Relaxed),
            tune_hz: f32::from_bits(packed as u32),
            peak_db: 0.0,
            rms_db: 0.0,
            mode: (packed >> 32) as u8,
            sideband: (packed >> 40) as u8,
            flags: (packed >> 48) as u8,
        }
    }
}

struct Shared {
    running: AtomicBool,
    /// Segments closed since the recorder started.
    segments: AtomicU32,
    /// Bytes the current segment holds.
    current_bytes: AtomicU64,
    /// Bytes the whole directory holds, refreshed after each rotation.
    total_bytes: AtomicU64,
    /// Blocks the tap could not supply in time, which is a gap in the file.
    gaps: AtomicU64,
    /// Seconds recorded since the start.
    seconds_bits: AtomicU32,
    /// Name of the segment being written, for a readout.
    current: Mutex<String>,
    error: Mutex<String>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            segments: AtomicU32::new(0),
            current_bytes: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            gaps: AtomicU64::new(0),
            seconds_bits: AtomicU32::new(0.0f32.to_bits()),
            current: Mutex::new(String::new()),
            error: Mutex::new(String::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecorderStatus {
    pub running: bool,
    pub segments: u32,
    pub current_bytes: u64,
    pub total_bytes: u64,
    pub gaps: u64,
    pub seconds: f32,
    pub current: String,
    pub error: String,
}

impl RecorderStatus {
    pub fn idle() -> RecorderStatus {
        RecorderStatus {
            running: false,
            segments: 0,
            current_bytes: 0,
            total_bytes: 0,
            gaps: 0,
            seconds: 0.0,
            current: String::new(),
            error: String::new(),
        }
    }
}

/// Everything the recorder needs that cannot change while it runs.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub directory: PathBuf,
    pub rate: u32,
    /// Channels written. Two whenever the input carries a quadrature pair, and
    /// one otherwise: a real input duplicates its channel and storing the copy
    /// would double the file for nothing.
    pub channels: usize,
    pub complex: bool,
    pub format: rxr::SampleFormat,
    pub block_len: usize,
    pub segment_blocks: u32,
    pub budget_bytes: u64,
    pub meta: Arc<SharedMeta>,
}

impl RecorderConfig {
    /// Derives the configuration from the settings and the running rate.
    pub fn from_settings(
        settings: &RecordSettings,
        rate: u32,
        complex: bool,
        meta: Arc<SharedMeta>,
    ) -> RecorderConfig {
        let block_len = ((rate as f32 * settings.block_seconds).round() as usize).clamp(64, 65536);
        let per_second = rate as f32 / block_len as f32;
        let segment_blocks =
            ((settings.segment_seconds as f32 * per_second).round() as u32).max(1);
        RecorderConfig {
            directory: resolve(&settings.path),
            rate,
            channels: if complex { 2 } else { 1 },
            complex,
            format: match settings.format {
                RecordFormat::I16 => rxr::SampleFormat::I16,
                RecordFormat::F32 => rxr::SampleFormat::F32,
            },
            block_len,
            segment_blocks,
            budget_bytes: settings.budget_mb as u64 * 1024 * 1024,
            meta,
        }
    }
}

/// Resolves a configured path against the directory of the configuration file.
///
/// The working directory is not it: a shortcut started from anywhere has to find
/// the same recordings as a double click on the executable.
pub fn resolve(path: &str) -> PathBuf {
    let stated = Path::new(path);
    if stated.is_absolute() {
        return stated.to_path_buf();
    }
    let base = crate::config::Settings::default_path()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    base.join(stated)
}

// ---------------------------------------------------------------- recorder

pub struct Recorder {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Takes the reading end of the capture tap and starts writing.
    pub fn start(cfg: RecorderConfig, source: Arc<Consumer<[f32; 2]>>) -> Result<Recorder> {
        std::fs::create_dir_all(&cfg.directory)?;

        let shared = Arc::new(Shared::new());
        let stop = Arc::new(AtomicBool::new(false));

        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread_cfg = cfg.clone();

        let handle = std::thread::Builder::new()
            .name("rxscope-record".to_string())
            .spawn(move || {
                let result = run(&thread_cfg, source, &thread_shared, &thread_stop);
                thread_shared.running.store(false, Ordering::Release);
                if let Err(e) = result {
                    crate::log_error!("record", "stopped: {}", e);
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = e.to_string();
                    }
                }
            })
            .map_err(|e| Error::io(format!("cannot start the recorder thread: {}", e)))?;

        crate::log_info!(
            "record",
            "started in {}, {} Hz {} channel {:?}, {} frames per block, {} blocks per segment",
            cfg.directory.display(),
            cfg.rate,
            cfg.channels,
            cfg.format,
            cfg.block_len,
            cfg.segment_blocks
        );

        Ok(Recorder { shared, stop, handle: Some(handle) })
    }

    pub fn status(&self) -> RecorderStatus {
        let current = self
            .shared
            .current
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        let error = self
            .shared
            .error
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        RecorderStatus {
            running: self.shared.running.load(Ordering::Acquire),
            segments: self.shared.segments.load(Ordering::Relaxed),
            current_bytes: self.shared.current_bytes.load(Ordering::Relaxed),
            total_bytes: self.shared.total_bytes.load(Ordering::Relaxed),
            gaps: self.shared.gaps.load(Ordering::Relaxed),
            seconds: f32::from_bits(self.shared.seconds_bits.load(Ordering::Relaxed)),
            current,
            error,
        }
    }

    /// Signals the thread and waits for it.
    ///
    /// Waiting rather than detaching, because the segment being written is only
    /// complete once its block count is patched, and a caller that stopped the
    /// recorder is normally about to read the directory.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    cfg: &RecorderConfig,
    source: Arc<Consumer<[f32; 2]>>,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<()> {
    shared.running.store(true, Ordering::Release);
    shared
        .total_bytes
        .store(directory_bytes(&cfg.directory), Ordering::Relaxed);

    let mut input = vec![[0.0f32, 0.0f32]; CHUNK];
    let mut block: Vec<[f32; 2]> = Vec::with_capacity(cfg.block_len);
    let mut writer: Option<rxr::Writer> = None;
    let mut blocks_in_segment = 0u32;
    // The marker is captured when a block opens rather than when it closes: it
    // describes the state the samples were received in, and by the time the
    // block is full the dial may have moved.
    let mut marker = rxr::Marker::default();
    let mut recorded = 0f64;

    while !stop.load(Ordering::Relaxed) {
        let n = source.read(&mut input);
        if n == 0 {
            crate::platform::sleep_ms(IDLE_MS);
            continue;
        }

        for &frame in &input[..n] {
            if block.is_empty() {
                marker = cfg.meta.read();
            }
            block.push(frame);
            if block.len() < cfg.block_len {
                continue;
            }

            // Levels are measured here rather than taken from the capture
            // status, because a scrub bar has to reflect the block it sits over
            // and the status reflects whatever the last frame saw.
            let (peak, rms) = levels(&block, cfg.channels);
            marker.peak_db = peak;
            marker.rms_db = rms;

            if writer.is_none() {
                let (file, name) = segment_path(&cfg.directory);
                let info = rxr::Info {
                    channels: cfg.channels,
                    rate: cfg.rate,
                    format: cfg.format,
                    complex: cfg.complex,
                    block_len: cfg.block_len,
                    start_unix: unix_seconds(),
                    start_ticks: crate::core::time::ticks(),
                };
                writer = Some(rxr::Writer::create(&file, info)?);
                blocks_in_segment = 0;
                if let Ok(mut slot) = shared.current.lock() {
                    *slot = name;
                }
            }

            if let Some(w) = writer.as_mut() {
                w.write_block(&marker, &block)?;
                blocks_in_segment += 1;
                shared.current_bytes.store(w.bytes(), Ordering::Relaxed);
                recorded += cfg.block_len as f64 / cfg.rate.max(1) as f64;
                shared
                    .seconds_bits
                    .store((recorded as f32).to_bits(), Ordering::Relaxed);
            }
            block.clear();

            if blocks_in_segment >= cfg.segment_blocks {
                close(&mut writer, shared)?;
                prune(&cfg.directory, cfg.budget_bytes, shared);
            }
        }
    }

    // The partial block is discarded rather than padded. Padding would put up
    // to half a second of silence at the end of every recording, and nothing
    // downstream could tell it from a band that went quiet.
    close(&mut writer, shared)?;
    prune(&cfg.directory, cfg.budget_bytes, shared);
    crate::log_info!("record", "stopped, {:.1} seconds written", recorded);
    Ok(())
}

fn close(writer: &mut Option<rxr::Writer>, shared: &Shared) -> Result<()> {
    if let Some(w) = writer.take() {
        let blocks = w.finish()?;
        shared.segments.fetch_add(1, Ordering::Relaxed);
        shared.current_bytes.store(0, Ordering::Relaxed);
        crate::log_debug!("record", "segment closed, {} blocks", blocks);
    }
    if let Ok(mut slot) = shared.current.lock() {
        slot.clear();
    }
    Ok(())
}

/// Peak and mean square of a block, in decibels.
fn levels(block: &[[f32; 2]], channels: usize) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    let mut count = 0u64;
    for frame in block {
        for c in 0..channels {
            let v = frame[c];
            let a = v.abs();
            if a > peak {
                peak = a;
            }
            sum += (v as f64) * (v as f64);
            count += 1;
        }
    }
    let rms = if count > 0 { (sum / count as f64).sqrt() as f32 } else { 0.0 };
    (to_db(peak), to_db(rms))
}

fn to_db(v: f32) -> f32 {
    if v <= 1e-9 {
        -180.0
    } else {
        20.0 * v.log10()
    }
}

/// Builds a path whose name sorts by time.
///
/// The wall clock rather than a counter, because the name is what an operator
/// reads to find a reception, and a counter says nothing about when it happened.
/// A collision within one second is broken by a suffix rather than by
/// overwriting.
fn segment_path(directory: &Path) -> (PathBuf, String) {
    let (year, month, day, hour, minute, second) = crate::platform::local_time_full();
    let base = format!(
        "rx-{:04}{:02}{:02}-{:02}{:02}{:02}",
        year, month, day, hour, minute, second
    );
    let mut name = format!("{}.{}", base, EXTENSION);
    let mut suffix = 1u32;
    while directory.join(&name).exists() {
        name = format!("{}-{}.{}", base, suffix, EXTENSION);
        suffix += 1;
        if suffix > 999 {
            break;
        }
    }
    (directory.join(&name), name)
}

fn unix_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- catalogue

/// One segment on disk, as a directory scan sees it.
#[derive(Debug, Clone)]
pub struct SegmentInfo {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    /// Header, absent when the file cannot be read as a segment.
    pub info: Option<rxr::Info>,
    pub blocks: usize,
    pub seconds: f64,
}

/// Lists the segments of a directory, oldest first.
///
/// Sorted by name, which sorts by time because the name carries it. Sorting by
/// modification time would be the obvious alternative and is wrong: copying a
/// directory rewrites every timestamp and the order would be whatever the copy
/// happened to produce.
pub fn scan(directory: &Path) -> Vec<SegmentInfo> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(directory) {
        Ok(e) => e,
        Err(_) => return out,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let is_segment = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case(EXTENSION))
            .unwrap_or(false);
        if !is_segment {
            continue;
        }
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        // A segment that cannot be read is listed rather than hidden: an
        // operator whose recording will not open is told which file is at fault
        // instead of finding it missing.
        let (info, blocks, seconds) = match rxr::Reader::open(&path) {
            Ok(reader) => (Some(reader.info()), reader.blocks(), reader.seconds()),
            Err(_) => (None, 0, 0.0),
        };

        out.push(SegmentInfo { path, name, bytes, info, blocks, seconds });
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn directory_bytes(directory: &Path) -> u64 {
    scan(directory).iter().map(|s| s.bytes).sum()
}

/// Deletes the oldest segments until the directory is inside the budget.
///
/// The one thing that makes the recording cyclic. A segment that cannot be
/// deleted stops the loop rather than being skipped: skipping it would delete
/// the next oldest instead, and the operator would lose a recording they can
/// still read in order to keep one they cannot.
fn prune(directory: &Path, budget: u64, shared: &Shared) {
    let mut segments = scan(directory);
    let mut total: u64 = segments.iter().map(|s| s.bytes).sum();

    while total > budget && segments.len() > 1 {
        let oldest = segments.remove(0);
        match std::fs::remove_file(&oldest.path) {
            Ok(()) => {
                total = total.saturating_sub(oldest.bytes);
                crate::log_info!(
                    "record",
                    "{} discarded, {:.0} MB in the directory",
                    oldest.name,
                    total as f64 / (1024.0 * 1024.0)
                );
            }
            Err(e) => {
                crate::log_warn!("record", "cannot discard {}: {}", oldest.name, e);
                break;
            }
        }
    }

    shared.total_bytes.store(total, Ordering::Relaxed);
}

// ------------------------------------------------------------------ export

/// Writes a segment into an export container.
///
/// One segment at a time. A range across several is the business of the replay
/// side, which knows which segments are contiguous; this is the step that turns
/// one file into another and it has no reason to know.
///
/// The destination path is derived from the source name, so an export never
/// silently replaces a different recording.
pub fn export_segment(
    source: &Path,
    directory: &Path,
    format: ExportFormat,
    bits: u32,
) -> Result<PathBuf> {
    let mut reader = rxr::Reader::open(source)?;
    let info = reader.info();
    if reader.blocks() == 0 {
        return Err(Error::io(format!("{} holds no audio", source.display())));
    }

    std::fs::create_dir_all(directory)?;
    let stem = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let extension = match format {
        ExportFormat::Wav => "wav",
        ExportFormat::Lossless => "rxl",
        ExportFormat::Qoa => "qoa",
    };
    let mut dest = directory.join(format!("{}.{}", stem, extension));
    let mut suffix = 1u32;
    while dest.exists() {
        dest = directory.join(format!("{}-{}.{}", stem, suffix, extension));
        suffix += 1;
        if suffix > 999 {
            return Err(Error::io("too many exports of the same segment"));
        }
    }

    let mut marker = rxr::Marker::default();
    let mut block: Vec<[f32; 2]> = Vec::with_capacity(info.block_len);

    // The three writers share no trait. A trait would need the finish call to
    // take the value, which a trait object cannot do, and the alternative is a
    // finish that leaves a usable object behind, which is a worse interface than
    // three branches.
    let frames = match format {
        ExportFormat::Wav => {
            let mut w = wav::Writer::create(&dest, info.rate, info.channels as u16, bits)?;
            for index in 0..reader.blocks() {
                reader.read_block(index, &mut marker, &mut block)?;
                w.push(&block)?;
            }
            w.finish()?
        }
        ExportFormat::Lossless => {
            let mut w = lossless::Writer::create(&dest, info.rate, info.channels)?;
            for index in 0..reader.blocks() {
                reader.read_block(index, &mut marker, &mut block)?;
                w.push(&block)?;
            }
            w.finish()?
        }
        ExportFormat::Qoa => {
            let mut w = qoa::Writer::create(&dest, info.rate, info.channels)?;
            for index in 0..reader.blocks() {
                reader.read_block(index, &mut marker, &mut block)?;
                w.push(&block)?;
            }
            w.finish()?
        }
    };

    // The lossless container is read back before the export is called done.
    // The whole reason to keep every sample is to decode from them again later,
    // and an archive that cannot be opened is worth less than no archive: the
    // operator believes they have it. The other two formats are not checked
    // because neither has a decoder in this build, and a check that cannot fail
    // is not a check.
    //
    // A file that fails is removed rather than left behind. A file sitting in
    // the export directory is one somebody will reach for, and one that cannot
    // be read wastes the attempt at the moment the recording is wanted.
    if format == ExportFormat::Lossless {
        match lossless::decode(&dest) {
            Ok((rate, channels, samples)) => {
                if rate != info.rate
                    || channels != info.channels
                    || samples.len() as u64 != frames
                {
                    let _ = std::fs::remove_file(&dest);
                    return Err(Error::io(format!(
                        "{} reads back as {} Hz {} channel {} frames rather than {} Hz {} channel {} frames",
                        dest.display(),
                        rate,
                        channels,
                        samples.len(),
                        info.rate,
                        info.channels,
                        frames
                    )));
                }
            }
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                crate::log_error!("record", "{} cannot be read back: {}", dest.display(), e);
                return Err(e);
            }
        }
    }

    let out_bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    let in_bytes = std::fs::metadata(source).map(|m| m.len()).unwrap_or(1);
    crate::log_info!(
        "record",
        "{} exported to {}, {} frames, {:.0} percent of the segment",
        source.display(),
        dest.display(),
        frames,
        out_bytes as f64 * 100.0 / in_bytes.max(1) as f64
    );
    Ok(dest)
}