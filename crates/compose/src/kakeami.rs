//! CPU rendering, LRU `PreparedScene` cache, `512² → 4×256²` `GRAY8` tile caching, and banded
//! export for [`KakeamiLayer`] (`LayerContent::Kakeami`).
//!
//! Polarity contract:
//! * `kakeami-core` outputs grayscale luminance (`0 = ink`, `255 = paper`).
//! * PhotoCraft's `GRAY8` alpha cache and straight-alpha compositors use `alpha = 255 - gray`
//!   (`255 = ink`, `0 = transparent`), tinted with `layer.color` (`[r, g, b, a * alpha]`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use kakeami_core::{KakeamiConfig, PreparedScene, RasterScratch, Rect as KRect, render_tile_gray_into};
use photocraft_color::PixelFormat;
use photocraft_doc::{Document, KakeamiLayer, Layer, LayerId, Rect};
use photocraft_geom::{TILE_SIZE, TileCoord};
use photocraft_raster::Surface;

use crate::Buffer;
use crate::content::fill_frame;

/// Render super-block edge (512 px = 2×2 PhotoCraft `256×256` tiles).
pub const KAKEAMI_BLOCK_SIZE: i32 = TILE_SIZE * 2;
/// Maximum cached `PreparedScene` entries in the process-wide LRU.
const SCENE_LRU_CAP: usize = 12;
/// Maximum cached derived `GRAY8` alpha surfaces for GPU reads when `k.cache` is `None`.
const SURFACE_LRU_CAP: usize = 4;
/// Maximum `GRAY8` cache pixels built automatically on the GPU read path (64 MiB at 1 B/px).
const MAX_AUTO_CACHE_PIXELS: i64 = 64_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SceneKey {
    layer_id: LayerId,
    config_hash: u32,
    scaled_dpi_bits: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SurfaceKey {
    layer_id: LayerId,
    config_hash: u32,
    scaled_dpi_bits: u32,
    frame: [i32; 4],
    origin_bits: [u32; 2],
}

static SCENE_CACHE: LazyLock<Mutex<VecDeque<(SceneKey, Arc<PreparedScene>)>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));
static SURFACE_CACHE: LazyLock<Mutex<VecDeque<(SurfaceKey, Arc<Surface>)>>> = LazyLock::new(|| Mutex::new(VecDeque::new()));

/// Effective document-space bounding rect for rendering `k`.
pub fn effective_frame(layer: &Layer, k: &KakeamiLayer, canvas: Rect) -> Rect {
    if let Some(f) = k.fill_frame {
        let clipped = f.intersect(&canvas);
        if !clipped.is_empty() {
            return clipped;
        }
    }
    fill_frame(layer, canvas)
}

/// Builds the effective `KakeamiConfig` for `frame` and `doc_dpi` (applying `dpi_sync` relative to
/// 600 DPI when enabled).
pub fn effective_config(k: &KakeamiLayer, frame: Rect, doc_dpi: f32) -> (KakeamiConfig, f32) {
    let mut cfg = k.config.clone();
    cfg.width = frame.width().max(1) as u32;
    cfg.height = frame.height().max(1) as u32;
    let effective_dpi = if k.dpi_sync && doc_dpi.is_finite() && doc_dpi > 0.0 {
        let factor = f64::from(doc_dpi) / 600.0;
        cfg.nawa_scale = (cfg.nawa_scale * factor).clamp(0.25, 8.0);
        doc_dpi
    } else {
        600.0
    };
    (cfg, effective_dpi)
}

/// Looks up or builds a `PreparedScene` from the bounded thread-safe LRU cache.
pub fn prepared_scene(layer_id: LayerId, cfg: &KakeamiConfig, effective_dpi: f32) -> Arc<PreparedScene> {
    let key = SceneKey { layer_id, config_hash: cfg.config_hash(), scaled_dpi_bits: effective_dpi.to_bits() };
    {
        let mut cache = SCENE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = cache.iter().position(|(k, _)| *k == key)
            && let Some(entry) = cache.remove(pos)
        {
            let scene = Arc::clone(&entry.1);
            cache.push_back(entry);
            return scene;
        }
    }
    // Prepare outside the lock so concurrent tiles/layers never block each other.
    let built = Arc::new(PreparedScene::prepare(cfg));
    let mut cache = SCENE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = cache.iter().position(|(k, _)| *k == key) {
        if let Some(entry) = cache.remove(pos) {
            let scene = Arc::clone(&entry.1);
            cache.push_back(entry);
            return scene;
        }
    }
    if cache.len() >= SCENE_LRU_CAP {
        cache.pop_front();
    }
    cache.push_back((key, Arc::clone(&built)));
    built
}

/// Renders `k` into a `PixelFormat::GRAY8` alpha `Surface` (`0 = transparent`, `255 = ink`, 1 B/px)
/// over `frame` using `512×512` super-blocks mapped directly to up to `4 × 256²` tiles.
///
/// Never allocates a full-canvas buffer. Returns `None` if `cancel` is signalled mid-render.
pub fn render_gray8_surface(
    layer_id: LayerId,
    k: &KakeamiLayer,
    frame: Rect,
    doc_dpi: f32,
    cancel: Option<&AtomicBool>,
) -> Option<Surface> {
    let mut surface = Surface::new(PixelFormat::GRAY8);
    if frame.is_empty() {
        return Some(surface);
    }
    let (cfg, effective_dpi) = effective_config(k, frame, doc_dpi);
    let scene = prepared_scene(layer_id, &cfg, effective_dpi);
    let mut scratch = RasterScratch::new();
    let mut block_buf = vec![0u8; (KAKEAMI_BLOCK_SIZE as usize) * (KAKEAMI_BLOCK_SIZE as usize)];

    let tx0 = frame.x0.div_euclid(TILE_SIZE);
    let ty0 = frame.y0.div_euclid(TILE_SIZE);
    let tx1 = (frame.x1 - 1).div_euclid(TILE_SIZE) + 1;
    let ty1 = (frame.y1 - 1).div_euclid(TILE_SIZE) + 1;

    // Step by 2 tiles (512 px) so each kakeami-core call covers up to 4 PhotoCraft 256×256 tiles.
    let mut ty = ty0;
    while ty < ty1 {
        let mut tx = tx0;
        while tx < tx1 {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let block_rect = Rect::new(
                tx * TILE_SIZE,
                ty * TILE_SIZE,
                (tx + 2).min(tx1) * TILE_SIZE,
                (ty + 2).min(ty1) * TILE_SIZE,
            )
            .intersect(&frame);
            if !block_rect.is_empty() {
                write_block_to_surface(&scene, &cfg, k.origin, frame, block_rect, &mut scratch, &mut block_buf, &mut surface);
            }
            tx += 2;
        }
        ty += 2;
    }
    Some(surface)
}

#[allow(clippy::too_many_arguments)]
fn write_block_to_surface(
    scene: &PreparedScene,
    cfg: &KakeamiConfig,
    origin: (f32, f32),
    frame: Rect,
    block: Rect,
    scratch: &mut RasterScratch,
    buf: &mut [u8],
    surface: &mut Surface,
) {
    let bw = block.width() as usize;
    let bh = block.height() as usize;
    let slice = &mut buf[..bw * bh];
    let krect = KRect {
        x: f64::from(block.x0 - frame.x0) - f64::from(origin.0),
        y: f64::from(block.y0 - frame.y0) - f64::from(origin.1),
        w: bw as f64,
        h: bh as f64,
    };
    render_tile_gray_into(scene, cfg, krect, scratch, slice);
    // Polarity inversion: kakeami-core `0 = ink, 255 = paper` -> PhotoCraft alpha `255 = ink, 0 = transparent`.
    for px in slice.iter_mut() {
        *px = 255u8.saturating_sub(*px);
    }

    let btx0 = block.x0.div_euclid(TILE_SIZE);
    let bty0 = block.y0.div_euclid(TILE_SIZE);
    let btx1 = (block.x1 - 1).div_euclid(TILE_SIZE) + 1;
    let bty1 = (block.y1 - 1).div_euclid(TILE_SIZE) + 1;

    for sub_ty in bty0..bty1 {
        for sub_tx in btx0..btx1 {
            let coord = TileCoord::new(sub_tx, sub_ty);
            let tile_rect = coord.rect();
            let clip = tile_rect.intersect(&block);
            if clip.is_empty() {
                continue;
            }
            let cw = clip.width() as usize;
            let ch = clip.height() as usize;
            let src_x0 = (clip.x0 - block.x0) as usize;
            let src_y0 = (clip.y0 - block.y0) as usize;
            // Skip allocating sparse tiles that are completely transparent.
            let mut any_ink = false;
            for r in 0..ch {
                let row = &slice[(src_y0 + r) * bw + src_x0..(src_y0 + r) * bw + src_x0 + cw];
                if row.iter().any(|&v| v != 0) {
                    any_ink = true;
                    break;
                }
            }
            if !any_ink {
                continue;
            }
            let dst_x0 = (clip.x0 - tile_rect.x0) as usize;
            let dst_y0 = (clip.y0 - tile_rect.y0) as usize;
            let mut tile_bytes = surface.tile_mut(coord).bytes_mut();
            let stride = TILE_SIZE as usize;
            for r in 0..ch {
                let src_off = (src_y0 + r) * bw + src_x0;
                let dst_off = (dst_y0 + r) * stride + dst_x0;
                tile_bytes[dst_off..dst_off + cw].copy_from_slice(&slice[src_off..src_off + cw]);
            }
        }
    }
}

/// Returns a `GRAY8` alpha `Surface` for GPU rendering when `k.cache` is not populated.
/// Bounded by [`MAX_AUTO_CACHE_PIXELS`] so ultra-large canvases fall back cleanly without OOM.
pub fn derived_gray8_for_gpu(layer: &Layer, k: &KakeamiLayer, canvas: Rect, doc_dpi: f32) -> Option<Arc<Surface>> {
    let frame = effective_frame(layer, k, canvas);
    if frame.is_empty() {
        return None;
    }
    let pixels = i64::from(frame.width()) * i64::from(frame.height());
    if pixels > MAX_AUTO_CACHE_PIXELS {
        return None;
    }
    let (cfg, effective_dpi) = effective_config(k, frame, doc_dpi);
    let key = SurfaceKey {
        layer_id: layer.id,
        config_hash: cfg.config_hash(),
        scaled_dpi_bits: effective_dpi.to_bits(),
        frame: [frame.x0, frame.y0, frame.x1, frame.y1],
        origin_bits: [k.origin.0.to_bits(), k.origin.1.to_bits()],
    };
    {
        let mut cache = SURFACE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = cache.iter().position(|(ek, _)| *ek == key)
            && let Some(entry) = cache.remove(pos)
        {
            let s = Arc::clone(&entry.1);
            cache.push_back(entry);
            return Some(s);
        }
    }
    let built = Arc::new(render_gray8_surface(layer.id, k, frame, doc_dpi, None)?);
    let mut cache = SURFACE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(pos) = cache.iter().position(|(ek, _)| *ek == key)
        && let Some(entry) = cache.remove(pos)
    {
        let s = Arc::clone(&entry.1);
        cache.push_back(entry);
        return Some(s);
    }
    if cache.len() >= SURFACE_LRU_CAP {
        cache.pop_front();
    }
    cache.push_back((key, Arc::clone(&built)));
    Some(built)
}

/// Renders a `KakeamiLayer` into straight-alpha RGBA `Buffer` over `rect` without applying masks.
///
/// Uses `k.cache` (`GRAY8` alpha) when populated; otherwise renders tiled on-demand via
/// `kakeami-core` with polarity inversion (`alpha = 255 - gray`) and `k.color` tinting.
pub fn render_unmasked(layer: &Layer, k: &KakeamiLayer, rect: Rect, canvas: Rect, doc_dpi: f32) -> Buffer {
    let mut buf = Buffer::transparent(rect);
    if rect.is_empty() {
        return buf;
    }
    if let Some(cache) = &k.cache
        && cache.tile_count() > 0
    {
        tint_gray8_surface(cache, k.color, rect, &mut buf);
        return buf;
    }
    let frame = effective_frame(layer, k, canvas);
    let clip = rect.intersect(&frame);
    if clip.is_empty() {
        return buf;
    }
    let (cfg, effective_dpi) = effective_config(k, frame, doc_dpi);
    let scene = prepared_scene(layer.id, &cfg, effective_dpi);
    let mut scratch = RasterScratch::new();
    let mut block_buf = vec![0u8; (KAKEAMI_BLOCK_SIZE as usize) * (KAKEAMI_BLOCK_SIZE as usize)];
    let out_w = rect.width() as usize;
    let [cr, cg, cb, ca] = k.color;

    let mut y = clip.y0;
    while y < clip.y1 {
        let y1 = y.saturating_add(KAKEAMI_BLOCK_SIZE).min(clip.y1);
        let mut x = clip.x0;
        while x < clip.x1 {
            let x1 = x.saturating_add(KAKEAMI_BLOCK_SIZE).min(clip.x1);
            let bw = (x1 - x) as usize;
            let bh = (y1 - y) as usize;
            let slice = &mut block_buf[..bw * bh];
            let krect = KRect {
                x: f64::from(x - frame.x0) - f64::from(k.origin.0),
                y: f64::from(y - frame.y0) - f64::from(k.origin.1),
                w: bw as f64,
                h: bh as f64,
            };
            render_tile_gray_into(&scene, &cfg, krect, &mut scratch, slice);
            for row in 0..bh {
                let dy = (y - rect.y0) as usize + row;
                let dx0 = (x - rect.x0) as usize;
                let dst_row = &mut buf.px[dy * out_w + dx0..dy * out_w + dx0 + bw];
                let src_row = &slice[row * bw..(row + 1) * bw];
                for (dst, &gray) in dst_row.iter_mut().zip(src_row) {
                    let alpha = f32::from(255u8.saturating_sub(gray)) / 255.0;
                    let a = ca * alpha;
                    if a > 0.0 {
                        *dst = [cr, cg, cb, a];
                    }
                }
            }
            x = x1;
        }
        y = y1;
    }
    buf
}

fn tint_gray8_surface(cache: &Surface, color: [f32; 4], rect: Rect, out: &mut Buffer) {
    let mut alpha_f = Vec::new();
    cache.read_region_into(rect, &mut alpha_f);
    let ch = cache.channels().max(1);
    let [cr, cg, cb, ca] = color;
    for (dst, src) in out.px.iter_mut().zip(alpha_f.chunks_exact(ch)) {
        let a = ca * src[0];
        if a > 0.0 {
            *dst = [cr, cg, cb, a];
        }
    }
}

/// Scales spatial dimensions of a `KakeamiLayer` when building a downsampled `1/k` proxy document.
pub(crate) fn scale_for_downsample(k: &mut KakeamiLayer, factor: u32, downsampled_cache: Option<Surface>) {
    let inv_k = 1.0 / factor as f32;
    k.origin = (k.origin.0 * inv_k, k.origin.1 * inv_k);
    if let Some(f) = &mut k.fill_frame {
        let d = i64::from(factor);
        let edge = |v: i32| {
            let v = i64::from(v);
            (v.div_euclid(d) + i64::from(v.rem_euclid(d) != 0)) as i32
        };
        *f = Rect::new(edge(f.x0), edge(f.y0), edge(f.x1), edge(f.y1));
    }
    k.config.nawa_scale = (k.config.nawa_scale * f64::from(inv_k)).clamp(0.25, 8.0);
    k.config.line_spacing = (k.config.line_spacing * f64::from(inv_k)).max(1.0);
    k.config.cluster_size = (k.config.cluster_size * f64::from(inv_k)).max(8.0);
    k.config.stroke_width = (k.config.stroke_width * f64::from(inv_k)).max(0.25);
    k.cache = downsampled_cache;
}

/// Renders a `KakeamiLayer` into an owned [`Surface`] in `fmt` for layered export (e.g. PSD) in
/// memory-bounded horizontal bands (zero full-canvas buffer allocation).
pub fn render_for_export(doc: &Document, layer: &Layer, k: &KakeamiLayer, fmt: PixelFormat) -> Surface {
    let canvas = doc.bounds();
    let frame = effective_frame(layer, k, canvas);
    let mut surf = Surface::new(fmt);
    if frame.is_empty() {
        return surf;
    }
    let band_h = crate::render::band_rows_for(frame.width(), 0).max(1);
    let ch = fmt.channels();
    let mut raw = Vec::new();
    let mut y = frame.y0;
    while y < frame.y1 {
        let y1 = y.saturating_add(band_h).min(frame.y1);
        let band = Rect::new(frame.x0, y, frame.x1, y1);
        let rgba = render_unmasked(layer, k, band, canvas, doc.resolution_dpi);
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
    use photocraft_doc::{Document, KakeamiConfig, KakeamiLayer, Layer, LayerContent, Rect, Size};

    use super::{render_gray8_surface, render_unmasked};

    #[test]
    fn polarity_inversion_maps_zero_gray_to_opaque_ink_and_255_to_transparent() {
        let mut cfg = KakeamiConfig::default();
        cfg.pattern_family = "kakeami-v8".into();
        cfg.finish_mode = "clean".into();
        cfg.preset = "linear-bt".into();
        cfg.darken_cap = 95;
        cfg.grad_start = 90;
        cfg.grad_end = 95;
        cfg.one_bit_mode = true;
        cfg.dpi_sync_noop();

        let rect = Rect::new(0, 0, 256, 256);
        let mut k = KakeamiLayer::new(cfg);
        k.dpi_sync = false;
        k.color = [0.0, 0.0, 0.0, 1.0];
        let layer = Layer::new("kake", LayerContent::Kakeami(k.clone()));

        let uncached = render_unmasked(&layer, &k, rect, rect, 600.0);
        let opaque_black = uncached.px.iter().filter(|p| p[0] == 0.0 && p[1] == 0.0 && p[2] == 0.0 && (p[3] - 1.0).abs() < 1e-5).count();
        let transparent = uncached.px.iter().filter(|p| p[3] == 0.0).count();
        assert!(opaque_black > 500, "expected opaque black ink pixels, got {opaque_black}");
        assert!(transparent > 500, "expected transparent paper pixels, got {transparent}");
        assert_eq!(opaque_black + transparent, uncached.px.len(), "1-bit mode must produce strictly binary alpha");

        // Cached GRAY8 surface must match on-demand render pixel-for-pixel.
        let gray8 = render_gray8_surface(layer.id, &k, rect, 600.0, None).unwrap();
        assert_eq!(gray8.format(), PixelFormat::GRAY8);
        let mut k_cached = k.clone();
        k_cached.cache = Some(gray8);
        let cached = render_unmasked(&layer, &k_cached, rect, rect, 600.0);
        assert_eq!(uncached.px, cached.px, "cached GRAY8 and on-demand Kakeami must be bit-exact");
    }

    #[test]
    fn banded_512_render_matches_monolithic_and_4x256_tiles_seamlessly() {
        let mut doc = Document::new("kake_seamless", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);
        doc.resolution_dpi = 600.0;
        let bounds = doc.bounds();

        let mut cfg = KakeamiConfig::default();
        cfg.pattern_family = "nawa-ami".into();
        cfg.finish_mode = "pencil".into();
        cfg.preset = "linear-bt".into();
        cfg.nawa_tone_style = "denseWhite".into();
        cfg.one_bit_mode = false;
        cfg.seamless = true;

        let mut k = KakeamiLayer::new(cfg);
        k.color = [0.15, 0.25, 0.65, 0.9];
        let layer_id = photocraft_doc::LayerId::fresh();
        let mut layer = Layer::new("nawa", LayerContent::Kakeami(k.clone()));
        layer.id = layer_id;
        doc.layers.push(layer);

        // 1. Monolithic render (uncached)
        let mono = crate::render(&doc, bounds);

        // 2. Banded render across horizontal strips (forces cross-band seamlessness)
        let mut banded_px = Vec::with_capacity(mono.px.len());
        let _ = crate::render_bands(&doc, bounds, 128, |band| -> Result<(), ()> {
            banded_px.extend_from_slice(&band.px);
            Ok(())
        });
        assert_eq!(mono.px.len(), banded_px.len());
        let band_diff = mono.px.iter().zip(&banded_px).filter(|(a, b)| a != b).count();
        assert_eq!(band_diff, 0, "render_bands differed from monolithic render by {band_diff} px");

        // 3. Cached 512² -> 4×256² GRAY8 surface must also match 0 px
        let gray8 = render_gray8_surface(layer_id, &k, bounds, 600.0, None).unwrap();
        assert_eq!(gray8.tile_count(), 4, "512×512 active pattern should populate 4 × 256² GRAY8 tiles");
        if let LayerContent::Kakeami(kl) = &mut doc.layers[0].content {
            kl.cache = Some(gray8);
        }
        let cached_mono = crate::render(&doc, bounds);
        let cache_diff = mono.px.iter().zip(&cached_mono.px).filter(|(a, b)| a != b).count();
        assert_eq!(cache_diff, 0, "4×256² GRAY8 cache differed from monolithic render by {cache_diff} px");
    }

    trait ConfigExt {
        fn dpi_sync_noop(&mut self) {}
    }
    impl ConfigExt for KakeamiConfig {}
}
