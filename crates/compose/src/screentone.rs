//! On-demand CPU evaluation for unbaked procedural [`ScreentoneLayer`] layers.
//!
//! Layers created or restyled in GPU/Auto mode keep `surface.tile_count() == 0` so interactive
//! document edits avoid multi-megapixel CPU bakes. Document saves, exports (PNG/TIFF/ORA/PDF/PSD),
//! thumbnails, and `render_bands` evaluate those unbaked screentone layers per tile or band on the
//! fly through this module instead of reading empty surfaces.

use photocraft_algo::comic_screentone::{CustomTextureSampler, PatternType, ScreentoneMode, ScreentoneSpec, screentone_with_options, sheet_spec};
use photocraft_algo::{Ctx as AlgoCtx, Image};
use photocraft_color::{ColorMode, PixelFormat};
use photocraft_doc::{Document, Layer, LayerContent, LayerId, Rect, ScreentoneLayer};
use photocraft_raster::Surface;

use super::Buffer;
use super::content::fill_frame;
use super::ctx::Ctx;
use super::extents::{mask_vals, recycle_mask};

/// Whether `layer` is a live `Screentone` layer whose pixels must be evaluated procedurally
/// (either unbaked with 0 tiles, `reflect_layer_opacity` at `< 1.0`, or `CustomTexture`).
pub(crate) fn needs_live_eval(layer: &Layer) -> bool {
    matches!(
        &layer.content,
        LayerContent::Screentone(st)
            if st.surface.tile_count() == 0
                || (st.spec.reflect_layer_opacity && layer.opacity < 1.0)
                || st.spec.pattern_type == PatternType::CustomTexture
    )
}

/// Effective compositing opacity for `layer`: `1.0` when `reflect_layer_opacity` folds
/// `layer.opacity` into halftone dot density (and `layer.opacity > 0.0`), else `layer.opacity`.
pub(crate) fn effective_blend_opacity(layer: &Layer) -> f32 {
    if matches!(&layer.content, LayerContent::Screentone(st) if st.spec.reflect_layer_opacity) && layer.opacity > 0.0 {
        1.0
    } else {
        layer.opacity
    }
}

/// Whether `layer` is an unbaked `Shading` screentone whose luminance comes from underlying layers.
pub(crate) fn is_unbaked_shading(layer: &Layer) -> bool {
    matches!(
        &layer.content,
        LayerContent::Screentone(st)
            if needs_live_eval(layer) && st.spec.mode == ScreentoneMode::Shading && st.spec.use_source_luminance
    )
}

/// Renders an unbaked `ScreentoneLayer` into straight-alpha RGBA over `rect` without applying
/// layer masks. When `backdrop` is supplied and `st` uses source luminance in `Shading` mode,
/// `backdrop`'s pixels drive dot/halftone density.
pub(crate) fn render_unmasked(
    layer: &Layer,
    st: &ScreentoneLayer,
    rect: Rect,
    canvas: Rect,
    backdrop: Option<&Buffer>,
    patterns: &crate::pattern::PreparedPatterns<'_>,
) -> Buffer {
    if rect.is_empty() {
        return Buffer::transparent(rect);
    }
    let frame = fill_frame(layer, canvas);
    let ink_opacity = if st.spec.reflect_layer_opacity { layer.opacity.clamp(0.0, 1.0) } else { 1.0 };
    let custom_tile = if st.spec.pattern_type == PatternType::CustomTexture {
        st.spec.custom_pattern_id.as_deref().filter(|id| !id.is_empty()).and_then(|id| patterns.get(id, id))
    } else {
        None
    };
    let sampler: Option<&dyn CustomTextureSampler> = custom_tile.as_deref().map(|t| t as &dyn CustomTextureSampler);
    eval_rgba(&st.spec, st.origin, rect, frame, backdrop, sampler, ink_opacity)
}

/// Same as [`render_unmasked`], plus multiplies the layer's pixel/vector masks into alpha.
/// When `backdrop` is supplied for a `Shading` screentone whose `halo_radius() > 0` (e.g.
/// `preserve_lines = true`), expands the backdrop sample by `halo_radius()` clamped to the canvas
/// so 256×256 tile and band boundaries stay bit-exact with a monolithic render.
pub(crate) fn render_masked(layer: &Layer, st: &ScreentoneLayer, rect: Rect, cx: &Ctx, backdrop: Option<&Buffer>) -> Buffer {
    let halo = st.spec.halo_radius();
    let padded = if halo > 0
        && let Some(bd) = backdrop
        && let Some(doc) = cx.doc
    {
        let sample_rect = rect.inflate(halo).intersect(&cx.canvas);
        if sample_rect != bd.rect && !sample_rect.is_empty() {
            let mut below = doc.clone();
            hide_at_and_above(&mut below.layers, layer.id);
            let tile_side = sample_rect.width().max(sample_rect.height()).max(1) as i32;
            Some(crate::render_tiled(&below, sample_rect, tile_side))
        } else {
            None
        }
    } else {
        None
    };
    let mut buf = render_unmasked(layer, st, rect, cx.canvas, padded.as_ref().or(backdrop), cx.patterns);
    if let Some(m) = mask_vals(layer, rect, cx) {
        for (p, k) in buf.px.iter_mut().zip(&m) {
            p[3] *= k;
        }
        recycle_mask(Some(m));
    }
    buf
}

fn eval_rgba(
    spec_in: &ScreentoneSpec,
    origin: (f32, f32),
    rect: Rect,
    frame: Rect,
    backdrop: Option<&Buffer>,
    custom_tex: Option<&dyn CustomTextureSampler>,
    ink_opacity: f32,
) -> Buffer {
    let mut spec = sheet_spec(spec_in, frame, origin);
    let src_img = match backdrop.filter(|b| spec.mode == ScreentoneMode::Shading && spec.use_source_luminance && !b.rect.is_empty()) {
        Some(bd) => {
            let mut data = Vec::with_capacity(bd.px.len() * 4);
            for p in &bd.px {
                data.extend_from_slice(p);
            }
            Image { rect: bd.rect, ch: 4, data }
        }
        None => {
            spec.use_source_luminance = false;
            Image { rect: Rect::EMPTY, ch: 4, data: Vec::new() }
        }
    };
    let algo_ctx = AlgoCtx { bounds: frame, mode: ColorMode::Rgb, alpha: true };
    let out = screentone_with_options(&src_img, rect, &algo_ctx, &spec, custom_tex, ink_opacity);
    let mut buf = Buffer::transparent(rect);
    for (dst, src) in buf.px.iter_mut().zip(out.chunks_exact(4)) {
        *dst = [src[0], src[1], src[2], src[3]];
    }
    buf
}

/// Scales spatial pattern dimensions in `spec` when downsampling a proxy/reduced document by `1/k`.
pub(crate) fn scale_spec_for_downsample(spec: &mut ScreentoneSpec, inv_k: f32) {
    spec.spacing_x = (spec.spacing_x * inv_k).clamp(1.0, 2000.0);
    spec.spacing_y = (spec.spacing_y * inv_k).clamp(1.0, 2000.0);
    spec.dot_size = (spec.dot_size * inv_k).clamp(0.0, 500.0);
    spec.line_width = (spec.line_width * inv_k).clamp(0.0, 200.0);
    spec.satellite_size = (spec.satellite_size * inv_k).clamp(0.0, 500.0);
    spec.satellite_distance = (spec.satellite_distance * inv_k).clamp(0.0, 2000.0);
    spec.focus_inner_radius = (spec.focus_inner_radius * inv_k).clamp(0.0, 2000.0);
    spec.origin_x *= inv_k;
    spec.origin_y *= inv_k;
}

/// Hides `target` and every layer above it in bottom-to-top draw order so [`crate::render`] yields
/// the exact backdrop beneath `target` for `Shading` screentones.
fn hide_at_and_above(layers: &mut [Layer], target: LayerId) -> bool {
    let mut found = false;
    for l in layers.iter_mut() {
        if found {
            l.visible = false;
            continue;
        }
        if l.id == target {
            l.visible = false;
            found = true;
            continue;
        }
        if let LayerContent::Group(g) = &mut l.content
            && hide_at_and_above(&mut g.children, target)
        {
            found = true;
        }
    }
    found
}

/// Whether a single pixel in `fmt` carries non-white tonal ink that can drive `Shading` dots.
fn pixel_has_tonal_ink(px: &[f32], fmt: PixelFormat) -> bool {
    let ch = fmt.channels();
    if ch == 0 || px.len() < ch {
        return false;
    }
    if fmt.alpha && px[ch - 1] <= 1e-3 {
        return false;
    }
    let color_ch = fmt.mode.color_channels();
    match fmt.mode {
        ColorMode::Grayscale | ColorMode::Bitmap => 1.0 - px[0].clamp(0.0, 1.0) > 1e-3,
        ColorMode::Cmyk if color_ch >= 4 => px[0] > 1e-3 || px[1] > 1e-3 || px[2] > 1e-3 || px[3] > 1e-3,
        ColorMode::Lab if color_ch >= 3 => 1.0 - px[0].clamp(0.0, 1.0) > 1e-3,
        _ if color_ch >= 3 => {
            let l = (0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]).clamp(0.0, 1.0);
            1.0 - l > 1e-3
        }
        _ => 1.0 - px[0].clamp(0.0, 1.0) > 1e-3,
    }
}

/// Whether a straight-alpha RGBA `Buffer` already contains any non-white tonal ink.
pub(crate) fn buffer_has_tonal_ink(buf: &Buffer) -> bool {
    buf.px.iter().any(|p| {
        p[3] > 1e-3 && {
            let l = (0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).clamp(0.0, 1.0);
            1.0 - l > 1e-3
        }
    })
}

/// Tile-by-tile ink check on `surf` (O(1) memory, short-circuits on the first inked pixel).
fn surface_has_tonal_ink(surf: &Surface) -> bool {
    let fmt = surf.format();
    let ch = fmt.channels();
    if ch == 0 {
        return false;
    }
    if pixel_has_tonal_ink(&surf.default_pixel(), fmt) {
        return true;
    }
    if surf.tile_count() == 0 {
        return false;
    }
    let mut tile_buf = Vec::new();
    for (tc, _) in surf.tiles() {
        surf.read_region_into(tc.rect(), &mut tile_buf);
        if tile_buf.chunks_exact(ch).any(|px| pixel_has_tonal_ink(px, fmt)) {
            return true;
        }
    }
    false
}

/// Whether `layer` has any visible non-white tonal content (non-screentone) that can supply a
/// backdrop for a `Shading` screentone above it.
fn layer_has_backdrop_content(layer: &Layer) -> bool {
    if !layer.visible || layer.opacity <= 0.0 {
        return false;
    }
    match &layer.content {
        LayerContent::Screentone(_) | LayerContent::Kakeami(_) => false,
        LayerContent::Raster(s) => surface_has_tonal_ink(s),
        LayerContent::Group(g) => g.children.iter().any(layer_has_backdrop_content),
        LayerContent::Adjustment(_) => false,
        LayerContent::Fill(_) | LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_) => true,
    }
}

/// Returns `true` when at least one visible non-screentone layer beneath `target` in bottom-to-top
/// draw order carries non-empty content. When `false` (e.g. a brand-new blank transparent canvas),
/// `Shading` screentones fall back to uniform sheet evaluation instead of rendering empty.
pub(crate) fn has_visible_backdrop_below(layers: &[Layer], target: LayerId) -> bool {
    fn walk(layers: &[Layer], target: LayerId, found_content: &mut bool) -> bool {
        for l in layers {
            if l.id == target {
                return true;
            }
            if !l.visible || l.opacity <= 0.0 {
                continue;
            }
            if let LayerContent::Group(g) = &l.content {
                if walk(&g.children, target, found_content) {
                    return true;
                }
            } else if layer_has_backdrop_content(l) {
                *found_content = true;
            }
        }
        false
    }
    let mut found = false;
    let _ = walk(layers, target, &mut found);
    found
}

/// Renders an unbaked `ScreentoneLayer` into an owned [`Surface`] in `fmt` for layered export (e.g.
/// PSD), sampling the layers below `layer` when `mode == Shading && use_source_luminance`.
pub fn render_unbaked_for_export(doc: &Document, layer: &Layer, st: &ScreentoneLayer, fmt: PixelFormat) -> Surface {
    let canvas = doc.bounds();
    let frame = fill_frame(layer, canvas);
    let mut surf = Surface::new(fmt);
    if frame.is_empty() {
        return surf;
    }

    let needs_backdrop = st.spec.mode == ScreentoneMode::Shading
        && st.spec.use_source_luminance
        && has_visible_backdrop_below(&doc.layers, layer.id);
    let below_doc = if needs_backdrop {
        let mut d = doc.clone();
        hide_at_and_above(&mut d.layers, layer.id);
        Some(d)
    } else {
        None
    };

    let prepared = crate::pattern::PreparedPatterns::new(&doc.patterns, crate::pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    let custom_tile = if st.spec.pattern_type == PatternType::CustomTexture {
        st.spec.custom_pattern_id.as_deref().filter(|id| !id.is_empty()).and_then(|id| prepared.get(id, id))
    } else {
        None
    };
    let sampler: Option<&dyn CustomTextureSampler> = custom_tile.as_deref().map(|t| t as &dyn CustomTextureSampler);
    let ink_opacity = if st.spec.reflect_layer_opacity { layer.opacity.clamp(0.0, 1.0) } else { 1.0 };

    let band_h = crate::render::band_rows_for(frame.width(), 0).max(1);
    let ch = fmt.channels();
    let mut raw = Vec::new();
    let mut y = frame.y0;
    while y < frame.y1 {
        let y1 = y.saturating_add(band_h).min(frame.y1);
        let band = Rect::new(frame.x0, y, frame.x1, y1);
        let backdrop = below_doc.as_ref().and_then(|d| {
            let sample_rect = band.inflate(st.spec.halo_radius()).intersect(&canvas);
            (!sample_rect.is_empty()).then(|| crate::render(d, sample_rect))
        });
        let rgba = eval_rgba(&st.spec, st.origin, band, frame, backdrop.as_ref(), sampler, ink_opacity);
        raw.resize(rgba.px.len() * ch, 0.0);
        for (dst, &px) in raw.chunks_exact_mut(ch).zip(&rgba.px) {
            photocraft_raster::from_rgba_into(&fmt, px, dst);
        }
        surf.write_region(band, &raw);
        y = y1;
    }
    surf.prune();
    surf
}

#[cfg(test)]
mod tests {
    use photocraft_color::{ColorMode, PixelFormat, SampleType};
    use photocraft_doc::{Document, Layer, LayerContent, LayerMask, PatternType, Rect, ScreentoneBackend, ScreentoneLayer, ScreentoneMode, ScreentoneSpec, Size};
    use photocraft_raster::Surface;

    #[test]
    fn unbaked_screentone_sheet_and_shading_render_through_compose() {
        let mut doc = Document::new("tone_compose", Size::new(64, 64), ColorMode::Rgb, SampleType::U8);
        let mut bg = Surface::new(PixelFormat::RGBA8);
        let bounds = doc.bounds();
        bg.fill_rect(Rect::new(0, 0, 32, 64), &[0.2, 0.2, 0.2, 1.0]);
        bg.fill_rect(Rect::new(32, 0, 64, 64), &[0.95, 0.95, 0.95, 1.0]);
        doc.layers.push(Layer::new("bg", LayerContent::Raster(bg)));

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            spacing_x: 8.0,
            spacing_y: 8.0,
            dot_size: 3.5,
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            preserve_lines: false,
            backend: ScreentoneBackend::Gpu,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = photocraft_algo::comic_screentone::origin_doc(&spec, bounds);
        let unbaked = ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8));
        let mut tone_layer = Layer::new("tone", LayerContent::Screentone(unbaked));
        tone_layer.blend = photocraft_color::BlendMode::Multiply;
        doc.layers.push(tone_layer);

        let buf = crate::render(&doc, bounds);
        let left_dark_pixels = buf.px.chunks_exact(64).flat_map(|row| &row[..32]).filter(|p| p[0] < 0.05).count();
        let right_dark_pixels = buf.px.chunks_exact(64).flat_map(|row| &row[32..]).filter(|p| p[0] < 0.05).count();
        assert!(left_dark_pixels > 100, "dark left half must receive shading dots, got {left_dark_pixels}");
        assert!(right_dark_pixels == 0, "white right half must receive no dots, got {right_dark_pixels}");
    }

    #[test]
    fn unbaked_screentone_export_honors_mask_frame_and_backdrop() {
        let mut doc = Document::new("tone_export", Size::new(64, 64), ColorMode::Rgb, SampleType::U8);
        let mut bg = Surface::new(PixelFormat::RGBA8);
        bg.fill_rect(Rect::new(0, 0, 64, 64), &[0.3, 0.3, 0.3, 1.0]);
        doc.layers.push(Layer::new("bg", LayerContent::Raster(bg)));

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            spacing_x: 8.0,
            spacing_y: 8.0,
            dot_size: 3.5,
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            backend: ScreentoneBackend::Gpu,
            ..Default::default()
        };
        let mut mask = LayerMask::hide_all();
        mask.surface.fill_rect(Rect::new(8, 8, 40, 40), &[1.0]);
        let st = ScreentoneLayer::new(spec, (24.0, 24.0), Surface::new(PixelFormat::RGBA8));
        let st_clone = st.clone();
        let mut layer = Layer::new("tone", LayerContent::Screentone(st));
        layer.mask = Some(mask);
        let layer_clone = layer.clone();
        doc.layers.push(layer);

        let exported = super::render_unbaked_for_export(&doc, &layer_clone, &st_clone, PixelFormat::RGBA8);
        assert!(!exported.content_bounds().is_empty());
        assert!(exported.content_bounds().x0 >= 8 && exported.content_bounds().x1 <= 40);
        assert!(exported.content_bounds().y0 >= 8 && exported.content_bounds().y1 <= 40);
    }

    #[test]
    fn preserve_lines_render_bands_matches_monolithic_on_seams() {
        let mut doc = Document::new("seam_check", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        let bounds = doc.bounds();
        let mut bg = Surface::new(PixelFormat::RGBA8);
        // White paper with mid-gray shading field and crisp dark lineart crossing x=256 and y=256.
        bg.fill_rect(bounds, &[0.95, 0.95, 0.95, 1.0]);
        bg.fill_rect(Rect::new(64, 64, 448, 448), &[0.55, 0.55, 0.55, 1.0]);
        // Horizontal and vertical ink strokes straddling y=256 and x=256.
        bg.fill_rect(Rect::new(40, 254, 472, 259), &[0.02, 0.02, 0.02, 1.0]);
        bg.fill_rect(Rect::new(254, 40, 259, 472), &[0.02, 0.02, 0.02, 1.0]);
        // Diagonal ink stroke crossing (256, 256).
        for i in 120..392 {
            bg.fill_rect(Rect::new(i, i, (i + 3).min(512), (i + 3).min(512)), &[0.03, 0.03, 0.03, 1.0]);
        }
        doc.layers.push(Layer::new("art", LayerContent::Raster(bg)));

        let spec = ScreentoneSpec {
            pattern_type: PatternType::Dots,
            spacing_x: 12.0,
            spacing_y: 12.0,
            dot_size: 4.5,
            mode: ScreentoneMode::Shading,
            use_source_luminance: true,
            preserve_lines: true,
            line_thickness: 5.0,
            line_threshold: 0.25,
            backend: ScreentoneBackend::Gpu,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            ..Default::default()
        };
        let origin = photocraft_algo::comic_screentone::origin_doc(&spec, bounds);
        let unbaked = ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8));
        let mut tone_layer = Layer::new("tone", LayerContent::Screentone(unbaked));
        tone_layer.blend = photocraft_color::BlendMode::Multiply;
        doc.layers.push(tone_layer);

        // Monolithic single-tile reference (tile = 512).
        let mono = crate::render_tiled(&doc, bounds, 512);

        // Banded render with band height <= 256 (internal 256×256 tiles across x=256 and y=256).
        let mut banded_px = vec![[0.0f32; 4]; 512 * 512];
        let mut band_count = 0usize;
        crate::render_bands(&doc, bounds, 256, |band| -> Result<(), ()> {
            assert!(band.rect.height() <= 256, "expected band height <= 256, got {}", band.rect.height());
            band_count += 1;
            let bw = band.rect.width() as usize;
            for ry in 0..band.rect.height() as usize {
                let dst_y = (band.rect.y0 as usize + ry) * 512 + band.rect.x0 as usize;
                let src_y = ry * bw;
                banded_px[dst_y..dst_y + bw].copy_from_slice(&band.px[src_y..src_y + bw]);
            }
            Ok(())
        })
        .unwrap();
        assert!(band_count >= 2, "expected >= 2 bands, got {band_count}");

        let mut total_diff = 0usize;
        let mut seam_diff = 0usize;
        for y in 0..512usize {
            for x in 0..512usize {
                let idx = y * 512 + x;
                let a = mono.px[idx];
                let b = banded_px[idx];
                if (0..4).any(|c| a[c].to_bits() != b[c].to_bits()) {
                    total_diff += 1;
                    if (254..=258).contains(&x) || (254..=258).contains(&y) {
                        seam_diff += 1;
                    }
                }
            }
        }
        assert_eq!(seam_diff, 0, "preserve_lines band/tile seam mismatch at x=256/y=256: {seam_diff} seam px ({total_diff} total)");
        assert_eq!(total_diff, 0, "preserve_lines banded render must match monolithic bit-for-bit, got {total_diff} differing px");
    }

    #[test]
    fn block_b_wobble_dash_kezuri_render_bands_zero_seam_at_256() {
        let mut doc = Document::new("block_b_seam", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        let bounds = doc.bounds();
        let spec = ScreentoneSpec {
            pattern_type: PatternType::Lines,
            spacing_x: 10.0,
            spacing_y: 10.0,
            line_width: 2.8,
            angle: 19.0,
            density: 0.9,
            smoothness: 0.5,
            jitter_pos: 30.0,
            jitter_size: 25.0,
            line_wobble: 0.6,
            line_wave_freq: 2.2,
            line_dash: 0.5,
            scrape_strength: 0.55,
            scrape_angle: 42.0,
            mode: ScreentoneMode::Sheet,
            use_source_luminance: false,
            backend: ScreentoneBackend::Gpu,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            seed: 9001,
            ..Default::default()
        };
        let origin = photocraft_algo::comic_screentone::origin_doc(&spec, bounds);
        let unbaked = ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8));
        doc.layers.push(Layer::new("tone_b", LayerContent::Screentone(unbaked)));

        let mono = crate::render_tiled(&doc, bounds, 512);
        let mut banded_px = vec![[0.0f32; 4]; 512 * 512];
        crate::render_bands(&doc, bounds, 256, |band| -> Result<(), ()> {
            let bw = band.rect.width() as usize;
            for ry in 0..band.rect.height() as usize {
                let dst_y = (band.rect.y0 as usize + ry) * 512 + band.rect.x0 as usize;
                let src_y = ry * bw;
                banded_px[dst_y..dst_y + bw].copy_from_slice(&band.px[src_y..src_y + bw]);
            }
            Ok(())
        })
        .unwrap();

        let mut seam_diff = 0usize;
        let mut total_diff = 0usize;
        for y in 0..512usize {
            for x in 0..512usize {
                let idx = y * 512 + x;
                if (0..4).any(|c| mono.px[idx][c].to_bits() != banded_px[idx][c].to_bits()) {
                    total_diff += 1;
                    if (254..=258).contains(&x) || (254..=258).contains(&y) {
                        seam_diff += 1;
                    }
                }
            }
        }
        assert_eq!(seam_diff, 0, "Block B render_bands seam diff at x=256/y=256: {seam_diff}");
        assert_eq!(total_diff, 0, "Block B render_bands must match monolithic bit-for-bit, got {total_diff}");
    }

    #[test]
    fn block_c_sand_noise_render_bands_zero_seam_at_256() {
        let mut doc = Document::new("block_c_sand_seam", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        let bounds = doc.bounds();
        let spec = ScreentoneSpec {
            pattern_type: PatternType::SandNoise,
            spacing_x: 12.0,
            spacing_y: 12.0,
            angle: 21.0,
            density: 0.58,
            smoothness: 0.45,
            sand_aspect_x: 3.2,
            sand_aspect_y: 0.65,
            sand_octaves_mix: 0.4,
            scrape_strength: 0.3,
            scrape_angle: 35.0,
            mode: ScreentoneMode::Sheet,
            use_source_luminance: false,
            backend: ScreentoneBackend::Gpu,
            foreground: [0.0, 0.0, 0.0, 1.0],
            background: [1.0, 1.0, 1.0, 0.0],
            seed: 4040,
            ..Default::default()
        };
        let origin = photocraft_algo::comic_screentone::origin_doc(&spec, bounds);
        let unbaked = ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8));
        doc.layers.push(Layer::new("sand_c", LayerContent::Screentone(unbaked)));

        let mono = crate::render_tiled(&doc, bounds, 512);
        let mut banded_px = vec![[0.0f32; 4]; 512 * 512];
        crate::render_bands(&doc, bounds, 256, |band| -> Result<(), ()> {
            let bw = band.rect.width() as usize;
            for ry in 0..band.rect.height() as usize {
                let dst_y = (band.rect.y0 as usize + ry) * 512 + band.rect.x0 as usize;
                let src_y = ry * bw;
                banded_px[dst_y..dst_y + bw].copy_from_slice(&band.px[src_y..src_y + bw]);
            }
            Ok(())
        })
        .unwrap();

        let mut seam_diff = 0usize;
        let mut total_diff = 0usize;
        for y in 0..512usize {
            for x in 0..512usize {
                let idx = y * 512 + x;
                if (0..4).any(|c| mono.px[idx][c].to_bits() != banded_px[idx][c].to_bits()) {
                    total_diff += 1;
                    if (254..=258).contains(&x) || (254..=258).contains(&y) {
                        seam_diff += 1;
                    }
                }
            }
        }
        assert_eq!(seam_diff, 0, "Block C SandNoise render_bands seam diff at x=256/y=256: {seam_diff}");
        assert_eq!(total_diff, 0, "Block C SandNoise render_bands must match monolithic bit-for-bit, got {total_diff}");
    }

    #[test]
    fn block_c_reflect_layer_opacity_keeps_full_black_ink_and_shrinks_dots() {
        let make_doc = |opacity: f32, reflect: bool| {
            let mut doc = Document::new("reflect_opacity", Size::new(96, 96), ColorMode::Rgb, SampleType::U8);
            let bounds = doc.bounds();
            let spec = ScreentoneSpec {
                pattern_type: PatternType::Dots,
                dot_shape: photocraft_doc::DotShape::Euclidean,
                spacing_x: 12.0,
                spacing_y: 12.0,
                density: 0.75,
                smoothness: 0.0,
                reflect_layer_opacity: reflect,
                mode: ScreentoneMode::Sheet,
                use_source_luminance: false,
                backend: ScreentoneBackend::Gpu,
                foreground: [0.0, 0.0, 0.0, 1.0],
                background: [1.0, 1.0, 1.0, 0.0],
                ..Default::default()
            };
            let origin = photocraft_algo::comic_screentone::origin_doc(&spec, bounds);
            let mut layer = Layer::new("tone", LayerContent::Screentone(ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8))));
            layer.opacity = opacity;
            doc.layers.push(layer);
            doc
        };

        let bounds = Rect::new(0, 0, 96, 96);
        let full = crate::render(&make_doc(1.0, true), bounds);
        let reflected = crate::render(&make_doc(0.35, true), bounds);
        let normal = crate::render(&make_doc(0.35, false), bounds);

        let full_ink = full.px.iter().filter(|p| p[3] > 0.99).count();
        let refl_ink = reflected.px.iter().filter(|p| p[3] > 0.99).count();
        let refl_translucent = reflected.px.iter().filter(|p| p[3] > 0.01 && p[3] < 0.99).count();
        let normal_translucent = normal.px.iter().filter(|p| p[3] > 0.25 && p[3] < 0.45).count();

        assert_eq!(refl_translucent, 0, "reflect_layer_opacity=true must have ZERO translucent ~0.35 ink pixels");
        assert!(refl_ink > 500, "reflect_layer_opacity=true at 35% must still draw solid black dots, got {refl_ink}");
        assert!(refl_ink * 2 < full_ink, "reflect_layer_opacity=true at 35% must shrink dot coverage vs 100%: {refl_ink} vs {full_ink}");
        assert!(normal_translucent > 500, "reflect_layer_opacity=false at 35% must composite translucent ~0.35 pixels, got {normal_translucent}");
    }
}

