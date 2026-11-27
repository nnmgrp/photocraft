//! Continuous density field d(x, y) in [0, 1].
//! Exact port of `KDensity.sample` in `src/01-core.js` (including v3/v7/v8 presets).

use crate::config::KakeamiConfig;
use crate::grid::Grid;
use crate::math::{clamp, mod_f64, smoothstep, ValueNoise2D};

/// Calibration ladder M95%..M5%.
pub const RAMP_9: [f64; 9] = [0.95, 0.80, 0.65, 0.50, 0.40, 0.30, 0.20, 0.10, 0.05];

/// White-nawa calibration ladder M60%..M5%.
pub const RAMP_WHITE: [f64; 7] = [0.60, 0.50, 0.40, 0.30, 0.20, 0.10, 0.05];

const BOKEH_SPOTS: [(f64, f64, f64, f64); 6] = [
    (0.24, 0.22, 0.045, 0.145),
    (0.68, 0.25, 0.055, 0.165),
    (0.42, 0.52, 0.065, 0.190),
    (0.78, 0.62, 0.040, 0.135),
    (0.22, 0.76, 0.050, 0.160),
    (0.58, 0.82, 0.035, 0.125),
];

pub fn sample_density(cfg: &KakeamiConfig, grid: &Grid, x: f64, y: f64, noise: &ValueNoise2D) -> f64 {
    let w_f = cfg.width as f64;
    let h_f = cfg.height as f64;

    if cfg.seamless {
        // Exact floating-point wrap (matches JS `mod(x, cfg.width)` on fractional coords).
        let wx = mod_f64(x, w_f);
        let wy = mod_f64(y, h_f);
        let nx = wx / grid.cell_step_x;
        let ny = wy / grid.cell_step_y;
        let w = (noise.sample(nx * 0.5, ny * 0.5) - 0.5) * 0.1;
        return clamp(cfg.darken_cap as f64 * 0.01 + w, 0.0, 1.0);
    }

    let wx = x;
    let wy = y;
    let nx2 = clamp(wx / w_f, 0.0, 1.0);
    let ny2 = clamp(wy / h_f, 0.0, 1.0);

    if cfg.preset == "ramp-9" {
        let col = ((wx / w_f) * 9.0).floor() as i64;
        let col = col.clamp(0, 8) as usize;
        return RAMP_9[col];
    }

    if cfg.preset == "ramp-white" {
        let wcol = ((wx / w_f) * 7.0).floor() as i64;
        let wcol = wcol.clamp(0, 6) as usize;
        return RAMP_WHITE[wcol];
    }

    if cfg.preset == "bokeh" {
        let min_dim = (cfg.width.min(cfg.height)).max(1) as f64;
        let asp = w_f / cfg.height.max(1) as f64;
        let b_wave = (noise.sample(nx2 * 4.5 * asp.max(1.0), ny2 * 4.5 * (1.0 / asp).max(1.0)) - 0.5) * 0.04;
        let base_db = clamp((0.97 - 0.03 * nx2 + b_wave) * (cfg.darken_cap as f64 * 0.01), 0.0, 1.0);
        let mut b_mul = 1.0f64;
        for &(sx_b, sy_b, r_in, r_out) in &BOKEH_SPOTS {
            let dx_b = (wx - sx_b * w_f) / min_dim;
            let dy_b = (wy - sy_b * h_f) / min_dim;
            let dist_b = (dx_b * dx_b + dy_b * dy_b).sqrt();
            if dist_b <= r_in {
                return 0.0;
            }
            if dist_b < r_out {
                b_mul *= smoothstep(r_in, r_out, dist_b);
            }
        }
        return clamp(base_db * b_mul, 0.0, 1.0);
    }

    let aspect = w_f / cfg.height.max(1) as f64;
    let sx = nx2 * 4.5 * aspect.max(1.0);
    let sy = ny2 * 4.5 * (1.0 / aspect).max(1.0);
    let wave = (noise.sample(sx, sy) - 0.5) * 0.065
        + (noise.sample(sx * 2.6 + 3.1, sy * 2.6 + 1.7) - 0.5) * 0.03;

    if cfg.preset == "isolated-5" {
        let dc = if cfg.darken_cap == 0 { 40.0 } else { cfg.darken_cap as f64 };
        return clamp(dc * 0.01, 0.05, 1.0);
    }

    if cfg.preset == "tutorial-sheet" {
        let col = (nx2 * 4.0).floor() as i64;
        let col = col.clamp(0, 3) as usize;
        let targets = [0.14f64, 0.34, 0.58, 0.88];
        let sub_grad = (1.0 - ny2 - 0.5) * 0.14 + wave * 0.6;
        let scale = 0.55 + (cfg.darken_cap as f64 / 100.0) * 0.55;
        return ((targets[col] + sub_grad) * scale).clamp(0.05, 1.0);
    }

    let raw_axis = match cfg.preset.as_str() {
        "linear-bt" => 1.0 - ny2 + wave,
        "linear-tb" => ny2 + wave,
        "linear-lr" => nx2 + wave,
        "radial" => {
            let dx = (nx2 - 0.5) * 2.0;
            let dy = (ny2 - 0.5) * 2.0;
            (dx * dx + dy * dy).sqrt() / 1.25 + wave
        }
        _ => {
            let n1 = noise.sample(sx * 0.9 + 1.2, sy * 0.9 + 0.8);
            let n2 = noise.sample(sx * 2.2 + 5.4, sy * 2.2 + 2.3);
            let n3 = noise.sample(sx * 4.8 + 9.1, sy * 4.8 + 4.7);
            n1 * 0.56 + n2 * 0.30 + n3 * 0.14
        }
    };

    let s0 = cfg.grad_start as f64 * 0.01;
    let s1 = (s0 + 0.02).max(cfg.grad_end as f64 * 0.01);
    let t = clamp((raw_axis - s0) / (s1 - s0), 0.0, 1.0);
    let d = t.powf(cfg.gamma) * (cfg.darken_cap as f64 * 0.01);
    d.clamp(0.0, 1.0)
}
