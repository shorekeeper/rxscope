//! Configuration layer: a small INI reader and writer plus the typed settings
//! tree used by every subsystem.

pub mod ini;
pub mod settings;

// Only the trait is re exported: the document type is reached through its own
// module by the two places that construct one.
pub use ini::ConfigEnum;
pub use settings::Settings;