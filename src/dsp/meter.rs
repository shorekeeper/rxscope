//! Signal strength meter.
//!
//! The input is the root mean square level of the audio before the automatic
//! gain control, in decibels relative to full scale. Attack and release are
//! applied here rather than in the audio thread because the interface refresh
//! rate is what the operator actually sees, and a meter that settles in the
//! audio thread would then be resampled by the display anyway.
//!
//! The S unit mapping follows the amateur convention: six decibels per unit
//! with S9 at a calibrated reference. The reference is a property of the
//! receiver and the sound card wiring, so it lives in the configuration.

use crate::config::settings::{MeterScale, MeterSettings};

/// Level reported when nothing has been measured yet.
const FLOOR_DB: f32 = -140.0;

/// Decibels per second the held peak falls once the hold time expires.
const PEAK_FALL_DB_PER_S: f32 = 24.0;

pub struct SMeter {
    level_db: f32,
    peak_db: f32,
    hold_left: f32,
}

impl SMeter {
    pub fn new() -> SMeter {
        SMeter { level_db: FLOOR_DB, peak_db: FLOOR_DB, hold_left: 0.0 }
    }

    pub fn update(&mut self, input_db: f32, dt: f32, cfg: &MeterSettings) {
        if !input_db.is_finite() {
            return;
        }
        let target = input_db + cfg.calibration_db;

        // A single pole with a different coefficient in each direction gives
        // the classic fast attack and slow release of an analogue meter.
        let tau = if target > self.level_db { cfg.attack_ms } else { cfg.release_ms } * 0.001;
        let a = if tau <= 1e-6 { 1.0 } else { 1.0 - (-dt / tau).exp() };
        self.level_db += a.clamp(0.0, 1.0) * (target - self.level_db);

        if self.level_db >= self.peak_db {
            self.peak_db = self.level_db;
            self.hold_left = cfg.peak_hold_ms * 0.001;
        } else {
            self.hold_left -= dt;
            if self.hold_left <= 0.0 {
                self.peak_db = (self.peak_db - PEAK_FALL_DB_PER_S * dt).max(self.level_db);
            }
        }
    }

    pub fn reset(&mut self) {
        self.level_db = FLOOR_DB;
        self.peak_db = FLOOR_DB;
        self.hold_left = 0.0;
    }

    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    pub fn peak_db(&self) -> f32 {
        self.peak_db
    }

    /// Position of a level on the displayed scale, zero at the left end.
    ///
    /// The S scale is deliberately not linear across the whole width: the nine
    /// units below S9 take sixty percent of it and the sixty decibels above S9
    /// take the rest, which matches the printed scale of a receiver.
    pub fn fraction(&self, db: f32, cfg: &MeterSettings) -> f32 {
        match cfg.scale {
            MeterScale::SUnits => {
                let over = db - cfg.s9_reference_dbfs;
                if over <= 0.0 {
                    let units = 9.0 + over / 6.0;
                    (((units - 1.0) / 8.0) * 0.6).clamp(0.0, 0.6)
                } else {
                    (0.6 + (over / 60.0) * 0.4).clamp(0.6, 1.0)
                }
            }
            // S9 corresponds to fifty microvolts at fifty ohms, which is
            // minus seventy three decibels relative to a milliwatt.
            MeterScale::Dbm => {
                let dbm = db - cfg.s9_reference_dbfs - 73.0;
                ((dbm + 121.0) / 108.0).clamp(0.0, 1.0)
            }
            MeterScale::DbFs => ((db + 100.0) / 100.0).clamp(0.0, 1.0),
        }
    }

    pub fn text(&self, cfg: &MeterSettings) -> String {
        match cfg.scale {
            MeterScale::SUnits => {
                let over = self.level_db - cfg.s9_reference_dbfs;
                if over <= 0.0 {
                    let units = (9.0 + over / 6.0).clamp(0.0, 9.0);
                    format!("S{:.0}", units)
                } else {
                    format!("S9+{:.0}", over)
                }
            }
            MeterScale::Dbm => format!("{:.0} dBm", self.level_db - cfg.s9_reference_dbfs - 73.0),
            MeterScale::DbFs => format!("{:.1} dBFS", self.level_db),
        }
    }
}

impl Default for SMeter {
    fn default() -> SMeter {
        SMeter::new()
    }
}