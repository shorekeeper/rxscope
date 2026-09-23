//! RXScope entry point.
//!
//! Windows only. Everything specific to the operating system goes through the
//! platform layer, and all rendering goes through the hand written Vulkan
//! bindings under render::vk.
//!
//! Startup order matters:
//!   1. logger, so every later failure is recorded;
//!   2. settings, because window geometry and interface scale come from them;
//!   3. window, because the Vulkan surface needs a window handle;
//!   4. application loop.
//! 
// Release builds have no console: the log goes to a file and to the debugger,
// so stdout carries nothing and the window is a blank rectangle beside the
// application. Debug builds keep it, because a panic message printed there is
// the fastest way to read one.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod callsign;
mod config;
mod core;
mod decode;
mod dsp;
mod font;
mod gui;
mod platform;
mod record;
mod render;
mod rig;
mod i18n;
mod stations;

use crate::config::Settings;
use crate::core::log;

fn main() {
    // The config path can be overridden with a single positional argument, which
    // is what lets several profiles run side by side.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.first() {
        Some(p) => std::path::PathBuf::from(p),
        None => Settings::default_path(),
    };

    // The settings have to be readable before the logger is fully configured, so
    // the logger starts with defaults and is reconfigured immediately after.
    log::init(log::Level::Info, None, true, 0);

    let mut settings = match Settings::load(&config_path) {
        Ok(s) => s,
        Err(e) => {
            log_warn!("config", "load failed ({}), using defaults", e);
            Settings::default()
        }
    };

    let log_file = if settings.log.file_path.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&settings.log.file_path))
    };
    log::init(
        settings.log.level,
        log_file.as_deref(),
        settings.log.to_debugger,
        settings.log.max_size_kb,
    );
    log_info!("main", "RXScope {} starting", env!("CARGO_PKG_VERSION"));
    log_info!("main", "config: {}", config_path.display());

    // Written back immediately so keys added by a newer build materialize and
    // the reference file stays complete for the operator.
    if let Err(e) = settings.save(&config_path) {
        log_warn!("config", "save failed: {}", e);
    }

    // The catalogue is installed before the interface exists, so every widget
    // built later resolves through it. A template is written whenever the
    // directory is missing, which gives a translator a complete starting file
    // rather than a list extracted from the source.
    let language_dir = settings.language_dir();
    if !language_dir.exists() {
        if let Err(e) = i18n::Catalog::write_template(&language_dir) {
            log_warn!("i18n", "cannot write the translation template: {}", e);
        }
    }
    i18n::install(i18n::Catalog::load(&language_dir, &settings.ui.language));

    let exit_code = match app::App::new(settings) {
        Ok(mut a) => {
            let code = a.run();
            // Geometry and any interface state changed at run time are persisted
            // here rather than inside the loop.
            settings = a.take_settings();
            if let Err(e) = settings.save(&config_path) {
                log_warn!("config", "final save failed: {}", e);
            }
            code
        }
        Err(e) => {
            log_error!("main", "startup failed: {}", e);
            platform::message_box("RXScope startup error", &format!("{}", e));
            1
        }
    };

    log_info!("main", "exit code {}", exit_code);
    log::shutdown();
    std::process::exit(exit_code);
}