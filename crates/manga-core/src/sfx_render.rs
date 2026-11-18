//! CPU oracle & offscreen renderer for Manga SFX layers (`L3`, wasm-pure).
//!
//! Renders [`SfxData`] directly into [`Surface`] in any [`PixelFormat`] using
//! `photocraft-vector`'s exact area-coverage rasterizer and `photocraft-algo`'s
//! procedural screentone sheet generator. Zero edits required in `compose` or `gpu`.

use photocraft_algo::comic_screentone::{ScreentoneMode, render_screentone_to_surface};
use photocraft_color::PixelFormat;
use photocraft_doc::sfx::{SfxData, SfxFillSource, SfxLineJoin, SfxStylePass};
use photocraft_doc::{FillRule, LineCap, LineJoin, PathOp};
use photocraft_geom::Rect;
use photocraft_raster::{Surface, from_rgba_into};
use photocraft_text::TextEngine;
use photocraft_vector::{Polyline, Rasterizer, StrokeStyle, stroke_polygons};

use crate::sfx_geom::{PreparedSfx, build_sfx_geometry};

const MAX_RENDER_EXTENT: i32 = 8192;
const MAX_RENDER_PIXELS: usize = 16 * 1024 * 1024;

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
    let mut stroke_pad: f64 = 0.0;
    let mut min_dx: f64 = 0.0;
    let mut max_dx: f64 = 0.0;
    let mut min_dy: f64 = 0.0;
    let mut max_dy: f64 = 0.0;
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
    let pad = stroke_pad + 6.0;
    let x0 = (geom.warped_box[0] + min_dx - pad).floor().clamp(-1e6, 1e6) as i32;
    let y0 = (geom.warped_box[1] + min_dy - pad).floor().clamp(-1e6, 1e6) as i32;
    let x1 = (geom.warped_box[2] + max_dx + pad).ceil().clamp(-1e6, 1e6) as i32;
    let y1 = (geom.warped_box[3] + max_dy + pad).ceil().clamp(-1e6, 1e6) as i32;
    if x1 <= x0 || y1 <= y0 {
        return Rect::EMPTY;
    }
    let w = (x1 - x0).min(MAX_RENDER_EXTENT);
    let h = (y1 - y0).min(MAX_RENDER_EXTENT);
    let area = (w as usize).saturating_mul(h as usize);
    if area > MAX_RENDER_PIXELS {
        return Rect::EMPTY;
    }
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

fn eval_fill_rgba(fill: &SfxFillSource, rect: Rect) -> Vec<[f32; 4]> {
    let n = rect.width() as usize * rect.height() as usize;
    match fill {
        SfxFillSource::Solid { color } => {
            let rgb = color.to_rgb();
            vec![[rgb[0], rgb[1], rgb[2], color.alpha]; n]
        }
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
                dst[row + x] = (sum / win).clamp(0.0, 1.0);
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
                dst[y * w + x] = (sum / win).clamp(0.0, 1.0);
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

fn render_shadow_passes(data: &SfxData, geom: &PreparedSfx, bounds: Rect, w: usize, h: usize, under_rgba: &mut [[f32; 4]]) {
    for pass in &data.styles {
        if let SfxStylePass::Shadow { enabled: true, dx_px, dy_px, blur_px, color, opacity } = pass {
            let sr = fill_rasterizer_from_polys(&geom.polylines, [f64::from(*dx_px), f64::from(*dy_px)]);
            let scov = blur_coverage(&sr.render(bounds), w, h, *blur_px);
            let rgb = color.to_rgb();
            paint_solid_over(under_rgba, &scov, [rgb[0], rgb[1], rgb[2], color.alpha], *opacity);
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
            let rgb = color.to_rgb();
            paint_solid_over(under_rgba, &glow_cov, [rgb[0], rgb[1], rgb[2], color.alpha], *opacity);
        }
    }
}

/// Renders an [`SfxData`] non-destructively into offscreen document [`Surface`]`(s)`.
pub fn render_sfx(data: &SfxData, engine: &mut TextEngine, format: PixelFormat, dpi: f32) -> SfxRenderOutput {
    let geom = build_sfx_geometry(data, engine, dpi);
    let bounds = compute_render_bounds(data, &geom);
    if bounds.is_empty() {
        let s = Surface::new(format);
        return if data.has_negative_invert() {
            SfxRenderOutput::InvertPair { under: s.clone(), invert: s, bounds: Rect::EMPTY, geom }
        } else {
            SfxRenderOutput::Single { surface: s, bounds: Rect::EMPTY, geom }
        };
    }

    let w = bounds.width() as usize;
    let h = bounds.height() as usize;
    let n = w * h;
    let mut under_rgba = vec![[0.0_f32; 4]; n];

    let base_ras = fill_rasterizer_from_polys(&geom.polylines, [0.0, 0.0]);
    let mut base_cov = base_ras.render(bounds);
    apply_grain_distress(data, &mut base_cov, bounds, w);
    render_shadow_passes(data, &geom, bounds, w, h, &mut under_rgba);
    render_extrude_passes(data, &geom, bounds, &mut under_rgba);
    render_stroke_passes(data, &geom, bounds, &mut under_rgba);

    if data.has_negative_invert() {
        let mut invert_rgba = vec![[0.0_f32; 4]; n];
        for (o, &c) in invert_rgba.iter_mut().zip(base_cov.iter()) {
            let ca = c.clamp(0.0, 1.0);
            if ca > 1e-6 {
                *o = [1.0, 1.0, 1.0, ca];
            }
        }
        let under = rgba_to_surface(&under_rgba, bounds, format);
        let invert = rgba_to_surface(&invert_rgba, bounds, format);
        SfxRenderOutput::InvertPair { under, invert, bounds, geom }
    } else {
        let face_paint = eval_fill_rgba(&data.fill, bounds);
        paint_over(&mut under_rgba, &base_cov, &face_paint);
        render_inner_glow_passes(data, &base_cov, w, h, &mut under_rgba);
        let surface = rgba_to_surface(&under_rgba, bounds, format);
        SfxRenderOutput::Single { surface, bounds, geom }
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
}
