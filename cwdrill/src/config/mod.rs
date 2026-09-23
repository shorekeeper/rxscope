//! Configuration layer: a small INI reader and writer plus the typed settings
//! tree used by every subsystem.

pub mod ini;
pub mod settings;

pub use ini::ConfigEnum;
pub use settings::Settings;