//! Headphone monitor.
//!
//! A second path out of the capture chain, taken at the decoder rate so the
//! operator hears exactly the samples the detectors are working on. It has no
//! influence over decoding whatsoever, and that separation is the point: the
//! processing an ear wants is precisely the processing a keying detector must
//! not have.
//!
//! The two paths are joined only by a lock free queue. The capture thread
//! writes and never waits; the monitor thread reads and fills any shortfall
//! with silence. A monitor that stalls therefore costs a gap in the headphones
//! and nothing else, which is the correct ranking: the recording is the
//! product, the listening is a convenience.
//!
//! ## Two arrangements, chosen by the operating mode
//!
//! In the skimmer mode the monitor is a receiver in miniature. It is not a
//! plain bandpass: a bandpass leaves a channel at whatever audio frequency it
//! happens to occupy, so a station three kilohertz from the dial is heard at
//! three kilohertz, which is a whistle rather than a signal. The chain here
//! forms a complex signal, filters the chosen channel to baseband with a
//! complex bandpass, and puts what survived back at a fixed pitch. The channel
//! then sounds the same wherever it sits in the span, and following the tracker
//! moves the mixer rather than the pitch.
//!
//! Where the complex signal comes from depends on the input. A quadrature pair
//! is already one, and using it directly is what makes the half below the tuning
//! point reachable: reducing the pair first would fold the spectrum about
//! nought, so a station ten kilohertz below the dial would be heard as the one
//! ten kilohertz above it, superimposed. A single channel carries no such
//! distinction, so a Hilbert transform supplies the quadrature part and the
//! result covers the positive half alone, which is all there was.
//!
//! In the receiver mode the receiver chain is the listening path. Its filter,
//! its detector and its gain loop replace everything here, so the controls that
//! belong to the narrow filter are dead: leaving them live would let a filter
//! follow the keying tracker while the operator works a voice signal, which
//! reproduces the tracker in the headphones as a tone sweeping up the band.
//!
//! ## Why the chain lives on this thread
//!
//! It has exactly one consumer. Running it on the interface thread would mean
//! a queue between it and the only thing that uses it, plus a frame of latency
//! on a path an operator is listening through while turning a dial. The
//! settings reach it through a slot carrying a version, read once per block
//! rather than per sample.
//!
//! Chain, in order:
//!   queue at the decoder rate
//!   receiver chain, or translation to a fixed pitch and gain control
//!   rate conversion to the device rate, with drift correction
//!   operator volume
//!   interleave to the device channel count
//!
//! Clock drift between the capture device and the render device is corrected by
//! trimming the conversion ratio rather than by resynchronizing. The two are
//! independent crystals, a hundred parts per million apart is ordinary, and a
//! resynchronization is an audible discontinuity every few minutes. A trim of
//! the same magnitude is inaudible and removes the accumulation entirely.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::audio::convert::{Converter, SampleFormat};
use crate::audio::process::{db_to_linear, Agc, AgcConfig};
use crate::audio::resample::Resampler;
use crate::config::settings::{ChannelMode, Detector, ReceiverSettings};
use crate::core::ring::Consumer;
use crate::core::{Error, Result};
use crate::dsp::receiver::detector::DetectorBank;
use crate::dsp::receiver::filter::Bandpass;
use crate::dsp::receiver::iq::IqFront;
use crate::dsp::receiver::{ReceiverChain, ReceiverStatus};
use crate::platform::win32::com::*;
use crate::platform::win32::{sleep_ms, wide};

/// Nominal fill of the tap queue, as a fraction of its capacity.
///
/// The drift loop holds the queue here. Halfway is the only choice that leaves
/// equal room in both directions, which matters because the sign of the drift
/// is not known in advance and may differ between two runs on the same
/// hardware.
const TARGET_FILL: f32 = 0.5;

/// Correction applied per pass, in parts per million per unit of fill error.
///
/// Deliberately weak. The queue absorbs short term jitter on its own, and a
/// loop fast enough to chase that jitter would modulate the pitch at the jitter
/// rate, which is audible where a slow constant offset is not.
const DRIFT_GAIN: f32 = 400.0;

/// Ceiling on the accumulated correction.
const DRIFT_LIMIT: f32 = 500.0;

/// Bounds on the pitch the skimmer monitor brings a channel down to.
///
/// Below two hundred hertz a small headphone reproduces nothing, and above
/// fifteen hundred a long session is tiring. The same range the beat oscillator
/// of the receiver chain offers, so an operator who moves between the two modes
/// is choosing from one set of values.
const PITCH_MIN_HZ: f32 = 200.0;
const PITCH_MAX_HZ: f32 = 1500.0;

/// Poll interval as a fraction of the device buffer.
const POLL_DIVISOR: u32 = 4;

// ------------------------------------------------------- receiver settings

/// Settings of the receiver chain, handed across the thread boundary.
///
/// A lock rather than a set of atomics, because the chain reads them as a group
/// and a torn read would mean a filter planned from one edge of the old pair
/// and one of the new. The lock is taken once per block and only when the
/// version says something changed, so it never appears on the sample path.
pub struct SharedReceiver {
    settings: Mutex<ReceiverSettings>,
    /// Raised on every write. The reader compares rather than locking, so an
    /// unchanged configuration costs one atomic load per block.
    version: AtomicU64,
    /// True while the receiver mode is in force.
    active: AtomicBool,
}

impl SharedReceiver {
    pub fn new(settings: &ReceiverSettings, active: bool) -> Arc<SharedReceiver> {
        Arc::new(SharedReceiver {
            settings: Mutex::new(settings.clone()),
            version: AtomicU64::new(1),
            active: AtomicBool::new(active),
        })
    }

    /// Publishes a new configuration, if it differs from the last one.
    pub fn publish(&self, settings: &ReceiverSettings, active: bool) {
        self.active.store(active, Ordering::Release);
        let mut held = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        // Compared field by field through the derived equality rather than by a
        // fingerprint: the structure is a few dozen bytes and a fingerprint
        // would be one more thing to keep in step with the fields.
        if *held != *settings {
            *held = settings.clone();
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    fn read(&self) -> ReceiverSettings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

// ------------------------------------------------------------------ shared

/// State the interface thread writes and the monitor thread reads.
struct Shared {
    running: AtomicBool,
    volume_bits: AtomicU32,
    centre_bits: AtomicU32,
    bandwidth_bits: AtomicU32,
    /// Pitch the skimmer path puts the chosen band at.
    pitch_bits: AtomicU32,
    filter: AtomicBool,
    agc: AtomicBool,
    /// True when the tap carries a quadrature pair.
    ///
    /// Held apart from the receiver settings, because it is a property of the
    /// wiring and of the device rather than of the receiver: a mono endpoint
    /// duplicates its one channel whatever the setting says.
    source_complex: AtomicBool,
    /// Channel reduction applied when the pair is not a quadrature one.
    channel_mode: AtomicU32,
    device_rate: AtomicU32,
    channels: AtomicU32,
    /// Blocks the queue could not supply, filled with silence instead.
    underruns: AtomicU64,
    /// Accumulated clock correction, in parts per million.
    drift_bits: AtomicU32,
    fill_bits: AtomicU32,
    /// Receiver chain readings, published from the monitor thread.
    rx_gain_bits: AtomicU32,
    rx_offset_bits: AtomicU32,
    rx_open: AtomicBool,
    rx_locked: AtomicBool,
    rx_blanked: AtomicU64,
    error: Mutex<String>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            volume_bits: AtomicU32::new(0.5f32.to_bits()),
            centre_bits: AtomicU32::new(700.0f32.to_bits()),
            bandwidth_bits: AtomicU32::new(300.0f32.to_bits()),
            pitch_bits: AtomicU32::new(700.0f32.to_bits()),
            filter: AtomicBool::new(true),
            agc: AtomicBool::new(true),
            source_complex: AtomicBool::new(false),
            channel_mode: AtomicU32::new(0),
            device_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            underruns: AtomicU64::new(0),
            drift_bits: AtomicU32::new(0.0f32.to_bits()),
            fill_bits: AtomicU32::new(0.0f32.to_bits()),
            rx_gain_bits: AtomicU32::new(0.0f32.to_bits()),
            rx_offset_bits: AtomicU32::new(0.0f32.to_bits()),
            rx_open: AtomicBool::new(true),
            rx_locked: AtomicBool::new(false),
            rx_blanked: AtomicU64::new(0),
            error: Mutex::new(String::new()),
        }
    }

    fn store(slot: &AtomicU32, value: f32) {
        slot.store(value.to_bits(), Ordering::Relaxed);
    }

    fn load(slot: &AtomicU32) -> f32 {
        f32::from_bits(slot.load(Ordering::Relaxed))
    }
}

#[derive(Debug, Clone)]
pub struct MonitorStatus {
    pub running: bool,
    pub device_rate: u32,
    pub channels: u32,
    pub underruns: u64,
    /// Clock correction currently applied, in parts per million.
    pub drift_ppm: f32,
    /// Fraction of the tap queue in use.
    pub fill: f32,
    /// True while the receiver chain is the path.
    pub receiver: bool,
    pub rx_gain_db: f32,
    pub rx_open: bool,
    pub rx_locked: bool,
    pub rx_offset_hz: f32,
    pub rx_blanked: u64,
    pub error: String,
}

impl MonitorStatus {
    pub fn idle() -> MonitorStatus {
        MonitorStatus {
            running: false,
            device_rate: 0,
            channels: 0,
            underruns: 0,
            drift_ppm: 0.0,
            fill: 0.0,
            receiver: false,
            rx_gain_db: 0.0,
            rx_open: true,
            rx_locked: false,
            rx_offset_hz: 0.0,
            rx_blanked: 0,
            error: String::new(),
        }
    }
}

/// Everything the monitor needs that cannot change while it runs.
#[derive(Clone)]
pub struct MonitorConfig {
    /// Render endpoint, empty for the system default.
    pub device_id: String,
    /// Rate of the samples in the tap queue.
    pub source_rate: u32,
    pub period_ms: u32,
    pub agc: AgcConfig,
    /// Settings of the receiver chain, and whether it is the path.
    pub receiver: Arc<SharedReceiver>,
}

// ------------------------------------------------------------------ stream

pub struct MonitorStream {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    receiver: Arc<SharedReceiver>,
}

impl MonitorStream {
    /// Takes the reading end of the capture tap and starts playing it.
    pub fn start(cfg: MonitorConfig, source: Arc<Consumer<[f32; 2]>>) -> Result<MonitorStream> {
        let shared = Arc::new(Shared::new());
        let stop = Arc::new(AtomicBool::new(false));
        let receiver = cfg.receiver.clone();

        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread_cfg = cfg.clone();

        let handle = std::thread::Builder::new()
            .name("rxscope-monitor".to_string())
            .spawn(move || {
                crate::platform::win32::raise_thread_priority();
                let result = run(&thread_cfg, source, &thread_shared, &thread_stop);
                thread_shared.running.store(false, Ordering::Release);
                if let Err(e) = result {
                    crate::log_error!("monitor", "ended: {}", e);
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = e.to_string();
                    }
                }
            })
            .map_err(|e| Error::audio(format!("cannot start the monitor thread: {}", e)))?;

        crate::log_info!(
            "monitor",
            "requested on '{}', source {} Hz",
            if cfg.device_id.is_empty() { "default" } else { cfg.device_id.as_str() },
            cfg.source_rate
        );

        Ok(MonitorStream { stop, handle: Some(handle), shared, receiver })
    }

    pub fn set_volume(&self, value: f32) {
        Shared::store(&self.shared.volume_bits, value.clamp(0.0, 1.0));
    }

    /// Centre and width of the listening filter, in hertz. Skimmer arrangement
    /// only; the receiver chain takes its edges from the shared settings.
    ///
    /// The centre may be negative, which on a quadrature input is a real
    /// position below the tuning point rather than a mirror of one above it.
    pub fn set_passband(&self, centre_hz: f32, bandwidth_hz: f32) {
        Shared::store(&self.shared.centre_bits, centre_hz);
        Shared::store(&self.shared.bandwidth_bits, bandwidth_hz);
    }

    /// Pitch the skimmer path brings the chosen band down to.
    pub fn set_pitch(&self, hz: f32) {
        Shared::store(&self.shared.pitch_bits, hz.clamp(PITCH_MIN_HZ, PITCH_MAX_HZ));
    }

    /// States whether the tap carries a quadrature pair.
    pub fn set_complex(&self, complex: bool) {
        self.shared.source_complex.store(complex, Ordering::Relaxed);
    }

    pub fn set_filter(&self, on: bool) {
        self.shared.filter.store(on, Ordering::Relaxed);
    }

    pub fn set_agc(&self, on: bool) {
        self.shared.agc.store(on, Ordering::Relaxed);
    }

    /// Sets how the pair is reduced when it is not a quadrature one.
    pub fn set_channel_mode(&self, mode: ChannelMode) {
        let code = match mode {
            ChannelMode::Left => 0u32,
            ChannelMode::Right => 1,
            ChannelMode::Mix => 2,
            ChannelMode::Difference => 3,
        };
        self.shared.channel_mode.store(code, Ordering::Relaxed);
    }

    /// Publishes the receiver configuration and whether it is the path.
    pub fn set_receiver(&self, settings: &ReceiverSettings, active: bool) {
        self.receiver.publish(settings, active);
    }

    pub fn status(&self) -> MonitorStatus {
        let error = self
            .shared
            .error
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        MonitorStatus {
            running: self.shared.running.load(Ordering::Acquire),
            device_rate: self.shared.device_rate.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            underruns: self.shared.underruns.load(Ordering::Relaxed),
            drift_ppm: Shared::load(&self.shared.drift_bits),
            fill: Shared::load(&self.shared.fill_bits),
            receiver: self.receiver.is_active(),
            rx_gain_db: Shared::load(&self.shared.rx_gain_bits),
            rx_open: self.shared.rx_open.load(Ordering::Relaxed),
            rx_locked: self.shared.rx_locked.load(Ordering::Relaxed),
            rx_offset_hz: Shared::load(&self.shared.rx_offset_bits),
            rx_blanked: self.shared.rx_blanked.load(Ordering::Relaxed),
            error,
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MonitorStream {
    fn drop(&mut self) {
        self.stop();
    }
}

// -------------------------------------------------------------- render loop

fn hr_error(op: &str, hr: HRESULT) -> Error {
    Error::with_code(
        crate::core::error::Category::Audio,
        format!("{} failed, 0x{:08X}", op, hr as u32),
        hr as i64,
    )
}

fn run(
    cfg: &MonitorConfig,
    source: Arc<Consumer<[f32; 2]>>,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<()> {
    let _apartment = Apartment::new(true);

    let mut enumerator: ComPtr<IMMDeviceEnumerator> = ComPtr::null();
    let hr = unsafe {
        CoCreateInstance(
            &CLSID_MMDeviceEnumerator,
            std::ptr::null_mut(),
            CLSCTX_ALL,
            &IID_IMMDeviceEnumerator,
            enumerator.out() as *mut *mut c_void,
        )
    };
    if hr != S_OK || enumerator.is_null() {
        return Err(hr_error("CoCreateInstance", hr));
    }

    let mut device: ComPtr<IMMDevice> = ComPtr::null();
    unsafe {
        let e = enumerator.as_raw();
        let hr = if cfg.device_id.is_empty() {
            ((*(*e).vtbl).GetDefaultAudioEndpoint)(e, E_RENDER, E_CONSOLE, device.out())
        } else {
            let id = wide(&cfg.device_id);
            ((*(*e).vtbl).GetDevice)(e, id.as_ptr(), device.out())
        };
        if hr != S_OK || device.is_null() {
            return Err(hr_error("render device open", hr));
        }
    }

    let mut client: ComPtr<IAudioClient> = ComPtr::null();
    unsafe {
        let d = device.as_raw();
        let hr = ((*(*d).vtbl).Activate)(
            d,
            &IID_IAudioClient,
            CLSCTX_ALL,
            std::ptr::null_mut(),
            client.out() as *mut *mut c_void,
        );
        if hr != S_OK || client.is_null() {
            return Err(hr_error("IMMDevice::Activate", hr));
        }
    }

    let mut mix = MixFormat(std::ptr::null_mut());
    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).GetMixFormat)(c, &mut mix.0);
        if hr != S_OK || mix.0.is_null() {
            return Err(hr_error("GetMixFormat", hr));
        }
    }
    let (rate, channels, format, frame_bytes) = unsafe { parse_format(mix.0)? };

    // Shared mode only. Exclusive mode would take the endpoint away from every
    // other application, which for a monitor is the wrong trade in every case:
    // the operator is listening, not measuring.
    let duration: REFERENCE_TIME = (cfg.period_ms.max(5) as i64) * 10_000;
    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).Initialize)(
            c,
            AUDCLNT_SHAREMODE_SHARED,
            0,
            duration,
            0,
            mix.0,
            std::ptr::null(),
        );
        if hr != S_OK {
            return Err(hr_error("IAudioClient::Initialize", hr));
        }
    }

    let mut buffer_frames = 0u32;
    unsafe {
        let c = client.as_raw();
        ((*(*c).vtbl).GetBufferSize)(c, &mut buffer_frames);
    }
    if buffer_frames == 0 {
        return Err(Error::audio("render endpoint reported an empty buffer"));
    }

    let mut render: ComPtr<IAudioRenderClient> = ComPtr::null();
    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).GetService)(
            c,
            &IID_IAudioRenderClient,
            render.out() as *mut *mut c_void,
        );
        if hr != S_OK || render.is_null() {
            return Err(hr_error("GetService IAudioRenderClient", hr));
        }
    }

    shared.device_rate.store(rate, Ordering::Relaxed);
    shared.channels.store(channels as u32, Ordering::Relaxed);

    // Skimmer listening path, a receiver in miniature. The complex signal
    // removes the image a real mixer would fold onto the channel, the complex
    // bandpass isolates it at baseband, and the beat oscillator puts it back at
    // a fixed pitch. The three together are what makes a channel sound the same
    // wherever it sits in the span, on either side of the tuning point.
    let mut front = IqFront::new();
    front.configure(false, false, 0.0, 0.0);
    let mut narrow = Bandpass::new(cfg.source_rate);
    let mut beat = DetectorBank::new(cfg.source_rate);

    let mut agc = Agc::new(cfg.agc, cfg.source_rate);
    let mut resampler = Resampler::new(cfg.source_rate, rate);

    // The chain is built on the first block that needs it rather than here, so
    // a monitor started in the skimmer mode does not plan a filter it will
    // never evaluate.
    let mut chain: Option<ReceiverChain> = None;
    let mut chain_version = 0u64;

    // Quadrature correction, refreshed only when the settings move. The front
    // end applies it in both modes, so a session that switches between them
    // hears the same image rejection either way.
    let mut iq_version = 0u64;
    let mut iq_swap = false;
    let mut iq_gain_db = 0.0f32;
    let mut iq_phase_deg = 0.0f32;
    let mut iq_settings = false;
    let mut front_complex = false;

    // Scratch buffers, sized once so the loop never allocates.
    let block = (cfg.source_rate as usize / 20).max(256);
    let mut input = vec![[0.0f32, 0.0f32]; block];
    let mut mono: Vec<f32> = Vec::with_capacity(block);
    let mut chain_out: Vec<f32> = Vec::with_capacity(block);
    let mut converted: Vec<f32> = Vec::with_capacity(block * 4);
    let mut ready: Vec<f32> = Vec::with_capacity(block * 8);

    let capacity = source.capacity().max(1) as f32;
    let mut drift = 0.0f32;
    let mut fill_avg = TARGET_FILL;

    // The buffer is primed with silence so the first real block does not have
    // to arrive within one period of the start.
    unsafe {
        let r = render.as_raw();
        let mut data: *mut u8 = std::ptr::null_mut();
        if ((*(*r).vtbl).GetBuffer)(r, buffer_frames, &mut data) == S_OK {
            ((*(*r).vtbl).ReleaseBuffer)(r, buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT);
        }
    }

    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).Start)(c);
        if hr != S_OK {
            return Err(hr_error("IAudioClient::Start", hr));
        }
    }
    shared.running.store(true, Ordering::Release);

    let poll_ms = (cfg.period_ms / POLL_DIVISOR).max(1);
    crate::log_info!(
        "monitor",
        "started, {} Hz {} channels {}, buffer {} frames, poll {} ms",
        rate,
        channels,
        format.as_str(),
        buffer_frames,
        poll_ms
    );

    let mut result = Ok(());
    while !stop.load(Ordering::Relaxed) {
        let mut padding = 0u32;
        let hr = unsafe {
            let c = client.as_raw();
            ((*(*c).vtbl).GetCurrentPadding)(c, &mut padding)
        };
        if hr != S_OK {
            result = Err(hr_error("GetCurrentPadding", hr));
            break;
        }

        let wanted = buffer_frames.saturating_sub(padding) as usize;
        if wanted == 0 {
            sleep_ms(poll_ms);
            continue;
        }

        // Drift correction. The queue depth is the only observable that reveals
        // the difference between the two device clocks, and it is noisy, so it
        // is smoothed before it reaches the loop.
        let pending = source.len() as f32 / capacity;
        fill_avg += 0.02 * (pending - fill_avg);
        drift = (drift + (fill_avg - TARGET_FILL) * DRIFT_GAIN * 0.01)
            .clamp(-DRIFT_LIMIT, DRIFT_LIMIT);
        resampler.set_drift(drift);
        Shared::store(&shared.drift_bits, drift);
        Shared::store(&shared.fill_bits, pending);

        // The quadrature settings, read once per change rather than per block.
        let version = cfg.receiver.version();
        if version != iq_version || !iq_settings {
            let settings = cfg.receiver.read();
            iq_swap = settings.iq_swap;
            iq_gain_db = settings.iq_gain_db;
            iq_phase_deg = settings.iq_phase_deg;
            iq_version = version;
            iq_settings = true;
            if let Some(c) = chain.as_mut() {
                c.sync_settings(&settings);
                chain_version = version;
            }
        }

        // Which arrangement is in force, decided once per block.
        let receiver_mode = cfg.receiver.is_active();
        let source_complex = shared.source_complex.load(Ordering::Relaxed);
        let mut iq = false;
        if receiver_mode {
            if chain.is_none() {
                let settings = cfg.receiver.read();
                iq = settings.iq_input;
                let mut built = ReceiverChain::new(cfg.source_rate, &settings);
                built.reset();
                chain = Some(built);
                chain_version = version;
                // The skimmer path is cleared on the way out, so switching back
                // does not release whatever was left in its delay lines.
                narrow.reset();
                beat.reset();
                agc.reset();
                crate::log_info!("monitor", "receiver chain engaged");
            } else {
                iq = source_complex;
                if version != chain_version {
                    let settings = cfg.receiver.read();
                    if let Some(c) = chain.as_mut() {
                        c.sync_settings(&settings);
                    }
                    chain_version = version;
                }
            }
        } else if chain.is_some() {
            chain = None;
            crate::log_info!("monitor", "receiver chain released");
        }

        // The front end supplies the quadrature part on a single channel input
        // and applies the correction on a pair. Reconfiguring it is cheap and it
        // clears its own delay line only when the arrangement really changed.
        if source_complex != front_complex {
            front_complex = source_complex;
            crate::log_info!(
                "monitor",
                "listening path is {} sided",
                if source_complex { "two" } else { "one" }
            );
        }
        front.configure(source_complex, iq_swap, iq_gain_db, iq_phase_deg);

        let use_filter = shared.filter.load(Ordering::Relaxed);
        let use_agc = shared.agc.load(Ordering::Relaxed);
        let centre = Shared::load(&shared.centre_bits);
        let bandwidth = Shared::load(&shared.bandwidth_bits);
        let pitch = Shared::load(&shared.pitch_bits).clamp(PITCH_MIN_HZ, PITCH_MAX_HZ);

        if !receiver_mode {
            if use_filter {
                // The band is stated symmetrically about the channel, so the
                // filter mixes exactly that channel to baseband whichever side
                // of the tuning point it sits on, and the only offset the
                // detector has to put back is the pitch.
                narrow.set_band(centre, -bandwidth * 0.5, bandwidth * 0.5);

                // A real output has its own mirror at nought, so the band is
                // never placed across it: below the pitch the lower half would
                // fold onto the upper one. A narrow channel keeps the stated
                // pitch, and a wide one is pushed up until its lower edge
                // reaches nought.
                let offset = pitch.max(bandwidth * 0.5);
                beat.configure(Detector::Cw, narrow.rel_centre_hz() + offset);
            } else {
                // Held clear so switching the filter back on does not release a
                // burst of whatever was left in the delay lines.
                front.reset();
                narrow.reset();
                beat.reset();
            }
        }

        // Produce until the device request is covered. A queue that runs dry
        // contributes silence rather than stalling the loop: the render buffer
        // has to be handed back on time whatever happened upstream.
        let mode = match shared.channel_mode.load(Ordering::Relaxed) {
            1 => ChannelMode::Right,
            2 => ChannelMode::Mix,
            3 => ChannelMode::Difference,
            _ => ChannelMode::Left,
        };

        while ready.len() < wanted {
            let n = source.read(&mut input);
            if n == 0 {
                let missing = wanted - ready.len();
                ready.resize(ready.len() + missing, 0.0);
                shared.underruns.fetch_add(1, Ordering::Relaxed);
                // The state is cleared as well: resuming with a gain raised
                // against silence would deliver the first real block at full
                // amplification.
                front.reset();
                narrow.reset();
                beat.reset();
                agc.reset();
                if let Some(c) = chain.as_mut() {
                    c.reset();
                }
                break;
            }
            let frames = &input[..n];

            match chain.as_mut() {
                Some(c) => {
                    // The chain is handed the pair as it is when the input is a
                    // quadrature one, and the reduction with a silent second
                    // slot otherwise. The front end reads the two cases apart by
                    // its own configuration, so nothing here has to.
                    if iq {
                        c.process(frames, &mut chain_out);
                    } else {
                        mono.clear();
                        for &pair in frames {
                            mono.push(Converter::reduce(mode, pair));
                        }
                        // Rebuilt as pairs with a silent quadrature slot, which
                        // is what a real input is.
                        let paired: Vec<[f32; 2]> =
                            mono.iter().map(|&v| [v, 0.0]).collect();
                        c.process(&paired, &mut chain_out);
                    }
                    let s: ReceiverStatus = c.status();
                    Shared::store(&shared.rx_gain_bits, s.gain_db);
                    Shared::store(&shared.rx_offset_bits, s.carrier_offset_hz);
                    shared.rx_open.store(s.open, Ordering::Relaxed);
                    shared.rx_locked.store(s.locked, Ordering::Relaxed);
                    shared
                        .rx_blanked
                        .store(s.wide_events + s.narrow_events, Ordering::Relaxed);

                    converted.clear();
                    resampler.process(&chain_out, &mut converted);
                }
                None => {
                    mono.clear();
                    for &pair in frames {
                        if !use_filter {
                            mono.push(Converter::reduce(mode, pair));
                            continue;
                        }
                        // The pair when it carries one, the reduction otherwise.
                        // Reducing a quadrature pair first would fold the
                        // spectrum about nought and the half below the tuning
                        // point would be heard as its mirror above it.
                        let z = if source_complex {
                            front.sample(pair[0], pair[1])
                        } else {
                            front.sample(Converter::reduce(mode, pair), 0.0)
                        };
                        mono.push(beat.sample(narrow.sample(z)));
                    }
                    if use_agc {
                        agc.process(&mut mono);
                    }
                    converted.clear();
                    resampler.process(&mono, &mut converted);
                }
            }

            ready.extend_from_slice(&converted);
        }

        let frames = wanted.min(ready.len());
        let mut data: *mut u8 = std::ptr::null_mut();
        let hr = unsafe {
            let r = render.as_raw();
            ((*(*r).vtbl).GetBuffer)(r, frames as u32, &mut data)
        };
        if hr != S_OK || data.is_null() {
            result = Err(hr_error("GetBuffer", hr));
            break;
        }

        let volume = Shared::load(&shared.volume_bits);
        unsafe {
            let bytes = std::slice::from_raw_parts_mut(data, frames * frame_bytes);
            write_frames(bytes, &ready[..frames], channels, format, frame_bytes, volume);
        }
        ready.drain(..frames);

        let hr = unsafe {
            let r = render.as_raw();
            ((*(*r).vtbl).ReleaseBuffer)(r, frames as u32, 0)
        };
        if hr != S_OK {
            result = Err(hr_error("ReleaseBuffer", hr));
            break;
        }

        sleep_ms(poll_ms);
    }

    unsafe {
        let c = client.as_raw();
        ((*(*c).vtbl).Stop)(c);
    }
    crate::log_info!("monitor", "stopped");
    result
}

/// Writes mono samples into an interleaved device buffer.
unsafe fn write_frames(
    dst: &mut [u8],
    src: &[f32],
    channels: usize,
    format: SampleFormat,
    frame_bytes: usize,
    volume: f32,
) {
    let sample_bytes = format.bytes();
    for (i, &value) in src.iter().enumerate() {
        // Clipped rather than wrapped: an overload has to sound like an
        // overload and not like noise.
        let v = (value * volume).clamp(-1.0, 1.0);
        let base = i * frame_bytes;
        for c in 0..channels {
            let at = base + c * sample_bytes;
            if at + sample_bytes > dst.len() {
                return;
            }
            match format {
                SampleFormat::F32 => {
                    dst[at..at + 4].copy_from_slice(&v.to_le_bytes());
                }
                SampleFormat::I16 => {
                    let s = (v * 32767.0) as i16;
                    dst[at..at + 2].copy_from_slice(&s.to_le_bytes());
                }
                SampleFormat::I32 => {
                    let s = (v as f64 * 2_147_483_647.0) as i32;
                    dst[at..at + 4].copy_from_slice(&s.to_le_bytes());
                }
                SampleFormat::I24 => {
                    let s = (v as f64 * 8_388_607.0) as i32;
                    let b = s.to_le_bytes();
                    dst[at] = b[0];
                    dst[at + 1] = b[1];
                    dst[at + 2] = b[2];
                }
                SampleFormat::U8 => {
                    dst[at] = ((v * 127.0) + 128.0) as u8;
                }
            }
        }
    }
}

/// Extracts the sample layout of a render endpoint.
unsafe fn parse_format(wf: *const WAVEFORMATEX) -> Result<(u32, usize, SampleFormat, usize)> {
    let base = *wf;
    let rate = base.nSamplesPerSec;
    let channels = base.nChannels as usize;
    let bits = base.wBitsPerSample;
    let frame_bytes = base.nBlockAlign as usize;
    let extension_bytes = base.cbSize;

    if rate == 0 || channels == 0 || frame_bytes == 0 {
        return Err(Error::audio("render endpoint reported an empty format"));
    }

    let mut tag = base.wFormatTag;
    if tag == WAVE_FORMAT_EXTENSIBLE {
        if extension_bytes < 22 {
            return Err(Error::audio("extensible render format block is truncated"));
        }
        let ext = wf as *const WAVEFORMATEXTENSIBLE;
        let sub = (*ext).SubFormat;
        tag = if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            WAVE_FORMAT_PCM
        } else {
            return Err(Error::audio("unsupported render subformat"));
        };
    }

    let format = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleFormat::F32,
        (WAVE_FORMAT_PCM, 8) => SampleFormat::U8,
        (WAVE_FORMAT_PCM, 16) => SampleFormat::I16,
        (WAVE_FORMAT_PCM, 24) => SampleFormat::I24,
        (WAVE_FORMAT_PCM, 32) => SampleFormat::I32,
        _ => {
            return Err(Error::audio(format!(
                "unsupported render format, tag {} with {} bits",
                tag, bits
            )))
        }
    };

    Ok((rate, channels, format, frame_bytes))
}

/// Linear gain from a volume control position.
///
/// The position is squared rather than used directly. A linear position sounds
/// as though almost all of the change happens in the bottom quarter of the
/// travel, because loudness follows roughly the square of amplitude; squaring
/// spreads the useful range across the whole control.
pub fn volume_gain(position: f32) -> f32 {
    let p = position.clamp(0.0, 1.0);
    p * p
}

/// Present so the decibel helper stays reachable from the monitor path, which
/// is where a gain expressed in decibels is meaningful.
pub fn gain_from_db(db: f32) -> f32 {
    db_to_linear(db)
}