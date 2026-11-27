//! 256-level analytic scanline polygon rasterizer (FreeType `ftgrays` / Skia `SkAnalyticEdge`
//! algorithm with exact `i32` `16.16` fixed-point accumulators and per-ribbon BBox windowing).
//!
//! Compositing each closed ribbon independently via exact 8-bit `SourceOver` guarantees:
//! * **100% tile-size & culling invariance (`0 px` stitch error)** across arbitrary tile splits
//!   and multithreaded execution (no cross-ribbon chunk boundary shifts or far-occluder splits).
//! * **BBox-only scanning (`x_min..=x_max`, `y_min..=y_max`)**: each dot/stroke touches only
//!   its own pixels instead of scanning full tile rows, dramatically accelerating 2400 DPI Swarm.

use crate::geometry::RibbonBatch;

const FP_SHIFT: u32 = 16;
const FP_ONE: f64 = (1u32 << FP_SHIFT) as f64; // 65536.0
const FP_ONE_I32: i32 = 1 << FP_SHIFT;

/// Reusable scratch buffers for the analytic scanline rasterizer.
#[derive(Default)]
pub(crate) struct AnalyticScratch {
    /// Per-pixel signed winding delta entering from the left (`(pw + 1) * ph`), in `16.16` fixed-point.
    cover: Vec<i32>,
    /// Per-pixel signed right-side area contribution (`pw * ph`), in `16.16` fixed-point.
    area: Vec<i32>,
    /// Left-of-viewport winding accumulator per scanline (`ph`), in `16.16` fixed-point.
    left_winding: Vec<i32>,
    /// High-res grayscale buffer when `ss == 2`.
    hi_gray: Vec<u8>,
}

/// Integrate one directed polygon edge `(x0, y0) -> (x1, y1)` into exact `16.16` fixed-point
/// analytic coverage buffers for viewport `[0, pw) × [0, ph)`.
#[allow(clippy::too_many_arguments)]
#[inline]
fn rasterize_edge(
    mut x0: f64,
    mut y0: f64,
    mut x1: f64,
    mut y1: f64,
    pw: usize,
    ph: usize,
    cover: &mut [i32],
    area: &mut [i32],
    left_winding: &mut [i32],
    x_min_active: &mut usize,
    x_max_active: &mut usize,
    y_min_active: &mut usize,
    y_max_active: &mut usize,
) {
    let mut dir = 1.0f64;
    if y0 > y1 {
        std::mem::swap(&mut x0, &mut x1);
        std::mem::swap(&mut y0, &mut y1);
        dir = -1.0;
    }
    let dy_total = y1 - y0;
    if dy_total <= 1e-12 || y1 <= 0.0 || y0 >= ph as f64 {
        return;
    }

    let dx_dy = (x1 - x0) / dy_total;
    if y0 < 0.0 {
        x0 += (-y0) * dx_dy;
        y0 = 0.0;
    }
    let ph_f = ph as f64;
    if y1 > ph_f {
        x1 -= (y1 - ph_f) * dx_dy;
        y1 = ph_f;
    }
    if y1 <= y0 {
        return;
    }

    let iy_start = (y0.floor() as isize).clamp(0, ph as isize - 1) as usize;
    let iy_end = ((y1 - 1e-12).floor() as isize).clamp(0, ph as isize - 1) as usize;
    if iy_start < *y_min_active {
        *y_min_active = iy_start;
    }
    if iy_end > *y_max_active {
        *y_max_active = iy_end;
    }

    let pw_f = pw as f64;
    let pw_i = pw as isize;
    let stride_cov = pw + 1;

    // Update horizontal active window for this edge
    let ex_lo = x0.min(x1);
    let ex_hi = x0.max(x1);
    if ex_hi > 0.0 && ex_lo < pw_f {
        let bx0 = (ex_lo.floor() as isize).clamp(0, pw_i - 1) as usize;
        let bx1 = (ex_hi.floor() as isize).clamp(0, pw_i - 1) as usize;
        if bx0 < *x_min_active {
            *x_min_active = bx0;
        }
        if bx1 > *x_max_active {
            *x_max_active = bx1;
        }
    } else if ex_hi <= 0.0 {
        // Touches left_winding -> scan must start at x = 0 and reach rightmost active edge
        *x_min_active = 0;
    }

    let mut cur_y = y0;
    let mut cur_x = x0;

    for (iy, left_w) in left_winding
        .iter_mut()
        .enumerate()
        .take(iy_end + 1)
        .skip(iy_start)
    {
        let next_y = ((iy + 1) as f64).min(y1);
        let seg_dy = next_y - cur_y;
        if seg_dy <= 1e-12 {
            cur_y = next_y;
            continue;
        }
        let next_x = if next_y >= y1 {
            x1
        } else {
            x0 + (next_y - y0) * dx_dy
        };

        if cur_x <= 0.0 && next_x <= 0.0 {
            let d_fp = (dir * seg_dy * FP_ONE).round() as i32;
            *left_w = left_w.wrapping_add(d_fp);
            cur_y = next_y;
            cur_x = next_x;
            continue;
        }
        if cur_x >= pw_f && next_x >= pw_f {
            cur_y = next_y;
            cur_x = next_x;
            continue;
        }

        let cov_row = iy * stride_cov;
        let area_row = iy * pw;

        let (lx0, lx1, ly0, ly1) = if cur_x <= next_x {
            (cur_x, next_x, cur_y, next_y)
        } else {
            (next_x, cur_x, next_y, cur_y)
        };
        let span_x = lx1 - lx0;
        let span_y_signed = ly1 - ly0;

        if span_x <= 1e-12 {
            let ix = lx0.floor() as isize;
            let d_fp = (dir * seg_dy * FP_ONE).round() as i32;
            if ix < 0 {
                *left_w = left_w.wrapping_add(d_fp);
            } else if ix < pw_i {
                let ux = ix as usize;
                let xm = lx0 - ix as f64;
                let a_fp = (dir * seg_dy * (1.0 - xm) * FP_ONE).round() as i32;
                area[area_row + ux] = area[area_row + ux].wrapping_add(a_fp);
                cover[cov_row + ux + 1] = cover[cov_row + ux + 1].wrapping_add(d_fp);
            }
        } else {
            let dy_dx = span_y_signed / span_x;
            let ix_min = lx0.floor() as isize;
            let ix_max = (lx1 - 1e-12).floor() as isize;

            let mut sx = lx0;
            let mut sy = ly0;
            let total_d_fp = (dir * seg_dy * FP_ONE).round() as i32;
            let mut used_d_fp: i32 = 0;

            if ix_min < 0 {
                *x_min_active = 0;
                let cx_end = lx1.min(0.0);
                let cy_end = if cx_end >= lx1 {
                    ly1
                } else {
                    ly0 + (cx_end - lx0) * dy_dx
                };
                let sub_dy = (cy_end - sy).abs();
                let d_fp = (dir * sub_dy * FP_ONE).round() as i32;
                *left_w = left_w.wrapping_add(d_fp);
                used_d_fp = used_d_fp.wrapping_add(d_fp);
                sx = cx_end;
                sy = cy_end;
            }

            let col_lo = ix_min.max(0);
            let col_hi = ix_max.min(pw_i - 1);
            for ix in col_lo..=col_hi {
                let col_right = (ix + 1) as f64;
                let cx_end = lx1.min(col_right);
                if cx_end <= sx {
                    continue;
                }
                let cy_end = if cx_end >= lx1 {
                    ly1
                } else {
                    ly0 + (cx_end - lx0) * dy_dx
                };
                let sub_dy = (cy_end - sy).abs();
                if sub_dy > 0.0 {
                    let ux = ix as usize;
                    let xm = 0.5 * (sx + cx_end) - ix as f64;
                    let is_last_seg = cx_end >= lx1;
                    let d_fp = if is_last_seg {
                        total_d_fp.wrapping_sub(used_d_fp)
                    } else {
                        let v = (dir * sub_dy * FP_ONE).round() as i32;
                        used_d_fp = used_d_fp.wrapping_add(v);
                        v
                    };
                    let a_fp = (d_fp as f64 * (1.0 - xm)).round() as i32;
                    area[area_row + ux] = area[area_row + ux].wrapping_add(a_fp);
                    cover[cov_row + ux + 1] = cover[cov_row + ux + 1].wrapping_add(d_fp);
                }
                sx = cx_end;
                sy = cy_end;
            }
        }

        cur_y = next_y;
        cur_x = next_x;
    }
}

/// Rasterize `batch` in strict painter's order (per-ribbon analytic coverage + 8-bit `SourceOver`)
/// into `gray_out` (`0` = black ink, `255` = white paper) for `[origin_x..origin_x+gw] × [origin_y..origin_y+gh]`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rasterize_batch_to_gray(
    aa: &mut AnalyticScratch,
    gray_out: &mut Vec<u8>,
    batch: &RibbonBatch,
    origin_x: i64,
    origin_y: i64,
    gw: usize,
    gh: usize,
    ss: usize,
) {
    let n_pix = gw * gh;
    if gray_out.len() < n_pix {
        gray_out.resize(n_pix, 255);
    }
    if gw == 0 || gh == 0 {
        return;
    }
    if batch.meta.is_empty() {
        gray_out[..n_pix].fill(255);
        return;
    }

    let pw = gw * ss;
    let ph = gh * ss;
    let hi_pix = pw * ph;
    let need_cov = (pw + 1) * ph;

    if aa.cover.len() < need_cov {
        aa.cover.resize(need_cov, 0);
    }
    if aa.area.len() < hi_pix {
        aa.area.resize(hi_pix, 0);
    }
    if aa.left_winding.len() < ph {
        aa.left_winding.resize(ph, 0);
    }

    let target: &mut [u8] = if ss == 1 {
        gray_out[..n_pix].fill(255);
        &mut gray_out[..n_pix]
    } else {
        if aa.hi_gray.len() < hi_pix {
            aa.hi_gray.resize(hi_pix, 255);
        }
        aa.hi_gray[..hi_pix].fill(255);
        &mut aa.hi_gray[..hi_pix]
    };

    let ox_f = origin_x as f64;
    let oy_f = origin_y as f64;
    let ss_f = ss as f64;
    let v_all = &batch.vertices;
    let stride_cov = pw + 1;

    for meta in &batch.meta {
        let b = (meta.offset as usize) * 4;
        let n = meta.point_count as usize;
        if n < 2 || b + n * 4 > v_all.len() {
            continue;
        }
        let x_first = (v_all[b] as f64 - ox_f) * ss_f;
        let y_first = (v_all[b + 1] as f64 - oy_f) * ss_f;
        if !x_first.is_finite() || !y_first.is_finite() {
            continue;
        }

        let mut x_min = pw;
        let mut x_max = 0usize;
        let mut y_min = ph;
        let mut y_max = 0usize;
        let mut prev_x = x_first;
        let mut prev_y = y_first;
        let mut has_right_clip = x_first >= pw as f64;

        for i in 1..n {
            let xi = (v_all[b + i * 4] as f64 - ox_f) * ss_f;
            let yi = (v_all[b + i * 4 + 1] as f64 - oy_f) * ss_f;
            if xi.is_finite() && yi.is_finite() {
                if xi >= pw as f64 {
                    has_right_clip = true;
                }
                rasterize_edge(
                    prev_x,
                    prev_y,
                    xi,
                    yi,
                    pw,
                    ph,
                    &mut aa.cover,
                    &mut aa.area,
                    &mut aa.left_winding,
                    &mut x_min,
                    &mut x_max,
                    &mut y_min,
                    &mut y_max,
                );
                prev_x = xi;
                prev_y = yi;
            }
        }
        for j in (0..n).rev() {
            let xj = (v_all[b + j * 4 + 2] as f64 - ox_f) * ss_f;
            let yj = (v_all[b + j * 4 + 3] as f64 - oy_f) * ss_f;
            if xj.is_finite() && yj.is_finite() {
                if xj >= pw as f64 {
                    has_right_clip = true;
                }
                rasterize_edge(
                    prev_x,
                    prev_y,
                    xj,
                    yj,
                    pw,
                    ph,
                    &mut aa.cover,
                    &mut aa.area,
                    &mut aa.left_winding,
                    &mut x_min,
                    &mut x_max,
                    &mut y_min,
                    &mut y_max,
                );
                prev_x = xj;
                prev_y = yj;
            }
        }
        rasterize_edge(
            prev_x,
            prev_y,
            x_first,
            y_first,
            pw,
            ph,
            &mut aa.cover,
            &mut aa.area,
            &mut aa.left_winding,
            &mut x_min,
            &mut x_max,
            &mut y_min,
            &mut y_max,
        );

        if y_min > y_max || y_min >= ph {
            continue;
        }
        // If polygon extends past right edge (x >= pw), winding remains non-zero up to pw - 1
        if has_right_clip {
            x_max = pw - 1;
        }
        if x_min > x_max || x_min >= pw {
            // Only left_winding may have been touched (polygon strictly left of x = 0): zero it
            for y in y_min..=y_max {
                aa.left_winding[y] = 0;
            }
            continue;
        }

        let is_occ = meta.role == 1;
        for y in y_min..=y_max {
            let cov_row = y * stride_cov;
            let area_row = y * pw;
            let mut acc: i32 = if x_min == 0 { aa.left_winding[y] } else { 0 };
            aa.left_winding[y] = 0;

            for x in x_min..=x_max {
                acc = acc.wrapping_add(aa.cover[cov_row + x]);
                aa.cover[cov_row + x] = 0;
                let a_val = aa.area[area_row + x];
                aa.area[area_row + x] = 0;

                let cov_fp = acc.wrapping_add(a_val).unsigned_abs();
                if cov_fp == 0 {
                    continue;
                }
                let idx = area_row + x;
                if cov_fp >= FP_ONE_I32 as u32 {
                    target[idx] = if is_occ { 255 } else { 0 };
                } else {
                    let alpha =
                        ((cov_fp as u64 * 255 + (FP_ONE_I32 as u64 / 2)) >> FP_SHIFT) as u32;
                    if alpha == 0 {
                        continue;
                    }
                    if alpha >= 255 {
                        target[idx] = if is_occ { 255 } else { 0 };
                    } else {
                        let inv = 255 - alpha;
                        let g_old = target[idx] as u32;
                        target[idx] = if is_occ {
                            ((g_old * inv + 255 * alpha + 127) / 255) as u8
                        } else {
                            ((g_old * inv + 127) / 255) as u8
                        };
                    }
                }
            }
            aa.cover[cov_row + x_max + 1] = 0;
        }
    }

    if ss == 2 {
        let hi = &aa.hi_gray;
        for y in 0..gh {
            let row_base = y * gw;
            let r0 = (y * 2) * pw;
            let r1 = r0 + pw;
            for x in 0..gw {
                let x2 = x * 2;
                let sum = hi[r0 + x2] as u32
                    + hi[r0 + x2 + 1] as u32
                    + hi[r1 + x2] as u32
                    + hi[r1 + x2 + 1] as u32;
                gray_out[row_base + x] = (sum >> 2) as u8;
            }
        }
    }
}
