//! waveIn API.
//!
//! Kept as a fallback for drivers whose WASAPI capture path misbehaves and for
//! virtual cables that only expose the legacy interface. The API is older and
//! coarser: the format is fixed integer PCM and the buffer granularity is
//! whatever the driver decides, but it works everywhere.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

use super::com::WAVEFORMATEX;
use super::ffi::{BOOL, DWORD, UINT};

pub type MMRESULT = u32;
pub type HWAVEIN = *mut c_void;

pub const MMSYSERR_NOERROR: MMRESULT = 0;
pub const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
/// Signal an event object instead of invoking a callback. An event avoids the
/// restrictions that apply inside a waveIn callback.
pub const CALLBACK_EVENT: u32 = 0x0005_0000;
pub const WHDR_DONE: DWORD = 0x0000_0001;
pub const WAVE_FORMAT_DIRECT: u32 = 0x0008;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WAVEHDR {
    pub lpData: *mut u8,
    pub dwBufferLength: DWORD,
    pub dwBytesRecorded: DWORD,
    pub dwUser: usize,
    pub dwFlags: DWORD,
    pub dwLoops: DWORD,
    pub lpNext: *mut WAVEHDR,
    pub reserved: usize,
}

impl Default for WAVEHDR {
    fn default() -> WAVEHDR {
        WAVEHDR {
            lpData: std::ptr::null_mut(),
            dwBufferLength: 0,
            dwBytesRecorded: 0,
            dwUser: 0,
            dwFlags: 0,
            dwLoops: 0,
            lpNext: std::ptr::null_mut(),
            reserved: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WAVEINCAPSW {
    pub wMid: u16,
    pub wPid: u16,
    pub vDriverVersion: u32,
    /// Fixed size name field, always null terminated by the driver.
    pub szPname: [u16; 32],
    pub dwFormats: DWORD,
    pub wChannels: u16,
    pub wReserved1: u16,
}

impl Default for WAVEINCAPSW {
    fn default() -> WAVEINCAPSW {
        WAVEINCAPSW {
            wMid: 0,
            wPid: 0,
            vDriverVersion: 0,
            szPname: [0; 32],
            dwFormats: 0,
            wChannels: 0,
            wReserved1: 0,
        }
    }
}

#[link(name = "winmm")]
extern "system" {
    pub fn waveInGetNumDevs() -> UINT;
    pub fn waveInGetDevCapsW(uDeviceID: usize, pwic: *mut WAVEINCAPSW, cbwic: UINT) -> MMRESULT;
    pub fn waveInOpen(
        phwi: *mut HWAVEIN,
        uDeviceID: UINT,
        pwfx: *const WAVEFORMATEX,
        dwCallback: usize,
        dwInstance: usize,
        fdwOpen: DWORD,
    ) -> MMRESULT;
    pub fn waveInClose(hwi: HWAVEIN) -> MMRESULT;
    pub fn waveInPrepareHeader(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: UINT) -> MMRESULT;
    pub fn waveInUnprepareHeader(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: UINT) -> MMRESULT;
    pub fn waveInAddBuffer(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: UINT) -> MMRESULT;
    pub fn waveInStart(hwi: HWAVEIN) -> MMRESULT;
    pub fn waveInStop(hwi: HWAVEIN) -> MMRESULT;
    pub fn waveInReset(hwi: HWAVEIN) -> MMRESULT;
    pub fn waveInGetErrorTextW(mmrError: MMRESULT, pszText: *mut u16, cchText: UINT) -> MMRESULT;
}

/// Human readable driver error, for the status line.
pub fn error_text(code: MMRESULT) -> String {
    let mut buf = [0u16; 256];
    let r = unsafe { waveInGetErrorTextW(code, buf.as_mut_ptr(), buf.len() as UINT) };
    if r == MMSYSERR_NOERROR {
        super::from_wide(&buf)
    } else {
        format!("waveIn error {}", code)
    }
}

/// Reads the friendly name of a legacy input device.
pub fn device_name(index: u32) -> Option<String> {
    let mut caps = WAVEINCAPSW::default();
    let r = unsafe {
        waveInGetDevCapsW(
            index as usize,
            &mut caps,
            std::mem::size_of::<WAVEINCAPSW>() as UINT,
        )
    };
    if r != MMSYSERR_NOERROR {
        return None;
    }
    Some(super::from_wide(&caps.szPname))
}

/// Present only so the linker keeps the BOOL alias in use across builds.
const _: Option<BOOL> = None;