//! CPU oracle & offscreen renderer for Manga SFX layers (`L3`, wasm-pure).
//!
//! Full Rust port of the Manga SFX rendering pipeline:
//! - 2-pass Halo-then-Ink compositing (plus Vector Shadow / 3D Extrude & legacy `SfxStylePass` stack)
//! - 6 stylized Line/Outline engines (`Clean`, `OneBit`, curvature-clamped `Spikes`,
//!   `GPenNib` & `BrushStroke` with `#` corner cross-overs, and `Shtack` in
//!   `SaiPenSketch` / `DryPencil` modes built directly from the vector glyph contours
//!   (no raster skeleton downsampling), with proportional needle overshoots, guaranteed
//!   compact-contour preservation and several independent tapered sketch passes per stroke)
//! - 4 Rust-ready Dry Brush engines (`SdfPhysics`, `SdfSeg`, `SdfEtf` + LIC, `Vector`)
//! - Procedural screentone sheets (`photocraft-algo`) and `NegativeInvert` (`BlendMode::Difference`).

use std::f64::consts::PI;
use std::sync::Mutex;

use photocraft_algo::comic_screentone::{ScreentoneMode, render_screentone_to_surface};
use photocraft_color::PixelFormat;
use photocraft_doc::sfx::{
    SfxBlendEffect, SfxChannelBlend, SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineEffect, SfxLineJoin, SfxLineStyle,
    SfxShtackFillMode, SfxShtackFillSpec, SfxShtackFlowOvershoot, SfxShtackOvershootMode, SfxShtackVariant, SfxSpikeShape, SfxStrokePosition,
    SfxStylePass,
};
use photocraft_doc::{FillRule, LineCap, LineJoin, PathOp};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use photocraft_text::TextEngine;
use photocraft_vector::{Polyline, Rasterizer, StrokeStyle, stroke_polygons};
use web_time::Instant;

use crate::sfx_geom::{PreparedSfx, build_sfx_geometry};
use crate::sfx_stroke_flow::{
    FlowEndKind, FlowStroke, catmull_rom_resample_flow, catmull_rom_sample_closed_sweep, extract_glyph_flow_strokes,
};

/// A closed ink polygon in document space.
type InkPolygon = Vec<(f64, f64)>;

const MAX_RENDER_EXTENT: i32 = 8192;
const MAX_RENDER_PIXELS: usize = 16 * 1024 * 1024;
/// Hard safety ceiling for the *returned* document-space rect. The surface stays sparse: only
/// proxy-derived ink rows are written, so a gigapixel layer does not allocate a gigapixel buffer.
const MAX_FULL_RENDER_EXTENT: i32 = 65_536;

/// Fast interactive proxy ceiling used while a slider or gizmo handle is being dragged.
/// The committed (mouse-release) render uses the full 4096 px / 16 MP ceilings instead.
const PREVIEW_MAX_SIDE: i32 = 420;
const PREVIEW_MAX_PIXELS: usize = 135_000;

/// Pure vector contour with cumulative arc-length `pts: [x, y, s_arc]` for GPU JFA curvilinear `(s, d)`.
#[derive(Clone, Debug)]
pub struct SfxSegContourData {
    pub pts: Vec<[f32; 3]>,
    pub total_len: f32,
    pub phase: f32,
}

/// Document-space closed polygon ring `(x, y)`.
pub type SfxPolyRing = Vec<(f64, f64)>;

/// Pure O(vertices) vector scene extracted from [`SfxData`] (~1–4 ms regardless of 12K×17K canvas size).
/// Contains ZERO pixel coverage buffers — ready to feed directly into `photocraft_gpu::SfxGpuRenderer`.
#[derive(Clone, Debug)]
pub struct SfxVectorScene {
    pub full_bounds: Rect,
    pub geom: PreparedSfx,
    /// Glyph fill contours (font winding preserved for NonZero Stencil-then-Cover on GPU).
    pub face_contours: Vec<SfxPolyRing>,
    /// Stylized outline / Shtack / Spikes / G-Pen / BrushStroke ink polygons (positive winding).
    pub line_polygons: Vec<SfxPolyRing>,
    /// Primary `ShtackHatch` (`Hatch` / `StrokeFlow` SAI / `UkiyoBrush`) ink polygons (positive winding).
    pub primary_shtack_polygons: Vec<SfxPolyRing>,
    /// Legacy secondary `ShtackHatch` ink polygons (positive winding).
    pub secondary_shtack_polygons: Vec<SfxPolyRing>,
    /// Per-effect `ShtackHatch` ink polygons indexed by `data.blending.effects` index.
    pub effect_shtack_polygons: Vec<(usize, Vec<SfxPolyRing>)>,
    /// Smoothed closed glyph contours with arc-length `s` for GPU JFA `Б1: Seg` dry brush.
    pub seg_contours: Vec<SfxSegContourData>,
    /// Primary `DryBrushEngine::Vector` cut polygons (positive winding).
    pub primary_dry_vector_cuts: Vec<SfxPolyRing>,
    /// Per-effect `DryBrushEngine::Vector` cut polygons indexed by `data.blending.effects` index.
    pub effect_dry_vector_cuts: Vec<(usize, Vec<SfxPolyRing>)>,
}

/// Output of rendering an [`SfxData`] stack into document-space surfaces.
#[derive(Clone, Debug)]
pub enum SfxRenderOutput {
    /// Normal SFX rendered into a single raster surface.
    Single { surface: Surface, bounds: Rect, geom: PreparedSfx },
    /// Negative/Invert SFX: `under` (Normal strokes/extrude/shadow) + `invert` (Difference silhouette).
    InvertPair { under: Surface, invert: Surface, bounds: Rect, geom: PreparedSfx },
}

impl SfxRenderOutput {
    pub fn bounds(&self) -> Rect {
        match self {
            Self::Single { bounds, .. } | Self::InvertPair { bounds, .. } => *bounds,
        }
    }

    pub fn geom(&self) -> &PreparedSfx {
        match self {
            Self::Single { geom, .. } | Self::InvertPair { geom, .. } => geom,
        }
    }
}

// =============================================================================
// 1. DETERMINISTIC HASH, BOX-MULLER GAUSSIAN, VALUE NOISE & FELZENSZWALB EDT
// =============================================================================

#[inline]
fn hash_u32(n: u32) -> f32 {
    let mut x = n ^ 0x9E37_79B9;
    x = (x ^ (x >> 16)).wrapping_mul(0x85EB_CA6B);
    x = (x ^ (x >> 13)).wrapping_mul(0xC2B2_AE35);
    let out = x ^ (x >> 16);
    (out as f32) / 4_294_967_296.0
}

#[inline]
fn box_muller_gaussian(seed1: u32, seed2: u32) -> f32 {
    let u1 = hash_u32(seed1).max(1e-7);
    let u2 = hash_u32(seed2);
    (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
}

fn value_noise_1d(x: f32, seed: u32) -> f32 {
    let i = x.floor() as i32;
    let f = x - i as f32;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash_u32((i as u32).wrapping_mul(374_761_393).wrapping_add(seed));
    let b = hash_u32(((i + 1) as u32).wrapping_mul(374_761_393).wrapping_add(seed));
    a + (b - a) * u
}

fn value_noise_2d(x: f32, y: f32, seed: u32) -> f32 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let ux = fx * fx * (3.0 - 2.0 * fx);
    let uy = fy * fy * (3.0 - 2.0 * fy);
    let h = |px: i32, py: i32| -> f32 {
        hash_u32((px as u32).wrapping_mul(374_761_393).wrapping_add((py as u32).wrapping_mul(668_265_263)).wrapping_add(seed))
    };
    let a = h(ix, iy);
    let b = h(ix + 1, iy);
    let c = h(ix, iy + 1);
    let d = h(ix + 1, iy + 1);
    (a + (b - a) * ux) * (1.0 - uy) + (c + (d - c) * ux) * uy
}

fn fbm_2d(x: f32, y: f32, seed: u32) -> f32 {
    value_noise_2d(x, y, seed) * 0.65 + value_noise_2d(x * 2.03 + 17.1, y * 2.03 - 9.4, seed.wrapping_add(101)) * 0.35
}

/// Crisp paper tooth: a 1px micro-grid plus a per-pixel hash speckle over a medium fiber.
/// Unlike a single smooth octave this has hard, pixel-scale intensity steps, so at high
/// sharpness the dry-brush threshold punches clean holes instead of a soft gray haze.
fn crisp_paper_grain(x: f32, y: f32, seed: u32) -> f32 {
    let micro = value_noise_2d(x * 1.15, y * 1.15, seed);
    let ix = x.round() as i32;
    let iy = y.round() as i32;
    let speckle = hash_u32(
        (ix as u32)
            .wrapping_mul(1_662_606_333)
            .wrapping_add((iy as u32).wrapping_mul(84_348_833))
            .wrapping_add(seed),
    );
    let fiber = value_noise_2d(x * 0.45, y * 0.45, seed.wrapping_add(0x9E37));
    (micro * 0.55 + speckle * 0.25 + fiber * 0.20).clamp(0.0, 1.0)
}

fn edt_1d(f: &[f32], d: &mut [f32], v: &mut [usize], z: &mut [f32], n: usize) {
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = -1e18;
    z[1] = 1e18;
    for q in 1..n {
        let qf = q as f32;
        let mut s = ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32) * (v[k] as f32))) / (2.0 * qf - 2.0 * v[k] as f32);
        while s <= z[k] && k > 0 {
            k -= 1;
            s = ((f[q] + qf * qf) - (f[v[k]] + (v[k] as f32) * (v[k] as f32))) / (2.0 * qf - 2.0 * v[k] as f32);
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = 1e18;
    }
    k = 0;
    for (q, dst) in d.iter_mut().enumerate().take(n) {
        let qf = q as f32;
        while z[k + 1] < qf {
            k += 1;
        }
        let dx = qf - v[k] as f32;
        *dst = dx * dx + f[v[k]];
    }
}

/// 2-pass Felzenszwalb Euclidean Distance Transform returning interior distance (px) to mask boundary.
pub(crate) fn compute_mask_sdf(cov: &[f32], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    if n == 0 {
        return Vec::new();
    }
    const INF: f32 = 1e12;
    let mut grid = vec![INF; n];
    for y in 0..h {
        for x in 0..w {
            let idx = y * w + x;
            let inside = cov[idx] >= 0.45;
            if !inside || x == 0 || y == 0 || x + 1 == w || y + 1 == h {
                grid[idx] = 0.0;
            }
        }
    }

    let max_dim = w.max(h);
    let mut f = vec![0.0_f32; max_dim];
    let mut d = vec![0.0_f32; max_dim];
    let mut v = vec![0usize; max_dim];
    let mut z = vec![0.0_f32; max_dim + 1];

    // Column pass
    for x in 0..w {
        for y in 0..h {
            f[y] = grid[y * w + x];
        }
        edt_1d(&f, &mut d, &mut v, &mut z, h);
        for y in 0..h {
            grid[y * w + x] = d[y];
        }
    }
    // Row pass
    for y in 0..h {
        let row = y * w;
        f[..w].copy_from_slice(&grid[row..(row + w)]);
        edt_1d(&f, &mut d, &mut v, &mut z, w);
        for x in 0..w {
            grid[row + x] = d[x].max(0.0).sqrt();
        }
    }
    grid
}

// =============================================================================
// 2. GEOMETRY HELPERS: RDP, EDGE SPLITTING & TAPERED POLYGON STRIPS
// =============================================================================

fn dist_pt_to_seg(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-9 {
        return (p.0 - a.0).hypot(p.1 - a.1);
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(0.0, 1.0);
    (p.0 - (a.0 + dx * t)).hypot(p.1 - (a.1 + dy * t))
}

fn rdp_open(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let a = pts[0];
    let b = pts[pts.len() - 1];
    let mut max_d = -1.0_f64;
    let mut idx = 0usize;
    for (i, &pt) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = dist_pt_to_seg(pt, a, b);
        if d > max_d {
            max_d = d;
            idx = i;
        }
    }
    if max_d > eps {
        let mut left = rdp_open(&pts[..=idx], eps);
        let right = rdp_open(&pts[idx..], eps);
        left.pop();
        left.extend_from_slice(&right);
        left
    } else {
        vec![a, b]
    }
}

fn rdp_closed(pts: &[(f64, f64)], eps: f64) -> Vec<(f64, f64)> {
    if pts.len() <= 4 {
        return pts.to_vec();
    }
    let mut i_a = 0usize;
    let mut best_d = -1.0_f64;
    for (i, &p) in pts.iter().enumerate() {
        let d = (p.0 - pts[0].0).hypot(p.1 - pts[0].1);
        if d > best_d {
            best_d = d;
            i_a = i;
        }
    }
    let mut i_b = 0usize;
    best_d = -1.0;
    for (i, &p) in pts.iter().enumerate() {
        let d = (p.0 - pts[i_a].0).hypot(p.1 - pts[i_a].1);
        if d > best_d {
            best_d = d;
            i_b = i;
        }
    }
    if i_a == i_b {
        return pts.to_vec();
    }
    let (lo, hi) = if i_a < i_b { (i_a, i_b) } else { (i_b, i_a) };
    let mut seg1 = rdp_open(&pts[lo..=hi], eps);
    let mut wrapped = Vec::with_capacity(pts.len() - hi + lo + 1);
    wrapped.extend_from_slice(&pts[hi..]);
    wrapped.extend_from_slice(&pts[..=lo]);
    let mut seg2 = rdp_open(&wrapped, eps);
    seg1.pop();
    seg2.pop();
    seg1.extend_from_slice(&seg2);
    seg1
}

/// Splits a closed contour into clean straight/curved edges at sharp corners (`turn >= corner_turn_rad`).
fn split_contour_into_edges(raw_pts: &[(f64, f64)], rdp_eps: f64, corner_turn_rad: f64) -> Vec<Vec<(f64, f64)>> {
    let simp = rdp_closed(raw_pts, rdp_eps);
    let n = simp.len();
    if n < 3 {
        return Vec::new();
    }
    let mut corners = Vec::new();
    for i in 0..n {
        let prev = simp[(i + n - 1) % n];
        let cur = simp[i];
        let next = simp[(i + 1) % n];
        let v1 = (cur.0 - prev.0, cur.1 - prev.1);
        let v2 = (next.0 - cur.0, next.1 - cur.1);
        let l1 = v1.0.hypot(v1.1);
        let l2 = v2.0.hypot(v2.1);
        if l1 > 1e-4 && l2 > 1e-4 {
            let dot = ((v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2)).clamp(-1.0, 1.0);
            if dot.acos() >= corner_turn_rad {
                corners.push(i);
            }
        }
    }
    if corners.is_empty() {
        let mut closed = simp;
        if let Some(&first) = closed.first() {
            closed.push(first);
        }
        return vec![closed];
    }
    let mut edges = Vec::with_capacity(corners.len());
    for k in 0..corners.len() {
        let start = corners[k];
        let end = corners[(k + 1) % corners.len()];
        let mut edge = Vec::new();
        let mut idx = start;
        for _ in 0..=n {
            edge.push(simp[idx]);
            if idx == end && edge.len() >= 2 {
                break;
            }
            idx = (idx + 1) % n;
        }
        if edge.len() >= 2 {
            edges.push(edge);
        }
    }
    edges
}

/// Builds a closed anti-aliased tapered ribbon polygon from a sequence of `(x, y, half_width)` samples.
/// The emitted ring always has POSITIVE winding and never folds back on itself in a tight bend.
fn ribbon_polygon_from_samples(samples: &[(f64, f64, f64)]) -> Option<Vec<(f64, f64)>> {
    let n = samples.len();
    if n < 2 {
        return None;
    }
    let mut left: Vec<(f64, f64)> = Vec::with_capacity(n);
    let mut right: Vec<(f64, f64)> = Vec::with_capacity(n);
    let mut kept: Vec<usize> = Vec::with_capacity(n);
    for i in 0..n {
        let prev = samples[i.saturating_sub(1)];
        let next = samples[(i + 1).min(n - 1)];
        let tx = next.0 - prev.0;
        let ty = next.1 - prev.1;
        let len = tx.hypot(ty);
        if len < 1e-6 || !len.is_finite() {
            continue;
        }
        let nx = -ty / len;
        let ny = tx / len;
        let hw = samples[i].2.max(0.15);
        let lp = (samples[i].0 + nx * hw, samples[i].1 + ny * hw);
        let rp = (samples[i].0 - nx * hw, samples[i].1 - ny * hw);
        if !lp.0.is_finite() || !lp.1.is_finite() || !rp.0.is_finite() || !rp.1.is_finite() {
            continue;
        }
        left.push(lp);
        right.push(rp);
        kept.push(i);
    }
    if left.len() < 2 {
        return None;
    }
    // In a concave cusp (e.g. the middle of `B`) half_w can exceed the local radius of
    // curvature and a side boundary moves BACKWARDS against the spine. Clamp such a point
    // onto its predecessor so the ribbon never forms an inverted, self-intersecting loop.
    for i in 1..left.len() {
        let s0 = samples[kept[i - 1]];
        let s1 = samples[kept[i]];
        let sx = s1.0 - s0.0;
        let sy = s1.1 - s0.1;
        let dlx = left[i].0 - left[i - 1].0;
        let dly = left[i].1 - left[i - 1].1;
        if dlx * sx + dly * sy < 0.0 {
            left[i] = left[i - 1];
        }
        let drx = right[i].0 - right[i - 1].0;
        let dry = right[i].1 - right[i - 1].1;
        if drx * sx + dry * sy < 0.0 {
            right[i] = right[i - 1];
        }
    }
    let mut ring = left;
    ring.extend(right.into_iter().rev());
    // `left + right.rev()` is CW for a CCW spine; unify winding so NonZero combine never
    // treats overlapping sketch passes as opposing rings.
    if signed_area(&ring) < 0.0 {
        ring.reverse();
    }
    Some(ring)
}

/// Builds a sharp razor triangle / tapered bristle polygon from `base` along `dir` (`length`, `half_w`).
fn razor_bristle_poly(base: (f64, f64), dir: (f64, f64), length: f64, half_w: f64) -> Vec<(f64, f64)> {
    let nx = -dir.1;
    let ny = dir.0;
    let tip = (base.0 + dir.0 * length, base.1 + dir.1 * length);
    vec![
        (base.0 + nx * half_w, base.1 + ny * half_w),
        tip,
        (base.0 - nx * half_w, base.1 - ny * half_w),
    ]
}

fn signed_area(pts: &[(f64, f64)]) -> f64 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        acc += a.0 * b.1 - b.0 * a.1;
    }
    acc * 0.5
}

fn outward_normal(pts: &[(f64, f64)], i: usize, ccw: bool) -> (f64, f64) {
    let n = pts.len();
    let prev = pts[(i + n - 1) % n];
    let next = pts[(i + 1) % n];
    let tx = next.0 - prev.0;
    let ty = next.1 - prev.1;
    let len = tx.hypot(ty).max(1e-6);
    if ccw { (ty / len, -tx / len) } else { (-ty / len, tx / len) }
}

// =============================================================================
// 3. BASIC RASTERIZER, BOUNDS & COMPOSITING HELPERS
// =============================================================================

fn to_line_join(j: SfxLineJoin) -> LineJoin {
    match j {
        SfxLineJoin::Miter => LineJoin::Miter,
        SfxLineJoin::Round => LineJoin::Round,
        SfxLineJoin::Bevel => LineJoin::Bevel,
    }
}

fn compute_render_bounds(data: &SfxData, geom: &PreparedSfx) -> Rect {
    if geom.is_empty() {
        return Rect::EMPTY;
    }
    let le = &data.line_effect;
    let mut stroke_pad: f64 = f64::from(le.halo_width_px.max(0.0)) + f64::from(le.line_width_px.max(0.0));
    stroke_pad += match le.style {
        SfxLineStyle::Clean => 0.0,
        SfxLineStyle::OneBit => f64::from(le.one_bit_intensity) * 0.05 + 2.0,
        SfxLineStyle::Spikes => f64::from(le.spike_length_px) * 1.45 + f64::from(le.spike_size_px) * 0.5,
        SfxLineStyle::GPenNib => f64::from(le.gpen_overshoot_px) + f64::from(le.line_width_px) * 0.8 + 4.0,
        SfxLineStyle::BrushStroke => f64::from(le.ink_brush_overshoot_px) + f64::from(le.line_width_px) * 0.9 + 4.0,
        // Needle overshoots are randomized up to ~1.5x the slider value; keep long SAI
        // needles plus the round EDT halo fully inside the surface (never clipped).
        SfxLineStyle::Shtack => {
            // Needles are a percentage of glyph height, so the pad must grow with the lettering.
            let gh = (geom.warped_box[3] - geom.warped_box[1]).abs().max(24.0);
            let ratio = f64::from(le.shtack_overshoot_px).clamp(0.0, 60.0) / 100.0;
            let needle = gh * ratio * if le.shtack_variant == SfxShtackVariant::DryPencil { 0.48 } else { 1.0 };
            // Thickness is a percentage of glyph height: the FULL stroke width is gh * pct/100,
            // with half that on each side of the spine plus the taper/whisker lateral offsets.
            let thickness_pct = f64::from(le.shtack_thickness_px).clamp(0.5, 8.0);
            needle * 1.6 + gh * (thickness_pct / 100.0) + 6.0
        }
    };
    let gh_box = (geom.warped_box[3] - geom.warped_box[1]).abs().max(24.0);
    let hatch_pad = |fill: &SfxFillSource| -> f64 {
        if let SfxFillSource::ShtackHatch { spec } = fill {
            let max_len_scale = spec
                .flow_overrides
                .iter()
                .filter(|ov| ov.enabled)
                .map(|ov| f64::from(ov.length_scale.clamp(1.0, 3.0)))
                .fold(1.0_f64, f64::max);
            let ov_pct = f64::from(spec.overshoot_pct).clamp(0.0, 45.0).max(if spec.flow_overrides.iter().any(|ov| ov.enabled) { 14.0 } else { 0.0 });
            gh_box * (ov_pct / 100.0) * 1.55 * max_len_scale
                + gh_box * (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 100.0)
                + 6.0
        } else {
            0.0
        }
    };
    let mut fill_hatch_pad = hatch_pad(&data.fill).max(data.blending.secondary_fill.as_ref().map_or(0.0, hatch_pad));

    let mut min_dx: f64 = 0.0;
    let mut max_dx: f64 = 0.0;
    let mut min_dy: f64 = 0.0;
    let mut max_dy: f64 = 0.0;

    for fx in &data.blending.effects {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::Stroke { width_px, position, fill, .. } => {
                let w = f64::from((*width_px).max(0.0));
                match position {
                    SfxStrokePosition::Outside => stroke_pad += w,
                    SfxStrokePosition::Center => stroke_pad += w * 0.5,
                    SfxStrokePosition::Inside => {}
                }
                fill_hatch_pad = fill_hatch_pad.max(hatch_pad(fill));
            }
            SfxBlendEffect::FillOverlay { fill, .. } => {
                fill_hatch_pad = fill_hatch_pad.max(hatch_pad(fill));
            }
            SfxBlendEffect::OuterGlow { spread_px, blur_px, .. } => {
                stroke_pad += f64::from((*spread_px).max(0.0)) + f64::from((*blur_px).max(0.0)) * 2.5;
            }
            SfxBlendEffect::DropShadow { angle_deg, distance_px, extrude_depth_px, blur_px, .. } => {
                let rad = f64::from(*angle_deg).to_radians();
                let reach = f64::from((*distance_px).max(0.0)).max(f64::from((*extrude_depth_px).max(0.0)));
                let dx = rad.cos() * reach;
                let dy = rad.sin() * reach;
                let b = f64::from((*blur_px).max(0.0)) * 2.5;
                min_dx = min_dx.min(dx - b).min(-b);
                max_dx = max_dx.max(dx + b).max(b);
                min_dy = min_dy.min(dy - b).min(-b);
                max_dy = max_dy.max(dy + b).max(b);
            }
            SfxBlendEffect::Bevel { .. } | SfxBlendEffect::InnerShadow { .. } | SfxBlendEffect::InnerGlow { .. } => {}
        }
    }
    stroke_pad += fill_hatch_pad;

    if data.vector_shadow.enabled {
        let off = data.vector_shadow.offset_px();
        min_dx = min_dx.min(off[0]);
        max_dx = max_dx.max(off[0]);
        min_dy = min_dy.min(off[1]);
        max_dy = max_dy.max(off[1]);
        if data.vector_shadow.extrude_depth_px > 0.1 {
            let rad = f64::from(data.vector_shadow.angle_deg).to_radians();
            let ex = rad.cos() * f64::from(data.vector_shadow.extrude_depth_px);
            let ey = rad.sin() * f64::from(data.vector_shadow.extrude_depth_px);
            min_dx = min_dx.min(ex);
            max_dx = max_dx.max(ex);
            min_dy = min_dy.min(ey);
            max_dy = max_dy.max(ey);
        }
    }

    for pass in &data.styles {
        match pass {
            SfxStylePass::Stroke { enabled: true, width_px, miter_limit, join, .. } => {
                let m = if *join == SfxLineJoin::Miter { f64::from(miter_limit.clamp(1.0, 8.0)) * 0.5 } else { 1.0 };
                stroke_pad += f64::from((*width_px).max(0.0)) * m;
            }
            SfxStylePass::Extrude3D { enabled: true, dx_px, dy_px, .. } => {
                min_dx = min_dx.min(f64::from(*dx_px));
                max_dx = max_dx.max(f64::from(*dx_px));
                min_dy = min_dy.min(f64::from(*dy_px));
                max_dy = max_dy.max(f64::from(*dy_px));
            }
            SfxStylePass::Shadow { enabled: true, dx_px, dy_px, blur_px, .. } => {
                let b = f64::from((*blur_px).max(0.0)) * 2.5;
                min_dx = min_dx.min(f64::from(*dx_px) - b);
                max_dx = max_dx.max(f64::from(*dx_px) + b);
                min_dy = min_dy.min(f64::from(*dy_px) - b);
                max_dy = max_dy.max(f64::from(*dy_px) + b);
            }
            _ => {}
        }
    }

    let pad = stroke_pad + 8.0;
    let x0 = (geom.warped_box[0] + min_dx - pad).floor().clamp(-1e6, 1e6) as i32;
    let y0 = (geom.warped_box[1] + min_dy - pad).floor().clamp(-1e6, 1e6) as i32;
    let x1 = (geom.warped_box[2] + max_dx + pad).ceil().clamp(-1e6, 1e6) as i32;
    let y1 = (geom.warped_box[3] + max_dy + pad).ceil().clamp(-1e6, 1e6) as i32;
    if x1 <= x0 || y1 <= y0 {
        return Rect::EMPTY;
    }
    // Return the full document-space rect. Large layers are rendered through a bounded proxy and
    // upsampled in `render_sfx`; returning EMPTY here used to make >16 MP SFX layers vanish.
    let w = (x1 - x0).min(MAX_FULL_RENDER_EXTENT);
    let h = (y1 - y0).min(MAX_FULL_RENDER_EXTENT);
    Rect::from_xywh(x0, y0, w as u32, h as u32)
}

fn fill_rasterizer_from_polys(polylines: &[Polyline], shift: [f64; 2]) -> Rasterizer {
    let mut r = Rasterizer::new(false);
    let polys: Vec<Vec<(f64, f64)>> = polylines.iter().map(|pl| pl.pts.iter().map(|&(x, y)| (x + shift[0], y + shift[1])).collect()).collect();
    if !polys.is_empty() {
        r.add_component(&polys, PathOp::Combine, FillRule::NonZero);
    }
    r
}

fn rasterize_polygon_list(polys: &[Vec<(f64, f64)>], bounds: Rect) -> Vec<f32> {
    let n = bounds.width() as usize * bounds.height() as usize;
    if polys.is_empty() || n == 0 {
        return vec![0.0_f32; n];
    }
    // Stroke/ink polygons are all SOLID ink, never holes. A CW font `solid_core` combined
    // with a CCW tapered ribbon under NonZero cancels to 0 and punches a white star into
    // compact dots. Normalize every ring to positive winding before combining so overlaps
    // always accumulate ink instead of erasing each other. (Glyph fills keep their real
    // counter-winding in `fill_rasterizer_from_polys`; do not normalize there.)
    let mut normalized: Vec<Vec<(f64, f64)>> = Vec::with_capacity(polys.len());
    for poly in polys {
        let mut p = poly.clone();
        if signed_area(&p) < 0.0 {
            p.reverse();
        }
        normalized.push(p);
    }
    let mut r = Rasterizer::new(false);
    r.add_component(&normalized, PathOp::Combine, FillRule::NonZero);
    r.render(bounds)
}

fn expanded_rasterizer(polylines: &[Polyline], radius_px: f64, join: SfxLineJoin, miter_limit: f32) -> Rasterizer {
    let mut r = fill_rasterizer_from_polys(polylines, [0.0, 0.0]);
    if radius_px > 0.02 && !polylines.is_empty() {
        let style = StrokeStyle {
            width: radius_px * 2.0,
            cap: LineCap::Round,
            join: to_line_join(join),
            miter_limit: f64::from(miter_limit.clamp(1.0, 24.0)),
            dashes: Vec::new(),
            dash_offset: 0.0,
        };
        let stroke_polys = stroke_polygons(polylines, &style, 0.25);
        if !stroke_polys.is_empty() {
            r.add_component(&stroke_polys, PathOp::Combine, FillRule::NonZero);
        }
    }
    r
}

fn dilate_coverage_approx(cov: &[f32], w: usize, h: usize, radius_px: f32) -> Vec<f32> {
    let r = radius_px.round().clamp(0.0, 64.0) as isize;
    if r == 0 || cov.is_empty() || w == 0 || h == 0 {
        return cov.to_vec();
    }
    // Exact Euclidean/circular dilation via 2-pass Felzenszwalb EDT. The mask is padded
    // with an outside ring so the EDT grid border is never mistaken for ink: without it,
    // the halo would be full opacity along the entire layer edge (a box-like shovel
    // artifact) instead of a round rim around each sharp Shtack needle.
    let pad = (radius_px.ceil() as usize).clamp(2, 64) + 2;
    let (pw, ph) = (w + pad * 2, h + pad * 2);
    let mut inv = vec![1.0_f32; pw * ph];
    for y in 0..h {
        for x in 0..w {
            inv[(y + pad) * pw + (x + pad)] = if cov[y * w + x] >= 0.45 { 0.0 } else { 1.0 };
        }
    }
    let dist = compute_mask_sdf(&inv, pw, ph);
    let rf = radius_px.max(0.5);
    let mut out = Vec::with_capacity(cov.len());
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if cov[i] >= 0.45 {
                out.push(1.0);
            } else {
                let d = dist[(y + pad) * pw + (x + pad)];
                out.push((rf + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }
    out
}

fn paint_over(dst: &mut [[f32; 4]], cov: &[f32], src: &[[f32; 4]]) {
    for ((o, &c), s) in dst.iter_mut().zip(cov.iter()).zip(src.iter()) {
        let sa = s[3] * c.clamp(0.0, 1.0);
        if sa <= 1e-6 {
            continue;
        }
        let da = o[3];
        let a = sa + da * (1.0 - sa);
        if a > 1e-6 {
            for k in 0..3 {
                o[k] = (s[k] * sa + o[k] * da * (1.0 - sa)) / a;
            }
        }
        o[3] = a;
    }
}

fn paint_solid_over(dst: &mut [[f32; 4]], cov: &[f32], rgba: [f32; 4], opacity: f32) {
    for (o, &c) in dst.iter_mut().zip(cov.iter()) {
        let sa = rgba[3] * opacity * c.clamp(0.0, 1.0);
        if sa <= 1e-6 {
            continue;
        }
        let da = o[3];
        let a = sa + da * (1.0 - sa);
        if a > 1e-6 {
            for k in 0..3 {
                o[k] = (rgba[k] * sa + o[k] * da * (1.0 - sa)) / a;
            }
        }
        o[3] = a;
    }
}

fn color_rgba(c: photocraft_color::Color) -> [f32; 4] {
    let rgb = c.to_rgb();
    [rgb[0], rgb[1], rgb[2], c.alpha]
}

fn eval_fill_rgba(fill: &SfxFillSource, rect: Rect) -> Vec<[f32; 4]> {
    let n = rect.width() as usize * rect.height() as usize;
    match fill {
        SfxFillSource::None => vec![[0.0; 4]; n],
        SfxFillSource::Solid { color } | SfxFillSource::DryBrush { color, .. } => vec![color_rgba(*color); n],
        SfxFillSource::ShtackHatch { spec } => vec![color_rgba(spec.color); n],
        SfxFillSource::Gradient { start, end, angle_deg } => {
            let s_rgb = start.to_rgb();
            let e_rgb = end.to_rgb();
            let w = f64::from(rect.width()).max(1.0);
            let h = f64::from(rect.height()).max(1.0);
            let cx = f64::from(rect.x0) + w * 0.5;
            let cy = f64::from(rect.y0) + h * 0.5;
            let rad = f64::from(*angle_deg).to_radians();
            let (sin_a, cos_a) = rad.sin_cos();
            let span = (w * cos_a.abs() + h * sin_a.abs()).max(1.0);
            let rw = rect.width() as usize;
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let x = f64::from(rect.x0 + (i % rw) as i32) + 0.5 - cx;
                let y = f64::from(rect.y0 + (i / rw) as i32) + 0.5 - cy;
                let t = ((x * cos_a + y * sin_a) / span + 0.5).clamp(0.0, 1.0) as f32;
                out.push([
                    s_rgb[0] + (e_rgb[0] - s_rgb[0]) * t,
                    s_rgb[1] + (e_rgb[1] - s_rgb[1]) * t,
                    s_rgb[2] + (e_rgb[2] - s_rgb[2]) * t,
                    start.alpha + (end.alpha - start.alpha) * t,
                ]);
            }
            out
        }
        SfxFillSource::Screentone { spec, opacity } => {
            let mut s = Surface::new(PixelFormat::RGBA8);
            let mut sheet = spec.clone();
            sheet.mode = ScreentoneMode::Sheet;
            sheet.use_source_luminance = false;
            let _ = render_screentone_to_surface(&mut s, rect, &sheet, (rect.x0 as f32, rect.y0 as f32));
            let px = s.read_region(rect);
            let op = opacity.clamp(0.0, 1.0);
            let mut out = Vec::with_capacity(n);
            for c in px.chunks_exact(4) {
                out.push([c[0], c[1], c[2], c[3] * op]);
            }
            out
        }
    }
}

#[inline]
fn blend_channel_rgb(b: f32, s: f32, mode: SfxChannelBlend) -> f32 {
    match mode {
        SfxChannelBlend::Normal | SfxChannelBlend::Knockout => s,
        SfxChannelBlend::Multiply => b * s,
        SfxChannelBlend::ColorBurn => {
            if s <= 1e-6 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }
        SfxChannelBlend::Darken => b.min(s),
        SfxChannelBlend::Screen => 1.0 - (1.0 - b) * (1.0 - s),
        SfxChannelBlend::ColorDodge => {
            if s >= 1.0 - 1e-6 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }
        SfxChannelBlend::Lighten => b.max(s),
        SfxChannelBlend::Overlay => {
            if b < 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        SfxChannelBlend::SoftLight => {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }
        SfxChannelBlend::HardLight => {
            if s < 0.5 {
                2.0 * b * s
            } else {
                1.0 - 2.0 * (1.0 - b) * (1.0 - s)
            }
        }
        SfxChannelBlend::Difference => (b - s).abs(),
        SfxChannelBlend::Exclusion => b + s - 2.0 * b * s,
    }
}

#[inline]
fn composite_pixel_blend(d: &mut [f32; 4], src: [f32; 4], cov: f32, opacity: f32, mode: SfxChannelBlend) {
    let sa = (cov * src[3] * opacity).clamp(0.0, 1.0);
    if sa <= 1e-6 {
        return;
    }
    if mode == SfxChannelBlend::Knockout {
        d[3] *= 1.0 - sa;
        if d[3] <= 1e-6 {
            *d = [0.0; 4];
        }
        return;
    }
    let da = d[3];
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-6 {
        *d = [0.0; 4];
        return;
    }
    let inv_da = 1.0 - da;
    let inv_sa = 1.0 - sa;
    let sa_da = sa * da;
    for c in 0..3 {
        let bl = blend_channel_rgb(d[c], src[c], mode);
        d[c] = ((inv_da * sa * src[c] + inv_sa * da * d[c] + sa_da * bl) / out_a).clamp(0.0, 1.0);
    }
    d[3] = out_a;
}

fn composite_rgba_blend(dst: &mut [[f32; 4]], cov: &[f32], src: &[[f32; 4]], opacity: f32, mode: SfxChannelBlend) {
    let op = opacity.clamp(0.0, 1.0);
    if op <= 1e-6 {
        return;
    }
    for ((d, &c), &s) in dst.iter_mut().zip(cov.iter()).zip(src.iter()) {
        composite_pixel_blend(d, s, c, op, mode);
    }
}

fn composite_solid_blend(dst: &mut [[f32; 4]], cov: &[f32], color: [f32; 4], opacity: f32, mode: SfxChannelBlend) {
    let op = opacity.clamp(0.0, 1.0);
    if op <= 1e-6 {
        return;
    }
    for (d, &c) in dst.iter_mut().zip(cov.iter()) {
        composite_pixel_blend(d, color, c, op, mode);
    }
}

fn box_blur_1d(src: &[f32], w: usize, h: usize, radius: usize, horizontal: bool, dst: &mut [f32]) {
    if radius == 0 || w == 0 || h == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let r = radius as isize;
    let win = (radius * 2 + 1) as f32;
    if horizontal {
        for y in 0..h {
            let row = y * w;
            let mut sum = 0.0_f32;
            for k in -r..=r {
                let x = k.clamp(0, w as isize - 1) as usize;
                sum += src[row + x];
            }
            for x in 0..w {
                dst[row + x] = sum / win;
                let out_x = (x as isize - r).clamp(0, w as isize - 1) as usize;
                let in_x = (x as isize + r + 1).clamp(0, w as isize - 1) as usize;
                sum += src[row + in_x] - src[row + out_x];
            }
        }
    } else {
        for x in 0..w {
            let mut sum = 0.0_f32;
            for k in -r..=r {
                let y = k.clamp(0, h as isize - 1) as usize;
                sum += src[y * w + x];
            }
            for y in 0..h {
                dst[y * w + x] = sum / win;
                let out_y = (y as isize - r).clamp(0, h as isize - 1) as usize;
                let in_y = (y as isize + r + 1).clamp(0, h as isize - 1) as usize;
                sum += src[in_y * w + x] - src[out_y * w + x];
            }
        }
    }
}

fn blur_coverage(cov: &[f32], w: usize, h: usize, sigma_px: f32) -> Vec<f32> {
    let rad = (sigma_px.clamp(0.0, 128.0) * 0.75).round() as usize;
    if rad == 0 || cov.is_empty() {
        return cov.to_vec();
    }
    let mut a = vec![0.0_f32; cov.len()];
    let mut b = cov.to_vec();
    for _ in 0..2 {
        box_blur_1d(&b, w, h, rad, true, &mut a);
        box_blur_1d(&a, w, h, rad, false, &mut b);
    }
    for v in &mut b {
        *v = v.clamp(0.0, 1.0);
    }
    b
}

fn hash_grain(seed: u32, x: i32, y: i32) -> f32 {
    let mut h = seed ^ (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 16;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 15;
    (h & 0xFF) as f32 / 255.0
}

fn rgba_to_surface(rgba: &[[f32; 4]], rect: Rect, format: PixelFormat) -> Surface {
    let mut s = Surface::new(format);
    if rect.is_empty() || rgba.is_empty() {
        return s;
    }
    let ch = format.channels();
    let mut vals = vec![0.0_f32; rgba.len() * ch];
    let mut any = false;
    for (p, o) in rgba.iter().zip(vals.chunks_exact_mut(ch)) {
        if p[3] > 1e-6 {
            from_rgba_into(&format, *p, o);
            any = true;
        }
    }
    if any {
        s.write_region(rect, &vals);
        s.prune();
    }
    s
}

/// Proxy dimensions for a full document-space target. `proxy_scale == 1.0` keeps the exact
/// historical fast path; otherwise the target is bounded to `MAX_RENDER_EXTENT`² / `MAX_RENDER_PIXELS`.
fn proxy_target_with(bounds: Rect, max_side: i32, max_pixels: usize) -> (usize, usize, f64) {
    let tw = bounds.width() as usize;
    let th = bounds.height() as usize;
    let mut scale = 1.0_f64;
    if tw > max_side as usize {
        scale = scale.min(max_side as f64 / tw as f64);
    }
    if th > max_side as usize {
        scale = scale.min(max_side as f64 / th as f64);
    }
    if tw.saturating_mul(th) > max_pixels {
        let area_scale = (max_pixels as f64 / (tw.saturating_mul(th) as f64)).sqrt();
        scale = scale.min(area_scale);
    }
    if scale >= 1.0 {
        return (tw, th, 1.0);
    }
    let mut pw = ((tw as f64 * scale).round() as usize).clamp(1, max_side as usize);
    let mut ph = ((th as f64 * scale).round() as usize).clamp(1, max_side as usize);
    while pw.saturating_mul(ph) > max_pixels {
        if pw >= ph && pw > 1 {
            pw -= 1;
        } else if ph > 1 {
            ph -= 1;
        } else {
            break;
        }
    }
    (pw, ph, scale)
}

fn proxy_target(bounds: Rect) -> (usize, usize, f64) {
    proxy_target_with(bounds, MAX_RENDER_EXTENT, MAX_RENDER_PIXELS)
}

fn scale_box(box4: [f64; 4], origin: [f64; 2], scale: f64) -> [f64; 4] {
    [
        origin[0] + (box4[0] - origin[0]) * scale,
        origin[1] + (box4[1] - origin[1]) * scale,
        origin[0] + (box4[2] - origin[0]) * scale,
        origin[1] + (box4[3] - origin[1]) * scale,
    ]
}

/// Geometry mapped into the proxy buffer around the target's origin. Pixel-sized effect widths
/// are scaled separately in [`proxy_scaled_data`] so upsampled ink keeps its exact thickness.
fn scaled_geometry(geom: &PreparedSfx, origin: [f64; 2], scale: f64) -> PreparedSfx {
    let map_pt = |p: (f64, f64)| -> (f64, f64) {
        (origin[0] + (p.0 - origin[0]) * scale, origin[1] + (p.1 - origin[1]) * scale)
    };
    let map = |p: [f64; 2]| -> [f64; 2] {
        let q = map_pt((p[0], p[1]));
        [q.0, q.1]
    };
    let mut out = geom.clone();
    for pl in &mut out.polylines {
        for p in &mut pl.pts {
            *p = map_pt(*p);
        }
    }
    for g in &mut out.glyphs {
        for pl in &mut g.polylines {
            for p in &mut pl.pts {
                *p = map_pt(*p);
            }
        }
        g.rest_bounds = scale_box(g.rest_bounds, origin, scale);
        g.rest_center = map(g.rest_center);
        g.warped_bounds = scale_box(g.warped_bounds, origin, scale);
        g.warped_center = map(g.warped_center);
        for q in &mut g.quad_doc {
            *q = map(*q);
        }
    }
    out.rest_box = scale_box(out.rest_box, origin, scale);
    out.warped_box = scale_box(out.warped_box, origin, scale);
    for q in &mut out.world_corners {
        *q = map(*q);
    }
    out
}

/// Scales every pixel-sized effect width by `proxy_scale` for rendering into a downsampled proxy.
/// Without this, geometry shrinks by `proxy_scale` while halo/line/spike/shadow widths stay in
/// document pixels, so upsampled >16 MP lettering looks `1/proxy_scale` times fatter.
fn proxy_scaled_data(data: &SfxData, scale: f64) -> SfxData {
    let mut out = data.clone();
    let px = |v: f32| -> f32 { (f64::from(v) * scale).max(0.0) as f32 };
    let signed = |v: f32| -> f32 { (f64::from(v) * scale) as f32 };
    {
        let le = &mut out.line_effect;
        le.halo_width_px = px(le.halo_width_px);
        le.line_width_px = px(le.line_width_px);
        le.spike_length_px = px(le.spike_length_px);
        le.spike_size_px = px(le.spike_size_px);
        le.gpen_overshoot_px = px(le.gpen_overshoot_px);
        le.ink_brush_overshoot_px = px(le.ink_brush_overshoot_px);
        // `shtack_overshoot_px` and `shtack_thickness_px` are PERCENTAGES of the glyph height.
        // The proxy geometry already scales that height, so scaling either percentage here
        // would shrink the needles (and the stroke weight) a second time.

    }
    out.dry_brush.lic_length_px = px(out.dry_brush.lic_length_px);
    out.vector_shadow.distance_px = px(out.vector_shadow.distance_px);
    out.vector_shadow.extrude_depth_px = px(out.vector_shadow.extrude_depth_px);
    for pass in &mut out.styles {
        match pass {
            SfxStylePass::Stroke { width_px, .. } => *width_px = px(*width_px),
            SfxStylePass::Extrude3D { dx_px, dy_px, .. } => {
                *dx_px = signed(*dx_px);
                *dy_px = signed(*dy_px);
            }
            SfxStylePass::Shadow { dx_px, dy_px, blur_px, .. } => {
                *dx_px = signed(*dx_px);
                *dy_px = signed(*dy_px);
                *blur_px = px(*blur_px);
            }
            SfxStylePass::InnerGlow { width_px, .. } => *width_px = px(*width_px),
            SfxStylePass::NegativeInvert { .. } => {}
        }
    }
    let scale_fill_px = |fill: &mut SfxFillSource| {
        if let SfxFillSource::DryBrush { spec, .. } = fill {
            spec.lic_length_px = px(spec.lic_length_px);
        }
    };
    scale_fill_px(&mut out.fill);
    if let Some(sec) = &mut out.blending.secondary_fill {
        scale_fill_px(sec);
    }
    for fx in &mut out.blending.effects {
        match fx {
            SfxBlendEffect::Bevel { depth_px, .. } => *depth_px = px(*depth_px).max(1.0),
            SfxBlendEffect::Stroke { width_px, fill, .. } => {
                *width_px = px(*width_px);
                scale_fill_px(fill);
            }
            SfxBlendEffect::InnerShadow { distance_px, blur_px, .. } => {
                *distance_px = px(*distance_px);
                *blur_px = px(*blur_px);
            }
            SfxBlendEffect::DropShadow { distance_px, extrude_depth_px, blur_px, .. } => {
                *distance_px = px(*distance_px);
                *extrude_depth_px = px(*extrude_depth_px);
                *blur_px = px(*blur_px);
            }
            SfxBlendEffect::InnerGlow { width_px, .. } => *width_px = px(*width_px),
            SfxBlendEffect::FillOverlay { fill, .. } => scale_fill_px(fill),
            SfxBlendEffect::OuterGlow { spread_px, blur_px, .. } => {
                *spread_px = px(*spread_px);
                *blur_px = px(*blur_px);
            }
        }
    }
    out
}

/// Bilinear upsample of a bounded RGBA proxy onto the full clipped target rect.
///
/// Only rows/columns that contain proxy ink are written, so the returned `Surface` stays sparse
/// even when `bounds` spans tens of millions of pixels.
fn upsample_rgba_to_surface(proxy: &[[f32; 4]], pw: usize, ph: usize, target: Rect, format: PixelFormat) -> Surface {
    let mut s = Surface::new(format);
    if proxy.len() < pw.saturating_mul(ph) || target.is_empty() {
        return s;
    }
    let tw = target.width() as usize;
    let th = target.height() as usize;
    let mut sx0 = pw;
    let mut sy0 = ph;
    let mut sx1 = 0_usize;
    let mut sy1 = 0_usize;
    for y in 0..ph {
        for x in 0..pw {
            if proxy[y * pw + x][3] > 1e-6 {
                sx0 = sx0.min(x);
                sy0 = sy0.min(y);
                sx1 = sx1.max(x);
                sy1 = sy1.max(y);
            }
        }
    }
    if sx0 > sx1 || sy0 > sy1 {
        return s;
    }
    let first = |s: usize, sn: usize, tn: usize| -> usize {
        (((s as f32 + 0.5) * tn as f32 / sn as f32 - 0.5).floor() as isize).clamp(0, tn as isize) as usize
    };
    let last = |s: usize, sn: usize, tn: usize| -> usize {
        (((s as f32 + 0.5) * tn as f32 / sn as f32 + 0.5).ceil() as isize).clamp(0, tn as isize) as usize
    };
    let x0 = first(sx0.saturating_sub(1), pw, tw);
    let x1 = last((sx1 + 1).min(pw - 1), pw, tw).max(x0 + 1).min(tw);
    let y0 = first(sy0.saturating_sub(1), ph, th);
    let y1 = last((sy1 + 1).min(ph - 1), ph, th).max(y0 + 1).min(th);
    let ch = format.channels();
    let mut row_vals = vec![0.0_f32; (x1 - x0) * ch];
    for ty in y0..y1 {
        let sy = ((ty as f32 + 0.5) * ph as f32 / th as f32 - 0.5).clamp(0.0, (ph - 1) as f32);
        let iy = (sy as usize).min(ph - 1);
        let ny = (iy + 1).min(ph - 1);
        let wy = sy - iy as f32;
        for tx in x0..x1 {
            let fx = ((tx as f32 + 0.5) * pw as f32 / tw as f32 - 0.5).clamp(0.0, (pw - 1) as f32);
            let ix = (fx as usize).min(pw - 1);
            let nx = (ix + 1).min(pw - 1);
            let wx = fx - ix as f32;
            let p00 = proxy[iy * pw + ix];
            let p10 = proxy[iy * pw + nx];
            let p01 = proxy[ny * pw + ix];
            let p11 = proxy[ny * pw + nx];
            let base = (tx - x0) * ch;
            let mut px = [0.0_f32; 4];
            for c in 0..4 {
                px[c] = p00[c] * (1.0 - wx) * (1.0 - wy)
                    + p10[c] * wx * (1.0 - wy)
                    + p01[c] * (1.0 - wx) * wy
                    + p11[c] * wx * wy;
            }
            from_rgba_into(&format, px, &mut row_vals[base..base + ch]);
        }
        let r = Rect::from_xywh(target.x0 + x0 as i32, target.y0 + ty as i32, (x1 - x0) as u32, 1);
        s.write_region(r, &row_vals);
    }
    s.prune();
    s
}

// =============================================================================
// 4. LINE / OUTLINE ENGINES (6 MODES: CLEAN, 1-BIT, SPIKES, G-PEN, BRUSH, SHTACK)
// =============================================================================

/// Curvature-clamped contour spikes (`Triangle`, `Square`, `Circle`) with Box-Muller randomness.
fn build_spikes_polygons(geom: &PreparedSfx, le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let mut polys = Vec::new();
    let count = usize::from(le.spike_count.clamp(2, 40));
    let base_len = f64::from(le.spike_length_px);
    let base_size = f64::from(le.spike_size_px);
    let rand_amt = f64::from(le.spike_random_pct) / 100.0;
    let seed_base = le.spike_seed.wrapping_mul(7919);

    for (g_i, glyph) in geom.glyphs.iter().enumerate() {
        let g_seed = seed_base.wrapping_add((g_i as u32).wrapping_mul(1319));
        for (p_i, pl) in glyph.polylines.iter().enumerate() {
            let pts = &pl.pts;
            let n = pts.len();
            if n < 3 {
                continue;
            }
            let ccw = signed_area(pts) < 0.0;
            // Compute cumulative arc length around closed contour.
            let mut cum = Vec::with_capacity(n + 1);
            cum.push(0.0_f64);
            for i in 0..n {
                let a = pts[i];
                let b = pts[(i + 1) % n];
                let prev_len = cum.last().copied().unwrap_or(0.0);
                cum.push(prev_len + (b.0 - a.0).hypot(b.1 - a.1));
            }
            let total_len = cum.last().copied().unwrap_or(0.0);
            if total_len < 12.0 {
                continue;
            }
            let n_spikes = ((total_len / (total_len / count as f64).max(6.0)).round() as usize).clamp(2, count * 2);
            let step = total_len / n_spikes as f64;

            for s_i in 0..n_spikes {
                let s_seed = g_seed.wrapping_add((p_i as u32).wrapping_mul(313)).wrapping_add((s_i as u32).wrapping_mul(97));
                let jitter = f64::from(box_muller_gaussian(s_seed, s_seed.wrapping_add(17))) * 0.18 * rand_amt;
                let target = ((s_i as f64 + 0.5 + jitter) * step).rem_euclid(total_len);

                // Binary search segment containing `target`.
                let seg_idx = cum.partition_point(|&v| v <= target).saturating_sub(1).min(n - 1);
                let seg_start = cum[seg_idx];
                let seg_end = cum[seg_idx + 1];
                let t = if seg_end > seg_start + 1e-6 { (target - seg_start) / (seg_end - seg_start) } else { 0.0 };
                let a = pts[seg_idx];
                let b = pts[(seg_idx + 1) % n];
                let pos = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);

                // Curvature-clamped normal to prevent U-shape overshoot distortions in tight corners.
                let na = outward_normal(pts, seg_idx, ccw);
                let nb = outward_normal(pts, (seg_idx + 1) % n, ccw);
                let nx_raw = na.0 * (1.0 - t) + nb.0 * t;
                let ny_raw = na.1 * (1.0 - t) + nb.1 * t;
                let nlen = nx_raw.hypot(ny_raw);
                if nlen < 0.25 {
                    continue;
                }
                let nx = nx_raw / nlen;
                let ny = ny_raw / nlen;
                let tx = -ny;
                let ty = nx;

                // Measure local curvature turning between adjacent vertices and damp extrusion in sharp crevices.
                let v_prev = pts[(seg_idx + n - 1) % n];
                let v_next = pts[(seg_idx + 2) % n];
                let d1 = (a.0 - v_prev.0, a.1 - v_prev.1);
                let d2 = (v_next.0 - b.0, v_next.1 - b.1);
                let l1 = d1.0.hypot(d1.1).max(1e-5);
                let l2 = d2.0.hypot(d2.1).max(1e-5);
                let align = ((d1.0 * d2.0 + d1.1 * d2.1) / (l1 * l2)).clamp(-1.0, 1.0);
                let curvature_damp = (0.35 + 0.65 * ((align + 1.0) * 0.5)).clamp(0.35, 1.0);

                let g_len = f64::from(box_muller_gaussian(s_seed.wrapping_add(31), s_seed.wrapping_add(53)));
                let g_wid = f64::from(box_muller_gaussian(s_seed.wrapping_add(71), s_seed.wrapping_add(89)));
                let len = (base_len * (1.0 + g_len * 0.35 * rand_amt)).clamp(2.0, base_len * 1.6) * curvature_damp;
                let hw = (base_size * 0.5 * (1.0 + g_wid * 0.28 * rand_amt)).clamp(1.0, base_size);
                // Slightly embed base inside contour so spikes weld cleanly with zero gap.
                let base = (pos.0 - nx * 1.5, pos.1 - ny * 1.5);

                match le.spike_shape {
                    SfxSpikeShape::Triangle => {
                        polys.push(vec![
                            (base.0 + tx * hw, base.1 + ty * hw),
                            (pos.0 + nx * len, pos.1 + ny * len),
                            (base.0 - tx * hw, base.1 - ty * hw),
                        ]);
                    }
                    SfxSpikeShape::Square => {
                        let tip_w = hw * 0.72;
                        let tip = (pos.0 + nx * len, pos.1 + ny * len);
                        polys.push(vec![
                            (base.0 + tx * hw, base.1 + ty * hw),
                            (tip.0 + tx * tip_w, tip.1 + ty * tip_w),
                            (tip.0 - tx * tip_w, tip.1 - ty * tip_w),
                            (base.0 - tx * hw, base.1 - ty * hw),
                        ]);
                    }
                    SfxSpikeShape::Circle => {
                        let rad = (hw * 0.85).max(1.5);
                        let center = (pos.0 + nx * (len * 0.55), pos.1 + ny * (len * 0.55));
                        let mut circle = Vec::with_capacity(10);
                        for k in 0..10 {
                            let ang = (k as f64 / 10.0) * std::f64::consts::TAU;
                            circle.push((center.0 + ang.cos() * rad, center.1 + ang.sin() * rad));
                        }
                        polys.push(circle);
                    }
                }
            }
        }
    }
    polys
}

/// Appends `#`-style straight edge-extension cross-overs at sharp corners (zero whiskers!).
#[allow(clippy::too_many_arguments)]
fn append_cross_overshoot(
    polys: &mut Vec<Vec<(f64, f64)>>,
    edge: &[(f64, f64)],
    base_half_w: f64,
    overshoot_px: f64,
    count_pct: f64,
    random_pct: f64,
    seed: u32,
) {
    if overshoot_px < 0.4 || count_pct < 1.0 || edge.len() < 2 {
        return;
    }
    let prob = (count_pct / 100.0).clamp(0.0, 1.0);
    let rand_amt = (random_pct / 100.0).clamp(0.0, 1.0);

    // Start extension (pointing backwards along first segment)
    if f64::from(hash_u32(seed)) <= prob {
        let p0 = edge[0];
        let p1 = edge[1];
        let dx = p0.0 - p1.0;
        let dy = p0.1 - p1.1;
        let len = dx.hypot(dy);
        if len > 4.0 {
            let dir = (dx / len, dy / len);
            let jitter = 1.0 + (f64::from(hash_u32(seed.wrapping_add(13))) - 0.5) * 1.1 * rand_amt;
            let ext = (overshoot_px * jitter).clamp(1.2, overshoot_px * 1.65);
            polys.push(razor_bristle_poly(p0, dir, ext, (base_half_w * 0.82).max(0.75)));
        }
    }
    // End extension (pointing forwards along last segment)
    if f64::from(hash_u32(seed.wrapping_add(29))) <= prob {
        let n = edge.len();
        let p0 = edge[n - 2];
        let p1 = edge[n - 1];
        let dx = p1.0 - p0.0;
        let dy = p1.1 - p0.1;
        let len = dx.hypot(dy);
        if len > 4.0 {
            let dir = (dx / len, dy / len);
            let jitter = 1.0 + (f64::from(hash_u32(seed.wrapping_add(47))) - 0.5) * 1.1 * rand_amt;
            let ext = (overshoot_px * jitter).clamp(1.2, overshoot_px * 1.65);
            polys.push(razor_bristle_poly(p1, dir, ext, (base_half_w * 0.82).max(0.75)));
        }
    }
}

/// Builds ✒ G-Pen variable-width calligraphic ink polygons + `#` corner cross-overs.
fn build_gpen_polygons(geom: &PreparedSfx, le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let mut polys = Vec::new();
    let base_w = f64::from(le.line_width_px.max(1.5));
    let contrast = f64::from(le.gpen_contrast_pct) / 100.0;
    let nib_rad = f64::from(le.gpen_angle_deg).to_radians();
    let pinch = f64::from(le.gpen_pinch_pct) / 100.0;
    let overshoot = f64::from(le.gpen_overshoot_px);
    let ov_count = f64::from(le.gpen_overshoot_count_pct);
    let ov_rand = f64::from(le.gpen_overshoot_random_pct);
    let seed_base = le.gpen_seed.wrapping_mul(65537);

    for (g_i, glyph) in geom.glyphs.iter().enumerate() {
        for (p_i, pl) in glyph.polylines.iter().enumerate() {
            let edges = split_contour_into_edges(&pl.pts, 1.5, 0.56);
            for (e_i, edge) in edges.iter().enumerate() {
                let e_seed = seed_base
                    .wrapping_add((g_i as u32).wrapping_mul(1009))
                    .wrapping_add((p_i as u32).wrapping_mul(193))
                    .wrapping_add((e_i as u32).wrapping_mul(37));

                // Subdivide edge to capture smooth calligraphic pressure & gentle concave edge pinch.
                let mut samples = Vec::new();
                let mut total_edge_len = 0.0_f64;
                for seg in edge.windows(2) {
                    total_edge_len += (seg[1].0 - seg[0].0).hypot(seg[1].1 - seg[0].1);
                }
                if total_edge_len < 1.0 {
                    continue;
                }
                let mut accum = 0.0_f64;
                for (si, seg) in edge.windows(2).enumerate() {
                    let a = seg[0];
                    let b = seg[1];
                    let dx = b.0 - a.0;
                    let dy = b.1 - a.1;
                    let slen = dx.hypot(dy);
                    if slen < 1e-4 {
                        continue;
                    }
                    let dir_ang = dy.atan2(dx);
                    let nib_factor = 1.0 - contrast * 0.72 * (dir_ang - nib_rad).sin().abs();
                    let nx = -dy / slen;
                    let ny = dx / slen;
                    let sub_steps = ((slen / 6.0).ceil() as usize).clamp(1, 24);
                    let start_k = usize::from(si > 0);
                    for k in start_k..=sub_steps {
                        let f = k as f64 / sub_steps as f64;
                        let u = ((accum + slen * f) / total_edge_len).clamp(0.0, 1.0);
                        // Gentle inward pinch near edge midpoint.
                        let pinch_off = (u * PI).sin() * pinch * 1.4;
                        let hw = (base_w * 0.5 * nib_factor * (0.85 + 0.3 * (u * PI).sin())).max(0.45);
                        samples.push((a.0 + dx * f + nx * pinch_off, a.1 + dy * f + ny * pinch_off, hw));
                    }
                    accum += slen;
                }
                if let Some(poly) = ribbon_polygon_from_samples(&samples) {
                    polys.push(poly);
                }
                append_cross_overshoot(&mut polys, edge, base_w * 0.45, overshoot, ov_count, ov_rand, e_seed);
            }
        }
    }
    polys
}

/// Builds 🖌 Кисть-штрих organic rough ink strokes with pressure variance, ink dropout, and `#` cross-overs.
fn build_brush_stroke_polygons(geom: &PreparedSfx, le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let mut polys = Vec::new();
    let base_w = f64::from(le.line_width_px.max(1.5));
    let rough = f64::from(le.ink_brush_roughness_pct) / 100.0;
    let dropout = f64::from(le.ink_brush_dropout_pct) / 100.0;
    let press_var = f64::from(le.ink_brush_pressure_pct) / 100.0;
    let overshoot = f64::from(le.ink_brush_overshoot_px);
    let ov_count = f64::from(le.ink_brush_overshoot_count_pct);
    let ov_rand = f64::from(le.ink_brush_overshoot_random_pct);
    let seed_base = le.ink_brush_seed.wrapping_mul(49_157);

    for (g_i, glyph) in geom.glyphs.iter().enumerate() {
        for (p_i, pl) in glyph.polylines.iter().enumerate() {
            let edges = split_contour_into_edges(&pl.pts, 1.6, 0.54);
            for (e_i, edge) in edges.iter().enumerate() {
                let e_seed = seed_base
                    .wrapping_add((g_i as u32).wrapping_mul(1103))
                    .wrapping_add((p_i as u32).wrapping_mul(211))
                    .wrapping_add((e_i as u32).wrapping_mul(43));

                let mut cur_run: Vec<(f64, f64, f64)> = Vec::new();
                let mut dist_acc = 0.0_f64;

                for seg in edge.windows(2) {
                    let a = seg[0];
                    let b = seg[1];
                    let dx = b.0 - a.0;
                    let dy = b.1 - a.1;
                    let slen = dx.hypot(dy);
                    if slen < 1e-4 {
                        continue;
                    }
                    let nx = -dy / slen;
                    let ny = dx / slen;
                    let sub_steps = ((slen / 4.5).ceil() as usize).clamp(1, 32);
                    for k in 0..=sub_steps {
                        let f = k as f64 / sub_steps as f64;
                        let s_pos = (dist_acc + slen * f) as f32;
                        let n_drop = value_noise_1d(s_pos * 0.14, e_seed.wrapping_add(7));
                        let is_dropped = dropout > 0.05 && n_drop < (dropout as f32 * 0.42);
                        if is_dropped {
                            if cur_run.len() >= 2
                                && let Some(poly) = ribbon_polygon_from_samples(&cur_run)
                            {
                                polys.push(poly);
                            }
                            cur_run.clear();
                            continue;
                        }
                        let wobble = (f64::from(value_noise_1d(s_pos * 0.22, e_seed.wrapping_add(19))) - 0.5) * 2.0 * rough * base_w * 0.42;
                        let p_mod = 1.0 + (f64::from(value_noise_1d(s_pos * 0.08, e_seed.wrapping_add(59))) - 0.5) * 1.15 * press_var;
                        let hw = (base_w * 0.52 * p_mod).clamp(0.4, base_w * 1.25);
                        cur_run.push((a.0 + dx * f + nx * wobble, a.1 + dy * f + ny * wobble, hw));
                    }
                    dist_acc += slen;
                }
                if cur_run.len() >= 2
                    && let Some(poly) = ribbon_polygon_from_samples(&cur_run)
                {
                    polys.push(poly);
                }
                append_cross_overshoot(&mut polys, edge, base_w * 0.48, overshoot, ov_count, ov_rand, e_seed);
            }
        }
    }
    polys
}

// =============================================================================
// 5. ⚡ SHTACK BRUSH — SHARED MULTI-PASS HAND-DRAWN SKETCH GEOMETRY
// =============================================================================
//
// Both submodes share one geometry engine, working DIRECTLY on the exact vector glyph contours:
//   * each closed contour is simplified only at micro-collinear vertices, then split into
//     independent line/arc segments at sharp corners (>36° turns); smooth loops become two arcs;
//   * compact contours (the dot/bar of `!`, `?`, `i`, accents) keep their solid core PLUS a few
//     short sketch strokes, so grid downsampling can never make them vanish;
//   * every segment overshoots along its end tangent at BOTH ends by a percentage of glyph height;
//   * 3..=6 independent tapered passes (2 dense core + thinner, laterally separated whiskers)
//     create natural hand-drawn air gaps — no fake white `cut_polys` wedges.
// `SaiPenSketch` keeps long sharp needles; `DryPencil` shortens/roughens them and adds
// deterministic paper-grain breakup in coverage space.

/// Even arc-length resampling of a polyline; guards short/zero-length inputs.
fn resample_polyline(pts: &[(f64, f64)], count: usize) -> Vec<(f64, f64)> {
    if pts.is_empty() || count < 2 {
        return Vec::new();
    }
    if pts.len() == 1 {
        return vec![pts[0]; count];
    }
    let mut cum = vec![0.0_f64; pts.len()];
    for i in 1..pts.len() {
        cum[i] = cum[i - 1] + (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
    }
    let total = cum[pts.len() - 1];
    if !total.is_finite() || total < 1e-6 {
        return vec![pts[0]; count];
    }
    let mut out = Vec::with_capacity(count);
    let mut seg = 1usize;
    for k in 0..count {
        let t = total * k as f64 / (count - 1) as f64;
        while seg < cum.len() - 1 && cum[seg] < t {
            seg += 1;
        }
        let a = cum[seg - 1];
        let b = cum[seg];
        let u = if b > a { ((t - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
        out.push((pts[seg - 1].0 + (pts[seg].0 - pts[seg - 1].0) * u, pts[seg - 1].1 + (pts[seg].1 - pts[seg - 1].1) * u));
    }
    out
}

/// One open stroke segment for Shtack, tagged with whether its start/end may shoot razor needles.
struct ShtackStroke {
    pts: Vec<(f64, f64)>,
    ext_start: bool,
    ext_end: bool,
}

/// Odd-even ray-cast test against a single closed ring.
fn point_in_ring(pt: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let (px, py) = pt;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        let crosses = (yi > py) != (yj > py);
        if crosses {
            let x_int = (xj - xi) * (py - yi) / (yj - yi) + xi;
            if px < x_int {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// True when `pt` lies inside the filled body of a glyph (odd-even across all glyph contours).
fn point_in_glyph(pt: (f64, f64), polylines: &[Polyline]) -> bool {
    let mut inside = false;
    for pl in polylines {
        if point_in_ring(pt, &pl.pts) {
            inside = !inside;
        }
    }
    inside
}

/// Determines whether contour `idx` is an outer silhouette (`true`) or an inner counter/hole (`false`),
/// invariant to TrueType vs CFF winding order.
fn contour_is_outer(idx: usize, polylines: &[Polyline]) -> bool {
    let Some(&probe) = polylines[idx].pts.first() else {
        return true;
    };
    let mut depth = 0usize;
    for (other, pl) in polylines.iter().enumerate() {
        if other != idx && point_in_ring(probe, &pl.pts) {
            depth += 1;
        }
    }
    depth.is_multiple_of(2)
}

/// Checks if shooting a needle from `end_pt` along unit direction `dir` goes cleanly OUTSIDE the glyph.
fn needle_protrudes_outside(end_pt: (f64, f64), dir: (f64, f64), polylines: &[Polyline], gh: f64) -> bool {
    let len = dir.0.hypot(dir.1);
    if len < 1e-6 {
        return false;
    }
    let (ux, uy) = (dir.0 / len, dir.1 / len);
    for frac in [0.035, 0.085] {
        let d = (gh * frac).max(3.0);
        let probe = (end_pt.0 + ux * d, end_pt.1 + uy * d);
        if point_in_glyph(probe, polylines) {
            return false;
        }
    }
    true
}

/// Extends a stroke past enabled ends along its end tangents (razor needles). Ends disabled by
/// `OuterOnly` get only a tiny `seal_px` closure (~half stroke width) so corners meet cleanly
/// with zero gap and zero interior overshoot.
fn extend_stroke_ends_masked(
    pts: &mut Vec<(f64, f64)>,
    max_needle: f64,
    seal_px: f64,
    seed: u32,
    ext_start: bool,
    ext_end: bool,
) {
    if pts.len() < 2 {
        return;
    }
    let first = pts[0];
    let second = pts[1];
    let last = pts[pts.len() - 1];
    let prev = pts[pts.len() - 2];
    let d0 = (first.0 - second.0, first.1 - second.1);
    let l0 = d0.0.hypot(d0.1).max(1e-6);
    let d1 = (last.0 - prev.0, last.1 - prev.1);
    let l1 = d1.0.hypot(d1.1).max(1e-6);
    let r0 = f64::from(hash_u32(seed));
    let r1 = f64::from(hash_u32(seed.wrapping_add(0x51ED)));
    let o0 = if ext_start { max_needle * (0.75 + 0.55 * r0) } else { seal_px };
    let o1 = if ext_end { max_needle * (0.75 + 0.55 * r1) } else { seal_px };
    if o0 > 0.05 {
        let head = (first.0 + d0.0 / l0 * o0, first.1 + d0.1 / l0 * o0);
        if head.0.is_finite() && head.1.is_finite() {
            pts.insert(0, head);
        }
    }
    if o1 > 0.05 {
        let tail = (last.0 + d1.0 / l1 * o1, last.1 + d1.1 / l1 * o1);
        if tail.0.is_finite() && tail.1.is_finite() {
            pts.push(tail);
        }
    }
}

/// Splits a simplified closed contour into independent open stroke segments and tags each end
/// according to `outer_only`: in `OuterOnly` mode only convex outer-silhouette corners whose
/// tangent exits into empty space may shoot needles; inner counters and concave bends stay clean.
fn split_contour_at_corners(
    simp: &[(f64, f64)],
    is_outer: bool,
    polylines: &[Polyline],
    gh: f64,
    outer_only: bool,
) -> Vec<ShtackStroke> {
    const COS_CORNER: f64 = 0.809;
    let n = simp.len();
    if n < 3 {
        return Vec::new();
    }
    let area_sign = signed_area(simp).signum();
    let mut corners: Vec<(usize, bool)> = Vec::new();
    for i in 0..n {
        let prev = simp[(i + n - 1) % n];
        let cur = simp[i];
        let next = simp[(i + 1) % n];
        let v1 = (cur.0 - prev.0, cur.1 - prev.1);
        let v2 = (next.0 - cur.0, next.1 - cur.1);
        let l1 = v1.0.hypot(v1.1);
        let l2 = v2.0.hypot(v2.1);
        if l1 > 1e-4 && l2 > 1e-4 {
            let cos_turn = ((v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2)).clamp(-1.0, 1.0);
            if cos_turn < COS_CORNER {
                let cross = v1.0 * v2.1 - v1.1 * v2.0;
                let is_convex = area_sign * cross > 0.0;
                corners.push((i, is_convex));
            }
        }
    }
    if corners.len() >= 2 {
        let mut edges = Vec::with_capacity(corners.len());
        for k in 0..corners.len() {
            let (start, start_convex) = corners[k];
            let (end, end_convex) = corners[(k + 1) % corners.len()];
            let mut edge = Vec::new();
            let mut idx = start;
            for _ in 0..=n {
                edge.push(simp[idx]);
                if idx == end && edge.len() >= 2 {
                    break;
                }
                idx = (idx + 1) % n;
            }
            if edge.len() >= 2 {
                let (ext_start, ext_end) = if !outer_only {
                    (true, true)
                } else if !is_outer {
                    (false, false)
                } else {
                    let s_dir = (edge[0].0 - edge[1].0, edge[0].1 - edge[1].1);
                    let m = edge.len();
                    let e_dir = (edge[m - 1].0 - edge[m - 2].0, edge[m - 1].1 - edge[m - 2].1);
                    (
                        start_convex && needle_protrudes_outside(edge[0], s_dir, polylines, gh),
                        end_convex && needle_protrudes_outside(edge[m - 1], e_dir, polylines, gh),
                    )
                };
                edges.push(ShtackStroke { pts: edge, ext_start, ext_end });
            }
        }
        if !edges.is_empty() {
            return edges;
        }
    }
    // Smooth loop: split into two arcs between the vertical extremes.
    let (mut i_top, mut i_bot) = (0usize, 0usize);
    for (i, &p) in simp.iter().enumerate() {
        if p.1 < simp[i_top].1 {
            i_top = i;
        }
        if p.1 > simp[i_bot].1 {
            i_bot = i;
        }
    }
    if i_top == i_bot {
        i_bot = (i_top + n / 2) % n;
    }
    let walk = |from: usize, to: usize| -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut idx = from;
        for _ in 0..=n {
            out.push(simp[idx]);
            if idx == to && out.len() >= 2 {
                break;
            }
            idx = (idx + 1) % n;
        }
        out
    };
    let smooth_ext = !outer_only || is_outer;
    let mut arcs = Vec::with_capacity(2);
    for (a, b) in [(i_top, i_bot), (i_bot, i_top)] {
        let arc = walk(a, b);
        if arc.len() >= 2 {
            arcs.push(ShtackStroke { pts: arc, ext_start: smooth_ext, ext_end: smooth_ext });
        }
    }
    arcs
}

/// Compact contours (the dot of `!`/`?`, `i` dots) trace two contour-following half-arcs scaled
/// to `c_span`. In hollow Shtack mode (`!shtack_fill_interior`) dots with readable size stay
/// hollow sketch rings matching the rest of the lettering; tiny dots or filled mode keep a solid core.
fn compact_dot_segments_and_core(
    pl: &Polyline,
    le: &SfxLineEffect,
    c_span: f64,
    perimeter: f64,
    outer_only: bool,
) -> (Option<InkPolygon>, Vec<ShtackStroke>) {
    // Only microscopic dots (<10 px) keep a solid line core so they never vanish on small pt sizes;
    // readable dots render as a sketch contour ring so hollow mode stays hollow and filled modes
    // (Solid / Screentone / Shtack Fill) show cleanly inside the dot instead of being painted black.
    let keep_solid_core = c_span < 10.0;
    let core = keep_solid_core.then(|| {
        let mut c = pl.pts.clone();
        if signed_area(&c) < 0.0 {
            c.reverse();
        }
        c
    });
    let n_trace = (perimeter / 2.5).round().clamp(10.0, 24.0) as usize;
    let outline = resample_polyline(&pl.pts, n_trace);
    let mut traces = Vec::new();
    if outline.len() >= 4 {
        let half = outline.len() / 2;
        let jitter_amp = (c_span * 0.035).clamp(0.2, 1.4);
        for (pass, range) in [(0usize, 0..=half), (1usize, half..=(outline.len() - 1))] {
            let mut trace: Vec<(f64, f64)> = outline[range].to_vec();
            if pass == 1
                && let Some(&first) = outline.first()
            {
                trace.push(first);
            }
            let seed = le.shtack_seed.wrapping_add((pass as u32).wrapping_mul(977));
            for (i, pt) in trace.iter_mut().enumerate() {
                let jx = (f64::from(hash_u32(seed.wrapping_add((i as u32).wrapping_mul(7)))) - 0.5) * jitter_amp;
                let jy = (f64::from(hash_u32(seed.wrapping_add((i as u32).wrapping_mul(13)).wrapping_add(31))) - 0.5) * jitter_amp;
                pt.0 += jx;
                pt.1 += jy;
            }
            if trace.len() >= 2 {
                traces.push(ShtackStroke { pts: trace, ext_start: !outer_only, ext_end: !outer_only });
            }
        }
    }
    (core, traces)
}

/// Builds the complete Shtack polygon set DIRECTLY from the exact vector glyph contours.
/// Returns ready-to-rasterize polygons: optional solid cores for filled/micro dots plus
/// every tapered sketch pass. No raster grid, no skeleton thinning, no lost punctuation.
fn build_shtack_contour_polygons(geom: &PreparedSfx, le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let outer_only = le.shtack_overshoot_mode == SfxShtackOvershootMode::OuterOnly;
    let mut polys: Vec<Vec<(f64, f64)>> = Vec::new();
    for glyph in &geom.glyphs {
        // Letter height in document pixels; every proportional decision uses it.
        let gh = (glyph.warped_bounds[3] - glyph.warped_bounds[1]).abs().max(24.0);
        let mut segments: Vec<ShtackStroke> = Vec::new();
        let mut compact_segments: Vec<ShtackStroke> = Vec::new();
        let mut solid_cores: Vec<Vec<(f64, f64)>> = Vec::new();

        for (pi, pl) in glyph.polylines.iter().enumerate() {
            if pl.pts.len() < 3 {
                continue;
            }
            let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
            let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
            let mut perimeter = 0.0_f64;
            for (i, &p) in pl.pts.iter().enumerate() {
                x0 = x0.min(p.0);
                y0 = y0.min(p.1);
                x1 = x1.max(p.0);
                y1 = y1.max(p.1);
                let q = pl.pts[(i + 1) % pl.pts.len()];
                perimeter += (q.0 - p.0).hypot(q.1 - p.1);
            }
            if !x0.is_finite() || !y0.is_finite() || !x1.is_finite() || !y1.is_finite() {
                continue;
            }
            let c_span = (x1 - x0).max(y1 - y0);

            // Compact closed contour (`!`, `?`, `i` dots): sketch ring around the dot outline.
            if c_span <= gh * 0.26 || perimeter < gh * 0.75 {
                let (core, traces) = compact_dot_segments_and_core(pl, le, c_span, perimeter, outer_only);
                if let Some(c) = core {
                    solid_cores.push(c);
                }
                compact_segments.extend(traces);
                continue;
            }

            // Normal contour: lightly simplify micro-collinear vertices (curves stay smooth),
            // then split at sharp corners into independent line / arc segments.
            let is_outer = contour_is_outer(pi, &glyph.polylines);
            let simp = rdp_closed(&pl.pts, 0.75);
            segments.extend(split_contour_at_corners(&simp, is_outer, &glyph.polylines, gh, outer_only));
        }

        polys.extend(build_shtack_stroke_polygons(&segments, le, gh));
        if !compact_segments.is_empty() {
            let mut dot_le = *le;
            dot_le.shtack_bristles = 3;
            // Scale overshoot, spread and nib proportionally for compact punctuation so dots
            // never shoot full-letter needles or scatter wide whiskers.
            dot_le.shtack_overshoot_px = (le.shtack_overshoot_px * 0.25).min(8.0);
            dot_le.shtack_split_pct = le.shtack_split_pct * 0.45;
            dot_le.shtack_thickness_px = (f64::from(le.shtack_thickness_px) * 0.75).clamp(0.5, 8.0) as f32;
            polys.extend(build_shtack_stroke_polygons(&compact_segments, &dot_le, gh));
        }
        polys.extend(solid_cores);
    }
    polys
}

/// Builds 3..=6 independent variable-width tapered sketch passes per segment. Core passes keep
/// a dense body with long razor tapers; whiskers are visibly thinner and offset by a guaranteed
/// lateral air gap, then tilted and bowed so they diverge organically from the core stroke.
fn build_shtack_stroke_polygons(strokes: &[ShtackStroke], le: &SfxLineEffect, gh: f64) -> Vec<Vec<(f64, f64)>> {
    // Thickness is a PERCENTAGE of glyph height, not a document-pixel length. Compute it once,
    // directly from gh: no `size_scale`, no double scaling on large lettering or proxies.
    let thickness_pct = f64::from(le.shtack_thickness_px).clamp(0.5, 8.0);
    let half_w = (gh * (thickness_pct / 100.0) * 0.5).max(0.75);
    let bristles = usize::from(le.shtack_bristles.clamp(3, 6));
    let split = (f64::from(le.shtack_split_pct) / 100.0).clamp(0.0, 1.0);
    let dry = (f64::from(le.shtack_dryness_pct) / 100.0).clamp(0.0, 1.0);
    let is_dry = le.shtack_variant == SfxShtackVariant::DryPencil;
    let overshoot_ratio = (f64::from(le.shtack_overshoot_px).clamp(0.0, 60.0) / 100.0) * if is_dry { 0.48 } else { 1.0 };
    let needle_px = gh * overshoot_ratio;
    let seed_base = le.shtack_seed.wrapping_mul(12_347);
    let mut polys = Vec::new();

    for (si, st) in strokes.iter().enumerate() {
        let stroke = &st.pts;
        if stroke.len() < 2 {
            continue;
        }
        let s_seed = seed_base.wrapping_add((si as u32).wrapping_mul(311));
        let seg_len: f64 = stroke.windows(2).map(|seg| (seg[1].0 - seg[0].0).hypot(seg[1].1 - seg[0].1)).sum();
        let sample_step = if gh < 130.0 { 5.5 } else { 3.0 };
        let max_samples = if gh < 130.0 { 18.0 } else { 36.0 };
        let min_samples = if gh < 130.0 { 6.0 } else { 10.0 };
        let samples_n = (seg_len / sample_step).round().clamp(min_samples, max_samples) as usize;
        // Clamp relative to segment length so tiny segments never shoot giant needles.
        let max_needle = needle_px.min(seg_len * 0.95 + gh * 0.12);

        for k in 0..bristles {
            let p_seed = s_seed.wrapping_add((k as u32).wrapping_mul(1_073));
            let r0 = f64::from(hash_u32(p_seed));
            let r1 = f64::from(hash_u32(p_seed.wrapping_add(17)));
            let r2 = f64::from(hash_u32(p_seed.wrapping_add(31)));
            let r3 = f64::from(hash_u32(p_seed.wrapping_add(59)));
            let core = k < 2;

            let mut extended = stroke.clone();
            let pass_needle = if core { max_needle } else { max_needle * (0.45 + r3 * 0.9) };
            let seal_px = if core { (half_w * 0.85).min(seg_len * 0.2) } else { 0.0 };
            extend_stroke_ends_masked(&mut extended, pass_needle, seal_px, p_seed, st.ext_start, st.ext_end);
            let base = resample_polyline(&extended, samples_n);
            if base.len() < 2 {
                continue;
            }

            // Base passes are dense; whiskers are visibly thinner. A core body is never
            // sub-pixel even at 0.5% thickness: it stays >= 0.65 half-width so the ink is
            // continuous instead of a broken line of isolated speckles.
            let width = if core {
                (half_w * (0.78 + 0.18 * r0)).max(0.65)
            } else {
                half_w * (0.24 + 0.18 * r0)
            };
            let side = if k >= 2 && (k - 2) % 2 == 0 { 1.0 } else { -1.0 };
            let rank = (k.saturating_sub(2) / 2) as f64;
            let base_off = if core {
                (if k == 0 { -1.0 } else { 1.0 }) * half_w * 0.15 * (0.5 + 0.5 * r1)
            } else {
                // Guaranteed minimum clearance so whiskers never fuse into one fat line.
                let gap = half_w * (1.05 + 0.65 * rank) * (0.45 + 0.75 * split);
                side * gap.max(half_w * (1.35 + 0.4 * rank)) * (0.85 + 0.3 * r1)
            };
            let tilt = (r2 - 0.5) * half_w * if core { 0.10 } else { 0.55 };
            let bow_amp = (r3 - 0.5) * half_w * if core { 0.18 } else { 0.70 };
            let jitter_amp = if is_dry { dry * 2.0 } else { 0.35 };

            let mut samples = Vec::with_capacity(base.len());
            for (i, &p) in base.iter().enumerate() {
                let t = i as f64 / (base.len() - 1) as f64;
                let prev = base[i.saturating_sub(1)];
                let next = base[(i + 1).min(base.len() - 1)];
                let tx = next.0 - prev.0;
                let ty = next.1 - prev.1;
                let tl = tx.hypot(ty).max(1e-6);
                let nx = -ty / tl;
                let ny = tx / tl;
                // Smooth 0 -> full -> 0 taper: razor needle tips at both ends. Core passes
                // hold a flat full-width body and taper only over the outer 15% at each end,
                // so minimum-thickness strokes render as crisp, unbroken ink.
                let tip_taper = (PI * t).sin().max(0.0).powf(0.55);
                let taper = if core {
                    tip_taper.max((t / 0.15).min((1.0 - t) / 0.15).clamp(0.0, 1.0))
                } else {
                    tip_taper
                };
                let hw = (taper * width).max(0.15);
                let jitter = if jitter_amp > 0.01 {
                    (f64::from(value_noise_1d(t as f32 * 42.0 + (k as f32) * 3.7, p_seed)) - 0.5) * 2.0 * jitter_amp
                } else {
                    0.0
                };
                let off = base_off + tilt * (t - 0.5) + bow_amp * (PI * t).sin() + jitter;
                samples.push((p.0 + nx * off, p.1 + ny * off, hw));
            }
            if let Some(poly) = ribbon_polygon_from_samples(&samples) {
                polys.push(poly);
            }
        }
    }
    polys
}

/// Deterministic crisp paper-grain + directional dry-streak breakup shared by Dry Pencil Shtack
/// outlines and `Shtack Fill` interior hatching.
fn apply_dry_grain_breakup(cov: &mut [f32], w: usize, h: usize, dryness_pct: f32, sharpness_pct: f32, seed_in: u32) {
    let dryness = (f64::from(dryness_pct) / 100.0).clamp(0.0, 1.0);
    if dryness <= 0.001 || w == 0 || h == 0 {
        return;
    }
    let sharp = (f64::from(sharpness_pct) / 100.0).clamp(0.0, 1.0);
    let src = cov.to_vec();
    let seed = seed_in.wrapping_mul(7_919);
    for (i, c) in cov.iter_mut().enumerate() {
        if *c <= 1e-4 {
            continue;
        }
        let px = i % w;
        let py = i / w;
        let cv = src[i];
        let up = if py > 0 { src[i - w] } else { cv };
        let down = if py + 1 < h { src[i + w] } else { cv };
        let left = if px > 0 { src[i - 1] } else { cv };
        let right = if px + 1 < w { src[i + 1] } else { cv };
        let min_nb = up.min(down).min(left).min(right);
        let edge = f64::from((cv * min_nb).clamp(0.0, 1.0));
        let x = px as f32;
        let y = py as f32;
        let grain = f64::from(crisp_paper_grain(x, y, seed));
        let streak = f64::from(value_noise_2d(x * 0.12, y * 0.85, seed.wrapping_add(31)));
        let noise = (grain * 0.72 + streak * 0.28).clamp(0.0, 1.0);
        let ink_score = (1.0 - noise) + 0.26 * edge;
        let thresh = dryness * 0.92;
        let keep = if sharp >= 0.99 {
            if ink_score >= thresh { 1.0 } else { 0.0 }
        } else {
            let slope = 2.5 + sharp * sharp * 55.0;
            ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
        };
        *c = (*c as f64 * keep).clamp(0.0, 1.0) as f32;
    }
}

/// Dry Pencil finish: deterministic paper grain + directional dry streaks break up the
/// rendered coverage, strongest at stroke edges and low-coverage tapered ends.
fn apply_shtack_dry_breakup(cov: &mut [f32], w: usize, h: usize, le: &SfxLineEffect) {
    apply_dry_grain_breakup(cov, w, h, le.shtack_dryness_pct, le.shtack_grain_sharpness_pct, le.shtack_seed);
}

// =============================================================================
// 5B. INDEPENDENT INTERIOR SHTACK FILL (ANALYTIC HATCHING WITHOUT OUTLINE)
// =============================================================================

/// Intersects the scanline `dot(p, n) == v_coord` against all closed polylines of one glyph
/// and returns sorted `[u0, u1]` interior segments along unit direction `u`.
fn scanline_intervals_in_glyph(polylines: &[Polyline], v_coord: f64, u: (f64, f64), n: (f64, f64)) -> Vec<(f64, f64)> {
    let mut hits: Vec<f64> = Vec::new();
    for pl in polylines {
        let len = pl.pts.len();
        if len < 3 {
            continue;
        }
        let mut prev = pl.pts[len - 1];
        let mut v_prev = prev.0 * n.0 + prev.1 * n.1;
        for &cur in &pl.pts {
            let v_cur = cur.0 * n.0 + cur.1 * n.1;
            if (v_prev > v_coord) != (v_cur > v_coord) {
                let t = (v_coord - v_prev) / (v_cur - v_prev);
                let ix = prev.0 + (cur.0 - prev.0) * t;
                let iy = prev.1 + (cur.1 - prev.1) * t;
                let u_hit = ix * u.0 + iy * u.1;
                if u_hit.is_finite() {
                    hits.push(u_hit);
                }
            }
            prev = cur;
            v_prev = v_cur;
        }
    }
    hits.sort_by(f64::total_cmp);
    hits.chunks_exact(2).filter_map(|p| (p[1] - p[0] >= 1.5).then_some((p[0], p[1]))).collect()
}

/// Builds tapered organic hatch ribbon polygons filling every glyph interior along `angle_deg`.
fn build_shtack_fill_pass(
    geom: &PreparedSfx,
    spec: &SfxShtackFillSpec,
    angle_deg: f64,
    spacing_scale: f64,
    width_scale: f64,
    seed_salt: u32,
) -> Vec<InkPolygon> {
    let (sin_a, cos_a) = angle_deg.to_radians().sin_cos();
    let u = (cos_a, sin_a);
    let n = (-sin_a, cos_a);
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let seed_base = spec.seed.wrapping_mul(16_381).wrapping_add(seed_salt);
    let mut polys = Vec::new();

    for (gi, glyph) in geom.glyphs.iter().enumerate() {
        let gh = (glyph.warped_bounds[3] - glyph.warped_bounds[1]).abs().max(24.0);
        let step = (gh * (0.105 - 0.082 * density) * spacing_scale).max(2.2);
        let half_w = (gh * (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 100.0) * 0.5 * width_scale).max(0.6);
        let overshoot = gh * (f64::from(spec.overshoot_pct).clamp(0.0, 25.0) / 100.0);

        let (mut v_min, mut v_max) = (f64::INFINITY, f64::NEG_INFINITY);
        for pl in &glyph.polylines {
            for &p in &pl.pts {
                let v = p.0 * n.0 + p.1 * n.1;
                if v.is_finite() {
                    v_min = v_min.min(v);
                    v_max = v_max.max(v);
                }
            }
        }
        if !v_min.is_finite() || v_max <= v_min {
            continue;
        }
        let line_count = (((v_max - v_min) / step).ceil() as usize).clamp(1, 220);
        let eff_step = (v_max - v_min) / (line_count as f64);

        for li in 0..line_count {
            let l_seed = seed_base.wrapping_add((gi as u32).wrapping_mul(2_027)).wrapping_add((li as u32).wrapping_mul(389));
            let r0 = f64::from(hash_u32(l_seed));
            let v_line = v_min + (li as f64 + 0.5) * eff_step + (r0 - 0.5) * eff_step * 0.45 * rough;
            let intervals = scanline_intervals_in_glyph(&glyph.polylines, v_line, u, n);
            for (si, (u0, u1)) in intervals.into_iter().enumerate() {
                let s_seed = l_seed.wrapping_add((si as u32).wrapping_mul(1_093));
                let r1 = f64::from(hash_u32(s_seed.wrapping_add(11)));
                let r2 = f64::from(hash_u32(s_seed.wrapping_add(29)));
                let r3 = f64::from(hash_u32(s_seed.wrapping_add(53)));
                let r4 = f64::from(hash_u32(s_seed.wrapping_add(89)));
                let o0 = if overshoot > 0.05 { overshoot * (0.35 + 0.9 * r1) } else { 0.0 };
                let o1 = if overshoot > 0.05 { overshoot * (0.35 + 0.9 * r2) } else { 0.0 };
                let a = u0 - o0;
                let b = u1 + o1;
                let w_var = half_w * (0.78 + 0.44 * r3);
                let tilt = (r3 - 0.5) * eff_step * 0.55 * rough;
                let bow = (r4 - 0.5) * eff_step * 0.45 * rough;
                let mut samples = Vec::with_capacity(7);
                for k in 0..7 {
                    let t = f64::from(k) / 6.0;
                    let uu = a + (b - a) * t;
                    let vv = v_line + tilt * (t - 0.5) + bow * (PI * t).sin();
                    let tip = (PI * t).sin().max(0.0).powf(0.42);
                    let body = (t / 0.16).min((1.0 - t) / 0.16).clamp(0.0, 1.0);
                    let hw = (tip.max(body) * w_var).max(0.18);
                    samples.push((uu * u.0 + vv * n.0, uu * u.1 + vv * n.1, hw));
                }
                if let Some(poly) = ribbon_polygon_from_samples(&samples) {
                    polys.push(poly);
                }
            }
        }
    }
    polys
}

/// One interactive canvas gizmo handle for a `StrokeFlow` (`⚡ SAI`) stroke endpoint.
#[derive(Clone, Copy, Debug)]
pub struct SfxFlowNeedleHandle {
    pub glyph_index: u16,
    /// Document-space position of the stroke endpoint.
    pub end_doc: [f64; 2],
    /// Effective outgoing unit needle direction in document space (includes `angle_offset_deg`).
    pub dir: [f64; 2],
    /// Original unrotated base outgoing unit direction (used as the stable override key).
    pub base_dir: [f64; 2],
    /// Normalized `[u, v]` inside the glyph's warped bounding box (invariant to move/resize).
    pub uv: [f32; 2],
    /// Effective needle state after applying automatic rules + any user override.
    pub enabled: bool,
    /// True when the user explicitly overrode this endpoint on the canvas.
    pub overridden: bool,
    /// Automatic default state before user overrides (toggling back clears the override).
    pub default_enabled: bool,
    /// User-dragged angle offset relative to `base_dir` in degrees (`-165..=165`).
    pub angle_offset_deg: f32,
    /// User-dragged length multiplier (`0.15..=3.0`, default `1.0`).
    pub length_scale: f32,
    /// Baseline document-space needle reach (px) before `length_scale`, for tip handle placement.
    pub base_needle_px: f64,
}

/// Per-glyph context for stroke-flow and Ukiyo-e brush synthesis.
struct FlowGlyphCtx<'a> {
    glyph_index: u16,
    warped_bounds: [f64; 4],
    outer_rings: &'a [&'a [(f64, f64)]],
    polylines: &'a [Polyline],
    gh: f64,
}

impl FlowGlyphCtx<'_> {
    fn normalized_uv(&self, pt: (f64, f64)) -> [f32; 2] {
        let gw = (self.warped_bounds[2] - self.warped_bounds[0]).abs().max(1.0);
        let gh = (self.warped_bounds[3] - self.warped_bounds[1]).abs().max(1.0);
        [
            ((pt.0 - self.warped_bounds[0]) / gw) as f32,
            ((pt.1 - self.warped_bounds[1]) / gh) as f32,
        ]
    }
}

/// Computes median local half-width along a flow stroke so core ribbons adapt to font weight.
fn median_flow_radius(pts: &[(f64, f64, f64)]) -> f64 {
    if pts.is_empty() {
        return 2.0;
    }
    let mut rs: Vec<f64> = pts.iter().map(|p| p.2).collect();
    rs.sort_by(f64::total_cmp);
    rs[rs.len() / 2].max(1.0)
}

/// Returns true when shooting a needle from `end_pt` along unit `dir` lands inside an enclosed
/// letter counter/hole (e.g. `B`'s middle-right pinch aiming into the lower bowl).
fn flow_end_enters_counter(end_pt: (f64, f64), dir: (f64, f64), r_end: f64, gctx: &FlowGlyphCtx<'_>) -> bool {
    for mult in [0.55, 1.35] {
        let d = (r_end * mult + gctx.gh * 0.045).max(3.5);
        let probe = (end_pt.0 + dir.0 * d, end_pt.1 + dir.1 * d);
        let inside_outer = gctx.outer_rings.iter().any(|ring| point_in_ring(probe, ring));
        if inside_outer && !point_in_glyph(probe, gctx.polylines) {
            return true;
        }
    }
    false
}

/// Automatic needle state before per-endpoint gizmo overrides.
fn flow_end_default_enabled(
    kind: FlowEndKind,
    mode: SfxShtackFlowOvershoot,
    end_pt: (f64, f64),
    dir: (f64, f64),
    r_end: f64,
    gctx: &FlowGlyphCtx<'_>,
) -> bool {
    match kind {
        FlowEndKind::ShortBridge => false,
        FlowEndKind::FreeTip => true,
        FlowEndKind::Junction => match mode {
            SfxShtackFlowOvershoot::FreeEnds => false,
            SfxShtackFlowOvershoot::OuterJunctions => !flow_end_enters_counter(end_pt, dir, r_end, gctx),
            SfxShtackFlowOvershoot::All => true,
        },
    }
}

#[derive(Clone, Copy)]
struct ResolvedFlowEnd {
    enabled: bool,
    default_enabled: bool,
    overridden: bool,
    uv: [f32; 2],
    angle_offset_deg: f32,
    length_scale: f32,
}

#[inline]
fn rotate_unit_dir(dir: (f64, f64), angle_deg: f32) -> (f64, f64) {
    if angle_deg.abs() <= 1e-4 {
        return dir;
    }
    let (s, c) = f64::from(angle_deg).to_radians().sin_cos();
    (dir.0 * c - dir.1 * s, dir.0 * s + dir.1 * c)
}

/// Resolves effective state + angle/length overrides for one open stroke endpoint.
fn resolve_flow_end(
    kind: FlowEndKind,
    spec: &SfxShtackFillSpec,
    end_pt: (f64, f64),
    dir: (f64, f64),
    r_end: f64,
    gctx: &FlowGlyphCtx<'_>,
) -> ResolvedFlowEnd {
    let uv = gctx.normalized_uv(end_pt);
    let default_enabled = flow_end_default_enabled(kind, spec.flow_overshoot, end_pt, dir, r_end, gctx);
    let dir_f = [dir.0 as f32, dir.1 as f32];
    if let Some(idx) = spec.find_flow_override_idx(gctx.glyph_index, uv, dir_f) {
        let ov = spec.flow_overrides[idx];
        ResolvedFlowEnd {
            enabled: ov.enabled,
            default_enabled,
            overridden: true,
            uv,
            angle_offset_deg: ov.angle_offset_deg.clamp(-165.0, 165.0),
            length_scale: ov.length_scale.clamp(0.15, 3.0),
        }
    } else {
        ResolvedFlowEnd {
            enabled: default_enabled,
            default_enabled,
            overridden: false,
            uv,
            angle_offset_deg: 0.0,
            length_scale: 1.0,
        }
    }
}

/// Extracts interactive canvas gizmo handles for every non-bridge open stroke endpoint in `geom`.
pub fn extract_sfx_flow_handles(geom: &PreparedSfx, spec: &SfxShtackFillSpec) -> Vec<SfxFlowNeedleHandle> {
    let mut handles = Vec::new();
    for glyph in &geom.glyphs {
        let gh = (glyph.warped_bounds[3] - glyph.warped_bounds[1]).abs().max(24.0);
        let outer_rings: Vec<&[(f64, f64)]> = glyph
            .polylines
            .iter()
            .enumerate()
            .filter(|&(pi, _)| contour_is_outer(pi, &glyph.polylines))
            .map(|(_, pl)| pl.pts.as_slice())
            .collect();
        let gctx = FlowGlyphCtx {
            glyph_index: glyph.glyph_index.min(usize::from(u16::MAX)) as u16,
            warped_bounds: glyph.warped_bounds,
            outer_rings: &outer_rings,
            polylines: &glyph.polylines,
            gh,
        };
        let strokes = extract_glyph_flow_strokes(glyph);
        for st in &strokes {
            if st.is_dot || st.closed || st.pts.len() < 2 {
                continue;
            }
            let seg_len: f64 = st.pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
            let r_med = median_flow_radius(&st.pts);
            let needle_px = gh * (f64::from(spec.overshoot_pct).clamp(0.0, 45.0).max(14.0) / 100.0);
            let stub_scale = (seg_len / (r_med * 2.2).max(1.0)).clamp(0.25, 1.0);
            let base_needle_px = (needle_px * stub_scale).min(seg_len * 0.85 + gh * 0.14).max(12.0);

            let first = st.pts[0];
            let second = st.pts[1];
            let d0 = (first.0 - second.0, first.1 - second.1);
            let l0 = d0.0.hypot(d0.1);
            if st.start_kind != FlowEndKind::ShortBridge && l0 > 1e-4 {
                let t0 = (d0.0 / l0, d0.1 / l0);
                let res = resolve_flow_end(st.start_kind, spec, (first.0, first.1), t0, first.2, &gctx);
                let eff = rotate_unit_dir(t0, res.angle_offset_deg);
                handles.push(SfxFlowNeedleHandle {
                    glyph_index: gctx.glyph_index,
                    end_doc: [first.0, first.1],
                    dir: [eff.0, eff.1],
                    base_dir: [t0.0, t0.1],
                    uv: res.uv,
                    enabled: res.enabled,
                    overridden: res.overridden,
                    default_enabled: res.default_enabled,
                    angle_offset_deg: res.angle_offset_deg,
                    length_scale: res.length_scale,
                    base_needle_px,
                });
            }
            let last = st.pts[st.pts.len() - 1];
            let prev = st.pts[st.pts.len() - 2];
            let d1 = (last.0 - prev.0, last.1 - prev.1);
            let l1 = d1.0.hypot(d1.1);
            if st.end_kind != FlowEndKind::ShortBridge && l1 > 1e-4 {
                let t1 = (d1.0 / l1, d1.1 / l1);
                let res = resolve_flow_end(st.end_kind, spec, (last.0, last.1), t1, last.2, &gctx);
                let eff = rotate_unit_dir(t1, res.angle_offset_deg);
                handles.push(SfxFlowNeedleHandle {
                    glyph_index: gctx.glyph_index,
                    end_doc: [last.0, last.1],
                    dir: [eff.0, eff.1],
                    base_dir: [t1.0, t1.1],
                    uv: res.uv,
                    enabled: res.enabled,
                    overridden: res.overridden,
                    default_enabled: res.default_enabled,
                    angle_offset_deg: res.angle_offset_deg,
                    length_scale: res.length_scale,
                    base_needle_px,
                });
            }
        }
    }
    handles
}

/// Synthesizes PaintTool SAI-style tapered pen strokes along one smooth open centerline:
/// 4..=8 varied passes (rich overlapping black core + 1..3 thinner outer whiskers, no zebra),
/// with only 2..3 sharp razor needles shooting past enabled ends (no broomstick).
fn build_flow_open_stroke_polys(
    stroke: &FlowStroke,
    spec: &SfxShtackFillSpec,
    gctx: &FlowGlyphCtx<'_>,
    stroke_seed: u32,
) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    if pts.len() < 2 {
        return Vec::new();
    }
    let seg_len: f64 = pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
    if !seg_len.is_finite() || seg_len < 1.0 {
        return Vec::new();
    }
    let gh = gctx.gh;
    let r_med = median_flow_radius(pts);
    let half_w = (gh * (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 100.0) * 0.5).max(0.55);
    let thickness_scale = (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 1.5).clamp(0.35, 2.2);
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let passes = ((3.5 + density * 4.5).round() as usize).clamp(4, 8);
    let core_count = (passes / 2).clamp(2, 4);

    let needle_px = gh * (f64::from(spec.overshoot_pct).clamp(0.0, 45.0) / 100.0);
    let stub_scale = (seg_len / (r_med * 2.2).max(1.0)).clamp(0.25, 1.0);
    let max_needle = (needle_px * stub_scale).min(seg_len * 0.85 + gh * 0.14);

    let first = pts[0];
    let second = pts[1];
    let d0 = (first.0 - second.0, first.1 - second.1);
    let l0 = d0.0.hypot(d0.1).max(1e-6);
    let t0 = (d0.0 / l0, d0.1 / l0);

    let last = pts[pts.len() - 1];
    let prev = pts[pts.len() - 2];
    let d1 = (last.0 - prev.0, last.1 - prev.1);
    let l1 = d1.0.hypot(d1.1).max(1e-6);
    let t1 = (d1.0 / l1, d1.1 / l1);

    let res0 = resolve_flow_end(stroke.start_kind, spec, (first.0, first.1), t0, first.2, gctx);
    let res1 = resolve_flow_end(stroke.end_kind, spec, (last.0, last.1), t1, last.2, gctx);
    let shoot0 = rotate_unit_dir(t0, res0.angle_offset_deg);
    let shoot1 = rotate_unit_dir(t1, res1.angle_offset_deg);
    let fallback_needle = (gh * 0.14 * stub_scale).min(seg_len * 0.85 + gh * 0.14);
    let eff_needle0 = if res0.overridden && res0.enabled { max_needle.max(fallback_needle) } else { max_needle };
    let eff_needle1 = if res1.overridden && res1.enabled { max_needle.max(fallback_needle) } else { max_needle };
    let max_reach = (eff_needle0 * f64::from(res0.length_scale)).max(eff_needle1 * f64::from(res1.length_scale));
    let samples_n = ((seg_len + max_reach * 2.0) / if gh < 130.0 { 4.0 } else { 2.4 }).round().clamp(14.0, 96.0) as usize;

    let mut polys = Vec::with_capacity(passes);
    for k in 0..passes {
        let p_seed = stroke_seed.wrapping_add((k as u32).wrapping_mul(1_277));
        let r0 = f64::from(hash_u32(p_seed));
        let r1 = f64::from(hash_u32(p_seed.wrapping_add(17)));
        let r2 = f64::from(hash_u32(p_seed.wrapping_add(43)));
        let r3 = f64::from(hash_u32(p_seed.wrapping_add(79)));
        let r4 = f64::from(hash_u32(p_seed.wrapping_add(113)));

        let is_core = k < core_count;
        // Core passes overlap richly across [-0.54, +0.54] of local radius leaving rare crisp
        // SAI pen-slit highlights; outer whiskers sit near ±(0.68..0.94) with organic separation.
        let u_k = if is_core {
            let frac = if core_count <= 1 { 0.5 } else { k as f64 / (core_count - 1) as f64 };
            (-0.54 + 1.08 * frac) + (r0 - 0.5) * 0.28 * rough
        } else {
            let wk = k - core_count;
            let side = if wk.is_multiple_of(2) { 1.0 } else { -1.0 };
            let tier = (wk / 2) as f64;
            side * (0.70 + 0.18 * tier + 0.14 * r0) * (0.85 + 0.20 * rough)
        };

        // Only 2..3 designated passes shoot needles past enabled ends (no broomstick!).
        let needle_mult = |rv: f64| -> f64 {
            if k == 0 {
                0.92 + 0.42 * rv
            } else if k == core_count {
                0.55 + 0.45 * rv
            } else if k == 1 && (spec.overshoot_pct >= 10.0 || res0.overridden || res1.overridden) {
                0.28 + 0.34 * rv
            } else {
                0.0
            }
        };
        let o0 = if res0.enabled && eff_needle0 > 0.1 {
            eff_needle0 * f64::from(res0.length_scale) * needle_mult(r2)
        } else {
            0.0
        };
        let o1 = if res1.enabled && eff_needle1 > 0.1 {
            eff_needle1 * f64::from(res1.length_scale) * needle_mult(r3)
        } else {
            0.0
        };

        let mut ext = pts.clone();
        if o0 > 0.1 {
            if res0.angle_offset_deg.abs() > 2.0 {
                let p_mid = (first.0 + t0.0 * (o0 * 0.14), first.1 + t0.1 * (o0 * 0.14), first.2 * 0.88);
                ext.insert(0, p_mid);
            }
            ext.insert(0, (first.0 + shoot0.0 * o0, first.1 + shoot0.1 * o0, first.2 * 0.72));
        }
        if o1 > 0.1 {
            if res1.angle_offset_deg.abs() > 2.0 {
                let p_mid = (last.0 + t1.0 * (o1 * 0.14), last.1 + t1.1 * (o1 * 0.14), last.2 * 0.88);
                ext.push(p_mid);
            }
            ext.push((last.0 + shoot1.0 * o1, last.1 + shoot1.1 * o1, last.2 * 0.72));
        }
        let base = catmull_rom_resample_flow(&ext, samples_n);
        if base.len() < 2 {
            continue;
        }

        let total_ext = (seg_len + o0 + o1).max(1.0);
        let start_body = (o0 / total_ext).clamp(0.0, 0.45);
        let end_body = (1.0 - o1 / total_ext).clamp(0.55, 1.0);
        let tilt = (r2 - 0.5) * 0.26 * rough;
        let bow = (r4 - 0.5) * 0.28 * rough;
        let wobble_scale = (0.10 + 0.22 * rough) * half_w;

        let mut samples = Vec::with_capacity(base.len());
        for (i, &p) in base.iter().enumerate() {
            let t = i as f64 / (base.len() - 1) as f64;
            let p_prev = base[i.saturating_sub(1)];
            let p_next = base[(i + 1).min(base.len() - 1)];
            let tx = p_next.0 - p_prev.0;
            let ty = p_next.1 - p_prev.1;
            let tl = tx.hypot(ty).max(1e-6);
            let nx = -ty / tl;
            let ny = tx / tl;

            let local_r = p.2.max(half_w * 0.85);
            let body_hw = if is_core {
                let target = local_r * (0.25 + 0.17 * density) * thickness_scale.powf(0.65);
                target.clamp(half_w * 0.68, local_r * 0.62) * (0.82 + 0.34 * r1)
            } else {
                (half_w * (0.26 + 0.26 * r1)).max(0.28)
            };

            // Protruding needles stay slim & razor-sharp like a real SAI G-Pen overshoot
            // (never a thick triangular thorn wedge).
            let in_start_needle = o0 > 0.1 && t < start_body;
            let in_end_needle = o1 > 0.1 && t > end_body;
            let hw = if in_start_needle {
                let u = (t / start_body.max(1e-4)).clamp(0.0, 1.0);
                let slim_cap = (body_hw * 0.52).min(half_w * 0.82);
                let w_blend = slim_cap + (body_hw - slim_cap) * u.powf(3.0);
                (u.powf(1.10) * w_blend).max(0.11)
            } else if in_end_needle {
                let u = ((1.0 - t) / (1.0 - end_body).max(1e-4)).clamp(0.0, 1.0);
                let slim_cap = (body_hw * 0.52).min(half_w * 0.82);
                let w_blend = slim_cap + (body_hw - slim_cap) * u.powf(3.0);
                (u.powf(1.10) * w_blend).max(0.11)
            } else {
                let body_span = (end_body - start_body).max(1e-4);
                let d_in = ((t - start_body) / body_span).clamp(0.0, 1.0);
                let d_out = ((end_body - t) / body_span).clamp(0.0, 1.0);
                let edge_s = if o0 > 0.1 { 1.0 } else { (d_in / 0.12).clamp(0.0, 1.0).powf(0.65) };
                let edge_e = if o1 > 0.1 { 1.0 } else { (d_out / 0.12).clamp(0.0, 1.0).powf(0.65) };
                let body_taper = if is_core { edge_s.min(edge_e) } else { edge_s.min(edge_e) * (PI * t).sin().max(0.0).powf(0.35) };
                (body_taper * body_hw).max(0.14)
            };

            let tip_gather = if in_start_needle || in_end_needle {
                0.55 + 0.45 * (PI * t).sin().max(0.0)
            } else {
                1.0
            };
            let noise = (f64::from(value_noise_1d(t as f32 * 16.0 + (k as f32) * 5.3, p_seed)) - 0.5) * 2.0 * wobble_scale;
            let off = (u_k * tip_gather + tilt * (t - 0.5) + bow * (PI * t).sin()) * local_r + noise;
            samples.push((p.0 + nx * off, p.1 + ny * off, hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&samples) {
            polys.push(poly);
        }
    }
    polys
}

/// Synthesizes smooth Catmull-Rom SAI pen sweeps around a closed glyph loop (`O`, `0`)
/// using 4..=8 varied-thickness core + whisker passes (no zebra rings).
fn build_flow_closed_loop_polys(stroke: &FlowStroke, spec: &SfxShtackFillSpec, gh: f64, stroke_seed: u32) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let total: f64 = (0..n)
        .map(|i| {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            (b.0 - a.0).hypot(b.1 - a.1)
        })
        .sum();
    if !total.is_finite() || total < 1.5 {
        return Vec::new();
    }

    let half_w = (gh * (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 100.0) * 0.5).max(0.55);
    let thickness_scale = (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 1.5).clamp(0.35, 2.2);
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let overshoot_ratio = f64::from(spec.overshoot_pct).clamp(0.0, 45.0) / 100.0;
    let passes = ((3.5 + density * 4.5).round() as usize).clamp(4, 8);
    let core_count = (passes / 2).clamp(2, 4);
    let samples_n = (total / if gh < 130.0 { 3.6 } else { 2.4 }).round().clamp(24.0, 96.0) as usize;

    let mut polys = Vec::with_capacity(passes);
    for k in 0..passes {
        let p_seed = stroke_seed.wrapping_add((k as u32).wrapping_mul(1_451));
        let r0 = f64::from(hash_u32(p_seed));
        let r1 = f64::from(hash_u32(p_seed.wrapping_add(23)));
        let r2 = f64::from(hash_u32(p_seed.wrapping_add(59)));
        let r3 = f64::from(hash_u32(p_seed.wrapping_add(97)));

        let is_core = k < core_count;
        let u_k = if is_core {
            let frac = if core_count <= 1 { 0.5 } else { k as f64 / (core_count - 1) as f64 };
            (-0.42 + 0.84 * frac) + (r0 - 0.5) * 0.22 * rough
        } else {
            let wk = k - core_count;
            let side = if wk.is_multiple_of(2) { 1.0 } else { -1.0 };
            side * (0.64 + 0.20 * r0) * (0.85 + 0.20 * rough)
        };

        let s0 = total * ((k as f64 + 0.5 + (r2 - 0.5) * 0.7) / passes as f64);
        let sweep_len = total * (1.06 + 0.08 * r3 + overshoot_ratio * 0.32);
        let wobble_scale = (0.10 + 0.18 * rough) * half_w;

        let base = catmull_rom_sample_closed_sweep(pts, s0, sweep_len, samples_n);
        if base.len() < 3 {
            continue;
        }
        let mut samples = Vec::with_capacity(base.len());
        for (i, &p) in base.iter().enumerate() {
            let t = i as f64 / (base.len() - 1) as f64;
            let p_prev = base[i.saturating_sub(1)];
            let p_next = base[(i + 1).min(base.len() - 1)];
            let tx = p_next.0 - p_prev.0;
            let ty = p_next.1 - p_prev.1;
            let tl = tx.hypot(ty).max(1e-6);
            let nx = -ty / tl;
            let ny = tx / tl;

            let local_r = p.2.max(half_w * 0.8);
            let body_hw = if is_core {
                let target = local_r * (0.36 + 0.18 * density) * thickness_scale.powf(0.65);
                target.clamp(half_w * 0.72, local_r * 0.76) * (0.84 + 0.28 * r1)
            } else {
                (half_w * (0.28 + 0.26 * r1)).max(0.30)
            };
            let taper = (t / 0.10).min((1.0 - t) / 0.10).clamp(0.0, 1.0).powf(0.75);
            let hw = (taper * body_hw).max(0.12);
            let noise = (f64::from(value_noise_1d(t as f32 * 18.0 + (k as f32) * 4.1, p_seed)) - 0.5) * 2.0 * wobble_scale;
            let off = u_k * local_r + noise;
            samples.push((p.0 + nx * off, p.1 + ny * off, hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&samples) {
            polys.push(poly);
        }
    }
    polys
}

/// Synthesizes a rich, hand-inked SAI pen dot matching the weight and character of the main
/// letter strokes (dense overlapping curved core strokes + 2–3 crisp outer flicks, never a bare ring).
fn build_flow_dot_polys(stroke: &FlowStroke, spec: &SfxShtackFillSpec, gh: f64, stroke_seed: u32) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    if pts.len() < 3 {
        return Vec::new();
    }
    let n = pts.len() as f64;
    let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let cy = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let rx = pts.iter().map(|p| (p.0 - cx).abs()).fold(0.0_f64, f64::max).max(2.0);
    let ry = pts.iter().map(|p| (p.1 - cy).abs()).fold(0.0_f64, f64::max).max(2.0);
    let rad = rx.max(ry).max(median_flow_radius(pts) * 0.85).max(3.0);
    let half_w = (gh * (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 100.0) * 0.5).max(0.65);
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let overshoot_ratio = f64::from(spec.overshoot_pct).clamp(0.0, 45.0) / 100.0;
    let stroke_count = ((5.0 + density * 4.0).round() as usize).clamp(5, 9);
    let base_ang = -0.68_f64 + (f64::from(hash_u32(stroke_seed)) - 0.5) * 0.45;

    let mut polys = Vec::with_capacity(stroke_count + 3);
    // 1. Overlapping curved ink strokes matching the bold vertical/diagonal SAI pen body.
    for k in 0..stroke_count {
        let p_seed = stroke_seed.wrapping_add((k as u32).wrapping_mul(1_301));
        let r0 = f64::from(hash_u32(p_seed));
        let r1 = f64::from(hash_u32(p_seed.wrapping_add(19)));
        let r2 = f64::from(hash_u32(p_seed.wrapping_add(53)));
        let is_cross = k >= stroke_count - 2;
        let ang = if is_cross {
            base_ang + 1.15 + (r1 - 0.5) * 0.35
        } else {
            base_ang + (r1 - 0.5) * 0.32
        };
        let (sa, ca) = ang.sin_cos();
        let nx = -sa;
        let ny = ca;
        let main_n = (stroke_count - 2).max(1) as f64;
        let u = if is_cross {
            (r0 - 0.5) * 0.72
        } else {
            -0.68 + 1.36 * ((k as f64 + 0.5 + (r0 - 0.5) * 0.35) / main_n)
        };
        let chord = (1.0 - (u * 0.85).powi(2)).max(0.32).sqrt();
        let needle_boost = if k == 0 || k == stroke_count - 1 { overshoot_ratio * 0.65 } else { 0.08 };
        let ext = rad * (chord * 1.02 + needle_boost * (0.6 + 0.6 * r2));
        let off = u * rad * 0.78;
        // Core dot strokes scale proportionally with dot radius so thick fonts get rich solid ink.
        let w_pass = if is_cross {
            (rad * 0.24).max(half_w * 0.55) * (0.75 + 0.35 * r2)
        } else {
            (rad * (0.28 + 0.14 * density)).max(half_w * 0.80) * (0.78 + 0.38 * r2)
        };
        let mut samples = Vec::with_capacity(9);
        for i in 0..9 {
            let t = f64::from(i) / 8.0;
            let s = (t * 2.0 - 1.0) * ext;
            let bow = (r0 - 0.5) * rad * 0.24 * (PI * t).sin();
            let taper = (PI * t).sin().max(0.0).powf(0.42);
            let hw = (taper * w_pass).max(0.16);
            samples.push((cx + ca * s + nx * (off + bow), cy + sa * s + ny * (off + bow), hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&samples) {
            polys.push(poly);
        }
    }
    // 2. Two expressive eccentric ink wraps sealing the dot contour organically.
    for loop_i in 0..2usize {
        let l_seed = stroke_seed.wrapping_add((loop_i as u32).wrapping_mul(2_903)).wrapping_add(77);
        let jx = (f64::from(hash_u32(l_seed)) - 0.5) * rad * 0.18;
        let jy = (f64::from(hash_u32(l_seed.wrapping_add(13))) - 0.5) * rad * 0.18;
        let s0 = f64::from(hash_u32(l_seed.wrapping_add(31))) * std::f64::consts::TAU;
        let sweep = std::f64::consts::TAU * (1.08 + 0.15 * overshoot_ratio);
        let wrap_w = (rad * 0.22).max(half_w * 0.72);
        let mut samples = Vec::with_capacity(20);
        for i in 0..20 {
            let t = f64::from(i) / 19.0;
            let a = s0 + sweep * t;
            let r_mod = 0.84 + 0.18 * f64::from(value_noise_1d(t as f32 * 6.0, l_seed));
            let taper = (t / 0.14).min((1.0 - t) / 0.14).clamp(0.0, 1.0).powf(0.7);
            let hw = (taper * wrap_w).max(0.16);
            samples.push((cx + jx + rad * r_mod * a.cos(), cy + jy + rad * r_mod * a.sin(), hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&samples) {
            polys.push(poly);
        }
    }
    polys
}

/// Builds PaintTool SAI-style stroke-following pen bundles across all glyphs in `geom`.
fn build_shtack_flow_polygons(geom: &PreparedSfx, spec: &SfxShtackFillSpec) -> Vec<InkPolygon> {
    let seed_base = spec.seed.wrapping_mul(19_417);
    let mut polys = Vec::new();
    for (gi, glyph) in geom.glyphs.iter().enumerate() {
        let gh = (glyph.warped_bounds[3] - glyph.warped_bounds[1]).abs().max(24.0);
        let outer_rings: Vec<&[(f64, f64)]> = glyph
            .polylines
            .iter()
            .enumerate()
            .filter(|&(pi, _)| contour_is_outer(pi, &glyph.polylines))
            .map(|(_, pl)| pl.pts.as_slice())
            .collect();
        let gctx = FlowGlyphCtx {
            glyph_index: glyph.glyph_index.min(usize::from(u16::MAX)) as u16,
            warped_bounds: glyph.warped_bounds,
            outer_rings: &outer_rings,
            polylines: &glyph.polylines,
            gh,
        };
        let strokes = extract_glyph_flow_strokes(glyph);
        for (si, st) in strokes.iter().enumerate() {
            let s_seed = seed_base.wrapping_add((gi as u32).wrapping_mul(2_671)).wrapping_add((si as u32).wrapping_mul(547));
            if st.is_dot {
                polys.extend(build_flow_dot_polys(st, spec, gh, s_seed));
            } else if st.closed {
                polys.extend(build_flow_closed_loop_polys(st, spec, gh, s_seed));
            } else {
                polys.extend(build_flow_open_stroke_polys(st, spec, &gctx, s_seed));
            }
        }
    }
    polys
}

// =============================================================================
// 5C. 🖌 UKIYO-E WET SUMI-E BRUSH (PRESSURE CORE + KASURE EDGE/TAIL BRISTLES)
// =============================================================================

/// Synthesizes one wet Ukiyo-e / Sumi-e brush stroke along a smoothed open centerline:
/// - ZERO SAI cross-overs at junctions (only free tips get sweeping calligraphic brush tails);
/// - Juicy solid ink core with organic calligraphic pressure swell (`uchikomi` entry + breath + flick);
/// - Split dry bristles (`kasure`) concentrated at the outer edges and trailing exit tails (never a comb).
fn build_ukiyo_open_stroke_polys(stroke: &FlowStroke, spec: &SfxShtackFillSpec, gh: f64, stroke_seed: u32) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    if pts.len() < 2 {
        return Vec::new();
    }
    let seg_len: f64 = pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
    if !seg_len.is_finite() || seg_len < 1.0 {
        return Vec::new();
    }
    let r_med = median_flow_radius(pts);
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let thickness_scale = (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 1.5).clamp(0.45, 1.8);
    let flick_px = gh * (f64::from(spec.overshoot_pct).clamp(0.0, 45.0) / 100.0) * 0.58;
    let max_flick = flick_px.min(seg_len * 0.55 + r_med * 1.4);

    let first = pts[0];
    let second = pts[1];
    let d0 = (first.0 - second.0, first.1 - second.1);
    let l0 = d0.0.hypot(d0.1).max(1e-6);
    let t0 = (d0.0 / l0, d0.1 / l0);

    let last = pts[pts.len() - 1];
    let prev = pts[pts.len() - 2];
    let d1 = (last.0 - prev.0, last.1 - prev.1);
    let l1 = d1.0.hypot(d1.1).max(1e-6);
    let t1 = (d1.0 / l1, d1.1 / l1);

    let r_a = f64::from(hash_u32(stroke_seed));
    let r_b = f64::from(hash_u32(stroke_seed.wrapping_add(29)));
    let free_start = stroke.start_kind == FlowEndKind::FreeTip;
    let free_end = stroke.end_kind == FlowEndKind::FreeTip;
    let o0 = if free_start && max_flick > 0.2 { max_flick * (0.28 + 0.42 * r_a) } else { 0.0 };
    let o1 = if free_end && max_flick > 0.2 { max_flick * (0.55 + 0.65 * r_b) } else { 0.0 };

    let mut ext = pts.clone();
    if o0 > 0.2 {
        ext.insert(0, (first.0 + t0.0 * o0, first.1 + t0.1 * o0, first.2 * 0.62));
    }
    if o1 > 0.2 {
        // Subtle calligraphic wrist curve on the brush exit flick.
        let curve = (r_b - 0.5) * o1 * 0.28 * rough;
        let nx1 = -t1.1;
        let ny1 = t1.0;
        ext.push((last.0 + t1.0 * o1 + nx1 * curve, last.1 + t1.1 * o1 + ny1 * curve, last.2 * 0.52));
    }

    let samples_n = ((seg_len + o0 + o1) / if gh < 130.0 { 3.8 } else { 2.2 }).round().clamp(16.0, 80.0) as usize;
    let base = catmull_rom_resample_flow(&ext, samples_n);
    if base.len() < 2 {
        return Vec::new();
    }

    let total_ext = (seg_len + o0 + o1).max(1.0);
    let start_body = (o0 / total_ext).clamp(0.0, 0.35);
    let end_body = (1.0 - o1 / total_ext).clamp(0.65, 1.0);
    let mut polys = Vec::with_capacity(8);

    // 1. Juicy wet sumi-e core ribbon with calligraphic pressure swell.
    let mut core_samples = Vec::with_capacity(base.len());
    let mut normals = Vec::with_capacity(base.len());
    for (i, &p) in base.iter().enumerate() {
        let t = i as f64 / (base.len() - 1) as f64;
        let p_prev = base[i.saturating_sub(1)];
        let p_next = base[(i + 1).min(base.len() - 1)];
        let tx = p_next.0 - p_prev.0;
        let ty = p_next.1 - p_prev.1;
        let tl = tx.hypot(ty).max(1e-6);
        let nx = -ty / tl;
        let ny = tx / tl;
        normals.push((nx, ny));

        // Organic sumi-e pressure: bold entry press, expressive mid-lift, tapering flick tail.
        let breath = 0.88 + 0.18 * (f64::from(value_noise_1d(t as f32 * 5.5, stroke_seed.wrapping_add(7))) - 0.5) * (0.6 + rough);
        let entry_press = if free_start {
            let u = ((t - start_body) / 0.16).clamp(0.0, 1.0);
            1.0 + 0.16 * (PI * u).sin()
        } else {
            1.0
        };
        let end_taper = if t < start_body {
            (t / start_body.max(1e-4)).clamp(0.0, 1.0).powf(0.75)
        } else if t > end_body {
            ((1.0 - t) / (1.0 - end_body).max(1e-4)).clamp(0.0, 1.0).powf(1.20)
        } else {
            let s_cap = if free_start && o0 <= 0.2 { ((t / 0.08).clamp(0.0, 1.0)).powf(0.45) } else { 1.0 };
            let e_cap = if free_end && o1 <= 0.2 { (((1.0 - t) / 0.10).clamp(0.0, 1.0)).powf(0.55) } else { 1.0 };
            s_cap.min(e_cap)
        };
        let core_w = p.2 * (0.76 + 0.22 * density) * thickness_scale.powf(0.45) * breath * entry_press;
        let hw = (core_w * end_taper).max(0.18);
        let edge_wobble = (f64::from(value_noise_1d(t as f32 * 10.0, stroke_seed.wrapping_add(41))) - 0.5) * p.2 * 0.14 * rough;
        core_samples.push((p.0 + nx * edge_wobble, p.1 + ny * edge_wobble, hw));
    }
    if let Some(poly) = ribbon_polygon_from_samples(&core_samples) {
        polys.push(poly);
    }

    // 2. Dry-edge & trailing-tail split bristles (`kasure`) — concentrated on outer rims & exit tails.
    let bristle_n = ((2.0 + rough * 4.5).round() as usize).clamp(2, 6);
    for bk in 0..bristle_n {
        let b_seed = stroke_seed.wrapping_add((bk as u32).wrapping_mul(1_613)).wrapping_add(307);
        let r0 = f64::from(hash_u32(b_seed));
        let r1 = f64::from(hash_u32(b_seed.wrapping_add(19)));
        let r2 = f64::from(hash_u32(b_seed.wrapping_add(53)));
        let side = if bk.is_multiple_of(2) { 1.0 } else { -1.0 };
        let is_tail_wisp = bk < 3 && (free_end || free_start);

        let (i_start, i_end) = if is_tail_wisp && free_end {
            let s = ((base.len() as f64) * (0.45 + 0.25 * r0)).floor() as usize;
            (s.min(base.len().saturating_sub(2)), base.len() - 1)
        } else if is_tail_wisp && free_start {
            let e = ((base.len() as f64) * (0.35 + 0.25 * r0)).ceil() as usize;
            (0, e.clamp(1, base.len() - 1))
        } else {
            let a = ((base.len() as f64) * (0.12 + 0.35 * r0)).floor() as usize;
            let b = ((base.len() as f64) * (0.58 + 0.36 * r1)).ceil() as usize;
            (a.min(base.len().saturating_sub(2)), b.clamp(a + 1, base.len() - 1))
        };
        if i_end <= i_start + 1 {
            continue;
        }
        let span = (i_end - i_start) as f64;
        let lat_base = side * (0.76 + 0.30 * r1);
        let splay = side * (0.12 + 0.24 * rough) * r2;
        let mut b_samples = Vec::with_capacity(i_end - i_start + 1);
        for (local_i, idx) in (i_start..=i_end).enumerate() {
            let u = local_i as f64 / span;
            let p = base[idx];
            let (nx, ny) = normals[idx];
            let taper = (PI * u).sin().max(0.0).powf(0.65);
            let hw = (p.2 * (0.10 + 0.14 * r2) * taper).clamp(0.14, p.2 * 0.30);
            let off = (lat_base + splay * u) * p.2 + (f64::from(value_noise_1d(u as f32 * 14.0, b_seed)) - 0.5) * p.2 * 0.22;
            b_samples.push((p.0 + nx * off, p.1 + ny * off, hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&b_samples) {
            polys.push(poly);
        }
    }
    polys
}

/// Wet Ukiyo-e brush around a closed glyph loop (`O`, `0`): single sweeping calligraphic circle
/// with organic entry/exit pressure and subtle outer dry-edge hair wisps.
fn build_ukiyo_closed_loop_polys(stroke: &FlowStroke, spec: &SfxShtackFillSpec, gh: f64, stroke_seed: u32) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    let n = pts.len();
    if n < 3 {
        return Vec::new();
    }
    let total: f64 = (0..n)
        .map(|i| {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            (b.0 - a.0).hypot(b.1 - a.1)
        })
        .sum();
    if !total.is_finite() || total < 1.5 {
        return Vec::new();
    }
    let density = (f64::from(spec.density_pct) / 100.0).clamp(0.1, 1.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let thickness_scale = (f64::from(spec.thickness_pct).clamp(0.4, 5.0) / 1.5).clamp(0.45, 1.8);
    let s0 = total * f64::from(hash_u32(stroke_seed));
    let sweep_len = total * (1.06 + 0.08 * rough);
    let samples_n = (total / if gh < 130.0 { 3.4 } else { 2.2 }).round().clamp(28.0, 96.0) as usize;
    let base = catmull_rom_sample_closed_sweep(pts, s0, sweep_len, samples_n);
    if base.len() < 3 {
        return Vec::new();
    }

    let mut polys = Vec::with_capacity(5);
    let mut core_samples = Vec::with_capacity(base.len());
    let mut normals = Vec::with_capacity(base.len());
    for (i, &p) in base.iter().enumerate() {
        let t = i as f64 / (base.len() - 1) as f64;
        let p_prev = base[i.saturating_sub(1)];
        let p_next = base[(i + 1).min(base.len() - 1)];
        let tx = p_next.0 - p_prev.0;
        let ty = p_next.1 - p_prev.1;
        let tl = tx.hypot(ty).max(1e-6);
        let nx = -ty / tl;
        let ny = tx / tl;
        normals.push((nx, ny));
        let breath = 0.88 + 0.18 * (f64::from(value_noise_1d(t as f32 * 6.0, stroke_seed.wrapping_add(11))) - 0.5) * (0.6 + rough);
        let taper = (t / 0.08).min((1.0 - t) / 0.10).clamp(0.0, 1.0).powf(0.75);
        let hw = (p.2 * (0.78 + 0.20 * density) * thickness_scale.powf(0.45) * breath * taper).max(0.18);
        core_samples.push((p.0, p.1, hw));
    }
    if let Some(poly) = ribbon_polygon_from_samples(&core_samples) {
        polys.push(poly);
    }

    // 3 subtle outer dry-edge wisps hugging segments of the curve.
    for bk in 0..3usize {
        let b_seed = stroke_seed.wrapping_add((bk as u32).wrapping_mul(1_871));
        let r0 = f64::from(hash_u32(b_seed));
        let r1 = f64::from(hash_u32(b_seed.wrapping_add(31)));
        let side = if bk.is_multiple_of(2) { 1.0 } else { -1.0 };
        let start = ((base.len() as f64) * (0.15 + 0.55 * r0)).floor() as usize;
        let end = (start + (base.len() / 3).max(4)).min(base.len() - 1);
        let span = (end - start).max(1) as f64;
        let mut b_samples = Vec::with_capacity(end - start + 1);
        for (li, idx) in (start..=end).enumerate() {
            let u = li as f64 / span;
            let p = base[idx];
            let (nx, ny) = normals[idx];
            let hw = (p.2 * (0.10 + 0.12 * r1) * (PI * u).sin().max(0.0).powf(0.6)).max(0.14);
            let off = side * (0.78 + 0.24 * r1) * p.2;
            b_samples.push((p.0 + nx * off, p.1 + ny * off, hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&b_samples) {
            polys.push(poly);
        }
    }
    polys
}

/// Wet Sumi-e calligraphic ink dab for compact punctuation dots (`!`, `?`, `i`).
fn build_ukiyo_dot_polys(stroke: &FlowStroke, spec: &SfxShtackFillSpec, stroke_seed: u32) -> Vec<InkPolygon> {
    let pts = &stroke.pts;
    if pts.len() < 3 {
        return Vec::new();
    }
    let n = pts.len() as f64;
    let cx = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let cy = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let rx = pts.iter().map(|p| (p.0 - cx).abs()).fold(0.0_f64, f64::max).max(2.0);
    let ry = pts.iter().map(|p| (p.1 - cy).abs()).fold(0.0_f64, f64::max).max(2.0);
    let rad = rx.max(ry).max(median_flow_radius(pts) * 0.85).max(3.0);
    let rough = (f64::from(spec.roughness_pct) / 100.0).clamp(0.0, 1.0);
    let ang = -0.82_f64 + (f64::from(hash_u32(stroke_seed)) - 0.5) * 0.35;
    let (sa, ca) = ang.sin_cos();
    let nx = -sa;
    let ny = ca;

    let mut polys = Vec::with_capacity(3);
    // Juicy teardrop sumi-e dab.
    let mut samples = Vec::with_capacity(11);
    for i in 0..11 {
        let t = f64::from(i) / 10.0;
        let s = (t * 2.0 - 1.0) * rad * 1.08;
        let bow = rad * 0.16 * (PI * t).sin();
        let swell = (PI * t).sin().max(0.0).powf(0.45) * (0.78 + 0.42 * (1.0 - t));
        samples.push((cx + ca * s + nx * bow, cy + sa * s + ny * bow, (rad * 0.82 * swell).max(0.2)));
    }
    if let Some(poly) = ribbon_polygon_from_samples(&samples) {
        polys.push(poly);
    }
    // 2 fine dry trailing hairs.
    for k in 0..2usize {
        let s_seed = stroke_seed.wrapping_add((k as u32).wrapping_mul(733));
        let side = if k == 0 { 1.0 } else { -1.0 };
        let mut flick = Vec::with_capacity(7);
        for i in 0..7 {
            let t = f64::from(i) / 6.0;
            let s = (-0.2 + 1.55 * t) * rad * (1.0 + 0.2 * rough);
            let off = side * rad * (0.52 + 0.25 * t);
            let hw = (rad * 0.16 * (PI * t).sin().max(0.0).powf(0.6) * (0.7 + 0.3 * f64::from(hash_u32(s_seed)))).max(0.14);
            flick.push((cx + ca * s + nx * off, cy + sa * s + ny * off, hw));
        }
        if let Some(poly) = ribbon_polygon_from_samples(&flick) {
            polys.push(poly);
        }
    }
    polys
}

fn build_ukiyo_brush_polygons(geom: &PreparedSfx, spec: &SfxShtackFillSpec) -> Vec<InkPolygon> {
    let seed_base = spec.seed.wrapping_mul(23_561);
    let mut polys = Vec::new();
    for (gi, glyph) in geom.glyphs.iter().enumerate() {
        let gh = (glyph.warped_bounds[3] - glyph.warped_bounds[1]).abs().max(24.0);
        let strokes = extract_glyph_flow_strokes(glyph);
        for (si, st) in strokes.iter().enumerate() {
            let s_seed = seed_base.wrapping_add((gi as u32).wrapping_mul(3_011)).wrapping_add((si as u32).wrapping_mul(619));
            if st.is_dot {
                polys.extend(build_ukiyo_dot_polys(st, spec, s_seed));
            } else if st.closed {
                polys.extend(build_ukiyo_closed_loop_polys(st, spec, gh, s_seed));
            } else {
                polys.extend(build_ukiyo_open_stroke_polys(st, spec, gh, s_seed));
            }
        }
    }
    polys
}

/// Edge-weighted Washi `kasure` dry breakup for `UkiyoBrush`: keeps the deep ink core rich and
/// solid wet black while fracturing the outer stroke edges and bristle tails organically.
fn apply_ukiyo_kasure_breakup(cov: &mut [f32], w: usize, h: usize, dryness_pct: f32, sharpness_pct: f32, seed_in: u32) {
    let dryness = (f64::from(dryness_pct) / 100.0).clamp(0.0, 1.0);
    if dryness <= 0.001 || w == 0 || h == 0 {
        return;
    }
    let sdf = compute_mask_sdf(cov, w, h);
    let sharp = (f64::from(sharpness_pct) / 100.0).clamp(0.0, 1.0);
    let seed = seed_in.wrapping_mul(11_173);
    for (i, c) in cov.iter_mut().enumerate() {
        if *c <= 1e-4 {
            continue;
        }
        let d_in = f64::from(sdf[i]);
        // Protect deep wet core (`d_in > 3.5px`) unless dryness is pushed very high.
        let edge_weight = (1.0 - ((d_in - 1.0) / (2.8 + dryness * 5.0)).clamp(0.0, 1.0)).powf(0.85);
        let core_skip = ((dryness - 0.55) / 0.45).clamp(0.0, 1.0) * 0.45;
        let local_dry = (dryness * (edge_weight * 1.15 + core_skip)).clamp(0.0, 1.0);
        if local_dry <= 0.02 {
            continue;
        }
        let x = (i % w) as f32;
        let y = (i / w) as f32;
        let grain = f64::from(crisp_paper_grain(x, y, seed));
        let streak = f64::from(value_noise_2d(x * 0.16, y * 0.72, seed.wrapping_add(47)));
        let noise = (grain * 0.65 + streak * 0.35).clamp(0.0, 1.0);
        let ink_score = 1.0 - noise;
        let thresh = local_dry * 0.88;
        let keep = if sharp >= 0.99 {
            if ink_score >= thresh { 1.0 } else { 0.0 }
        } else {
            let slope = 3.0 + sharp * sharp * 55.0;
            ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
        };
        *c = (*c as f64 * keep).clamp(0.0, 1.0) as f32;
    }
}

/// Builds the normalized-winding ink polygons for an independent `Shtack Fill` pass.
fn build_shtack_fill_polygons(geom: &PreparedSfx, spec: &SfxShtackFillSpec) -> Vec<InkPolygon> {
    let mut polys = match spec.mode {
        SfxShtackFillMode::Hatch => {
            let mut p = build_shtack_fill_pass(geom, spec, f64::from(spec.angle_deg), 1.0, 1.0, 0);
            if spec.cross_hatch {
                let cross_angle = f64::from(spec.angle_deg) + f64::from(spec.cross_angle_deg);
                p.extend(build_shtack_fill_pass(geom, spec, cross_angle, 1.25, 0.78, 0x5A17));
            }
            p
        }
        SfxShtackFillMode::StrokeFlow => build_shtack_flow_polygons(geom, spec),
        SfxShtackFillMode::UkiyoBrush => build_ukiyo_brush_polygons(geom, spec),
    };
    normalize_positive_winding(&mut polys);
    polys
}

fn normalize_positive_winding(polys: &mut [InkPolygon]) {
    for poly in polys {
        if signed_area(poly) < 0.0 {
            poly.reverse();
        }
    }
}

/// Renders an independent interior `Shtack Fill` mask (`Hatch`, `StrokeFlow`, or `UkiyoBrush`).
fn render_shtack_fill_coverage(geom: &PreparedSfx, raw_base_cov: &[f32], bounds: Rect, spec: &SfxShtackFillSpec) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let polys = build_shtack_fill_polygons(geom, spec);
    let mut cov = rasterize_polygon_list(&polys, bounds);
    if spec.overshoot_pct <= 0.1 {
        for (c, &mask) in cov.iter_mut().zip(raw_base_cov.iter()) {
            *c *= mask;
        }
    }
    if spec.mode == SfxShtackFillMode::UkiyoBrush {
        let eff_dry = spec.dryness_pct.max(18.0);
        apply_ukiyo_kasure_breakup(&mut cov, w, h, eff_dry, spec.grain_sharpness_pct, spec.seed);
    } else if spec.dryness_pct > 0.5 {
        apply_dry_grain_breakup(&mut cov, w, h, spec.dryness_pct, spec.grain_sharpness_pct, spec.seed);
    }
    cov
}

// =============================================================================
// 6. 4 RUST-READY DRY BRUSH FILL ENGINES (SDF PHYSICS, SEG, ETF+LIC, VECTOR)
// =============================================================================

fn apply_dry_brush_mask(base_cov: &mut [f32], geom: &PreparedSfx, bounds: Rect, w: usize, h: usize, db: &SfxDryBrushSpec, is_binary: bool) {
    if !db.enabled || w == 0 || h == 0 {
        return;
    }
    match db.engine {
        SfxDryBrushEngine::SdfPhysics => apply_dry_brush_sdf_physics(base_cov, bounds, w, h, db, is_binary),
        SfxDryBrushEngine::SdfSeg => apply_dry_brush_seg(base_cov, geom, bounds, w, h, db, is_binary),
        SfxDryBrushEngine::SdfEtf => apply_dry_brush_etf_lic(base_cov, bounds, w, h, db, is_binary),
        SfxDryBrushEngine::Vector => apply_dry_brush_vector(base_cov, geom, bounds, db, is_binary),
    }
}

fn apply_dry_brush_sdf_physics(base_cov: &mut [f32], bounds: Rect, w: usize, h: usize, db: &SfxDryBrushSpec, is_binary: bool) {
    let sdf = compute_mask_sdf(base_cov, w, h);
    let ang = db.angle_deg.to_radians();
    let (sin_a, cos_a) = ang.sin_cos();
    let dryness = db.dryness_pct / 100.0;
    let freq = 0.08 + (db.freq_pct / 100.0) * 0.42;
    let warp = (db.warp_pct / 100.0) * 14.0;
    let grain = db.paper_grain_pct / 100.0;
    let wear = db.edge_wear_pct / 100.0;
    let sharp = (db.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
    let seed = db.seed.wrapping_mul(10_007);

    for y in 0..h {
        let py = (bounds.y0 + y as i32) as f32;
        for x in 0..w {
            let idx = y * w + x;
            if base_cov[idx] <= 0.02 {
                continue;
            }
            let px = (bounds.x0 + x as i32) as f32;
            let u = px * cos_a + py * sin_a;
            let v = -px * sin_a + py * cos_a;
            let w_off = (fbm_2d(u * 0.035, v * 0.035, seed.wrapping_add(11)) - 0.5) * warp;
            let bristle = value_noise_2d(u * 0.025, (v + w_off) * freq, seed);
            let paper = crisp_paper_grain(px, py, seed.wrapping_add(97));
            let edge_fade = (sdf[idx] / (3.0 + wear * 16.0)).clamp(0.0, 1.0);
            let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary || sharp >= 0.99 {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                let slope = 5.5 + sharp * sharp * 70.0;
                ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
            };
            base_cov[idx] *= keep;
        }
    }
}

/// A smoothed glyph contour with cumulative arc length and chunk AABBs for fast curvilinear Seg lookup.
struct SegContour {
    pts: Vec<(f64, f64)>,
    cum: Vec<f64>,
    aabb: [f64; 4],
    chunks: Vec<(usize, usize, [f64; 4])>,
    phase: f64,
}

fn build_seg_chunks(pts: &[(f64, f64)]) -> Vec<(usize, usize, [f64; 4])> {
    const CHUNK_SEGS: usize = 8;
    let n_segs = pts.len().saturating_sub(1);
    let mut chunks = Vec::with_capacity(n_segs.div_ceil(CHUNK_SEGS));
    let mut start = 0;
    while start < n_segs {
        let end = (start + CHUNK_SEGS).min(n_segs);
        let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for &(x, y) in &pts[start..=end] {
            bb[0] = bb[0].min(x);
            bb[1] = bb[1].min(y);
            bb[2] = bb[2].max(x);
            bb[3] = bb[3].max(y);
        }
        chunks.push((start, end, bb));
        start = end;
    }
    chunks
}

fn build_seg_contours(geom: &PreparedSfx) -> Vec<SegContour> {
    let mut contours = Vec::new();
    for (gi, pl) in geom.polylines.iter().enumerate() {
        // Keep the smooth source contour; only duplicate and sub-half-pixel edges are dropped.
        let mut pts: Vec<(f64, f64)> = Vec::with_capacity(pl.pts.len() + 1);
        for &p in &pl.pts {
            if !p.0.is_finite() || !p.1.is_finite() {
                continue;
            }
            if let Some(&q) = pts.last()
                && (p.0 - q.0).hypot(p.1 - q.1) < 0.5
            {
                continue;
            }
            pts.push(p);
        }
        if pts.len() < 2 {
            continue;
        }
        if let (Some(&first), Some(&last)) = (pts.first(), pts.last())
            && (first.0 - last.0).hypot(first.1 - last.1) >= 0.5
        {
            pts.push(first);
        }
        let mut cum = vec![0.0_f64; pts.len()];
        for i in 1..pts.len() {
            cum[i] = cum[i - 1] + (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
        }
        let total = cum[pts.len() - 1];
        if !total.is_finite() || total < 0.5 {
            continue;
        }
        let mut aabb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for &(x, y) in &pts {
            aabb[0] = aabb[0].min(x);
            aabb[1] = aabb[1].min(y);
            aabb[2] = aabb[2].max(x);
            aabb[3] = aabb[3].max(y);
        }
        let chunks = build_seg_chunks(&pts);
        contours.push(SegContour { pts, cum, aabb, chunks, phase: (gi as f64) * 37.0 });
    }
    contours
}

fn seg_aabb_dist2(aabb: [f64; 4], px: f64, py: f64) -> f64 {
    let dx = (aabb[0] - px).max(0.0).max(px - aabb[2]);
    let dy = (aabb[1] - py).max(0.0).max(py - aabb[3]);
    dx * dx + dy * dy
}

/// Closest point on one smooth contour as `(arc length, squared distance)`.
fn closest_on_seg_contour(c: &SegContour, px: f64, py: f64, best_d2: f64) -> Option<(f64, f64)> {
    let mut best = best_d2;
    let mut best_s = 0.0_f64;
    let mut found = false;
    for &(start, end, bb) in &c.chunks {
        if seg_aabb_dist2(bb, px, py) > best {
            continue;
        }
        for i in start..end {
            let a = c.pts[i];
            let b = c.pts[i + 1];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let l2 = dx * dx + dy * dy;
            if l2 < 1e-9 {
                continue;
            }
            let t = (((px - a.0) * dx + (py - a.1) * dy) / l2).clamp(0.0, 1.0);
            let cx = a.0 + dx * t - px;
            let cy = a.1 + dy * t - py;
            let d2 = cx * cx + cy * cy;
            if d2 < best {
                best = d2;
                best_s = c.cum[i] + t * (c.cum[i + 1] - c.cum[i]);
                found = true;
            }
        }
    }
    if found { Some((best_s, best)) } else { None }
}

/// Evaluates the closest smooth contour point across all contours, seeding `best_d2` with the
/// previous pixel's winning contour so AABB pruning rejects distant contours immediately.
/// Returns `(arc_len, contour_total_len, contour_phase, best_d2)`.
fn closest_curvilinear_sd(contours: &[SegContour], px: f64, py: f64, hint: &mut usize) -> (f64, f64, f64, f64) {
    let mut best_d2 = f64::INFINITY;
    let mut s = 0.0_f64;
    let mut total = 0.0_f64;
    let mut phase = 0.0_f64;
    let h = (*hint).min(contours.len().saturating_sub(1));
    if let Some((cs, d2)) = closest_on_seg_contour(&contours[h], px, py, best_d2) {
        best_d2 = d2;
        s = cs;
        total = *contours[h].cum.last().unwrap_or(&0.0);
        phase = contours[h].phase;
    }
    for (ci, c) in contours.iter().enumerate() {
        if ci == h || seg_aabb_dist2(c.aabb, px, py) > best_d2 {
            continue;
        }
        if let Some((cs, d2)) = closest_on_seg_contour(c, px, py, best_d2) {
            best_d2 = d2;
            s = cs;
            total = *c.cum.last().unwrap_or(&0.0);
            phase = c.phase;
            *hint = ci;
        }
    }
    (s, total, phase, best_d2)
}

/// Evaluates bristle + warp noise periodically along a closed contour of perimeter `total`
/// so the `s = 0 ↔ total` loop seam on round glyphs (`O`, `0`, `B`) is completely invisible.
fn sample_seamless_bristle(s: f64, total: f64, phase: f64, v: f32, warp: f32, freq: f32, seed: u32) -> f32 {
    let eval = |u_arc: f64| -> f32 {
        let u = (u_arc + phase) as f32;
        let w_off = (fbm_2d(u * 0.035, v * 0.035, seed.wrapping_add(17)) - 0.5) * warp;
        value_noise_2d(u * 0.024, (v + w_off) * freq, seed)
    };
    let fade = (total * 0.2).clamp(0.0, 36.0);
    if fade > 1.0 && s > total - fade {
        let t = ((s - (total - fade)) / fade).clamp(0.0, 1.0) as f32;
        let w = t * t * (3.0 - 2.0 * t);
        eval(s) * (1.0 - w) + eval(s - total) * w
    } else {
        eval(s)
    }
}

/// A concave exterior cusp or inner-counter corner whose distance-field fan curves into the glyph body
/// (the source of "plastic-wrap" U-loops at T-junctions such as the crossbar of `B`).
struct SegJunction {
    x: f64,
    y: f64,
    dir_x: f64,
    dir_y: f64,
    radius: f64,
}

fn sample_grid_at(buf: &[f32], bounds: Rect, w: usize, h: usize, wx: f64, wy: f64) -> f32 {
    let gx = (wx.round() as i32) - bounds.x0;
    let gy = (wy.round() as i32) - bounds.y0;
    if gx < 0 || gy < 0 || gx >= w as i32 || gy >= h as i32 {
        return 0.0;
    }
    buf[(gy as usize) * w + (gx as usize)]
}

/// Walks along a closed contour's vertex list by approximately `target_arc` pixels in `step` (`-1` or `+1`).
fn walk_contour_offset(c: &SegContour, start: usize, step: isize, target_arc: f64) -> (f64, f64) {
    let m = c.pts.len().saturating_sub(1);
    if m < 2 {
        return c.pts[start.min(c.pts.len() - 1)];
    }
    let mut cur = start % m;
    let mut acc = 0.0_f64;
    for _ in 0..m {
        let next = ((cur as isize + step).rem_euclid(m as isize)) as usize;
        let seg = (c.pts[next].0 - c.pts[cur].0).hypot(c.pts[next].1 - c.pts[cur].1);
        acc += seg;
        cur = next;
        if acc >= target_arc {
            break;
        }
    }
    c.pts[cur]
}

/// Detects inner-counter corners and concave exterior cusps where distance isolines fan into ink.
fn build_seg_junctions(contours: &[SegContour], base_cov: &[f32], sdf: &[f32], bounds: Rect, w: usize, h: usize) -> Vec<SegJunction> {
    let mut raw: Vec<(f64, SegJunction)> = Vec::new();
    for c in contours {
        let m = c.pts.len().saturating_sub(1);
        if m < 4 {
            continue;
        }
        let total = *c.cum.last().unwrap_or(&0.0);
        let probe_arc = (total * 0.03).clamp(4.0, 9.0);
        for i in 0..m {
            let p = c.pts[i];
            let p_back = walk_contour_offset(c, i, -1, probe_arc);
            let p_fwd = walk_contour_offset(c, i, 1, probe_arc);
            let v1 = (p.0 - p_back.0, p.1 - p_back.1);
            let v2 = (p_fwd.0 - p.0, p_fwd.1 - p.1);
            let l1 = v1.0.hypot(v1.1);
            let l2 = v2.0.hypot(v2.1);
            if l1 < 1e-4 || l2 < 1e-4 {
                continue;
            }
            let (u1x, u1y) = (v1.0 / l1, v1.1 / l1);
            let (u2x, u2y) = (v2.0 / l2, v2.1 / l2);
            let cos_turn = (u1x * u2x + u1y * u2y).clamp(-1.0, 1.0);
            if cos_turn >= 0.78 {
                continue;
            }
            let (bx, by) = (u2x - u1x, u2y - u1y);
            let bl = bx.hypot(by);
            if bl < 1e-4 {
                continue;
            }
            let (bx, by) = (bx / bl, by / bl);
            // Distance-field fan into glyph ink happens iff `+b` lies in empty space and `-b` in ink.
            let empty_side = sample_grid_at(base_cov, bounds, w, h, p.0 + bx * 4.0, p.1 + by * 4.0);
            let ink_side = sample_grid_at(base_cov, bounds, w, h, p.0 - bx * 4.0, p.1 - by * 4.0);
            if empty_side >= 0.35 || ink_side <= 0.5 {
                continue;
            }
            let (dx, dy) = (-bx, -by);
            let mut max_sdf = 6.0_f64;
            for step in 1..=20 {
                let d = f64::from(step) * 2.0;
                let cv = sample_grid_at(base_cov, bounds, w, h, p.0 + dx * d, p.1 + dy * d);
                if cv < 0.3 {
                    break;
                }
                let sv = f64::from(sample_grid_at(sdf, bounds, w, h, p.0 + dx * d, p.1 + dy * d));
                max_sdf = max_sdf.max(sv);
            }
            let radius = (max_sdf * 1.75).clamp(10.0, 44.0);
            raw.push((cos_turn, SegJunction { x: p.0, y: p.1, dir_x: dx, dir_y: dy, radius }));
        }
    }
    // Keep only the sharpest corner within each ~10px cluster.
    raw.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept: Vec<SegJunction> = Vec::new();
    for (_, cand) in raw {
        let dup = kept.iter().any(|k| (k.x - cand.x).hypot(k.y - cand.y) < 10.0);
        if !dup {
            kept.push(cand);
        }
    }
    kept
}

/// Smooth `0..=1` mask covering the plastic-wrap distance-field fan around glyph junctions.
fn junction_cover_at(junctions: &[SegJunction], px: f64, py: f64) -> f32 {
    let mut cover = 0.0_f64;
    for j in junctions {
        let dx = px - j.x;
        let dy = py - j.y;
        let d2 = dx * dx + dy * dy;
        let r2 = j.radius * j.radius;
        if d2 >= r2 {
            continue;
        }
        let proj = dx * j.dir_x + dy * j.dir_y;
        if proj < -j.radius * 0.28 {
            continue;
        }
        let t = (1.0 - d2.sqrt() / j.radius).clamp(0.0, 1.0);
        let w = t * t * (3.0 - 2.0 * t);
        cover = cover.max(w);
    }
    cover as f32
}

fn apply_dry_brush_seg(base_cov: &mut [f32], geom: &PreparedSfx, bounds: Rect, w: usize, h: usize, db: &SfxDryBrushSpec, is_binary: bool) {
    let contours = build_seg_contours(geom);
    if contours.is_empty() {
        apply_dry_brush_sdf_physics(base_cov, bounds, w, h, db, is_binary);
        return;
    }
    let sdf = compute_mask_sdf(base_cov, w, h);
    let junctions = build_seg_junctions(&contours, base_cov, &sdf, bounds, w, h);
    let dryness = db.dryness_pct / 100.0;
    let freq = 0.09 + (db.freq_pct / 100.0) * 0.42;
    let warp = (db.warp_pct / 100.0) * 12.0;
    let grain = db.paper_grain_pct / 100.0;
    let wear = db.edge_wear_pct / 100.0;
    let sharp = (db.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
    let seed = db.seed.wrapping_mul(13_109);
    let mut hint = 0usize;

    for y in 0..h {
        let py = (bounds.y0 + y as i32) as f64;
        for x in 0..w {
            let idx = y * w + x;
            if base_cov[idx] <= 0.02 {
                continue;
            }
            let px = (bounds.x0 + x as i32) as f64;
            // Curvilinear coordinates: arc length s runs ALONG the smoothed outline, and v
            // is the exact continuous distance to it. No chord snapping => no faceting.
            let (mut s, total, phase, mut best_d2) = closest_curvilinear_sd(&contours, px, py, &mut hint);
            if !best_d2.is_finite() {
                best_d2 = f64::from(sdf[idx]).powi(2);
                s = px + py;
            }
            let v = best_d2.sqrt().max(0.0) as f32;
            let mut bristle = sample_seamless_bristle(s, total, phase, v, warp, freq, seed);
            if !junctions.is_empty() {
                let jc = junction_cover_at(&junctions, px, py);
                if jc > 0.0 {
                    // Cover the plastic-wrap distance-field fan at T-junctions & concave cusps
                    // with ink while keeping paper grain natural.
                    bristle += (1.0 - bristle) * jc * 0.90;
                }
            }
            let paper = crisp_paper_grain(px as f32, py as f32, seed.wrapping_add(113));
            let edge_fade = (sdf[idx] / (3.0 + wear * 15.0)).clamp(0.0, 1.0);
            let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary || sharp >= 0.99 {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                let slope = 5.5 + sharp * sharp * 70.0;
                ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
            };
            base_cov[idx] *= keep;
        }
    }
}

fn apply_dry_brush_etf_lic(base_cov: &mut [f32], bounds: Rect, w: usize, h: usize, db: &SfxDryBrushSpec, is_binary: bool) {
    let n = w * h;
    let sdf = compute_mask_sdf(base_cov, w, h);
    let mut tx = vec![0.0_f32; n];
    let mut ty = vec![0.0_f32; n];

    // Seed tangent vector field perpendicular to SDF gradient near boundaries.
    for y in 1..(h.saturating_sub(1)) {
        for x in 1..(w.saturating_sub(1)) {
            let idx = y * w + x;
            let gx = sdf[idx + 1] - sdf[idx - 1];
            let gy = sdf[idx + w] - sdf[idx - w];
            let mag = gx.hypot(gy);
            if mag > 1e-4 {
                tx[idx] = -gy / mag;
                ty[idx] = gx / mag;
            }
        }
    }

    // Harmonic diffusion via 2-pass separable box blur.
    let mut tmp = vec![0.0_f32; n];
    let mut out_x = vec![0.0_f32; n];
    let mut out_y = vec![0.0_f32; n];
    for _ in 0..2 {
        box_blur_1d(&tx, w, h, 5, true, &mut tmp);
        box_blur_1d(&tmp, w, h, 5, false, &mut out_x);
        box_blur_1d(&ty, w, h, 5, true, &mut tmp);
        box_blur_1d(&tmp, w, h, 5, false, &mut out_y);
        tx.copy_from_slice(&out_x);
        ty.copy_from_slice(&out_y);
    }
    for i in 0..n {
        let len = tx[i].hypot(ty[i]);
        if len > 1e-5 {
            tx[i] /= len;
            ty[i] /= len;
        } else {
            tx[i] = 1.0;
            ty[i] = 0.0;
        }
    }

    // High-frequency bristle noise field for Line Integral Convolution (LIC).
    let seed = db.seed.wrapping_mul(19_417);
    let freq = 0.12 + (db.freq_pct / 100.0) * 0.45;
    let mut noise = vec![0.0_f32; n];
    for y in 0..h {
        let py = (bounds.y0 + y as i32) as f32;
        for x in 0..w {
            let px = (bounds.x0 + x as i32) as f32;
            noise[y * w + x] = value_noise_2d(px * freq, py * freq, seed);
        }
    }

    let steps = (db.lic_length_px.round() as usize).clamp(4, 26) / 2;
    let dryness = db.dryness_pct / 100.0;
    let grain = db.paper_grain_pct / 100.0;
    let wear = db.edge_wear_pct / 100.0;
    let sharp = (db.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);

    for y in 0..h {
        let py = (bounds.y0 + y as i32) as f32;
        for x in 0..w {
            let idx = y * w + x;
            if base_cov[idx] <= 0.02 {
                continue;
            }
            let px = (bounds.x0 + x as i32) as f32;
            let mut sum = noise[idx];
            let mut count = 1.0_f32;
            for dir_sign in [-1.0_f32, 1.0_f32] {
                let mut fx = x as f32;
                let mut fy = y as f32;
                for _ in 0..steps {
                    let ix = fx.round().clamp(0.0, (w - 1) as f32) as usize;
                    let iy = fy.round().clamp(0.0, (h - 1) as f32) as usize;
                    let k = iy * w + ix;
                    fx += tx[k] * dir_sign * 1.15;
                    fy += ty[k] * dir_sign * 1.15;
                    let nx = fx.round().clamp(0.0, (w - 1) as f32) as usize;
                    let ny = fy.round().clamp(0.0, (h - 1) as f32) as usize;
                    sum += noise[ny * w + nx];
                    count += 1.0;
                }
            }
            let lic = sum / count;
            let paper = crisp_paper_grain(px, py, seed.wrapping_add(131));
            let edge_fade = (sdf[idx] / (3.0 + wear * 14.0)).clamp(0.0, 1.0);
            let ink_score = lic * (1.0 - grain * 0.32) + paper * (grain * 0.32) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary || sharp >= 0.99 {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                let slope = 5.5 + sharp * sharp * 70.0;
                ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
            };
            base_cov[idx] *= keep;
        }
    }
}

fn build_dry_brush_vector_cut_polys(geom: &PreparedSfx, db: &SfxDryBrushSpec) -> Vec<InkPolygon> {
    let mut cut_polys = Vec::new();
    let count = usize::from(db.vec_bristle_count.clamp(4, 40));
    let rand_amt = f64::from(db.vec_bristle_random_pct) / 100.0;
    let base_ang = f64::from(db.angle_deg).to_radians();
    let mutual_tilt = f64::from(db.vec_mutual_tilt_deg).to_radians();
    let tilt_rand = f64::from(db.vec_tilt_random_deg).to_radians();
    let dryness = f64::from(db.dryness_pct) / 100.0;
    let gauss_dots = f64::from(db.vec_gauss_dots_pct) / 100.0;
    let seed_base = db.seed.wrapping_mul(31_337);

    for (gi, g) in geom.glyphs.iter().enumerate() {
        let gw = (g.warped_bounds[2] - g.warped_bounds[0]).max(12.0);
        let gh = (g.warped_bounds[3] - g.warped_bounds[1]).max(12.0);
        let diag = gw.hypot(gh);
        let cx = g.warped_center[0];
        let cy = g.warped_center[1];
        let g_seed = seed_base.wrapping_add((gi as u32).wrapping_mul(1601));

        for k in 0..count {
            let k_seed = g_seed.wrapping_add((k as u32).wrapping_mul(79));
            let u = ((k as f64 + 0.5) / count as f64 - 0.5) * 2.0;
            let jitter_u = (f64::from(hash_u32(k_seed)) - 0.5) * 0.25 * rand_amt;
            let ang = base_ang + u * mutual_tilt + (f64::from(hash_u32(k_seed.wrapping_add(13))) - 0.5) * 2.0 * tilt_rand;
            let (sa, ca) = ang.sin_cos();
            let px = -sa;
            let py = ca;
            let off = (u + jitter_u) * diag * 0.48;
            let half_len = diag * (0.38 + 0.28 * f64::from(hash_u32(k_seed.wrapping_add(29))));
            let mid_x = cx + px * off;
            let mid_y = cy + py * off;
            let half_w = (diag / count as f64 * 0.32 * dryness * (0.65 + 0.7 * f64::from(hash_u32(k_seed.wrapping_add(47))))).clamp(0.45, 5.5);

            let p0 = (mid_x - ca * half_len, mid_y - sa * half_len);
            let p1 = (mid_x + ca * half_len, mid_y + sa * half_len);
            cut_polys.push(vec![
                (p0.0 + px * half_w * 0.35, p0.1 + py * half_w * 0.35),
                (mid_x + px * half_w, mid_y + py * half_w),
                (p1.0 + px * half_w * 0.35, p1.1 + py * half_w * 0.35),
                (p1.0 - px * half_w * 0.35, p1.1 - py * half_w * 0.35),
                (mid_x - px * half_w, mid_y - py * half_w),
                (p0.0 - px * half_w * 0.35, p0.1 - py * half_w * 0.35),
            ]);
        }

        // Box-Muller Gaussian micro ink-break dots
        if gauss_dots > 0.02 {
            let n_dots = ((count as f64 * 2.5 * gauss_dots).round() as usize).clamp(4, 140);
            for d in 0..n_dots {
                let d_seed = g_seed.wrapping_add((d as u32).wrapping_mul(131)).wrapping_add(999);
                let gx = cx + f64::from(box_muller_gaussian(d_seed, d_seed.wrapping_add(7))) * gw * 0.28;
                let gy = cy + f64::from(box_muller_gaussian(d_seed.wrapping_add(19), d_seed.wrapping_add(31))) * gh * 0.28;
                let rad = (0.75 + f64::from(hash_u32(d_seed.wrapping_add(53))) * 1.8 * dryness).clamp(0.6, 3.2);
                cut_polys.push(vec![
                    (gx - rad, gy),
                    (gx, gy - rad),
                    (gx + rad, gy),
                    (gx, gy + rad),
                ]);
            }
        }
    }
    normalize_positive_winding(&mut cut_polys);
    cut_polys
}

fn apply_dry_brush_vector(base_cov: &mut [f32], geom: &PreparedSfx, bounds: Rect, db: &SfxDryBrushSpec, is_binary: bool) {
    let cut_polys = build_dry_brush_vector_cut_polys(geom, db);
    let cut_cov = rasterize_polygon_list(&cut_polys, bounds);
    for (c, &cut) in base_cov.iter_mut().zip(cut_cov.iter()) {
        *c = (*c * (1.0 - cut)).clamp(0.0, 1.0);
        if is_binary {
            *c = if *c >= 0.5 { 1.0 } else { 0.0 };
        }
    }
}

// =============================================================================
// 7. LEGACY & VECTOR SHADOW / EXTRUDE / GLOW PASSES + MAIN ENTRYPOINT
// =============================================================================

fn apply_grain_distress(data: &SfxData, base_cov: &mut [f32], bounds: Rect, w: usize) {
    if !data.distress.enabled || data.distress.grain <= 0.01 {
        return;
    }
    let g = data.distress.grain.clamp(0.0, 1.0);
    for (i, c) in base_cov.iter_mut().enumerate() {
        if *c > 0.01 {
            let x = bounds.x0 + (i % w) as i32;
            let y = bounds.y0 + (i / w) as i32;
            if hash_grain(data.distress.seed, x, y) < g * 0.22 {
                *c *= 0.2;
            }
        }
    }
}

fn render_vector_shadow_pass(data: &SfxData, geom: &PreparedSfx, bounds: Rect, under_rgba: &mut [[f32; 4]]) {
    let sh = &data.vector_shadow;
    if !sh.enabled {
        return;
    }
    let rgba = color_rgba(sh.color);
    let rad = f64::from(sh.angle_deg).to_radians();
    let (sin_a, cos_a) = rad.sin_cos();

    // Swept 3D extrude behind letters along shadow ray.
    if sh.extrude_depth_px > 0.2 {
        let steps = (sh.extrude_depth_px.ceil() as usize).clamp(2, 48);
        let mut ex_ras = Rasterizer::new(false);
        for st in 1..=steps {
            let d = f64::from(sh.extrude_depth_px) * (st as f64 / steps as f64);
            let polys: Vec<Vec<(f64, f64)>> =
                geom.polylines.iter().map(|pl| pl.pts.iter().map(|&(x, y)| (x + cos_a * d, y + sin_a * d)).collect()).collect();
            ex_ras.add_component(&polys, PathOp::Combine, FillRule::NonZero);
        }
        let ecov = ex_ras.render(bounds);
        paint_solid_over(under_rgba, &ecov, rgba, 1.0);
    }

    // Offset drop shadow silhouette.
    if sh.distance_px > 0.2 {
        let off = sh.offset_px();
        let sr = fill_rasterizer_from_polys(&geom.polylines, off);
        let scov = sr.render(bounds);
        paint_solid_over(under_rgba, &scov, rgba, 0.92);
    }
}

fn render_shadow_passes(data: &SfxData, geom: &PreparedSfx, bounds: Rect, w: usize, h: usize, under_rgba: &mut [[f32; 4]]) {
    for pass in &data.styles {
        if let SfxStylePass::Shadow { enabled: true, dx_px, dy_px, blur_px, color, opacity } = pass {
            let sr = fill_rasterizer_from_polys(&geom.polylines, [f64::from(*dx_px), f64::from(*dy_px)]);
            let scov = blur_coverage(&sr.render(bounds), w, h, *blur_px);
            paint_solid_over(under_rgba, &scov, color_rgba(*color), *opacity);
        }
    }
}

fn render_extrude_passes(data: &SfxData, geom: &PreparedSfx, bounds: Rect, under_rgba: &mut [[f32; 4]]) {
    for pass in &data.styles {
        if let SfxStylePass::Extrude3D { enabled: true, dx_px, dy_px, steps, fill } = pass {
            let step_count = (*steps).clamp(1, 128);
            let mut extrude_ras = Rasterizer::new(false);
            for st in 1..=step_count {
                let t = f64::from(st) / f64::from(step_count);
                let shift = [f64::from(*dx_px) * t, f64::from(*dy_px) * t];
                let polys: Vec<Vec<(f64, f64)>> = geom.polylines.iter().map(|pl| pl.pts.iter().map(|&(x, y)| (x + shift[0], y + shift[1])).collect()).collect();
                extrude_ras.add_component(&polys, PathOp::Combine, FillRule::NonZero);
            }
            let ecov = extrude_ras.render(bounds);
            let epaint = eval_fill_rgba(fill, bounds);
            paint_over(under_rgba, &ecov, &epaint);
        }
    }
}

fn render_stroke_passes(data: &SfxData, geom: &PreparedSfx, bounds: Rect, under_rgba: &mut [[f32; 4]]) {
    let mut cumulative_radius = 0.0_f64;
    let mut stroke_specs: Vec<(f64, SfxLineJoin, f32, &SfxFillSource)> = Vec::new();
    for pass in &data.styles {
        if let SfxStylePass::Stroke { enabled: true, width_px, join, miter_limit, fill } = pass
            && *width_px > 0.01
        {
            cumulative_radius += f64::from(*width_px);
            stroke_specs.push((cumulative_radius, *join, *miter_limit, fill));
        }
    }
    for (radius, join, miter_limit, fill) in stroke_specs.into_iter().rev() {
        let sras = expanded_rasterizer(&geom.polylines, radius, join, miter_limit);
        let scov = sras.render(bounds);
        let spaint = eval_fill_rgba(fill, bounds);
        paint_over(under_rgba, &scov, &spaint);
    }
}

fn render_inner_glow_passes(data: &SfxData, base_cov: &[f32], w: usize, h: usize, under_rgba: &mut [[f32; 4]]) {
    for pass in &data.styles {
        if let SfxStylePass::InnerGlow { enabled: true, width_px, color, opacity } = pass
            && *width_px > 0.01
        {
            let inner_blur = blur_coverage(base_cov, w, h, *width_px);
            let glow_cov: Vec<f32> = base_cov.iter().zip(inner_blur.iter()).map(|(&bc, &ib)| (((1.0 - ib) * 2.0).clamp(0.0, 1.0)) * bc).collect();
            paint_solid_over(under_rgba, &glow_cov, color_rgba(*color), *opacity);
        }
    }
}

fn prepare_proxy_setup(orig_data: &SfxData, full_geom: &PreparedSfx, full_bounds: Rect, preview: bool) -> (Rect, PreparedSfx, f64, Option<SfxData>) {
    // Interactive slider / gizmo drags use a small proxy ceiling for a fast preview; the
    // committed render uses the full-resolution bounds (proxying only above 16 MP / 4096 px).
    let (pw, ph, proxy_scale) = if preview {
        proxy_target_with(full_bounds, PREVIEW_MAX_SIDE, PREVIEW_MAX_PIXELS)
    } else {
        proxy_target(full_bounds)
    };
    if proxy_scale >= 1.0 {
        return (full_bounds, full_geom.clone(), 1.0, None);
    }
    let bounds = Rect::from_xywh(full_bounds.x0, full_bounds.y0, pw as u32, ph as u32);
    let geom = scaled_geometry(full_geom, [full_bounds.x0 as f64, full_bounds.y0 as f64], proxy_scale);
    // Effect widths must shrink with the proxy geometry, otherwise upsampling grows them by
    // 1/proxy_scale and the lettering's halo / outline / shadow is far too thick.
    (bounds, geom, proxy_scale, Some(proxy_scaled_data(orig_data, proxy_scale)))
}

fn finish_sfx_surface(rgba: &[[f32; 4]], bounds: Rect, full_bounds: Rect, proxy_scale: f64, format: PixelFormat) -> Surface {
    if proxy_scale < 1.0 {
        upsample_rgba_to_surface(rgba, bounds.width() as usize, bounds.height() as usize, full_bounds, format)
    } else {
        rgba_to_surface(rgba, bounds, format)
    }
}

// =============================================================================
// STAGE CACHE — reuse geometry + stroke coverage across halo / colour / shadow drags
// =============================================================================

/// The expensive front half of the pipeline: warped glyph geometry plus the base fill and
/// stylized-line / Shtack coverages. Halo width/colour, fill, line colour and vector shadow are
/// applied AFTER these stages, so interactive drags of those parameters hit this cache.
struct CachedSfxStages {
    /// Exact byte-for-byte key over every geometry / stroke parameter (see `sfx_stage_key`).
    key: Vec<u8>,
    proxy_scale: f64,
    /// Halo- and shadow-free coverage rect the cached buffers were rasterized into.
    cache_bounds: Rect,
    full_geom: PreparedSfx,
    raw_base_cov: Vec<f32>,
    line_or_shtack_cov: Option<Vec<f32>>,
}

static SFX_STAGE_CACHE: Mutex<Option<CachedSfxStages>> = Mutex::new(None);

/// Test-only stage timing probe (thread-local so parallel tests cannot interfere).
#[cfg(test)]
#[derive(Clone, Copy, Default)]
struct SfxStageTimings {
    cache_hit: bool,
    geom_ms: f64,
    strokes_ms: f64,
    dry_ms: f64,
    halo_ms: f64,
    total_ms: f64,
}

#[cfg(test)]
thread_local! {
    static LAST_SFX_TIMINGS: std::cell::RefCell<SfxStageTimings> = const {
        std::cell::RefCell::new(SfxStageTimings {
            cache_hit: false,
            geom_ms: 0.0,
            strokes_ms: 0.0,
            dry_ms: 0.0,
            halo_ms: 0.0,
            total_ms: 0.0,
        })
    };
}

/// Deterministic key for all parameters that can change geometry or stroke polygons.
/// Deliberately EXCLUDES `halo_width_px`, `halo_color`, `fill`, `line_color`, `vector_shadow`,
/// `styles` and `dry_brush`: those are cheap post-stages that reuse the cached coverage.
/// Small serde-backed key builder. Avoids a giant `serde_json::json!` expansion (which hits
/// the macro recursion limit) while keeping an exact, collision-free byte key.
struct KeyBuilder(Vec<u8>);

impl KeyBuilder {
    fn new() -> Self {
        Self(Vec::with_capacity(1024))
    }

    fn push<T: serde::Serialize>(&mut self, label: &str, value: &T) {
        self.0.extend_from_slice(label.as_bytes());
        self.0.push(0);
        if let Ok(bytes) = serde_json::to_vec(value) {
            self.0.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            self.0.extend_from_slice(&bytes);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// Deterministic key for all parameters that can change geometry or stroke polygons.
/// Deliberately EXCLUDES `halo_width_px`, `halo_color`, `fill`, `line_color`, `vector_shadow`,
/// `styles` and `dry_brush`: those are cheap post-stages that reuse the cached coverage.
fn sfx_stage_key(data: &SfxData, dpi: f32, preview: bool) -> Vec<u8> {
    let mut k = KeyBuilder::new();
    let le = &data.line_effect;
    k.push("dpi", &dpi.to_bits());
    k.push("preview", &preview);
    k.push("text", &data.text);
    k.push("font_family", &data.font_family);
    k.push("font_style", &data.font_style);
    k.push("postscript_name", &data.postscript_name);
    k.push("size_pt", &data.size_pt.to_bits());
    k.push("weight", &data.weight);
    k.push("italic", &data.italic);
    k.push("tracking", &data.tracking.to_bits());
    k.push("leading_scale", &data.leading_scale.to_bits());
    k.push("vertical", &data.vertical);
    k.push("warp_mode", &format!("{:?}", data.warp_mode));
    k.push("origin", &[data.origin[0].to_bits(), data.origin[1].to_bits()]);
    k.push("free_transform", &data.free_transform);
    k.push("spline", &data.spline);
    k.push("per_glyph", &data.per_glyph);
    k.push("mesh", &data.mesh);
    k.push("liquify_strokes", &data.liquify_strokes);
    k.push("distress", &data.distress);
    // Line / Shtack stroke parameters.
    k.push("style", &format!("{:?}", le.style));
    k.push("line_width_px", &le.line_width_px.to_bits());
    k.push("one_bit_intensity", &le.one_bit_intensity.to_bits());
    k.push("spike_shape", &format!("{:?}", le.spike_shape));
    k.push("spike_length_px", &le.spike_length_px.to_bits());
    k.push("spike_size_px", &le.spike_size_px.to_bits());
    k.push("spike_count", &le.spike_count);
    k.push("spike_random_pct", &le.spike_random_pct.to_bits());
    k.push("spike_seed", &le.spike_seed);
    k.push("gpen_contrast_pct", &le.gpen_contrast_pct.to_bits());
    k.push("gpen_angle_deg", &le.gpen_angle_deg.to_bits());
    k.push("gpen_pinch_pct", &le.gpen_pinch_pct.to_bits());
    k.push("gpen_overshoot_px", &le.gpen_overshoot_px.to_bits());
    k.push("gpen_overshoot_count_pct", &le.gpen_overshoot_count_pct.to_bits());
    k.push("gpen_overshoot_random_pct", &le.gpen_overshoot_random_pct.to_bits());
    k.push("gpen_seed", &le.gpen_seed);
    k.push("ink_brush_roughness_pct", &le.ink_brush_roughness_pct.to_bits());
    k.push("ink_brush_dropout_pct", &le.ink_brush_dropout_pct.to_bits());
    k.push("ink_brush_pressure_pct", &le.ink_brush_pressure_pct.to_bits());
    k.push("ink_brush_overshoot_px", &le.ink_brush_overshoot_px.to_bits());
    k.push("ink_brush_overshoot_count_pct", &le.ink_brush_overshoot_count_pct.to_bits());
    k.push("ink_brush_overshoot_random_pct", &le.ink_brush_overshoot_random_pct.to_bits());
    k.push("ink_brush_seed", &le.ink_brush_seed);
    k.push("shtack_variant", &format!("{:?}", le.shtack_variant));
    k.push("shtack_bristles", &le.shtack_bristles);
    k.push("shtack_split_pct", &le.shtack_split_pct.to_bits());
    k.push("shtack_overshoot_px", &le.shtack_overshoot_px.to_bits());
    k.push("shtack_overshoot_mode", &format!("{:?}", le.shtack_overshoot_mode));
    k.push("shtack_thickness_px", &le.shtack_thickness_px.to_bits());
    k.push("shtack_dryness_pct", &le.shtack_dryness_pct.to_bits());
    // Dry Pencil breakup happens in the cached stroke stage, so sharpness invalidates it.
    k.push("shtack_grain_sharpness_pct", &le.shtack_grain_sharpness_pct.to_bits());
    k.push("shtack_seed", &le.shtack_seed);
    k.finish()
}

/// True when neither primary fill, secondary blended fill, dry brush, nor any interior Blending FX
/// paints the glyph interior.
fn sfx_has_hollow_interior(data: &SfxData) -> bool {
    matches!(data.fill, SfxFillSource::None)
        && !data.dry_brush.enabled
        && data.blending.secondary_fill.is_none()
        && !data.blending.effects.iter().any(|fx| {
            fx.enabled()
                && matches!(
                    fx,
                    SfxBlendEffect::FillOverlay { .. }
                        | SfxBlendEffect::InnerShadow { .. }
                        | SfxBlendEffect::InnerGlow { .. }
                        | SfxBlendEffect::Bevel { .. }
                        | SfxBlendEffect::Stroke { position: SfxStrokePosition::Inside | SfxStrokePosition::Center, .. }
                )
        })
}

/// Rasterized coverage rect with halo / vector shadow / legacy style / blending FX padding removed,
/// so it stays identical while those cheap passes (or flow needle drags) are edited interactively.
fn cache_bounds_for(data: &SfxData, geom: &PreparedSfx) -> Rect {
    let mut d = data.clone();
    d.fill = SfxFillSource::None;
    d.blending.secondary_fill = None;
    d.blending.effects.clear();
    d.line_effect.halo_width_px = 0.0;
    d.vector_shadow.enabled = false;
    d.styles.clear();
    compute_render_bounds(&d, geom)
}

/// Copies cached coverage into the (possibly larger, halo-padded) working rect. Only cheap
/// row copies — no geometry or polygon rasterization — so a colour/halo drag stays interactive.
fn embed_coverage(cached: &[f32], from: Rect, to: Rect) -> Vec<f32> {
    let tw = to.width() as usize;
    let th = to.height() as usize;
    let mut out = vec![0.0_f32; tw.saturating_mul(th)];
    let cw = from.width() as usize;
    let ch = from.height() as usize;
    if cached.len() < cw.saturating_mul(ch) || tw == 0 || th == 0 {
        return out;
    }
    let dx = i64::from(from.x0) - i64::from(to.x0);
    let dy = i64::from(from.y0) - i64::from(to.y0);
    for cy in 0..ch {
        let ty = dy + cy as i64;
        if ty < 0 || ty >= th as i64 {
            continue;
        }
        for cx in 0..cw {
            let tx = dx + cx as i64;
            if tx < 0 || tx >= tw as i64 {
                continue;
            }
            out[ty as usize * tw + tx as usize] = cached[cy * cw + cx];
        }
    }
    out
}

/// Rasterizes the base glyph silhouette and stylized line / Shtack coverage into `cache_bounds`.
fn rasterize_uncached_stages(geom: &PreparedSfx, le: &SfxLineEffect, cache_bounds: Rect) -> (Vec<f32>, Option<Vec<f32>>, f64) {
    let cw = cache_bounds.width() as usize;
    let ch = cache_bounds.height() as usize;
    let base_ras = fill_rasterizer_from_polys(&geom.polylines, [0.0, 0.0]);
    let raw = base_ras.render(cache_bounds);
    let mut face = raw.clone();
    let mut line: Option<Vec<f32>> = None;
    let mut dry_ms = 0.0_f64;

    match le.style {
        SfxLineStyle::Clean => {
            if le.line_width_px > 0.05 {
                let style = StrokeStyle {
                    width: f64::from(le.line_width_px),
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.0,
                    dashes: Vec::new(),
                    dash_offset: 0.0,
                };
                let sp = stroke_polygons(&geom.polylines, &style, 0.25);
                line = Some(rasterize_polygon_list(&sp, cache_bounds));
            }
        }
        SfxLineStyle::OneBit => {
            let intensity = le.one_bit_intensity / 100.0;
            for (i, c) in face.iter_mut().enumerate() {
                let px = (cache_bounds.x0 + (i % cw) as i32) as f32;
                let py = (cache_bounds.y0 + (i / cw) as i32) as f32;
                let jitter = (value_noise_2d(px * 0.45, py * 0.45, 701) - 0.5) * intensity * 0.55;
                *c = if *c + jitter >= 0.5 { 1.0 } else { 0.0 };
            }
            if le.line_width_px > 0.05 {
                let mut lcov = dilate_coverage_approx(&face, cw, ch, le.line_width_px * 0.5);
                for (i, c) in lcov.iter_mut().enumerate() {
                    let px = (cache_bounds.x0 + (i % cw) as i32) as f32;
                    let py = (cache_bounds.y0 + (i / cw) as i32) as f32;
                    let j = (value_noise_2d(px * 0.38, py * 0.38, 919) - 0.5) * intensity * 0.5;
                    let edge = (*c - face[i]).max(0.0);
                    *c = if edge + j >= 0.45 { 1.0 } else { 0.0 };
                }
                line = Some(lcov);
            }
        }
        SfxLineStyle::Spikes => {
            let mut sp_polys = build_spikes_polygons(geom, le);
            if le.line_width_px > 0.05 {
                let style = StrokeStyle {
                    width: f64::from(le.line_width_px),
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.0,
                    dashes: Vec::new(),
                    dash_offset: 0.0,
                };
                sp_polys.extend(stroke_polygons(&geom.polylines, &style, 0.25));
            }
            line = Some(rasterize_polygon_list(&sp_polys, cache_bounds));
        }
        SfxLineStyle::GPenNib => {
            let gp_polys = build_gpen_polygons(geom, le);
            line = Some(rasterize_polygon_list(&gp_polys, cache_bounds));
        }
        SfxLineStyle::BrushStroke => {
            let bs_polys = build_brush_stroke_polygons(geom, le);
            line = Some(rasterize_polygon_list(&bs_polys, cache_bounds));
        }
        SfxLineStyle::Shtack => {
            let polys = build_shtack_contour_polygons(geom, le);
            let mut shtack_cov = rasterize_polygon_list(&polys, cache_bounds);
            if le.shtack_variant == SfxShtackVariant::DryPencil {
                let t = Instant::now();
                apply_shtack_dry_breakup(&mut shtack_cov, cw, ch, le);
                dry_ms += t.elapsed().as_secs_f64() * 1000.0;
            }
            line = Some(shtack_cov);
        }
    }
    (raw, line, dry_ms)
}

/// Evaluates Stage 1 (Halo dilation behind all letters) into `under_rgba` and returns elapsed ms.
fn render_halo_pass(
    data: &SfxData,
    face_cov: Option<&[f32]>,
    line_cov: Option<&[f32]>,
    bounds: Rect,
    under_rgba: &mut [[f32; 4]],
) -> f64 {
    let le = &data.line_effect;
    if le.halo_width_px <= 0.05 || data.blending.halo_opacity <= 1e-4 {
        return 0.0;
    }
    let halo_start = Instant::now();
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let mut union_cov = face_cov.map_or_else(|| vec![0.0_f32; w * h], <[f32]>::to_vec);
    if let Some(lc) = line_cov {
        for (u, &l) in union_cov.iter_mut().zip(lc.iter()) {
            *u = u.max(l);
        }
    }
    let mut halo_cov = dilate_coverage_approx(&union_cov, w, h, le.halo_width_px);
    if le.style == SfxLineStyle::OneBit {
        let intensity = le.one_bit_intensity / 100.0;
        for (i, c) in halo_cov.iter_mut().enumerate() {
            let px = (bounds.x0 + (i % w) as i32) as f32;
            let py = (bounds.y0 + (i / w) as i32) as f32;
            let j = (value_noise_2d(px * 0.3, py * 0.3, 1301) - 0.5) * intensity * 0.45;
            *c = if *c + j >= 0.48 { 1.0 } else { 0.0 };
        }
    }
    composite_solid_blend(
        under_rgba,
        &halo_cov,
        color_rgba(le.halo_color),
        data.blending.halo_opacity,
        data.blending.halo_blend,
    );
    halo_start.elapsed().as_secs_f64() * 1000.0
}

struct FillPassCtx<'a> {
    geom: &'a PreparedSfx,
    base_cov: &'a [f32],
    bounds: Rect,
    is_binary: bool,
}

/// Evaluates one fill channel (`Primary` or `Secondary` blended fill) into `dst`.
/// Returns elapsed ms spent on dry-brush / Shtack-fill breakup.
fn render_fill_channel(
    fill: &SfxFillSource,
    ctx: &FillPassCtx<'_>,
    dry_brush: Option<&SfxDryBrushSpec>,
    opacity: f32,
    blend: SfxChannelBlend,
    dst: &mut [[f32; 4]],
) -> f64 {
    if opacity <= 1e-4 {
        return 0.0;
    }
    let w = ctx.bounds.width() as usize;
    let h = ctx.bounds.height() as usize;
    match fill {
        SfxFillSource::None => 0.0,
        SfxFillSource::ShtackHatch { spec } => {
            let t = Instant::now();
            let cov = render_shtack_fill_coverage(ctx.geom, ctx.base_cov, ctx.bounds, spec);
            composite_solid_blend(dst, &cov, color_rgba(spec.color), opacity, blend);
            t.elapsed().as_secs_f64() * 1000.0
        }
        SfxFillSource::DryBrush { color, spec } => {
            let t = Instant::now();
            let mut cov = ctx.base_cov.to_vec();
            let mut db = *spec;
            db.enabled = true;
            apply_dry_brush_mask(&mut cov, ctx.geom, ctx.bounds, w, h, &db, ctx.is_binary);
            composite_solid_blend(dst, &cov, color_rgba(*color), opacity, blend);
            t.elapsed().as_secs_f64() * 1000.0
        }
        SfxFillSource::Solid { .. } | SfxFillSource::Gradient { .. } | SfxFillSource::Screentone { .. } => {
            let mut dry_ms = 0.0_f64;
            let mut cov = ctx.base_cov.to_vec();
            if let Some(db) = dry_brush.filter(|d| d.enabled) {
                let t = Instant::now();
                apply_dry_brush_mask(&mut cov, ctx.geom, ctx.bounds, w, h, db, ctx.is_binary);
                dry_ms = t.elapsed().as_secs_f64() * 1000.0;
            }
            let paint = eval_fill_rgba(fill, ctx.bounds);
            composite_rgba_blend(dst, &cov, &paint, opacity, blend);
            dry_ms
        }
    }
}

/// Bilinear pixel shift of a coverage mask by `(dx, dy)` pixels (used by DropShadow & InnerShadow).
fn shift_coverage(cov: &[f32], w: usize, h: usize, dx: f32, dy: f32) -> Vec<f32> {
    let mut out = vec![0.0_f32; w * h];
    if w == 0 || h == 0 || cov.len() < w * h {
        return out;
    }
    let wi = w as isize;
    let hi = h as isize;
    for y in 0..h {
        let sy = y as f32 - dy;
        let y0 = sy.floor() as isize;
        let fy = sy - y0 as f32;
        for x in 0..w {
            let sx = x as f32 - dx;
            let x0 = sx.floor() as isize;
            let fx = sx - x0 as f32;
            let sample = |ix: isize, iy: isize| -> f32 {
                if ix >= 0 && iy >= 0 && ix < wi && iy < hi {
                    cov[iy as usize * w + ix as usize]
                } else {
                    0.0
                }
            };
            let v00 = sample(x0, y0);
            let v10 = sample(x0 + 1, y0);
            let v01 = sample(x0, y0 + 1);
            let v11 = sample(x0 + 1, y0 + 1);
            out[y * w + x] = (v00 * (1.0 - fx) * (1.0 - fy) + v10 * fx * (1.0 - fy) + v01 * (1.0 - fx) * fy + v11 * fx * fy).clamp(0.0, 1.0);
        }
    }
    out
}

/// Builds a combined silhouette mask for outer effects (DropShadow / OuterGlow / Outside Stroke).
fn union_silhouette_cov(face_cov: Option<&[f32]>, line_cov: Option<&[f32]>, n: usize) -> Vec<f32> {
    let mut u = face_cov.map_or_else(|| vec![0.0_f32; n], <[f32]>::to_vec);
    if let Some(lc) = line_cov {
        for (dst, &l) in u.iter_mut().zip(lc.iter()) {
            *dst = dst.max(l);
        }
    }
    u
}

/// Stage 0B: Photoshop-style outer effects rendered BEHIND Halo & letters (`DropShadow`, `OuterGlow`).
fn render_blend_effects_behind(
    effects: &[SfxBlendEffect],
    union_cov: &[f32],
    w: usize,
    h: usize,
    dst: &mut [[f32; 4]],
) {
    for fx in effects.iter().rev() {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::DropShadow { angle_deg, distance_px, extrude_depth_px, blur_px, color, blend, opacity, .. } => {
                if *opacity <= 1e-4 {
                    continue;
                }
                let rad = angle_deg.to_radians();
                let (sin_a, cos_a) = rad.sin_cos();
                let rgba = color_rgba(*color);
                if *extrude_depth_px > 0.2 {
                    let steps = (extrude_depth_px.ceil() as usize).clamp(2, 48);
                    let mut ecov = vec![0.0_f32; w * h];
                    for st in 1..=steps {
                        let d = *extrude_depth_px * (st as f32 / steps as f32);
                        let s = shift_coverage(union_cov, w, h, cos_a * d, sin_a * d);
                        for (e, &v) in ecov.iter_mut().zip(s.iter()) {
                            *e = e.max(v);
                        }
                    }
                    composite_solid_blend(dst, &ecov, rgba, *opacity, *blend);
                }
                if *distance_px > 0.2 || *blur_px > 0.1 {
                    let shifted = shift_coverage(union_cov, w, h, cos_a * *distance_px, sin_a * *distance_px);
                    let scov = if *blur_px > 0.1 { blur_coverage(&shifted, w, h, *blur_px) } else { shifted };
                    composite_solid_blend(dst, &scov, rgba, *opacity, *blend);
                }
            }
            SfxBlendEffect::OuterGlow { spread_px, blur_px, color, blend, opacity, .. } => {
                if *opacity <= 1e-4 || (*spread_px <= 0.05 && *blur_px <= 0.05) {
                    continue;
                }
                let expanded = if *spread_px > 0.05 {
                    dilate_coverage_approx(union_cov, w, h, *spread_px)
                } else {
                    union_cov.to_vec()
                };
                let gcov = if *blur_px > 0.1 { blur_coverage(&expanded, w, h, *blur_px) } else { expanded };
                composite_solid_blend(dst, &gcov, color_rgba(*color), *opacity, *blend);
            }
            _ => {}
        }
    }
}

/// Stage 1B: Outside Strokes from `SfxBlending::effects` (rendered over Halo, just outside letters).
fn render_blend_outside_strokes(
    effects: &[SfxBlendEffect],
    union_cov: &[f32],
    ctx: &FillPassCtx<'_>,
    dst: &mut [[f32; 4]],
) -> f64 {
    let w = ctx.bounds.width() as usize;
    let h = ctx.bounds.height() as usize;
    let mut ms = 0.0_f64;
    for fx in effects.iter().rev() {
        if let SfxBlendEffect::Stroke { enabled: true, width_px, position: SfxStrokePosition::Outside, fill, blend, opacity, .. } = fx
            && *width_px > 0.05
            && *opacity > 1e-4
        {
            let dilated = dilate_coverage_approx(union_cov, w, h, *width_px);
            let ring_cov: Vec<f32> = dilated.iter().zip(union_cov.iter()).map(|(&d, &u)| (d - u).clamp(0.0, 1.0)).collect();
            let sctx = FillPassCtx { geom: ctx.geom, base_cov: &ring_cov, bounds: ctx.bounds, is_binary: ctx.is_binary };
            ms += render_fill_channel(fill, &sctx, None, *opacity, *blend, dst);
        }
    }
    ms
}

struct BevelParams {
    depth_px: f32,
    angle_deg: f32,
    hi_rgba: [f32; 4],
    hi_op: f32,
    sh_rgba: [f32; 4],
    sh_op: f32,
}

/// Crisp SDF-based bevel highlight & shadow clipped to `face_cov`.
fn apply_bevel_effect(sdf: &[f32], face_cov: &[f32], w: usize, h: usize, p: BevelParams, dst: &mut [[f32; 4]]) {
    if w < 3 || h < 3 || p.depth_px <= 0.1 {
        return;
    }
    let rad = p.angle_deg.to_radians();
    let (lx, ly) = (rad.cos(), rad.sin());
    let depth = p.depth_px.max(1.0);
    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let i = y * w + x;
            let fc = face_cov[i];
            if fc <= 0.02 || sdf[i] >= depth + 1.0 {
                continue;
            }
            let gx = sdf[i + 1] - sdf[i - 1];
            let gy = sdf[i + w] - sdf[i - w];
            let mag = gx.hypot(gy);
            if mag <= 1e-4 {
                continue;
            }
            let ramp = ((depth + 0.5 - sdf[i]) / depth).clamp(0.0, 1.0);
            let ndotl = (gx / mag) * lx + (gy / mag) * ly;
            if ndotl > 0.05 && p.hi_op > 1e-4 {
                let cov = (ndotl * ramp * fc).clamp(0.0, 1.0);
                composite_pixel_blend(&mut dst[i], p.hi_rgba, cov, p.hi_op, SfxChannelBlend::Screen);
            } else if ndotl < -0.05 && p.sh_op > 1e-4 {
                let cov = ((-ndotl) * ramp * fc).clamp(0.0, 1.0);
                composite_pixel_blend(&mut dst[i], p.sh_rgba, cov, p.sh_op, SfxChannelBlend::Multiply);
            }
        }
    }
}

fn render_inside_or_center_stroke(
    fx: &SfxBlendEffect,
    ctx: &FillPassCtx<'_>,
    union_cov: &[f32],
    cached_sdf: &mut Option<Vec<f32>>,
    dst: &mut [[f32; 4]],
) -> f64 {
    let w = ctx.bounds.width() as usize;
    let h = ctx.bounds.height() as usize;
    match fx {
        SfxBlendEffect::Stroke { width_px, position: SfxStrokePosition::Inside, fill, blend, opacity, .. }
            if *width_px > 0.05 && *opacity > 1e-4 =>
        {
            let sdf = cached_sdf.get_or_insert_with(|| compute_mask_sdf(ctx.base_cov, w, h));
            let ring_cov: Vec<f32> = ctx.base_cov.iter().zip(sdf.iter()).map(|(&bc, &d)| ((width_px + 0.5 - d).clamp(0.0, 1.0)) * bc).collect();
            let sctx = FillPassCtx { geom: ctx.geom, base_cov: &ring_cov, bounds: ctx.bounds, is_binary: ctx.is_binary };
            render_fill_channel(fill, &sctx, None, *opacity, *blend, dst)
        }
        SfxBlendEffect::Stroke { width_px, position: SfxStrokePosition::Center, fill, blend, opacity, .. }
            if *width_px > 0.05 && *opacity > 1e-4 =>
        {
            let half = (*width_px * 0.5).max(0.5);
            let dilated = dilate_coverage_approx(union_cov, w, h, half);
            let u_sdf = compute_mask_sdf(union_cov, w, h);
            let ring_cov: Vec<f32> = dilated
                .iter()
                .zip(union_cov.iter().zip(u_sdf.iter()))
                .map(|(&dl, (&uc, &d))| (dl - uc).clamp(0.0, 1.0).max(((half + 0.5 - d).clamp(0.0, 1.0)) * uc))
                .collect();
            let sctx = FillPassCtx { geom: ctx.geom, base_cov: &ring_cov, bounds: ctx.bounds, is_binary: ctx.is_binary };
            render_fill_channel(fill, &sctx, None, *opacity, *blend, dst)
        }
        _ => 0.0,
    }
}

/// Stage 2B: Photoshop-style interior & overlay effects rendered OVER primary/secondary fill.
fn render_blend_effects_over(effects: &[SfxBlendEffect], ctx: &FillPassCtx<'_>, union_cov: &[f32], dst: &mut [[f32; 4]]) -> f64 {
    let w = ctx.bounds.width() as usize;
    let h = ctx.bounds.height() as usize;
    let mut cached_sdf: Option<Vec<f32>> = None;
    let mut ms = 0.0_f64;

    for fx in effects.iter().rev() {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::FillOverlay { fill, blend, opacity, .. } => {
                ms += render_fill_channel(fill, ctx, None, *opacity, *blend, dst);
            }
            SfxBlendEffect::Stroke { position: SfxStrokePosition::Inside | SfxStrokePosition::Center, .. } => {
                ms += render_inside_or_center_stroke(fx, ctx, union_cov, &mut cached_sdf, dst);
            }
            SfxBlendEffect::InnerShadow { angle_deg, distance_px, blur_px, color, blend, opacity, .. } if *opacity > 1e-4 => {
                let rad = angle_deg.to_radians();
                let shifted = shift_coverage(ctx.base_cov, w, h, rad.cos() * *distance_px, rad.sin() * *distance_px);
                let inv: Vec<f32> = shifted.iter().map(|&v| (1.0 - v).clamp(0.0, 1.0)).collect();
                let blurred = if *blur_px > 0.1 { blur_coverage(&inv, w, h, *blur_px) } else { inv };
                let scov: Vec<f32> = blurred.iter().zip(ctx.base_cov.iter()).map(|(&b, &fc)| b * fc).collect();
                composite_solid_blend(dst, &scov, color_rgba(*color), *opacity, *blend);
            }
            SfxBlendEffect::InnerGlow { width_px, color, blend, opacity, .. } if *width_px > 0.05 && *opacity > 1e-4 => {
                let inner_blur = blur_coverage(ctx.base_cov, w, h, *width_px);
                let gcov: Vec<f32> = ctx.base_cov.iter().zip(inner_blur.iter()).map(|(&bc, &ib)| (((1.0 - ib) * 2.0).clamp(0.0, 1.0)) * bc).collect();
                composite_solid_blend(dst, &gcov, color_rgba(*color), *opacity, *blend);
            }
            SfxBlendEffect::Bevel { depth_px, angle_deg, highlight_color, highlight_opacity, shadow_color, shadow_opacity, .. } => {
                let sdf = cached_sdf.get_or_insert_with(|| compute_mask_sdf(ctx.base_cov, w, h));
                let bp = BevelParams {
                    depth_px: *depth_px,
                    angle_deg: *angle_deg,
                    hi_rgba: color_rgba(*highlight_color),
                    hi_op: *highlight_opacity,
                    sh_rgba: color_rgba(*shadow_color),
                    sh_op: *shadow_opacity,
                };
                apply_bevel_effect(sdf, ctx.base_cov, w, h, bp, dst);
            }
            _ => {}
        }
    }
    ms
}

struct OuterFxResult {
    primary_shtack_cov: Option<Vec<f32>>,
    union_cov: Option<Vec<f32>>,
    halo_ms: f64,
    extra_dry_ms: f64,
}

/// Stage 1: precomputes primary Shtack coverage when outer passes need it, renders behind-halo
/// Blending FX (`DropShadow`, `OuterGlow`), renders the Halo, and paints `Outside` strokes.
fn render_outer_fx_and_halo(
    data: &SfxData,
    ctx: &FillPassCtx<'_>,
    line_cov: Option<&[f32]>,
    hollow_interior: bool,
    under_rgba: &mut [[f32; 4]],
) -> OuterFxResult {
    let le = &data.line_effect;
    let w = ctx.bounds.width() as usize;
    let h = ctx.bounds.height() as usize;
    let n = w.saturating_mul(h);
    let mut extra_dry_ms = 0.0_f64;

    let has_outer_fx = data.blending.effects.iter().any(|fx| {
        fx.enabled()
            && matches!(
                fx,
                SfxBlendEffect::DropShadow { .. }
                    | SfxBlendEffect::OuterGlow { .. }
                    | SfxBlendEffect::Stroke { position: SfxStrokePosition::Outside | SfxStrokePosition::Center, .. }
            )
    });
    let primary_shtack_cov = if let SfxFillSource::ShtackHatch { spec } = &data.fill
        && ((le.halo_width_px > 0.05 && data.blending.halo_opacity > 1e-4) || has_outer_fx)
    {
        let t = Instant::now();
        let cov = render_shtack_fill_coverage(ctx.geom, ctx.base_cov, ctx.bounds, spec);
        extra_dry_ms += t.elapsed().as_secs_f64() * 1000.0;
        Some(cov)
    } else {
        None
    };
    let halo_face = primary_shtack_cov.as_deref().or((!hollow_interior).then_some(ctx.base_cov));
    let union_cov = has_outer_fx.then(|| union_silhouette_cov(halo_face, line_cov, n));
    if let Some(uc) = union_cov.as_deref() {
        render_blend_effects_behind(&data.blending.effects, uc, w, h, under_rgba);
    }
    let halo_ms = render_halo_pass(data, halo_face, line_cov, ctx.bounds, under_rgba);
    if let Some(uc) = union_cov.as_deref() {
        extra_dry_ms += render_blend_outside_strokes(&data.blending.effects, uc, ctx, under_rgba);
    }
    OuterFxResult { primary_shtack_cov, union_cov, halo_ms, extra_dry_ms }
}

fn build_line_effect_polygons(geom: &PreparedSfx, le: &SfxLineEffect) -> Vec<InkPolygon> {
    let mut polys = match le.style {
        SfxLineStyle::Clean => {
            if le.line_width_px > 0.05 {
                let style = StrokeStyle {
                    width: f64::from(le.line_width_px),
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.0,
                    dashes: Vec::new(),
                    dash_offset: 0.0,
                };
                stroke_polygons(&geom.polylines, &style, 0.25)
            } else {
                Vec::new()
            }
        }
        // `OneBit` line dilation is evaluated on GPU via JFA directly from `face_cov`.
        SfxLineStyle::OneBit => Vec::new(),
        SfxLineStyle::Spikes => {
            let mut sp_polys = build_spikes_polygons(geom, le);
            if le.line_width_px > 0.05 {
                let style = StrokeStyle {
                    width: f64::from(le.line_width_px),
                    cap: LineCap::Round,
                    join: LineJoin::Round,
                    miter_limit: 4.0,
                    dashes: Vec::new(),
                    dash_offset: 0.0,
                };
                sp_polys.extend(stroke_polygons(&geom.polylines, &style, 0.25));
            }
            sp_polys
        }
        SfxLineStyle::GPenNib => build_gpen_polygons(geom, le),
        SfxLineStyle::BrushStroke => build_brush_stroke_polygons(geom, le),
        SfxLineStyle::Shtack => build_shtack_contour_polygons(geom, le),
    };
    normalize_positive_winding(&mut polys);
    polys
}

fn fill_uses_seg_dry_brush(fill: &SfxFillSource) -> bool {
    matches!(fill, SfxFillSource::DryBrush { spec, .. } if spec.engine == SfxDryBrushEngine::SdfSeg)
}

fn data_uses_seg_dry_brush(data: &SfxData) -> bool {
    let primary = data.dry_brush.enabled
        && data.dry_brush.engine == SfxDryBrushEngine::SdfSeg
        && matches!(data.fill, SfxFillSource::Solid { .. } | SfxFillSource::Gradient { .. } | SfxFillSource::Screentone { .. });
    primary
        || fill_uses_seg_dry_brush(&data.fill)
        || data.blending.secondary_fill.as_ref().is_some_and(fill_uses_seg_dry_brush)
        || data.blending.effects.iter().any(|fx| match fx {
            SfxBlendEffect::FillOverlay { enabled: true, fill, .. } | SfxBlendEffect::Stroke { enabled: true, fill, .. } => {
                fill_uses_seg_dry_brush(fill)
            }
            _ => false,
        })
}

/// Extracts a pure O(vertices) [`SfxVectorScene`] with ZERO pixel rasterization (~1–4 ms even on
/// 12 000 × 17 000 px canvases), ready to feed into `photocraft_gpu::SfxGpuRenderer`.
pub fn extract_sfx_vector_scene(data: &SfxData, engine: &mut TextEngine, dpi: f32) -> SfxVectorScene {
    let geom = build_sfx_geometry(data, engine, dpi);
    let full_bounds = compute_render_bounds(data, &geom);
    if full_bounds.is_empty() {
        return SfxVectorScene {
            full_bounds: Rect::EMPTY,
            geom,
            face_contours: Vec::new(),
            line_polygons: Vec::new(),
            primary_shtack_polygons: Vec::new(),
            secondary_shtack_polygons: Vec::new(),
            effect_shtack_polygons: Vec::new(),
            seg_contours: Vec::new(),
            primary_dry_vector_cuts: Vec::new(),
            effect_dry_vector_cuts: Vec::new(),
        };
    }

    let face_contours: Vec<InkPolygon> = geom.polylines.iter().map(|pl| pl.pts.clone()).collect();
    let line_polygons = build_line_effect_polygons(&geom, &data.line_effect);
    let primary_shtack_polygons = match &data.fill {
        SfxFillSource::ShtackHatch { spec } => build_shtack_fill_polygons(&geom, spec),
        _ => Vec::new(),
    };
    let secondary_shtack_polygons = match &data.blending.secondary_fill {
        Some(SfxFillSource::ShtackHatch { spec }) => build_shtack_fill_polygons(&geom, spec),
        _ => Vec::new(),
    };
    let mut effect_shtack_polygons = Vec::new();
    let mut effect_dry_vector_cuts = Vec::new();
    for (idx, fx) in data.blending.effects.iter().enumerate() {
        if !fx.enabled() {
            continue;
        }
        let fill_opt = match fx {
            SfxBlendEffect::FillOverlay { fill, .. } | SfxBlendEffect::Stroke { fill, .. } => Some(fill),
            _ => None,
        };
        if let Some(fill) = fill_opt {
            match fill {
                SfxFillSource::ShtackHatch { spec } => {
                    effect_shtack_polygons.push((idx, build_shtack_fill_polygons(&geom, spec)));
                }
                SfxFillSource::DryBrush { spec, .. } if spec.engine == SfxDryBrushEngine::Vector => {
                    effect_dry_vector_cuts.push((idx, build_dry_brush_vector_cut_polys(&geom, spec)));
                }
                _ => {}
            }
        }
    }
    let primary_dry_vector_cuts = if let SfxFillSource::DryBrush { spec, .. } = &data.fill
        && spec.engine == SfxDryBrushEngine::Vector
    {
        build_dry_brush_vector_cut_polys(&geom, spec)
    } else if data.dry_brush.enabled && data.dry_brush.engine == SfxDryBrushEngine::Vector {
        build_dry_brush_vector_cut_polys(&geom, &data.dry_brush)
    } else {
        Vec::new()
    };
    let seg_contours = if data_uses_seg_dry_brush(data) {
        build_seg_contours(&geom)
            .into_iter()
            .map(|c| {
                let total_len = *c.cum.last().unwrap_or(&0.0) as f32;
                let pts = c.pts.iter().zip(c.cum.iter()).map(|(&(x, y), &s)| [x as f32, y as f32, s as f32]).collect();
                SfxSegContourData { pts, total_len, phase: c.phase as f32 }
            })
            .collect()
    } else {
        Vec::new()
    };

    SfxVectorScene {
        full_bounds,
        geom,
        face_contours,
        line_polygons,
        primary_shtack_polygons,
        secondary_shtack_polygons,
        effect_shtack_polygons,
        seg_contours,
        primary_dry_vector_cuts,
        effect_dry_vector_cuts,
    }
}

/// Renders an [`SfxData`] non-destructively into offscreen document [`Surface`]`(s)`.
/// Existing callers keep working unchanged; pass `preview = true` for fast interactive drags.
pub fn render_sfx(data: &SfxData, engine: &mut TextEngine, format: PixelFormat, dpi: f32) -> SfxRenderOutput {
    render_sfx_with_options(data, engine, format, dpi, false)
}

/// Full SFX render with stage timings emitted through `log::info!` (the browser build prints
/// these straight to the DevTools console).
pub fn render_sfx_with_options(data: &SfxData, engine: &mut TextEngine, format: PixelFormat, dpi: f32, preview: bool) -> SfxRenderOutput {
    let total_start = Instant::now();
    let key = sfx_stage_key(data, dpi, preview);

    // --- Geometry (skipped on a geometry/stroke cache hit) -------------------------------
    let mut cache_hit = false;
    let cached_geom = {
        let guard = SFX_STAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(c) if c.key == key => {
                cache_hit = true;
                Some(c.full_geom.clone())
            }
            _ => None,
        }
    };
    let (full_geom, geom_ms) = if cache_hit {
        (cached_geom.unwrap_or_else(|| build_sfx_geometry(data, engine, dpi)), 0.0)
    } else {
        let t = Instant::now();
        let g = build_sfx_geometry(data, engine, dpi);
        (g, t.elapsed().as_secs_f64() * 1000.0)
    };

    let full_bounds = compute_render_bounds(data, &full_geom);
    if full_bounds.is_empty() {
        let s = Surface::new(format);
        return if data.has_negative_invert() {
            SfxRenderOutput::InvertPair { under: s.clone(), invert: s, bounds: Rect::EMPTY, geom: full_geom }
        } else {
            SfxRenderOutput::Single { surface: s, bounds: Rect::EMPTY, geom: full_geom }
        };
    }

    let (bounds, geom, proxy_scale, scaled_data) = prepare_proxy_setup(data, &full_geom, full_bounds, preview);
    let data = scaled_data.as_ref().unwrap_or(data);
    let le = &data.line_effect;
    let hollow_interior = sfx_has_hollow_interior(data);
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let n = w.saturating_mul(h);

    // The cache rasterizes into the halo/shadow-free rect; the working buffers embed that into
    // the actual (halo-padded) bounds so dilation still has room and never clips the halo.
    let cache_bounds_full = cache_bounds_for(data, &full_geom);
    let cache_bounds = if proxy_scale < 1.0 {
        Rect::from_xywh(
            cache_bounds_full.x0,
            cache_bounds_full.y0,
            ((cache_bounds_full.width() as f64 * proxy_scale).round().max(1.0)) as u32,
            ((cache_bounds_full.height() as f64 * proxy_scale).round().max(1.0)) as u32,
        )
    } else {
        cache_bounds_full
    };
    let mut dry_ms = 0.0_f64;

    // --- Stylized line / Shtack polygons + rasterization (skipped on cache hit) -----------
    let strokes_start = Instant::now();
    let cached_coverages: Option<(Vec<f32>, Option<Vec<f32>>)> = {
        let guard = SFX_STAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()
            .filter(|c| c.key == key && c.proxy_scale == proxy_scale && c.cache_bounds == cache_bounds)
            .map(|c| {
                (
                    embed_coverage(&c.raw_base_cov, c.cache_bounds, bounds),
                    c.line_or_shtack_cov.as_ref().map(|v| embed_coverage(v, c.cache_bounds, bounds)),
                )
            })
    };
    let cached_coverages_hit = cached_coverages.is_some();
    let (raw_base_cov, line_cov) = match cached_coverages {
        Some(pair) => pair,
        None => {
            let (raw, line, stage_dry_ms) = rasterize_uncached_stages(&geom, le, cache_bounds);
            dry_ms += stage_dry_ms;
            let entry = CachedSfxStages {
                key: key.clone(),
                proxy_scale,
                cache_bounds,
                full_geom: full_geom.clone(),
                raw_base_cov: raw.clone(),
                line_or_shtack_cov: line.clone(),
            };
            let mut guard = SFX_STAGE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            *guard = Some(entry);
            (
                embed_coverage(&raw, cache_bounds, bounds),
                line.as_ref().map(|v| embed_coverage(v, cache_bounds, bounds)),
            )
        }
    };
    let strokes_ms = if cached_coverages_hit {
        0.0
    } else {
        strokes_start.elapsed().as_secs_f64() * 1000.0
    };

    let mut face_cov = raw_base_cov;
    if le.style == SfxLineStyle::OneBit {
        let intensity = le.one_bit_intensity / 100.0;
        for (i, c) in face_cov.iter_mut().enumerate() {
            let px = (bounds.x0 + (i % w) as i32) as f32;
            let py = (bounds.y0 + (i / w) as i32) as f32;
            let jitter = (value_noise_2d(px * 0.45, py * 0.45, 701) - 0.5) * intensity * 0.55;
            *c = if *c + jitter >= 0.5 { 1.0 } else { 0.0 };
        }
    }

    // Stage 0: Vector Shadow / 3D + Legacy Shadows / Extrude3D / Stroke Stack
    let mut under_rgba = vec![[0.0_f32; 4]; n];
    render_vector_shadow_pass(data, &geom, bounds, &mut under_rgba);
    render_shadow_passes(data, &geom, bounds, w, h, &mut under_rgba);
    render_extrude_passes(data, &geom, bounds, &mut under_rgba);
    render_stroke_passes(data, &geom, bounds, &mut under_rgba);

    // Stage 1 (Pass 1 — Outer Blending FX + Halo / Ореол + Outside Strokes)
    let stage1_ctx = FillPassCtx { geom: &geom, base_cov: &face_cov, bounds, is_binary: le.style == SfxLineStyle::OneBit };
    let OuterFxResult { primary_shtack_cov, union_cov, halo_ms, extra_dry_ms } =
        render_outer_fx_and_halo(data, &stage1_ctx, line_cov.as_deref(), hollow_interior, &mut under_rgba);
    dry_ms += extra_dry_ms;

    // Stage 2 (Pass 2 — Independent Fill 1, Secondary Blended Fill 2, Photoshop FX & Stylized Line)
    apply_grain_distress(data, &mut face_cov, bounds, w);
    let blending = &data.blending;
    let out = if data.has_negative_invert() {
        let mut inv_cov = if hollow_interior {
            line_cov.clone().unwrap_or_else(|| vec![0.0_f32; n])
        } else {
            let mut c = face_cov.clone();
            if data.dry_brush.enabled {
                let t = Instant::now();
                apply_dry_brush_mask(&mut c, &geom, bounds, w, h, &data.dry_brush, le.style == SfxLineStyle::OneBit);
                dry_ms += t.elapsed().as_secs_f64() * 1000.0;
            }
            c
        };
        let mut invert_rgba = vec![[0.0_f32; 4]; n];
        for (o, c) in invert_rgba.iter_mut().zip(inv_cov.drain(..)) {
            let ca = c.clamp(0.0, 1.0);
            if ca > 1e-6 {
                *o = [1.0, 1.0, 1.0, ca];
            }
        }
        if !hollow_interior
            && let Some(lc) = &line_cov
        {
            composite_solid_blend(&mut under_rgba, lc, color_rgba(le.line_color), blending.line_opacity, blending.line_blend);
        }
        let under = finish_sfx_surface(&under_rgba, bounds, full_bounds, proxy_scale, format);
        let invert = finish_sfx_surface(&invert_rgba, bounds, full_bounds, proxy_scale, format);
        SfxRenderOutput::InvertPair { under, invert, bounds: full_bounds, geom: full_geom }
    } else {
        if blending.line_behind_fill
            && let Some(lc) = &line_cov
        {
            composite_solid_blend(&mut under_rgba, lc, color_rgba(le.line_color), blending.line_opacity, blending.line_blend);
        }
        let ctx = FillPassCtx { geom: &geom, base_cov: &face_cov, bounds, is_binary: le.style == SfxLineStyle::OneBit };
        if let (Some(sc), SfxFillSource::ShtackHatch { spec }) = (&primary_shtack_cov, &data.fill) {
            if blending.fill_opacity > 1e-4 {
                composite_solid_blend(&mut under_rgba, sc, color_rgba(spec.color), blending.fill_opacity, blending.fill_blend);
            }
        } else {
            dry_ms += render_fill_channel(&data.fill, &ctx, Some(&data.dry_brush), blending.fill_opacity, blending.fill_blend, &mut under_rgba);
        }
        if let Some(sec) = &blending.secondary_fill {
            dry_ms += render_fill_channel(sec, &ctx, None, blending.secondary_opacity, blending.secondary_blend, &mut under_rgba);
        }
        if !blending.effects.is_empty() {
            let fallback_union;
            let uc = match union_cov.as_deref() {
                Some(u) => u,
                None => {
                    let eff_face = primary_shtack_cov.as_deref().or((!hollow_interior).then_some(face_cov.as_slice()));
                    fallback_union = union_silhouette_cov(eff_face, line_cov.as_deref(), n);
                    &fallback_union
                }
            };
            dry_ms += render_blend_effects_over(&blending.effects, &ctx, uc, &mut under_rgba);
        }
        if !hollow_interior {
            render_inner_glow_passes(data, &face_cov, w, h, &mut under_rgba);
        }
        if !blending.line_behind_fill
            && let Some(lc) = &line_cov
        {
            composite_solid_blend(&mut under_rgba, lc, color_rgba(le.line_color), blending.line_opacity, blending.line_blend);
        }
        let surface = finish_sfx_surface(&under_rgba, bounds, full_bounds, proxy_scale, format);
        SfxRenderOutput::Single { surface, bounds: full_bounds, geom: full_geom }
    };

    #[cfg(test)]
    LAST_SFX_TIMINGS.with(|t| {
        *t.borrow_mut() = SfxStageTimings {
            cache_hit,
            geom_ms,
            strokes_ms,
            dry_ms,
            halo_ms,
            total_ms: total_start.elapsed().as_secs_f64() * 1000.0,
        };
    });

    log::info!(
        "[SFX render] text={:?} box={}x{} (proxy {:.2}x, preview={}, cache_hit={}) | geom={:.2}ms strokes={:.2}ms dry={:.2}ms halo={:.2}ms | total={:.2}ms",
        data.text,
        full_bounds.width(),
        full_bounds.height(),
        proxy_scale,
        preview,
        cache_hit,
        geom_ms,
        strokes_ms,
        dry_ms,
        halo_ms,
        total_start.elapsed().as_secs_f64() * 1000.0
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::Color;
    use photocraft_doc::sfx::SfxDistress;
    use photocraft_tone::{PatternType, ScreentoneSpec};

    #[test]
    fn render_sfx_solid_strokes_and_screentone_produce_opaque_surface() {
        let mut engine = TextEngine::new();
        let mut data = SfxData {
            text: "BAM".to_owned(),
            fill: SfxFillSource::Screentone {
                spec: ScreentoneSpec { pattern_type: PatternType::Dots, dot_size: 4.0, spacing_x: 10.0, spacing_y: 10.0, ..Default::default() },
                opacity: 1.0,
            },
            distress: SfxDistress { enabled: true, seed: 7, roughness: 1.0, grain: 0.2, edge_chip: 0.1 },
            ..SfxData::default()
        };
        data.styles.push(SfxStylePass::Extrude3D {
            enabled: true,
            dx_px: 5.0,
            dy_px: 5.0,
            steps: 6,
            fill: SfxFillSource::Solid { color: Color::rgb(0.2, 0.2, 0.2) },
        });
        data.styles.push(SfxStylePass::InnerGlow { enabled: true, width_px: 3.0, color: Color::WHITE, opacity: 0.7 });

        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        assert!(matches!(out, SfxRenderOutput::Single { .. }));
        if let SfxRenderOutput::Single { surface, bounds, .. } = out {
            assert!(!bounds.is_empty());
            assert!(surface.tile_count() > 0);
            assert!(!surface.content_bounds().is_empty());
        }
    }

    /// Regression: a >16 MP / >8192 px SFX bbox used to return `Rect::EMPTY` and silently vanish.
    #[test]
    fn render_sfx_beyond_16mp_upsamples_from_proxy_and_keeps_ink() {
        let mut engine = TextEngine::new();
        let data = SfxData {
            text: "BOOM!".to_owned(),
            size_pt: 6_000.0,
            origin: [120.0, 120.0],
            ..SfxData::default()
        };
        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        let bounds = out.bounds();
        assert!(!bounds.is_empty(), "a large SFX layer must never vanish");
        assert!(bounds.width() as usize * bounds.height() as usize > MAX_RENDER_PIXELS, "bbox must exceed 16 MP");
        assert!(
            bounds.width() > MAX_RENDER_EXTENT as u32 || bounds.height() > MAX_RENDER_EXTENT as u32,
            "bbox must exceed 8192 px on at least one axis"
        );
        let SfxRenderOutput::Single { surface, .. } = out else {
            panic!("default SFX has no negative-invert pass");
        };
        let ink = surface.content_bounds();
        assert!(!ink.is_empty(), "proxy must be upsampled into the full target rect");
        let mut visible = false;
        for y in ink.y0..ink.y1 {
            for x in ink.x0..ink.x1 {
                if surface.sample_channel(x, y, 3) > 0.05 {
                    visible = true;
                    break;
                }
            }
            if visible {
                break;
            }
        }
        assert!(visible, "the upsampled layer must contain visible ink pixels");
    }

    #[test]
    fn render_sfx_negative_invert_returns_under_and_invert_pair() {
        let mut engine = TextEngine::new();
        let mut data = SfxData::default();
        data.styles.push(SfxStylePass::NegativeInvert { enabled: true });
        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        assert!(matches!(out, SfxRenderOutput::InvertPair { .. }));
        if let SfxRenderOutput::InvertPair { under, invert, bounds, .. } = out {
            assert!(!bounds.is_empty());
            assert!(under.tile_count() > 0);
            assert!(invert.tile_count() > 0);
        }
    }

    #[test]
    fn render_sfx_all_six_line_styles_produce_valid_surfaces() {
        let mut engine = TextEngine::new();
        let variants = [
            (SfxLineStyle::Clean, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::OneBit, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::Spikes, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::GPenNib, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::BrushStroke, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::Shtack, SfxShtackVariant::SaiPenSketch),
            (SfxLineStyle::Shtack, SfxShtackVariant::DryPencil),
        ];
        for (style, shtack_var) in variants {
            let mut data = SfxData { text: "BA!".to_owned(), ..SfxData::default() };
            data.line_effect.style = style;
            data.line_effect.shtack_variant = shtack_var;
            data.line_effect.halo_width_px = 6.0;
            data.line_effect.line_width_px = 4.0;
            let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
            if let SfxRenderOutput::Single { surface, bounds, .. } = out {
                assert!(!bounds.is_empty(), "{style:?}/{shtack_var:?} bounds empty");
                assert!(surface.tile_count() > 0, "{style:?}/{shtack_var:?} surface empty");
            } else {
                panic!("expected Single output for {style:?}");
            }
        }
    }

    #[test]
    fn render_sfx_all_four_dry_brush_engines_and_vector_shadow() {
        let mut engine = TextEngine::new();
        for eng in [SfxDryBrushEngine::SdfPhysics, SfxDryBrushEngine::SdfSeg, SfxDryBrushEngine::SdfEtf, SfxDryBrushEngine::Vector] {
            let mut data = SfxData { text: "SFX".to_owned(), ..SfxData::default() };
            data.dry_brush.enabled = true;
            data.dry_brush.engine = eng;
            data.vector_shadow.enabled = true;
            data.vector_shadow.angle_deg = 45.0;
            data.vector_shadow.distance_px = 10.0;
            data.vector_shadow.extrude_depth_px = 6.0;
            let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
            if let SfxRenderOutput::Single { surface, bounds, .. } = out {
                assert!(!bounds.is_empty(), "{eng:?} bounds empty");
                assert!(surface.tile_count() > 0, "{eng:?} surface empty");
            } else {
                panic!("expected Single output for {eng:?}");
            }
        }
    }

    // --- Part 4: Shtack overhaul regressions -------------------------------------------------

    fn render_shtack_bam(variant: SfxShtackVariant, halo: f32, overshoot: f32) -> (Surface, Rect, [f64; 4], [f64; 4]) {
        let mut engine = TextEngine::new();
        let mut data = SfxData { text: "BAM!".to_owned(), fill: SfxFillSource::None, ..SfxData::default() };
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_variant = variant;
        data.line_effect.halo_width_px = halo;
        data.line_effect.shtack_overshoot_px = overshoot;
        data.line_effect.shtack_bristles = 5;
        data.line_effect.shtack_dryness_pct = 45.0;
        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        let warped_box = out.geom().warped_box;
        let bang_bounds = out.geom().glyphs.last().map(|g| g.warped_bounds).unwrap_or([0.0; 4]);
        let SfxRenderOutput::Single { surface, bounds, .. } = out else {
            panic!("expected Single output for {variant:?}");
        };
        (surface, bounds, warped_box, bang_bounds)
    }

    /// `(dark ink pixels, visible pixels, all values finite)`. Halo is white, ink is dark,
    /// so the red channel distinguishes real letterpress ink from the halo ring.
    fn shtack_ink_stats(surface: &Surface, bounds: Rect) -> (usize, usize, bool) {
        let (mut dark, mut visible, mut finite) = (0usize, 0usize, true);
        for y in bounds.y0..bounds.y1 {
            for x in bounds.x0..bounds.x1 {
                let a = surface.sample_channel(x, y, 3);
                let r = surface.sample_channel(x, y, 0);
                if !a.is_finite() || !r.is_finite() {
                    finite = false;
                }
                if a > 0.25 {
                    visible += 1;
                    if r < 0.5 {
                        dark += 1;
                    }
                }
            }
        }
        (dark, visible, finite)
    }

    #[test]
    fn shtack_both_modes_keep_the_exclamation_dot_and_finite_coverage() {
        for variant in [SfxShtackVariant::SaiPenSketch, SfxShtackVariant::DryPencil] {
            let (surface, bounds, _, bang) = render_shtack_bam(variant, 8.0, 20.0);
            assert!(!bounds.is_empty(), "{variant:?}: empty bounds");
            let (dark, visible, finite) = shtack_ink_stats(&surface, bounds);
            assert!(finite, "{variant:?}: non-finite pixel");
            assert!(visible > 0 && dark > 0, "{variant:?}: no visible ink");

            // The dot of `!` lives in the bottom quarter of its glyph box. Compact-component
            // preservation must keep solid ink there in BOTH submodes (with halo enabled!).
            let x0 = bang[0].floor() as i32;
            let x1 = bang[2].ceil() as i32;
            let dot_y = bang[1] + (bang[3] - bang[1]) * 0.72;
            let y0 = dot_y.floor() as i32;
            let y1 = bang[3].ceil() as i32;
            let mut dot_ink = 0usize;
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if surface.sample_channel(x, y, 3) > 0.25 && surface.sample_channel(x, y, 0) < 0.5 {
                        dot_ink += 1;
                    }
                }
            }
            assert!(dot_ink > 0, "{variant:?}: the bottom dot of '!' vanished");
        }
    }

    #[test]
    fn shtack_sai_pen_needles_exceed_glyph_bounds_and_dry_pencil_differs() {
        let (sai, sai_bounds, wb, _) = render_shtack_bam(SfxShtackVariant::SaiPenSketch, 0.0, 26.0);
        let ink = sai.content_bounds();
        assert!(!ink.is_empty(), "SAI pen produced no ink");
        let overshot = ink.x0 < wb[0] as i32 - 2
            || ink.y0 < wb[1] as i32 - 2
            || ink.x1 > wb[2] as i32 + 2
            || ink.y1 > wb[3] as i32 + 2;
        assert!(overshot, "SAI pen needles must extend past the raw glyph bounds");

        let (sai_dark, _, _) = shtack_ink_stats(&sai, sai_bounds);
        let (dry, dry_bounds, _, _) = render_shtack_bam(SfxShtackVariant::DryPencil, 0.0, 26.0);
        let (dry_dark, _, _) = shtack_ink_stats(&dry, dry_bounds);
        assert!(sai_dark > 0 && dry_dark > 0, "both submodes need ink");
        assert_ne!(sai_dark, dry_dark, "Dry Pencil must not be a flat copy of SAI pen");
    }

    #[test]
    fn shtack_halo_uses_round_dilation_and_is_never_clipped() {
        // A Euclidean (round) dilation keeps diagonal points at radius <= r and never emits
        // miter spikes beyond it. This is the exact dilation used for the Shtack halo pass.
        // Use a wide margin so the grid border (treated as a distance source) cannot bias
        // thediagonal probe values.
        let (w, h, r, cx, cy) = (81usize, 81usize, 10usize, 40usize, 40usize);
        let mut cov = vec![0.0_f32; w * h];
        cov[cy * w + cx] = 1.0;
        let dil = dilate_coverage_approx(&cov, w, h, r as f32);
        assert!(dil[(cy + 7) * w + (cx + 7)] > 0.5, "round halo must cover 45° at sqrt(98) < r");
        assert!(dil[(cy + 8) * w + (cx + 7)] < 0.5, "no miter spike: sqrt(113) > r");
        assert!(dil[cy * w + (cx + r)] >= 0.45);
        assert!(dil[cy * w + (cx + r + 1)] < 0.5);

        let (surface, bounds, _, _) = render_shtack_bam(SfxShtackVariant::SaiPenSketch, 8.0, 26.0);
        let (_, visible, finite) = shtack_ink_stats(&surface, bounds);
        assert!(finite && visible > 0);
        // Halo + overshoot must fit inside the layer: no opaque pixel on the outer border.
        let (mut border, mut worst, mut where_) = (false, 0.0_f32, (0, 0));
        for x in bounds.x0..bounds.x1 {
            for &(px, py) in &[(x, bounds.y0), (x, bounds.y1 - 1)] {
                let a = surface.sample_channel(px, py, 3);
                if a > worst {
                    worst = a;
                    where_ = (px, py);
                }
                if a > 0.02 {
                    border = true;
                }
            }
        }
        for y in bounds.y0..bounds.y1 {
            for &(px, py) in &[(bounds.x0, y), (bounds.x1 - 1, y)] {
                let a = surface.sample_channel(px, py, 3);
                if a > worst {
                    worst = a;
                    where_ = (px, py);
                }
                if a > 0.02 {
                    border = true;
                }
            }
        }
        assert!(
            !border,
            "sharp Shtack needles + halo must not be clipped at the layer edge: worst alpha {worst} at {where_:?}, bounds {bounds:?}, ink {:?}",
            surface.content_bounds()
        );
    }

    // --- Part 3: large-canvas regressions & stage benchmarks ---------------------------------

    fn render_large_bam(variant: SfxShtackVariant, fill_interior: bool, halo: f32, preview: bool) -> (Surface, Rect, [f64; 4]) {
        let mut engine = TextEngine::new();
        let mut data = SfxData {
            text: "BAM!".to_owned(),
            size_pt: 220.0,
            origin: [60.0, 60.0],
            fill: if fill_interior { SfxFillSource::Solid { color: Color::BLACK } } else { SfxFillSource::None },
            ..SfxData::default()
        };
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_variant = variant;
        data.line_effect.shtack_bristles = 5;
        data.line_effect.shtack_overshoot_px = 20.0;
        data.line_effect.shtack_thickness_px = 2.0; // percentage of glyph height (not pixels)
        data.line_effect.halo_width_px = halo;
        let out = render_sfx_with_options(&data, &mut engine, PixelFormat::RGBA8, 72.0, preview);
        let bang = out.geom().glyphs.last().map(|g| g.warped_bounds).unwrap_or([0.0; 4]);
        let SfxRenderOutput::Single { surface, bounds, .. } = out else {
            panic!("expected Single output for large BAM!");
        };
        (surface, bounds, bang)
    }

    fn dark_ink_in(surface: &Surface, bounds: Rect, x0: i32, x1: i32, y0: i32, y1: i32) -> usize {
        let mut n = 0usize;
        for y in y0.max(bounds.y0)..y1.min(bounds.y1) {
            for x in x0.max(bounds.x0)..x1.min(bounds.x1) {
                if surface.sample_channel(x, y, 3) > 0.25 && surface.sample_channel(x, y, 0) < 0.5 {
                    n += 1;
                }
            }
        }
        n
    }

    /// The bar and dot of `!` are independent vector contours: they must both survive on the
    /// right of a ~1500 px "BAM!" in BOTH Shtack submodes.
    #[test]
    fn shtack_large_bam_keeps_both_exclamation_bar_and_dot() {
        for variant in [SfxShtackVariant::SaiPenSketch, SfxShtackVariant::DryPencil] {
            let (surface, bounds, bang) = render_large_bam(variant, false, 8.0, false);
            println!("[BAM!] {variant:?} bounds {}x{} bang={bang:?}", bounds.width(), bounds.height());
            assert!(bounds.width() > 400, "{variant:?}: expected large lettering, got {}", bounds.width());
            let right = bounds.x0 + (bounds.width() as i32) * 3 / 4;
            let bx0 = right.max(bang[0].floor() as i32);
            let bx1 = bang[2].ceil() as i32;
            let split = bang[1] + (bang[3] - bang[1]) * 0.52;
            let bar = dark_ink_in(&surface, bounds, bx0, bx1, bang[1].floor() as i32, split.floor() as i32);
            let dot = dark_ink_in(&surface, bounds, bx0, bx1, split.ceil() as i32, bang[3].ceil() as i32);
            assert!(bar > 0, "{variant:?}: upper bar of '!' vanished ({bar} ink px)");
            assert!(dot > 0, "{variant:?}: lower dot of '!' vanished ({dot} ink px)");
        }
    }

    #[test]
    fn shtack_fill_interior_produces_distinct_interior_coverage() {
        let (hollow, hb, _) = render_large_bam(SfxShtackVariant::SaiPenSketch, false, 0.0, false);
        let (filled, fb, _) = render_large_bam(SfxShtackVariant::SaiPenSketch, true, 0.0, false);
        let (_, hollow_visible, hf) = shtack_ink_stats(&hollow, hb);
        let (_, filled_visible, ff) = shtack_ink_stats(&filled, fb);
        assert!(hf && ff, "coverage must stay finite in both fill modes");
        assert!(
            filled_visible > hollow_visible,
            "filled interior must cover more pixels ({filled_visible}) than hollow sketch ({hollow_visible})"
        );
    }

    /// Prints exact stage timings with `--nocapture` and checks the cache / preview / no-halo paths.
    #[test]
    fn shtack_render_stage_benchmarks() {
        let dpi = 72.0_f32;
        let mut engine = TextEngine::new();
        let mut data = SfxData {
            text: "BAM!".to_owned(),
            size_pt: 220.0,
            origin: [60.0, 60.0],
            ..SfxData::default()
        };
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_bristles = 5;
        data.line_effect.shtack_overshoot_px = 20.0;
        data.line_effect.halo_width_px = 8.0;

        // 1. Full-quality render (halo = 8 px).
        let t = Instant::now();
        let _ = render_sfx_with_options(&data, &mut engine, PixelFormat::RGBA8, dpi, false);
        let full_wall = t.elapsed().as_secs_f64() * 1000.0;
        let full = LAST_SFX_TIMINGS.with(|t| *t.borrow());
        assert!(!full.cache_hit, "first render must build geometry");

        // 2. Cached halo/colour-only update: only parameters applied AFTER the cached stages.
        let mut colour = data.clone();
        colour.line_effect.halo_width_px = 12.0;
        colour.line_effect.halo_color = Color::rgb(0.9, 0.8, 0.7);
        colour.line_effect.line_color = Color::rgb(0.2, 0.2, 0.2);
        colour.fill = SfxFillSource::Solid { color: Color::rgb(0.1, 0.1, 0.1) };
        colour.vector_shadow.enabled = true;
        assert_eq!(sfx_stage_key(&data, dpi, false), sfx_stage_key(&colour, dpi, false));
        let t = Instant::now();
        let _ = render_sfx_with_options(&colour, &mut engine, PixelFormat::RGBA8, dpi, false);
        let cached_wall = t.elapsed().as_secs_f64() * 1000.0;
        let cached = LAST_SFX_TIMINGS.with(|t| *t.borrow());
        assert!(cached.cache_hit, "halo / colour / shadow drag must hit the stage cache");
        assert_eq!(cached.geom_ms, 0.0, "geometry build must be skipped on cache hit");
        assert_eq!(cached.strokes_ms, 0.0, "polygon rasterization must be skipped on cache hit");

        // 3. Interactive preview drag (small proxy, separate key).
        let t = Instant::now();
        let _ = render_sfx_with_options(&data, &mut engine, PixelFormat::RGBA8, dpi, true);
        let preview_wall = t.elapsed().as_secs_f64() * 1000.0;
        let preview = LAST_SFX_TIMINGS.with(|t| *t.borrow());

        // 4. Halo = 0: no dilation whatsoever.
        let mut no_halo = data.clone();
        no_halo.line_effect.halo_width_px = 0.0;
        let t = Instant::now();
        let _ = render_sfx_with_options(&no_halo, &mut engine, PixelFormat::RGBA8, dpi, false);
        let no_halo_wall = t.elapsed().as_secs_f64() * 1000.0;
        let zero = LAST_SFX_TIMINGS.with(|t| *t.borrow());
        assert_eq!(zero.halo_ms, 0.0, "halo_width_px == 0 must run zero EDT dilation");

        println!(
            "[SFX BENCH 220pt (729x346)] full={full_wall:.2}ms (geom={:.2} strokes={:.2} dry={:.2} halo={:.2}) | preview={preview_wall:.2}ms | cached={cached_wall:.2}ms (hit={}) | halo0={no_halo_wall:.2}ms",
            full.geom_ms, full.strokes_ms, full.dry_ms, full.halo_ms, cached.cache_hit
        );
        assert!(preview.total_ms.is_finite());
        assert!(full_wall.is_finite() && preview_wall.is_finite() && cached_wall.is_finite() && no_halo_wall.is_finite());

        // 5. Large ~1920px-wide BAM! (size_pt = 540) matching a full-canvas SFX banner.
        let mut big = data.clone();
        big.size_pt = 540.0;
        big.line_effect.shtack_variant = SfxShtackVariant::DryPencil;
        let t = Instant::now();
        let big_out = render_sfx_with_options(&big, &mut engine, PixelFormat::RGBA8, dpi, false);
        let big_full_wall = t.elapsed().as_secs_f64() * 1000.0;
        let big_full = LAST_SFX_TIMINGS.with(|t| *t.borrow());
        let big_b = big_out.bounds();

        let t = Instant::now();
        let _ = render_sfx_with_options(&big, &mut engine, PixelFormat::RGBA8, dpi, true);
        let big_preview_wall = t.elapsed().as_secs_f64() * 1000.0;
        let big_preview = LAST_SFX_TIMINGS.with(|t| *t.borrow());

        // Re-render full first so cache holds the full-res stage, then measure cached halo/color change on ~1920px BAM!
        let _ = render_sfx_with_options(&big, &mut engine, PixelFormat::RGBA8, dpi, false);
        let mut big_cached_data = big.clone();
        big_cached_data.line_effect.halo_width_px = 14.0;
        let t = Instant::now();
        let _ = render_sfx_with_options(&big_cached_data, &mut engine, PixelFormat::RGBA8, dpi, false);
        let big_cached_wall = t.elapsed().as_secs_f64() * 1000.0;
        let big_cached = LAST_SFX_TIMINGS.with(|t| *t.borrow());
        assert!(big_cached.cache_hit);

        let mut big_halo0 = big.clone();
        big_halo0.line_effect.halo_width_px = 0.0;
        let t = Instant::now();
        let _ = render_sfx_with_options(&big_halo0, &mut engine, PixelFormat::RGBA8, dpi, false);
        let big_halo0_wall = t.elapsed().as_secs_f64() * 1000.0;

        println!(
            "[SFX BENCH 540pt ({}x{}) DryPencil] full={big_full_wall:.2}ms (geom={:.2} strokes={:.2} dry={:.2} halo={:.2}) | preview={big_preview_wall:.2}ms (strokes={:.2} halo={:.2}) | cached={big_cached_wall:.2}ms (hit={}) | halo0={big_halo0_wall:.2}ms",
            big_b.width(),
            big_b.height(),
            big_full.geom_ms,
            big_full.strokes_ms,
            big_full.dry_ms,
            big_full.halo_ms,
            big_preview.strokes_ms,
            big_preview.halo_ms,
            big_cached.cache_hit
        );
    }

    // --- Task 1.3 / 2 / 3: percentage thickness, winding, curvilinear dry brush, sharpness ---

    /// Thickness is now a PERCENTAGE of glyph height. At the validated minimums (0.5%, 1.5%)
    /// the core nib must stay >= 0.65 half-width, so the exclamation mark renders continuous ink.
    #[test]
    fn shtack_min_percent_thickness_keeps_continuous_ink() {
        let mut engine = TextEngine::new();
        let mut counts = Vec::new();
        for pct in [0.5_f32, 1.5_f32] {
            let mut data = SfxData {
                text: "!".to_owned(),
                size_pt: 220.0,
                origin: [60.0, 60.0],
                fill: SfxFillSource::None,
                ..SfxData::default()
            };
            data.line_effect.style = SfxLineStyle::Shtack;
            data.line_effect.halo_width_px = 0.0;
            data.line_effect.shtack_dryness_pct = 0.0;
            data.line_effect.shtack_overshoot_px = 0.0;
            data.line_effect.shtack_thickness_px = pct;
            let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
            let bounds = out.bounds();
            let glyph = out.geom().glyphs.first().map(|g| g.warped_bounds).unwrap_or([0.0; 4]);
            let SfxRenderOutput::Single { surface, .. } = out else {
                panic!("expected Single output at {pct}% thickness");
            };
            let (dark, _, finite) = shtack_ink_stats(&surface, bounds);
            assert!(finite && dark > 0, "{pct}%: no finite dark ink");
            // Dot region (lower 30% of `!`) must contain continuous ring ink.
            let x0 = (glyph[0].floor() as i32 - 4).max(bounds.x0);
            let x1 = (glyph[2].ceil() as i32 + 4).min(bounds.x1);
            let y0 = (glyph[1] + (glyph[3] - glyph[1]) * 0.72).floor() as i32;
            let y1 = (glyph[3].ceil() as i32 + 4).min(bounds.y1);
            let mut dot_dark = 0usize;
            for yy in y0.max(bounds.y0)..y1 {
                for xx in x0..x1 {
                    if surface.sample_channel(xx, yy, 3) > 0.5 && surface.sample_channel(xx, yy, 0) < 0.5 {
                        dot_dark += 1;
                    }
                }
            }
            assert!(dot_dark >= 60, "{pct}%: exclamation dot ring must keep continuous ink, got {dot_dark}");
            // No giant blobs: the ink must remain a small fraction of the glyph box.
            let area = ((glyph[2] - glyph[0]) * (glyph[3] - glyph[1])).max(1.0);
            assert!((dark as f64) < area * 0.6, "{pct}%: stroke became a giant blob ({dark} px)");
            counts.push(dark);
        }
        assert!(counts[1] >= counts[0], "1.5% must not be lighter than 0.5%: {counts:?}");
    }

    /// Regression for the white star cut into compact dots when filled, and hollow ring parity
    /// when `shtack_fill_interior == false`.
    #[test]
    fn shtack_exclamation_dot_center_has_no_winding_hole() {
        for variant in [SfxShtackVariant::SaiPenSketch, SfxShtackVariant::DryPencil] {
            let mut engine = TextEngine::new();
            // 1. Filled interior: center of the `!` dot must be solid dark with zero winding hole.
            let mut data = SfxData {
                text: "!".to_owned(),
                size_pt: 220.0,
                origin: [60.0, 60.0],
                ..SfxData::default()
            };
            data.line_effect.style = SfxLineStyle::Shtack;
            data.line_effect.shtack_variant = variant;
            data.line_effect.halo_width_px = 8.0;
            data.line_effect.shtack_dryness_pct = 0.0;
            data.line_effect.shtack_fill_interior = true;
            let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
            let glyph = out.geom().glyphs.first().map(|g| g.warped_bounds).unwrap_or([0.0; 4]);
            let SfxRenderOutput::Single { surface, .. } = out else {
                panic!("expected Single output for {variant:?}");
            };
            let cx = ((glyph[0] + glyph[2]) * 0.5).round() as i32;
            let cy = (glyph[1] + (glyph[3] - glyph[1]) * 0.86).round() as i32;
            let mut dark = 0usize;
            for yy in (cy - 2)..=(cy + 2) {
                for xx in (cx - 2)..=(cx + 2) {
                    if surface.sample_channel(xx, yy, 3) > 0.5 && surface.sample_channel(xx, yy, 0) < 0.5 {
                        dark += 1;
                    }
                }
            }
            assert!(dark >= 20, "{variant:?}: filled dot center must be solid, got {dark}/25 dark pixels");

            // 2. Hollow mode: the dot stays a hollow sketch ring matching the hollow lettering.
            let mut hollow = data;
            hollow.fill = SfxFillSource::None;
            hollow.line_effect.halo_width_px = 0.0;
            let out_h = render_sfx(&hollow, &mut engine, PixelFormat::RGBA8, 72.0);
            let SfxRenderOutput::Single { surface: surf_h, .. } = out_h else {
                panic!("expected Single output for hollow {variant:?}");
            };
            assert!(
                surf_h.sample_channel(cx, cy, 3) < 0.25,
                "{variant:?}: hollow dot center should stay open, got alpha={}",
                surf_h.sample_channel(cx, cy, 3)
            );
        }
    }

    /// `OuterOnly` suppresses interior/concave corner needles while `All` emits them everywhere.
    #[test]
    fn shtack_overshoot_mode_outer_only_keeps_interior_clean() {
        let mut engine = TextEngine::new();
        let mut outer = SfxData {
            text: "BAM!".to_owned(),
            size_pt: 220.0,
            origin: [60.0, 60.0],
            fill: SfxFillSource::None,
            ..SfxData::default()
        };
        outer.line_effect.style = SfxLineStyle::Shtack;
        outer.line_effect.halo_width_px = 0.0;
        outer.line_effect.shtack_dryness_pct = 0.0;
        outer.line_effect.shtack_overshoot_px = 24.0;
        outer.line_effect.shtack_overshoot_mode = SfxShtackOvershootMode::OuterOnly;

        let mut all = outer.clone();
        all.line_effect.shtack_overshoot_mode = SfxShtackOvershootMode::All;

        assert_ne!(sfx_stage_key(&outer, 72.0, false), sfx_stage_key(&all, 72.0, false));

        let out_outer = render_sfx(&outer, &mut engine, PixelFormat::RGBA8, 72.0);
        let out_all = render_sfx(&all, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_outer = out_outer.bounds();
        let b_all = out_all.bounds();
        let (SfxRenderOutput::Single { surface: s_outer, .. }, SfxRenderOutput::Single { surface: s_all, .. }) = (out_outer, out_all) else {
            panic!("expected Single outputs");
        };
        let (dark_outer, _, fin_o) = shtack_ink_stats(&s_outer, b_outer);
        let (dark_all, _, fin_a) = shtack_ink_stats(&s_all, b_all);
        assert!(fin_o && fin_a && dark_outer > 0);
        assert!(
            dark_outer < dark_all,
            "OuterOnly must omit interior counter/concave needles: outer={dark_outer} vs all={dark_all}"
        );
    }

    /// Dry Brush Seg detects junctions on `B` (crossbar / cusp) and zero junctions on smooth `O`.
    #[test]
    fn dry_seg_junction_cover_targets_b_and_leaves_o_untouched() {
        let mut engine = TextEngine::new();
        for (txt, expect_junctions) in [("B", true), ("O", false)] {
            let mut data = SfxData {
                text: txt.to_owned(),
                size_pt: 240.0,
                origin: [40.0, 40.0],
                ..SfxData::default()
            };
            data.line_effect.halo_width_px = 0.0;
            let geom = build_sfx_geometry(&data, &mut engine, 72.0);
            let bounds = compute_render_bounds(&data, &geom);
            let (w, h) = (bounds.width() as usize, bounds.height() as usize);
            let cov = fill_rasterizer_from_polys(&geom.polylines, [0.0, 0.0]).render(bounds);
            let sdf = compute_mask_sdf(&cov, w, h);
            let contours = build_seg_contours(&geom);
            let juncs = build_seg_junctions(&contours, &cov, &sdf, bounds, w, h);
            if expect_junctions {
                assert!(!juncs.is_empty(), "glyph '{txt}' must detect T-junction / cusp corners");
            } else {
                assert!(juncs.is_empty(), "smooth glyph '{txt}' should have 0 junctions, got {}", juncs.len());
            }
        }
    }

    /// The Seg engine must sample a smooth round contour in continuous arc length instead of
    /// snapping to RDP chords (which produced blocky Voronoi seams on `O` / `B`).
    #[test]
    fn dry_seg_curvilinear_sampling_is_continuous_on_round_glyphs() {
        let mut geom = PreparedSfx::default();
        let (ccx, ccy, radius, n) = (200.0_f64, 200.0_f64, 90.0_f64, 96usize);
        let mut pts = Vec::with_capacity(n + 1);
        for i in 0..n {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            pts.push((ccx + radius * a.cos(), ccy + radius * a.sin()));
        }
        pts.push(pts[0]);
        geom.polylines.push(Polyline { pts, knot: Vec::new(), closed: true });
        let contours = build_seg_contours(&geom);
        assert_eq!(contours.len(), 1);
        let cum = &contours[0].cum;
        let total = cum[cum.len() - 1];
        let mut last_s: Option<f64> = None;
        let mut max_step = 0.0_f64;
        for i in 0..n {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            let px = ccx + radius * a.cos();
            let py = ccy + radius * a.sin();
            let (s, d2) = closest_on_seg_contour(&contours[0], px, py, f64::INFINITY).unwrap_or((f64::NAN, f64::NAN));
            assert!(s.is_finite() && d2.is_finite(), "non-finite curvilinear sample at {i}");
            assert!(d2.sqrt() < 0.01, "a probe on the contour must map to ~0 distance");
            if let Some(prev) = last_s {
                let jump = (s - prev).abs();
                max_step = max_step.max(jump.min((total - jump).abs())); // unwrap the seam
            }
            last_s = Some(s);
        }
        let ideal = total / n as f64;
        assert!(max_step < ideal * 1.5, "chord jump {max_step} exceeds smooth step {ideal}");

        // Full-render smoke: Dry: Seg on a round glyph stays finite and keeps ink.
        let mut engine = TextEngine::new();
        let mut data = SfxData {
            text: "O".to_owned(),
            size_pt: 180.0,
            origin: [60.0, 60.0],
            ..SfxData::default()
        };
        data.line_effect.halo_width_px = 0.0;
        data.dry_brush.enabled = true;
        data.dry_brush.engine = SfxDryBrushEngine::SdfSeg;
        data.dry_brush.dryness_pct = 60.0;
        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        let bounds = out.bounds();
        let SfxRenderOutput::Single { surface, .. } = out else {
            panic!("expected Single output for Seg dry brush");
        };
        let (dark, _, finite) = shtack_ink_stats(&surface, bounds);
        assert!(finite && dark > 0, "Seg dry brush on 'O' must stay finite and keep ink");
    }

    /// `grain_sharpness_pct == 100` must remove the old 0.08 density floor and punch exact 0.0
    /// gaps in both the Dry Pencil (Shtack) path and the dry-brush engines.
    #[test]
    fn sharpness_100_punches_exact_zero_gaps() {
        let mut engine = TextEngine::new();
        let mut data = SfxData {
            text: "BAM!".to_owned(),
            size_pt: 180.0,
            origin: [60.0, 60.0],
            fill: SfxFillSource::None,
            ..SfxData::default()
        };
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_variant = SfxShtackVariant::DryPencil;
        data.line_effect.halo_width_px = 0.0;
        data.line_effect.shtack_dryness_pct = 100.0;
        data.line_effect.shtack_grain_sharpness_pct = 100.0;
        let out = render_sfx(&data, &mut engine, PixelFormat::RGBA8, 72.0);
        let glyph = out.geom().glyphs.last().map(|g| g.warped_bounds).unwrap_or([0.0; 4]);
        let bounds = out.bounds();
        let SfxRenderOutput::Single { surface, .. } = out else {
            panic!("expected Single output for Dry Pencil");
        };
        let x0 = glyph[0].floor() as i32;
        let x1 = glyph[2].ceil() as i32;
        let y0 = (glyph[1] + (glyph[3] - glyph[1]) * 0.72).floor() as i32;
        let y1 = glyph[3].ceil() as i32;
        let (mut zero, mut ink) = (0usize, 0usize);
        for y in y0.max(bounds.y0)..y1.min(bounds.y1) {
            for x in x0.max(bounds.x0)..x1.min(bounds.x1) {
                let a = surface.sample_channel(x, y, 3);
                if a == 0.0 {
                    zero += 1;
                }
                if a > 0.5 {
                    ink += 1;
                }
            }
        }
        assert!(zero > 0 && ink > 0, "100% sharp DryPencil: true 0.0 gaps required (zero={zero}, ink={ink})");
        // Sharpness participates in the cached stroke key, so a change invalidates coverage.
        let mut other = data.clone();
        other.line_effect.shtack_grain_sharpness_pct = 50.0;
        assert_ne!(sfx_stage_key(&data, 72.0, false), sfx_stage_key(&other, 72.0, false));

        let mut db = SfxDryBrushSpec {
            enabled: true,
            engine: SfxDryBrushEngine::SdfPhysics,
            dryness_pct: 55.0,
            paper_grain_pct: 100.0,
            grain_sharpness_pct: 100.0,
            ..SfxDryBrushSpec::default()
        };
        let box2 = Rect::from_xywh(0, 0, 64, 64);
        let mut cov = vec![0.9_f32; 64 * 64];
        apply_dry_brush_sdf_physics(&mut cov, box2, 64, 64, &db, false);
        let z = cov.iter().filter(|&&v| v == 0.0).count();
        let i = cov.iter().filter(|&&v| v > 0.5).count();
        assert!(z > 0 && i > 0, "100% sharp dry brush: true 0.0 gaps required (zero={z}, ink={i})");

        db.grain_sharpness_pct = 0.0;
        let mut cov_soft = vec![0.9_f32; 64 * 64];
        apply_dry_brush_sdf_physics(&mut cov_soft, box2, 64, 64, &db, false);
        assert!(cov_soft.iter().any(|&v| v > 0.0 && v < 0.9), "0% sharpness must keep a soft ramp");
    }

    /// Independent interior `Shtack Fill` fills the glyph interior with hatch strokes even when
    /// the outline is completely disabled (`Clean 0px`), and `cross_hatch` adds crossing density.
    #[test]
    fn shtack_fill_renders_interior_hatching_independently_of_outline() {
        let mut engine = TextEngine::new();
        let mut single = SfxData {
            text: "BO".to_owned(),
            size_pt: 200.0,
            origin: [40.0, 40.0],
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    cross_hatch: false,
                    density_pct: 60.0,
                    thickness_pct: 1.5,
                    overshoot_pct: 0.0,
                    dryness_pct: 0.0,
                    ..SfxShtackFillSpec::default()
                },
            },
            ..SfxData::default()
        };
        single.line_effect.style = SfxLineStyle::Clean;
        single.line_effect.line_width_px = 0.0;
        single.line_effect.halo_width_px = 0.0;

        let out_s = render_sfx(&single, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_s = out_s.bounds();
        let SfxRenderOutput::Single { surface: surf_s, .. } = out_s else {
            panic!("expected Single output for Shtack Fill");
        };
        let (dark_s, _, fin_s) = shtack_ink_stats(&surf_s, b_s);
        assert!(fin_s && dark_s > 200, "Shtack Fill without outline must render rich hatching, got {dark_s}");

        let mut crossed = single.clone();
        if let SfxFillSource::ShtackHatch { spec } = &mut crossed.fill {
            spec.cross_hatch = true;
        }
        let out_c = render_sfx(&crossed, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_c = out_c.bounds();
        let SfxRenderOutput::Single { surface: surf_c, .. } = out_c else {
            panic!("expected Single output for cross-hatch Shtack Fill");
        };
        let (dark_c, _, fin_c) = shtack_ink_stats(&surf_c, b_c);
        assert!(fin_c && dark_c > dark_s, "cross_hatch must add ink coverage: {dark_c} vs {dark_s}");
    }

    /// `SfxBlending` supports mixing a secondary fill (`Knockout` punches alpha out of Fill 1),
    /// and `line_behind_fill` flips the stacking order of Fill vs Line.
    #[test]
    fn sfx_blending_secondary_fill_knockout_and_line_order() {
        let mut engine = TextEngine::new();
        let mut base = SfxData {
            text: "B".to_owned(),
            size_pt: 200.0,
            origin: [40.0, 40.0],
            fill: SfxFillSource::Solid { color: Color::rgb(0.1, 0.1, 0.1) },
            ..SfxData::default()
        };
        base.line_effect.style = SfxLineStyle::Clean;
        base.line_effect.line_width_px = 0.0;
        base.line_effect.halo_width_px = 0.0;

        let out_solid = render_sfx(&base, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_solid = out_solid.bounds();
        let SfxRenderOutput::Single { surface: s_solid, .. } = out_solid else {
            panic!("expected Single output");
        };
        let (dark_solid, _, _) = shtack_ink_stats(&s_solid, b_solid);

        let mut knocked = base.clone();
        knocked.blending.secondary_fill = Some(SfxFillSource::ShtackHatch {
            spec: SfxShtackFillSpec { density_pct: 65.0, thickness_pct: 2.0, overshoot_pct: 0.0, dryness_pct: 0.0, ..SfxShtackFillSpec::default() },
        });
        knocked.blending.secondary_blend = SfxChannelBlend::Knockout;
        let out_ko = render_sfx(&knocked, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_ko = out_ko.bounds();
        let SfxRenderOutput::Single { surface: s_ko, .. } = out_ko else {
            panic!("expected Single output");
        };
        let (dark_ko, _, fin_ko) = shtack_ink_stats(&s_ko, b_ko);
        assert!(fin_ko && dark_ko > 0 && dark_ko < dark_solid, "Knockout ShtackHatch must carve alpha from Solid fill: {dark_ko} vs {dark_solid}");

        // Line behind fill vs line above fill with contrasting colours must differ visibly.
        let mut over = base;
        over.fill = SfxFillSource::Solid { color: Color::rgb(1.0, 0.0, 0.0) };
        over.line_effect.line_width_px = 14.0;
        over.line_effect.line_color = Color::rgb(0.0, 0.0, 1.0);
        over.blending.line_behind_fill = false;
        let mut under = over.clone();
        under.blending.line_behind_fill = true;

        let SfxRenderOutput::Single { surface: s_over, bounds: bo, .. } = render_sfx(&over, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        let SfxRenderOutput::Single { surface: s_under, bounds: bu, .. } = render_sfx(&under, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        let red_px = |s: &Surface, b: Rect| -> usize {
            let mut n = 0;
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if s.sample_channel(x, y, 3) > 0.5 && s.sample_channel(x, y, 0) > 0.7 && s.sample_channel(x, y, 2) < 0.3 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(
            red_px(&s_under, bu) > red_px(&s_over, bo),
            "line_behind_fill=true must reveal more red interior fill ({} vs {})",
            red_px(&s_under, bu),
            red_px(&s_over, bo)
        );
    }

    /// `SfxShtackFillMode::StrokeFlow` extracts clean medial-axis strokes along stems/bowls
    /// (zero outline required), extends needles past the glyph silhouette when `overshoot_pct > 0`,
    /// preserves closed loops (`O`) and compact dots (`!`), and scales stripe density with `density_pct`.
    #[test]
    fn shtack_fill_stroke_flow_renders_clean_stems_and_overshoot_needles() {
        let mut engine = TextEngine::new();
        let make = |text: &str, density_pct: f32, overshoot_pct: f32| {
            let mut d = SfxData {
                text: text.to_owned(),
                size_pt: 200.0,
                origin: [40.0, 40.0],
                fill: SfxFillSource::ShtackHatch {
                    spec: SfxShtackFillSpec {
                        mode: SfxShtackFillMode::StrokeFlow,
                        density_pct,
                        thickness_pct: 1.5,
                        roughness_pct: 48.0,
                        overshoot_pct,
                        dryness_pct: 0.0,
                        ..SfxShtackFillSpec::default()
                    },
                },
                ..SfxData::default()
            };
            d.line_effect.style = SfxLineStyle::Clean;
            d.line_effect.line_width_px = 0.0;
            d.line_effect.halo_width_px = 0.0;
            d
        };

        let d_clipped = make("BAM!", 55.0, 0.0);
        let out_clip = render_sfx(&d_clipped, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_clip = out_clip.bounds();
        let SfxRenderOutput::Single { surface: s_clip, geom: g_clip, .. } = out_clip else {
            panic!("expected Single output");
        };
        let (dark_clip, _, fin_clip) = shtack_ink_stats(&s_clip, b_clip);
        assert!(fin_clip && dark_clip > 2000, "StrokeFlow must paint substantial ink for BAM! ({dark_clip})");

        let d_needles = make("BAM!", 85.0, 24.0);
        let out_n = render_sfx(&d_needles, &mut engine, PixelFormat::RGBA8, 72.0);
        let b_n = out_n.bounds();
        let SfxRenderOutput::Single { surface: s_n, .. } = out_n else {
            panic!("expected Single output");
        };
        let (dark_n, _, fin_n) = shtack_ink_stats(&s_n, b_n);
        assert!(fin_n && dark_n > dark_clip, "higher density + overshoot must add ink ({dark_n} vs {dark_clip})");

        // Verify needles actually extend beyond the base glyph bounding box.
        let mut outside_px = 0usize;
        for y in b_n.y0..b_n.y1 {
            for x in b_n.x0..b_n.x1 {
                let outside = (x as f64) < g_clip.warped_box[0] - 4.0
                    || (x as f64) > g_clip.warped_box[2] + 4.0
                    || (y as f64) < g_clip.warped_box[1] - 4.0
                    || (y as f64) > g_clip.warped_box[3] + 4.0;
                if outside && s_n.sample_channel(x, y, 3) > 0.35 {
                    outside_px += 1;
                }
            }
        }
        assert!(outside_px > 40, "StrokeFlow overshoot needles must protrude past glyph box ({outside_px})");

        // Verify closed loop `O` renders cleanly with hollow interior center.
        let d_o = make("O", 65.0, 14.0);
        let SfxRenderOutput::Single { surface: s_o, bounds: b_o, geom: g_o } = render_sfx(&d_o, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single output");
        };
        let (dark_o, _, fin_o) = shtack_ink_stats(&s_o, b_o);
        assert!(fin_o && dark_o > 800, "StrokeFlow must render closed loop O ({dark_o})");
        let cx = ((g_o.warped_box[0] + g_o.warped_box[2]) * 0.5).round() as i32;
        let cy = ((g_o.warped_box[1] + g_o.warped_box[3]) * 0.5).round() as i32;
        assert!(s_o.sample_channel(cx, cy, 3) < 0.1, "center of O must remain hollow");
    }

    /// `SfxShtackFillMode::UkiyoBrush` produces rich wet sumi-e ink without SAI cross-over needles,
    /// and `SfxShtackFlowOvershoot::FreeEnds` emits fewer exterior junction needles than `OuterJunctions`.
    #[test]
    fn ukiyo_brush_and_flow_overshoot_modes_render_cleanly() {
        let mut engine = TextEngine::new();
        let make = |mode: SfxShtackFillMode, ov: SfxShtackFlowOvershoot| {
            let mut d = SfxData {
                text: "BAM!".to_owned(),
                size_pt: 200.0,
                origin: [40.0, 40.0],
                fill: SfxFillSource::ShtackHatch {
                    spec: SfxShtackFillSpec {
                        mode,
                        flow_overshoot: ov,
                        density_pct: 64.0,
                        thickness_pct: 1.5,
                        roughness_pct: 52.0,
                        overshoot_pct: 20.0,
                        dryness_pct: if mode == SfxShtackFillMode::UkiyoBrush { 32.0 } else { 0.0 },
                        ..SfxShtackFillSpec::default()
                    },
                },
                ..SfxData::default()
            };
            d.line_effect.style = SfxLineStyle::Clean;
            d.line_effect.line_width_px = 0.0;
            d.line_effect.halo_width_px = 0.0;
            d
        };

        let SfxRenderOutput::Single { surface: s_free, bounds: b_free, geom: g_free } =
            render_sfx(&make(SfxShtackFillMode::StrokeFlow, SfxShtackFlowOvershoot::FreeEnds), &mut engine, PixelFormat::RGBA8, 72.0)
        else {
            panic!("expected Single");
        };
        let SfxRenderOutput::Single { surface: s_junc, bounds: b_junc, .. } =
            render_sfx(&make(SfxShtackFillMode::StrokeFlow, SfxShtackFlowOvershoot::OuterJunctions), &mut engine, PixelFormat::RGBA8, 72.0)
        else {
            panic!("expected Single");
        };
        let left_outside = |surf: &Surface, b: Rect| -> usize {
            let mut n = 0;
            let limit = g_free.warped_box[0] - 6.0;
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if (x as f64) < limit && surf.sample_channel(x, y, 3) > 0.35 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(
            left_outside(&s_junc, b_junc) > left_outside(&s_free, b_free),
            "OuterJunctions must shoot needles past B's left stem while FreeEnds keeps junctions clean ({} vs {})",
            left_outside(&s_junc, b_junc),
            left_outside(&s_free, b_free)
        );

        let SfxRenderOutput::Single { surface: s_ukiyo, bounds: b_ukiyo, .. } =
            render_sfx(&make(SfxShtackFillMode::UkiyoBrush, SfxShtackFlowOvershoot::FreeEnds), &mut engine, PixelFormat::RGBA8, 72.0)
        else {
            panic!("expected Single");
        };
        let (dark_u, _, fin_u) = shtack_ink_stats(&s_ukiyo, b_ukiyo);
        assert!(fin_u && dark_u > 3000, "UkiyoBrush must render rich wet sumi-e ink ({dark_u})");
    }

    /// Disabling all left-pointing `SfxFlowNeedleHandle`s via `flow_overrides` removes the
    /// left-side needles from `B` while preserving the rest of the lettering.
    #[test]
    fn flow_needle_overrides_suppress_exact_endpoint_needles_in_render() {
        let mut engine = TextEngine::new();
        let mut d = SfxData {
            text: "B".to_owned(),
            size_pt: 220.0,
            origin: [60.0, 60.0],
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    mode: SfxShtackFillMode::StrokeFlow,
                    flow_overshoot: SfxShtackFlowOvershoot::OuterJunctions,
                    overshoot_pct: 24.0,
                    dryness_pct: 0.0,
                    ..SfxShtackFillSpec::default()
                },
            },
            ..SfxData::default()
        };
        d.line_effect.style = SfxLineStyle::Clean;
        d.line_effect.line_width_px = 0.0;
        d.line_effect.halo_width_px = 0.0;

        let SfxRenderOutput::Single { surface: s_on, bounds: b_on, geom } = render_sfx(&d, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        let SfxFillSource::ShtackHatch { spec } = &d.fill else { panic!("expected ShtackHatch") };
        let handles = extract_sfx_flow_handles(&geom, spec);
        let left_handles: Vec<_> = handles.iter().filter(|h| h.enabled && h.dir[0] < -0.5).copied().collect();
        assert!(!left_handles.is_empty(), "B in OuterJunctions must expose enabled left-shooting handles");

        let mut d_off = d.clone();
        if let SfxFillSource::ShtackHatch { spec: sp } = &mut d_off.fill {
            for h in &left_handles {
                sp.toggle_flow_override(h.glyph_index, h.uv, [h.dir[0] as f32, h.dir[1] as f32], h.default_enabled);
            }
        }
        let SfxRenderOutput::Single { surface: s_off, bounds: b_off, .. } = render_sfx(&d_off, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        let left_ink = |surf: &Surface, b: Rect| -> usize {
            let mut n = 0;
            let limit = geom.warped_box[0] - 8.0;
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if (x as f64) < limit && surf.sample_channel(x, y, 3) > 0.35 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(left_ink(&s_on, b_on) > 20, "baseline B must have left-protruding needles");
        assert_eq!(left_ink(&s_off, b_off), 0, "disabling all left handles must eliminate left needles completely");
    }

    /// Steering a flow needle's angle and length via `set_flow_override_vector` physically rotates
    /// and extends the rendered needle in `StrokeFlow` mode without clipping its bounds.
    #[test]
    fn flow_needle_angle_and_length_override_steers_rendered_needle() {
        let mut engine = TextEngine::new();
        let mut base = SfxData {
            text: "B".to_owned(),
            size_pt: 220.0,
            origin: [80.0, 80.0],
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    mode: SfxShtackFillMode::StrokeFlow,
                    flow_overshoot: SfxShtackFlowOvershoot::OuterJunctions,
                    overshoot_pct: 20.0,
                    dryness_pct: 0.0,
                    ..SfxShtackFillSpec::default()
                },
            },
            ..SfxData::default()
        };
        base.line_effect.style = SfxLineStyle::Clean;
        base.line_effect.line_width_px = 0.0;
        base.line_effect.halo_width_px = 0.0;

        let SfxRenderOutput::Single { surface: s_base, bounds: b_base, geom } = render_sfx(&base, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        let SfxFillSource::ShtackHatch { spec } = &base.fill else { panic!("expected ShtackHatch") };
        let handles = extract_sfx_flow_handles(&geom, spec);
        let top_left = handles
            .iter()
            .filter(|h| h.enabled && h.base_dir[0] < -0.5)
            .min_by(|a, b| a.end_doc[1].total_cmp(&b.end_doc[1]))
            .copied()
            .expect("expected an enabled left-pointing handle on B");

        let mut steered = base.clone();
        if let SfxFillSource::ShtackHatch { spec: sp } = &mut steered.fill {
            sp.set_flow_override_vector(
                top_left.glyph_index,
                top_left.uv,
                [top_left.base_dir[0] as f32, top_left.base_dir[1] as f32],
                45.0,
                2.4,
            );
        }
        let SfxRenderOutput::Single { surface: s_steered, bounds: b_steered, .. } = render_sfx(&steered, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        assert!(b_steered.width() > b_base.width(), "2.4x needle length_scale must expand render bounds");
        let far_left_ink = |surf: &Surface, b: Rect| -> usize {
            let mut n = 0;
            let limit = geom.warped_box[0] - 42.0;
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if (x as f64) < limit && surf.sample_channel(x, y, 3) > 0.35 {
                        n += 1;
                    }
                }
            }
            n
        };
        assert!(
            far_left_ink(&s_steered, b_steered) > far_left_ink(&s_base, b_base),
            "steered 2.4x needle must push ink much farther left than baseline"
        );
    }

    /// Photoshop-style Manga Blending FX (`Bevel`, `Stroke`, `InnerShadow`, `FillOverlay(DryBrush)`,
    /// `OuterGlow`, `DropShadow`) expand bounds appropriately and visibly paint the SFX surface.
    #[test]
    fn blending_effects_bevel_inner_shadow_outer_glow_and_dry_overlay_render() {
        let mut engine = TextEngine::new();
        let mut plain = SfxData {
            text: "A".to_owned(),
            size_pt: 180.0,
            origin: [60.0, 60.0],
            fill: SfxFillSource::Solid { color: Color::rgb(0.85, 0.2, 0.2) },
            ..SfxData::default()
        };
        plain.line_effect.style = SfxLineStyle::Clean;
        plain.line_effect.line_width_px = 0.0;
        plain.line_effect.halo_width_px = 0.0;

        let SfxRenderOutput::Single { surface: s_plain, bounds: b_plain, .. } = render_sfx(&plain, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };

        let mut fx_data = plain.clone();
        fx_data.blending.effects = vec![
            SfxBlendEffect::Bevel {
                enabled: true,
                depth_px: 8.0,
                angle_deg: 120.0,
                highlight_color: Color::WHITE,
                highlight_opacity: 0.9,
                shadow_color: Color::BLACK,
                shadow_opacity: 0.9,
            },
            SfxBlendEffect::Stroke {
                enabled: true,
                width_px: 6.0,
                position: SfxStrokePosition::Outside,
                join: SfxLineJoin::Round,
                fill: SfxFillSource::Solid { color: Color::rgb(0.05, 0.05, 0.85) },
                blend: SfxChannelBlend::Normal,
                opacity: 1.0,
            },
            SfxBlendEffect::FillOverlay {
                enabled: true,
                is_pattern: true,
                fill: SfxFillSource::DryBrush {
                    color: Color::BLACK,
                    spec: SfxDryBrushSpec { enabled: true, dryness_pct: 45.0, ..SfxDryBrushSpec::default() },
                },
                blend: SfxChannelBlend::Multiply,
                opacity: 0.7,
            },
            SfxBlendEffect::InnerShadow {
                enabled: true,
                angle_deg: 135.0,
                distance_px: 6.0,
                blur_px: 3.0,
                color: Color::BLACK,
                blend: SfxChannelBlend::Multiply,
                opacity: 0.8,
            },
            SfxBlendEffect::OuterGlow {
                enabled: true,
                spread_px: 4.0,
                blur_px: 8.0,
                color: Color::rgb(1.0, 0.95, 0.2),
                blend: SfxChannelBlend::Screen,
                opacity: 0.85,
            },
            SfxBlendEffect::DropShadow {
                enabled: true,
                angle_deg: 45.0,
                distance_px: 14.0,
                extrude_depth_px: 6.0,
                blur_px: 4.0,
                color: Color::BLACK,
                blend: SfxChannelBlend::Multiply,
                opacity: 0.8,
            },
        ];

        let SfxRenderOutput::Single { surface: s_fx, bounds: b_fx, .. } = render_sfx(&fx_data, &mut engine, PixelFormat::RGBA8, 72.0) else {
            panic!("expected Single");
        };
        assert!(b_fx.width() > b_plain.width() + 20 && b_fx.height() > b_plain.height() + 20, "outer FX must expand render bounds");
        let count_opaque = |surf: &Surface, b: Rect| -> usize {
            let mut c = 0;
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    if surf.sample_channel(x, y, 3) > 0.2 {
                        c += 1;
                    }
                }
            }
            c
        };
        assert!(
            count_opaque(&s_fx, b_fx) > count_opaque(&s_plain, b_plain) + 500,
            "Photoshop Manga FX stack must visibly add stroke, glow, and drop shadow ink"
        );
    }
}
