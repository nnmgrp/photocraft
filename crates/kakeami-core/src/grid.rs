//! Hex (staggered-row) lattice and seamless period fitting.

use crate::config::KakeamiConfig;
use serde::Deserialize;

pub const ROW: f64 = 0.8660254; // sqrt(3)/2, same literal as the oracle

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grid {
    pub cell_step: f64,
    pub cell_step_x: f64,
    pub cell_step_y: f64,
    pub row_step: f64,
    pub period_x: u32,
    pub period_y: u32,
    pub seamless: bool,
}

impl Grid {
    pub fn compute(cfg: &KakeamiConfig) -> Self {
        let overlap_factor = (cfg.overlap_density as f64 * 0.01).max(0.5).sqrt();
        let cell_step = (cfg.cluster_size * 0.56 / overlap_factor).max(18.0);

        let mut period_x = 0u32;
        let mut period_y = 0u32;
        let mut cell_step_x = cell_step;
        let mut cell_step_y = cell_step;

        if cfg.seamless {
            period_x = ((cfg.width as f64 / cell_step).round() as i64).max(2) as u32;
            period_y = ((cfg.height as f64 / (cell_step * ROW)).round() as i64).max(2) as u32;
            if period_x & 1 == 1 { period_x += 1; }
            if period_y & 1 == 1 { period_y += 1; }
            period_x = period_x.min(512);
            period_y = period_y.min(512);
            cell_step_x = cfg.width as f64 / period_x as f64;
            cell_step_y = cfg.height as f64 / (period_y as f64 * ROW);
        }

        Self {
            cell_step,
            cell_step_x,
            cell_step_y,
            row_step: ROW * cell_step_y,
            period_x,
            period_y,
            seamless: cfg.seamless,
        }
    }
}
