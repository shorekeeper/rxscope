//! Font file discovery.
//!
//! Search order: the path from the configuration, then a fonts directory next
//! to the executable so a portable install can ship its own faces, then the
//! system Fonts directory. Candidate file names are plain TrueType faces;
//! collections and CFF based faces are rejected by the parser.

use std::path::PathBuf;

/// Preferred interface faces, first match wins.
pub const UI_CANDIDATES: &[&str] = &[
    "segoeui.ttf",
    "tahoma.ttf",
    "verdana.ttf",
    "arial.ttf",
    "calibri.ttf",
];

/// Preferred fixed pitch faces for decoded text and numeric readouts.
pub const MONO_CANDIDATES: &[&str] = &["consola.ttf", "cour.ttf", "lucon.ttf"];

pub fn find_font(explicit: &str, candidates: &[&str]) -> Option<PathBuf> {
    if !explicit.is_empty() {
        let p = PathBuf::from(explicit);
        if p.is_file() {
            return Some(p);
        }
        crate::log_warn!("font", "configured font not found: {}", explicit);
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let local = dir.join("fonts");
            for name in candidates {
                let p = local.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }

    if let Some(win) = crate::platform::win32::windows_directory() {
        let dir = win.join("Fonts");
        for name in candidates {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}