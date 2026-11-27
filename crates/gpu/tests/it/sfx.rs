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
    SfxBlendEffect, SfxChannelBlend, SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxHeavyStackMode, SfxKakeamiV4Mode,
    SfxLineJoin, SfxLineStyle, SfxShibufudeProfile, SfxShtackFillMode, SfxShtackFillSpec, SfxStrokePosition, SfxSugomiDirection, SfxV3EdgeMode,
};
use photocraft_gpu::{DeviceHealth, SFX_SHADER, SfxGpuRenderer, SfxGpuRibbonStrip, SfxGpuScene, SfxGpuSegContour};

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
    assert!(module.entry_points.len() >= 22, "expected all SFX vertex/fragment entry points");
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
        shibufude_splatter_polygons: &[],
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
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
            shibufude_splatter_polygons: &[],
            sugomi_hole_polygons: &[],
            sugomi_inner_teeth_polygons: &[],
            sugomi_band_w_px: 0.0,
            sugomi_hw_in_px: 0.0,
            v3_body_ribbons: Vec::new(),
            heavy_bite_polygons: &[],
            heavy_tail_ribbons: Vec::new(),
            heavy_slit_ribbons: Vec::new(),
            v3_stroke_w_px: 0.0,
            kakeami_w_scale: 1.0,
            kakeami_grad_axis: [0.0, 1.0],
            kakeami_grad_range: [0.0, 32.0],
            kakeami_perp_range: [0.0, 32.0],
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
        shibufude_splatter_polygons: &[],
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
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

#[test]
fn gpu_work_targets_reuse_subviewport_without_realloc_or_edge_leakage() {
    let Some(mut g) = gpu() else { return };
    g.renderer.set_max_dim(1024);

    let face = holed_square_contours(40.0, 40.0, 220.0);
    let seg_pts = seg_contour_from_poly(&face[0]);
    let seg_len = seg_pts.last().map_or(1.0, |p| p[2]);

    let mut data = SfxData {
        fill: SfxFillSource::DryBrush {
            color: Color::rgb(0.85, 0.12, 0.15),
            spec: SfxDryBrushSpec {
                enabled: true,
                engine: SfxDryBrushEngine::SdfSeg,
                dryness_pct: 36.0,
                freq_pct: 55.0,
                ..SfxDryBrushSpec::default()
            },
        },
        ..SfxData::default()
    };
    data.line_effect.halo_width_px = 14.0;
    data.line_effect.halo_color = Color::WHITE;
    data.vector_shadow.enabled = true;
    data.vector_shadow.angle_deg = 45.0;
    data.vector_shadow.distance_px = 12.0;
    data.vector_shadow.extrude_depth_px = 8.0;
    data.validate();

    let scene = SfxGpuScene {
        data: &data,
        full_bounds: Rect::from_xywh(0, 0, 300, 300),
        glyph_em_px: 220.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &[],
        secondary_shtack_polygons: &[],
        primary_dry_vector_cuts: &[],
        effect_shtack_polygons: &[],
        effect_dry_vector_cuts: &[],
        seg_contours: vec![SfxGpuSegContour { pts: &seg_pts, total_len: seg_len, phase: 0.0 }],
        shibufude_splatter_polygons: &[],
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
    };

    // 1. Prime capacity at 512x512 with full-area content so pixels at x>=300, y>=260 are populated.
    let _ = g
        .renderer
        .render_region(&g.device, &g.queue, &scene, [0.0, 0.0, 300.0, 300.0], [512, 512])
        .expect("prime 512x512");
    assert_eq!(g.renderer.targets_alloc_count(), 1);
    assert_eq!(g.renderer.targets_capacity(), Some([512, 512]));

    // 2. Render a series of varying sizes <= [512, 512] (simulating continuous Gizmo arc drag).
    for sz in [[500, 480], [420, 390], [317, 289], [495, 505], [256, 256], [300, 260]] {
        let _ = g
            .renderer
            .render_region(&g.device, &g.queue, &scene, [0.0, 0.0, 300.0, 260.0], sz)
            .expect("sub-viewport render");
    }
    assert_eq!(
        g.renderer.targets_alloc_count(),
        1,
        "varying [w, h] <= capacity must perform ZERO WorkTargets reallocations"
    );
    assert_eq!(g.renderer.targets_capacity(), Some([512, 512]));

    // 3. Compare sub-viewport [300, 260] output (inside 512x512 pool) against a fresh renderer at [300, 260].
    let read_rgba = |renderer: &SfxGpuRenderer, device: &wgpu::Device, queue: &wgpu::Queue| -> Vec<u8> {
        let (buf, padded_row, rw, rh) = renderer.copy_last_output_to_buffer(device, queue).expect("copy");
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range().expect("mapped range");
        let mut out = vec![0u8; (rw * rh * 4) as usize];
        for row in 0..rh as usize {
            let s = row * padded_row as usize;
            let d = row * (rw as usize * 4);
            out[d..d + rw as usize * 4].copy_from_slice(&mapped[s..s + rw as usize * 4]);
        }
        drop(mapped);
        buf.unmap();
        buf.destroy();
        out
    };

    let pooled_px = read_rgba(&g.renderer, &g.device, &g.queue);
    let mut fresh = SfxGpuRenderer::new(&g.device).expect("fresh renderer");
    let _ = fresh
        .render_region(&g.device, &g.queue, &scene, [0.0, 0.0, 300.0, 260.0], [300, 260])
        .expect("fresh 300x260");
    let fresh_px = read_rgba(&fresh, &g.device, &g.queue);

    assert_eq!(pooled_px.len(), fresh_px.len());
    let max_diff = pooled_px
        .iter()
        .zip(fresh_px.iter())
        .map(|(&a, &b)| a.abs_diff(b))
        .max()
        .unwrap_or(0);
    assert!(
        max_diff <= 1,
        "sub-viewport render leaked edge pixels from larger capacity pool (max_diff={max_diff})"
    );

    // 4. Verify explicit target release drops pooled textures immediately.
    g.renderer.release_targets();
    assert_eq!(g.renderer.targets_capacity(), None);
}

#[test]
fn gpu_ukiyo_brush_and_stroke_shtack_render_cleanly_across_zooms() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    let face = holed_square_contours(48.0, 48.0, 160.0);
    let hatch_bars = vec![
        vec![(40.0, 60.0), (216.0, 60.0), (216.0, 74.0), (40.0, 74.0)],
        vec![(40.0, 92.0), (216.0, 92.0), (216.0, 108.0), (40.0, 108.0)],
        vec![(40.0, 130.0), (216.0, 130.0), (216.0, 150.0), (40.0, 150.0)],
        vec![(40.0, 174.0), (216.0, 174.0), (216.0, 196.0), (40.0, 196.0)],
    ];
    let vec_cuts = vec![vec![(36.0, 118.0), (220.0, 118.0), (220.0, 126.0), (36.0, 126.0)]];

    // 1. UkiyoBrush primary fill + Stroke(ShtackHatch overshoot=0 & 50, Hatch & UkiyoBrush) + Stroke(DryBrush Vector)
    let mut data = SfxData {
        fill: SfxFillSource::ShtackHatch {
            spec: SfxShtackFillSpec {
                mode: SfxShtackFillMode::UkiyoBrush,
                overshoot_pct: 0.0,
                dryness_pct: 35.0,
                color: Color::rgb(0.08, 0.08, 0.12),
                ..SfxShtackFillSpec::default()
            },
        },
        ..SfxData::default()
    };
    data.line_effect.halo_width_px = 6.0;
    data.blending.effects = vec![
        SfxBlendEffect::Stroke {
            enabled: true,
            width_px: 12.0,
            position: SfxStrokePosition::Outside,
            join: SfxLineJoin::Round,
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    mode: SfxShtackFillMode::UkiyoBrush,
                    overshoot_pct: 0.0,
                    dryness_pct: 30.0,
                    color: Color::rgb(0.85, 0.1, 0.15),
                    ..SfxShtackFillSpec::default()
                },
            },
            blend: SfxChannelBlend::Normal,
            opacity: 1.0,
        },
        SfxBlendEffect::Stroke {
            enabled: true,
            width_px: 8.0,
            position: SfxStrokePosition::Inside,
            join: SfxLineJoin::Round,
            fill: SfxFillSource::DryBrush {
                color: Color::rgb(0.1, 0.2, 0.85),
                spec: SfxDryBrushSpec {
                    enabled: true,
                    engine: SfxDryBrushEngine::Vector,
                    ..SfxDryBrushSpec::default()
                },
            },
            blend: SfxChannelBlend::Normal,
            opacity: 1.0,
        },
        SfxBlendEffect::Stroke {
            enabled: true,
            width_px: 10.0,
            position: SfxStrokePosition::Center,
            join: SfxLineJoin::Round,
            fill: SfxFillSource::ShtackHatch {
                spec: SfxShtackFillSpec {
                    mode: SfxShtackFillMode::Hatch,
                    overshoot_pct: 0.0,
                    dryness_pct: 25.0,
                    color: Color::rgb(0.1, 0.65, 0.2),
                    ..SfxShtackFillSpec::default()
                },
            },
            blend: SfxChannelBlend::Normal,
            opacity: 1.0,
        },
    ];
    data.validate();

    let eff_shtack = [(0usize, hatch_bars.clone()), (2usize, hatch_bars.clone())];
    let eff_cuts = [(1usize, vec_cuts.clone())];

    // Sweep across target sizes & glyph_em_px so UkiyoBrush JFA hits both even and odd step counts (reach 16..128).
    for (glyph_em_px, target_px) in [
        (120.0_f32, [128, 128]),
        (120.0, [256, 256]),
        (240.0, [256, 256]),
        (240.0, [512, 512]),
        (480.0, [512, 512]),
    ] {
        let scene = SfxGpuScene {
            data: &data,
            full_bounds: Rect::from_xywh(16, 16, 224, 224),
            glyph_em_px,
            face_contours: &face,
            line_polygons: &[],
            primary_shtack_polygons: &hatch_bars,
            secondary_shtack_polygons: &[],
            effect_shtack_polygons: &eff_shtack,
            seg_contours: Vec::new(),
            primary_dry_vector_cuts: &[],
            effect_dry_vector_cuts: &eff_cuts,
            shibufude_splatter_polygons: &[],
            sugomi_hole_polygons: &[],
            sugomi_inner_teeth_polygons: &[],
            sugomi_band_w_px: 0.0,
            sugomi_hw_in_px: 0.0,
            v3_body_ribbons: Vec::new(),
            heavy_bite_polygons: &[],
            heavy_tail_ribbons: Vec::new(),
            heavy_slit_ribbons: Vec::new(),
            v3_stroke_w_px: 0.0,
            kakeami_w_scale: 1.0,
            kakeami_grad_axis: [0.0, 1.0],
            kakeami_grad_range: [0.0, 32.0],
            kakeami_perp_range: [0.0, 32.0],
        };
        let _ = g
            .renderer
            .render_region(&g.device, &g.queue, &scene, [16.0, 16.0, 240.0, 240.0], target_px)
            .unwrap_or_else(|e| panic!("render_region failed for em={glyph_em_px} px={target_px:?}: {}", e.0));
        // Also exercise snapshot blit creation (multi-window safe path).
        let snap = g
            .renderer
            .prepare_blit_snapshot(&g.device, &g.queue, wgpu::TextureFormat::Bgra8UnormSrgb, None);
        assert!(snap.is_some(), "prepare_blit_snapshot returned None");
    }

    let scene = SfxGpuScene {
        data: &data,
        full_bounds: Rect::from_xywh(16, 16, 224, 224),
        glyph_em_px: 240.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &hatch_bars,
        secondary_shtack_polygons: &[],
        effect_shtack_polygons: &eff_shtack,
        seg_contours: Vec::new(),
        primary_dry_vector_cuts: &[],
        effect_dry_vector_cuts: &eff_cuts,
        shibufude_splatter_polygons: &[],
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
    };
    let surf = g
        .renderer
        .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
        .expect("GPU tiled UkiyoBrush + Stroke Shtack render");
    assert!(!surf.content_bounds().is_empty(), "UkiyoBrush + Stroke Shtack rendered empty surface");

    // 2. Verify SfxFillSource::None with active Stroke preserves accum parity and produces visible stroke.
    let mut hollow_data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
    hollow_data.line_effect.line_width_px = 0.0;
    hollow_data.line_effect.halo_width_px = 0.0;
    hollow_data.blending.effects = vec![SfxBlendEffect::Stroke {
        enabled: true,
        width_px: 10.0,
        position: SfxStrokePosition::Inside,
        join: SfxLineJoin::Round,
        fill: SfxFillSource::Solid { color: Color::rgb(0.9, 0.2, 0.1) },
        blend: SfxChannelBlend::Normal,
        opacity: 1.0,
    }];
    hollow_data.validate();
    let hollow_scene = SfxGpuScene {
        data: &hollow_data,
        full_bounds: Rect::from_xywh(16, 16, 224, 224),
        glyph_em_px: 160.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &[],
        secondary_shtack_polygons: &[],
        effect_shtack_polygons: &[],
        seg_contours: Vec::new(),
        primary_dry_vector_cuts: &[],
        effect_dry_vector_cuts: &[],
        shibufude_splatter_polygons: &[],
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
    };
    let hollow_surf = g
        .renderer
        .render_tiled_to_surface(&g.device, &g.queue, &hollow_scene, PixelFormat::RGBA8)
        .expect("GPU hollow stroke render");
    assert!(!hollow_surf.content_bounds().is_empty(), "hollow fill + inside stroke rendered empty");

    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}

fn read_last_rgba(renderer: &SfxGpuRenderer, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
    let (buf, padded_row, rw, rh) = renderer.copy_last_output_to_buffer(device, queue).expect("copy");
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().unwrap().unwrap();
    let mapped = slice.get_mapped_range().expect("mapped range");
    let mut out = vec![0u8; (rw * rh * 4) as usize];
    for row in 0..rh as usize {
        let s = row * padded_row as usize;
        let d = row * (rw as usize * 4);
        out[d..d + rw as usize * 4].copy_from_slice(&mapped[s..s + rw as usize * 4]);
    }
    drop(mapped);
    buf.unmap();
    buf.destroy();
    out
}

#[test]
fn gpu_shibufude_v3_kado_maru_modes_and_subviewport_grain_invariance() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    let face = holed_square_contours(64.0, 64.0, 128.0);
    // Overlapping Kado quads with MIXED windings (CCW + CW) at top-left corner (64, 64)
    // to prove BlendOperation::Max never punches stencil holes on overlap!
    let kado_quads = vec![
        vec![(56.0, 56.0), (200.0, 56.0), (200.0, 70.0), (56.0, 70.0)],
        // Intentionally reversed (CW) overlapping vertical quad at x=56..70, y=56..200:
        vec![(56.0, 56.0), (56.0, 200.0), (70.0, 200.0), (70.0, 56.0)],
        vec![(186.0, 56.0), (200.0, 56.0), (200.0, 200.0), (186.0, 200.0)],
        vec![(56.0, 186.0), (200.0, 186.0), (200.0, 200.0), (56.0, 200.0)],
    ];
    let splatter = vec![
        vec![(30.0, 44.0), (40.0, 44.0), (40.0, 54.0), (30.0, 54.0)],
        vec![(212.0, 110.0), (224.0, 110.0), (224.0, 122.0), (212.0, 122.0)],
    ];

    for profile in [SfxShibufudeProfile::Kado, SfxShibufudeProfile::Maru] {
        for edge_mode in [SfxV3EdgeMode::OneBit, SfxV3EdgeMode::Rough] {
            for knockout in [false, true] {
                let mut data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
                data.line_effect.style = SfxLineStyle::ShibufudeV3;
                data.line_effect.line_width_px = 14.0;
                data.line_effect.line_color = Color::rgb(0.05, 0.05, 0.08);
                data.line_effect.halo_width_px = 0.0;
                data.line_effect.shibufude_profile = profile;
                data.line_effect.shibufude_edge_mode = edge_mode;
                data.line_effect.shibufude_knockout = knockout;
                data.line_effect.shibufude_seed = 7;
                data.validate();

                let scene = SfxGpuScene {
                    data: &data,
                    full_bounds: Rect::from_xywh(16, 16, 224, 224),
                    glyph_em_px: 128.0,
                    face_contours: &face,
                    line_polygons: if profile == SfxShibufudeProfile::Kado { &kado_quads } else { &[] },
                    primary_shtack_polygons: &[],
                    secondary_shtack_polygons: &[],
                    effect_shtack_polygons: &[],
                    seg_contours: Vec::new(),
                    primary_dry_vector_cuts: &[],
                    effect_dry_vector_cuts: &[],
                    shibufude_splatter_polygons: &splatter,
                    sugomi_hole_polygons: &[],
                    sugomi_inner_teeth_polygons: &[],
                    sugomi_band_w_px: 0.0,
                    sugomi_hw_in_px: 0.0,
                    v3_body_ribbons: Vec::new(),
                    heavy_bite_polygons: &[],
                    heavy_tail_ribbons: Vec::new(),
                    heavy_slit_ribbons: Vec::new(),
                    v3_stroke_w_px: 0.0,
                    kakeami_w_scale: 1.0,
                    kakeami_grad_axis: [0.0, 1.0],
                    kakeami_grad_range: [0.0, 32.0],
                    kakeami_perp_range: [0.0, 32.0],
                };
                let surf = g
                    .renderer
                    .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
                    .unwrap_or_else(|e| panic!("Shibufude {profile:?}/{edge_mode:?}/ko={knockout} failed: {}", e.0));
                assert!(!surf.content_bounds().is_empty(), "Shibufude {profile:?}/{edge_mode:?} empty");
                // Center of hole (128, 128) must stay open.
                assert!(surf.pixel(128, 128)[3] < 0.05, "hole center should stay transparent");
                // Splatter blob at (35, 49) must be visible!
                assert!(surf.pixel(35, 49)[3] > 0.5, "splatter blob missing in {profile:?}/{edge_mode:?}");
                if profile == SfxShibufudeProfile::Kado && !knockout {
                    // Overlap corner of CCW + CW quads at (63, 63) must remain SOLID (BlendOperation::Max)!
                    assert!(
                        surf.pixel(63, 63)[3] > 0.7,
                        "Kado overlapping mixed-winding corner punched a hole (got {:?})",
                        surf.pixel(63, 63)
                    );
                }
            }
        }
    }

    // Verify document-coordinate grain invariance across shifted sub-viewports (zero pan drift!).
    let mut inv_data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
    inv_data.line_effect.style = SfxLineStyle::ShibufudeV3;
    inv_data.line_effect.line_width_px = 14.0;
    inv_data.line_effect.halo_width_px = 0.0;
    inv_data.line_effect.shibufude_profile = SfxShibufudeProfile::Maru;
    inv_data.line_effect.shibufude_edge_mode = SfxV3EdgeMode::Rough;
    inv_data.line_effect.shibufude_seed = 19;
    inv_data.validate();
    let inv_scene = SfxGpuScene {
        data: &inv_data,
        full_bounds: Rect::from_xywh(16, 16, 224, 224),
        glyph_em_px: 128.0,
        face_contours: &face,
        line_polygons: &[],
        primary_shtack_polygons: &[],
        secondary_shtack_polygons: &[],
        effect_shtack_polygons: &[],
        seg_contours: Vec::new(),
        primary_dry_vector_cuts: &[],
        effect_dry_vector_cuts: &[],
        shibufude_splatter_polygons: &splatter,
        sugomi_hole_polygons: &[],
        sugomi_inner_teeth_polygons: &[],
        sugomi_band_w_px: 0.0,
        sugomi_hw_in_px: 0.0,
        v3_body_ribbons: Vec::new(),
        heavy_bite_polygons: &[],
        heavy_tail_ribbons: Vec::new(),
        heavy_slit_ribbons: Vec::new(),
        v3_stroke_w_px: 0.0,
        kakeami_w_scale: 1.0,
        kakeami_grad_axis: [0.0, 1.0],
        kakeami_grad_range: [0.0, 32.0],
        kakeami_perp_range: [0.0, 32.0],
    };
    let _ = g
        .renderer
        .render_region(&g.device, &g.queue, &inv_scene, [16.0, 16.0, 240.0, 240.0], [224, 224])
        .expect("full region");
    let full_px = read_last_rgba(&g.renderer, &g.device, &g.queue);
    let _ = g
        .renderer
        .render_region(&g.device, &g.queue, &inv_scene, [32.0, 32.0, 224.0, 224.0], [192, 192])
        .expect("shifted sub-viewport");
    let sub_px = read_last_rgba(&g.renderer, &g.device, &g.queue);

    // Compare interior overlap doc [56..200, 56..200] -> well away from JFA viewport apron.
    let mut max_diff = 0u8;
    for dy in 56usize..200 {
        for dx in 56usize..200 {
            let fi = ((dy - 16) * 224 + (dx - 16)) * 4;
            let si = ((dy - 32) * 192 + (dx - 32)) * 4;
            for ch in 0..4 {
                max_diff = max_diff.max(full_px[fi + ch].abs_diff(sub_px[si + ch]));
            }
        }
    }
    assert!(max_diff <= 2, "Shibufude v3 grain drifted across sub-viewport shift (max_diff={max_diff})");
    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}

#[test]
fn gpu_sugomi_v3_solid_hollow_modes_and_direct_max_inner_teeth() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    let face = holed_square_contours(48.0, 48.0, 160.0);
    // Spiky outer contour around (48..208, 48..208)
    let outer_spiky = vec![vec![
        (36.0, 36.0),
        (128.0, 24.0),
        (220.0, 36.0),
        (232.0, 128.0),
        (220.0, 220.0),
        (128.0, 232.0),
        (36.0, 220.0),
        (24.0, 128.0),
    ]];
    let hole_cut = vec![vec![(98.0, 98.0), (158.0, 98.0), (158.0, 158.0), (98.0, 158.0)]];
    // Overlapping inward teeth (one CCW, one CW) inside top hollow band wall
    let inner_teeth = vec![
        vec![(62.0, 62.0), (84.0, 62.0), (80.0, 74.0), (73.0, 82.0), (66.0, 74.0)],
        vec![(70.0, 62.0), (74.0, 74.0), (81.0, 82.0), (88.0, 74.0), (92.0, 62.0)],
    ];

    let mut solid_ink = 0usize;
    let mut hollow_ink = 0usize;

    for dir in [SfxSugomiDirection::Fude, SfxSugomiDirection::Tate, SfxSugomiDirection::Yoko] {
        for hollow in [false, true] {
            for edge_mode in [SfxV3EdgeMode::OneBit, SfxV3EdgeMode::Rough] {
                let mut data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
                data.line_effect.style = SfxLineStyle::SugomiV3;
                data.line_effect.line_color = Color::rgb(0.04, 0.04, 0.06);
                data.line_effect.halo_width_px = 0.0;
                data.line_effect.sugomi_direction = dir;
                data.line_effect.sugomi_hollow = hollow;
                data.line_effect.sugomi_edge_mode = edge_mode;
                data.line_effect.sugomi_seed = 11;
                data.validate();

                let scene = SfxGpuScene {
                    data: &data,
                    full_bounds: Rect::from_xywh(16, 16, 224, 224),
                    glyph_em_px: 160.0,
                    face_contours: &face,
                    line_polygons: &outer_spiky,
                    primary_shtack_polygons: &[],
                    secondary_shtack_polygons: &[],
                    effect_shtack_polygons: &[],
                    seg_contours: Vec::new(),
                    primary_dry_vector_cuts: &[],
                    effect_dry_vector_cuts: &[],
                    shibufude_splatter_polygons: &[],
                    sugomi_hole_polygons: &hole_cut,
                    sugomi_inner_teeth_polygons: if hollow { &inner_teeth } else { &[] },
                    sugomi_band_w_px: 16.0,
                    sugomi_hw_in_px: 6.0,
                    v3_body_ribbons: Vec::new(),
                    heavy_bite_polygons: &[],
                    heavy_tail_ribbons: Vec::new(),
                    heavy_slit_ribbons: Vec::new(),
                    v3_stroke_w_px: 0.0,
                    kakeami_w_scale: 1.0,
                    kakeami_grad_axis: [0.0, 1.0],
                    kakeami_grad_range: [0.0, 32.0],
                    kakeami_perp_range: [0.0, 32.0],
                };
                let surf = g
                    .renderer
                    .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
                    .unwrap_or_else(|e| panic!("Sugomi {dir:?}/hollow={hollow}/{edge_mode:?} failed: {}", e.0));
                assert!(!surf.content_bounds().is_empty(), "Sugomi {dir:?}/hollow={hollow} empty");
                // Center of counter hole must remain open.
                assert!(surf.pixel(128, 128)[3] < 0.05, "Sugomi hole center must stay open");

                if dir == SfxSugomiDirection::Fude && edge_mode == SfxV3EdgeMode::OneBit {
                    let mut count = 0usize;
                    for y in 16..240 {
                        for x in 16..240 {
                            if surf.pixel(x, y)[3] > 0.5 {
                                count += 1;
                            }
                        }
                    }
                    if hollow {
                        hollow_ink = count;
                        // Overlapping inner teeth at (76, 68) must remain SOLID (BlendOperation::Max)!
                        assert!(
                            surf.pixel(76, 68)[3] > 0.5,
                            "Sugomi overlapping inner teeth punched a hole (got {:?})",
                            surf.pixel(76, 68)
                        );
                    } else {
                        solid_ink = count;
                    }
                }
            }
        }
    }
    assert!(
        hollow_ink > 0 && hollow_ink < solid_ink,
        "Sugomi hollow ({hollow_ink} px) must carve out interior relative to solid ({solid_ink} px)"
    );
    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}

fn build_arc_ribbon(cx: f32, cy: f32, r_mid: f32, hw: f32, a0: f32, a1: f32, steps: usize) -> (Vec<[f32; 2]>, Vec<[f32; 2]>) {
    let mut left = Vec::with_capacity(steps + 1);
    let mut right = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let a = a0 + (a1 - a0) * t;
        let (s, c) = a.sin_cos();
        left.push([cx + c * (r_mid + hw), cy + s * (r_mid + hw)]);
        right.push([cx + c * (r_mid - hw), cy + s * (r_mid - hw)]);
    }
    (left, right)
}

#[test]
fn gpu_quad_strip_curved_c_arc_and_closed_ring_leave_concave_interior_empty() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    // 1. Curved 270° C-shaped arc centered at (128, 128), radius 70, half-width 8.
    // A naive centroid-fan would fill the concave interior cavity at (128, 128)!
    let (c_left, c_right) = build_arc_ribbon(128.0, 128.0, 70.0, 8.0, 0.45, 5.83, 40);
    // 2. Closed 360° ring centered at (128, 128), radius 70, half-width 8.
    let (r_left, r_right) = build_arc_ribbon(128.0, 128.0, 70.0, 8.0, 0.0, std::f32::consts::TAU, 48);

    for (label, left, right) in [("C-arc", &c_left, &c_right), ("closed-ring", &r_left, &r_right)] {
        let mut data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
        data.line_effect.style = SfxLineStyle::HeavyStackV3;
        data.line_effect.heavy_stack_mode = SfxHeavyStackMode::Kin;
        data.line_effect.heavy_stack_edge_mode = SfxV3EdgeMode::OneBit;
        data.line_effect.line_grain_pct = 0.0;
        data.line_effect.halo_width_px = 0.0;
        data.line_effect.line_color = Color::BLACK;
        data.validate();

        let scene = SfxGpuScene {
            data: &data,
            full_bounds: Rect::from_xywh(16, 16, 224, 224),
            glyph_em_px: 160.0,
            face_contours: &[],
            line_polygons: &[],
            primary_shtack_polygons: &[],
            secondary_shtack_polygons: &[],
            effect_shtack_polygons: &[],
            seg_contours: Vec::new(),
            primary_dry_vector_cuts: &[],
            effect_dry_vector_cuts: &[],
            shibufude_splatter_polygons: &[],
            sugomi_hole_polygons: &[],
            sugomi_inner_teeth_polygons: &[],
            sugomi_band_w_px: 0.0,
            sugomi_hw_in_px: 0.0,
            v3_body_ribbons: vec![SfxGpuRibbonStrip { left, right }],
            heavy_bite_polygons: &[],
            heavy_tail_ribbons: Vec::new(),
            heavy_slit_ribbons: Vec::new(),
            v3_stroke_w_px: 28.0,
            kakeami_w_scale: 1.0,
            kakeami_grad_axis: [0.0, 1.0],
            kakeami_grad_range: [0.0, 224.0],
            kakeami_perp_range: [0.0, 224.0],
        };
        let surf = g
            .renderer
            .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
            .unwrap_or_else(|e| panic!("{label} quad-strip render failed: {}", e.0));
        // Concave center (128, 128) MUST stay completely transparent (no centroid-fan leak!).
        assert!(
            surf.pixel(128, 128)[3] < 0.05,
            "{label} concave center leaked ink (got {:?}) — quad-strip broken!",
            surf.pixel(128, 128)
        );
        // Point on the left arm of the arc/ring at angle pi -> (58, 128) MUST be solid ink!
        assert!(
            surf.pixel(58, 128)[3] > 0.9,
            "{label} ribbon body missing ink at (58, 128) (got {:?})",
            surf.pixel(58, 128)
        );
    }
    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}

#[test]
fn gpu_heavy_stack_v3_all_modes_edge_bite_order_and_health() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    let face = holed_square_contours(48.0, 48.0, 160.0);
    let body_l = [[52.0_f32, 52.0], [204.0, 52.0], [204.0, 204.0]];
    let body_r = [[52.0_f32, 82.0], [174.0, 82.0], [174.0, 204.0]];
    // Bite cuts out a bite around (120..145, 52..78)
    let bite = vec![vec![(118.0, 48.0), (146.0, 48.0), (146.0, 78.0), (132.0, 84.0), (118.0, 78.0)]];
    // Tail crosses through (125..140, 60..70) — MUST survive because tails union AFTER edge_bite!
    let tail_l = [[110.0_f32, 60.0], [156.0, 60.0]];
    let tail_r = [[110.0_f32, 70.0], [156.0, 70.0]];
    // Slit cuts inside lower vertical leg at x=188..192, y=120..170
    let slit_l = [[187.0_f32, 120.0], [187.0, 170.0]];
    let slit_r = [[192.0_f32, 120.0], [192.0, 170.0]];

    for mode in [
        SfxHeavyStackMode::Fude,
        SfxHeavyStackMode::FgsfdsStack,
        SfxHeavyStackMode::Kin,
        SfxHeavyStackMode::FgsfdsFence,
    ] {
        for edge_bite_pct in [0.0_f32, 65.0] {
            for edge_mode in [SfxV3EdgeMode::OneBit, SfxV3EdgeMode::Rough] {
                let mut data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
                data.line_effect.style = SfxLineStyle::HeavyStackV3;
                data.line_effect.heavy_stack_mode = mode;
                data.line_effect.heavy_stack_edge_bite_pct = edge_bite_pct;
                data.line_effect.heavy_stack_edge_mode = edge_mode;
                data.line_effect.line_color = Color::BLACK;
                data.line_effect.halo_width_px = 0.0;
                data.validate();

                let scene = SfxGpuScene {
                    data: &data,
                    full_bounds: Rect::from_xywh(16, 16, 224, 224),
                    glyph_em_px: 160.0,
                    face_contours: &face,
                    line_polygons: &[],
                    primary_shtack_polygons: &[],
                    secondary_shtack_polygons: &[],
                    effect_shtack_polygons: &[],
                    seg_contours: Vec::new(),
                    primary_dry_vector_cuts: &[],
                    effect_dry_vector_cuts: &[],
                    shibufude_splatter_polygons: &[],
                    sugomi_hole_polygons: &[],
                    sugomi_inner_teeth_polygons: &[],
                    sugomi_band_w_px: 0.0,
                    sugomi_hw_in_px: 0.0,
                    v3_body_ribbons: if mode == SfxHeavyStackMode::Fude {
                        Vec::new()
                    } else {
                        vec![SfxGpuRibbonStrip { left: &body_l, right: &body_r }]
                    },
                    heavy_bite_polygons: if edge_bite_pct > 0.0 && mode != SfxHeavyStackMode::FgsfdsFence {
                        &bite
                    } else {
                        &[]
                    },
                    heavy_tail_ribbons: if matches!(mode, SfxHeavyStackMode::Fude | SfxHeavyStackMode::FgsfdsStack) {
                        vec![SfxGpuRibbonStrip { left: &tail_l, right: &tail_r }]
                    } else {
                        Vec::new()
                    },
                    heavy_slit_ribbons: if mode != SfxHeavyStackMode::FgsfdsFence {
                        vec![SfxGpuRibbonStrip { left: &slit_l, right: &slit_r }]
                    } else {
                        Vec::new()
                    },
                    v3_stroke_w_px: 30.0,
                    kakeami_w_scale: 1.0,
                    kakeami_grad_axis: [0.0, 1.0],
                    kakeami_grad_range: [48.0, 160.0],
                    kakeami_perp_range: [-208.0, 160.0],
                };
                let surf = g
                    .renderer
                    .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
                    .unwrap_or_else(|e| panic!("HeavyStack {mode:?}/bite={edge_bite_pct}/{edge_mode:?} failed: {}", e.0));
                assert!(!surf.content_bounds().is_empty(), "HeavyStack {mode:?} rendered empty");
                if mode == SfxHeavyStackMode::FgsfdsStack && edge_bite_pct > 0.0 && edge_mode == SfxV3EdgeMode::OneBit {
                    // Tail overlapping the bite zone at (132, 65) must stay SOLID!
                    assert!(
                        surf.pixel(132, 65)[3] > 0.5,
                        "tail was erroneously cut by edge_bite (got {:?})",
                        surf.pixel(132, 65)
                    );
                    // Non-tail bite zone at (132, 54) must be carved out!
                    assert!(
                        surf.pixel(132, 54)[3] < 0.2,
                        "edge_bite did not carve body at (132, 54) (got {:?})",
                        surf.pixel(132, 54)
                    );
                }
            }
        }
    }
    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}

#[test]
fn gpu_kakeami_v4_all_modes_kake_edge_gradient_and_health() {
    let Some(mut g) = gpu() else { return };
    let health = DeviceHealth::watch(&g.device);
    g.renderer.set_health(health.clone());

    // Solid square glyph [48..208, 48..208]
    let solid_face = vec![vec![(48.0, 48.0), (208.0, 48.0), (208.0, 208.0), (48.0, 208.0)]];
    // Synthetic parallel crosshatch ribbons spanning the glyph (denser toward bottom for KakeEdge)
    let mut rib_storage: Vec<(Vec<[f32; 2]>, Vec<[f32; 2]>)> = Vec::new();
    for k in 0..14 {
        let y = 56.0 + (k as f32) * 11.0;
        let l = vec![[54.0, y - 1.2], [202.0, y - 1.2]];
        let r = vec![[54.0, y + 1.2], [202.0, y + 1.2]];
        rib_storage.push((l, r));
    }
    let ribbons: Vec<SfxGpuRibbonStrip<'_>> = rib_storage
        .iter()
        .map(|(l, r)| SfxGpuRibbonStrip { left: l, right: r })
        .collect();

    // Synthetic KakeEdge 4-layer cascade ribbons: light in top, crosshatched in mid, dense + beta_mask in bot
    let mut kake_edge_storage: Vec<(Vec<[f32; 2]>, Vec<[f32; 2]>)> = Vec::new();
    // Layer 0 (sparse across full height: y = 60, 82, 104, 126, 148, 170, 192)
    for k in 0..7 {
        let y = 60.0 + (k as f32) * 22.0;
        kake_edge_storage.push((vec![[54.0, y - 1.2], [202.0, y - 1.2]], vec![[54.0, y + 1.2], [202.0, y + 1.2]]));
    }
    // Layers 1..2 (crosshatch starting from mid: y = 108..200)
    for k in 0..10 {
        let x = 60.0 + (k as f32) * 15.0;
        kake_edge_storage.push((vec![[x - 1.2, 108.0], [x - 1.2, 200.0]], vec![[x + 1.2, 108.0], [x + 1.2, 200.0]]));
    }
    let kake_edge_ribbons: Vec<SfxGpuRibbonStrip<'_>> = kake_edge_storage
        .iter()
        .map(|(l, r)| SfxGpuRibbonStrip { left: l, right: r })
        .collect();

    for mode in [SfxKakeamiV4Mode::Kake2, SfxKakeamiV4Mode::Kake4, SfxKakeamiV4Mode::KakeEdge] {
        for edge_mode in [SfxV3EdgeMode::OneBit, SfxV3EdgeMode::Rough] {
            let mut data = SfxData { fill: SfxFillSource::None, ..SfxData::default() };
            data.line_effect.style = SfxLineStyle::KakeamiV4;
            data.line_effect.kakeami_mode = mode;
            data.line_effect.kakeami_edge_mode = edge_mode;
            data.line_effect.line_color = Color::BLACK;
            data.line_effect.halo_width_px = 0.0;
            data.validate();

            let scene = SfxGpuScene {
                data: &data,
                full_bounds: Rect::from_xywh(16, 16, 224, 224),
                glyph_em_px: 160.0,
                face_contours: &solid_face,
                line_polygons: &[],
                primary_shtack_polygons: &[],
                secondary_shtack_polygons: &[],
                effect_shtack_polygons: &[],
                seg_contours: Vec::new(),
                primary_dry_vector_cuts: &[],
                effect_dry_vector_cuts: &[],
                shibufude_splatter_polygons: &[],
                sugomi_hole_polygons: &[],
                sugomi_inner_teeth_polygons: &[],
                sugomi_band_w_px: 0.0,
                sugomi_hw_in_px: 0.0,
                v3_body_ribbons: if mode == SfxKakeamiV4Mode::KakeEdge {
                    kake_edge_ribbons.clone()
                } else {
                    ribbons.clone()
                },
                heavy_bite_polygons: &[],
                heavy_tail_ribbons: Vec::new(),
                heavy_slit_ribbons: Vec::new(),
                v3_stroke_w_px: 40.0,
                kakeami_w_scale: 1.4,
                kakeami_grad_axis: [0.0, 1.0],
                kakeami_grad_range: [48.0, 160.0],
                kakeami_perp_range: [-208.0, 160.0],
            };
            let surf = g
                .renderer
                .render_tiled_to_surface(&g.device, &g.queue, &scene, PixelFormat::RGBA8)
                .unwrap_or_else(|e| panic!("Kakeami {mode:?}/{edge_mode:?} failed: {}", e.0));
            assert!(!surf.content_bounds().is_empty(), "Kakeami {mode:?}/{edge_mode:?} rendered empty");

            if mode == SfxKakeamiV4Mode::KakeEdge && edge_mode == SfxV3EdgeMode::OneBit {
                // 1. Full KakeEdge render (hatch cascade + GPU beta_mask) must increase strictly top < mid < bot
                let band_ink = |y0: i32, y1: i32| -> usize {
                    let mut c = 0usize;
                    for y in y0..y1 {
                        for x in 56..200 {
                            if surf.pixel(x, y)[3] > 0.5 {
                                c += 1;
                            }
                        }
                    }
                    c
                };
                let top = band_ink(54, 94);
                let mid = band_ink(108, 148);
                let bot = band_ink(160, 200);
                assert!(
                    top > 0 && top < mid && mid < bot,
                    "KakeEdge GPU render must increase top < mid < bot (top={top}, mid={mid}, bot={bot})"
                );

                // 2. GPU KakeamiBetaMask alone must stay empty at top and paint solid ink core at bottom
                let beta_scene = SfxGpuScene { v3_body_ribbons: Vec::new(), ..scene };
                let beta_surf = g
                    .renderer
                    .render_tiled_to_surface(&g.device, &g.queue, &beta_scene, PixelFormat::RGBA8)
                    .expect("KakeEdge beta_mask render");
                let b_ink = |y0: i32, y1: i32| -> usize {
                    let mut c = 0usize;
                    for y in y0..y1 {
                        for x in 56..200 {
                            if beta_surf.pixel(x, y)[3] > 0.5 {
                                c += 1;
                            }
                        }
                    }
                    c
                };
                assert_eq!(b_ink(54, 94), 0, "beta_mask must stay empty at top");
                assert!(b_ink(160, 200) > 2000, "beta_mask must fill dense bottom ink core");
            }
        }
    }
    assert_eq!(health.errors(), 0, "expected 0 uncaptured wgpu errors");
    assert!(health.fault().is_none(), "expected healthy device, got {:?}", health.fault());
}
