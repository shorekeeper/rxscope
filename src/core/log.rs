//! Minimal logger: level filter, monotonic timestamp, module tag, optional
//! file sink and optional OutputDebugStringW sink.
//!
//! The logger is global because DSP and audio threads must be able to report
//! problems without carrying a handle around. Writes are serialized by a
//! mutex; log volume in normal operation is a few lines per second, so the
//! contention cost is irrelevant compared to the audio path.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Off = 5,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
            Level::Off => "OFF  ",
        }
    }
}

struct Sink {
    level: Level,
    file: Option<File>,
    /// Kept so the file can be reopened after a rotation.
    path: Option<PathBuf>,
    /// Size at which the file is rotated, nought disabling it.
    max_bytes: u64,
    /// Bytes in the current file.
    ///
    /// Tracked rather than queried, because a metadata call per line would be
    /// a system call per line on a path a decoder thread reaches through.
    written: u64,
    to_debugger: bool,
    start: crate::core::time::Instant,
}

impl Sink {
    /// Moves the current file aside and opens a fresh one.
    ///
    /// One generation is kept. Two would be a retention policy, and a receiver
    /// log is read within minutes of the fault it describes or not at all; the
    /// previous file exists so a rotation that happens between the fault and
    /// the reading does not lose it.
    fn rotate(&mut self) {
        let path = match self.path.as_ref() {
            Some(p) => p.clone(),
            None => return,
        };

        // The handle is released first. A rename of an open file is refused on
        // this platform, and the failure would be silent: the size test would
        // pass on every subsequent line and the rotation would never happen.
        self.file = None;

        let previous = PathBuf::from(format!("{}.1", path.display()));
        let _ = std::fs::remove_file(&previous);
        let _ = std::fs::rename(&path, &previous);

        self.file = OpenOptions::new().create(true).append(true).open(&path).ok();
        self.written = 0;
    }
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Installs or replaces the global sink. Safe to call twice: the second call
/// simply reconfigures the existing logger (used after the config is parsed).
///
/// The size limit is in kilobytes and nought disables rotation, which is what a
/// session being captured by another tool wants: an external reader following
/// the file would lose its position at every rename.
pub fn init(level: Level, file_path: Option<&Path>, to_debugger: bool, max_size_kb: u32) {
    let file = file_path.and_then(|p| {
        if let Some(dir) = p.parent() {
            if !dir.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(dir);
            }
        }
        OpenOptions::new().create(true).append(true).open(p).ok()
    });

    // The existing size is read once so a session appended to a file that is
    // already at the limit rotates on its first line rather than doubling it.
    let written = file_path
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let start = guard
        .as_ref()
        .map(|s| s.start)
        .unwrap_or_else(crate::core::time::Instant::now);
    *guard = Some(Sink {
        level,
        file,
        path: file_path.map(|p| p.to_path_buf()),
        max_bytes: max_size_kb as u64 * 1024,
        written,
        to_debugger,
        start,
    });
}

pub fn shutdown() {
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sink) = guard.as_mut() {
        if let Some(f) = sink.file.as_mut() {
            let _ = f.flush();
        }
    }
    *guard = None;
}

pub fn set_level(level: Level) {
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sink) = guard.as_mut() {
        sink.level = level;
    }
}

/// Formatting entry point used by the macros. Keep it out of hot loops.
pub fn record(level: Level, module: &str, args: std::fmt::Arguments<'_>) {
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    let sink = match guard.as_mut() {
        Some(s) if level >= s.level && s.level != Level::Off => s,
        _ => return,
    };

    let t = sink.start.elapsed_secs();
    // Thread id is printed because audio, DSP and UI all log into one file.
    let tid = platform::current_thread_id();
    let line = format!("[{:9.3}] [{}] [{:5}] {:<10} {}\n", t, level.as_str(), tid, module, args);

    if let Some(f) = sink.file.as_mut() {
        let _ = f.write_all(line.as_bytes());
        sink.written += line.len() as u64;
        // Tested after the write rather than before it, so a line is never
        // split across a rotation. The overshoot is one line.
        if sink.max_bytes > 0 && sink.written >= sink.max_bytes {
            sink.rotate();
        }
    }
    if sink.to_debugger {
        platform::debug_output(&line);
    }
}

#[macro_export]
macro_rules! log_trace {
    ($module:expr, $($arg:tt)*) => {
        $crate::core::log::record($crate::core::log::Level::Trace, $module, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_debug {
    ($module:expr, $($arg:tt)*) => {
        $crate::core::log::record($crate::core::log::Level::Debug, $module, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_info {
    ($module:expr, $($arg:tt)*) => {
        $crate::core::log::record($crate::core::log::Level::Info, $module, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_warn {
    ($module:expr, $($arg:tt)*) => {
        $crate::core::log::record($crate::core::log::Level::Warn, $module, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_error {
    ($module:expr, $($arg:tt)*) => {
        $crate::core::log::record($crate::core::log::Level::Error, $module, format_args!($($arg)*))
    };
}