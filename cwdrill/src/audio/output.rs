//! WASAPI render stream.
//!
//! ## Why one thread and an event
//!
//! The endpoint signals an event when it wants another period, so the thread
//! spends its life blocked and wakes exactly as often as the buffer needs.
//! Polling would either wake too often, which costs a core, or too rarely, which
//! costs a gap in the tone; and a gap in a tone is not a dropped frame, it is a
//! click in the middle of a character.
//!
//! ## Why shared mode only
//!
//! Exclusive mode buys bit perfect output and lower latency. Neither matters
//! here: the signal is a sine this application generated, so there is nothing to
//! preserve, and nothing responds to the operator through the audio path so
//! there is no latency to reduce. What it costs is the alignment retry the
//! specification requires, and locking the endpoint away from every other
//! application on the machine, which for a trainer is the wrong default and a
//! poor option.
//!
//! ## Why the format is taken rather than requested
//!
//! In shared mode the mixer hands back its own format whatever is asked for, so
//! a request is a setting that cannot fail and cannot take effect. The
//! generator produces samples at whatever rate the endpoint states, so there is
//! nothing to resample; the rate is reported rather than chosen.
//!
//! ## Recovery
//!
//! An endpoint that goes away is the ordinary case rather than a fault: a pair
//! of headphones is unplugged, a dock is disconnected, a machine wakes from
//! sleep. The stream reports the reason and the shell reopens it on a growing
//! delay, so a trainer that was interrupted comes back on its own.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::config::settings::{
    AudioSettings, ConditionsSettings, PaddleSettings, TimingSettings, ToneSettings,
};
use crate::core::ring::{self, Consumer, Producer};
use crate::core::{Error, Result};
use crate::platform::win32::com::*;
use crate::platform::win32::ffi::{
    CloseHandle, CreateEventW, WaitForSingleObject, HANDLE, INFINITE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use crate::synth::{
    Edge, Element, Format, Generator, Keyed, Live, Paddle, SampleKind, SCOPE_RATE,
};

/// Elements the queue holds.
///
/// A group of five characters at the widest spacing is well under a hundred
/// elements, so this holds a group and the one behind it. Deeper would delay a
/// timing change by the depth, because an element already queued keeps the
/// speed it was made at.
const ELEMENT_CAPACITY: usize = 256;

/// Envelope entries the picture queue holds.
///
/// Drained once per frame, which at sixty frames a second is seventeen entries
/// at the scope rate. Two thousand covers a stall of two seconds, past which the
/// picture has lost the moment anyway.
const SCOPE_CAPACITY: usize = 2048;

/// Element boundaries the queue holds.
const EDGE_CAPACITY: usize = 512;

/// Classifications the paddle queue holds.
///
/// A character is a handful of entries and arrives a few times a second, so this
/// covers a stall of a minute. Deeper would be storage for a case in which the
/// interface has stopped drawing altogether.
const KEYED_CAPACITY: usize = 256;

/// Longest wait for the endpoint, as a multiple of the buffer.
///
/// An endpoint that stops signalling is either gone or wedged, and both are
/// reported the same way: the next call into it fails and the reason reaches the
/// operator. Three periods and a margin is long enough that a scheduling delay
/// is not mistaken for either.
const WAIT_PERIODS: u32 = 3;

struct Shared {
    running: AtomicBool,
    /// Requests the generator discard everything queued.
    ///
    /// A counter rather than a flag, so two stops in one buffer period are two
    /// requests rather than one: a flag set and cleared inside a period would be
    /// missed entirely.
    flush: AtomicU32,
    device_rate: AtomicU32,
    channels: AtomicU32,
    /// Elements waiting, staged and queued together.
    pub pending: AtomicU32,
    /// The same for the interfering station.
    pub qrm_pending: AtomicU32,
    /// Frames the endpoint holds before they are heard.
    ///
    /// Measured rather than derived from the request, because the two differ: an
    /// endpoint hands back whatever buffer it likes, and until this was published
    /// the setting that claims to control the latency did not.
    pub latency_frames: AtomicU32,
    frames: AtomicU64,
    /// Periods the endpoint did not ask for in time.
    underruns: AtomicU64,
    format: Mutex<String>,
    error: Mutex<String>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            running: AtomicBool::new(false),
            flush: AtomicU32::new(0),
            device_rate: AtomicU32::new(0),
            channels: AtomicU32::new(0),
            pending: AtomicU32::new(0),
            qrm_pending: AtomicU32::new(0),
            latency_frames: AtomicU32::new(0),
            frames: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            format: Mutex::new(String::new()),
            error: Mutex::new(String::new()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputStatus {
    pub running: bool,
    pub device_rate: u32,
    pub channels: u32,
    pub pending: u32,
    pub qrm_pending: u32,
    /// Frames queued ahead of the loudspeaker, which is the key latency.
    pub latency_frames: u32,
    pub frames: u64,
    pub underruns: u64,
    pub format: String,
    pub error: String,
}

impl OutputStatus {
    pub fn idle() -> OutputStatus {
        OutputStatus {
            running: false,
            device_rate: 0,
            channels: 0,
            pending: 0,
            qrm_pending: 0,
            latency_frames: 0,
            frames: 0,
            underruns: 0,
            format: String::new(),
            error: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub device_id: String,
    pub buffer_ms: u32,
}

impl OutputConfig {
    pub fn from_settings(settings: &AudioSettings) -> OutputConfig {
        OutputConfig {
            device_id: settings.device_id.clone(),
            buffer_ms: settings.buffer_ms.clamp(2, 200),
        }
    }
}

pub struct OutputStream {
    shared: Arc<Shared>,
    live: Arc<Live>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    elements: Producer<Element>,
    /// The interfering station. A queue of its own, because it carries different
    /// material: sharing one would make the interference the same text at another
    /// pitch, which the ear separates trivially and learns nothing from.
    interference: Producer<Element>,
    scope: Consumer<f32>,
    edges: Consumer<Edge>,
    keyed: Consumer<Keyed>,
    /// Contacts and the settings the paddle machine reads.
    ///
    /// Shared with the audio thread rather than passed through the element
    /// queue, because a contact is a state and not an event: what the machine
    /// needs at an element boundary is whether the paddle is closed now.
    paddle: Arc<Paddle>,
    /// Rate the endpoint opened at, so a caller can convert samples to time
    /// without reading the status.
    rate: u32,
    buffer_ms: u32,
}

impl OutputStream {
    /// Opens the endpoint and starts the thread.
    ///
    /// The rate is not known until the endpoint is open, so the open happens
    /// here rather than on the thread: a caller that has to convert seconds to
    /// samples would otherwise have to wait for the thread to publish one.
    pub fn start(
        cfg: OutputConfig,
        tone: &ToneSettings,
        conditions: &ConditionsSettings,
    ) -> Result<OutputStream> {
        let shared = Arc::new(Shared::new());
        let live = Live::new(tone, conditions);
        let stop = Arc::new(AtomicBool::new(false));

        let paddle = Paddle::new();

        let (element_tx, element_rx) = ring::channel::<Element>(ELEMENT_CAPACITY);
        let (qrm_tx, qrm_rx) = ring::channel::<Element>(ELEMENT_CAPACITY);
        let (scope_tx, scope_rx) = ring::channel::<f32>(SCOPE_CAPACITY);
        let (edge_tx, edge_rx) = ring::channel::<Edge>(EDGE_CAPACITY);
        let (keyed_tx, keyed_rx) = ring::channel::<Keyed>(KEYED_CAPACITY);

        // The endpoint is probed on this thread so the failure is returned
        // rather than published, and so the rate is known before the first
        // element is made.
        let probed = unsafe { probe(&cfg) }?;
        let rate = probed.rate;
        shared.device_rate.store(rate, Ordering::Relaxed);
        shared.channels.store(probed.channels as u32, Ordering::Relaxed);
        if let Ok(mut slot) = shared.format.lock() {
            *slot = probed.describe.clone();
        }

        let thread_shared = shared.clone();
        let thread_stop = stop.clone();
        let thread_live = live.clone();
        let thread_paddle = paddle.clone();
        let thread_cfg = cfg.clone();

        let handle = std::thread::Builder::new()
            .name("cwdrill-output".to_string())
            .spawn(move || {
                let result = run(
                    &thread_cfg,
                    thread_live,
                    thread_paddle,
                    element_rx,
                    qrm_rx,
                    scope_tx,
                    edge_tx,
                    keyed_tx,
                    &thread_shared,
                    &thread_stop,
                );
                thread_shared.running.store(false, Ordering::Release);
                if let Err(e) = result {
                    crate::log_error!("audio", "output stopped: {}", e);
                    if let Ok(mut slot) = thread_shared.error.lock() {
                        *slot = e.to_string();
                    }
                }
            })
            .map_err(|e| Error::audio(format!("cannot start the output thread: {}", e)))?;

        crate::log_info!(
            "audio",
            "output started, {}, buffer {} ms",
            probed.describe,
            cfg.buffer_ms
        );

        Ok(OutputStream {
            shared,
            live,
            stop,
            handle: Some(handle),
            elements: element_tx,
            interference: qrm_tx,
            scope: scope_rx,
            edges: edge_rx,
            keyed: keyed_rx,
            paddle,
            rate,
            buffer_ms: cfg.buffer_ms,
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Frames the endpoint holds before they are heard.
    ///
    /// Read so a sample index can be turned into a time: the frame counter says
    /// what has been rendered, and what is audible is that less the buffer.
    ///
    /// The measured depth rather than the requested one. The two differ whenever
    /// the endpoint hands back a buffer larger than the request, which is common
    /// and which used to make every derived time wrong by the difference.
    pub fn buffer_frames(&self) -> u32 {
        let measured = self.shared.latency_frames.load(Ordering::Relaxed);
        if measured > 0 {
            measured
        } else {
            (self.rate as f32 * self.buffer_ms as f32 * 0.001) as u32
        }
    }

    /// Samples one envelope entry covers.
    pub fn scope_step(&self) -> f32 {
        self.rate as f32 / SCOPE_RATE
    }

    pub fn status(&self) -> OutputStatus {
        let format = self
            .shared
            .format
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        let error = self
            .shared
            .error
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|e| e.into_inner().clone());
        OutputStatus {
            running: self.shared.running.load(Ordering::Acquire),
            device_rate: self.shared.device_rate.load(Ordering::Relaxed),
            channels: self.shared.channels.load(Ordering::Relaxed),
            pending: self.shared.pending.load(Ordering::Relaxed),
            qrm_pending: self.shared.qrm_pending.load(Ordering::Relaxed),
            latency_frames: self.shared.latency_frames.load(Ordering::Relaxed),
            frames: self.shared.frames.load(Ordering::Relaxed),
            underruns: self.shared.underruns.load(Ordering::Relaxed),
            format,
            error,
        }
    }

    /// Pushes the settings the generator reads live.
    ///
    /// One call for both, because they are read in one pass: publishing them
    /// separately would let the audio thread see a note from one frame and a band
    /// from another, and the two together decide the level of the noise.
    pub fn publish(&self, tone: &ToneSettings, conditions: &ConditionsSettings) {
        self.live.publish(tone, conditions);
    }

    /// Elements waiting to be sent.
    pub fn pending(&self) -> u32 {
        self.shared.pending.load(Ordering::Relaxed)
    }

    /// Elements waiting for the interfering station.
    pub fn interference_pending(&self) -> u32 {
        self.shared.qrm_pending.load(Ordering::Relaxed)
    }

    /// Queues material for the interfering station.
    pub fn push_interference(&self, elements: &[Element]) -> bool {
        if elements.is_empty() {
            return true;
        }
        self.interference.write(elements)
    }

    /// Queues elements. False when the queue has no room for all of them.
    ///
    /// All or nothing, because half a character is not a character: the queue
    /// would send the first three elements of a letter and the caller would have
    /// no way to know which.
    pub fn push(&self, elements: &[Element]) -> bool {
        if elements.is_empty() {
            return true;
        }
        self.elements.write(elements)
    }

    /// Discards everything queued and cuts the tone.
    pub fn flush(&self) {
        self.shared.flush.fetch_add(1, Ordering::Release);
    }

    /// Moves envelope entries into the destination, oldest first.
    pub fn read_scope(&self, dst: &mut [f32]) -> usize {
        self.scope.read(dst)
    }

    /// Moves element boundaries into the destination, oldest first.
    pub fn read_edges(&self, dst: &mut [Edge]) -> usize {
        self.edges.read(dst)
    }

    /// Moves what the paddle produced into the destination, oldest first.
    pub fn read_keyed(&self, dst: &mut [Keyed]) -> usize {
        self.keyed.read(dst)
    }

    /// Closes or opens the dot contact.
    pub fn paddle_dit(&self, down: bool) {
        self.paddle.set_dit(down);
    }

    /// Closes or opens the dash contact.
    pub fn paddle_dah(&self, down: bool) {
        self.paddle.set_dah(down);
    }

    /// Opens both contacts.
    ///
    /// Called when the session ends, because a button held at that moment
    /// receives no release the paddle would see and the tone would stay on.
    pub fn paddle_release(&self) {
        self.paddle.release();
    }

    /// State of the two contacts, as the picture draws them.
    ///
    /// Read by the picture rather than derived from what the interface last
    /// wrote: the two would agree, and a second copy of one fact is a second
    /// chance for them not to.
    pub fn paddle_contacts(&self) -> (bool, bool) {
        self.paddle.contacts()
    }

    /// The hand keyed mark being held, and the length of one dot, in samples.
    ///
    /// Nought while nothing is held, and always nought with a paddle: an iambic
    /// element is decided whole and has no in between state to draw.
    pub fn paddle_progress(&self) -> (u32, u32) {
        self.paddle.progress()
    }

    /// Pushes the paddle settings and whether the sidetone is wanted.
    pub fn publish_paddle(
        &self,
        paddle: &PaddleSettings,
        timing: &TimingSettings,
        sending: bool,
    ) {
        self.paddle.publish(paddle.mode, timing, sending);
    }

    /// Sample index of the newest sample rendered.
    pub fn frames(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for OutputStream {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What a probe of the endpoint discovered.
struct Probed {
    rate: u32,
    channels: usize,
    describe: String,
}

/// Opens the endpoint, reads its format and releases it again.
///
/// Paid so the caller learns the rate before the thread exists. The endpoint is
/// opened a second time on the thread, which costs a few milliseconds once per
/// start and removes the alternative: a caller that waits for the thread to
/// publish a rate before it can make its first element.
unsafe fn probe(cfg: &OutputConfig) -> Result<Probed> {
    let _apartment = Apartment::new(true);
    let device = crate::audio::open_device(&cfg.device_id)?;
    let table = &*(*device.as_raw()).vtbl;

    let mut client: ComPtr<IAudioClient> = ComPtr::null();
    let hr = (table.Activate)(
        device.as_raw(),
        &IID_IAudioClient,
        CLSCTX_ALL,
        std::ptr::null_mut(),
        client.out() as *mut *mut c_void,
    );
    if hr != S_OK || client.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot activate the audio client",
            hr as i64,
        ));
    }
    let ctable = &*(*client.as_raw()).vtbl;

    let mut raw: *mut WAVEFORMATEX = std::ptr::null_mut();
    let hr = (ctable.GetMixFormat)(client.as_raw(), &mut raw);
    if hr != S_OK || raw.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot read the mix format",
            hr as i64,
        ));
    }
    let guard = MixFormat(raw);
    let format = parse_format(guard.0)?;

    Ok(Probed {
        rate: format.rate,
        channels: format.channels,
        describe: format.describe(),
    })
}

/// Reads a format descriptor, following the extension when there is one.
///
/// The extension is where every modern endpoint states its sample kind: the tag
/// in the base structure says only that an extension follows, and a reader that
/// stopped there would take a float stream for integer.
unsafe fn parse_format(raw: *const WAVEFORMATEX) -> Result<Format> {
    if raw.is_null() {
        return Err(Error::audio("the endpoint reported no format"));
    }
    // The structures are byte packed, so a field is read through a copy rather
    // than through a reference: a reference to a packed field is not aligned and
    // is undefined behaviour to form.
    let base = std::ptr::read_unaligned(raw);
    let channels = base.nChannels as usize;
    let rate = base.nSamplesPerSec;
    let bits = base.wBitsPerSample;

    if channels == 0 || rate == 0 || bits == 0 {
        return Err(Error::audio("the endpoint reported an unusable format"));
    }

    let mut tag = base.wFormatTag;
    if tag == WAVE_FORMAT_EXTENSIBLE {
        if base.cbSize < 22 {
            return Err(Error::audio("the format extension is too short"));
        }
        let extended = std::ptr::read_unaligned(raw as *const WAVEFORMATEXTENSIBLE);
        let sub = extended.SubFormat;
        tag = if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            WAVE_FORMAT_PCM
        } else {
            return Err(Error::audio("the endpoint uses a sample format this build cannot write"));
        };
    }

    let kind = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleKind::F32,
        (WAVE_FORMAT_PCM, 16) => SampleKind::I16,
        // Twenty four valid bits in a thirty two bit container is the same write
        // as thirty two: the low bits are ignored rather than misread.
        (WAVE_FORMAT_PCM, 24) | (WAVE_FORMAT_PCM, 32) => SampleKind::I32,
        _ => {
            return Err(Error::audio(format!(
                "the endpoint reports {} bit format {}, which this build cannot write",
                bits, tag
            )))
        }
    };
    let bytes = match kind {
        SampleKind::I16 => 2,
        _ => 4,
    };

    // Twenty four bit in a three byte container is refused rather than written
    // wrongly: the frame stride would be right and every sample would land one
    // byte off.
    if (bits / 8) as usize != bytes {
        return Err(Error::audio(format!(
            "the endpoint packs {} bits into {} bytes, which this build cannot write",
            bits,
            base.nBlockAlign as usize / channels.max(1)
        )));
    }

    Ok(Format { kind, rate, channels, bytes })
}

/// Everything the fill loop needs, held so the failure paths release it.
struct Opened {
    client: ComPtr<IAudioClient>,
    render: ComPtr<IAudioRenderClient>,
    event: HANDLE,
    buffer_frames: u32,
    /// Frames kept queued, which is what the operator hears as the key latency.
    ///
    /// Held apart from the endpoint buffer because the two are different numbers.
    /// The endpoint decides how large a buffer it offers and some drivers offer
    /// several times the request; filling all of it queues that much audio, and
    /// with the paddle running that is the delay between a press and the tone.
    /// Every frame queued is a frame the machine already decided, so the depth
    /// cannot be recovered afterwards.
    target_frames: u32,
    format: Format,
}

impl Drop for Opened {
    fn drop(&mut self) {
        if !self.event.is_null() {
            unsafe { CloseHandle(self.event) };
            self.event = std::ptr::null_mut();
        }
    }
}

unsafe fn open(cfg: &OutputConfig) -> Result<Opened> {
    let device = crate::audio::open_device(&cfg.device_id)?;
    let table = &*(*device.as_raw()).vtbl;

    let mut client: ComPtr<IAudioClient> = ComPtr::null();
    let hr = (table.Activate)(
        device.as_raw(),
        &IID_IAudioClient,
        CLSCTX_ALL,
        std::ptr::null_mut(),
        client.out() as *mut *mut c_void,
    );
    if hr != S_OK || client.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot activate the audio client",
            hr as i64,
        ));
    }
    let ctable = &*(*client.as_raw()).vtbl;

    let mut raw: *mut WAVEFORMATEX = std::ptr::null_mut();
    let hr = (ctable.GetMixFormat)(client.as_raw(), &mut raw);
    if hr != S_OK || raw.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot read the mix format",
            hr as i64,
        ));
    }
    let mix = MixFormat(raw);
    let format = parse_format(mix.0)?;

    // The periodicity must be nought in shared mode: the mixer decides it, and a
    // stated value is refused outright.
    let duration = cfg.buffer_ms as REFERENCE_TIME * 10_000;
    let hr = (ctable.Initialize)(
        client.as_raw(),
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
        duration,
        0,
        mix.0,
        std::ptr::null(),
    );
    if hr != S_OK {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot initialize the audio client",
            hr as i64,
        ));
    }

    // Automatic reset: the endpoint signals once per period and the wait clears
    // it, so a manual reset event would have to be cleared by hand and a missed
    // clear would spin the loop.
    let event = CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null());
    if event.is_null() {
        return Err(Error::audio("cannot create the buffer event"));
    }
    let hr = (ctable.SetEventHandle)(client.as_raw(), event);
    if hr != S_OK {
        CloseHandle(event);
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "the endpoint refused the buffer event",
            hr as i64,
        ));
    }

    let mut render: ComPtr<IAudioRenderClient> = ComPtr::null();
    let hr = (ctable.GetService)(
        client.as_raw(),
        &IID_IAudioRenderClient,
        render.out() as *mut *mut c_void,
    );
    if hr != S_OK || render.is_null() {
        CloseHandle(event);
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "cannot obtain the render client",
            hr as i64,
        ));
    }

    let mut buffer_frames: u32 = 0;
    let hr = (ctable.GetBufferSize)(client.as_raw(), &mut buffer_frames);
    if hr != S_OK || buffer_frames == 0 {
        CloseHandle(event);
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "the endpoint reported no buffer",
            hr as i64,
        ));
    }

    // The floor is two periods. The endpoint signals once per period and the
    // fill happens after the signal, so one period leaves no margin for the
    // scheduler and the result is a gap in the tone rather than a shorter delay.
    let mut default_period: REFERENCE_TIME = 0;
    let mut min_period: REFERENCE_TIME = 0;
    (ctable.GetDevicePeriod)(client.as_raw(), &mut default_period, &mut min_period);
    let period_frames = if default_period > 0 {
        ((default_period as f64 * 1.0e-7) * format.rate as f64).round() as u32
    } else {
        buffer_frames / 2
    };
    let wanted = ((cfg.buffer_ms as f64 * 1.0e-3) * format.rate as f64).round() as u32;
    let target_frames = wanted
        .max(period_frames.saturating_mul(2))
        .min(buffer_frames)
        .max(1);

    crate::log_info!(
        "audio",
        "endpoint buffer {} frames, period {}, keeping {} queued ({:.0} ms)",
        buffer_frames,
        period_frames,
        target_frames,
        target_frames as f32 * 1000.0 / format.rate as f32
    );

    Ok(Opened { client, render, event, buffer_frames, target_frames, format })
}

#[allow(clippy::too_many_arguments)]
fn run(
    cfg: &OutputConfig,
    live: Arc<Live>,
    paddle: Arc<Paddle>,
    elements: Consumer<Element>,
    interference: Consumer<Element>,
    scope: Producer<f32>,
    edges: Producer<Edge>,
    keyed: Producer<Keyed>,
    shared: &Shared,
    stop: &AtomicBool,
) -> Result<()> {
    let _apartment = Apartment::new(true);
    // Above normal rather than time critical. A trainer that stutters is a
    // defect; a machine whose interface cannot be reached because a tone
    // generator has priority over everything is worse.
    crate::platform::win32::raise_thread_priority();

    let opened = unsafe { open(cfg) }?;
    let format = opened.format;

    shared.device_rate.store(format.rate, Ordering::Relaxed);
    shared.channels.store(format.channels as u32, Ordering::Relaxed);
    shared
        .latency_frames
        .store(opened.target_frames, Ordering::Relaxed);
    if let Ok(mut slot) = shared.format.lock() {
        *slot = format.describe();
    }

    let mut generator = Generator::new(
        format.rate,
        live,
        paddle,
        elements,
        interference,
        scope,
        edges,
        keyed,
    );
    let mut seen_flush = shared.flush.load(Ordering::Acquire);

    // Primed before the endpoint starts, so the first period plays silence this
    // application wrote rather than whatever the previous owner left there.
    unsafe { fill(&opened, &mut generator, shared) }?;

    let ctable = unsafe { &*(*opened.client.as_raw()).vtbl };
    let hr = unsafe { (ctable.Start)(opened.client.as_raw()) };
    if hr != S_OK {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            "the endpoint refused to start",
            hr as i64,
        ));
    }
    shared.running.store(true, Ordering::Release);

    let timeout = cfg.buffer_ms * WAIT_PERIODS + 50;
    let mut consecutive_timeouts = 0u32;

    while !stop.load(Ordering::Relaxed) {
        let waited = unsafe { WaitForSingleObject(opened.event, timeout) };
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match waited {
            WAIT_OBJECT_0 => consecutive_timeouts = 0,
            WAIT_TIMEOUT => {
                // Counted rather than fatal on its own: a scheduling delay looks
                // the same as an endpoint that has stopped, and only the second
                // repeats.
                shared.underruns.fetch_add(1, Ordering::Relaxed);
                consecutive_timeouts += 1;
                if consecutive_timeouts >= 4 {
                    return Err(Error::audio("the endpoint stopped asking for audio"));
                }
                continue;
            }
            other => {
                return Err(Error::with_code(
                    crate::core::error::Category::Audio,
                    "the buffer event failed",
                    other as i64,
                ))
            }
        }

        let flush = shared.flush.load(Ordering::Acquire);
        if flush != seen_flush {
            seen_flush = flush;
            generator.flush();
        }

        unsafe { fill(&opened, &mut generator, shared) }?;
    }

    unsafe {
        (ctable.Stop)(opened.client.as_raw());
    }
    crate::log_info!(
        "audio",
        "output ended, {} frames, {} underruns",
        shared.frames.load(Ordering::Relaxed),
        shared.underruns.load(Ordering::Relaxed)
    );
    let _ = INFINITE;
    Ok(())
}

/// Writes as much as the endpoint will take.
unsafe fn fill(opened: &Opened, generator: &mut Generator, shared: &Shared) -> Result<()> {
    let ctable = &*(*opened.client.as_raw()).vtbl;
    let rtable = &*(*opened.render.as_raw()).vtbl;

    let mut padding: u32 = 0;
    let hr = (ctable.GetCurrentPadding)(opened.client.as_raw(), &mut padding);
    if hr != S_OK {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            if hr == AUDCLNT_E_DEVICE_INVALIDATED {
                "the endpoint went away"
            } else {
                "cannot read the buffer fill"
            },
            hr as i64,
        ));
    }

    // The target rather than the whole buffer. Filling everything free queues as
    // much audio as the endpoint will hold, and with the paddle running that is
    // the delay between the press and the tone: a frame already written is a
    // decision already taken.
    let frames = opened.target_frames.saturating_sub(padding);
    if frames == 0 {
        return Ok(());
    }

    let mut raw: *mut u8 = std::ptr::null_mut();
    let hr = (rtable.GetBuffer)(opened.render.as_raw(), frames, &mut raw);
    if hr != S_OK || raw.is_null() {
        return Err(Error::with_code(
            crate::core::error::Category::Audio,
            if hr == AUDCLNT_E_DEVICE_INVALIDATED {
                "the endpoint went away"
            } else {
                "cannot obtain the render buffer"
            },
            hr as i64,
        ));
    }

    let bytes = frames as usize * opened.format.stride();
    let dst = std::slice::from_raw_parts_mut(raw, bytes);
    generator.render(dst, frames as usize, opened.format);

    // Released whatever happened above. A buffer obtained and not released is a
    // buffer the endpoint waits on forever.
    (rtable.ReleaseBuffer)(opened.render.as_raw(), frames, 0);

    shared.frames.store(generator.frames(), Ordering::Relaxed);
    shared.pending.store(generator.pending(), Ordering::Relaxed);
    shared
        .qrm_pending
        .store(generator.interference_pending(), Ordering::Relaxed);
    Ok(())
}