//! Monotonic timing on top of QueryPerformanceCounter.
//!
//! std::time::Instant would work, but the whole platform layer is FFI based
//! and the DSP code needs raw ticks for sample clock drift estimation, so the
//! counter is exposed directly.

use std::sync::OnceLock;

use crate::platform::win32::ffi;

fn frequency() -> f64 {
    static FREQ: OnceLock<f64> = OnceLock::new();
    *FREQ.get_or_init(|| {
        let mut f: i64 = 0;
        unsafe { ffi::QueryPerformanceFrequency(&mut f) };
        if f <= 0 { 1.0 } else { f as f64 }
    })
}

/// Raw performance counter value.
pub fn ticks() -> i64 {
    let mut t: i64 = 0;
    unsafe { ffi::QueryPerformanceCounter(&mut t) };
    t
}

pub fn ticks_per_second() -> f64 {
    frequency()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instant(i64);

impl Instant {
    pub fn now() -> Self {
        Instant(ticks())
    }

    pub fn elapsed_secs(&self) -> f64 {
        (ticks() - self.0) as f64 / frequency()
    }

    pub fn elapsed_ms(&self) -> f64 {
        self.elapsed_secs() * 1000.0
    }

    pub fn duration_since(&self, earlier: Instant) -> f64 {
        (self.0 - earlier.0) as f64 / frequency()
    }

    pub fn raw(&self) -> i64 {
        self.0
    }
}