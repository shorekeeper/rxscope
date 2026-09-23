//! Horizontal axis of the display.
//!
//! Every conversion between a position on the screen and a frequency goes
//! through here. Before this existed the same arithmetic appeared in the grid,
//! in four kinds of marker, in the click handler and in the cursor readout, and
//! two of those had already drifted: the grid was labelled on the air while the
//! trace underneath it was drawn in audio, so a change to the correction moved
//! one and not the other.
//!
//! ## Three facts the axis carries
//!
//! The full span. What the transform represents, from nought to the Nyquist
//! frequency on a real input and from minus it to plus it on a complex one.
//!
//! The view. The part of that span currently drawn. Everything downstream reads
//! the view and nothing reads the full span except the two places that index
//! into data laid out over it: the bin selection of the trace and the texture
//! coordinates of the waterfall. That is why zoom costs almost nothing here and
//! would have cost a rewrite anywhere else.
//!
//! The direction. A spectrum runs from low frequency to high, left to right,
//! always. On the lower sideband a rising audio frequency is a falling one on
//! the air, so the audio has to be drawn mirrored for the picture to obey that
//! rule. Without the mirror the grid labels descend across the display, which
//! is not a convention anybody reads.

use crate::rig::{Mapping, Sideband};

#[derive(Debug, Clone, Copy)]
pub struct Axis {
    /// Whole span the transform represents.
    full_low_hz: f32,
    full_high_hz: f32,
    /// Part of it currently drawn.
    low_hz: f32,
    high_hz: f32,
    /// True when the audio has to be drawn right to left.
    mirrored: bool,
    /// Correspondence to the band, absent without a transceiver.
    mapping: Option<Mapping>,
    /// Offset of the receiver from the reference the transceiver states.
    ///
    /// The mapping already carries that reference through its zero point: the
    /// audio frequency at which a station on the dial is heard, which is nought
    /// for a sideband mode and the sidetone pitch for a keyed one. This is what
    /// the receiver added on top of it, and the two together name the point the
    /// readout reports.
    ///
    /// Deliberately not the middle of the passband. That moves with either
    /// filter edge, so a readout built on it drifts whenever the width changes
    /// and stops agreeing with the transceiver.
    tuning_hz: f32,
}

impl Axis {
    /// Builds an axis.
    ///
    /// The view is stated as a magnification and a centre expressed as a
    /// fraction of the full span. A fraction rather than a frequency, because
    /// the full span changes with the sample rate and with the quadrature
    /// setting, and a stored frequency would land somewhere arbitrary after
    /// either; a fraction keeps the same relative window.
    ///
    /// Mirroring follows the sideband and nothing else. Every consumer goes
    /// through this object, so the mirror is applied once and consistently: a
    /// marker is placed with fraction_of_audio, a click is read with
    /// audio_of_fraction, the trace reverses its column mapping and the
    /// waterfall reverses its texture range. A decoder that works in audio is
    /// therefore unaffected by the mirror, which is why it is not conditioned
    /// on the operating mode: conditioning it there only produced a display
    /// whose frequency axis ran downwards on the lower sideband.
    pub fn new(
        full_low_hz: f32,
        full_high_hz: f32,
        zoom: f32,
        centre: f32,
        mapping: Option<Mapping>,
    ) -> Axis {
        let full_high_hz = full_high_hz.max(full_low_hz + 1.0);
        let full_span = full_high_hz - full_low_hz;

        let zoom = zoom.clamp(1.0, 4096.0);
        let span = full_span / zoom;
        let half = 0.5 / zoom;
        let centre = centre.clamp(half, 1.0 - half);
        let low = full_low_hz + full_span * centre - span * 0.5;

        let mirrored = matches!(mapping.as_ref().map(|m| m.sideband), Some(Sideband::Lower));

        Axis {
            full_low_hz,
            full_high_hz,
            low_hz: low,
            high_hz: low + span,
            mirrored,
            mapping,
            tuning_hz: 0.0,
        }
    }

    /// States how far the receiver moved from the transceiver reference.
    ///
    /// Nought in the skimmer, where nothing moves it: the transceiver is the
    /// only thing tuning and the mapping already describes it.
    pub fn with_tuning(mut self, offset_hz: f32) -> Axis {
        self.tuning_hz = offset_hz;
        self
    }

    pub fn mirrored(&self) -> bool {
        self.mirrored
    }

    pub fn low_hz(&self) -> f32 {
        self.low_hz
    }

    pub fn high_hz(&self) -> f32 {
        self.high_hz
    }

    pub fn span_hz(&self) -> f32 {
        self.high_hz - self.low_hz
    }

    pub fn full_low_hz(&self) -> f32 {
        self.full_low_hz
    }

    pub fn full_span_hz(&self) -> f32 {
        self.full_high_hz - self.full_low_hz
    }

    /// True when the whole span is on screen.
    pub fn is_full(&self) -> bool {
        self.span_hz() >= self.full_span_hz() - 0.5
    }

    pub fn mapping(&self) -> Option<&Mapping> {
        self.mapping.as_ref()
    }

    /// Position of an audio frequency, nought at the left edge.
    ///
    /// Not clamped. A caller drawing a marker wants to know that it fell off
    /// the display rather than to have it pinned to an edge, where it would
    /// look like a signal sitting exactly there.
    pub fn fraction_of_audio(&self, hz: f32) -> f32 {
        let t = (hz - self.low_hz) / self.span_hz();
        if self.mirrored {
            1.0 - t
        } else {
            t
        }
    }

    /// Audio frequency at a position.
    pub fn audio_of_fraction(&self, t: f32) -> f32 {
        let t = if self.mirrored { 1.0 - t } else { t };
        self.low_hz + t * self.span_hz()
    }

    /// Frequency on the air at a position.
    pub fn rf_of_fraction(&self, t: f32) -> Option<i64> {
        self.mapping.as_ref().map(|m| m.rf_of(self.audio_of_fraction(t)))
    }

    /// Position of a frequency on the air.
    pub fn fraction_of_rf(&self, hz: i64) -> Option<f32> {
        self.mapping
            .as_ref()
            .map(|m| self.fraction_of_audio(m.audio_of(hz)))
    }

    /// Range of frequencies on the air the view covers, ascending.
    pub fn rf_range(&self) -> Option<(i64, i64)> {
        let m = self.mapping.as_ref()?;
        let a = m.rf_of(self.low_hz);
        let b = m.rf_of(self.high_hz);
        Some(if a < b { (a, b) } else { (b, a) })
    }

    /// Audio frequency of the reference point.
    ///
    /// Where a signal has to sit for the readout to name its frequency. For a
    /// sideband mode it is the suppressed carrier, for a keyed mode the tone,
    /// for a double sideband mode the carrier.
    ///
    /// Distinct from the point the view is centred on. On audio from a
    /// transceiver this is nought hertz, which is the extreme edge of the
    /// display, so a view built on it would be pinned against that edge.
    pub fn reference_audio_hz(&self) -> f32 {
        self.mapping.as_ref().map(|m| m.zero_hz).unwrap_or(0.0) + self.tuning_hz
    }

    /// Frequency at the reference point, which is what is being received.
    ///
    /// The number the readout shows and the number an operator logs. Taken from
    /// the axis rather than from the transceiver directly, so a correction that
    /// moves the grid moves the readout with it: two statements about the same
    /// point that disagree are worse than either alone.
    pub fn reference_rf(&self) -> Option<i64> {
        let m = self.mapping.as_ref()?;
        Some(m.rf_of(m.zero_hz + self.tuning_hz))
    }

    /// Bin range the view covers, given a spectrum laid out over the full span.
    ///
    /// Half open and never empty: a view narrower than one bin still has to
    /// draw something, and an empty slice would leave the trace blank at the
    /// magnification where it matters most.
    pub fn bin_range(&self, count: usize) -> (usize, usize) {
        if count == 0 {
            return (0, 0);
        }
        let span = self.full_span_hz().max(1e-6);
        let t0 = ((self.low_hz - self.full_low_hz) / span).clamp(0.0, 1.0);
        let t1 = ((self.high_hz - self.full_low_hz) / span).clamp(0.0, 1.0);

        let lo = ((t0 * count as f32) as usize).min(count.saturating_sub(1));
        let hi = ((t1 * count as f32).ceil() as usize).clamp(lo + 1, count);
        (lo, hi)
    }

    /// Part of the full span in view, as fractions of it.
    ///
    /// Ascending and free of the mirror. Mirroring is a property of how the
    /// picture is laid out on screen, not of which part of the record is
    /// wanted, and a consumer that has to split the range at a wrap needs the
    /// two facts apart: folding them together makes the split land in the
    /// wrong place on a lower sideband display.
    pub fn view_fraction(&self) -> (f32, f32) {
        let span = self.full_span_hz().max(1e-6);
        (
            (self.low_hz - self.full_low_hz) / span,
            (self.high_hz - self.full_low_hz) / span,
        )
    }
}