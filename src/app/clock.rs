//! Frame timing: measured frame rate and an optional limiter.
//!
//! The rate is a plain count of frames completed inside a fixed interval, not
//! a filtered value: a running average hides exactly the stalls that matter
//! when the waterfall drops a line. The worst frame of the interval is reported
//! beside it for the same reason.
//!
//! The limiter sleeps for the bulk of the remaining time and spins for the
//! last fraction of a millisecond, because Sleep overshoots even with a 1 ms
//! timer period and a late present shows up as visible waterfall jitter.

use crate::core::time::{ticks, ticks_per_second};
use crate::platform;

/// Measurement window for the frame rate readout.
const FPS_INTERVAL_SECS: f64 = 0.5;

pub struct FrameClock {
    last: i64,
    /// Deadline for the next frame when the limiter is active.
    next_deadline: i64,
    /// Longest frame inside the current measurement interval.
    worst_dt: f32,
    worst_dt_last: f32,
    interval_start: i64,
    interval_frames: u32,
    fps: f32,
}

impl FrameClock {
    pub fn new() -> FrameClock {
        let now = ticks();
        FrameClock {
            last: now,
            next_deadline: now,
            worst_dt: 0.0,
            worst_dt_last: 0.0,
            interval_start: now,
            interval_frames: 0,
            fps: 0.0,
        }
    }

    /// Advances the clock and returns the delta time in seconds. The value is
    /// clamped so a debugger break does not produce an absurd step.
    pub fn tick(&mut self) -> f32 {
        let now = ticks();
        let freq = ticks_per_second();
        let raw = (now - self.last) as f64 / freq;
        self.last = now;
        let dt = raw.clamp(0.0, 0.25) as f32;

        self.interval_frames += 1;
        if dt > self.worst_dt {
            self.worst_dt = dt;
        }

        let elapsed = (now - self.interval_start) as f64 / freq;
        if elapsed >= FPS_INTERVAL_SECS {
            self.fps = (self.interval_frames as f64 / elapsed) as f32;
            self.worst_dt_last = self.worst_dt;
            self.interval_frames = 0;
            self.worst_dt = 0.0;
            self.interval_start = now;
        }
        dt
    }

    /// Frames per second measured over the last completed interval.
    pub fn fps(&self) -> f32 {
        self.fps
    }

    /// Longest frame time of the previous interval, in milliseconds.
    pub fn worst_frame_ms(&self) -> f32 {
        self.worst_dt_last * 1000.0
    }

    /// Blocks until the next frame slot for the requested rate.
    pub fn limit(&mut self, target_fps: u32) {
        if target_fps == 0 {
            return;
        }
        let freq = ticks_per_second();
        let period = (freq / target_fps as f64) as i64;
        let now = ticks();

        // Resync when the deadline is far behind, for example after a stall.
        if self.next_deadline < now - period * 4 {
            self.next_deadline = now;
        }
        self.next_deadline += period;

        loop {
            let now = ticks();
            let remaining = self.next_deadline - now;
            if remaining <= 0 {
                break;
            }
            let remaining_ms = remaining as f64 * 1000.0 / freq;
            if remaining_ms > 1.5 {
                platform::sleep_ms((remaining_ms - 1.0) as u32);
            } else {
                std::hint::spin_loop();
            }
        }
    }
}