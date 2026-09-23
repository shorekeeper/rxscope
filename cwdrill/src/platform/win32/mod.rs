//! Windows backend. Every call is a direct FFI call, no wrapper crates.

pub mod com;
pub mod ffi;
pub mod window;

/// Converts a Rust string to a null terminated UTF-16 buffer for the W APIs.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads a null terminated UTF-16 buffer back into a String.
pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Reads a null terminated UTF-16 string from a raw pointer. Used for the
/// strings COM hands back, where the length is not known in advance.
///
/// Safety: the pointer must be valid and terminated. A null pointer yields an
/// empty string, and the scan is capped so a missing terminator cannot run
/// away through the address space.
pub unsafe fn string_from_wide_ptr(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    const LIMIT: usize = 8192;
    let mut len = 0usize;
    while len < LIMIT && *p.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
}

pub fn current_thread_id() -> u32 {
    unsafe { ffi::GetCurrentThreadId() }
}

pub fn last_error() -> u32 {
    unsafe { ffi::GetLastError() }
}

pub fn debug_output(text: &str) {
    let w = wide(text);
    unsafe { ffi::OutputDebugStringW(w.as_ptr()) };
}

pub fn sleep_ms(ms: u32) {
    unsafe { ffi::Sleep(ms) };
}

/// Blocking message box, only used for fatal startup errors before the GUI
/// exists.
pub fn message_box(title: &str, text: &str) {
    let t = wide(title);
    let m = wide(text);
    unsafe {
        ffi::MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), ffi::MB_ICONERROR | ffi::MB_OK)
    };
}

/// Raises the timer resolution so the frame limiter and audio buffering are
/// predictable. Windows defaults to a 15.6 ms tick which is too coarse for a
/// 60 fps waterfall.
pub fn begin_high_resolution_timing() -> bool {
    unsafe { ffi::timeBeginPeriod(1) == 0 }
}

pub fn end_high_resolution_timing() {
    unsafe { ffi::timeEndPeriod(1) };
}

/// Windows directory, the parent of the system Fonts folder.
pub fn windows_directory() -> Option<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    let n = unsafe { ffi::GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 || n as usize >= buf.len() {
        return None;
    }
    Some(std::path::PathBuf::from(from_wide(&buf[..n as usize])))
}

/// Raises the priority of the calling thread. Failure is not fatal, it only
/// means the capture loop is more likely to be preempted.
pub fn raise_thread_priority() -> bool {
    unsafe {
        let h = ffi::GetCurrentThread();
        ffi::SetThreadPriority(h, ffi::THREAD_PRIORITY_HIGHEST) != 0
    }
}

/// Local wall clock as hours, minutes and seconds. The decode log stamps every
/// line, and the operator compares those stamps against a paper log, so local
/// time is what is wanted rather than a monotonic counter.
pub fn local_time_hms() -> (u16, u16, u16) {
    let mut t = ffi::SYSTEMTIME::default();
    unsafe { ffi::GetLocalTime(&mut t) };
    (t.wHour, t.wMinute, t.wSecond)
}

/// Coordinated time as hours, minutes and seconds.
///
/// What the decode transcript stamps every line with. A station log is kept in
/// coordinated time everywhere, so a transcript in local time forces whoever
/// reads it beside one to know which offset applied on the day of the
/// reception, which after a change of season nobody remembers.
pub fn utc_time_hms() -> (u16, u16, u16) {
    let mut t = ffi::SYSTEMTIME::default();
    unsafe { ffi::GetSystemTime(&mut t) };
    (t.wHour, t.wMinute, t.wSecond)
}

/// Coordinated wall clock, whole date and time.
pub fn utc_time_full() -> (u16, u16, u16, u16, u16, u16) {
    let mut t = ffi::SYSTEMTIME::default();
    unsafe { ffi::GetSystemTime(&mut t) };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Local wall clock, whole date and time.
///
/// A segment file name carries it, so the name sorts by time and an operator
/// reading the directory sees when a reception happened rather than a counter.
pub fn local_time_full() -> (u16, u16, u16, u16, u16, u16) {
    let mut t = ffi::SYSTEMTIME::default();
    unsafe { ffi::GetLocalTime(&mut t) };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}