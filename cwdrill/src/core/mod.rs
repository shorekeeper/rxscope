//! Foundation types shared by every subsystem: errors, logging, timing and
//! the lock-free queue used to move audio between threads.

pub mod error;
pub mod log;
pub mod ring;
pub mod rng;
pub mod time;

pub use error::{Error, Result};
pub use rng::Rng;
pub use time::Instant;