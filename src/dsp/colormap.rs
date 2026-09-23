//! Waterfall palettes.
//!
//! Each map is a short list of key colours interpolated linearly into a two
//! hundred and fifty six entry table. Linear interpolation in sRGB is not
//! perceptually uniform, but the key points are chosen close enough together
//! that the visible banding stays below the noise of the display itself.

use crate::config::settings::ColorMap;

type Stop = (f32, [f32; 3]);

const GRAYSCALE: &[Stop] = &[(0.0, [0.0, 0.0, 0.0]), (1.0, [1.0, 1.0, 1.0])];

/// House palette: near black, deep navy, the interface accent, cyan, white.
const BLUE_STEEL: &[Stop] = &[
    (0.00, [0.02, 0.02, 0.04]),
    (0.20, [0.05, 0.10, 0.28]),
    (0.45, [0.11, 0.42, 0.78]),
    (0.70, [0.30, 0.75, 0.95]),
    (0.88, [0.80, 0.94, 1.00]),
    (1.00, [1.00, 1.00, 1.00]),
];

const INFERNO: &[Stop] = &[
    (0.00, [0.00, 0.00, 0.02]),
    (0.20, [0.22, 0.04, 0.33]),
    (0.40, [0.51, 0.12, 0.42]),
    (0.60, [0.79, 0.28, 0.27]),
    (0.80, [0.96, 0.56, 0.05]),
    (1.00, [0.99, 1.00, 0.64]),
];

const VIRIDIS: &[Stop] = &[
    (0.00, [0.27, 0.00, 0.33]),
    (0.25, [0.23, 0.32, 0.55]),
    (0.50, [0.13, 0.57, 0.55]),
    (0.75, [0.37, 0.79, 0.38]),
    (1.00, [0.99, 0.91, 0.14]),
];

const TURBO: &[Stop] = &[
    (0.00, [0.19, 0.07, 0.23]),
    (0.15, [0.16, 0.44, 0.86]),
    (0.35, [0.11, 0.85, 0.79]),
    (0.55, [0.60, 0.99, 0.27]),
    (0.75, [0.99, 0.72, 0.14]),
    (0.90, [0.92, 0.25, 0.04]),
    (1.00, [0.48, 0.01, 0.01]),
];

/// Builds an opaque two hundred and fifty six entry RGBA table.
pub fn build(kind: ColorMap) -> Vec<[u8; 4]> {
    let stops = match kind {
        ColorMap::Grayscale => GRAYSCALE,
        ColorMap::BlueSteel => BLUE_STEEL,
        ColorMap::Inferno => INFERNO,
        ColorMap::Viridis => VIRIDIS,
        ColorMap::Turbo => TURBO,
    };

    let mut table = Vec::with_capacity(256);
    for i in 0..256 {
        let t = i as f32 / 255.0;
        let c = sample(stops, t);
        table.push([
            (c[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (c[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            (c[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8,
            255,
        ]);
    }
    table
}

fn sample(stops: &[Stop], t: f32) -> [f32; 3] {
    if stops.is_empty() {
        return [0.0, 0.0, 0.0];
    }
    if t <= stops[0].0 {
        return stops[0].1;
    }
    for pair in stops.windows(2) {
        let (t0, c0) = pair[0];
        let (t1, c1) = pair[1];
        if t <= t1 {
            let span = (t1 - t0).max(1e-6);
            let f = (t - t0) / span;
            return [
                c0[0] + (c1[0] - c0[0]) * f,
                c0[1] + (c1[1] - c0[1]) * f,
                c0[2] + (c1[2] - c0[2]) * f,
            ];
        }
    }
    stops[stops.len() - 1].1
}