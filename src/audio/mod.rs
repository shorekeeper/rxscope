//! Audio capture subsystem.
//!
//! Layering: this module owns the capture thread, the lock free queue and the
//! published status. The device specific loops live in wasapi and wavein, the
//! conditioning chain in process and resample, the format handling in convert,
//! the headphone path in monitor. Only the raw declarations stay under the
//! platform layer.
//!
//! Nothing on the interface thread ever blocks on the capture thread, and the
//! capture thread stops allocating after the first block, which keeps the time
//! spent inside the driver callback bounded.
//!
//! Chain applied to every captured block, in order:
//!   interleaved device frames -> mono f32 (Converter)
//!   direct current removal, operator gain, level measurement (Frontend)
//!   rate conversion to the decoder rate (Resampler)
//!   lock free queue to the decoders (Producer)
//!   lock free queue to the monitor (Producer), when one is listening
//!
//! The two queues carry the same samples and are otherwise independent. That is
//! deliberate: the monitor is allowed to fall behind and drop, the decoders are
//! not, and a single queue would force one policy on both.

pub mod convert;
pub mod device;
pub mod monitor;
pub mod process;
pub mod resample;
pub mod wasapi;
pub mod wavein;


use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::config::settings::{AudioBackend, AudioSettings, ChannelMode, DspSettings};
use crate::core::ring::{self, Consumer, Producer};
use crate::core::{Error, Result};

use convert::{Converter, SampleFormat};
use process::{linear_to_db, Frontend};
use resample::Resampler;

pub use device::DeviceInfo;
pub use monitor::{MonitorConfig, MonitorStatus, MonitorStream};
pub use process::AgcConfig;

/// Seconds of audio the monitor tap holds.
///
/// Short on purpose. Every sample in it is a sample the operator has not heard
/// yet, and a monitor that lags the waterfall by a quarter of a second is worse
/// than useless for judging whether the detector is centred. A tenth of a second
/// is well above the jitter of either device and below what the ear notices as
/// delay against the display.
const MONITOR_TAP_SECONDS: f32 = 0.1;

/// Seconds of audio the recorder tap holds.
///
/// Far longer than the monitor tap, and for the opposite reason. The monitor
/// discards what it cannot play, because a sample heard late is worse than one
/// not heard; the recorder must not discard anything, because a hole in a
/// recording cannot be recovered from. Four seconds covers a disk that stalled
/// on a flush or on a directory scan.
const RECORD_TAP_SECONDS: f32 = 4.0;

/// Lists the devices that make sense for the selected backend. Enumeration
/// touches COM, so it runs on the caller thread and is not called per frame.
pub fn enumerate(kind: AudioBackend) -> Vec<DeviceInfo> {
    device::enumerate(kind)
}

/// Lists the endpoints the monitor can play to.
pub fn enumerate_output() -> Vec<DeviceInfo> {
    device::enumerate_render()
}

/// Dispatches to the backend loop. Blocks until the stop flag is raised or the
/// device fails, and returns the reason in the error case.
fn run_backend(cfg: &CaptureConfig, pipeline: &mut Pipeline, stop: &AtomicBool) -> Result<()> {
    // The direction comes from the device identifier, so the backend only picks
    // the API.
    match cfg.backend {
        AudioBackend::Wasapi => wasapi::run(cfg, pipeline, stop),
        AudioBackend::WaveIn => wavein::run(cfg, pipeline, stop),
    }
}

// ------------------------------------------------------------------ config

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub backend: AudioBackend,
    /// Endpoint identifier for WASAPI, decimal index for waveIn, empty for the
    /// system default.
    pub device_id: String,
    /// Rate asked of the device. Ignored by WASAPI shared mode, which always
    /// hands back the mix format.
    pub requested_rate: u32,
    pub period_ms: u32,
    pub exclusive: bool,
    pub channel_mode: ChannelMode,
    pub dc_block: bool,
    pub input_gain_db: f32,
    /// Rate the decoders run at, zero meaning follow the device.
    pub target_rate: u32,
    pub ring_seconds: f32,
}

impl CaptureConfig {
    /// Derives the configuration.
    ///
    /// The transform section takes part because the target rate is the stated
    /// rate divided by the reduction, and the resampler already band limits for
    /// whatever target it is given: a second decimation stage would need a
    /// second anti aliasing filter to do the same thing.
    pub fn from_settings(audio: &AudioSettings, dsp: &DspSettings) -> CaptureConfig {
        CaptureConfig {
            backend: audio.backend,
            device_id: audio.device_id.clone(),
            requested_rate: audio.sample_rate,
            period_ms: audio.capture_buffer_ms,
            exclusive: audio.exclusive_mode,
            channel_mode: audio.channel_mode,
            dc_block: audio.dc_block,
            input_gain_db: audio.input_gain_db,
            target_rate: dsp.effective_rate(audio.dsp_sample_rate),
            ring_seconds: audio.ring_seconds,
        }
    }
}

// ------------------------------------------------------------------ status

/// Format codes published through the atomic status. A plain integer avoids a
/// lock on a value the interface reads every frame.
const FMT_NONE: u32 = 0;
const FMT_U8: u32 = 1;
const FMT_I16: u32 = 2;
const FMT_I24: u32 = 3;
const FMT_I32: u32 = 4;
const FMT_F32: u32 = 5;

fn format_code(f: SampleFormat) -> u32 {
    match f {
        SampleFormat::U8 => FMT_U8,
        SampleFormat::I16 => FMT_I16,
        SampleFormat::I24 => FMT_I24,
        SampleFormat::I32 => FMT_I32,
        SampleFormat::F32 => FMT_F32,
    }
}

fn format_name(code: u32) -> &'static str {
    match code {
        FMT_U8 => "u8",
        FMT_I16 => "i16",
        FMT_I24 => "i24",
        FMT_I32 => "i32",
        FMT_F32 => "f32",
        _ => "none",
    }
}

struct Shared {
    running: AtomicBool,
    /// Set while a loopback stream receives nothing but silence. Not an error:
    /// the endpoint is open and healthy, there is simply no playback on it.
    silent: AtomicBool,
    /// Cleared while no monitor is listening, so the capture thread skips the
    /// tap write entirely rather than filling a queue nobody drains.
    monitor: AtomicBool,
    /// Cleared while nothing is recording, so the capture thread skips the
    /// second tap write entirely rather than filling a queue nobody drains.
    recording: AtomicBool,
    device_rate: AtomicU32,
    channels: AtomicU32,
    format: AtomicU32,
    /// Levels are stored as the raw bits of an f32; each slot is written by one
    /// thread and read by one thread, so a torn value is impossible.
    peak_bits: AtomicU32,
    rms_bits: AtomicU32,
    frames: AtomicU64,
    overruns: AtomicU64,
    discontinuities: AtomicU64,
    error: Mutex<String>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            silent: AtomicBool::new(false),
            monitor: AtomicBool::new(false),
            recording: AtomicBool::new(false),
            device_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            format: AtomicU32::new(FMT_NONE),
            peak_bits: AtomicU32::new(0.0f32.to_bits()),
            rms_bits: AtomicU32::new(0.0f32.to_bits()),
            frames: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            discontinuities: AtomicU64::new(0),
            error: Mutex::new(String::new()),
        }
    }

    fn store_f32(slot: &AtomicU32, value: f32) {
        slot.store(value.to_bits(), Ordering::Relaxed);
    }

    fn load_f32(slot: &AtomicU32) -> f32 {
        f32::from_bits(slot.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone)]
pub struct AudioStatus {
    pub running: bool,
    pub silent: bool,
    pub device_rate: u32,
    pub channels: u32,
    pub format: &'static str,
    pub peak_db: f32,
    pub rms_db: f32,
    pub frames: u64,
    pub overruns: u64,
    pub discontinuities: u64,
    pub error: String,
}

impl AudioStatus {
    /// Placeholder used while no stream exists, so the interface always has a
    /// value to render.
    pub fn idle() -> AudioStatus {
        AudioStatus {
            running: false,
            silent: false,
            device_rate: 0,
            channels: 0,
            format: "none",
            peak_db: -180.0,
            rms_db: -180.0,
            frames: 0,
            overruns: 0,
            discontinuities: 0,
            error: String::new(),
        }
    }
}

// ---------------------------------------------------------------- pipeline

/// Conditioning chain driven by the backend. Lives on the capture thread and is
/// never touched from anywhere else.
///
/// The chain deliberately has no automatic gain control. A gain loop fast enough
/// to be useful for listening operates on the same time scale as the keying it
/// is supposed to pass through: at twenty words per minute a dash lasts a
/// hundred and eighty milliseconds and a gap sixty, which is exactly the range
/// of any sane attack and release pair. The loop then flattens the very envelope
/// the keying detector measures, and in the gaps it raises the gain until the
/// noise reaches the target level, which the detector cannot distinguish from a
/// signal.
///
/// Both channels are carried to the end. A receiver fed quadrature needs them
/// apart, and which arrangement is in force is a receiver setting the capture
/// thread does not know. Reducing here would settle that question in the wrong
/// place and would cost a stream restart to change.
pub struct Pipeline {
    producer: Producer<[f32; 2]>,
    /// Second queue feeding the headphone path.
    tap: Producer<[f32; 2]>,
    /// Third queue feeding the recorder.
    ///
    /// Separate from the monitor tap because the two carry opposite policies: a
    /// block the monitor cannot take is dropped, a block the recorder cannot
    /// take is a fault worth counting.
    record: Producer<[f32; 2]>,
    shared: Arc<Shared>,
    cfg: CaptureConfig,
    converter: Option<Converter>,
    frontend: Option<Frontend>,
    /// One resampler per channel. Two instances rather than one pair aware
    /// filter, because they are driven by identical sample counts and are
    /// deterministic, so their phase cannot diverge; a single filter would have
    /// to interleave two histories for no benefit.
    resample_i: Option<Resampler>,
    resample_q: Option<Resampler>,
    /// Scratch buffers, reused so the chain does not allocate per block.
    pairs: Vec<[f32; 2]>,
    lane_i: Vec<f32>,
    lane_q: Vec<f32>,
    out_i: Vec<f32>,
    out_q: Vec<f32>,
    out: Vec<[f32; 2]>,
}

impl Pipeline {
    fn new(
        producer: Producer<[f32; 2]>,
        tap: Producer<[f32; 2]>,
        record: Producer<[f32; 2]>,
        shared: Arc<Shared>,
        cfg: CaptureConfig,
    ) -> Pipeline {
        Pipeline {
            producer,
            tap,
            record,
            shared,
            cfg,
            converter: None,
            frontend: None,
            resample_i: None,
            resample_q: None,
            pairs: Vec::with_capacity(8192),
            lane_i: Vec::with_capacity(8192),
            lane_q: Vec::with_capacity(8192),
            out_i: Vec::with_capacity(8192),
            out_q: Vec::with_capacity(8192),
            out: Vec::with_capacity(8192),
        }
    }

    /// Called once by the backend after the device format is known. Everything
    /// that depends on the rate or the layout is built here.
    pub fn configure(
        &mut self,
        rate: u32,
        channels: usize,
        format: SampleFormat,
        frame_bytes: usize,
    ) {
        let target = if self.cfg.target_rate == 0 { rate } else { self.cfg.target_rate };

        self.converter = Some(Converter::new(format, channels, frame_bytes, self.cfg.channel_mode));
        self.frontend = Some(Frontend::new(rate, self.cfg.dc_block, self.cfg.input_gain_db));
        self.resample_i = Some(Resampler::new(rate, target));
        self.resample_q = Some(Resampler::new(rate, target));

        self.shared.device_rate.store(rate, Ordering::Relaxed);
        self.shared.channels.store(channels as u32, Ordering::Relaxed);
        self.shared.format.store(format_code(format), Ordering::Relaxed);
        self.shared.running.store(true, Ordering::Release);

        crate::log_info!(
            "audio",
            "pipeline ready, {} Hz -> {} Hz, {} channels, {}",
            rate,
            target,
            channels,
            format.as_str()
        );
    }

    /// Feeds one captured block of interleaved device frames.
    pub fn push(&mut self, bytes: &[u8], frames: usize) {
        // The scratch buffer is moved out so the converter, which is borrowed
        // from the same structure, can write into it.
        let mut pairs = std::mem::take(&mut self.pairs);
        pairs.clear();
        if let Some(c) = self.converter.as_ref() {
            c.to_pairs(bytes, frames, &mut pairs);
        }
        self.pairs = pairs;
        self.run_chain();
    }

    /// Fills a gap the driver reported as silence. The samples still go through
    /// the chain so the resampler phase and the decoder sample clock stay
    /// aligned with real time.
    pub fn push_silence(&mut self, frames: usize) {
        self.pairs.clear();
        self.pairs.resize(frames, [0.0, 0.0]);
        self.run_chain();
    }

    pub fn note_discontinuity(&mut self) {
        self.shared.discontinuities.fetch_add(1, Ordering::Relaxed);
    }

    /// Reports whether the stream is currently carrying anything. A loopback
    /// endpoint stops delivering packets entirely when no application plays to
    /// it, which is indistinguishable from a broken device unless it is said out
    /// loud.
    pub fn note_silence(&mut self, silent: bool) {
        self.shared.silent.store(silent, Ordering::Relaxed);
    }

    fn run_chain(&mut self) {
        if self.pairs.is_empty() {
            return;
        }
        let mut pairs = std::mem::take(&mut self.pairs);

        if let Some(f) = self.frontend.as_mut() {
            f.process(&mut pairs, self.cfg.channel_mode);
            Shared::store_f32(&self.shared.peak_bits, linear_to_db(f.meter.peak()));
            Shared::store_f32(&self.shared.rms_bits, linear_to_db(f.meter.rms()));
        }

        // Split, convert, recombine. The two resamplers consume equal counts and
        // produce equal counts, so the pair is never torn; asserting that here
        // rather than trusting it would cost a branch per block for a property
        // the construction already guarantees.
        let mut lane_i = std::mem::take(&mut self.lane_i);
        let mut lane_q = std::mem::take(&mut self.lane_q);
        lane_i.clear();
        lane_q.clear();
        for p in &pairs {
            lane_i.push(p[0]);
            lane_q.push(p[1]);
        }

        let mut out_i = std::mem::take(&mut self.out_i);
        let mut out_q = std::mem::take(&mut self.out_q);
        out_i.clear();
        out_q.clear();
        match (self.resample_i.as_mut(), self.resample_q.as_mut()) {
            (Some(ri), Some(rq)) => {
                ri.process(&lane_i, &mut out_i);
                rq.process(&lane_q, &mut out_q);
            }
            _ => {
                out_i.extend_from_slice(&lane_i);
                out_q.extend_from_slice(&lane_q);
            }
        }

        let mut out = std::mem::take(&mut self.out);
        out.clear();
        let n = out_i.len().min(out_q.len());
        for k in 0..n {
            out.push([out_i[k], out_q[k]]);
        }

        if !out.is_empty() {
            // A full queue means the interface stalled. Dropping the block is
            // the correct behaviour for a receiver: the audio path must not wait
            // for the display.
            if self.producer.write(&out) {
                self.shared.frames.fetch_add(out.len() as u64, Ordering::Relaxed);
            } else {
                self.shared.overruns.fetch_add(1, Ordering::Relaxed);
            }

            // The monitor tap is best effort by construction. Its queue is
            // short, because latency there is the whole point, and a block that
            // does not fit is a block the listener was going to hear late anyway.
            if self.shared.monitor.load(Ordering::Relaxed) {
                let _ = self.tap.write(&out);
            }

            // The recorder is not best effort. A block that does not fit is a
            // gap in the file, so it is counted rather than ignored: the
            // operator has to be able to tell a recording with holes from one
            // without.
            if self.shared.recording.load(Ordering::Relaxed) && !self.record.write(&out) {
                self.shared.overruns.fetch_add(1, Ordering::Relaxed);
            }
            
        }

        self.out = out;
        self.out_i = out_i;
        self.out_q = out_q;
        self.lane_i = lane_i;
        self.lane_q = lane_q;
        self.pairs = pairs;
    }
}

// ------------------------------------------------------------------ stream

pub struct CaptureStream {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    consumer: Consumer<[f32; 2]>,
    /// Reading end of the monitor tap.
    ///
    /// Held rather than handed over, so a monitor that stopped can be started
    /// again. A queue has one reader and that rule is kept by the owner allowing
    /// one listener at a time, not by making the handle unrepeatable: the second
    /// is a mistake that presents as silence with no way back.
    tap: Arc<Consumer<[f32; 2]>>,
    /// Reading end of the recorder tap.
    record: Arc<Consumer<[f32; 2]>>,
    shared: Arc<Shared>,
    /// Rate of the samples in the queue, after conversion.
    rate: u32,
}

impl CaptureStream {
    pub fn start(cfg: CaptureConfig) -> Result<CaptureStream> {
        // The queue holds converted pairs, so its capacity follows the decoder
        // rate rather than the device rate.
        let rate = if cfg.target_rate == 0 { 48_000 } else { cfg.target_rate };
        let capacity = ((rate as f32 * cfg.ring_seconds.max(0.25)) as usize).max(4096);
        let (producer, consumer) = ring::channel::<[f32; 2]>(capacity);

        let tap_capacity = ((rate as f32 * MONITOR_TAP_SECONDS) as usize).max(1024);
        let (tap_producer, tap_consumer) = ring::channel::<[f32; 2]>(tap_capacity);

        let record_capacity = ((rate as f32 * RECORD_TAP_SECONDS) as usize).max(8192);
        let (record_producer, record_consumer) = ring::channel::<[f32; 2]>(record_capacity);

        let shared = Arc::new(Shared::new());
        let stop = Arc::new(AtomicBool::new(false));

        let mut pipeline = Pipeline::new(
            producer,
            tap_producer,
            record_producer,
            shared.clone(),
            cfg.clone(),
        );
        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread_cfg = cfg.clone();

        let handle = std::thread::Builder::new()
            .name("rxscope-audio".to_string())
            .spawn(move || {
                crate::platform::win32::raise_thread_priority();
                let result = run_backend(&thread_cfg, &mut pipeline, &thread_stop);
                thread_shared.running.store(false, Ordering::Release);
                if let Err(e) = result {
                    crate::log_error!("audio", "capture ended: {}", e);
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = e.to_string();
                    }
                }
            })
            .map_err(|e| Error::audio(format!("cannot start the capture thread: {}", e)))?;

        // The identifier carries the direction, so it is decoded here as well:
        // a log line that only shows the endpoint string tells nothing about
        // whether the stream is an input or a loopback.
        let reference = device::DeviceRef::parse(&cfg.device_id, cfg.backend);
        crate::log_info!(
            "audio",
            "stream requested: {:?} {:?}, device '{}', {} Hz, queue {} frames, tap {} frames",
            cfg.backend,
            reference.kind,
            if reference.is_default() { "default" } else { reference.raw.as_str() },
            rate,
            capacity,
            tap_capacity
        );

        Ok(CaptureStream {
            stop,
            handle: Some(handle),
            consumer,
            tap: Arc::new(tap_consumer),
            record: Arc::new(record_consumer),
            shared,
            rate,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
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

    /// Drops frames without copying, used to bound the latency after a stall.
    pub fn skip(&self, count: usize) -> usize {
        self.consumer.skip(count)
    }

    /// Hands a listener a reference to the monitor tap.
    ///
    /// Shared rather than moved. The reading end stays with the stream, so
    /// stopping the monitor and starting it again costs nothing; the caller is
    /// responsible for running one listener at a time, which it already is by
    /// holding one monitor.
    pub fn tap(&self) -> Arc<Consumer<[f32; 2]>> {
        self.tap.clone()
    }

    /// Tells the capture thread whether the tap is being drained.
    pub fn set_monitor(&self, active: bool) {
        self.shared.monitor.store(active, Ordering::Relaxed);
    }
    
    /// Hands a recorder a reference to its own tap.
    pub fn record_tap(&self) -> Arc<Consumer<[f32; 2]>> {
        self.record.clone()
    }

    pub fn set_recording(&self, active: bool) {
        self.shared.recording.store(active, Ordering::Relaxed);
    }

    /// Channels the device delivers.
    ///
    /// One means the two slots of a frame carry the same value, so a quadrature
    /// pair is impossible however the receiver is configured.
    pub fn device_channels(&self) -> u32 {
        self.shared.channels.load(Ordering::Relaxed)
    }

    pub fn status(&self) -> AudioStatus {
        let error = self
            .shared
            .error
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        AudioStatus {
            running: self.shared.running.load(Ordering::Acquire),
            silent: self.shared.silent.load(Ordering::Relaxed),
            device_rate: self.shared.device_rate.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            format: format_name(self.shared.format.load(Ordering::Relaxed)),
            peak_db: Shared::load_f32(&self.shared.peak_bits),
            rms_db: Shared::load_f32(&self.shared.rms_bits),
            frames: self.shared.frames.load(Ordering::Relaxed),
            overruns: self.shared.overruns.load(Ordering::Relaxed),
            discontinuities: self.shared.discontinuities.load(Ordering::Relaxed),
            error,
        }
    }

    /// Signals the loop and waits for it. The backend polls the flag at a
    /// quarter of the device period, so the wait is short.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.stop();
    }
}