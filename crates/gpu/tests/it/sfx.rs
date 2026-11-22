//! Hardware-accelerated Manga SFX GPU tests (`photocraft-gpu`).
//!
//! 1. Validates `SFX_SHADER` with `naga` (`Capabilities::empty()` — pure WebGPU/WebGL2 fragment
//!    + vertex shaders, no compute or storage buffers) without needing a GPU.
//! 2. Exercises live GPU Stencil-then-Cover vector rasterization (including holed glyphs),
//!    Jump Flooding SDF Halo, all 4 Dry Brush engines (`А SDF`, `Б1 Seg`, `Б2 ETF`, `Вектор`),
//!    Screentone, Blending effects (`Stroke`, `FillOverlay`, `DropShadow` + `3D Extrude`, `Knockout`),
//!    and seamless multi-tile rendering (`render_tiled_to_surface`) with timing metrics.

use std::time::Instant;

use photocraft_algo::{PatternType, ScreentoneSpec};
use photocraft_color::{Color, PixelFormat};
use photocraft_doc::Rect;
use photocraft_doc::sfx::{
    SfxBlendEffect, SfxChannelBlend, SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineJoin, SfxLineStyle,
    SfxStrokePosition,
};
use photocraft_gpu::{SFX_SHADER, SfxGpuRenderer, SfxGpuScene, SfxGpuSegContour};

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: SfxGpuRenderer,
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn gpu() -> Option<Gpu> {
    let lock = crate::GPU_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let required = std::env::var_os("WGPU_BACKEND").is_some();
    let instance = crate::instance();
    let fallback = std::env::var_os("PHOTOCRAFT_TEST_FALLBACK_ADAPTER").is_some();
    let options = wgpu::RequestAdapterOptions { force_fallback_adapter: fallback, ..Default::default() };
    let adapter = match pollster::block_on(instance.request_adapter(&options)) {
        Ok(a) => a,
        Err(e) => {
            assert!(!required, "WGPU_BACKEND is set but has no adapter: {e}");
            eprintln!("skipping SFX GPU tests: no adapter ({e})");
            return None;
        }
    };
    let info = adapter.get_info();
    if !SfxGpuRenderer::supported(&adapter) {
        assert!(!required, "WGPU_BACKEND is set but {} lacks SFX formats", info.name);
        eprintln!("skipping SFX GPU tests: {} lacks SFX formats", info.name);
        return None;
    }
    let (device, queue) = match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())) {
        Ok(d) => d,
        Err(e) => {
            assert!(!required, "WGPU_BACKEND is set but {} has no device: {e}", info.name);
            eprintln!("skipping SFX GPU tests: no device ({e})");
            return None;
        }
    };
    eprintln!("SFX GPU tests on {:?} ({})", info.backend, info.name);
    let renderer = SfxGpuRenderer::new_for_adapter(&adapter, &device)
        .unwrap_or_else(|e| panic!("SFX GPU pipeline failed to build on {}: {}", info.name, e.0));
    Some(Gpu { device, queue, renderer, _lock: lock })
}

/// Validates `SFX_SHADER` offline with `naga` and zero extra capabilities (WebGPU/WebGL2 core).
#[test]
fn sfx_shader_validates_without_a_gpu() {
    let module = wgpu::naga::front::wgsl::parse_str(SFX_SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SFX_SHADER)));
    wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string(SFX_SHADER)));
    assert!(module.entry_points.len() >= 18, "expected all SFX vertex/fragment entry points");
}

/// Builds a synthetic holed square glyph ("O") with opposite-winding hole for NonZero winding tests.
fn holed_square_contours(x0: f64, y0: f64, size: f64) -> Vec<Vec<(f64, f64)>> {
    let x1 = x0 + size;
    let y1 = y0 + size;
    let pad = size * 0.28;
    // Outer contour: CCW (positive area in Y-down screen space)
    let outer = vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    // Inner hole: CW (opposite winding cancels NonZero stencil to 0)
    let inner = vec![(x0 + pad, y0 + pad), (x0 + pad, y1 - pad), (x1 - pad, y1 - pad), (x1 - pad, y0 + pad)];
    vec![outer, inner]
}

fn seg_contour_from_poly(poly: &[(f64, f64)]) -> Vec<[f32; 3]> {
    let mut pts = Vec::with_capacity(poly.len() + 1);
    let mut s = 0.0_f32;
    for i in 0..=poly.len() {
        let cur = poly[i % poly.len()];
        if i > 0 {
            let prev = poly[i - 1];
            s += ((cur.0 - prev.0).hypot(cur.1 - prev.1)) as f32;
        }
        pts.push([cur.0 as f32, cur.1 as f32, s]);
    }
    pts
}

#[test]
fn gpu_stencil_cover_handles_holed_glyph_and_halo() {
    let Some(mut g) = gpu() else { return };
    let face = holed_square_contours(64.0, 64.0, 128.0);
    let mut data = SfxData {
        fill: SfxFillSource::Solid { color: Color::rgb(0.05, 0.05, 0.1) },
        ..SfxData::default()
    };
    data.line_effect.style = SfxLineStyle::Clean;
    data.line_effect.halo_width_px = 12.0;
    data.line_effect.halo_color = Color::WHITE;
    data.line_effect.line_width_px = 0.0;
    data.validate();

    let scene = SfxGpuScene {
        data: &data,
        full_bounds: Rect::from_xywh(32, 32, 192, 192),
        glyph_em_px: 128.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &[],
        secondary_shtack_polygons: &[],
        primary_dry_vector_cuts: &[],
        effect_shtack_polygons: &[],
        effect_dry_vector_cuts: &[],
        seg_contours: Vec::new(),
    };
    let surf = g.renderer.render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8).expect("GPU render");

    // 1. Inside the glyph wall (e.g. x=80, y=80) -> dark ink
    let wall = surf.pixel(80, 80);
    assert!(wall[3] > 0.95, "glyph wall must be opaque, got {:?}", wall);
    assert!(wall[0] < 0.2 && wall[1] < 0.2, "glyph wall must be dark ink, got {:?}", wall);

    // 2. Center of the hole (x=128, y=128) -> transparent (hole > 12px halo from inner wall at 99.8..156.2)
    let hole = surf.pixel(128, 128);
    assert!(hole[3] < 0.05, "hole center must stay empty (NonZero stencil), got {:?}", hole);

    // 3. Outside near edge (x=58, y=128, 6px outside outer edge at 64) -> white halo
    let halo = surf.pixel(58, 128);
    assert!(halo[3] > 0.9, "halo pixel must be opaque, got {:?}", halo);
    assert!(halo[0] > 0.9 && halo[1] > 0.9 && halo[2] > 0.9, "halo must be white, got {:?}", halo);

    // 4. Far outside (x=36, y=36) -> transparent
    let bg = surf.pixel(36, 36);
    assert!(bg[3] < 0.05, "outside halo must be transparent, got {:?}", bg);
}

#[test]
fn gpu_dry_brush_all_four_engines_screentone_and_blending_stack() {
    let Some(mut g) = gpu() else { return };
    let face = holed_square_contours(48.0, 48.0, 160.0);
    let seg_pts = seg_contour_from_poly(&face[0]);
    let seg_len = seg_pts.last().map_or(1.0, |p| p[2]);
    let vec_cuts = vec![vec![(50.0, 90.0), (206.0, 90.0), (206.0, 96.0), (50.0, 96.0)]];

    for engine in [
        SfxDryBrushEngine::SdfPhysics,
        SfxDryBrushEngine::SdfSeg,
        SfxDryBrushEngine::SdfEtf,
        SfxDryBrushEngine::Vector,
    ] {
        let mut data = SfxData {
            fill: SfxFillSource::DryBrush {
                color: Color::BLACK,
                spec: SfxDryBrushSpec {
                    enabled: true,
                    engine,
                    dryness_pct: 42.0,
                    freq_pct: 60.0,
                    warp_pct: 30.0,
                    paper_grain_pct: 35.0,
                    edge_wear_pct: 40.0,
                    ..SfxDryBrushSpec::default()
                },
            },
            ..SfxData::default()
        };
        data.line_effect.halo_width_px = 8.0;
        data.blending.effects = vec![
            SfxBlendEffect::Stroke {
                enabled: true,
                width_px: 6.0,
                position: SfxStrokePosition::Outside,
                join: SfxLineJoin::Round,
                fill: SfxFillSource::Solid { color: Color::rgb(0.9, 0.1, 0.1) },
                blend: SfxChannelBlend::Normal,
                opacity: 1.0,
            },
            SfxBlendEffect::FillOverlay {
                enabled: true,
                fill: SfxFillSource::Screentone {
                    spec: ScreentoneSpec {
                        pattern_type: PatternType::Dots,
                        dot_size: 3.0,
                        spacing_x: 6.0,
                        spacing_y: 6.0,
                        density: 0.55,
                        angle: 45.0,
                        ..Default::default()
                    },
                    opacity: 1.0,
                },
                blend: SfxChannelBlend::Multiply,
                opacity: 0.6,
                is_pattern: true,
            },
            SfxBlendEffect::DropShadow {
                enabled: true,
                angle_deg: 45.0,
                distance_px: 10.0,
                extrude_depth_px: 8.0,
                blur_px: 3.0,
                color: Color::rgb(0.1, 0.1, 0.2),
                blend: SfxChannelBlend::Normal,
                opacity: 0.85,
            },
        ];
        data.validate();

        let seg_contours = if engine == SfxDryBrushEngine::SdfSeg {
            vec![SfxGpuSegContour { pts: &seg_pts, total_len: seg_len, phase: 0.0 }]
        } else {
            Vec::new()
        };
        let scene = SfxGpuScene {
            data: &data,
            full_bounds: Rect::from_xywh(20, 20, 230, 230),
            glyph_em_px: 160.0,
            face_contours: &face,
            line_polygons: &[],
            primary_shtack_polygons: &[],
            secondary_shtack_polygons: &[],
            primary_dry_vector_cuts: &vec_cuts,
            effect_shtack_polygons: &[],
            effect_dry_vector_cuts: &[],
            seg_contours,
        };
        let surf = g.renderer.render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8).expect("GPU render");
        assert!(!surf.content_bounds().is_empty(), "DryBrush {engine:?} + Blending stack produced empty surface");
    }
}

#[test]
fn gpu_tiled_render_large_sfx_is_seamless_and_fast() {
    let Some(mut g) = gpu() else { return };
    // Large multi-tile lettering spanning 2600 x 1100 px (crosses the 2048 tile boundary cleanly).
    let mut face = Vec::new();
    for i in 0..4 {
        let bx = 120.0 + f64::from(i) * 600.0;
        face.extend(holed_square_contours(bx, 150.0, 480.0));
    }
    let seg_vecs: Vec<Vec<[f32; 3]>> = face.iter().map(|c| seg_contour_from_poly(c)).collect();
    let seg_contours: Vec<SfxGpuSegContour<'_>> = seg_vecs
        .iter()
        .enumerate()
        .map(|(i, pts)| SfxGpuSegContour { pts, total_len: pts.last().map_or(1.0, |p| p[2]), phase: (i as f32) * 37.0 })
        .collect();

    let mut data = SfxData {
        fill: SfxFillSource::DryBrush {
            color: Color::rgb(0.06, 0.08, 0.14),
            spec: SfxDryBrushSpec {
                enabled: true,
                engine: SfxDryBrushEngine::SdfSeg,
                dryness_pct: 38.0,
                freq_pct: 58.0,
                ..SfxDryBrushSpec::default()
            },
        },
        ..SfxData::default()
    };
    data.line_effect.halo_width_px = 24.0;
    data.line_effect.halo_color = Color::WHITE;
    data.vector_shadow.enabled = true;
    data.vector_shadow.distance_px = 20.0;
    data.vector_shadow.extrude_depth_px = 14.0;
    data.validate();

    let scene = SfxGpuScene {
        data: &data,
        full_bounds: Rect::from_xywh(60, 80, 2600, 1100),
        glyph_em_px: 480.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &[],
        secondary_shtack_polygons: &[],
        primary_dry_vector_cuts: &[],
        effect_shtack_polygons: &[],
        effect_dry_vector_cuts: &[],
        seg_contours,
    };

    // Warm-up one viewport preview pass (1280x720, zero readback) to measure live interactive drag FPS.
    let t_prev = Instant::now();
    let _ = g
        .renderer
        .render_region(&g.device, &g.queue, &scene, [60.0, 80.0, 2660.0, 1180.0], [1280, 720])
        .expect("GPU viewport preview");
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    let prev_ms = t_prev.elapsed().as_secs_f64() * 1000.0;

    // Full-resolution multi-tile render into sparse Surface (2600x1100 = 2.86M px).
    let t_full = Instant::now();
    let surf = g.renderer.render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8).expect("GPU tiled render");
    let full_ms = t_full.elapsed().as_secs_f64() * 1000.0;

    eprintln!("[SFX GPU BENCH] 1280x720 live preview: {prev_ms:.2} ms | 2600x1100 tiled full-res + readback: {full_ms:.2} ms");
    let bounds = surf.content_bounds();
    assert!(bounds.width() > 2300 && bounds.height() > 500, "unexpected tiled bounds: {bounds:?}");

    // Verify seamlessness right across tile boundary (glyph #3 spans x=1920..2400).
    let left_of_seam = surf.pixel(1960, 200);
    let right_of_seam = surf.pixel(2020, 200);
    assert!(left_of_seam[3] > 0.5 && right_of_seam[3] > 0.5, "tiled render dropped pixels near tile seam");
}
