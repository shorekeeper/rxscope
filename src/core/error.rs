//! Single error type for the whole application.
//!
//! Subsystems do not define private error enums. Every failure carries a
//! category plus a human readable message, which is enough for a desktop
//! application and keeps the module boundaries thin.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Platform,
    Config,
    Io,
    Vulkan,
    Audio,
    Font,
    Dsp,
    Other,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Platform => "platform",
            Category::Config => "config",
            Category::Io => "io",
            Category::Vulkan => "vulkan",
            Category::Audio => "audio",
            Category::Font => "font",
            Category::Dsp => "dsp",
            Category::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub category: Category,
    pub message: String,
    /// Win32 GetLastError or a Vulkan result code, 0 when not applicable.
    pub code: i64,
}

impl Error {
    pub fn new(category: Category, message: impl Into<String>) -> Self {
        Error { category, message: message.into(), code: 0 }
    }

    pub fn with_code(category: Category, message: impl Into<String>, code: i64) -> Self {
        Error { category, message: message.into(), code }
    }

    pub fn platform(message: impl Into<String>) -> Self {
        Error::new(Category::Platform, message)
    }
    pub fn config(message: impl Into<String>) -> Self {
        Error::new(Category::Config, message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Error::new(Category::Io, message)
    }
    pub fn vulkan(message: impl Into<String>) -> Self {
        Error::new(Category::Vulkan, message)
    }
    pub fn audio(message: impl Into<String>) -> Self {
        Error::new(Category::Audio, message)
    }
    pub fn font(message: impl Into<String>) -> Self {
        Error::new(Category::Font, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code != 0 {
            write!(f, "[{}] {} (code {})", self.category.as_str(), self.message, self.code)
        } else {
            write!(f, "[{}] {}", self.category.as_str(), self.message)
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::with_code(Category::Io, e.to_string(), e.raw_os_error().unwrap_or(0) as i64)
    }
}

pub type Result<T> = std::result::Result<T, Error>;