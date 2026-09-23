//! Platform abstraction. Only Windows is implemented; the public surface is
//! kept narrow so a future backend has a short contract to satisfy.

pub mod input;

#[cfg(windows)]
pub mod win32;

#[cfg(windows)]
pub use win32::window::{CursorKind, Window, WindowConfig};

#[cfg(windows)]
pub use win32::{current_thread_id, debug_output, message_box, sleep_ms};

pub use input::{Event, Key, Modifiers, MouseButton};