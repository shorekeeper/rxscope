//! WASAPI capture loop.
//!
//! One loop covers both directions. An input endpoint is opened plainly; an
//! output endpoint is opened with the loopback flag, which makes it deliver
//! what the system is playing. The direction comes from the device identifier.
//!
//! The stream is polled rather than event driven. Event notification is a
//! little tighter, but it is not supported uniformly for loopback streams
//! across Windows versions, and the polling interval used here is a quarter of
//! the configured period, which keeps the added latency well under a
//! millisecond at any sane setting.
//!
//! A loopback stream stops delivering packets entirely while nothing is
//! playing, rather than delivering silence. That would freeze the sample clock
//! and stall the waterfall, so the idle time is measured and filled in.
//!
//! Exclusive mode is attempted only when asked for, and any failure falls back
//! to shared mode with the device mix format. Exclusive mode has strict format
//! and buffer alignment rules that differ per driver, and a receiver that
//! silently keeps working is better than one that refuses to start.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::convert::SampleFormat;
use crate::audio::device::DeviceRef;
use crate::audio::{CaptureConfig, Pipeline};
use crate::core::{Error, Instant, Result};
use crate::platform::win32::com::*;
use crate::platform::win32::{sleep_ms, wide};

fn hr_error(op: &str, hr: HRESULT) -> Error {
    Error::with_code(
        crate::core::error::Category::Audio,
        format!("{} failed, 0x{:08X}", op, hr as u32),
        hr as i64,
    )
}

pub fn run(cfg: &CaptureConfig, pipeline: &mut Pipeline, stop: &AtomicBool) -> Result<()> {
    let reference = DeviceRef::parse(&cfg.device_id, cfg.backend);
    let loopback = reference.is_loopback();

    // The capture thread owns its own multithreaded apartment; the objects
    // created here must not outlive it.
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
        let hr = if reference.is_default() {
            ((*(*e).vtbl).GetDefaultAudioEndpoint)(e, reference.flow(), E_CONSOLE, device.out())
        } else {
            let id = wide(&reference.raw);
            ((*(*e).vtbl).GetDevice)(e, id.as_ptr(), device.out())
        };
        if hr != S_OK || device.is_null() {
            return Err(hr_error("device open", hr));
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
    crate::log_info!(
        "audio",
        "device format {} Hz, {} channels, {}, {} bytes per frame{}",
        rate,
        channels,
        format.as_str(),
        frame_bytes,
        if loopback { ", loopback" } else { "" }
    );

    let duration: REFERENCE_TIME = (cfg.period_ms.max(2) as i64) * 10_000;
    // Loopback and exclusive mode are mutually exclusive, so the flag is only
    // meaningful for a real capture endpoint.
    let stream_flags = if loopback { AUDCLNT_STREAMFLAGS_LOOPBACK } else { 0 };

    let mut shared = true;
    if cfg.exclusive && !loopback {
        let hr = unsafe {
            let c = client.as_raw();
            ((*(*c).vtbl).Initialize)(
                c,
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                stream_flags,
                duration,
                duration,
                mix.0,
                std::ptr::null(),
            )
        };
        if hr == S_OK {
            shared = false;
            crate::log_info!("audio", "exclusive mode active");
        } else {
            crate::log_warn!(
                "audio",
                "exclusive mode refused, 0x{:08X}, falling back to shared",
                hr as u32
            );
            // A failed Initialize leaves the client unusable, so it is replaced
            // rather than reinitialized.
            client = ComPtr::null();
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
                    return Err(hr_error("IMMDevice::Activate after fallback", hr));
                }
            }
        }
    }

    if shared {
        let hr = unsafe {
            let c = client.as_raw();
            ((*(*c).vtbl).Initialize)(
                c,
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                duration,
                0,
                mix.0,
                std::ptr::null(),
            )
        };
        if hr != S_OK {
            return Err(hr_error("IAudioClient::Initialize", hr));
        }
    }

    let mut buffer_frames = 0u32;
    unsafe {
        let c = client.as_raw();
        ((*(*c).vtbl).GetBufferSize)(c, &mut buffer_frames);
    }

    let mut capture: ComPtr<IAudioCaptureClient> = ComPtr::null();
    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).GetService)(
            c,
            &IID_IAudioCaptureClient,
            capture.out() as *mut *mut c_void,
        );
        if hr != S_OK || capture.is_null() {
            return Err(hr_error("GetService IAudioCaptureClient", hr));
        }
    }

    pipeline.configure(rate, channels, format, frame_bytes);

    unsafe {
        let c = client.as_raw();
        let hr = ((*(*c).vtbl).Start)(c);
        if hr != S_OK {
            return Err(hr_error("IAudioClient::Start", hr));
        }
    }

    let poll_ms = (cfg.period_ms / 4).max(1);
    crate::log_info!(
        "audio",
        "capture started, endpoint buffer {} frames, poll {} ms",
        buffer_frames,
        poll_ms
    );

    // Idle handling for a silent loopback endpoint. The threshold is two
    // periods so an ordinary scheduling gap is not mistaken for silence.
    let idle_threshold = (cfg.period_ms.max(2) as f64 / 1000.0) * 2.0;
    let mut last_activity = Instant::now();
    let mut reported_silent = false;
    let mut result = Ok(());

    while !stop.load(Ordering::Relaxed) {
        let mut packet = 0u32;
        let hr = unsafe {
            let c = capture.as_raw();
            ((*(*c).vtbl).GetNextPacketSize)(c, &mut packet)
        };
        if hr != S_OK {
            result = Err(hr_error("GetNextPacketSize", hr));
            break;
        }
        if packet == 0 {
            if loopback {
                let idle = last_activity.elapsed_secs();
                if idle > idle_threshold {
                    // The sample clock has to keep running: the waterfall scrolls
                    // in real time and the decoders measure element lengths in
                    // samples, so a gap has to be filled rather than skipped.
                    let frames = (idle * rate as f64) as usize;
                    if frames > 0 {
                        pipeline.push_silence(frames);
                    }
                    last_activity = Instant::now();
                    if reported_silent {
                        reported_silent = false;
                        pipeline.note_silence(false);
                    }
                }
                if idle > 1.0 && !reported_silent {
                    reported_silent = true;
                    pipeline.note_silence(true);
                    crate::log_info!(
                        "audio",
                        "loopback endpoint is idle, nothing is playing to it"
                    );
                }
            }
            sleep_ms(poll_ms);
            continue;
        }
        // Everything queued is drained before sleeping again, so a scheduling
        // hiccup does not leave data sitting in the endpoint buffer.
        while packet != 0 && !stop.load(Ordering::Relaxed) {
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            let hr = unsafe {
                let c = capture.as_raw();
                ((*(*c).vtbl).GetBuffer)(
                    c,
                    &mut data,
                    &mut frames,
                    &mut flags,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if hr == AUDCLNT_S_BUFFER_EMPTY {
                break;
            }
            if hr != S_OK {
                result = Err(hr_error("GetBuffer", hr));
                break;
            }

            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY != 0 {
                pipeline.note_discontinuity();
            }

            if frames > 0 {
                if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
                    // A silent packet carries no memory to read; the gap is
                    // filled so the sample clock stays aligned.
                    pipeline.push_silence(frames as usize);
                } else {
                    let len = frames as usize * frame_bytes;
                    let slice = unsafe { std::slice::from_raw_parts(data, len) };
                    pipeline.push(slice, frames as usize);
                }
                last_activity = Instant::now();
            }

            let hr = unsafe {
                let c = capture.as_raw();
                ((*(*c).vtbl).ReleaseBuffer)(c, frames)
            };
            if hr != S_OK {
                result = Err(hr_error("ReleaseBuffer", hr));
                break;
            }
            
            let hr = unsafe {
                let c = capture.as_raw();
                ((*(*c).vtbl).GetNextPacketSize)(c, &mut packet)
            };
            if hr != S_OK {
                result = Err(hr_error("GetNextPacketSize", hr));
                break;
            }
        }
    }

    unsafe {
        let c = client.as_raw();
        ((*(*c).vtbl).Stop)(c);
    }
    crate::log_info!("audio", "capture stopped");
    result
}

/// Extracts the sample layout from a format block.
///
/// A plain WAVEFORMATEX carries the type in its tag. The extensible form, which
/// is what a shared mode mix format always uses, stores the real type in a
/// subformat identifier and the tag is only a marker.
///
/// An unrecognized subformat is reported with the identifier spelled out: a
/// driver that invents its own is rare but not impossible, and without the
/// value in the log there is nothing to act on.
unsafe fn parse_format(wf: *const WAVEFORMATEX) -> Result<(u32, usize, SampleFormat, usize)> {
    let base = *wf;
    // Fields are copied out before use: a reference to a packed field is not
    // allowed, and the logging macros take their arguments by reference.
    let rate = base.nSamplesPerSec;
    let channels = base.nChannels as usize;
    let bits = base.wBitsPerSample;
    let frame_bytes = base.nBlockAlign as usize;
    let extension_bytes = base.cbSize;

    if rate == 0 || channels == 0 || frame_bytes == 0 {
        return Err(Error::audio("device reported an empty format"));
    }

    let mut tag = base.wFormatTag;
    if tag == WAVE_FORMAT_EXTENSIBLE {
        if extension_bytes < 22 {
            return Err(Error::audio(format!(
                "extensible format block is truncated, cbSize {}",
                extension_bytes
            )));
        }
        let ext = wf as *const WAVEFORMATEXTENSIBLE;
        let sub = (*ext).SubFormat;
        let valid_bits = (*ext).wValidBitsPerSample;

        tag = if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
            WAVE_FORMAT_IEEE_FLOAT
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            WAVE_FORMAT_PCM
        } else {
            let d1 = sub.data1;
            let d2 = sub.data2;
            let d3 = sub.data3;
            let d4 = sub.data4;
            return Err(Error::audio(format!(
                "unsupported subformat {:08X}-{:04X}-{:04X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                d1, d2, d3, d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]
            )));
        };

        // A container wider than the valid bits happens on some interfaces, for
        // example twenty four bits inside a thirty two bit slot. The converter
        // reads the container, so only a mismatch worth noting is logged.
        if valid_bits != 0 && valid_bits != bits {
            crate::log_debug!("audio", "container {} bits carries {} valid bits", bits, valid_bits);
        }
    }

    let format = match (tag, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => SampleFormat::F32,
        (WAVE_FORMAT_PCM, 8) => SampleFormat::U8,
        (WAVE_FORMAT_PCM, 16) => SampleFormat::I16,
        (WAVE_FORMAT_PCM, 24) => SampleFormat::I24,
        (WAVE_FORMAT_PCM, 32) => SampleFormat::I32,
        _ => {
            return Err(Error::audio(format!(
                "unsupported format, tag {} with {} bits",
                tag, bits
            )))
        }
    };

    Ok((rate, channels, format, frame_bytes))
}