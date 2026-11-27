//! Style 1 — しぶふでペン (`Shibufude v3`, rev 2) 1:1 port of `shibufude_v3.py`.
//!
//! 1. Clean base `band_sdf` (`Maru` analytical `|glyph_sdf - c_off| - hw` or `Kado` convex quads +
//!    miter-clip joins with contour-wide orientation sign, point dedup, and `bl < 1e-5` hairpin guard).
//! 2. Pure 2D SDF perturbation (`perturb_shibufude_sdf_doc`, 1:1 WGSL-portable in document coords).
//! 3. Clustered organic しぶき splatter (`build_shibufude_splatter`) computed purely on CPU against
//!    analytical `VectorGlyphSdf` (`hw_out + 0.30 * line_w`) — zero GPU readback, 60 FPS live drag.
//! 4. Seed-identical geometry feeding both `OneBit` and `Rough` edge kasure modes + `knockout` (`0.84`).

use std::f64::consts::TAU;

use photocraft_doc::sfx::{SfxLineEffect, SfxShibufudeProfile, SfxV3EdgeMode};
use photocraft_geom::Rect;

use crate::sfx_geom::PreparedSfx;
use crate::sfx_v3_common::{
    EdgeKasureParams, PolyRing, VectorGlyphSdf, apply_edge_kasure_breakup_doc, classify_glyph_contours, clean_closed_ring, exact_sdf, h1,
    polyline_resample, rasterize_polygons_max, value_noise_2d,
};

pub const SHIBUFUDE_MITER_LIMIT: f64 = 2.8;
pub const SHIBUFUDE_KNOCKOUT_WIDTH_SCALE: f32 = 0.84;

/// Effective Shibufude band width accounting for `knockout` (`抜き`, `0.84` scale) and sensible default.
#[inline]
pub fn effective_shibufude_line_w(le: &SfxLineEffect) -> f32 {
    let base = if le.line_width_px > 0.5 { le.line_width_px } else { 13.5 };
    let scaled = if le.shibufude_knockout { base * SHIBUFUDE_KNOCKOUT_WIDTH_SCALE } else { base };
    scaled.max(2.0)
}

/// Clean angular (`角`) stroke band: convex segment quads + analytical miter-clip joins (`miter_limit = 2.8`).
///
/// Deduplicates coincident points after flattening and guards degenerate `~180°` hairpins (`bl < 1e-5`)
/// so emitted vertices contain zero `NaN`/`Inf`.
pub fn build_kado_base_polys(contour_groups: &[Vec<PolyRing>], vsdf: &VectorGlyphSdf, hw_out: f64, hw_in: f64, miter_limit: f64) -> Vec<PolyRing> {
    let mut polys = Vec::new();
    let push_finite = |polys: &mut Vec<PolyRing>, ring: PolyRing| {
        if ring.len() >= 3 && ring.iter().all(|p| p.0.is_finite() && p.1.is_finite()) {
            polys.push(ring);
        }
    };

    for group in contour_groups {
        for cont in group {
            let ring = clean_closed_ring(cont, 0.4, 0.9992);
            let n = ring.len();
            if n < 3 {
                continue;
            }
            let orient = vsdf.contour_orient_sign(&ring);
            let mut tang = Vec::with_capacity(n);
            let mut nrm = Vec::with_capacity(n);
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let ex = b.0 - a.0;
                let ey = b.1 - a.1;
                let el = ex.hypot(ey).max(1e-9);
                let tx = ex / el;
                let ty = ey / el;
                tang.push((tx, ty));
                nrm.push((ty * orient, -tx * orient));
            }

            // 1. Convex segment quads
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let nu = nrm[i];
                push_finite(
                    &mut polys,
                    vec![
                        (a.0 + nu.0 * hw_out, a.1 + nu.1 * hw_out),
                        (b.0 + nu.0 * hw_out, b.1 + nu.1 * hw_out),
                        (b.0 - nu.0 * hw_in, b.1 - nu.1 * hw_in),
                        (a.0 - nu.0 * hw_in, a.1 - nu.1 * hw_in),
                    ],
                );
            }

            // 2. Analytical miter-clip joins with `bl < 1e-5` hairpin guard
            for i in 0..n {
                let prev = (i + n - 1) % n;
                let p = ring[i];
                let t0 = tang[prev];
                let n0 = nrm[prev];
                let n1 = nrm[i];
                let turn = n1.0 * t0.0 + n1.1 * t0.1;
                if turn.abs() < 1e-4 {
                    continue;
                }
                let (u0, u1, d) = if turn > 0.0 {
                    (n0, n1, hw_out)
                } else {
                    ((-n0.0, -n0.1), (-n1.0, -n1.1), hw_in)
                };
                let q0 = (p.0 + u0.0 * d, p.1 + u0.1 * d);
                let q1 = (p.0 + u1.0 * d, p.1 + u1.1 * d);
                let bis = (u0.0 + u1.0, u0.1 + u1.1);
                let bl = bis.0.hypot(bis.1);
                if !bl.is_finite() || bl < 1e-5 {
                    let cap_d = d * miter_limit.min(1.5);
                    push_finite(
                        &mut polys,
                        vec![p, q0, (q0.0 + t0.0 * cap_d, q0.1 + t0.1 * cap_d), (q1.0 + t0.0 * cap_d, q1.1 + t0.1 * cap_d), q1],
                    );
                    continue;
                }
                let u_bis = (bis.0 / bl, bis.1 / bl);
                let cos_h = (u_bis.0 * u0.0 + u_bis.1 * u0.1).max(1e-5);
                let miter_d = d / cos_h;
                let limit_d = d * miter_limit;
                if miter_d <= limit_d {
                    push_finite(&mut polys, vec![p, q0, (p.0 + u_bis.0 * miter_d, p.1 + u_bis.1 * miter_d), q1]);
                } else {
                    let s0 = (u0.0 - u_bis.0 * cos_h, u0.1 - u_bis.1 * cos_h);
                    let sin_h = s0.0.hypot(s0.1).max(1e-5);
                    let perp_len = ((d - limit_d * cos_h) / sin_h).max(0.0);
                    let perp = ((s0.0 / sin_h) * perp_len, (s0.1 / sin_h) * perp_len);
                    let mid = (p.0 + u_bis.0 * limit_d, p.1 + u_bis.1 * limit_d);
                    push_finite(&mut polys, vec![p, q0, (mid.0 + perp.0, mid.1 + perp.1), (mid.0 - perp.0, mid.1 - perp.1), q1]);
                }
            }
        }
    }
    polys
}

/// Pure 2D SDF perturbation (`shibufude_v3.py:285-336`) evaluated in **document coordinates**
/// (1:1 identical to `sfx.wgsl:fs_shibufude_perturb`).
pub fn perturb_shibufude_sdf_doc(band_sdf: &[f32], bounds: Rect, line_w: f32, roughness_pct: f32, seed: u32) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let rough = (roughness_pct / 100.0).clamp(0.0, 1.0);
    if rough <= 1e-4 || w == 0 || h == 0 {
        return band_sdf.to_vec();
    }
    let lw = line_w.max(4.0);
    let inv_w = 1.0 / lw;
    let wf = 0.32 * inv_w;
    let warp_amp = lw * 0.45 * rough;
    let r_amp = 0.36 + 0.64 * rough;
    let s_warp1 = seed.wrapping_mul(97).wrapping_add(11);
    let s_warp2 = seed.wrapping_mul(97).wrapping_add(29);
    let s_mul = seed.wrapping_mul(131);

    let rot_noise = |x_in: f32, y_in: f32, ang: f32, freq_mul: f32, s_off: u32| -> f32 {
        let (sa, ca) = ang.sin_cos();
        let f = freq_mul * inv_w;
        value_noise_2d((x_in * ca - y_in * sa) * f, (x_in * sa + y_in * ca) * f, s_mul.wrapping_add(s_off))
    };

    let mut out = vec![0.0_f32; w * h];
    for y in 0..h {
        let ys = (bounds.y0 as f32) + (y as f32) + 0.5;
        let row = y * w;
        for x in 0..w {
            let xs = (bounds.x0 as f32) + (x as f32) + 0.5;
            let wx = (value_noise_2d(xs * wf, ys * wf, s_warp1) - 0.5) * warp_amp;
            let wy = (value_noise_2d(xs * wf + 13.7, ys * wf - 7.3, s_warp2) - 0.5) * warp_amp;
            let px = xs + wx;
            let py = ys + wy;

            let n_low = (rot_noise(xs, ys, 0.37, 0.18, 17) - 0.5) * 2.0;
            let n_mid = (rot_noise(px, py, 1.09, 0.54, 41) - 0.5) * 2.0;
            let n_fine = (rot_noise(px, py, 1.93, 1.38, 89) - 0.5) * 2.0;
            let n_micro = (rot_noise(xs, ys, 2.61, 2.85, 151) - 0.5) * 2.0;

            let c1 = rot_noise(px, py, 0.73, 0.78, 211);
            let c2 = rot_noise(px, py, 2.29, 1.72, 277);
            let c_mix = c1 * 0.62 + c2 * 0.38;
            let chip = (c_mix - 0.65).max(0.0) / 0.35;
            let bump = (0.30 - c_mix).max(0.0) / 0.30;

            let disp = ((n_low * 0.26 + n_mid * 0.21 + n_fine * 0.125 + n_micro * 0.065) * lw * r_amp
                + bump.powf(1.7) * (lw * 0.24 * rough)
                - chip.powf(1.6) * (lw * 0.40 * rough))
                .clamp(-lw * 0.36, lw * 0.42);
            out[row + x] = band_sdf[row + x] - disp;
        }
    }
    out
}

fn irregular_blob(cx: f64, cy: f64, rad: f64, seed_i: u32, squash: f64) -> PolyRing {
    const M: usize = 12;
    let s3 = seed_i.wrapping_mul(3);
    let rot = h1(s3.wrapping_add(5)) * TAU;
    let (sr, cr) = rot.sin_cos();
    let rx = rad * (1.0 + (h1(s3.wrapping_add(11)) - 0.5) * 2.0 * squash * 0.72);
    let ry = rad * (1.0 - (h1(s3.wrapping_add(13)) - 0.5) * 2.0 * squash * 0.85);
    let ph = h1(s3.wrapping_add(17)) * TAU;
    let mut pts = Vec::with_capacity(M);
    for q in 0..M {
        let aq = (q as f64 / M as f64) * TAU;
        let lobe = 0.12 * (3.0 * aq + ph).sin();
        let wob = 1.0 + lobe + (h1(s3.wrapping_add(20 + q as u32)) - 0.5) * 2.0 * squash * 0.85;
        let px = aq.cos() * rx * wob;
        let py = aq.sin() * ry * wob;
        pts.push((cx + px * cr - py * sr, cy + px * sr + py * cr));
    }
    pts
}

/// Clustered organic しぶき droplets along outer glyph contours (`depth % 2 == 0`).
///
/// Validates clearance strictly against CPU analytical `vsdf.signed_distance` with safety margin
/// `hw_out + 0.30 * line_w` (zero GPU readback, 60 FPS interactive drags).
pub fn build_shibufude_splatter(contour_groups: &[Vec<PolyRing>], vsdf: &VectorGlyphSdf, line_w: f64, le: &SfxLineEffect) -> Vec<PolyRing> {
    let splat_pct = f64::from(le.shibufude_splatter_pct) / 100.0;
    if splat_pct <= 1e-3 {
        return Vec::new();
    }
    let seed = le.shibufude_seed;
    let spread_px = f64::from(le.shibufude_spread_px).max(4.0);
    let size_pct = f64::from(le.shibufude_splat_size_pct) / 100.0;
    let hw_out = line_w * 0.60;
    let min_glyph_clear = hw_out + 0.30 * line_w;
    let mut polys = Vec::new();
    let mut ci = 0u32;

    for group in contour_groups {
        if group.is_empty() {
            continue;
        }
        let classes = classify_glyph_contours(group);
        for (cont, cls) in group.iter().zip(classes) {
            ci = ci.wrapping_add(1);
            // Only emit outward splatter on outer contours (depth 0; skip holes and deep islands).
            if cls.depth != 0 {
                continue;
            }
            let ring = polyline_resample(cont, 3.0);
            let n = ring.len();
            if n < 10 {
                continue;
            }
            let orient = vsdf.contour_orient_sign(&ring);
            let mut seg = Vec::with_capacity(n);
            let mut arc = Vec::with_capacity(n);
            let mut total = 0.0_f64;
            for i in 0..n {
                arc.push(total);
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let l = (b.0 - a.0).hypot(b.1 - a.1);
                seg.push(l);
                total += l;
            }
            total = total.max(1e-6);

            let mut t0 = Vec::with_capacity(n);
            let mut nrm = Vec::with_capacity(n);
            for i in 0..n {
                let prev = ring[(i + n - 1) % n];
                let next = ring[(i + 1) % n];
                let tx = next.0 - prev.0;
                let ty = next.1 - prev.1;
                let tl = tx.hypot(ty).max(1e-9);
                let ux = tx / tl;
                let uy = ty / tl;
                t0.push((ux, uy));
                nrm.push((uy * orient, -ux * orient));
            }

            let mut curv = Vec::with_capacity(n);
            let mut max_curv = 0.0_f64;
            for i in 0..n {
                let tn = t0[(i + 1) % n];
                let c = (t0[i].0 * tn.1 - t0[i].1 * tn.0).abs();
                curv.push(c);
                if c > max_curv {
                    max_curv = c;
                }
            }
            let inv_curv = 1.0 / (max_curv + 1e-6);

            let n_clusters = ((total / (line_w * 4.4).max(54.0) * splat_pct) as usize).max(3);
            let min_gap = (line_w * 2.1).max(24.0);
            let cs_seed = seed.wrapping_mul(2749).wrapping_add(ci.wrapping_mul(313));
            let mut used_arcs: Vec<f64> = Vec::new();

            for k in 0..n_clusters {
                let ku = k as u32;
                let r0 = h1(cs_seed.wrapping_add(ku.wrapping_mul(5)));
                let r1 = h1(cs_seed.wrapping_add(ku.wrapping_mul(5)).wrapping_add(1));
                let c_idx = if r1 > 0.38 {
                    let mut best_i = 0usize;
                    let mut best_sc = -1.0_f64;
                    for tr in 0..8u32 {
                        let idx = (h1(cs_seed.wrapping_add(ku.wrapping_mul(11)).wrapping_add(100 + tr)) * n as f64) as usize % n;
                        let sc = curv[idx] * inv_curv + 0.32 * h1(cs_seed.wrapping_add(ku.wrapping_mul(7)).wrapping_add(tr));
                        if sc > best_sc {
                            best_sc = sc;
                            best_i = idx;
                        }
                    }
                    best_i
                } else {
                    let q = (r0 * total).rem_euclid(total);
                    arc.partition_point(|&v| v < q) % n
                };

                let a_c = arc[c_idx];
                let too_close = used_arcs
                    .iter()
                    .any(|&u| (((a_c - u + total * 0.5).rem_euclid(total)) - total * 0.5).abs() < min_gap);
                if too_close {
                    continue;
                }
                used_arcs.push(a_c);

                let (px, py) = ring[c_idx];
                let (nx, ny) = nrm[c_idx];
                let c_seed = cs_seed.wrapping_add(ku.wrapping_mul(23));
                let is_rich = h1(c_seed.wrapping_add(4)) > 0.42;
                let n_drops = if is_rich {
                    5 + (9.0 * h1(c_seed.wrapping_add(2)) * splat_pct) as usize
                } else {
                    3 + (4.5 * h1(c_seed.wrapping_add(2)) * splat_pct) as usize
                };

                for j in 0..n_drops {
                    let ds = c_seed.wrapping_add((j as u32).wrapping_mul(13));
                    let ang = ny.atan2(nx) + (h1(ds) - 0.5) * 1.75;
                    let u = h1(ds.wrapping_add(1));
                    let rr = u.powf(2.1) * spread_px;
                    let size_fall = 1.0 - 0.56 * (rr / spread_px).min(1.0);
                    let rsize = h1(ds.wrapping_add(2));
                    let mut rad = if is_rich && j == 0 && rr < spread_px * 0.45 {
                        (0.28 + 0.18 * h1(ds.wrapping_add(9))) * line_w * size_pct
                    } else if rsize > 0.86 && rr < spread_px * 0.55 {
                        (0.24 + 0.16 * h1(ds.wrapping_add(9))) * line_w * size_pct * size_fall
                    } else if rsize > 0.50 {
                        (0.13 + 0.15 * h1(ds.wrapping_add(5))) * line_w * size_pct * size_fall
                    } else {
                        (0.065 + 0.08 * h1(ds.wrapping_add(6))) * line_w * size_pct * size_fall
                    };
                    rad = rad.max(0.85);

                    let mut cx = px + ang.cos() * (hw_out + 1.8 + rr);
                    let mut cy = py + ang.sin() * (hw_out + 1.8 + rr);
                    let want = min_glyph_clear + 1.4 + 0.52 * rad;
                    let mut gs = vsdf.signed_distance((cx, cy));
                    if gs < want {
                        let push = want - gs;
                        cx += nx * push;
                        cy += ny * push;
                        gs = vsdf.signed_distance((cx, cy));
                    }
                    if gs < min_glyph_clear + 0.5 * rad {
                        continue;
                    }
                    polys.push(irregular_blob(cx, cy, rad, ds, 0.26));

                    if rad > line_w * 0.15 {
                        let n_sat = 1 + (3.2 * h1(ds.wrapping_add(7))) as usize;
                        for q in 0..n_sat {
                            let ss_s = ds.wrapping_add(70 + (q as u32).wrapping_mul(17));
                            let s_ang = h1(ss_s) * TAU;
                            let s_rad = (rad * (0.18 + 0.22 * h1(ss_s.wrapping_add(2)).powf(1.5))).max(0.68);
                            let s_dist = rad * (1.30 + 1.15 * h1(ss_s.wrapping_add(1))) + s_rad;
                            let sx = cx + s_ang.cos() * s_dist;
                            let sy = cy + s_ang.sin() * s_dist;
                            if vsdf.signed_distance((sx, sy)) > min_glyph_clear + 0.5 * s_rad {
                                polys.push(irregular_blob(sx, sy, s_rad, ss_s.wrapping_add(3), 0.26));
                            }
                        }
                    }
                }
            }
        }
    }
    polys
}

/// Extracts `Kado` band quads/joins (empty for `Maru`) and CPU-validated しぶき splatter polygons.
pub fn build_shibufude_v3_vector(geom: &PreparedSfx, le: &SfxLineEffect) -> (Vec<PolyRing>, Vec<PolyRing>) {
    let groups: Vec<Vec<PolyRing>> = geom
        .glyphs
        .iter()
        .map(|g| g.polylines.iter().map(|pl| pl.pts.clone()).collect())
        .collect();
    let vsdf = VectorGlyphSdf::from_prepared(geom);
    let line_w = f64::from(effective_shibufude_line_w(le));
    let hw_out = line_w * 0.60;
    let hw_in = line_w * 0.40;
    let band_polys = match le.shibufude_profile {
        SfxShibufudeProfile::Kado => build_kado_base_polys(&groups, &vsdf, hw_out, hw_in, SHIBUFUDE_MITER_LIMIT),
        SfxShibufudeProfile::Maru => Vec::new(),
    };
    let splat_polys = build_shibufude_splatter(&groups, &vsdf, line_w, le);
    (band_polys, splat_polys)
}

/// Full CPU oracle render for Style 1 (`Shibufude v3`).
pub fn render_shibufude_v3_cpu(geom: &PreparedSfx, face_cov: &[f32], bounds: Rect, le: &SfxLineEffect) -> Vec<f32> {
    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    if w == 0 || h == 0 {
        return Vec::new();
    }
    let line_w = effective_shibufude_line_w(le);
    let hw_out = line_w * 0.60;
    let hw_in = line_w * 0.40;
    let (band_polys, splat_polys) = build_shibufude_v3_vector(geom, le);

    let band_sdf = match le.shibufude_profile {
        SfxShibufudeProfile::Maru => {
            let glyph_sdf = exact_sdf(face_cov, w, h);
            let c_off = (hw_out - hw_in) * 0.5;
            let hw = (hw_out + hw_in) * 0.5;
            glyph_sdf.into_iter().map(|d| (d - c_off).abs() - hw).collect()
        }
        SfxShibufudeProfile::Kado => {
            let base_cov = rasterize_polygons_max(&band_polys, bounds);
            exact_sdf(&base_cov, w, h)
        }
    };

    let eff_sdf = perturb_shibufude_sdf_doc(&band_sdf, bounds, line_w, le.shibufude_roughness_pct, le.shibufude_seed);
    let mut cov: Vec<f32> = eff_sdf.into_iter().map(|d| (0.5 - d).clamp(0.0, 1.0)).collect();

    if !splat_polys.is_empty() {
        let splat_cov = rasterize_polygons_max(&splat_polys, bounds);
        for (c, s) in cov.iter_mut().zip(splat_cov) {
            if s > *c {
                *c = s;
            }
        }
    }

    match le.shibufude_edge_mode {
        SfxV3EdgeMode::OneBit => {
            let grain_pct = if le.line_grain_pct > 0.5 { le.line_grain_pct } else { 28.0 };
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: grain_pct,
                    sharpness_pct: 100.0,
                    seed: le.shibufude_seed,
                    core_protect_px: 1.8,
                    one_bit: true,
                },
            );
        }
        SfxV3EdgeMode::Rough => {
            let dry = le.shibufude_dryness_pct.max(le.line_grain_pct);
            apply_edge_kasure_breakup_doc(
                &mut cov,
                bounds,
                EdgeKasureParams {
                    dryness_pct: dry,
                    sharpness_pct: le.shibufude_sharpness_pct,
                    seed: le.shibufude_seed,
                    core_protect_px: 2.9,
                    one_bit: false,
                },
            );
        }
    }
    cov
}

#[cfg(test)]
mod tests {
    use super::{SHIBUFUDE_MITER_LIMIT, build_kado_base_polys};
    use crate::sfx_v3_common::VectorGlyphSdf;

    #[test]
    fn kado_miter_clip_degenerate_180_hairpin_has_zero_nan_or_inf() {
        // Construct a contour with coincident duplicate points AND a sharp ~180° hairpin spike.
        let hairpin = vec![
            (0.0, 0.0),
            (0.0, 0.0), // duplicate point
            (80.0, 0.0),
            (160.0, 0.00001), // 180° hairpin tip (u0 + u1 ~= 0 -> bl < 1e-5)
            (80.0, 0.00002),
            (80.0, 60.0),
            (0.0, 60.0),
            (0.0, 0.0), // closing duplicate
        ];
        let groups = vec![vec![hairpin]];
        let vsdf = VectorGlyphSdf::from_contour_groups(&groups);
        let polys = build_kado_base_polys(&groups, &vsdf, 8.0, 5.5, SHIBUFUDE_MITER_LIMIT);
        assert!(!polys.is_empty(), "kado builder must still emit valid band quads/joins");
        for poly in &polys {
            assert!(poly.len() >= 3);
            for &(x, y) in poly {
                assert!(x.is_finite() && y.is_finite(), "non-finite vertex ({x}, {y}) in kado miter-clip");
                assert!(x.abs() < 1e4 && y.abs() < 1e4, "exploding miter vertex ({x}, {y})");
            }
        }
    }
}
