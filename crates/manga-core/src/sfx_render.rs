//! CPU oracle & offscreen renderer for Manga SFX layers (`L3`, wasm-pure).
//!
//! Full Rust port of the Manga SFX rendering pipeline:
//! - 2-pass Halo-then-Ink compositing (plus Vector Shadow / 3D Extrude & legacy `SfxStylePass` stack)
//! - 6 stylized Line/Outline engines (`Clean`, `OneBit`, curvature-clamped `Spikes`,
//!   `GPenNib` & `BrushStroke` with `#` corner cross-overs, and `Shtack` in
//!   `SaiPenSketch` / `DryPencil` modes via merged Zhang-Suen skeletons, tangent
//!   overshoots and several independent tapered sketch passes per stroke)
//! - 4 Rust-ready Dry Brush engines (`SdfPhysics`, `SdfSeg`, `SdfEtf` + LIC, `Vector`)
//! - Procedural screentone sheets (`photocraft-algo`) and `NegativeInvert` (`BlendMode::Difference`).

use std::f64::consts::PI;

use photocraft_algo::comic_screentone::{ScreentoneMode, render_screentone_to_surface};
use photocraft_color::PixelFormat;
use photocraft_doc::sfx::{
    SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineEffect, SfxLineJoin, SfxLineStyle, SfxShtackVariant, SfxSpikeShape,
    SfxStylePass,
};
use photocraft_doc::{FillRule, LineCap, LineJoin, PathOp};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use photocraft_text::TextEngine;
use photocraft_vector::{Polyline, Rasterizer, StrokeStyle, stroke_polygons};

use crate::sfx_geom::{PreparedSfx, build_sfx_geometry};

const MAX_RENDER_EXTENT: i32 = 8192;
const MAX_RENDER_PIXELS: usize = 16 * 1024 * 1024;
/// Hard safety ceiling for the *returned* document-space rect. The surface stays sparse: only
/// proxy-derived ink rows are written, so a gigapixel layer does not allocate a gigapixel buffer.
const MAX_FULL_RENDER_EXTENT: i32 = 65_536;
const SKELETON_MAX_GRID_DIM: usize = 256;

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
fn ribbon_polygon_from_samples(samples: &[(f64, f64, f64)]) -> Option<Vec<(f64, f64)>> {
    let n = samples.len();
    if n < 2 {
        return None;
    }
    let mut left = Vec::with_capacity(n);
    let mut right = Vec::with_capacity(n);
    for i in 0..n {
        let prev = samples[i.saturating_sub(1)];
        let next = samples[(i + 1).min(n - 1)];
        let tx = next.0 - prev.0;
        let ty = next.1 - prev.1;
        let len = tx.hypot(ty);
        if len < 1e-6 {
            continue;
        }
        let nx = -ty / len;
        let ny = tx / len;
        let hw = samples[i].2.max(0.15);
        left.push((samples[i].0 + nx * hw, samples[i].1 + ny * hw));
        right.push((samples[i].0 - nx * hw, samples[i].1 - ny * hw));
    }
    if left.len() < 2 {
        return None;
    }
    left.extend(right.into_iter().rev());
    Some(left)
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
        SfxLineStyle::Shtack => f64::from(le.shtack_overshoot_px) * 1.6 + f64::from(le.shtack_thickness_px) + 6.0,
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
    let mut r = Rasterizer::new(false);
    r.add_component(polys, PathOp::Combine, FillRule::NonZero);
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
fn proxy_target(bounds: Rect) -> (usize, usize, f64) {
    let tw = bounds.width() as usize;
    let th = bounds.height() as usize;
    let mut scale = 1.0_f64;
    if tw > MAX_RENDER_EXTENT as usize {
        scale = scale.min(MAX_RENDER_EXTENT as f64 / tw as f64);
    }
    if th > MAX_RENDER_EXTENT as usize {
        scale = scale.min(MAX_RENDER_EXTENT as f64 / th as f64);
    }
    if tw.saturating_mul(th) > MAX_RENDER_PIXELS {
        let area_scale = (MAX_RENDER_PIXELS as f64 / (tw.saturating_mul(th) as f64)).sqrt();
        scale = scale.min(area_scale);
    }
    if scale >= 1.0 {
        return (tw, th, 1.0);
    }
    let mut pw = ((tw as f64 * scale).round() as usize).clamp(1, MAX_RENDER_EXTENT as usize);
    let mut ph = ((th as f64 * scale).round() as usize).clamp(1, MAX_RENDER_EXTENT as usize);
    while pw.saturating_mul(ph) > MAX_RENDER_PIXELS {
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
        le.shtack_overshoot_px = px(le.shtack_overshoot_px);
        le.shtack_thickness_px = px(le.shtack_thickness_px);
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
// Both submodes share one geometry engine:
//   * skeleton chains are MERGED across T-junctions when their tangents continue smoothly
//     (so the stem of `B` is one stroke, not a pile of fragments), and split at sharp
//     L-corners so intersecting strokes both pierce the corner;
//   * every resulting stroke overshoots along its end tangent at BOTH ends;
//   * 3..=6 independent tapered passes (2 dense core + expressive side whiskers) create
//     natural sketch air gaps — no parallel rails and no fake white `cut_polys` wedges.
// `SaiPenSketch` keeps long sharp needles; `DryPencil` shortens/roughens them and adds
// deterministic paper-grain breakup in coverage space.

/// A traced skeleton chain in grid space. `node_a`/`node_b` record a junction pixel at the
/// corresponding end (free endpoints and sharp-corner splits use `None`).
#[derive(Clone, Debug)]
struct ShtackChain {
    pts: Vec<(usize, usize)>,
    node_a: Option<(usize, usize)>,
    node_b: Option<(usize, usize)>,
}

/// Connected components (8-neighbourhood) of a binary grid, with per-pixel component ids
/// and integer bounding boxes. Used to keep compact glyph parts (dots on `!`, `i`, `?`,
/// hyphens) from being thinned away.
fn grid_connected_components(mask: &[u8], w: usize, h: usize) -> (Vec<usize>, Vec<GridBox>) {
    let mut ids = vec![usize::MAX; w * h];
    let mut boxes: Vec<GridBox> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let start = y * w + x;
            if mask[start] == 0 || ids[start] != usize::MAX {
                continue;
            }
            let id = boxes.len();
            let mut box_ = (x, y, x, y);
            ids[start] = id;
            stack.push((x, y));
            while let Some((cx, cy)) = stack.pop() {
                box_.0 = box_.0.min(cx);
                box_.1 = box_.1.min(cy);
                box_.2 = box_.2.max(cx);
                box_.3 = box_.3.max(cy);
                for dy in -1_isize..=1 {
                    for dx in -1_isize..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = cx as isize + dx;
                        let ny = cy as isize + dy;
                        if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                            continue;
                        }
                        let ni = ny as usize * w + nx as usize;
                        if mask[ni] != 0 && ids[ni] == usize::MAX {
                            ids[ni] = id;
                            stack.push((nx as usize, ny as usize));
                        }
                    }
                }
            }
            boxes.push(box_);
        }
    }
    (ids, boxes)
}

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

/// Extends a stroke past both ends along its end tangents (razor needles). `factor` scales the
/// overshoot per pass so side whiskers shoot different needle lengths than the core.
fn extend_stroke_ends(pts: &mut Vec<(f64, f64)>, overshoot: f64, factor: f64, seed: u32) {
    if pts.len() < 2 || overshoot <= 0.05 {
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
    let o0 = overshoot * factor * (0.72 + 0.66 * r0);
    let o1 = overshoot * factor * (0.72 + 0.66 * r1);
    let head = (first.0 + d0.0 / l0 * o0, first.1 + d0.1 / l0 * o0);
    let tail = (last.0 + d1.0 / l1 * o1, last.1 + d1.1 / l1 * o1);
    if head.0.is_finite() && head.1.is_finite() && tail.0.is_finite() && tail.1.is_finite() {
        pts.insert(0, head);
        pts.push(tail);
    }
}

/// Zhang-Suen morphological thinning on a binary mask (`0`/`1`), panic-free.
fn zhang_suen_thin(mask: &mut [u8], w: usize, h: usize) {
    if w < 3 || h < 3 {
        return;
    }
    let mut to_clear = Vec::new();
    for _iter in 0..64 {
        let mut changed = false;
        for step in 0..2 {
            to_clear.clear();
            for y in 1..(h - 1) {
                for x in 1..(w - 1) {
                    let idx = y * w + x;
                    if mask[idx] == 0 {
                        continue;
                    }
                    let p2 = mask[(y - 1) * w + x];
                    let p3 = mask[(y - 1) * w + (x + 1)];
                    let p4 = mask[y * w + (x + 1)];
                    let p5 = mask[(y + 1) * w + (x + 1)];
                    let p6 = mask[(y + 1) * w + x];
                    let p7 = mask[(y + 1) * w + (x - 1)];
                    let p8 = mask[y * w + (x - 1)];
                    let p9 = mask[(y - 1) * w + (x - 1)];
                    let b = p2 + p3 + p4 + p5 + p6 + p7 + p8 + p9;
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let a = u8::from(p2 == 0 && p3 == 1)
                        + u8::from(p3 == 0 && p4 == 1)
                        + u8::from(p4 == 0 && p5 == 1)
                        + u8::from(p5 == 0 && p6 == 1)
                        + u8::from(p6 == 0 && p7 == 1)
                        + u8::from(p7 == 0 && p8 == 1)
                        + u8::from(p8 == 0 && p9 == 1)
                        + u8::from(p9 == 0 && p2 == 1);
                    if a != 1 {
                        continue;
                    }
                    let cond = if step == 0 {
                        (p2 * p4 * p6 == 0) && (p4 * p6 * p8 == 0)
                    } else {
                        (p2 * p4 * p8 == 0) && (p2 * p6 * p8 == 0)
                    };
                    if cond {
                        to_clear.push(idx);
                    }
                }
            }
            if !to_clear.is_empty() {
                changed = true;
                for &idx in &to_clear {
                    mask[idx] = 0;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Simplifies a raw skeleton chain via RDP, snaps straight strokes via PCA (prevents letter bending),
/// and applies 2-pass Chaikin smoothing to genuinely curved strokes.
fn simplify_and_straighten_skeleton(raw: &[(f64, f64)]) -> Vec<(f64, f64)> {
    if raw.len() <= 2 {
        return raw.to_vec();
    }
    let simp = rdp_open(raw, 2.2);
    if simp.len() <= 2 {
        return simp;
    }
    // Check chord straightness: if max perpendicular deviation < 10% of chord length, snap straight via PCA!
    let a = simp[0];
    let b = simp[simp.len() - 1];
    let chord = (b.0 - a.0).hypot(b.1 - a.1);
    if chord > 4.0 {
        let mut max_dev = 0.0_f64;
        for &p in &simp[1..(simp.len() - 1)] {
            max_dev = max_dev.max(dist_pt_to_seg(p, a, b));
        }
        if max_dev / chord < 0.105 {
            // PCA principal axis fit across raw points projected to endpoints.
            let n = raw.len() as f64;
            let mean_x = raw.iter().map(|p| p.0).sum::<f64>() / n;
            let mean_y = raw.iter().map(|p| p.1).sum::<f64>() / n;
            let (mut sxx, mut sxy, mut syy) = (0.0_f64, 0.0_f64, 0.0_f64);
            for &(x, y) in raw {
                let dx = x - mean_x;
                let dy = y - mean_y;
                sxx += dx * dx;
                sxy += dx * dy;
                syy += dy * dy;
            }
            let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
            let ux = theta.cos();
            let uy = theta.sin();
            let mut t_min = f64::INFINITY;
            let mut t_max = f64::NEG_INFINITY;
            for &(x, y) in raw {
                let t = (x - mean_x) * ux + (y - mean_y) * uy;
                t_min = t_min.min(t);
                t_max = t_max.max(t);
            }
            return vec![(mean_x + ux * t_min, mean_y + uy * t_min), (mean_x + ux * t_max, mean_y + uy * t_max)];
        }
    }

    // Genuinely curved stroke: 2-pass Chaikin corner cutting preserving exact endpoints.
    let mut cur = simp;
    for _ in 0..2 {
        if cur.len() < 3 {
            break;
        }
        let mut next = Vec::with_capacity(cur.len() * 2);
        next.push(cur[0]);
        for seg in cur.windows(2) {
            let p = seg[0];
            let q = seg[1];
            next.push((p.0 * 0.75 + q.0 * 0.25, p.1 * 0.75 + q.1 * 0.25));
            next.push((p.0 * 0.25 + q.0 * 0.75, p.1 * 0.25 + q.1 * 0.75));
        }
        if let Some(&last) = cur.last() {
            next.push(last);
        }
        cur = next;
    }
    cur
}

type GridBox = (usize, usize, usize, usize);
type DocBox = (f64, f64, f64, f64);
type ChainEnd = (usize, bool, (f64, f64));

/// Downsamples ink coverage to a bounded grid and computes connected components before thinning.
fn build_skeleton_grid(base_cov: &[f32], w: usize, h: usize) -> (Vec<u8>, usize, usize, f64, Vec<GridBox>, usize) {
    let scale = ((w.max(h) as f64) / SKELETON_MAX_GRID_DIM as f64).max(1.0);
    let gw = ((w as f64 / scale).round() as usize).clamp(4, SKELETON_MAX_GRID_DIM);
    let gh = ((h as f64 / scale).round() as usize).clamp(4, SKELETON_MAX_GRID_DIM);
    let mut mask = vec![0u8; gw * gh];
    for gy in 0..gh {
        let sy = ((gy as f64 + 0.5) * scale).floor().clamp(0.0, (h - 1) as f64) as usize;
        for gx in 0..gw {
            let sx = ((gx as f64 + 0.5) * scale).floor().clamp(0.0, (w - 1) as f64) as usize;
            if base_cov[sy * w + sx] >= 0.45 {
                mask[gy * gw + gx] = 1;
            }
        }
    }
    // Compact element preservation starts BEFORE thinning: Zhang-Suen would otherwise reduce
    // the dot of `!` / `i` to a 1-2 px speck that no chain criterion can keep.
    let (_ids, comp_boxes) = grid_connected_components(&mask, gw, gh);
    let max_span = comp_boxes.iter().map(|&(x0, y0, x1, y1)| (x1 - x0).max(y1 - y0)).max().unwrap_or(0);
    zhang_suen_thin(&mut mask, gw, gh);
    (mask, gw, gh, scale, comp_boxes, max_span)
}

#[inline]
fn skeleton_neighbors(x: usize, y: usize, m: &[u8], gw: usize, gh: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(8);
    for dy in -1_isize..=1 {
        for dx in -1_isize..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0 && ny >= 0 && (nx as usize) < gw && (ny as usize) < gh && m[ny as usize * gw + nx as usize] != 0 {
                out.push((nx as usize, ny as usize));
            }
        }
    }
    out
}

/// Traces raw skeleton chains from endpoints/junctions (pass 1) and closed loops (pass 2).
fn trace_skeleton_chains(mask: &[u8], gw: usize, gh: usize) -> Vec<ShtackChain> {
    let mut deg = vec![0u8; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            if mask[y * gw + x] != 0 {
                deg[y * gw + x] = skeleton_neighbors(x, y, mask, gw, gh).len() as u8;
            }
        }
    }
    let mut visited = vec![false; gw * gh];
    let mut chains = Vec::new();
    for pass in 0..2 {
        for sy in 0..gh {
            for sx in 0..gw {
                let s_idx = sy * gw + sx;
                if mask[s_idx] == 0 || visited[s_idx] {
                    continue;
                }
                if pass == 0 && deg[s_idx] == 2 {
                    continue;
                }
                let nbs = skeleton_neighbors(sx, sy, mask, gw, gh);
                if nbs.is_empty() {
                    visited[s_idx] = true;
                    continue;
                }
                for &(nx, ny) in &nbs {
                    let n_idx = ny * gw + nx;
                    if visited[n_idx] && deg[n_idx] == 2 {
                        continue;
                    }
                    let mut pts = vec![(sx, sy), (nx, ny)];
                    visited[s_idx] = true;
                    let mut cur = (nx, ny);
                    let mut prev = (sx, sy);
                    for _ in 0..512 {
                        let c_idx = cur.1 * gw + cur.0;
                        if deg[c_idx] != 2 && c_idx != s_idx {
                            break;
                        }
                        visited[c_idx] = true;
                        let cnbs = skeleton_neighbors(cur.0, cur.1, mask, gw, gh);
                        let next_opt = cnbs.into_iter().find(|&p| p != prev && (!visited[p.1 * gw + p.0] || deg[p.1 * gw + p.0] >= 3));
                        let Some(next) = next_opt else { break };
                        pts.push(next);
                        prev = cur;
                        cur = next;
                        if cur == (sx, sy) {
                            break;
                        }
                    }
                    if pts.len() < 2 {
                        continue;
                    }
                    let node_a = if deg[s_idx] >= 3 { Some((sx, sy)) } else { None };
                    let tail = pts[pts.len() - 1];
                    let node_b = if deg[tail.1 * gw + tail.0] >= 3 { Some(tail) } else { None };
                    chains.push(ShtackChain { pts, node_a, node_b });
                }
            }
        }
    }
    chains
}

/// Merges chains meeting at a junction when their outgoing tangents continue smoothly
/// (`dot < -0.65`). This rebuilds the full vertical stem of `B` / `M`, not fragments.
fn merge_skeleton_chains(chains: &mut Vec<ShtackChain>) -> Vec<bool> {
    let mut active = vec![true; chains.len()];
    for _round in 0..8 {
        let mut ends: Vec<((usize, usize), Vec<ChainEnd>)> = Vec::new();
        for (i, ch) in chains.iter().enumerate() {
            if !active[i] {
                continue;
            }
            for (node, is_start) in [(ch.node_a, true), (ch.node_b, false)] {
                let Some(node) = node else { continue };
                let a = if is_start { ch.pts[0] } else { ch.pts[ch.pts.len() - 1] };
                let b = if is_start { ch.pts[1] } else { ch.pts[ch.pts.len() - 2] };
                let dx = a.0 as f64 - b.0 as f64;
                let dy = a.1 as f64 - b.1 as f64;
                let l = dx.hypot(dy).max(1e-6);
                let dir = (dx / l, dy / l);
                if let Some(entry) = ends.iter_mut().find(|(n, _)| *n == node) {
                    entry.1.push((i, is_start, dir));
                } else {
                    ends.push((node, vec![(i, is_start, dir)]));
                }
            }
        }
        let mut merges: Vec<(usize, bool, usize, bool)> = Vec::new();
        for (_, list) in &ends {
            let mut used = vec![false; list.len()];
            for a in 0..list.len() {
                if used[a] {
                    continue;
                }
                let mut best: Option<(usize, f64)> = None;
                for b in (a + 1)..list.len() {
                    if used[b] || list[a].0 == list[b].0 {
                        continue;
                    }
                    let dot = list[a].2.0 * list[b].2.0 + list[a].2.1 * list[b].2.1;
                    if dot < -0.65 && best.is_none_or(|(_, bd)| dot < bd) {
                        best = Some((b, dot));
                    }
                }
                if let Some((b, _)) = best {
                    used[a] = true;
                    used[b] = true;
                    merges.push((list[a].0, list[a].1, list[b].0, list[b].1));
                }
            }
        }
        if merges.is_empty() {
            break;
        }
        for (ia, sa, ib, sb) in merges {
            let a = &chains[ia];
            let b = &chains[ib];
            let mut seq_a = a.pts.clone();
            if sa {
                seq_a.reverse();
            }
            let mut seq_b = b.pts.clone();
            if !sb {
                seq_b.reverse();
            }
            if !seq_b.is_empty() && seq_a.last() == seq_b.first() {
                seq_b.remove(0);
            }
            let mut pts = seq_a;
            pts.extend(seq_b);
            let node_a = if sa { a.node_b } else { a.node_a };
            let node_b = if sb { b.node_a } else { b.node_b };
            chains.push(ShtackChain { pts, node_a, node_b });
            active.push(true);
            active[ia] = false;
            active[ib] = false;
        }
    }
    active
}

/// Splits sharp L-corners (turn > ~55°) so intersecting strokes both overshoot past the
/// corner, then simplifies each piece into a clean document-space stroke.
fn split_skeleton_chains(chains: &[ShtackChain], active: &[bool], bounds: Rect, scale: f64) -> Vec<Vec<(f64, f64)>> {
    const COS_SHARP_CORNER: f64 = 0.5736;
    let to_doc = |gx: usize, gy: usize| -> (f64, f64) {
        (f64::from(bounds.x0) + (gx as f64 + 0.5) * scale, f64::from(bounds.y0) + (gy as f64 + 0.5) * scale)
    };
    let mut strokes = Vec::new();
    for (i, ch) in chains.iter().enumerate() {
        if !active[i] || ch.pts.len() < 2 {
            continue;
        }
        let gp = &ch.pts;
        let mut cuts = Vec::new();
        for j in 1..(gp.len() - 1) {
            let k = 4.min(j).min(gp.len() - 1 - j);
            if k < 1 {
                continue;
            }
            let d0 = (gp[j].0 as f64 - gp[j - k].0 as f64, gp[j].1 as f64 - gp[j - k].1 as f64);
            let d1 = (gp[j + k].0 as f64 - gp[j].0 as f64, gp[j + k].1 as f64 - gp[j].1 as f64);
            let l0 = d0.0.hypot(d0.1);
            let l1 = d1.0.hypot(d1.1);
            if l0 < 2.0 || l1 < 2.0 {
                continue;
            }
            if (d0.0 * d1.0 + d0.1 * d1.1) / (l0 * l1) < COS_SHARP_CORNER {
                cuts.push(j);
            }
        }
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut s = 0usize;
        for &c in &cuts {
            if c > s {
                ranges.push((s, c));
            }
            s = c;
        }
        if s < gp.len() - 1 {
            ranges.push((s, gp.len() - 1));
        }
        for (a, b) in ranges {
            let raw: Vec<(f64, f64)> = (a..=b).map(|k| to_doc(gp[k].0, gp[k].1)).collect();
            let cleaned = simplify_and_straighten_skeleton(&raw);
            if cleaned.len() >= 2 {
                strokes.push(cleaned);
            }
        }
    }
    strokes
}

/// Compact components (dots, punctuation, hyphens) or components with no skeleton chain are
/// preserved as stylized multi-pass ink dots; strokes fully covered by them are not drawn twice.
fn compact_component_boxes(comp_boxes: &[GridBox], max_span: usize, strokes: &mut Vec<Vec<(f64, f64)>>, bounds: Rect, scale: f64) -> Vec<DocBox> {
    let doc_box = |b: GridBox| -> DocBox {
        let (x0, y0, x1, y1) = b;
        (
            f64::from(bounds.x0) + x0 as f64 * scale,
            f64::from(bounds.y0) + y0 as f64 * scale,
            f64::from(bounds.x0) + (x1 as f64 + 1.0) * scale,
            f64::from(bounds.y0) + (y1 as f64 + 1.0) * scale,
        )
    };
    let mut compact_doc = Vec::new();
    for &b in comp_boxes {
        let span = (b.2 - b.0).max(b.3 - b.1);
        let db = doc_box(b);
        let covered = strokes.iter().any(|s| {
            s.iter().any(|&(px, py)| px >= db.0 - scale && px <= db.2 + scale && py >= db.1 - scale && py <= db.3 + scale)
        });
        if span * 3 <= max_span.max(1) || !covered {
            compact_doc.push(db);
        }
    }
    strokes.retain(|s| {
        !compact_doc.iter().any(|&(x0, y0, x1, y1)| s.iter().all(|&(px, py)| px >= x0 && px <= x1 && py >= y0 && py <= y1))
    });
    compact_doc
}

/// Traces skeleton chains, merges those that continue smoothly through junctions, splits
/// sharp L-corners, and returns simplified document-space strokes plus compact-component
/// boxes (document space) that must be preserved as stylized ink dots.
fn extract_skeleton_chains(base_cov: &[f32], bounds: Rect, w: usize, h: usize) -> (Vec<Vec<(f64, f64)>>, Vec<DocBox>) {
    if w < 4 || h < 4 {
        return (Vec::new(), Vec::new());
    }
    let (mask, gw, gh, scale, comp_boxes, max_span) = build_skeleton_grid(base_cov, w, h);
    let mut chains = trace_skeleton_chains(&mask, gw, gh);
    let active = merge_skeleton_chains(&mut chains);
    let mut strokes = split_skeleton_chains(&chains, &active, bounds, scale);
    let compact_doc = compact_component_boxes(&comp_boxes, max_span, &mut strokes, bounds, scale);
    (strokes, compact_doc)
}

/// Builds 3..=6 independent variable-width tapered sketch passes per merged stroke.
/// Core passes keep a dense body with long razor tapers; side whiskers spread wider, tilt
/// and bow independently and shoot varied needle lengths, producing natural air gaps.
fn build_shtack_stroke_polygons(strokes: &[Vec<(f64, f64)>], le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let half_w = f64::from(le.shtack_thickness_px).max(4.0) * 0.5;
    let bristles = usize::from(le.shtack_bristles.clamp(3, 6));
    let split = (f64::from(le.shtack_split_pct) / 100.0).clamp(0.0, 1.0);
    let dry = (f64::from(le.shtack_dryness_pct) / 100.0).clamp(0.0, 1.0);
    let is_dry = le.shtack_variant == SfxShtackVariant::DryPencil;
    let needle = f64::from(le.shtack_overshoot_px) * if is_dry { 0.45 } else { 1.0 };
    let seed_base = le.shtack_seed.wrapping_mul(12_347);
    let mut polys = Vec::new();

    for (si, stroke) in strokes.iter().enumerate() {
        if stroke.len() < 2 {
            continue;
        }
        let s_seed = seed_base.wrapping_add((si as u32).wrapping_mul(311));
        let arc_len: f64 = stroke.windows(2).map(|seg| (seg[1].0 - seg[0].0).hypot(seg[1].1 - seg[0].1)).sum();
        let samples_n = (arc_len / 3.0).round().clamp(10.0, 36.0) as usize;

        for k in 0..bristles {
            let p_seed = s_seed.wrapping_add((k as u32).wrapping_mul(1_073));
            let r0 = f64::from(hash_u32(p_seed));
            let r1 = f64::from(hash_u32(p_seed.wrapping_add(17)));
            let r2 = f64::from(hash_u32(p_seed.wrapping_add(31)));
            let r3 = f64::from(hash_u32(p_seed.wrapping_add(59)));
            let core = k < 2;

            // Every pass extends past BOTH of its own ends along the end tangent.
            let factor = if core { 1.0 } else { 0.45 + r3 * 1.1 };
            let mut extended = stroke.clone();
            extend_stroke_ends(&mut extended, needle, factor, p_seed);
            let base = resample_polyline(&extended, samples_n);
            if base.len() < 2 {
                continue;
            }

            let width = half_w * if core { 0.82 + 0.16 * r0 } else { 0.28 + 0.27 * r0 };
            let side = if k >= 2 && (k - 2) % 2 == 0 { 1.0 } else { -1.0 };
            let rank = (k.saturating_sub(2) / 2) as f64;
            let base_off = if core {
                (if k == 0 { -1.0 } else { 1.0 }) * half_w * 0.10 * r1
            } else {
                side * half_w * (0.40 + 0.30 * rank + 0.35 * split) * (0.7 + 0.6 * r1)
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
                // Smooth 0 -> full -> 0 taper: razor needle tips at both ends.
                let taper = (PI * t).sin().max(0.0).powf(0.55);
                let hw = (taper * width).max(0.12);
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

/// Preserves compact glyph parts (dots of `!`, `i`, `?`, hyphens) as a diamond-tapered ink
/// dot plus a short sketch cross-hatch, using the same multi-pass pen as the rest of Shtack.
fn build_compact_dot_polys(boxes: &[(f64, f64, f64, f64)], le: &SfxLineEffect) -> Vec<Vec<(f64, f64)>> {
    let seed = le.shtack_seed.wrapping_mul(5_153);
    let mut polys = Vec::new();
    for (i, &(x0, y0, x1, y1)) in boxes.iter().enumerate() {
        let cx = (x0 + x1) * 0.5;
        let cy = (y0 + y1) * 0.5;
        let len = (x1 - x0).max(y1 - y0).max(4.0);
        let r = f64::from(hash_u32(seed.wrapping_add((i as u32).wrapping_mul(97))));
        let ang = PI * 0.25 + (r - 0.5) * 0.5;
        let (sa, ca) = ang.sin_cos();
        let main = vec![(cx - ca * len * 0.46, cy - sa * len * 0.46), (cx + ca * len * 0.46, cy + sa * len * 0.46)];

        let mut dot_le = *le;
        dot_le.shtack_thickness_px = (len * 1.05).max(6.0) as f32;
        dot_le.shtack_bristles = le.shtack_bristles.clamp(3, 4);
        dot_le.shtack_overshoot_px = (len * 0.5) as f32;
        polys.extend(build_shtack_stroke_polygons(&[main], &dot_le));

        let cross_ang = ang + PI * 0.55;
        let (sc, cc) = cross_ang.sin_cos();
        let slash = vec![(cx - cc * len * 0.32, cy - sc * len * 0.32), (cx + cc * len * 0.32, cy + sc * len * 0.32)];
        dot_le.shtack_thickness_px = (len * 0.42).max(4.0) as f32;
        dot_le.shtack_bristles = 3;
        polys.extend(build_shtack_stroke_polygons(&[slash], &dot_le));
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
    let mask: Vec<f32> = cov.iter().map(|&c| if c >= 0.45 { 1.0 } else { 0.0 }).collect();
    let dist = compute_mask_sdf(&mask, w, h);
    let seed = le.shtack_seed.wrapping_mul(7_919);
    for (i, c) in cov.iter_mut().enumerate() {
        if *c <= 1e-4 {
            continue;
        }
        let x = (i % w) as f32;
        let y = (i / w) as f32;
        let d = f64::from(dist.get(i).copied().unwrap_or(0.0));
        let edge = (d / (d + 2.0)).clamp(0.0, 1.0);
        let grain = fbm_2d(x * 0.55, y * 0.55, seed) as f64;
        let streak = f64::from(value_noise_2d(x * 0.12, y * 0.85, seed.wrapping_add(31)));
        let noise = (grain * 0.7 + streak * 0.3).clamp(0.0, 1.0);
        let density = 1.0 - dryness * (0.30 + 0.85 * (1.0 - edge)) * noise;
        *c = (*c as f64 * density.clamp(0.08, 1.0)).clamp(0.0, 1.0) as f32;
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
            let paper = value_noise_2d(px * 0.35, py * 0.35, seed.wrapping_add(97));
            let edge_fade = (sdf[idx] / (3.0 + wear * 16.0)).clamp(0.0, 1.0);
            let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                ((ink_score - thresh) * 6.5 + 0.5).clamp(0.0, 1.0)
            };
            base_cov[idx] *= keep;
        }
    }
}

fn apply_dry_brush_seg(base_cov: &mut [f32], geom: &PreparedSfx, bounds: Rect, w: usize, h: usize, db: &SfxDryBrushSpec, is_binary: bool) {
    // Collect long contour segments to establish local stroke tangents without vortices.
    let mut segs: Vec<(f32, f32, f32, f32, f32, f32)> = Vec::new();
    for pl in &geom.polylines {
        let simp = rdp_closed(&pl.pts, 2.0);
        let n = simp.len();
        for i in 0..n {
            let a = simp[i];
            let b = simp[(i + 1) % n];
            let dx = b.0 - a.0;
            let dy = b.1 - a.1;
            let len = dx.hypot(dy);
            if len >= 6.0 {
                segs.push((a.0 as f32, a.1 as f32, b.0 as f32, b.1 as f32, (dx / len) as f32, (dy / len) as f32));
            }
        }
    }
    if segs.is_empty() {
        apply_dry_brush_sdf_physics(base_cov, bounds, w, h, db, is_binary);
        return;
    }

    let sdf = compute_mask_sdf(base_cov, w, h);
    let dryness = db.dryness_pct / 100.0;
    let freq = 0.09 + (db.freq_pct / 100.0) * 0.42;
    let warp = (db.warp_pct / 100.0) * 12.0;
    let grain = db.paper_grain_pct / 100.0;
    let wear = db.edge_wear_pct / 100.0;
    let seed = db.seed.wrapping_mul(13_109);

    for y in 0..h {
        let py = (bounds.y0 + y as i32) as f32;
        for x in 0..w {
            let idx = y * w + x;
            if base_cov[idx] <= 0.02 {
                continue;
            }
            let px = (bounds.x0 + x as i32) as f32;
            let mut best_d2 = f32::INFINITY;
            let (mut tx, mut ty) = (1.0_f32, 0.0_f32);
            for &(ax, ay, bx, by, stx, sty) in &segs {
                let abx = bx - ax;
                let aby = by - ay;
                let l2 = (abx * abx + aby * aby).max(1e-6);
                let t = (((px - ax) * abx + (py - ay) * aby) / l2).clamp(0.0, 1.0);
                let cx = ax + abx * t - px;
                let cy = ay + aby * t - py;
                let d2 = cx * cx + cy * cy;
                if d2 < best_d2 {
                    best_d2 = d2;
                    tx = stx;
                    ty = sty;
                }
            }
            let u = px * tx + py * ty;
            let v = -px * ty + py * tx;
            let w_off = (fbm_2d(u * 0.035, v * 0.035, seed.wrapping_add(17)) - 0.5) * warp;
            let bristle = value_noise_2d(u * 0.024, (v + w_off) * freq, seed);
            let paper = value_noise_2d(px * 0.35, py * 0.35, seed.wrapping_add(113));
            let edge_fade = (sdf[idx] / (3.0 + wear * 15.0)).clamp(0.0, 1.0);
            let ink_score = bristle * (1.0 - grain * 0.35) + paper * (grain * 0.35) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                ((ink_score - thresh) * 6.5 + 0.5).clamp(0.0, 1.0)
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
            let paper = value_noise_2d(px * 0.35, py * 0.35, seed.wrapping_add(131));
            let edge_fade = (sdf[idx] / (3.0 + wear * 14.0)).clamp(0.0, 1.0);
            let ink_score = lic * (1.0 - grain * 0.32) + paper * (grain * 0.32) + edge_fade * wear * 0.55 - wear * 0.42;
            let thresh = dryness * 0.85;
            let keep = if is_binary {
                if ink_score >= thresh { 1.0 } else { 0.0 }
            } else {
                ((ink_score - thresh) * 7.0 + 0.5).clamp(0.0, 1.0)
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

fn prepare_proxy_setup(orig_data: &SfxData, full_geom: &PreparedSfx, full_bounds: Rect) -> (Rect, PreparedSfx, f64, Option<SfxData>) {
    let (pw, ph, proxy_scale) = proxy_target(full_bounds);
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

/// Renders an [`SfxData`] non-destructively into offscreen document [`Surface`]`(s)`.
pub fn render_sfx(data: &SfxData, engine: &mut TextEngine, format: PixelFormat, dpi: f32) -> SfxRenderOutput {
    let full_geom = build_sfx_geometry(data, engine, dpi);
    let full_bounds = compute_render_bounds(data, &full_geom);
    if full_bounds.is_empty() {
        let s = Surface::new(format);
        return if data.has_negative_invert() {
            SfxRenderOutput::InvertPair { under: s.clone(), invert: s, bounds: Rect::EMPTY, geom: full_geom }
        } else {
            SfxRenderOutput::Single { surface: s, bounds: Rect::EMPTY, geom: full_geom }
        };
    }

    // A >16 MP / >8192 px lettering layer used to be dropped silently. Render it into a bounded
    // proxy, then upsample onto the full clipped rect so the layer stays visible and editable.
    let (bounds, geom, proxy_scale, scaled_data) = prepare_proxy_setup(data, &full_geom, full_bounds);
    let data = scaled_data.as_ref().unwrap_or(data);

    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let n = w * h;
    let mut under_rgba = vec![[0.0_f32; 4]; n];

    let le = &data.line_effect;
    let is_one_bit = le.style == SfxLineStyle::OneBit;
    let base_ras = fill_rasterizer_from_polys(&geom.polylines, [0.0, 0.0]);
    let raw_base_cov = base_ras.render(bounds);

    // Precompute mode-specific line/silhouette coverages so Pass 1 (Halo) wraps them cleanly.
    let mut face_cov = raw_base_cov.clone();
    let mut line_cov: Option<Vec<f32>> = None;

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
                line_cov = Some(rasterize_polygon_list(&sp, bounds));
            }
        }
        SfxLineStyle::OneBit => {
            let intensity = le.one_bit_intensity / 100.0;
            for (i, c) in face_cov.iter_mut().enumerate() {
                let px = (bounds.x0 + (i % w) as i32) as f32;
                let py = (bounds.y0 + (i / w) as i32) as f32;
                let jitter = (value_noise_2d(px * 0.45, py * 0.45, 701) - 0.5) * intensity * 0.55;
                *c = if *c + jitter >= 0.5 { 1.0 } else { 0.0 };
            }
            if le.line_width_px > 0.05 {
                let mut lcov = dilate_coverage_approx(&face_cov, w, h, le.line_width_px * 0.5);
                for (i, c) in lcov.iter_mut().enumerate() {
                    let px = (bounds.x0 + (i % w) as i32) as f32;
                    let py = (bounds.y0 + (i / w) as i32) as f32;
                    let j = (value_noise_2d(px * 0.38, py * 0.38, 919) - 0.5) * intensity * 0.5;
                    let edge = (*c - face_cov[i]).max(0.0);
                    *c = if edge + j >= 0.45 { 1.0 } else { 0.0 };
                }
                line_cov = Some(lcov);
            }
        }
        SfxLineStyle::Spikes => {
            let mut sp_polys = build_spikes_polygons(&geom, le);
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
            line_cov = Some(rasterize_polygon_list(&sp_polys, bounds));
        }
        SfxLineStyle::GPenNib => {
            let gp_polys = build_gpen_polygons(&geom, le);
            line_cov = Some(rasterize_polygon_list(&gp_polys, bounds));
        }
        SfxLineStyle::BrushStroke => {
            let bs_polys = build_brush_stroke_polygons(&geom, le);
            line_cov = Some(rasterize_polygon_list(&bs_polys, bounds));
        }
        SfxLineStyle::Shtack => {
            // One shared multi-pass sketch geometry for both submodes. No fake white
            // `cut_polys` wedges: the air gaps come from overlapping tapered passes.
            let (strokes, dot_boxes) = extract_skeleton_chains(&raw_base_cov, bounds, w, h);
            let mut shtack_polys = build_shtack_stroke_polygons(&strokes, le);
            shtack_polys.extend(build_compact_dot_polys(&dot_boxes, le));
            let mut shtack_cov = rasterize_polygon_list(&shtack_polys, bounds);
            if le.shtack_variant == SfxShtackVariant::DryPencil {
                apply_shtack_dry_breakup(&mut shtack_cov, w, h, le);
            }
            for (dst, &src) in face_cov.iter_mut().zip(shtack_cov.iter()) {
                *dst = src.clamp(0.0, 1.0);
            }
        }
    }

    // Stage 0: Vector Shadow / 3D + Legacy Shadows / Extrude3D / Stroke Stack
    render_vector_shadow_pass(data, &geom, bounds, &mut under_rgba);
    render_shadow_passes(data, &geom, bounds, w, h, &mut under_rgba);
    render_extrude_passes(data, &geom, bounds, &mut under_rgba);
    render_stroke_passes(data, &geom, bounds, &mut under_rgba);

    // Stage 1 (Pass 1 — Halo / Ореол behind ALL letters)
    if le.halo_width_px > 0.05 {
        let mut union_cov = face_cov.clone();
        if let Some(lc) = &line_cov {
            for (u, &l) in union_cov.iter_mut().zip(lc.iter()) {
                *u = u.max(l);
            }
        }
        let mut halo_cov = dilate_coverage_approx(&union_cov, w, h, le.halo_width_px);
        if is_one_bit {
            let intensity = le.one_bit_intensity / 100.0;
            for (i, c) in halo_cov.iter_mut().enumerate() {
                let px = (bounds.x0 + (i % w) as i32) as f32;
                let py = (bounds.y0 + (i / w) as i32) as f32;
                let j = (value_noise_2d(px * 0.3, py * 0.3, 1301) - 0.5) * intensity * 0.45;
                *c = if *c + j >= 0.48 { 1.0 } else { 0.0 };
            }
        }
        paint_solid_over(&mut under_rgba, &halo_cov, color_rgba(le.halo_color), 1.0);
    }

    // Stage 2 (Pass 2 — Face Fill + Dry Brush + Distress)
    apply_grain_distress(data, &mut face_cov, bounds, w);
    if data.dry_brush.enabled {
        apply_dry_brush_mask(&mut face_cov, &geom, bounds, w, h, &data.dry_brush, is_one_bit);
    }

    if data.has_negative_invert() {
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
    }
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
}
