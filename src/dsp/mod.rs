//! Signal processing engine driven by the interface thread.
//!
//! The engine pulls converted samples out of the capture queue, runs the short
//! time transform, pushes every resulting line into the waterfall and keeps the
//! meter up to date. It is rebuilt only when a setting that changes the
//! transform geometry moves, which is detected through a signature rather than
//! by comparing every field.
//!
//! Everything here runs on the thread that owns the renderer, because the
//! waterfall writes into a GPU texture. The cost is bounded: at the default
//! settings the transform runs a few dozen times per second on four thousand
//! points, which is well under a tenth of a millisecond per frame.

pub mod blanker;
pub mod colormap;
pub mod fft;
pub mod meter;
pub mod receiver;
pub mod spectrum;
pub mod waterfall;
pub mod window;

use crate::config::Settings;
use crate::core::Result;
use crate::render::Renderer;

pub use meter::SMeter;
pub use receiver::{ReceiverChain, ReceiverStatus};
pub use spectrum::Spectrum;
pub use waterfall::Waterfall;

/// Largest factor the transform is widened by under magnification.
///
/// Four takes a four thousand point transform to sixteen thousand, which at the
/// decoder rate resolves under one hertz. Beyond that nothing further is gained:
/// the window already spans longer than any keying element, so the extra
/// resolution buys detail in frequency by discarding it in time.
const MAX_ZOOM_FACTOR: u32 = 4;

/// Longest window the widening will plan, in seconds.
///
/// A bound on the factor alone is not enough, because the factor multiplies
/// whatever the operator stated: an eight thousand point transform widened four
/// times spans nearly three seconds at the decoder rate, and a line that
/// integrates three seconds shows a keyed station as a continuous carrier.
///
/// A second and a half is a compromise stated in the terms that matter. It is
/// long enough that the widening still reaches its full factor at the default
/// transform size, and short enough that a slow transmission is still visible as
/// keying rather than as a stripe.
const MAX_WINDOW_SECONDS: f32 = 1.5;

/// Columns the history texture holds when the configuration states none.
///
/// The transform width, because a history coarser than the trace drawn over it
/// is invisible at full span and obvious under magnification: one stored column
/// then covers many pixels while the trace beside it still resolves individual
/// bins.
///
/// A complex transform produces one bin per point and a real one produces half
/// that, so this is exact for the first and twice what the second needs. The
/// arrangement can change while the application runs and the texture cannot, so
/// the wider of the two is stored.
fn waterfall_columns(settings: &Settings) -> u32 {
    let stated = settings.waterfall.columns;
    if stated > 0 {
        return stated.clamp(256, 16384);
    }
    // Twice the stated width when the magnification widens the transform, so the
    // history is not the coarser of the two surfaces at the magnification the
    // widening exists for. Not the whole factor: the trace reads the bins
    // directly and gains all of it, while the history is already at about one
    // stored column per pixel by then, and the texture cannot be resized once
    // the session has started.
    let boost = if settings.dsp.zoom_resolution { 2 } else { 1 };
    (settings.dsp.fft_size * boost).clamp(1024, 16384)
}

pub struct DspEngine {
    pub spectrum: Spectrum,
    pub waterfall: Waterfall,
    pub meter: SMeter,
    /// Impulse blanker ahead of the transform.
    ///
    /// Public because the panel reports what it acted on, and a count nobody can
    /// see is a setting nobody can judge.
    pub blanker: blanker::Blanker,
    sample_rate: u32,
    /// Fingerprint of the settings the spectrum was built from.
    signature: u64,
}

impl DspEngine {
    pub fn new(settings: &Settings, sample_rate: u32, renderer: &mut Renderer) -> Result<DspEngine> {
        let (size, hop) = plan(settings, sample_rate);
        let spectrum = Spectrum::new(
            size,
            hop,
            settings.dsp.fft_window,
            settings.dsp.kaiser_beta,
            settings.dsp.average_frames,
            sample_rate,
        );

        let mut waterfall = Waterfall::new(
            waterfall_columns(settings),
            settings.waterfall.history_lines,
            settings.waterfall.colormap,
            settings.waterfall.gpu_palette,
            settings.waterfall.smooth,
            renderer,
        )?;
        waterfall.set_range(
            settings.waterfall.min_db,
            settings.waterfall.max_db,
            settings.waterfall.gamma,
        );
        waterfall.set_mapping(
            settings.waterfall.style,
            settings.waterfall.auto_range,
            settings.waterfall.smoothing,
        );

        Ok(DspEngine {
            spectrum,
            waterfall,
            meter: SMeter::new(),
            blanker: blanker::Blanker::new(sample_rate),
            sample_rate,
            signature: signature(settings, sample_rate),
        })
    }

    /// Applies configuration changes. Cheap enough to call every frame.
    pub fn sync(&mut self, settings: &Settings, sample_rate: u32) {
        let sig = signature(settings, sample_rate);
        if sig != self.signature {
            let (size, hop) = plan(settings, sample_rate);
            self.spectrum = Spectrum::new(
                size,
                hop,
                settings.dsp.fft_window,
                settings.dsp.kaiser_beta,
                settings.dsp.average_frames,
                sample_rate,
            );
            self.signature = sig;
            self.sample_rate = sample_rate;
        }

        // Applied whatever the mode. The receiver blankers live on the monitor
        // thread and the samples this path carries never meet them, so this is
        // the only one the display and the decoders have.
        self.blanker.set_rate(sample_rate);
        self.blanker.configure(
            settings.dsp.noise_blanker,
            settings.dsp.noise_blanker_threshold,
            settings.audio.channel_mode,
        );

        // Whether the spectrum is two sided is decided by the caller and not
        // here. The setting says a quadrature pair is wanted and the device says
        // whether one is arriving, and only the caller holds both.
        //
        // Setting it from the setting alone is not merely incomplete, it is
        // destructive: on a mono endpoint with the pair requested the
        // arrangement would be set here and unset by the caller on the same
        // frame, and every change of arrangement resizes and clears the
        // accumulators, so the average and the peak hold would never fill.

        // The same quadrature correction the receiver front end applies. The
        // channel swap is the one that decides which way round the picture is:
        // swapping conjugates the spectrum, so a display that ignored it would
        // draw the upper sideband where the ear hears the lower one.
        self.spectrum.set_iq_correction(
            settings.receiver.iq_swap,
            settings.receiver.iq_gain_db,
            settings.receiver.iq_phase_deg,
        );

        // Palette, range and mapping affect only the next line, so they are
        // applied without any rebuild.
        self.waterfall.set_colormap(settings.waterfall.colormap);
        self.waterfall.set_range(
            settings.waterfall.min_db,
            settings.waterfall.max_db,
            settings.waterfall.gamma,
        );
        self.waterfall.set_mapping(
            settings.waterfall.style,
            settings.waterfall.auto_range,
            settings.waterfall.smoothing,
        );
    }

    /// Applies the settings that need the renderer.
    ///
    /// Held apart from the ordinary synchronization because the renderer is not
    /// available at every call site, and because the one setting here drains the
    /// device when it moves.
    pub fn sync_display(&mut self, settings: &Settings, renderer: &mut Renderer) {
        if let Err(e) = self.waterfall.set_smooth(settings.waterfall.smooth, renderer) {
            crate::log_warn!("dsp", "cannot change the waterfall filter: {}", e);
        }
    }

    /// Lines the transform is currently producing, per second.
    ///
    /// The rate the path settled on rather than the one that was requested. The
    /// two differ whenever the overlap setting is the binding constraint, see the
    /// note on the planner, and only this one describes what the operator is
    /// looking at.
    pub fn line_rate(&self) -> f32 {
        self.spectrum.sample_rate() as f32 / self.spectrum.hop().max(1) as f32
    }

    /// Removes impulses from one block, in place.
    ///
    /// A step of its own rather than the head of the feed, because the same
    /// blanked samples go to the transform and to the decoders and blanking them
    /// twice would blank a hole in the second copy that is not in the first.
    pub fn blank(&mut self, frames: &mut [[f32; 2]]) {
        self.blanker.process(frames);
    }

    /// Consumes a block of frames and emits as many waterfall lines as the hop
    /// allows.
    ///
    /// Takes pairs because the spectrum decides for itself whether the second
    /// channel is a quadrature component or a duplicate, and the caller has no
    /// reason to know.
    pub fn feed(&mut self, samples: &[[f32; 2]], renderer: &mut Renderer) -> Result<()> {
        self.spectrum.feed(samples);
        while self.spectrum.next_frame() {
            self.waterfall.push(self.spectrum.bins(), renderer)?;
        }
        Ok(())
    }

    /// Slides the accumulated spectrum surfaces after a retune.
    ///
    /// The waterfall needs nothing here: it records the dial of every line and
    /// places them at drawing time, so its history is corrected without being
    /// rewritten. The spectrum has no per line record, only one accumulation,
    /// so the accumulation is what moves.
    pub fn shift_history(&mut self, bins: i32) {
        self.spectrum.shift(bins);
    }

    pub fn update_meter(&mut self, rms_db: f32, dt: f32, settings: &Settings) {
        self.meter.update(rms_db, dt, &settings.meter);
    }

    pub fn reset(&mut self) {
        self.spectrum.reset();
        self.meter.reset();
        // The delay line holds samples from before a discontinuity, and its
        // level estimate describes a signal that is no longer arriving.
        self.blanker.reset();
    }
}

/// Chooses the transform size and hop.
///
/// ## Which of the two settings decides the line rate
///
/// The hop is the number of samples consumed per line, so the line rate is the
/// sample rate over the hop and nothing else. Two settings bound it and they
/// bound it from opposite sides.
///
/// The stated speed asks for a hop of the sample rate over the requested rate.
/// The overlap states the largest hop that may be used, as a fraction of the
/// window, and therefore states a floor on the line rate rather than a ceiling:
/// the rate cannot fall below the sample rate over that largest hop. Whichever
/// of the two demands the smaller hop is the one that decides.
///
/// At the ordinary settings the requested speed decides and the overlap sits far
/// above it, which is why the effective overlap is usually much higher than the
/// stated one. At a high sample rate or a high stated overlap the other one
/// decides, and the waterfall then runs faster than the speed control claims.
/// The panel reports the rate that resulted, because from the two controls alone
/// there is no way to tell which of them is acting.
///
/// ## Why the hop is planned against the unwidened window
///
/// The magnification widens the window, which raises the overlap ceiling in
/// proportion. Applying that raised ceiling would let the hop grow whenever the
/// overlap was the binding constraint, and the waterfall would slow by the whole
/// widening factor at the moment the operator magnified the display.
///
/// Planning the hop against the stated size instead makes it independent of the
/// magnification by construction, so the line rate is unchanged whichever of the
/// two settings is acting. What the widening then costs is arithmetic: the
/// transform runs at the same rate on a window several times longer, so the work
/// grows a little faster than the factor itself.
fn plan(settings: &Settings, sample_rate: u32) -> (usize, usize) {
    let base = (settings.dsp.fft_size as usize).max(64).next_power_of_two();
    let size = (base * zoom_factor(settings, sample_rate) as usize).min(65536);

    let lines = settings.waterfall.scroll_lines_per_second.max(1.0);
    let from_rate = (sample_rate as f32 / lines).round().max(1.0) as usize;
    // Against the stated size rather than the widened one, see the note above.
    let ceiling = ((base as f32) * (1.0 - settings.dsp.overlap_percent as f32 / 100.0))
        .round()
        .max(32.0) as usize;
    let hop = from_rate.min(ceiling).clamp(32, base);
    (size, hop)
}

/// How much the transform is widened for the current magnification.
///
/// Quantized to powers of two, so the transform is replanned at three
/// magnifications rather than on every notch of the wheel. A replan discards the
/// running average and the peak hold, which is visible as a flicker, and a
/// continuous factor would produce one on every gesture.
///
/// Bounded by the window duration as well as by the factor, because the factor
/// multiplies whatever size the operator stated and a long window shows keying
/// as a continuous carrier.
fn zoom_factor(settings: &Settings, sample_rate: u32) -> u32 {
    if !settings.dsp.zoom_resolution {
        return 1;
    }
    let base = (settings.dsp.fft_size as usize).max(64).next_power_of_two();
    let longest = (MAX_WINDOW_SECONDS * sample_rate.max(1) as f32) as usize;
    let zoom = settings.waterfall.zoom.max(1.0);

    let mut factor = 1u32;
    while factor < MAX_ZOOM_FACTOR
        && (factor as f32) * 2.0 <= zoom
        && base * (factor as usize) * 2 <= longest
    {
        factor *= 2;
    }
    factor
}

/// Fingerprint of everything that forces a spectrum rebuild.
///
/// The window kind is folded in as its discriminant. That is valid because the
/// enumeration carries no data, and it avoids pulling the configuration trait
/// into this module just to hash a name.
///
/// The quadrature correction is deliberately absent: it is pushed through on
/// every frame and changing it must not throw away the accumulated average.
fn signature(settings: &Settings, sample_rate: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    mix(settings.dsp.fft_size as u64);
    // The magnification decides the geometry as surely as the stated size does,
    // so a change to it has to force the rebuild.
    mix(zoom_factor(settings, sample_rate) as u64);
    mix(settings.dsp.fft_window as u64 + 1);
    mix(settings.dsp.kaiser_beta.to_bits() as u64);
    mix(settings.dsp.overlap_percent as u64);
    mix(settings.dsp.average_frames as u64);
    mix(settings.waterfall.scroll_lines_per_second.to_bits() as u64);
    mix(sample_rate as u64);
    h
}