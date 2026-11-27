//! Global deterministic `nawa-ami` stream skeleton (XP-Pipes `skein` + legacy `arcs`).
//! 1:1 port of `buildStreamChordGeometry`, `buildLegacyArcsSkeleton`, and
//! `buildNawaStreamSkeleton` from `src/02-geometry.js` (lines 946–2423).
//!
//! Strictly `Send + Sync` with zero global mutable state.

use crate::config::KakeamiConfig;
use crate::grid::ROW;
use crate::math::{clamp, hash2d, js_round, mod_f64, mod_i, smoothstep, Mulberry32};
use crate::nawa::swarm::StreamSwarmCache;
use std::f64::consts::PI;
use std::sync::OnceLock;

const TAU: f64 = PI * 2.0;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Pt {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Chord {
    pub len: f64,
    pub tx: f64,
    pub ty: f64,
    pub nx: f64,
    pub ny: f64,
    pub mx: f64,
    pub my: f64,
    #[allow(dead_code)]
    pub k: f64,
}

#[derive(Debug, Clone)]
pub struct StreamGeom {
    pub(crate) pts: Vec<Pt>,
    pub(crate) chords: Vec<Chord>,
    pub(crate) m: usize,
    pub(crate) cum_arc: Vec<f64>,
    pub(crate) total_arc: f64,
    pub(crate) d_theta: Vec<f64>,
    pub(crate) half_tan: Vec<f64>,
    pub(crate) miter_nx: Vec<f64>,
    pub(crate) miter_ny: Vec<f64>,
    pub(crate) miter_sec: Vec<f64>,
    pub(crate) is_closed_loop: bool,
    /// Stream-level tucked-end flags (invariant across all Z-chunks and tiles for `Swarm` `OnceLock`).
    pub(crate) head_tucked: bool,
    pub(crate) tail_tucked: bool,
    pub(crate) w_verts: Option<Vec<f64>>,
    pub(crate) k_strands: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct NawaChunk {
    pub(crate) geom_idx: usize,
    pub(crate) ci_from: usize,
    pub(crate) ci_to: usize,
    pub(crate) z_key: u32,
    pub(crate) gy: i64,
    pub(crate) gx: i64,
    pub(crate) ck: usize,
    pub(crate) stream_idx: usize,
    pub(crate) seed: u32,
    pub(crate) clip_start_at_s: bool,
    pub(crate) clip_end_at_s: bool,
    pub(crate) own_start_tail: bool,
    pub(crate) own_end_tail: bool,
    pub(crate) head_tucked: bool,
    pub(crate) tail_tucked: bool,
    pub(crate) shift_x: f64,
    pub(crate) shift_y: f64,
    pub(crate) min_x: f64,
    pub(crate) min_y: f64,
    pub(crate) max_x: f64,
    pub(crate) max_y: f64,
}

#[derive(Debug, Clone, Default)]
pub struct NawaSkeletonStats {
    pub stream_count: usize,
    pub primary_stream_count: usize,
    pub bg_fill_stream_count: usize,
    pub chunk_count: usize,
    pub unhidden_inside_ends: usize,
}

#[derive(Debug, Clone)]
pub struct NawaSkeleton {
    pub(crate) streams: Vec<StreamGeom>,
    pub(crate) chunks: Vec<NawaChunk>,
    /// Thread-safe per-stream lazy `Swarm` dot cache (`Send + Sync`, lock-free after init).
    pub(crate) swarm_cache: Vec<OnceLock<StreamSwarmCache>>,
    pub stats: NawaSkeletonStats,
}

impl NawaSkeleton {
    /// Number of streams whose Strip Poisson-Disc `Swarm` cache has been populated so far.
    pub fn populated_swarm_cache_count(&self) -> usize {
        self.swarm_cache.iter().filter(|slot| slot.get().is_some()).count()
    }
}

#[inline]
fn dist_2d(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = ax - bx;
    let dy = ay - by;
    (dx * dx + dy * dy).sqrt()
}

#[inline]
fn gcd_abs(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

#[inline]
fn wrap_angle_pi(mut a: f64) -> f64 {
    while a > PI {
        a -= TAU;
    }
    while a < -PI {
        a += TAU;
    }
    a
}

fn build_stream_chord_geometry(pts: Vec<Pt>, is_closed_loop: bool) -> Option<StreamGeom> {
    if pts.len() < 2 {
        return None;
    }
    let m = pts.len() - 1;
    let mut chords = Vec::with_capacity(m);
    let mut cum_arc = vec![0.0f64; m + 1];

    for i in 0..m {
        let mut dx = pts[i + 1].x - pts[i].x;
        let mut dy = pts[i + 1].y - pts[i].y;
        let mut len = (dx * dx + dy * dy).sqrt();
        if len < 1e-6 {
            dx = 1.0;
            dy = 0.0;
            len = 1e-6;
        }
        let tx = dx / len;
        let ty = dy / len;
        chords.push(Chord {
            len,
            tx,
            ty,
            nx: -ty,
            ny: tx,
            mx: 0.5 * (pts[i].x + pts[i + 1].x),
            my: 0.5 * (pts[i].y + pts[i + 1].y),
            k: 0.0,
        });
        cum_arc[i + 1] = cum_arc[i] + len;
    }

    let joint_count = m + 1;
    let mut d_theta = vec![0.0f64; joint_count];
    let mut half_tan = vec![0.0f64; joint_count];
    let mut miter_nx = vec![0.0f64; joint_count];
    let mut miter_ny = vec![0.0f64; joint_count];
    let mut miter_sec = vec![0.0f64; joint_count];

    for j in 0..joint_count {
        let has_prev = is_closed_loop || j > 0;
        let has_next = is_closed_loop || j < m;
        if has_prev && has_next {
            let prev_c = chords[if j == 0 { m - 1 } else { j - 1 }];
            let next_c = chords[if j == m { 0 } else { j }];
            let dot = prev_c.tx * next_c.tx + prev_c.ty * next_c.ty;
            let cross = prev_c.tx * next_c.ty - prev_c.ty * next_c.tx;
            let ang = cross.atan2(dot);
            d_theta[j] = ang;
            half_tan[j] = clamp((ang * 0.5).tan(), -0.80, 0.80);

            let sx = prev_c.nx + next_c.nx;
            let sy = prev_c.ny + next_c.ny;
            let slen = (sx * sx + sy * sy).sqrt();
            if slen < 1e-6 {
                miter_nx[j] = next_c.nx;
                miter_ny[j] = next_c.ny;
            } else {
                miter_nx[j] = sx / slen;
                miter_ny[j] = sy / slen;
            }
            miter_sec[j] = clamp(1.0 / (ang * 0.5).cos().max(0.45), 1.0, 1.35);
        } else {
            let edge_c = chords[if j == 0 { 0 } else { m - 1 }];
            d_theta[j] = 0.0;
            half_tan[j] = 0.0;
            miter_nx[j] = edge_c.nx;
            miter_ny[j] = edge_c.ny;
            miter_sec[j] = 1.0;
        }
    }

    for i in 0..m {
        chords[i].k = 0.5 * (d_theta[i] + d_theta[i + 1]) / chords[i].len.max(1.0);
    }

    let total_arc = cum_arc[m];
    Some(StreamGeom {
        pts,
        chords,
        m,
        cum_arc,
        total_arc,
        d_theta,
        half_tan,
        miter_nx,
        miter_ny,
        miter_sec,
        is_closed_loop,
        head_tucked: false,
        tail_tucked: false,
        w_verts: None,
        k_strands: None,
    })
}

pub fn build_nawa_skeleton(cfg: &KakeamiConfig) -> NawaSkeleton {
    if cfg.nawa_coil_style != "skein" || cfg.preset == "isolated-5" {
        return build_legacy_arcs_skeleton(cfg);
    }
    build_nawa_stream_skeleton(cfg)
}

// =========================================================================
// Legacy Arcs (`nawaCoilStyle === 'arcs'` or `preset === 'isolated-5'`)
// =========================================================================

struct ArcSpec {
    cx: f64,
    cy: f64,
    r: f64,
    l: f64,
    phi0: f64,
    spin: f64,
    shape_kind: i32,
    z_key: u32,
    gx: i64,
    gy: i64,
    seed: u32,
}

fn build_legacy_arcs_skeleton(cfg: &KakeamiConfig) -> NawaSkeleton {
    let n_scale = clamp(cfg.nawa_scale, 0.40, 4.50);
    let w = cfg.nawa_rope_width * n_scale;
    let r_cfg = cfg.nawa_coil_radius * n_scale;
    let overlap_scale = (cfg.overlap_density as f64 / 100.0).max(0.5).sqrt();
    let coil_step = (w * 0.85).max((w * 1.15) / overlap_scale);

    let mut period_x = 0i64;
    let mut period_y = 0i64;
    let mut cell_step_x = coil_step;
    let mut cell_step_y = coil_step;
    let w_f = cfg.width as f64;
    let h_f = cfg.height as f64;

    if cfg.seamless {
        period_x = (js_round(w_f / coil_step) as i64).max(2);
        period_y = (js_round(h_f / (coil_step * ROW)) as i64).max(2);
        if (period_x & 1) != 0 {
            period_x += 1;
        }
        if (period_y & 1) != 0 {
            period_y += 1;
        }
        period_x = period_x.min(512);
        period_y = period_y.min(512);
        cell_step_x = w_f / period_x as f64;
        cell_step_y = h_f / (period_y as f64 * ROW);
    }

    let pad = if cfg.seamless { 0.0 } else { r_cfg * 2.4 + w * 2.5 };
    let gx_min = if cfg.seamless { 0 } else { (-pad / cell_step_x).floor() as i64 - 1 };
    let gx_max = if cfg.seamless { period_x - 1 } else { ((w_f + pad) / cell_step_x).floor() as i64 + 1 };
    let gy_min = if cfg.seamless { 0 } else { (-pad / (ROW * cell_step_y)).floor() as i64 - 1 };
    let gy_max = if cfg.seamless { period_y - 1 } else { ((h_f + pad) / (ROW * cell_step_y)).floor() as i64 + 1 };

    let mut specs: Vec<ArcSpec> = Vec::new();
    if cfg.preset == "isolated-5" {
        let spots: [(f64, f64, i32, f64, f64); 5] = [
            (0.21, 0.30, 1, 0.20, 1.0),
            (0.77, 0.28, 0, 2.55, -1.0),
            (0.37, 0.72, 1, 0.90, 1.0),
            (0.52, 0.69, 0, 2.15, -1.0),
            (0.82, 0.72, 1, 1.55, -1.0),
        ];
        for (si, &(fx, fy, sk, phi0, spin)) in spots.iter().enumerate() {
            specs.push(ArcSpec {
                cx: fx * w_f,
                cy: fy * h_f,
                r: clamp(r_cfg, w * 0.92, w * 2.6),
                l: w * if sk == 1 { 5.1 } else { 5.3 },
                phi0,
                spin,
                shape_kind: sk,
                z_key: si as u32,
                gx: si as i64,
                gy: 0,
                seed: hash2d(cfg.seed ^ 0x4e41_5741, si as i32 + 1, si as i32 * 7 + 3),
            });
        }
    } else {
        for gy in gy_min..=gy_max {
            let row_offset = if (gy & 1) != 0 { 0.5 } else { 0.0 };
            let wy = if cfg.seamless { mod_i(gy, period_y) as i32 } else { gy as i32 };
            for gx in gx_min..=gx_max {
                let wx = if cfg.seamless { mod_i(gx, period_x) as i32 } else { gx as i32 };
                let bx = (gx as f64 + row_offset + 0.12) * cell_step_x;
                let by = (gy as f64 * ROW + 0.12) * cell_step_y;
                let cell_seed = hash2d(cfg.seed ^ 0x4e41_5741, wx, wy);
                let z_key = hash2d(cfg.seed ^ 0x51a9_e217, wx, wy);
                let mut rng = Mulberry32::new(cell_seed);
                let jx = rng.next_f64();
                let jy = rng.next_f64();
                let r_r = rng.next_f64();
                let r_phi = rng.next_f64();
                let r_spin = rng.next_f64();
                let r_shape = rng.next_f64();
                let r_span = rng.next_f64();
                let r_len = rng.next_f64();
                let r = clamp(r_cfg * (0.72 + r_r * 0.52), w * 0.92, w * 2.6);
                let shape_kind = if r_shape < 0.55 { 1 } else { 0 };
                let theta_target = if shape_kind == 1 {
                    (150.0 + r_span * 95.0) * PI / 180.0
                } else {
                    (180.0 + r_span * 90.0) * PI / 180.0
                };
                let l = clamp((r * theta_target).max(w * (4.8 + r_len * 2.4)), w * 4.8, w * 7.2);
                specs.push(ArcSpec {
                    cx: bx + (jx - 0.5) * 0.76 * coil_step,
                    cy: by + (jy - 0.5) * 0.76 * coil_step,
                    r,
                    l,
                    phi0: r_phi * TAU,
                    spin: if r_spin < 0.5 { 1.0 } else { -1.0 },
                    shape_kind,
                    z_key,
                    gx,
                    gy,
                    seed: cell_seed,
                });
            }
        }
    }

    let mut streams: Vec<StreamGeom> = Vec::with_capacity(specs.len());
    let mut chunks: Vec<NawaChunk> = Vec::with_capacity(specs.len());
    for (i, sp) in specs.iter().enumerate() {
        let chord_step = clamp(sp.r * 0.34, w * 0.45, w * 0.72);
        let n = (js_round(sp.l / chord_step) as usize).max(4);
        let step = sp.l / n as f64;
        let half_i = n >> 1;
        let mut pts = vec![Pt { x: 0.0, y: 0.0 }; n + 1];
        pts[half_i] = Pt { x: sp.cx, y: sp.cy };

        let mut ftx = sp.phi0.cos();
        let mut fty = sp.phi0.sin();
        for f in half_i..n {
            let u = (f as f64 - half_i as f64 + 0.5) / n as f64;
            let sgn = if sp.shape_kind == 1 {
                sp.spin * (1.0 - 2.0 * smoothstep(-0.08, 0.08, u))
            } else {
                sp.spin
            };
            let r_eff = if sp.shape_kind == 1 { sp.r } else { sp.r * (1.0 - 0.15 * u) };
            let ang = sgn * (step / r_eff);
            let ca = ang.cos();
            let sa = ang.sin();
            pts[f + 1] = Pt {
                x: pts[f].x + ftx * step,
                y: pts[f].y + fty * step,
            };
            let nfx = ftx * ca - fty * sa;
            let nfy = ftx * sa + fty * ca;
            ftx = nfx;
            fty = nfy;
        }

        let mut btx = sp.phi0.cos();
        let mut bty = sp.phi0.sin();
        for b in (1..=half_i).rev() {
            let ub = (b as f64 - half_i as f64 - 0.5) / n as f64;
            let sgnb = if sp.shape_kind == 1 {
                sp.spin * (1.0 - 2.0 * smoothstep(-0.08, 0.08, ub))
            } else {
                sp.spin
            };
            let r_effb = if sp.shape_kind == 1 { sp.r } else { sp.r * (1.0 - 0.15 * ub) };
            let angb = -sgnb * (step / r_effb);
            let cab = angb.cos();
            let sab = angb.sin();
            let nbx = btx * cab - bty * sab;
            let nby = btx * sab + bty * cab;
            btx = nbx;
            bty = nby;
            pts[b - 1] = Pt {
                x: pts[b].x - btx * step,
                y: pts[b].y - bty * step,
            };
        }

        let Some(geom) = build_stream_chord_geometry(pts, false) else {
            continue;
        };
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for p in &geom.pts {
            if p.x < min_x { min_x = p.x; }
            if p.x > max_x { max_x = p.x; }
            if p.y < min_y { min_y = p.y; }
            if p.y > max_y { max_y = p.y; }
        }
        let mrg = w * 1.4;
        let m_ch = geom.m;
        let geom_idx = streams.len();
        streams.push(geom);
        chunks.push(NawaChunk {
            geom_idx,
            ci_from: 0,
            ci_to: m_ch - 1,
            z_key: sp.z_key,
            gy: sp.gy,
            gx: sp.gx,
            ck: 0,
            stream_idx: i,
            seed: sp.seed,
            clip_start_at_s: false,
            clip_end_at_s: false,
            own_start_tail: false,
            own_end_tail: false,
            head_tucked: false,
            tail_tucked: false,
            shift_x: 0.0,
            shift_y: 0.0,
            min_x: min_x - mrg,
            min_y: min_y - mrg,
            max_x: max_x + mrg,
            max_y: max_y + mrg,
        });
    }

    chunks.sort_unstable_by_key(|c| (c.z_key, c.gy, c.gx));
    let chunk_count = chunks.len();
    let swarm_cache = (0..streams.len()).map(|_| OnceLock::new()).collect();
    NawaSkeleton {
        streams,
        chunks,
        swarm_cache,
        stats: NawaSkeletonStats {
            stream_count: specs.len(),
            primary_stream_count: specs.len(),
            bg_fill_stream_count: 0,
            chunk_count,
            unhidden_inside_ends: 0,
        },
    }
}

// =========================================================================
// Continuous XP-Pipes Stream Skeleton (`nawaCoilStyle === 'skein'`)
// =========================================================================

struct RawStreamMeta {
    idx: usize,
    geom_idx: usize,
    seed: u32,
    closed_loop: bool,
    bg_fill: bool,
    parity: usize,
}

#[derive(Clone, Copy)]
struct AxisSeg {
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    mx: f64,
    my: f64,
    s: usize,
    ci: usize,
    w_loc: f64,
}

struct StreamPlan {
    st_idx: usize,
    ranges: Vec<(usize, usize)>,
    z_list: Vec<u32>,
    head_inside: bool,
    tail_inside: bool,
}

struct SkeinCtx {
    w: f64,
    r_base: f64,
    k_base: usize,
    seamless: bool,
    sheet_w: f64,
    sheet_h: f64,
    curliness: f64,
    flow_bias: f64,
    ang_flow_rad: f64,
    width_var: f64,
    v_mode: u8, // 0=off, 1=vortex, 2=attract, 3=repel
    vx_center: f64,
    vy_center: f64,
    v_strength: f64,
    v_radius: f64,
    pad: f64,
    dom_x0: f64,
    dom_y0: f64,
    dom_w: f64,
    dom_h: f64,
    occ_step: f64,
    gw: usize,
    gh: usize,
    bridge_after_coil_prob: f64,
    coil_arc_min_deg: f64,
    coil_arc_span_deg: f64,
    coil_r_min_mul: f64,
    coil_r_span_mul: f64,
    occ_grid: Vec<u8>,
    roof_grid: Vec<u8>,
}

impl SkeinCtx {
    #[inline]
    fn to_cell_x(&self, x: f64) -> usize {
        let v = if self.seamless {
            (mod_f64(x, self.sheet_w) / self.occ_step).floor()
        } else {
            ((x - self.dom_x0) / self.occ_step).floor()
        };
        clamp(v, 0.0, (self.gw - 1) as f64) as usize
    }

    #[inline]
    fn to_cell_y(&self, y: f64) -> usize {
        let v = if self.seamless {
            (mod_f64(y, self.sheet_h) / self.occ_step).floor()
        } else {
            ((y - self.dom_y0) / self.occ_step).floor()
        };
        clamp(v, 0.0, (self.gh - 1) as f64) as usize
    }

    #[inline]
    fn grid_at_u8(&self, buf: &[u8], x: f64, y: f64) -> u8 {
        if !self.seamless
            && (x < self.dom_x0
                || x >= self.dom_x0 + self.dom_w
                || y < self.dom_y0
                || y >= self.dom_y0 + self.dom_h)
        {
            return 0;
        }
        buf[self.to_cell_y(y) * self.gw + self.to_cell_x(x)]
    }

    #[inline]
    fn grid_at_u32(&self, buf: &[u32], x: f64, y: f64) -> u32 {
        if !self.seamless
            && (x < self.dom_x0
                || x >= self.dom_x0 + self.dom_w
                || y < self.dom_y0
                || y >= self.dom_y0 + self.dom_h)
        {
            return 0;
        }
        buf[self.to_cell_y(y) * self.gw + self.to_cell_x(x)]
    }

    #[inline]
    fn inside_sheet(&self, x: f64, y: f64, margin: f64) -> bool {
        if self.seamless {
            return true;
        }
        x >= -margin && x <= self.sheet_w + margin && y >= -margin && y <= self.sheet_h + margin
    }

    #[inline]
    fn outside_extended(&self, x: f64, y: f64) -> bool {
        if self.seamless {
            return false;
        }
        x < -self.pad || x > self.sheet_w + self.pad || y < -self.pad || y > self.sheet_h + self.pad
    }

    #[inline]
    fn is_under_roof(&self, px: f64, py: f64, tx: f64, ty: f64, w_local: f64) -> bool {
        let nx = -ty;
        let ny = tx;
        let span = w_local * 0.52;
        if self.grid_at_u8(&self.roof_grid, px, py) < 1 {
            return false;
        }
        if self.grid_at_u8(&self.roof_grid, px + nx * span, py + ny * span) < 1 {
            return false;
        }
        if self.grid_at_u8(&self.roof_grid, px - nx * span, py - ny * span) < 1 {
            return false;
        }
        true
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn is_under_higher_roof_z(&self, roof_z: &[u32], px: f64, py: f64, tx: f64, ty: f64, tail_z: u32, w_local: f64) -> bool {
        let nx = -ty;
        let ny = tx;
        let span = w_local * 0.52;
        if self.grid_at_u32(roof_z, px, py) <= tail_z {
            return false;
        }
        if self.grid_at_u32(roof_z, px + nx * span, py + ny * span) <= tail_z {
            return false;
        }
        if self.grid_at_u32(roof_z, px - nx * span, py - ny * span) <= tail_z {
            return false;
        }
        true
    }

    fn stamp_disk_occ(&mut self, cx: f64, cy: f64, rad: f64) {
        let ix0 = ((cx - self.dom_x0 - rad) / self.occ_step).floor() as i64;
        let ix1 = ((cx - self.dom_x0 + rad) / self.occ_step).floor() as i64;
        let iy0 = ((cy - self.dom_y0 - rad) / self.occ_step).floor() as i64;
        let iy1 = ((cy - self.dom_y0 + rad) / self.occ_step).floor() as i64;
        let r2 = rad * rad;
        let gw_i = self.gw as i64;
        let gh_i = self.gh as i64;
        for gyi in iy0..=iy1 {
            let wyi = if self.seamless { mod_i(gyi, gh_i) } else { gyi };
            if wyi < 0 || wyi >= gh_i {
                continue;
            }
            let cell_cy = self.dom_y0 + (gyi as f64 + 0.5) * self.occ_step;
            let dy = cell_cy - cy;
            for gxi in ix0..=ix1 {
                let wxi = if self.seamless { mod_i(gxi, gw_i) } else { gxi };
                if wxi < 0 || wxi >= gw_i {
                    continue;
                }
                let cell_cx = self.dom_x0 + (gxi as f64 + 0.5) * self.occ_step;
                let dx = cell_cx - cx;
                if dx * dx + dy * dy <= r2 {
                    let idx = (wyi as usize) * self.gw + (wxi as usize);
                    if self.occ_grid[idx] < 250 {
                        self.occ_grid[idx] += 1;
                    }
                }
            }
        }
    }

    fn stamp_disk_roof(&mut self, cx: f64, cy: f64, rad: f64) {
        let ix0 = ((cx - self.dom_x0 - rad) / self.occ_step).floor() as i64;
        let ix1 = ((cx - self.dom_x0 + rad) / self.occ_step).floor() as i64;
        let iy0 = ((cy - self.dom_y0 - rad) / self.occ_step).floor() as i64;
        let iy1 = ((cy - self.dom_y0 + rad) / self.occ_step).floor() as i64;
        let r2 = rad * rad;
        let gw_i = self.gw as i64;
        let gh_i = self.gh as i64;
        for gyi in iy0..=iy1 {
            let wyi = if self.seamless { mod_i(gyi, gh_i) } else { gyi };
            if wyi < 0 || wyi >= gh_i {
                continue;
            }
            let cell_cy = self.dom_y0 + (gyi as f64 + 0.5) * self.occ_step;
            let dy = cell_cy - cy;
            for gxi in ix0..=ix1 {
                let wxi = if self.seamless { mod_i(gxi, gw_i) } else { gxi };
                if wxi < 0 || wxi >= gw_i {
                    continue;
                }
                let cell_cx = self.dom_x0 + (gxi as f64 + 0.5) * self.occ_step;
                let dx = cell_cx - cx;
                if dx * dx + dy * dy <= r2 {
                    let idx = (wyi as usize) * self.gw + (wxi as usize);
                    if self.roof_grid[idx] < 250 {
                        self.roof_grid[idx] += 1;
                    }
                }
            }
        }
    }

    fn stamp_max_z_disk(&self, z_buf: &mut [u32], cx: f64, cy: f64, rad: f64, z_val: u32) {
        let ix0 = ((cx - self.dom_x0 - rad) / self.occ_step).floor() as i64;
        let ix1 = ((cx - self.dom_x0 + rad) / self.occ_step).floor() as i64;
        let iy0 = ((cy - self.dom_y0 - rad) / self.occ_step).floor() as i64;
        let iy1 = ((cy - self.dom_y0 + rad) / self.occ_step).floor() as i64;
        let r2 = rad * rad;
        let gw_i = self.gw as i64;
        let gh_i = self.gh as i64;
        for gyi in iy0..=iy1 {
            let wyi = if self.seamless { mod_i(gyi, gh_i) } else { gyi };
            if wyi < 0 || wyi >= gh_i {
                continue;
            }
            let cell_cy = self.dom_y0 + (gyi as f64 + 0.5) * self.occ_step;
            let dy = cell_cy - cy;
            for gxi in ix0..=ix1 {
                let wxi = if self.seamless { mod_i(gxi, gw_i) } else { gxi };
                if wxi < 0 || wxi >= gw_i {
                    continue;
                }
                let cell_cx = self.dom_x0 + (gxi as f64 + 0.5) * self.occ_step;
                let dx = cell_cx - cx;
                if dx * dx + dy * dy <= r2 {
                    let idx = (wyi as usize) * self.gw + (wxi as usize);
                    if z_val > z_buf[idx] {
                        z_buf[idx] = z_val;
                    }
                }
            }
        }
    }

    #[inline]
    fn stream_width_at(&self, stream_scale: f64, breath_phase: f64, arc_s: f64) -> f64 {
        if self.width_var <= 1e-4 {
            return self.w;
        }
        let breath = 1.0 + 0.18 * self.width_var * (arc_s / (3.5 * self.r_base).max(1.0) + breath_phase).sin();
        clamp(self.w * stream_scale * breath, self.w * 0.58, self.w * 1.42)
    }

    fn probe_lower_side(&self, px: f64, py: f64, heading: f64, probe_r: f64) -> i32 {
        let lx = -heading.sin();
        let ly = heading.cos();
        let fx = heading.cos();
        let fy = heading.sin();
        let mut left_sum: u32 = 0;
        let mut right_sum: u32 = 0;
        for k in 1..=3 {
            let d = k as f64 * probe_r * 0.75;
            left_sum += self.grid_at_u8(&self.occ_grid, px + (fx * 0.6 + lx * 0.8) * d, py + (fy * 0.6 + ly * 0.8) * d) as u32;
            right_sum += self.grid_at_u8(&self.occ_grid, px + (fx * 0.6 - lx * 0.8) * d, py + (fy * 0.6 - ly * 0.8) * d) as u32;
        }
        if left_sum < right_sum {
            1
        } else if right_sum < left_sum {
            -1
        } else {
            0
        }
    }

    fn rotate_closed_loop_to_clearest(&self, pts_in: Vec<Pt>) -> Vec<Pt> {
        let n_seg = pts_in.len().saturating_sub(1);
        if n_seg < 12 {
            return pts_in;
        }
        let w_dx = pts_in[n_seg].x - pts_in[0].x;
        let w_dy = pts_in[n_seg].y - pts_in[0].y;
        let mut best_idx = 0usize;
        let mut best_min_d2 = -1.0f64;
        for i0 in 0..n_seg {
            let p_a = pts_in[i0];
            let mut min_d2 = f64::INFINITY;
            for (j0, pt_b) in pts_in.iter().enumerate().take(n_seg) {
                let mut dci = j0.abs_diff(i0);
                dci = dci.min(n_seg - dci);
                if dci < 6 {
                    continue;
                }
                let mut dx = pt_b.x - p_a.x;
                let mut dy = pt_b.y - p_a.y;
                if self.seamless {
                    dx = mod_f64(dx + self.sheet_w * 0.5, self.sheet_w) - self.sheet_w * 0.5;
                    dy = mod_f64(dy + self.sheet_h * 0.5, self.sheet_h) - self.sheet_h * 0.5;
                }
                let d2 = dx * dx + dy * dy;
                if d2 < min_d2 {
                    min_d2 = d2;
                }
            }
            if min_d2 > best_min_d2 {
                best_min_d2 = min_d2;
                best_idx = i0;
            }
        }
        if best_idx == 0 {
            return pts_in;
        }
        let mut out_pts = Vec::with_capacity(n_seg + 1);
        for k0 in 0..n_seg {
            let src = best_idx + k0;
            if src < n_seg {
                out_pts.push(pts_in[src]);
            } else {
                let s_wrap = src - n_seg;
                out_pts.push(Pt {
                    x: pts_in[s_wrap].x + w_dx,
                    y: pts_in[s_wrap].y + w_dy,
                });
            }
        }
        let first = out_pts[0];
        out_pts.push(Pt {
            x: first.x + w_dx,
            y: first.y + w_dy,
        });
        out_pts
    }

    #[allow(clippy::too_many_arguments)]
    fn grow_branch(
        &self,
        start_x: f64,
        start_y: f64,
        heading0: f64,
        br_seed: u32,
        br_sign: f64,
        len_scale: f64,
        stream_scale: f64,
        breath_phase: f64,
        is_bg_branch: bool,
    ) -> (Vec<Pt>, f64, bool) {
        let mut rng = Mulberry32::new(br_seed);
        let l_sc = len_scale;
        let s_scale = if self.width_var > 1e-4 { stream_scale } else { 1.0 };
        let b_phase = if self.width_var > 1e-4 { breath_phase } else { 0.0 };

        let mut state_arc_rem = 0.0f64;
        let mut r_loc = self.r_base;
        let mut c_step_loc = self.w * 0.56;
        let mut turn_rad = clamp(c_step_loc / r_loc, 8.0 * PI / 180.0, 28.0 * PI / 180.0);
        let mut sgn: i32 = if rng.next_f64() < 0.5 { 1 } else { -1 };
        // 0=none, 1=bridge, 2=coil_pos, 3=coil_neg
        let mut last_kind: u8 = 0;
        let mut last_coil_sign: i32 = sgn;

        let max_len_base = (20.0 + rng.next_f64() * 14.0) * self.w * l_sc;
        let hard_max_len = (if self.seamless { 40.0 * self.w } else { 48.0 * self.w }) * l_sc.max(0.65);
        let mut px = start_x;
        let mut py = start_y;
        let mut hd = heading0;
        let mut b_pts: Vec<Pt> = Vec::new();
        let mut b_hds: Vec<f64> = Vec::new();
        let mut b_len = 0.0f64;
        let mut step_count = 0usize;
        let mut closed_tor_loop = false;

        let apply_curvature = |w_now: f64, c_step_l: f64, r_l: f64| -> f64 {
            let max_safe_turn = c_step_l / (w_now * 1.18);
            let flow_damp = if self.flow_bias > 0.45 {
                1.0 - 0.45 * (self.flow_bias - 0.45)
            } else {
                1.0
            };
            let raw_t = (c_step_l / (w_now * 1.18).max(r_l)) * flow_damp;
            let min_t = if self.curliness < 0.35 || self.flow_bias > 0.45 {
                4.0 * PI / 180.0
            } else {
                10.0 * PI / 180.0
            };
            clamp(raw_t, min_t, (29.0 * PI / 180.0).min(max_safe_turn))
        };

        let pick_next_state = |rng: &mut Mulberry32,
                               px: f64,
                               py: f64,
                               hd: f64,
                               b_len: f64,
                               last_kind: &mut u8,
                               last_coil_sign: &mut i32,
                               sgn: &mut i32,
                               r_loc: &mut f64,
                               c_step_loc: &mut f64,
                               state_arc_rem: &mut f64,
                               turn_rad: &mut f64| {
            let rs = rng.next_f64();
            let w_now = self.stream_width_at(s_scale, b_phase, b_len);
            let bridge_len_mul = if self.flow_bias > 0.45 {
                1.0 + 1.15 * (self.flow_bias - 0.45)
            } else {
                1.0
            };
            *c_step_loc = clamp(self.w * (0.52 + rng.next_f64() * 0.14), self.w * 0.48, self.w * 0.68);
            if *last_kind == 2 || *last_kind == 3 {
                if rs < self.bridge_after_coil_prob {
                    *last_kind = 1;
                    *r_loc = (w_now * 1.85).max(self.r_base * (1.55 + rng.next_f64() * 1.05));
                    *state_arc_rem = *r_loc * ((55.0 + rng.next_f64() * 60.0) * PI / 180.0) * bridge_len_mul;
                    let steer_b = self.probe_lower_side(px, py, hd, *r_loc);
                    *sgn = if steer_b != 0 { steer_b } else { -*last_coil_sign };
                } else {
                    *sgn = -*last_coil_sign;
                    *last_coil_sign = *sgn;
                    *last_kind = if *sgn > 0 { 2 } else { 3 };
                    *r_loc = (w_now * 1.20).max(self.r_base * (self.coil_r_min_mul + rng.next_f64() * self.coil_r_span_mul));
                    *state_arc_rem = *r_loc * ((self.coil_arc_min_deg + rng.next_f64() * self.coil_arc_span_deg) * PI / 180.0);
                }
            } else if *last_kind == 1 && rs < (0.22 + 0.45 * (1.0 - self.curliness)) {
                *last_kind = 1;
                *r_loc = (w_now * 1.90).max(self.r_base * (1.60 + rng.next_f64() * 1.00));
                *state_arc_rem = *r_loc * ((50.0 + rng.next_f64() * 55.0) * PI / 180.0) * bridge_len_mul;
                let steer_e2 = self.probe_lower_side(px, py, hd, *r_loc);
                *sgn = if steer_e2 != 0 {
                    steer_e2
                } else if rng.next_f64() < 0.5 {
                    1
                } else {
                    -1
                };
            } else {
                let steer_c = self.probe_lower_side(px, py, hd, self.r_base);
                *sgn = if steer_c != 0 {
                    steer_c
                } else if *last_kind == 0 {
                    if rng.next_f64() < 0.5 { 1 } else { -1 }
                } else {
                    -*last_coil_sign
                };
                *last_coil_sign = *sgn;
                *last_kind = if *sgn > 0 { 2 } else { 3 };
                *r_loc = (w_now * 1.20).max(self.r_base * (self.coil_r_min_mul + rng.next_f64() * self.coil_r_span_mul));
                *state_arc_rem = *r_loc * ((self.coil_arc_min_deg + rng.next_f64() * self.coil_arc_span_deg) * PI / 180.0);
            }
            *turn_rad = apply_curvature(w_now, *c_step_loc, *r_loc);
        };

        pick_next_state(
            &mut rng, px, py, hd, b_len,
            &mut last_kind, &mut last_coil_sign, &mut sgn,
            &mut r_loc, &mut c_step_loc, &mut state_arc_rem, &mut turn_rad,
        );

        while b_len < hard_max_len && step_count < 240 {
            if state_arc_rem <= 0.0 {
                pick_next_state(
                    &mut rng, px, py, hd, b_len,
                    &mut last_kind, &mut last_coil_sign, &mut sgn,
                    &mut r_loc, &mut c_step_loc, &mut state_arc_rem, &mut turn_rad,
                );
            }
            let c_step = c_step_loc;
            let w_now = self.stream_width_at(s_scale, b_phase, b_len);
            let mut n_px = px + hd.cos() * c_step;
            let mut n_py = py + hd.sin() * c_step;
            let self_avoid_r2 = (w_now * 1.15) * (w_now * 1.15);

            if b_pts.len() >= 5 {
                let chk_lo = b_pts.len().saturating_sub(16);
                let chk_hi = b_pts.len() - 4;
                for sb in (chk_lo..=chk_hi).rev() {
                    let mut sdx = n_px - b_pts[sb].x;
                    let mut sdy = n_py - b_pts[sb].y;
                    if self.seamless {
                        sdx = mod_f64(sdx + self.sheet_w * 0.5, self.sheet_w) - self.sheet_w * 0.5;
                        sdy = mod_f64(sdy + self.sheet_h * 0.5, self.sheet_h) - self.sheet_h * 0.5;
                    }
                    if sdx * sdx + sdy * sdy < self_avoid_r2 {
                        let sin_cross = (hd - b_hds[sb]).sin().abs();
                        if sin_cross < 0.766 {
                            let fx_s = hd.cos();
                            let fy_s = hd.sin();
                            let side_cross = fx_s * sdy - fy_s * sdx;
                            let away_sgn = if side_cross >= 0.0 { 1 } else { -1 };
                            let low_side = self.probe_lower_side(px, py, hd, self.r_base * 1.6);
                            sgn = if low_side != 0 && low_side == away_sgn { low_side } else { away_sgn };
                            last_kind = 1;
                            r_loc = (w_now * 1.85).max(self.r_base * 1.75);
                            state_arc_rem = r_loc * ((65.0 + rng.next_f64() * 45.0) * PI / 180.0);
                            turn_rad = apply_curvature(w_now, c_step_loc, r_loc);
                            hd += br_sign * (sgn as f64) * turn_rad;
                            n_px = px + hd.cos() * c_step;
                            n_py = py + hd.sin() * c_step;
                        }
                        break;
                    }
                }
            }

            b_pts.push(Pt { x: n_px, y: n_py });
            b_hds.push(hd);
            b_len += c_step;
            state_arc_rem -= c_step;
            px = n_px;
            py = n_py;

            let mut d_theta_step = br_sign * (sgn as f64) * turn_rad;

            if self.flow_bias > 1e-4 {
                let steer_flow = 0.5 * (2.0 * (self.ang_flow_rad - hd)).sin() * (0.38 * self.flow_bias);
                d_theta_step = d_theta_step * (1.0 - 0.55 * self.flow_bias) + steer_flow;
            }

            if self.v_mode != 0 {
                let mut dx_v = px - self.vx_center;
                let mut dy_v = py - self.vy_center;
                if self.seamless {
                    dx_v = mod_f64(dx_v + self.sheet_w * 0.5, self.sheet_w) - self.sheet_w * 0.5;
                    dy_v = mod_f64(dy_v + self.sheet_h * 0.5, self.sheet_h) - self.sheet_h * 0.5;
                }
                let dist_v = (dx_v * dx_v + dy_v * dy_v).sqrt();
                let w_v = smoothstep(self.v_radius, 0.0, dist_v)
                    * (self.v_strength.abs() / 100.0)
                    * if is_bg_branch { 0.65 } else { 1.0 };
                if w_v > 0.001 {
                    let ang_rad_v = if dist_v > 1e-4 { dy_v.atan2(dx_v) } else { hd };
                    let spin_dir = (if self.v_strength >= 0.0 { 1.0 } else { -1.0 }) * br_sign;
                    let target_hd = match self.v_mode {
                        1 => {
                            if dist_v < 1.2 * w_now {
                                ang_rad_v + spin_dir * 1.15
                            } else {
                                ang_rad_v + spin_dir * (PI * 0.5 - 0.09)
                            }
                        }
                        2 => {
                            if dist_v < 1.35 * w_now {
                                ang_rad_v + spin_dir * 1.35
                            } else {
                                ang_rad_v + PI
                            }
                        }
                        3 => ang_rad_v,
                        _ => hd,
                    };
                    let err_v = wrap_angle_pi(target_hd - hd);
                    d_theta_step = d_theta_step * (1.0 - 0.75 * w_v) + err_v * (0.42 * w_v);
                }
            }

            let max_turn_safe = c_step / (w_now * 1.18);
            d_theta_step = clamp(d_theta_step, -max_turn_safe, max_turn_safe);
            hd += d_theta_step;
            step_count += 1;

            if !self.seamless && self.outside_extended(px, py) {
                break;
            }
            if self.seamless && b_len >= 14.0 * self.w && l_sc >= 0.9 {
                let dx_t = mod_f64(px - start_x + self.sheet_w * 0.5, self.sheet_w) - self.sheet_w * 0.5;
                let dy_t = mod_f64(py - start_y + self.sheet_h * 0.5, self.sheet_h) - self.sheet_h * 0.5;
                let d_t2 = dx_t * dx_t + dy_t * dy_t;
                if d_t2 < (1.35 * self.w) * (1.35 * self.w) && d_t2 > (0.35 * self.w) * (0.35 * self.w) {
                    let d_t = d_t2.sqrt();
                    let ux_t = -dx_t / d_t;
                    let uy_t = -dy_t / d_t;
                    let cur_tx = hd.cos();
                    let cur_ty = hd.sin();
                    let st_tx = heading0.cos();
                    let st_ty = heading0.sin();
                    let dot_end_start = cur_tx * st_tx + cur_ty * st_ty;
                    let dot_cur_vec = cur_tx * ux_t + cur_ty * uy_t;
                    let dot_vec_start = ux_t * st_tx + uy_t * st_ty;
                    if dot_end_start > 0.82 && dot_cur_vec > 0.86 && dot_vec_start > 0.86 {
                        let end_x = px - dx_t;
                        let end_y = py - dy_t;
                        if d_t > self.w * 0.72 {
                            let m_hx = 0.5 * (px + end_x) + (cur_tx - st_tx) * (d_t * 0.125);
                            let m_hy = 0.5 * (py + end_y) + (cur_ty - st_ty) * (d_t * 0.125);
                            b_pts.push(Pt { x: m_hx, y: m_hy });
                            b_hds.push(hd);
                        }
                        b_pts.push(Pt { x: end_x, y: end_y });
                        b_hds.push(heading0);
                        b_len += d_t;
                        closed_tor_loop = true;
                        break;
                    }
                }
            }
            if b_len >= max_len_base {
                let tx_next = hd.cos();
                let ty_next = hd.sin();
                if !self.inside_sheet(px, py, w_now * 0.65) || self.is_under_roof(px, py, tx_next, ty_next, w_now) {
                    break;
                }
            }
        }

        let mut w_end_now = self.stream_width_at(s_scale, b_phase, b_len);
        let mut tucked = closed_tor_loop
            || !self.inside_sheet(px, py, w_end_now * 0.55)
            || self.is_under_roof(px, py, hd.cos(), hd.sin(), w_end_now);
        if !tucked {
            let c_step_e = clamp(self.w * 0.56, self.w * 0.48, self.w * 0.66);
            let turn_e = clamp(c_step_e / (w_end_now * 1.25).max(self.r_base), 12.0 * PI / 180.0, 25.0 * PI / 180.0);
            let mut steer_hold = sgn;
            for ex in 0..96 {
                if (ex & 3) == 0 {
                    let st_probe = self.probe_lower_side(px, py, hd, self.r_base);
                    if st_probe != 0 {
                        steer_hold = st_probe;
                    }
                }
                px += hd.cos() * c_step_e;
                py += hd.sin() * c_step_e;
                b_pts.push(Pt { x: px, y: py });
                b_len += c_step_e;
                hd += br_sign * (steer_hold as f64) * turn_e;
                w_end_now = self.stream_width_at(s_scale, b_phase, b_len);
                if self.outside_extended(px, py)
                    || !self.inside_sheet(px, py, w_end_now * 0.55)
                    || self.is_under_roof(px, py, hd.cos(), hd.sin(), w_end_now)
                {
                    tucked = true;
                    break;
                }
            }
        }
        if !tucked && self.inside_sheet(px, py, w_end_now * 0.55) {
            let mut trimmed = false;
            for tb in (5..b_pts.len()).rev() {
                let p_cur = b_pts[tb];
                let p_prv = b_pts[tb - 1];
                let dtx = p_cur.x - p_prv.x;
                let dty = p_cur.y - p_prv.y;
                let dtl = (dtx * dtx + dty * dty).sqrt();
                if dtl > 1e-6 && self.is_under_roof(p_cur.x, p_cur.y, dtx / dtl, dty / dtl, w_end_now) {
                    b_pts.truncate(tb + 1);
                    trimmed = true;
                    break;
                }
            }
            if !trimmed && !self.seamless {
                let c_exit = self.w * 0.60;
                let mut ee = 0;
                while ee < 64 && self.inside_sheet(px, py, w_end_now * 0.60) {
                    px += hd.cos() * c_exit;
                    py += hd.sin() * c_exit;
                    b_pts.push(Pt { x: px, y: py });
                    b_len += c_exit;
                    ee += 1;
                }
            }
        }
        (b_pts, b_len, closed_tor_loop)
    }

    #[allow(clippy::too_many_arguments)]
    fn register_stream(
        &mut self,
        streams: &mut Vec<StreamGeom>,
        raw_streams: &mut Vec<RawStreamMeta>,
        mut pts: Vec<Pt>,
        closed_loop: bool,
        st_seed: u32,
        is_bg_fill: bool,
        stream_scale_in: f64,
        breath_phase_in: f64,
    ) -> bool {
        if pts.len() < 7 {
            return false;
        }
        let s_scale = if self.width_var > 1e-4 { stream_scale_in } else { 1.0 };
        let b_phase = if self.width_var > 1e-4 { breath_phase_in } else { 0.0 };
        let k_stream = if self.width_var > 1e-4 {
            clamp(js_round(self.k_base as f64 * s_scale), 3.0, 9.0) as usize
        } else {
            self.k_base
        };

        if closed_loop {
            pts = self.rotate_closed_loop_to_clearest(pts);
        }

        let mut cum_arc = vec![0.0f64; pts.len()];
        for ai0 in 1..pts.len() {
            cum_arc[ai0] = cum_arc[ai0 - 1] + dist_2d(pts[ai0 - 1].x, pts[ai0 - 1].y, pts[ai0].x, pts[ai0].y);
        }

        if !closed_loop && !raw_streams.is_empty() {
            let mut trim_start = 0usize;
            let mut trim_end = pts.len() - 1;
            let p0t = pts[0];
            let p1t = pts[1];
            let d0l = dist_2d(p0t.x, p0t.y, p1t.x, p1t.y);
            let w0t = self.stream_width_at(s_scale, b_phase, cum_arc[0]);
            if self.inside_sheet(p0t.x, p0t.y, w0t * 0.45)
                && d0l > 1e-6
                && !self.is_under_roof(p0t.x, p0t.y, (p1t.x - p0t.x) / d0l, (p1t.y - p0t.y) / d0l, w0t)
            {
                for ts in 1..=trim_end.saturating_sub(6) {
                    let pa_t = pts[ts];
                    let pb_t = pts[ts + 1];
                    let dl_t = dist_2d(pa_t.x, pa_t.y, pb_t.x, pb_t.y);
                    let w_ts = self.stream_width_at(s_scale, b_phase, cum_arc[ts]);
                    if !self.inside_sheet(pa_t.x, pa_t.y, w_ts * 0.45)
                        || (dl_t > 1e-6
                            && self.is_under_roof(pa_t.x, pa_t.y, (pb_t.x - pa_t.x) / dl_t, (pb_t.y - pa_t.y) / dl_t, w_ts))
                    {
                        trim_start = ts;
                        break;
                    }
                }
            }
            let p_et = pts[trim_end];
            let p_ept = pts[trim_end - 1];
            let d_el = dist_2d(p_ept.x, p_ept.y, p_et.x, p_et.y);
            let w_et = self.stream_width_at(s_scale, b_phase, cum_arc[trim_end]);
            if self.inside_sheet(p_et.x, p_et.y, w_et * 0.45)
                && d_el > 1e-6
                && !self.is_under_roof(p_et.x, p_et.y, (p_et.x - p_ept.x) / d_el, (p_et.y - p_ept.y) / d_el, w_et)
            {
                let lo_te = trim_start + 6;
                if trim_end > lo_te {
                    for te in (lo_te..trim_end).rev() {
                        let pc_t = pts[te];
                        let pd_t = pts[te - 1];
                        let dl_e = dist_2d(pd_t.x, pd_t.y, pc_t.x, pc_t.y);
                        let w_te = self.stream_width_at(s_scale, b_phase, cum_arc[te]);
                        if !self.inside_sheet(pc_t.x, pc_t.y, w_te * 0.45)
                            || (dl_e > 1e-6
                                && self.is_under_roof(pc_t.x, pc_t.y, (pc_t.x - pd_t.x) / dl_e, (pc_t.y - pd_t.y) / dl_e, w_te))
                        {
                            trim_end = te;
                            break;
                        }
                    }
                }
            }
            if trim_start > 0 || trim_end < pts.len() - 1 {
                pts = pts[trim_start..=trim_end].to_vec();
                if pts.len() < 7 {
                    return false;
                }
                cum_arc = vec![0.0f64; pts.len()];
                for ai1 in 1..pts.len() {
                    cum_arc[ai1] = cum_arc[ai1 - 1] + dist_2d(pts[ai1 - 1].x, pts[ai1 - 1].y, pts[ai1].x, pts[ai1].y);
                }
            }
            let f0 = pts[0];
            let f1 = pts[1];
            let fl0 = dist_2d(f0.x, f0.y, f1.x, f1.y);
            let wf0 = self.stream_width_at(s_scale, b_phase, cum_arc[0]);
            if self.inside_sheet(f0.x, f0.y, 0.0)
                && (fl0 <= 1e-6 || !self.is_under_roof(f0.x, f0.y, (f1.x - f0.x) / fl0, (f1.y - f0.y) / fl0, wf0))
            {
                return false;
            }
            let e1 = pts[pts.len() - 1];
            let e0 = pts[pts.len() - 2];
            let el = dist_2d(e0.x, e0.y, e1.x, e1.y);
            let we1 = self.stream_width_at(s_scale, b_phase, cum_arc[pts.len() - 1]);
            if self.inside_sheet(e1.x, e1.y, 0.0)
                && (el <= 1e-6 || !self.is_under_roof(e1.x, e1.y, (e1.x - e0.x) / el, (e1.y - e0.y) / el, we1))
            {
                return false;
            }
        }

        let Some(mut geom) = build_stream_chord_geometry(pts, closed_loop) else {
            return false;
        };
        if geom.m < 5 {
            return false;
        }

        let mut w_verts = vec![0.0f64; geom.m + 1];
        let mut actual_len = 0.0f64;
        w_verts[0] = self.stream_width_at(s_scale, b_phase, 0.0);
        for li_len in 0..geom.m {
            actual_len += geom.chords[li_len].len;
            w_verts[li_len + 1] = self.stream_width_at(s_scale, b_phase, actual_len);
        }
        if closed_loop {
            let w_closed = 0.5 * (w_verts[0] + w_verts[geom.m]);
            w_verts[0] = w_closed;
            w_verts[geom.m] = w_closed;
        }

        let roof_lo = if closed_loop { 0 } else { 2 };
        let roof_hi = if closed_loop { geom.m - 1 } else { geom.m - 3 };

        for (vi, &wv) in w_verts.iter().enumerate().take(geom.m + 1) {
            let vp = geom.pts[vi];
            self.stamp_disk_occ(vp.x, vp.y, wv * 0.40);
        }
        for ci_s in 0..geom.m {
            let mc = geom.chords[ci_s];
            let w_ch = 0.5 * (w_verts[ci_s] + w_verts[ci_s + 1]);
            self.stamp_disk_occ(mc.mx, mc.my, w_ch * 0.40);
            if ci_s >= roof_lo && ci_s <= roof_hi {
                self.stamp_disk_roof(mc.mx, mc.my, w_ch * 0.46);
            }
        }

        geom.w_verts = Some(w_verts);
        geom.k_strands = Some(k_stream);

        let stream_idx = raw_streams.len();
        let geom_idx = streams.len();
        streams.push(geom);
        raw_streams.push(RawStreamMeta {
            idx: stream_idx,
            geom_idx,
            seed: st_seed,
            closed_loop,
            bg_fill: is_bg_fill,
            parity: stream_idx & 1,
        });
        true
    }
}

#[inline]
fn pt_seg_dist2(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;
    let c1 = vx * wx + vy * wy;
    if c1 <= 0.0 {
        return wx * wx + wy * wy;
    }
    let c2 = vx * vx + vy * vy;
    if c2 <= c1 {
        let dx_e = px - bx;
        let dy_e = py - by;
        return dx_e * dx_e + dy_e * dy_e;
    }
    let t = c1 / c2;
    let proj_x = ax + t * vx - px;
    let proj_y = ay + t * vy - py;
    proj_x * proj_x + proj_y * proj_y
}

fn build_nawa_stream_skeleton(cfg: &KakeamiConfig) -> NawaSkeleton {
    let n_scale = clamp(cfg.nawa_scale, 0.40, 4.50);
    let w = cfg.nawa_rope_width * n_scale;
    let r_cfg = cfg.nawa_coil_radius * n_scale;
    let k_base = clamp(js_round(cfg.nawa_strands as f64), 2.0, 10.0) as usize;
    let seamless = cfg.seamless;
    let sheet_w = (cfg.width as i64).max(64) as f64;
    let sheet_h = (cfg.height as i64).max(64) as f64;
    let overlap_scale = (cfg.overlap_density as f64 / 100.0).max(0.5).sqrt();

    let curliness = clamp(cfg.nawa_curliness / 100.0, 0.0, 1.0);
    let flow_bias = clamp(cfg.nawa_flow_bias / 100.0, 0.0, 1.0);
    let ang_flow_rad = cfg.nawa_flow_angle * PI / 180.0;
    let width_var = clamp(cfg.nawa_width_var / 100.0, 0.0, 1.0);
    let v_mode = match cfg.nawa_vortex_mode.as_str() {
        "vortex" => 1u8,
        "attract" => 2u8,
        "repel" => 3u8,
        _ => 0u8,
    };
    let vx_center = clamp(cfg.nawa_vortex_x, 0.0, 1.0) * sheet_w;
    let vy_center = clamp(cfg.nawa_vortex_y, 0.0, 1.0) * sheet_h;
    let v_strength = clamp(cfg.nawa_vortex_strength, -100.0, 100.0);
    let v_radius = clamp(cfg.nawa_vortex_radius, 0.10, 1.0) * sheet_w.min(sheet_h);

    let r_scale = clamp(r_cfg / (w * 1.45).max(1.0), 0.78, 1.35);
    let r_base = clamp(w * 1.35 * r_scale, w * 1.18, w * 1.72);
    let pad = if seamless { 0.0 } else { r_base * 2.5 + w * 3.0 };
    let dom_x0 = -pad;
    let dom_y0 = -pad;
    let dom_w = sheet_w + 2.0 * pad;
    let dom_h = sheet_h + 2.0 * pad;

    let occ_step = (w * 0.25).max(1e-3);
    let gw = ((dom_w / occ_step).ceil() as usize).max(4);
    let gh = ((dom_h / occ_step).ceil() as usize).max(4);
    let total_cells = gw * gh;

    let bridge_after_coil_prob = if curliness <= 0.5 {
        0.92 + (0.55 - 0.92) * (curliness / 0.5)
    } else {
        0.55 + (0.22 - 0.55) * ((curliness - 0.5) / 0.5)
    };
    let coil_arc_min_deg = if curliness <= 0.5 {
        40.0 + 100.0 * (curliness / 0.5)
    } else {
        140.0 + 60.0 * ((curliness - 0.5) / 0.5)
    };
    let coil_arc_span_deg = if curliness <= 0.5 {
        45.0 + 50.0 * (curliness / 0.5)
    } else {
        95.0 - 5.0 * ((curliness - 0.5) / 0.5)
    };
    let coil_r_min_mul = if curliness <= 0.5 {
        2.20 - 1.30 * (curliness / 0.5)
    } else {
        0.90 - 0.08 * ((curliness - 0.5) / 0.5)
    };
    let coil_r_span_mul = if curliness <= 0.5 {
        2.00 - 1.64 * (curliness / 0.5)
    } else {
        0.36 - 0.14 * ((curliness - 0.5) / 0.5)
    };

    let mut ctx = SkeinCtx {
        w,
        r_base,
        k_base,
        seamless,
        sheet_w,
        sheet_h,
        curliness,
        flow_bias,
        ang_flow_rad,
        width_var,
        v_mode,
        vx_center,
        vy_center,
        v_strength,
        v_radius,
        pad,
        dom_x0,
        dom_y0,
        dom_w,
        dom_h,
        occ_step,
        gw,
        gh,
        bridge_after_coil_prob,
        coil_arc_min_deg,
        coil_arc_span_deg,
        coil_r_min_mul,
        coil_r_span_mul,
        occ_grid: vec![0u8; total_cells],
        roof_grid: vec![0u8; total_cells],
    };

    let mut perm: Vec<usize> = (0..total_cells).collect();
    let mut shuf_rng = Mulberry32::new(cfg.seed ^ 0x51a7_e201);
    for pi in (1..total_cells).rev() {
        let pj = (shuf_rng.next_f64() * (pi + 1) as f64).floor() as usize;
        perm.swap(pi, pj);
    }

    let target_layers: u8 = if overlap_scale >= 1.25 { 3 } else { 2 };
    let active_area = if seamless {
        sheet_w * sheet_h
    } else {
        (sheet_w + w * 2.0) * (sheet_h + w * 2.0)
    };
    let max_primary_streams = total_cells.min(
        (((active_area * (target_layers as f64 + 1.8)) / (w * w * 32.0)).ceil() as usize).max(20),
    );
    let max_bg_fill_streams = total_cells.min(
        max_primary_streams + (((active_area / (w * w * 10.0)).ceil() as usize).max(28)),
    );

    let mut streams: Vec<StreamGeom> = Vec::new();
    let mut raw_streams: Vec<RawStreamMeta> = Vec::new();

    // Wavy diagonal torus anchor streams in seamless mode
    if seamless {
        let diag_specs: [(i64, i64); 3] = if flow_bias > 0.30 {
            #[derive(Clone, Copy)]
            struct CandPq {
                p: i64,
                q: i64,
                align: f64,
            }
            let mut cands_pq: Vec<CandPq> = Vec::new();
            for pq_p in -2i64..=2 {
                for pq_q in -2i64..=2 {
                    if pq_p == 0 && pq_q == 0 {
                        continue;
                    }
                    if pq_p < 0 || (pq_p == 0 && pq_q < 0) {
                        continue;
                    }
                    if gcd_abs(pq_p, pq_q) != 1 {
                        continue;
                    }
                    let ang_pq = (pq_q as f64 * sheet_h).atan2(pq_p as f64 * sheet_w);
                    let align_pq = (ang_pq - ang_flow_rad).cos().abs();
                    cands_pq.push(CandPq { p: pq_p, q: pq_q, align: align_pq });
                }
            }
            // CRITICAL: stable sort matches V8 ES2019 TimSort on symmetric flowAngle ties!
            cands_pq.sort_by(|a, b| b.align.total_cmp(&a.align));
            let i1 = 1usize.min(cands_pq.len() - 1);
            [
                (cands_pq[0].p, cands_pq[0].q),
                (cands_pq[0].p, cands_pq[0].q),
                (cands_pq[i1].p, cands_pq[i1].q),
            ]
        } else if sheet_h > sheet_w {
            [(1, 1), (1, -1), (1, 2)]
        } else {
            [(1, 1), (1, -1), (2, -1)]
        };

        let anc_curl_amp = (0.35 + 0.65 * clamp(curliness * 1.6, 0.0, 1.0)) * (1.0 - 0.50 * flow_bias);
        for (ds, &(wp, wq)) in diag_specs.iter().enumerate() {
            let a_seed = hash2d(cfg.seed ^ 0x4e41_5742, ds as i32 + 1, (wp * 7 + wq * 13) as i32);
            let mut a_rng = Mulberry32::new(a_seed);
            let dx_w = wp as f64 * sheet_w;
            let dy_w = wq as f64 * sheet_h;
            let diag_len = (dx_w * dx_w + dy_w * dy_w).sqrt();
            let utx = dx_w / diag_len;
            let uty = dy_w / diag_len;
            let unx = -uty;
            let uny = utx;

            let k_cycles = (js_round(diag_len / (w * 9.0)) as usize).max(2);
            let wave_len = diag_len / (TAU * k_cycles as f64);
            let max_amp_curv = (wave_len * wave_len) / (w * 1.32);
            let amp_wave = clamp(
                w * (0.95 + a_rng.next_f64() * 0.35) * anc_curl_amp,
                w * 0.25,
                (w * 1.45).min(max_amp_curv),
            );
            let phase_w = a_rng.next_f64() * TAU;
            let base_ox = a_rng.next_f64() * sheet_w;
            let base_oy = a_rng.next_f64() * sheet_h;

            let dense_n = (k_cycles * 48).max(128);
            let mut dxs = vec![0.0f64; dense_n + 1];
            let mut dys = vec![0.0f64; dense_n + 1];
            let mut arc_s = vec![0.0f64; dense_n + 1];
            for di in 0..=dense_n {
                let u_f = di as f64 / dense_n as f64;
                let lat = amp_wave * (u_f * k_cycles as f64 * TAU + phase_w).sin();
                dxs[di] = base_ox + dx_w * u_f + unx * lat;
                dys[di] = base_oy + dy_w * u_f + uny * lat;
                if di > 0 {
                    arc_s[di] = arc_s[di - 1] + dist_2d(dxs[di - 1], dys[di - 1], dxs[di], dys[di]);
                }
            }
            let total_arc = arc_s[dense_n];
            let n_chords_a = (js_round(total_arc / (w * 0.58)) as usize).max(18);
            let mut anc_pts = Vec::with_capacity(n_chords_a + 1);
            let mut d_cursor = 0usize;
            for li in 0..n_chords_a {
                let target_s = (li as f64 / n_chords_a as f64) * total_arc;
                while d_cursor < dense_n - 1 && arc_s[d_cursor + 1] < target_s {
                    d_cursor += 1;
                }
                let seg_s = (arc_s[d_cursor + 1] - arc_s[d_cursor]).max(1e-6);
                let frac_s = clamp((target_s - arc_s[d_cursor]) / seg_s, 0.0, 1.0);
                anc_pts.push(Pt {
                    x: dxs[d_cursor] + (dxs[d_cursor + 1] - dxs[d_cursor]) * frac_s,
                    y: dys[d_cursor] + (dys[d_cursor + 1] - dys[d_cursor]) * frac_s,
                });
            }
            let p0 = anc_pts[0];
            anc_pts.push(Pt { x: p0.x + dx_w, y: p0.y + dy_w });
            let a_w_scale = 1.0 + (a_rng.next_f64() - 0.5) * 0.55 * width_var;
            let a_b_phase = a_rng.next_f64() * TAU;
            ctx.register_stream(&mut streams, &mut raw_streams, anc_pts, true, a_seed, false, a_w_scale, a_b_phase);
        }
    }

    // Three-phase seeding on occGrid
    for seed_pass in 0..3 {
        let is_bg_pass = seed_pass == 2;
        let pass_cap = if seed_pass == 1 { target_layers } else { 1 };
        let stream_limit = if is_bg_pass { max_bg_fill_streams } else { max_primary_streams };
        for &flat in &perm {
            if raw_streams.len() >= stream_limit {
                break;
            }
            if ctx.occ_grid[flat] >= pass_cap {
                continue;
            }
            let cy_cell = flat / gw;
            let cx_cell = flat - cy_cell * gw;

            let mut seed_x = dom_x0 + (cx_cell as f64 + 0.5) * occ_step;
            let mut seed_y = dom_y0 + (cy_cell as f64 + 0.5) * occ_step;
            let sheet_margin = if is_bg_pass { w * 0.35 } else { w * 0.85 };
            if !seamless && !ctx.inside_sheet(seed_x, seed_y, sheet_margin) {
                continue;
            }

            let st_seed = hash2d(
                cfg.seed ^ if is_bg_pass { 0x6b2f_9103 } else { 0x4e41_5741 },
                cx_cell as i32,
                cy_cell as i32,
            );
            let mut st_rng = Mulberry32::new(st_seed);
            let j_amp = if is_bg_pass { 0.35 } else { 0.75 };
            seed_x += (st_rng.next_f64() - 0.5) * occ_step * j_amp;
            seed_y += (st_rng.next_f64() - 0.5) * occ_step * j_amp;
            let mut heading0 = st_rng.next_f64() * TAU;
            if flow_bias > 0.15 {
                let dir_sign0 = if st_rng.next_f64() < 0.5 { 0.0 } else { PI };
                let spread0 = (st_rng.next_f64() - 0.5) * PI * (1.0 - 0.72 * flow_bias);
                heading0 = ang_flow_rad + dir_sign0 + spread0;
            }
            let s_w_scale = 1.0 + (st_rng.next_f64() - 0.5) * 0.55 * width_var;
            let s_b_phase = st_rng.next_f64() * TAU;
            let w_seed0 = ctx.stream_width_at(s_w_scale, s_b_phase, 0.0);

            let l_scale = if is_bg_pass { 0.65 } else { 1.0 };
            let (pts, closed_loop) = if seamless && !is_bg_pass {
                let (fwd_pts, _fwd_len, closed) =
                    ctx.grow_branch(seed_x, seed_y, heading0, st_seed ^ 0x1357_9bdf, 1.0, l_scale, s_w_scale, s_b_phase, false);
                let mut pts = Vec::with_capacity(fwd_pts.len() + 1);
                pts.push(Pt { x: seed_x, y: seed_y });
                pts.extend_from_slice(&fwd_pts);
                let start_covered = ctx.is_under_roof(seed_x, seed_y, heading0.cos(), heading0.sin(), w_seed0);
                if !closed && !start_covered {
                    let (mut bwd_pts, _bwd_len, _) = ctx.grow_branch(
                        seed_x, seed_y, heading0 + PI, st_seed ^ 0x2468_ace1, -1.0, l_scale, s_w_scale, s_b_phase, false,
                    );
                    bwd_pts.reverse();
                    bwd_pts.extend(pts);
                    pts = bwd_pts;
                }
                (pts, closed)
            } else {
                let (fwd_pts, _, _) =
                    ctx.grow_branch(seed_x, seed_y, heading0, st_seed ^ 0x1357_9bdf, 1.0, l_scale, s_w_scale, s_b_phase, is_bg_pass);
                let (mut bwd_pts, _, _) = ctx.grow_branch(
                    seed_x, seed_y, heading0 + PI, st_seed ^ 0x2468_ace1, -1.0, l_scale, s_w_scale, s_b_phase, is_bg_pass,
                );
                bwd_pts.reverse();
                bwd_pts.push(Pt { x: seed_x, y: seed_y });
                bwd_pts.extend_from_slice(&fwd_pts);
                (bwd_pts, false)
            };

            ctx.register_stream(&mut streams, &mut raw_streams, pts, closed_loop, st_seed, is_bg_pass, s_w_scale, s_b_phase);
        }
    }

    // Pass 2: Spatial centerline segment index for crossing-aware Z-splitting
    let ax_step = (w * if width_var > 1e-4 { 1.70 } else { 1.25 }).max(1e-3);
    let ax_w = ((dom_w / ax_step).ceil() as usize).max(4);
    let ax_h = ((dom_h / ax_step).ceil() as usize).max(4);
    let mut ax_buckets: Vec<Vec<AxisSeg>> = vec![Vec::new(); ax_w * ax_h];

    let ax_cell_x = |x: f64| -> usize {
        let v = if seamless {
            (mod_f64(x, sheet_w) / ax_step).floor()
        } else {
            ((x - dom_x0) / ax_step).floor()
        };
        clamp(v, 0.0, (ax_w - 1) as f64) as usize
    };
    let ax_cell_y = |y: f64| -> usize {
        let v = if seamless {
            (mod_f64(y, sheet_h) / ax_step).floor()
        } else {
            ((y - dom_y0) / ax_step).floor()
        };
        clamp(v, 0.0, (ax_h - 1) as f64) as usize
    };

    for (as_idx, st) in raw_streams.iter().enumerate() {
        let g_a = &streams[st.geom_idx];
        let wv = g_a.w_verts.as_ref().unwrap();
        for aci in 0..g_a.m {
            let pa = g_a.pts[aci];
            let pb = g_a.pts[aci + 1];
            let w_seg_a = 0.5 * (wv[aci] + wv[aci + 1]);
            let mx = 0.5 * (pa.x + pb.x);
            let my = 0.5 * (pa.y + pb.y);
            let idx = ax_cell_y(my) * ax_w + ax_cell_x(mx);
            ax_buckets[idx].push(AxisSeg {
                ax: pa.x,
                ay: pa.y,
                bx: pb.x,
                by: pb.y,
                mx,
                my,
                s: as_idx,
                ci: aci,
                w_loc: w_seg_a,
            });
        }
    }

    let nearest_earlier_clear_ratio = |px: f64, py: f64, w_self: f64, self_idx: usize, self_vi: usize, self_m: usize, self_closed: bool| -> f64 {
        let cx0 = ax_cell_x(px) as i64;
        let cy0 = ax_cell_y(py) as i64;
        let ax_w_i = ax_w as i64;
        let ax_h_i = ax_h as i64;
        let mut best_ratio = 4.0f64;
        for dy_b in -1i64..=1 {
            let ny_b = if seamless { mod_i(cy0 + dy_b, ax_h_i) } else { cy0 + dy_b };
            if ny_b < 0 || ny_b >= ax_h_i {
                continue;
            }
            for dx_b in -1i64..=1 {
                let nx_b = if seamless { mod_i(cx0 + dx_b, ax_w_i) } else { cx0 + dx_b };
                if nx_b < 0 || nx_b >= ax_w_i {
                    continue;
                }
                let bucket = &ax_buckets[(ny_b as usize) * ax_w + (nx_b as usize)];
                for smp in bucket {
                    if smp.s > self_idx {
                        continue;
                    }
                    if smp.s == self_idx {
                        let mut dci = smp.ci.abs_diff(self_vi);
                        if self_closed {
                            dci = dci.min(self_m - dci);
                        }
                        if dci < 6 {
                            continue;
                        }
                    }
                    let mut ax_p = smp.ax;
                    let mut ay_p = smp.ay;
                    let mut bx_p = smp.bx;
                    let mut by_p = smp.by;
                    if seamless {
                        let dm_x = mod_f64(smp.mx - px + sheet_w * 0.5, sheet_w) - sheet_w * 0.5;
                        let dm_y = mod_f64(smp.my - py + sheet_h * 0.5, sheet_h) - sheet_h * 0.5;
                        let sh_x = (px + dm_x) - smp.mx;
                        let sh_y = (py + dm_y) - smp.my;
                        ax_p += sh_x;
                        ay_p += sh_y;
                        bx_p += sh_x;
                        by_p += sh_y;
                    }
                    let d2 = pt_seg_dist2(px, py, ax_p, ay_p, bx_p, by_p);
                    let w_pair = w_self.max(smp.w_loc);
                    let ratio = d2.sqrt() / w_pair.max(1.0);
                    if ratio < best_ratio {
                        best_ratio = ratio;
                    }
                }
            }
        }
        best_ratio
    };

    let mut primary_stream_count = 0usize;
    let mut bg_fill_stream_count = 0usize;
    let mut unhidden_inside_ends = 0usize;
    let clear_thresh = 1.15;
    let mut stream_plans: Vec<StreamPlan> = Vec::with_capacity(raw_streams.len());
    let mut roof_z_grid = vec![0u32; total_cells];

    for (si2, st) in raw_streams.iter().enumerate() {
        let geom_s = &streams[st.geom_idx];
        let m = geom_s.m;
        let wv = geom_s.w_verts.as_ref().unwrap();
        let p_head = geom_s.pts[0];
        let p_tail = geom_s.pts[m];
        let head_inside = !st.closed_loop && ctx.inside_sheet(p_head.x, p_head.y, wv[0] * 0.45);
        let tail_inside = !st.closed_loop && ctx.inside_sheet(p_tail.x, p_tail.y, wv[m] * 0.45);

        let mut clear_d = vec![0.0f64; m + 1];
        for vci in 0..=m {
            clear_d[vci] = nearest_earlier_clear_ratio(
                geom_s.pts[vci].x,
                geom_s.pts[vci].y,
                wv[vci],
                st.idx,
                vci,
                m,
                st.closed_loop,
            );
        }
        let joint_clear = |k: usize| -> f64 {
            let km1 = if k > 0 { k - 1 } else if st.closed_loop { m - 1 } else { 0 };
            clear_d[k].min(clear_d[km1])
        };

        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut tiers: Vec<u8> = Vec::new();
        let mut c_rng = Mulberry32::new(st.seed ^ 0x9e37_79b1);

        let mut head_split = 0usize;
        let mut tail_split = m;

        if st.bg_fill {
            bg_fill_stream_count += 1;
            let mut cb0 = 0usize;
            while cb0 < m {
                let eb0 = if m - cb0 <= 8 { m } else { cb0 + 6 };
                ranges.push((cb0, eb0 - 1));
                tiers.push(1);
                cb0 = eb0;
            }
        } else {
            primary_stream_count += 1;
            if head_inside {
                head_split = m;
                if m >= 6 {
                    for hk in 3..=(m - 3) {
                        if joint_clear(hk) >= clear_thresh {
                            head_split = hk;
                            break;
                        }
                    }
                }
            }
            if tail_inside {
                tail_split = 0;
                let lo_tk = (head_split + 3).max(3);
                if m >= 3 && m - 3 >= lo_tk {
                    for tk in (lo_tk..=(m - 3)).rev() {
                        if joint_clear(tk) >= clear_thresh {
                            tail_split = tk;
                            break;
                        }
                    }
                }
            }

            let can_rise_at = |k_rise: usize, b_end: usize| -> bool {
                if !tail_inside && !st.closed_loop && b_end.saturating_sub(k_rise) <= 18 {
                    return true;
                }
                if b_end >= 3 {
                    let lim_hi = (b_end - 3).min(k_rise + 18);
                    let kr_start = k_rise + 3;
                    if kr_start <= lim_hi {
                        for kr in kr_start..=lim_hi {
                            if joint_clear(kr) >= clear_thresh {
                                return true;
                            }
                        }
                    }
                }
                if st.closed_loop && (m - k_rise) <= 18 && joint_clear(0) >= clear_thresh {
                    return true;
                }
                false
            };

            let mut cur_tier: u8 = if head_inside || !can_rise_at(0, tail_split) {
                1
            } else if (st.parity & 1) == 1 {
                2
            } else {
                1
            };

            if (head_inside || tail_inside) && head_split >= tail_split {
                let mut c0 = 0usize;
                while c0 < m {
                    let e0 = if m - c0 <= 8 { m } else { c0 + 6 };
                    ranges.push((c0, e0 - 1));
                    tiers.push(1);
                    c0 = e0;
                }
            } else {
                if head_inside && head_split > 0 {
                    let mut ch0 = 0usize;
                    while ch0 < head_split {
                        let eh0 = if head_split - ch0 <= 8 { head_split } else { ch0 + 6 };
                        ranges.push((ch0, eh0 - 1));
                        tiers.push(1);
                        ch0 = eh0;
                    }
                    cur_tier = if can_rise_at(head_split, tail_split) { 2 } else { 1 };
                }

                let mut c_cursor = head_split;
                let body_end = tail_split;
                while c_cursor < body_end {
                    let rem = body_end - c_cursor;
                    if rem <= 6 {
                        ranges.push((c_cursor, body_end - 1));
                        tiers.push(cur_tier);
                        break;
                    }
                    let win_lo = c_cursor + 3;
                    let win_hi = (body_end - 3).min(c_cursor + 7);
                    let target_k = c_cursor + 4 + (c_rng.next_f64() * 3.0).floor() as usize;
                    let mut best_k: Option<usize> = None;
                    let mut best_score = -1e9f64;
                    for ck_cand in win_lo..=win_hi {
                        let jc = joint_clear(ck_cand);
                        if jc >= clear_thresh {
                            let score = jc.min(1.6) - (ck_cand as f64 - target_k as f64).abs() * 0.03;
                            if score > best_score {
                                best_score = score;
                                best_k = Some(ck_cand);
                            }
                        }
                    }
                    if let Some(bk) = best_k {
                        ranges.push((c_cursor, bk - 1));
                        tiers.push(cur_tier);
                        if cur_tier == 2 {
                            cur_tier = 1;
                        } else if can_rise_at(bk, body_end) {
                            cur_tier = 2;
                        }
                        c_cursor = bk;
                    } else {
                        let mut next_clean: Option<usize> = None;
                        if body_end >= 3 && win_hi < body_end - 3 {
                            for sk in (win_hi + 1)..=(body_end - 3) {
                                if joint_clear(sk) >= clear_thresh {
                                    next_clean = Some(sk);
                                    break;
                                }
                            }
                        }
                        let span_end = next_clean.unwrap_or(body_end);
                        let mut sc = c_cursor;
                        while sc < span_end {
                            let se = if span_end - sc <= 8 { span_end } else { sc + 6 };
                            ranges.push((sc, se - 1));
                            tiers.push(cur_tier);
                            sc = se;
                        }
                        if let Some(nc) = next_clean {
                            if cur_tier == 2 {
                                cur_tier = 1;
                            } else if can_rise_at(nc, body_end) {
                                cur_tier = 2;
                            }
                        }
                        c_cursor = span_end;
                    }
                }

                if tail_inside && tail_split < m {
                    let mut ct0 = tail_split;
                    while ct0 < m {
                        let et0 = if m - ct0 <= 8 { m } else { ct0 + 6 };
                        ranges.push((ct0, et0 - 1));
                        tiers.push(1);
                        ct0 = et0;
                    }
                }
            }
        }

        if ranges.is_empty() {
            ranges.push((0, m - 1));
            tiers.push(if head_inside || tail_inside || st.bg_fill { 1 } else { 2 });
        }

        let n_ck = ranges.len();
        let step_z: u32 = 0x0000_4000;
        let s_clamp = (st.idx as u32).min(16000);
        let z_low_stream = 0x4800_0000u32.wrapping_sub((s_clamp + 1).wrapping_mul(step_z));
        let z_high_stream = 0x8000_0000u32.wrapping_add((s_clamp + 1).wrapping_mul(step_z));

        if st.closed_loop && n_ck > 1 && tiers[n_ck - 1] != tiers[0] && joint_clear(0) < clear_thresh {
            let wrap_target_tier = tiers[0];
            let mut w_back = n_ck - 1;
            let old_wrap_tier = tiers[w_back];
            while w_back > 0 && tiers[w_back] == old_wrap_tier {
                tiers[w_back] = wrap_target_tier;
                w_back -= 1;
            }
        }

        let mut z_list = Vec::with_capacity(n_ck);
        for &t_val in &tiers {
            z_list.push(if t_val == 2 { z_high_stream } else { z_low_stream });
        }

        let rz_lo = if st.closed_loop { 0 } else { 2 };
        let rz_hi = if st.closed_loop { m - 1 } else { m - 3 };
        for ck in 0..n_ck {
            let (rc_a, rc_b) = ranges[ck];
            let z_ck = z_list[ck];
            for rci in rc_a..=rc_b {
                if rci >= rz_lo && rci <= rz_hi {
                    let rmc = geom_s.chords[rci];
                    let w_roof_ch = 0.5 * (wv[rci] + wv[rci + 1]);
                    ctx.stamp_max_z_disk(&mut roof_z_grid, rmc.mx, rmc.my, w_roof_ch * 0.46, z_ck);
                }
            }
        }

        stream_plans.push(StreamPlan {
            st_idx: si2,
            ranges,
            z_list,
            head_inside,
            tail_inside,
        });
    }

    let mut chunks: Vec<NawaChunk> = Vec::new();
    for pl in &stream_plans {
        let st_p = &raw_streams[pl.st_idx];
        streams[st_p.geom_idx].head_tucked = pl.head_inside;
        streams[st_p.geom_idx].tail_tucked = pl.tail_inside;
        let geom_p = &streams[st_p.geom_idx];
        let wv_p = geom_p.w_verts.as_ref().unwrap();
        let n_ck_p = pl.ranges.len();

        if pl.head_inside {
            let ph0 = geom_p.pts[0];
            if ctx.inside_sheet(ph0.x, ph0.y, 0.0)
                && !ctx.is_under_higher_roof_z(
                    &roof_z_grid,
                    ph0.x,
                    ph0.y,
                    geom_p.chords[0].tx,
                    geom_p.chords[0].ty,
                    pl.z_list[0],
                    wv_p[0],
                )
            {
                unhidden_inside_ends += 1;
            }
        }
        if pl.tail_inside {
            let pt_e = geom_p.pts[geom_p.m];
            if ctx.inside_sheet(pt_e.x, pt_e.y, 0.0)
                && !ctx.is_under_higher_roof_z(
                    &roof_z_grid,
                    pt_e.x,
                    pt_e.y,
                    geom_p.chords[geom_p.m - 1].tx,
                    geom_p.chords[geom_p.m - 1].ty,
                    pl.z_list[n_ck_p - 1],
                    wv_p[geom_p.m],
                )
            {
                unhidden_inside_ends += 1;
            }
        }

        for ck_p in 0..n_ck_p {
            let (ci_a, ci_b) = pl.ranges[ck_p];
            let z_self = pl.z_list[ck_p];
            let has_prev_ck = ck_p > 0 || (st_p.closed_loop && n_ck_p > 1);
            let has_next_ck = ck_p + 1 < n_ck_p || (st_p.closed_loop && n_ck_p > 1);
            let prev_ck_idx = if ck_p > 0 { ck_p - 1 } else { n_ck_p - 1 };
            let next_ck_idx = if ck_p + 1 < n_ck_p { ck_p + 1 } else { 0 };

            let z_prev = if has_prev_ck { pl.z_list[prev_ck_idx] } else { 0 };
            let z_next = if has_next_ck { pl.z_list[next_ck_idx] } else { 0 };
            let own_start_tail = has_prev_ck && (z_self > z_prev || (z_self == z_prev && ck_p > prev_ck_idx));
            let own_end_tail = has_next_ck && (z_self > z_next || (z_self == z_next && ck_p > next_ck_idx));
            let head_tucked_flag = pl.head_inside && ci_a == 0;
            let tail_tucked_flag = pl.tail_inside && ci_b == geom_p.m - 1;

            let mut c_min_x = f64::INFINITY;
            let mut c_min_y = f64::INFINITY;
            let mut c_max_x = f64::NEG_INFINITY;
            let mut c_max_y = f64::NEG_INFINITY;
            for vp_idx in ci_a..=(ci_b + 1) {
                let vp_c = geom_p.pts[vp_idx];
                if vp_c.x < c_min_x { c_min_x = vp_c.x; }
                if vp_c.x > c_max_x { c_max_x = vp_c.x; }
                if vp_c.y < c_min_y { c_min_y = vp_c.y; }
                if vp_c.y > c_max_y { c_max_y = vp_c.y; }
            }
            let bb_pad = w * 1.8;
            c_min_x -= bb_pad;
            c_min_y -= bb_pad;
            c_max_x += bb_pad;
            c_max_y += bb_pad;

            if !seamless {
                if c_max_x < -w || c_min_x > sheet_w + w || c_max_y < -w || c_min_y > sheet_h + w {
                    continue;
                }
                chunks.push(NawaChunk {
                    geom_idx: st_p.geom_idx,
                    ci_from: ci_a,
                    ci_to: ci_b,
                    z_key: z_self,
                    gy: 0,
                    gx: st_p.idx as i64,
                    ck: ck_p,
                    stream_idx: st_p.idx,
                    seed: st_p.seed,
                    clip_start_at_s: has_prev_ck,
                    clip_end_at_s: has_next_ck,
                    own_start_tail,
                    own_end_tail,
                    head_tucked: head_tucked_flag,
                    tail_tucked: tail_tucked_flag,
                    shift_x: 0.0,
                    shift_y: 0.0,
                    min_x: c_min_x,
                    min_y: c_min_y,
                    max_x: c_max_x,
                    max_y: c_max_y,
                });
            } else {
                let kx_min = (-c_max_x / sheet_w).floor() as i64;
                let kx_max = ((sheet_w - c_min_x) / sheet_w).floor() as i64;
                let ky_min = (-c_max_y / sheet_h).floor() as i64;
                let ky_max = ((sheet_h - c_min_y) / sheet_h).floor() as i64;
                for ky in ky_min..=ky_max {
                    let s_off_y = ky as f64 * sheet_h;
                    for kx in kx_min..=kx_max {
                        let s_off_x = kx as f64 * sheet_w;
                        chunks.push(NawaChunk {
                            geom_idx: st_p.geom_idx,
                            ci_from: ci_a,
                            ci_to: ci_b,
                            z_key: z_self,
                            gy: ky,
                            gx: kx,
                            ck: ck_p,
                            stream_idx: st_p.idx,
                            seed: st_p.seed,
                            clip_start_at_s: has_prev_ck,
                            clip_end_at_s: has_next_ck,
                            own_start_tail,
                            own_end_tail,
                            head_tucked: head_tucked_flag,
                            tail_tucked: tail_tucked_flag,
                            shift_x: s_off_x,
                            shift_y: s_off_y,
                            min_x: c_min_x + s_off_x,
                            min_y: c_min_y + s_off_y,
                            max_x: c_max_x + s_off_x,
                            max_y: c_max_y + s_off_y,
                        });
                    }
                }
            }
        }
    }

    // Painter's order: strictly unique composite key (z_key, stream_idx, ck, gy, gx)
    chunks.sort_unstable_by_key(|c| (c.z_key, c.stream_idx, c.ck, c.gy, c.gx));
    let chunk_count = chunks.len();
    let swarm_cache = (0..streams.len()).map(|_| OnceLock::new()).collect();
    NawaSkeleton {
        streams,
        chunks,
        swarm_cache,
        stats: NawaSkeletonStats {
            stream_count: raw_streams.len(),
            primary_stream_count,
            bg_fill_stream_count,
            chunk_count,
            unhidden_inside_ends,
        },
    }
}
