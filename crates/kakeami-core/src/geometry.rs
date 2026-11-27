//! DOM-free cubic Bézier ribbon builder. Port of `src/02-geometry.js`.
//! All math is f64; only the output vertex buffer is narrowed to f32.

use crate::config::KakeamiConfig;
use crate::density::sample_density;
use crate::geometry_kakeami::{build_kakeami_4, build_kakeami_v8};
use crate::grid::{Grid, Rect, ROW};
use crate::math::{clamp, hash2d, js_round, mod_i, smoothstep, Mulberry32, ValueNoise2D};
use crate::nawa::{build_nawa_ami, build_nawa_skeleton, NawaSkeleton, NawaSkeletonStats};
use std::f64::consts::PI;

#[derive(Debug, Clone)]
pub struct RibbonMeta {
    pub id: u32,
    pub point_count: u32,
    pub stroke_seed: u32,
    /// Point index into `vertices` (multiply by 4 for f32 index).
    pub offset: u32,
    pub gap_start: f64,
    pub gap_end: f64,
    /// Painter's order fill role: `0 = ink` (black), `1 = occluder` (white).
    pub role: u8,
}

#[derive(Debug, Clone)]
pub struct RibbonBatch {
    pub vertices: Vec<f32>,
    pub meta: Vec<RibbonMeta>,
    pub indices: Vec<u32>,
    pub point_count: usize,
}

/// Immutable per-frame precomputed scene (`Send + Sync`, zero global mutable caches).
/// Prepare once per canvas/config and reuse concurrently across any number of tiles.
/// Per-stream Strip Poisson-Disc `Swarm` dot caches live inside `nawa_skeleton` via
/// `OnceLock`, so each stream is computed at most once per frame across all tiles.
pub struct PreparedScene {
    pub grid: Grid,
    pub noise: ValueNoise2D,
    pub(crate) nawa_skeleton: Option<NawaSkeleton>,
}

impl PreparedScene {
    pub fn prepare(cfg: &KakeamiConfig) -> Self {
        let grid = Grid::compute(cfg);
        let (px, py) = if cfg.seamless {
            (grid.period_x >> 1, grid.period_y >> 1)
        } else {
            (0, 0)
        };
        let noise = ValueNoise2D::new(cfg.seed, px, py);
        let nawa_skeleton = if cfg.pattern_family == "nawa-ami" {
            Some(build_nawa_skeleton(cfg))
        } else {
            None
        };
        Self {
            grid,
            noise,
            nawa_skeleton,
        }
    }

    pub fn nawa_skeleton_stats(&self) -> Option<&NawaSkeletonStats> {
        self.nawa_skeleton.as_ref().map(|s| &s.stats)
    }

    /// Number of `nawa-ami` streams whose lazy `OnceLock` Strip Poisson-Disc `Swarm` cache is populated.
    pub fn nawa_swarm_cache_populated(&self) -> usize {
        self.nawa_skeleton
            .as_ref()
            .map(|s| s.populated_swarm_cache_count())
            .unwrap_or(0)
    }

    pub fn build_ribbons(&self, cfg: &KakeamiConfig, rect: Rect) -> RibbonBatch {
        let mut b = Builder::new();
        match cfg.pattern_family.as_str() {
            "kakeami-4" => build_kakeami_4(&mut b, cfg, &self.grid, &self.noise, &rect),
            "kakeami-v8" => build_kakeami_v8(&mut b, cfg, &self.grid, &self.noise, &rect),
            "nawa-ami" => {
                if let Some(sk) = self.nawa_skeleton.as_ref() {
                    build_nawa_ami(&mut b, cfg, &self.grid, &self.noise, &rect, sk);
                } else {
                    let sk = build_nawa_skeleton(cfg);
                    build_nawa_ami(&mut b, cfg, &self.grid, &self.noise, &rect, &sk);
                }
            }
            _ => build_cluster(&mut b, cfg, &self.grid, &self.noise, rect),
        }
        b.finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PolylinePoint {
    pub x: f64,
    pub y: f64,
    pub nx: f64,
    pub ny: f64,
    pub hw: f64,
}

pub(crate) struct Builder {
    pub(crate) scratch: Vec<f32>,
    pub(crate) used: usize,
    pub(crate) meta: Vec<RibbonMeta>,
    pub(crate) indices: Vec<u32>,
}

impl Builder {
    pub(crate) fn new() -> Self {
        Self { scratch: vec![0.0; 1 << 17], used: 0, meta: Vec::new(), indices: Vec::new() }
    }

    pub(crate) fn ensure(&mut self, n: usize) {
        if n <= self.scratch.len() { return; }
        let mut cap = self.scratch.len();
        while cap < n { cap *= 2; }
        self.scratch.resize(cap, 0.0);
    }

    pub(crate) fn finish(mut self) -> RibbonBatch {
        self.scratch.truncate(self.used);
        RibbonBatch {
            vertices: self.scratch,
            meta: self.meta,
            indices: self.indices,
            point_count: self.used / 4,
        }
    }
}

#[inline]
fn bez(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let s = 1.0 - t;
    s * s * s * p0 + 3.0 * s * s * t * p1 + 3.0 * s * t * t * p2 + t * t * t * p3
}

#[inline]
fn dbez(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let s = 1.0 - t;
    3.0 * s * s * (p1 - p0) + 6.0 * s * t * (p2 - p1) + 3.0 * t * t * (p3 - p2)
}

#[allow(clippy::too_many_arguments)]
fn emit_stroke(
    b: &mut Builder, cfg: &KakeamiConfig,
    x0: f64, y0: f64, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64,
    width: f64, beta: f64, has_break: bool, ga: f64, gb: f64,
    stroke_seed: u32, rect: &Rect,
) {
    emit_stroke_ex(b, cfg, x0, y0, x1, y1, x2, y2, x3, y3, width, beta, has_break, ga, gb, stroke_seed, rect, 0, 1.0, None);
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_stroke_ex(
    b: &mut Builder, cfg: &KakeamiConfig,
    x0: f64, y0: f64, x1: f64, y1: f64, x2: f64, y2: f64, x3: f64, y3: f64,
    width: f64, beta: f64, has_break: bool, ga: f64, gb: f64,
    stroke_seed: u32, rect: &Rect, role: u8, alpha_scale: f64, samples: Option<usize>,
) {
    let alpha = cfg.nib_taper as f64 * 0.01 * alpha_scale;
    let expo = cfg.taper_exponent;
    let half_w = width * 0.5;

    let segs: [(f64, f64); 2] = if has_break { [(0.0, ga), (gb, 1.0)] } else { [(0.0, 1.0), (0.0, 1.0)] };
    let nseg = if has_break { 2 } else { 1 };

    for &(u0, u1) in segs.iter().take(nseg) {
        let span = u1 - u0;
        if span <= 1e-4 { continue; }

        let n = match samples {
            Some(ov) if ov > 0 => ov.max(3),
            _ => js_round(cfg.ribbon_samples as f64 * span).max(3.0) as usize,
        };
        let gap_at_start = u0 > 1e-6;
        let gap_at_end = u1 < 1.0 - 1e-6;
        let base = b.used;
        let first_point = base / 4;
        b.ensure(base + n * 4);

        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;

        for i in 0..n {
            let t = u0 + span * (i as f64 / (n - 1) as f64);
            let px = bez(x0, x1, x2, x3, t);
            let py = bez(y0, y1, y2, y3, t);

            let mut tx = dbez(x0, x1, x2, x3, t);
            let mut ty = dbez(y0, y1, y2, y3, t);
            let mut len = (tx * tx + ty * ty).sqrt();
            if len < 1e-9 {
                tx = x3 - x0;
                ty = y3 - y0;
                len = (tx * tx + ty * ty).sqrt();
                if len < 1e-9 { tx = 1.0; ty = 0.0; len = 1.0; }
            }
            tx /= len;
            ty /= len;
            let nxv = -ty;
            let nyv = tx;

            let tt = t.powf(beta);
            let env = (1.0 - alpha) + alpha * (PI * tt).sin().powf(expo);

            let s_local = (t - u0) / span;
            let mut fade = 1.0f64;
            if gap_at_start { fade *= smoothstep(0.0, 0.18, s_local); }
            if gap_at_end { fade *= smoothstep(1.0, 0.82, s_local); }

            let hw = half_w * env * fade;
            let lx = px + nxv * hw;
            let ly = py + nyv * hw;
            let rx = px - nxv * hw;
            let ry = py - nyv * hw;

            let k = base + i * 4;
            b.scratch[k] = lx as f32;
            b.scratch[k + 1] = ly as f32;
            b.scratch[k + 2] = rx as f32;
            b.scratch[k + 3] = ry as f32;

            min_x = min_x.min(lx.min(rx));
            max_x = max_x.max(lx.max(rx));
            min_y = min_y.min(ly.min(ry));
            max_y = max_y.max(ly.max(ry));
        }

        if max_x < rect.x - 2.0 || min_x > rect.x + rect.w + 2.0
            || max_y < rect.y - 2.0 || min_y > rect.y + rect.h + 2.0
        {
            continue;
        }

        b.meta.push(RibbonMeta {
            id: b.meta.len() as u32,
            point_count: n as u32,
            stroke_seed,
            offset: first_point as u32,
            gap_start: u0,
            gap_end: u1,
            role,
        });

        let ib = first_point * 2;
        for q in 0..n - 1 {
            let a = ib + q * 2;
            let c = a + 2;
            b.indices.extend_from_slice(&[a as u32, (a + 1) as u32, c as u32,
                                          (a + 1) as u32, (c + 1) as u32, c as u32]);
        }
        b.used += n * 4;
    }
}

pub(crate) fn emit_polyline(
    b: &mut Builder,
    points: &[PolylinePoint],
    role: u8,
    rect: &Rect,
    stroke_seed: u32,
) {
    let n = points.len();
    if n < 2 {
        return;
    }

    let base = b.used;
    let first_point = base / 4;
    b.ensure(base + n * 4);

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for (i, p) in points.iter().enumerate() {
        let lx = p.x + p.nx * p.hw;
        let ly = p.y + p.ny * p.hw;
        let rx = p.x - p.nx * p.hw;
        let ry = p.y - p.ny * p.hw;

        let k = base + i * 4;
        b.scratch[k] = lx as f32;
        b.scratch[k + 1] = ly as f32;
        b.scratch[k + 2] = rx as f32;
        b.scratch[k + 3] = ry as f32;

        if lx < min_x { min_x = lx; }
        if rx < min_x { min_x = rx; }
        if lx > max_x { max_x = lx; }
        if rx > max_x { max_x = rx; }
        if ly < min_y { min_y = ly; }
        if ry < min_y { min_y = ry; }
        if ly > max_y { max_y = ly; }
        if ry > max_y { max_y = ry; }
    }

    if max_x < rect.x - 2.0
        || min_x > rect.x + rect.w + 2.0
        || max_y < rect.y - 2.0
        || min_y > rect.y + rect.h + 2.0
    {
        return;
    }

    b.meta.push(RibbonMeta {
        id: b.meta.len() as u32,
        point_count: n as u32,
        stroke_seed,
        offset: first_point as u32,
        gap_start: 0.0,
        gap_end: 1.0,
        role,
    });

    let ib = first_point * 2;
    for q in 0..(n - 1) {
        let a = (ib + q * 2) as u32;
        let c = a + 2;
        b.indices.extend_from_slice(&[a, a + 1, c, a + 1, c + 1, c]);
    }
    b.used += n * 4;
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_dot_capsule(
    b: &mut Builder,
    rect: &Rect,
    cx: f64,
    cy: f64,
    tx: f64,
    ty: f64,
    nx: f64,
    ny: f64,
    half_mid: f64,
    rx: f64,
    ry: f64,
    ang_jit: f64,
    role: u8,
    stroke_seed: u32,
) {
    let r_max = rx.max(ry);
    let ext = half_mid + r_max * 1.1;
    if cx + ext < rect.x - 2.0
        || cx - ext > rect.x + rect.w + 2.0
        || cy + ext < rect.y - 2.0
        || cy - ext > rect.y + rect.h + 2.0
    {
        return;
    }

    let (rtx, rty, rnx, rny) = if ang_jit != 0.0 {
        let ca = ang_jit.cos();
        let sa = ang_jit.sin();
        let ntx = tx * ca - ty * sa;
        let nty = tx * sa + ty * ca;
        (ntx, nty, -nty, ntx)
    } else {
        (tx, ty, nx, ny)
    };

    let base = b.used;
    let first_point = base / 4;
    b.ensure(base + 16);

    let u0 = -(half_mid + 0.974 * rx);
    let u1 = -(half_mid + 0.404 * rx);
    let u2 = half_mid + 0.404 * rx;
    let u3 = half_mid + 0.974 * rx;
    let w_end = 0.404 * ry;
    let w_mid = 0.974 * ry;

    let px0 = cx + rtx * u0;
    let py0 = cy + rty * u0;
    let dx0 = rnx * w_end;
    let dy0 = rny * w_end;
    b.scratch[base] = (px0 + dx0) as f32;
    b.scratch[base + 1] = (py0 + dy0) as f32;
    b.scratch[base + 2] = (px0 - dx0) as f32;
    b.scratch[base + 3] = (py0 - dy0) as f32;

    let px1 = cx + rtx * u1;
    let py1 = cy + rty * u1;
    let dx1 = rnx * w_mid;
    let dy1 = rny * w_mid;
    b.scratch[base + 4] = (px1 + dx1) as f32;
    b.scratch[base + 5] = (py1 + dy1) as f32;
    b.scratch[base + 6] = (px1 - dx1) as f32;
    b.scratch[base + 7] = (py1 - dy1) as f32;

    let px2 = cx + rtx * u2;
    let py2 = cy + rty * u2;
    b.scratch[base + 8] = (px2 + dx1) as f32;
    b.scratch[base + 9] = (py2 + dy1) as f32;
    b.scratch[base + 10] = (px2 - dx1) as f32;
    b.scratch[base + 11] = (py2 - dy1) as f32;

    let px3 = cx + rtx * u3;
    let py3 = cy + rty * u3;
    b.scratch[base + 12] = (px3 + dx0) as f32;
    b.scratch[base + 13] = (py3 + dy0) as f32;
    b.scratch[base + 14] = (px3 - dx0) as f32;
    b.scratch[base + 15] = (py3 - dy0) as f32;

    b.meta.push(RibbonMeta {
        id: b.meta.len() as u32,
        point_count: 4,
        stroke_seed,
        offset: first_point as u32,
        gap_start: 0.0,
        gap_end: 1.0,
        role,
    });

    let ib = (first_point * 2) as u32;
    b.indices.extend_from_slice(&[
        ib, ib + 1, ib + 2,
        ib + 1, ib + 3, ib + 2,
        ib + 2, ib + 3, ib + 4,
        ib + 3, ib + 5, ib + 4,
        ib + 4, ib + 5, ib + 6,
        ib + 5, ib + 7, ib + 6,
    ]);
    b.used += 16;
}

#[allow(clippy::too_many_arguments)]
fn build_hatch_pass(
    b: &mut Builder, cfg: &KakeamiConfig,
    cx: f64, cy: f64, angle: f64, local_density: f64, pass_seed: u32, rect: &Rect,
) {
    let mut rng = Mulberry32::new(pass_seed);

    let tighten = if local_density > 0.76 { 1.0 - (local_density - 0.76) * 0.45 } else { 1.0 };
    let spacing = (cfg.line_spacing * tighten).max(2.6);

    let lo = cfg.min_lines.min(cfg.max_lines);
    let hi = cfg.min_lines.max(cfg.max_lines);
    let span_limit = ((cfg.cluster_size * 0.92) / spacing).round().max(2.0) as u32;
    let eff_hi = lo.max(hi.min(span_limit));
    let target = lo as f64 + (eff_hi - lo) as f64 * (local_density * 1.15).min(1.0);
    let jitter_lines = (rng.next_f64() - 0.5) * 1.4;
    let line_count = ((target + jitter_lines).round() as i64).max(1) as u32;

    let total_span = (line_count - 1) as f64 * spacing;
    let half_span = total_span * 0.5;

    let wobble_amt = cfg.wobble as f64 * 0.01 * cfg.cluster_size * 0.095;
    let broken_amt = cfg.broken_ink as f64 * 0.01;
    let jitter_norm = cfg.scatter_jitter as f64 * 0.01;
    let rand_norm = cfg.angle_randomness as f64 * 0.01;
    let fan_max = cfg.fan_spread * PI / 180.0;

    let wave_phase = rng.next_f64() * PI * 2.0;
    let wave_dir = if rng.next_f64() < 0.5 { 1.0 } else { -1.0 };

    for k in 0..line_count {
        let r_spacing = rng.next_f64();
        let r_len = rng.next_f64();
        let r_along = rng.next_f64();
        let r_wobble1 = rng.next_f64();
        let r_wobble2 = rng.next_f64();
        let r_break = rng.next_f64();
        let r_gap_pos = rng.next_f64();
        let r_gap_size = rng.next_f64();
        let r_width_delta = rng.next_f64();
        let r_taper_peak = rng.next_f64();
        let r_fan = rng.next_f64();

        let norm_pos = if line_count > 1 {
            ((k as f64 / (line_count - 1) as f64) * 2.0 - 1.0).abs()
        } else { 0.0 };
        let edge_taper = 1.0 - norm_pos * norm_pos * (0.14 + jitter_norm * 0.22);

        let spacing_jitter = (r_spacing - 0.5) * spacing * (0.06 + jitter_norm * 0.34);
        let line_offset = -half_span + k as f64 * spacing + spacing_jitter;

        let len_min = 0.88 - jitter_norm * 0.38;
        let len_range = 0.24 + jitter_norm * 0.62;
        let half_len = cfg.cluster_size * 0.5 * edge_taper * (len_min + r_len * len_range);
        let along_jitter = (r_along - 0.5) * cfg.cluster_size * (0.05 + jitter_norm * 0.36);

        let fan_rad = (r_fan - 0.5) * 2.0 * fan_max * (0.4 + rand_norm * 0.6);
        let line_angle = angle + fan_rad;
        let lc = line_angle.cos();
        let ls = line_angle.sin();

        let slant_shift = (k as f64 - (line_count - 1) as f64 * 0.5)
            * cfg.cluster_slant * spacing * 0.5;

        let lx1 = -half_len + along_jitter + slant_shift;
        let lx2 = half_len + along_jitter + slant_shift;

        let x0 = cx + lx1 * lc - line_offset * ls;
        let y0 = cy + lx1 * ls + line_offset * lc;
        let x3 = cx + lx2 * lc - line_offset * ls;
        let y3 = cy + lx2 * ls + line_offset * lc;

        let (x1, y1, x2, y2) = if wobble_amt <= 0.05 {
            (
                x0 + (x3 - x0) * 0.33, y0 + (y3 - y0) * 0.33,
                x0 + (x3 - x0) * 0.67, y0 + (y3 - y0) * 0.67,
            )
        } else {
            let w1 = (wave_phase + k as f64 * 0.36).sin() * wave_dir;
            let w2 = (wave_phase + k as f64 * 0.36 + 0.85).cos() * wave_dir;
            let d1 = (w1 * 0.62 + (r_wobble1 - 0.5) * 0.85) * wobble_amt;
            let d2 = (w2 * 0.62 + (r_wobble2 - 0.5) * 0.85) * wobble_amt;
            let c1x = lx1 + (lx2 - lx1) * 0.33;
            let c1y = line_offset + d1;
            let c2x = lx1 + (lx2 - lx1) * 0.67;
            let c2y = line_offset + d2;
            (
                cx + c1x * lc - c1y * ls, cy + c1x * ls + c1y * lc,
                cx + c2x * lc - c2y * ls, cy + c2x * ls + c2y * lc,
            )
        };

        let has_break = broken_amt > 0.02 && r_break < 0.28 + broken_amt * 0.68;
        let gap_center = 0.16 + r_gap_pos * 0.68;
        let gap_half = 0.02 + r_gap_size * 0.06 * broken_amt;
        let ga = clamp(gap_center - gap_half, 0.02, 0.98);
        let gb = clamp(gap_center + gap_half, 0.02, 0.98);

        let stroke_w = cfg.stroke_width * (1.0 + (r_width_delta - 0.5) * 0.24);
        let t_peak = 0.35 + r_taper_peak * 0.3;
        let beta = (0.5f64).ln() / t_peak.ln();

        emit_stroke(b, cfg, x0, y0, x1, y1, x2, y2, x3, y3,
            stroke_w, beta, has_break, ga, gb, hash2d(pass_seed, k as i32, 7), rect);
    }
}

fn pass_angles(mode: &str, layers: u32, a1: f64, cross: f64, sign: f64) -> Vec<f64> {
    let tri = cross * (2.0 / 3.0);
    if mode == "tutorial" {
        return match layers {
            1 => vec![a1],
            2 => vec![a1, a1 + sign * cross],
            3 => vec![a1, a1 + tri, a1 + tri * 2.0],
            _ => vec![a1, a1 + cross, a1 + cross * 0.5, a1 + cross * 1.5],
        };
    }
    match layers {
        1 => vec![a1],
        2 => vec![a1, a1 + sign * cross],
        3 => vec![a1, a1 + sign * cross, a1 + cross * 0.48],
        _ => vec![a1, a1 + sign * cross, a1 + cross * 0.48, a1 - cross * 0.48],
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cluster(
    b: &mut Builder, cfg: &KakeamiConfig, grid: &Grid,
    cx: f64, cy: f64, gx: i32, gy: i32, d: f64, cell_seed: u32,
    rng: &mut Mulberry32, rect: &Rect,
) {
    let base_rad = cfg.base_angle * PI / 180.0;
    let cross_rad = cfg.cross_angle as f64 * PI / 180.0;
    let rand_norm = cfg.angle_randomness as f64 * 0.01;
    let parquet_norm = cfg.parquet_weave as f64 * 0.01;

    let r_angle1 = rng.next_f64();
    let r_layer = rng.next_f64();
    let r_s2_sign = rng.next_f64();
    let r_s2_jit = rng.next_f64();
    let r_s3a_jit = rng.next_f64();
    let _r_s3b_jit = rng.next_f64(); // drawn for RNG order, intentionally unused
    let r_s4_jit = rng.next_f64();
    let r_s4_jx = rng.next_f64();
    let r_s4_jy = rng.next_f64();
    let r_parquet_flip = rng.next_f64();

    let domain_size = if grid.seamless {
        2
    } else {
        2 + (hash2d(cfg.seed ^ 0x51ed, gx.div_euclid(6), gy.div_euclid(6)) % 3) as i32
    };
    let domain_val = hash2d(cfg.seed ^ 0x9e37_79b9, gx.div_euclid(domain_size), gy.div_euclid(domain_size));
    let mut flip = (domain_val >> 11) & 1 == 1;
    if r_parquet_flip < 0.2 { flip = !flip; }
    let parquet_offset = if flip { parquet_norm * PI * 0.5 } else { 0.0 };

    let max_spread_rad = PI * (0.03 + rand_norm * 0.97);
    let angle1 = base_rad + parquet_offset + (r_angle1 - 0.5) * 2.0 * max_spread_rad;
    let pass_jitter_rad = 0.03 + rand_norm * 0.28;

    let forced = if cfg.preset == "tutorial-sheet" {
        ((cx / cfg.width as f64 * 4.0).floor() as i32 + 1).clamp(1, 4) as u32
    } else if cfg.layer_cap == "auto" {
        0
    } else {
        cfg.layer_cap.parse::<u32>().unwrap_or(0).min(4)
    };

    let layers = if forced > 0 {
        forced
    } else {
        let p1 = smoothstep(0.015, 0.15, d);
        let p2 = smoothstep(0.14, 0.28, d);
        let p3 = smoothstep(0.40, 0.56, d);
        let p4 = smoothstep(0.68, 0.84, d);
        let mut l = 0;
        if r_layer < p1 { l += 1; }
        if r_layer < p2 { l += 1; }
        if r_layer < p3 { l += 1; }
        if r_layer < p4 { l += 1; }
        if l == 0 { return; }
        l
    };

    let sign = if r_s2_sign < 0.5 { 1.0 } else { -1.0 };
    let angles = pass_angles(&cfg.geometry_mode, layers, angle1, cross_rad, sign);
    let jitters = [r_s2_jit, r_s3a_jit, r_s4_jit];

    for (i, angle) in angles.iter().enumerate() {
        let jitter = if i == 0 { 0.0 } else { (jitters[i - 1] - 0.5) * pass_jitter_rad };
        let mut px = cx;
        let mut py = cy;
        if i == 3 {
            px += (r_s4_jx - 0.5) * cfg.cluster_size * 0.16;
            py += (r_s4_jy - 0.5) * cfg.cluster_size * 0.16;
        }
        build_hatch_pass(b, cfg, px, py, angle + jitter, d,
            hash2d(cell_seed, (i + 1) as i32, (101 * (i + 1)) as i32), rect);
    }
}

/// Single-shot ribbon builder (legacy & standalone convenience API).
///
/// For tiled rendering (Phase 5 CPU rasterizer, `rayon` parallel tiles, or WASM),
/// prefer [`PreparedScene::prepare`] once per frame followed by [`PreparedScene::build_ribbons`]
/// per tile so the `nawa-ami` skeleton and per-stream `OnceLock` `Swarm` caches are reused.
pub fn build_ribbons(cfg: &KakeamiConfig, grid: &Grid, noise: &ValueNoise2D, rect: Rect) -> RibbonBatch {
    let mut b = Builder::new();
    match cfg.pattern_family.as_str() {
        "kakeami-4" => build_kakeami_4(&mut b, cfg, grid, noise, &rect),
        "kakeami-v8" => build_kakeami_v8(&mut b, cfg, grid, noise, &rect),
        "nawa-ami" => {
            let sk = build_nawa_skeleton(cfg);
            build_nawa_ami(&mut b, cfg, grid, noise, &rect, &sk);
        }
        _ => build_cluster(&mut b, cfg, grid, noise, rect),
    }
    b.finish()
}

fn build_cluster(b: &mut Builder, cfg: &KakeamiConfig, grid: &Grid, noise: &ValueNoise2D, rect: Rect) {

    let bleed = cfg.cluster_size * 0.95 + cfg.line_spacing * 5.0 + cfg.stroke_width * 2.0 + 16.0;
    let x0 = rect.x - bleed;
    let x1 = rect.x + rect.w + bleed;
    let y0 = rect.y - bleed;
    let y1 = rect.y + rect.h + bleed;

    let gx_min = (x0 / grid.cell_step_x).floor() as i64 - 1;
    let gx_max = (x1 / grid.cell_step_x).floor() as i64 + 1;
    let gy_min = (y0 / grid.row_step).floor() as i64 - 1;
    let gy_max = (y1 / grid.row_step).floor() as i64 + 1;

    for gy in gy_min..=gy_max {
        let row_offset = if gy & 1 == 1 { 0.5 } else { 0.0 };
        let wy = if grid.seamless { mod_i(gy, grid.period_y as i64) as i32 } else { gy as i32 };
        for gx in gx_min..=gx_max {
            let wx = if grid.seamless { mod_i(gx, grid.period_x as i64) as i32 } else { gx as i32 };
            let cell_seed = hash2d(cfg.seed, wx, wy);
            let mut rng = Mulberry32::new(cell_seed);
            let jx = rng.next_f64();
            let jy = rng.next_f64();
            let cx = (gx as f64 + row_offset + 0.12 + jx * 0.76) * grid.cell_step_x;
            let cy = (gy as f64 * ROW + 0.12 + jy * 0.76) * grid.cell_step_y;

            if cx < x0 || cx > x1 || cy < y0 || cy > y1 { continue; }
            let d = sample_density(cfg, grid, cx, cy, noise);
            if d < 0.015 { continue; }
            emit_cluster(b, cfg, grid, cx, cy, wx, wy, d, cell_seed, &mut rng, &rect);
        }
    }
}
