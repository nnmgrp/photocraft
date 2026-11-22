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
    SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineEffect, SfxLineJoin, SfxLineStyle, SfxShtackOvershootMode,
    SfxShtackVariant, SfxSpikeShape, SfxStylePass,
};
use photocraft_doc::{FillRule, LineCap, LineJoin, PathOp};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use photocraft_text::TextEngine;
use photocraft_vector::{Polyline, Rasterizer, StrokeStyle, stroke_polygons};
use web_time::Instant;

use crate::sfx_geom::{PreparedSfx, build_sfx_geometry};

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
fn compute_mask_sdf(cov: &[f32], w: usize, h: usize) -> Vec<f32> {
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

    let mut min_dx: f64 = 0.0;
    let mut max_dx: f64 = 0.0;
    let mut min_dy: f64 = 0.0;
    let mut max_dy: f64 = 0.0;

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
        SfxFillSource::Solid { color } => vec![color_rgba(*color); n],
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
    let keep_solid_core = le.shtack_fill_interior || c_span < 10.0;
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

            // Compact closed contour (`!`, `?`, `i` dots): hollow sketch ring when hollow mode is active.
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

/// Dry Pencil finish: deterministic paper grain + directional dry streaks break up the
/// rendered coverage, strongest at stroke edges and low-coverage tapered ends. Unlike a
/// noise texture pasted on top, this removes density from the ink itself.
fn apply_shtack_dry_breakup(cov: &mut [f32], w: usize, h: usize, le: &SfxLineEffect) {
    let dryness = (f64::from(le.shtack_dryness_pct) / 100.0).clamp(0.0, 1.0);
    if dryness <= 0.001 || w == 0 || h == 0 {
        return;
    }
    // Fast 1-pass 4-neighbourhood edge estimate. The old code ran a full 2-pass Felzenszwalb
    // EDT over EVERY pixel just to find coverage within ~2 px of the stroke boundary; this
    // local check is ~100x faster and skips all empty pixels entirely.
    let sharp = (f64::from(le.shtack_grain_sharpness_pct) / 100.0).clamp(0.0, 1.0);
    let src = cov.to_vec();
    let seed = le.shtack_seed.wrapping_mul(7_919);
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
        // Crisp tooth + a directional dry streak: grain can now fully sever the ink.
        // `ink_score` is higher in the stroke core (`+0.26 * edge`) so paper tooth eats into
        // stroke boundaries first and then speckles through the interior as dryness rises.
        let grain = f64::from(crisp_paper_grain(x, y, seed));
        let streak = f64::from(value_noise_2d(x * 0.12, y * 0.85, seed.wrapping_add(31)));
        let noise = (grain * 0.72 + streak * 0.28).clamp(0.0, 1.0);
        let ink_score = (1.0 - noise) + 0.26 * edge;
        let thresh = dryness * 0.92;
        // No 0.08 safety floor: at 100% sharpness dry gaps reach EXACTLY 0.0. Below that,
        // a steepness ramp transitions smoothly from soft graphite to razor-crisp paper tooth.
        let keep = if sharp >= 0.99 {
            if ink_score >= thresh { 1.0 } else { 0.0 }
        } else {
            let slope = 2.5 + sharp * sharp * 55.0;
            ((ink_score - thresh) * slope + 0.5).clamp(0.0, 1.0)
        };
        *c = (*c as f64 * keep).clamp(0.0, 1.0) as f32;
    }
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

fn apply_dry_brush_vector(base_cov: &mut [f32], geom: &PreparedSfx, bounds: Rect, db: &SfxDryBrushSpec, is_binary: bool) {
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
    k.push("shtack_fill_interior", &le.shtack_fill_interior);
    k.finish()
}

/// Rasterized coverage rect with halo / vector shadow / legacy style padding removed, so it
/// stays identical while those cheap passes are being dragged.
fn cache_bounds_for(data: &SfxData, geom: &PreparedSfx) -> Rect {
    let mut d = data.clone();
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
fn render_halo_pass(le: &SfxLineEffect, face_cov: &[f32], line_cov: Option<&[f32]>, bounds: Rect, under_rgba: &mut [[f32; 4]]) -> f64 {
    if le.halo_width_px <= 0.05 {
        return 0.0;
    }
    let halo_start = Instant::now();
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let mut union_cov = face_cov.to_vec();
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
    paint_solid_over(under_rgba, &halo_cov, color_rgba(le.halo_color), 1.0);
    halo_start.elapsed().as_secs_f64() * 1000.0
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
    let (raw_base_cov, line_or_shtack_cov) = match cached_coverages {
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
    let mut line_cov = line_or_shtack_cov;
    if le.style == SfxLineStyle::OneBit {
        let intensity = le.one_bit_intensity / 100.0;
        for (i, c) in face_cov.iter_mut().enumerate() {
            let px = (bounds.x0 + (i % w) as i32) as f32;
            let py = (bounds.y0 + (i / w) as i32) as f32;
            let jitter = (value_noise_2d(px * 0.45, py * 0.45, 701) - 0.5) * intensity * 0.55;
            *c = if *c + jitter >= 0.5 { 1.0 } else { 0.0 };
        }
    } else if le.style == SfxLineStyle::Shtack && !le.shtack_fill_interior {
        face_cov = line_cov.take().unwrap_or_else(|| vec![0.0_f32; n]);
    }

    // Stage 0: Vector Shadow / 3D + Legacy Shadows / Extrude3D / Stroke Stack
    let mut under_rgba = vec![[0.0_f32; 4]; n];
    render_vector_shadow_pass(data, &geom, bounds, &mut under_rgba);
    render_shadow_passes(data, &geom, bounds, w, h, &mut under_rgba);
    render_extrude_passes(data, &geom, bounds, &mut under_rgba);
    render_stroke_passes(data, &geom, bounds, &mut under_rgba);

    // Stage 1 (Pass 1 — Halo / Ореол behind ALL letters)
    let halo_ms = render_halo_pass(le, &face_cov, line_cov.as_deref(), bounds, &mut under_rgba);

    // Stage 2 (Pass 2 — Face Fill + Dry Brush + Distress)
    apply_grain_distress(data, &mut face_cov, bounds, w);
    if data.dry_brush.enabled {
        let t = Instant::now();
        apply_dry_brush_mask(&mut face_cov, &geom, bounds, w, h, &data.dry_brush, le.style == SfxLineStyle::OneBit);
        dry_ms += t.elapsed().as_secs_f64() * 1000.0;
    }

    let out = if data.has_negative_invert() {
        let mut invert_rgba = vec![[0.0_f32; 4]; n];
        for (o, &c) in invert_rgba.iter_mut().zip(face_cov.iter()) {
            let ca = c.clamp(0.0, 1.0);
            if ca > 1e-6 {
                *o = [1.0, 1.0, 1.0, ca];
            }
        }
        if let Some(lc) = &line_cov {
            paint_solid_over(&mut under_rgba, lc, color_rgba(le.line_color), 1.0);
        }
        let under = finish_sfx_surface(&under_rgba, bounds, full_bounds, proxy_scale, format);
        let invert = finish_sfx_surface(&invert_rgba, bounds, full_bounds, proxy_scale, format);
        SfxRenderOutput::InvertPair { under, invert, bounds: full_bounds, geom: full_geom }
    } else {
        let face_paint = eval_fill_rgba(&data.fill, bounds);
        paint_over(&mut under_rgba, &face_cov, &face_paint);
        render_inner_glow_passes(data, &face_cov, w, h, &mut under_rgba);

        // Stage 3 (Stylized Line / Outline on top of face fill)
        if let Some(lc) = &line_cov {
            paint_solid_over(&mut under_rgba, lc, color_rgba(le.line_color), 1.0);
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
        let mut data = SfxData { text: "BAM!".to_owned(), ..SfxData::default() };
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
            ..SfxData::default()
        };
        data.line_effect.style = SfxLineStyle::Shtack;
        data.line_effect.shtack_variant = variant;
        data.line_effect.shtack_bristles = 5;
        data.line_effect.shtack_overshoot_px = 20.0;
        data.line_effect.shtack_thickness_px = 2.0; // percentage of glyph height (not pixels)
        data.line_effect.halo_width_px = halo;
        data.line_effect.shtack_fill_interior = fill_interior;
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
            hollow.line_effect.halo_width_px = 0.0;
            hollow.line_effect.shtack_fill_interior = false;
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
}
