//! waveIn capture loop.
//!
//! The legacy interface fixes the format, so sixteen bit integer stereo is
//! requested at the configured rate and the driver either accepts it or the
//! open fails. Buffers are recycled through a small pool: the driver fills one
//! and signals the event, the loop drains every finished buffer and hands it
//! straight back.
//!
//! Buffer memory and headers are owned by this function for the whole session.
//! They must stay at fixed addresses while the driver holds them, which is why
//! they live in boxed slices rather than in a growable vector.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::convert::SampleFormat;
use crate::audio::{CaptureConfig, Pipeline};
use crate::core::{Error, Result};
use crate::platform::win32::com::{WAVEFORMATEX, WAVE_FORMAT_PCM};
use crate::platform::win32::ffi;
use crate::platform::win32::mm;

/// Buffers in the pool. Four covers one scheduling hiccup at any period.
const POOL: usize = 4;

pub fn run(cfg: &CaptureConfig, pipeline: &mut Pipeline, stop: &AtomicBool) -> Result<()> {
    // An empty identifier selects the wave mapper, which routes to whatever the
    // system considers the preferred input.
    let reference = crate::audio::device::DeviceRef::parse(&cfg.device_id, cfg.backend);
    let device = if reference.is_default() {
        mm::WAVE_MAPPER
    } else {
        reference
            .raw
            .parse::<u32>()
            .map_err(|_| Error::audio(format!("bad waveIn device index '{}'", reference.raw)))?
    };

    let rate = if cfg.requested_rate == 0 { 48_000 } else { cfg.requested_rate };
    let channels: u16 = 2;
    let bits: u16 = 16;
    let block_align = channels * bits / 8;

    let format = WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM,
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * block_align as u32,
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    };

    let event = unsafe {
        ffi::CreateEventW(std::ptr::null_mut(), 0, 0, std::ptr::null())
    };
    if event.is_null() {
        return Err(Error::audio("CreateEventW failed"));
    }

    let mut handle: mm::HWAVEIN = std::ptr::null_mut();
    let r = unsafe {
        mm::waveInOpen(
            &mut handle,
            device,
            &format,
            event as usize,
            0,
            mm::CALLBACK_EVENT,
        )
    };
    if r != mm::MMSYSERR_NOERROR {
        unsafe { ffi::CloseHandle(event) };
        return Err(Error::audio(format!("waveInOpen: {}", mm::error_text(r))));
    }

    // Buffer size follows the configured period, rounded to whole frames.
    let frames_per_buffer =
        ((rate as u64 * cfg.period_ms.max(5) as u64) / 1000).max(64) as usize;
    let bytes_per_buffer = frames_per_buffer * block_align as usize;

    let mut storage: Vec<Box<[u8]>> = (0..POOL)
        .map(|_| vec![0u8; bytes_per_buffer].into_boxed_slice())
        .collect();
    let mut headers: Box<[mm::WAVEHDR]> = vec![mm::WAVEHDR::default(); POOL].into_boxed_slice();

    let header_size = std::mem::size_of::<mm::WAVEHDR>() as u32;
    let mut prepared = 0usize;
    let mut setup_error: Option<String> = None;

    for i in 0..POOL {
        headers[i].lpData = storage[i].as_mut_ptr();
        headers[i].dwBufferLength = bytes_per_buffer as u32;
        headers[i].dwFlags = 0;
        headers[i].dwBytesRecorded = 0;

        let r = unsafe { mm::waveInPrepareHeader(handle, &mut headers[i], header_size) };
        if r != mm::MMSYSERR_NOERROR {
            setup_error = Some(format!("waveInPrepareHeader: {}", mm::error_text(r)));
            break;
        }
        prepared += 1;

        let r = unsafe { mm::waveInAddBuffer(handle, &mut headers[i], header_size) };
        if r != mm::MMSYSERR_NOERROR {
            setup_error = Some(format!("waveInAddBuffer: {}", mm::error_text(r)));
            break;
        }
    }

    if let Some(message) = setup_error {
        cleanup(handle, &mut headers, prepared, header_size, event);
        return Err(Error::audio(message));
    }

    pipeline.configure(rate, channels as usize, SampleFormat::I16, block_align as usize);

    let r = unsafe { mm::waveInStart(handle) };
    if r != mm::MMSYSERR_NOERROR {
        cleanup(handle, &mut headers, prepared, header_size, event);
        return Err(Error::audio(format!("waveInStart: {}", mm::error_text(r))));
    }

    crate::log_info!(
        "audio",
        "waveIn started, {} Hz, {} buffers of {} frames",
        rate,
        POOL,
        frames_per_buffer
    );

    while !stop.load(Ordering::Relaxed) {
        // The timeout bounds how long a stop request waits; the driver signals
        // the event whenever a buffer completes.
        unsafe { ffi::WaitForSingleObject(event, 100) };

        for i in 0..POOL {
            if headers[i].dwFlags & mm::WHDR_DONE == 0 {
                continue;
            }
            let recorded = headers[i].dwBytesRecorded as usize;
            if recorded > 0 {
                let frames = recorded / block_align as usize;
                let slice = unsafe {
                    std::slice::from_raw_parts(headers[i].lpData as *const u8, recorded)
                };
                pipeline.push(slice, frames);
            }

            headers[i].dwBytesRecorded = 0;
            headers[i].dwFlags &= !mm::WHDR_DONE;
            let r = unsafe { mm::waveInAddBuffer(handle, &mut headers[i], header_size) };
            if r != mm::MMSYSERR_NOERROR {
                crate::log_warn!("audio", "waveInAddBuffer: {}", mm::error_text(r));
            }
        }
    }

    unsafe {
        mm::waveInStop(handle);
        // Reset returns every queued buffer, which is required before the
        // headers can be unprepared.
        mm::waveInReset(handle);
    }
    cleanup(handle, &mut headers, prepared, header_size, event);
    crate::log_info!("audio", "waveIn stopped");
    Ok(())
}

fn cleanup(
    handle: mm::HWAVEIN,
    headers: &mut [mm::WAVEHDR],
    prepared: usize,
    header_size: u32,
    event: ffi::HANDLE,
) {
    unsafe {
        for i in 0..prepared.min(headers.len()) {
            mm::waveInUnprepareHeader(handle, &mut headers[i], header_size);
        }
        if !handle.is_null() {
            mm::waveInClose(handle);
        }
        if !event.is_null() {
            ffi::CloseHandle(event);
        }
    }
}