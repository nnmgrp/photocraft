//! Zero-readback hardware `wgpu` SFX preview overlay + Native tiled full-res commit hook.
//!
//! Works identically on WebGPU, WebGL2, DX12, Vulkan, and Metal:
//! - During interactive SFX slider / gizmo / Gentonik drags, [`paint`] schedules an `egui_wgpu`
//!   paint callback that rasterizes ONLY the visible viewport intersection of the SFX lettering
//!   directly in VRAM (`<= 2048 × 2048 px`, zero CPU readback) and blits it straight onto the
//!   canvas at 60 FPS — even on 12 000 × 17 000 px manga pages.
//! - On Native (`#[cfg(not(target_arch = "wasm32"))]`), [`init_for_render_state`] also installs a
//!   tiled full-resolution GPU renderer hook on `photocraft-engine`, so pointer-release commits
//!   render arbitrarily large lettering seamlessly across padded GPU tiles into a sparse `Surface`.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor};
use eframe::wgpu;
use photocraft_doc::sfx::{SfxBlendEffect, SfxData};
use photocraft_gpu::{SfxGpuRenderer, SfxGpuScene, SfxGpuSegContour};
use photocraft_manga_core::sfx_render::{SfxVectorScene, extract_sfx_vector_scene};

use super::ViewXform;

/// Cap live interactive viewport preview textures at 2048 px per side so 12K×17K pages stay 60 FPS.
const MAX_PREVIEW_DIM: u32 = 2048;

static GPU_SFX_FAILED: AtomicBool = AtomicBool::new(false);

struct CachedVector {
    data: SfxData,
    dpi_bits: u32,
    scene: SfxVectorScene,
}

struct SfxGpuPreviewResource {
    adapter: wgpu::Adapter,
    target_format: wgpu::TextureFormat,
    renderer: Option<SfxGpuRenderer>,
    failed: bool,
    cached_vec: Option<CachedVector>,
    last_render_key: Option<(SfxData, u32, [u64; 4], [u32; 2])>,
}

impl SfxGpuPreviewResource {
    fn new(rs: &RenderState) -> Self {
        Self {
            adapter: rs.adapter.clone(),
            target_format: rs.target_format,
            renderer: None,
            failed: false,
            cached_vec: None,
            last_render_key: None,
        }
    }

    fn ensure_renderer(&mut self, device: &wgpu::Device) -> Option<&mut SfxGpuRenderer> {
        if self.failed {
            return None;
        }
        if self.renderer.is_none() {
            match SfxGpuRenderer::new_for_adapter(&self.adapter, device) {
                Ok(r) => self.renderer = Some(r),
                Err(e) => {
                    log::warn!("SFX GPU preview disabled (falling back to CPU): {}", e.0);
                    self.failed = true;
                    GPU_SFX_FAILED.store(true, Ordering::Relaxed);
                    return None;
                }
            }
        }
        self.renderer.as_mut()
    }
}

/// True when `rs` supports hardware SFX rendering and no runtime compile error has occurred.
pub fn is_supported(rs: &RenderState) -> bool {
    !GPU_SFX_FAILED.load(Ordering::Relaxed) && SfxGpuRenderer::supported(&rs.adapter)
}

/// Registers the zero-readback GPU preview resource on `rs` (Web + Native) and installs the
/// Native tiled full-resolution SFX commit hook on `photocraft-engine`.
pub fn init_for_render_state(rs: &RenderState) {
    if !SfxGpuRenderer::supported(&rs.adapter) {
        log::info!("SFX GPU renderer unavailable on adapter {:?}; using CPU oracle", rs.adapter.get_info().name);
        return;
    }
    GPU_SFX_FAILED.store(false, Ordering::Relaxed);
    rs.renderer.write().callback_resources.insert(SfxGpuPreviewResource::new(rs));
    log::info!(
        "SFX GPU preview enabled on {:?} ({:?}, MSAA {}x)",
        rs.adapter.get_info().backend,
        rs.adapter.get_info().name,
        SfxGpuRenderer::preferred_msaa(&rs.adapter)
    );

    #[cfg(not(target_arch = "wasm32"))]
    install_native_engine_hook(rs);
}

fn build_gpu_scene<'a>(data: &'a SfxData, vec_scene: &'a SfxVectorScene) -> SfxGpuScene<'a> {
    let seg_contours = vec_scene
        .seg_contours
        .iter()
        .map(|c| SfxGpuSegContour { pts: &c.pts, total_len: c.total_len, phase: c.phase })
        .collect();
    SfxGpuScene {
        data,
        full_bounds: vec_scene.full_bounds,
        face_contours: &vec_scene.face_contours,
        line_polygons: &vec_scene.line_polygons,
        primary_shtack_polygons: &vec_scene.primary_shtack_polygons,
        secondary_shtack_polygons: &vec_scene.secondary_shtack_polygons,
        primary_dry_vector_cuts: &vec_scene.primary_dry_vector_cuts,
        effect_shtack_polygons: &vec_scene.effect_shtack_polygons,
        effect_dry_vector_cuts: &vec_scene.effect_dry_vector_cuts,
        seg_contours,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn install_native_engine_hook(rs: &RenderState) {
    use std::sync::Mutex;

    use photocraft_color::PixelFormat;
    use photocraft_manga_core::sfx_render::SfxRenderOutput;

    // Avoid blocking on software CPU adapters where CPU oracle is already faster without readback.
    if rs.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
        return;
    }
    let adapter = rs.adapter.clone();
    let device = rs.device.clone();
    let queue = rs.queue.clone();
    let state: Arc<Mutex<Option<Result<SfxGpuRenderer, ()>>>> = Arc::new(Mutex::new(None));

    let hook: photocraft_engine::cmd::manga_cmds::sfx::SfxGpuRenderHook = Arc::new(move |data: &SfxData, fmt: PixelFormat, dpi: f32| {
        if GPU_SFX_FAILED.load(Ordering::Relaxed) {
            return None;
        }
        let mut guard = state.lock().ok()?;
        let slot = guard.get_or_insert_with(|| match SfxGpuRenderer::new_for_adapter(&adapter, &device) {
            Ok(r) => Ok(r),
            Err(e) => {
                log::warn!("Native SFX GPU commit hook disabled: {}", e.0);
                GPU_SFX_FAILED.store(true, Ordering::Relaxed);
                Err(())
            }
        });
        let renderer = slot.as_mut().ok()?;
        let vec_scene = {
            let mut text = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
            extract_sfx_vector_scene(data, &mut text, dpi)
        };
        if vec_scene.full_bounds.is_empty() {
            return Some(SfxRenderOutput::Single {
                surface: photocraft_raster::Surface::new(fmt),
                bounds: photocraft_doc::Rect::EMPTY,
                geom: vec_scene.geom,
            });
        }
        let gpu_scene = build_gpu_scene(data, &vec_scene);
        match renderer.render_tiled_to_surface(&device, &queue, &gpu_scene, fmt) {
            Ok(surface) => {
                let bounds = surface.content_bounds();
                Some(SfxRenderOutput::Single { surface, bounds, geom: vec_scene.geom })
            }
            Err(e) => {
                log::warn!("Native SFX GPU tiled render failed (CPU fallback): {}", e.0);
                None
            }
        }
    });
    photocraft_engine::cmd::manga_cmds::sfx::set_sfx_gpu_renderer_hook(Some(hook));
}

fn max_preview_apron_doc(data: &SfxData) -> f64 {
    let mut pad = f64::from(data.line_effect.halo_width_px.max(0.0));
    if data.vector_shadow.enabled {
        pad = pad.max(f64::from(data.vector_shadow.distance_px.max(data.vector_shadow.extrude_depth_px)));
    }
    for fx in &data.blending.effects {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::Stroke { width_px, .. } => pad = pad.max(f64::from(*width_px)),
            SfxBlendEffect::DropShadow { distance_px, extrude_depth_px, blur_px, .. } => {
                pad = pad.max(f64::from(distance_px.max(*extrude_depth_px) + blur_px * 2.5));
            }
            _ => {}
        }
    }
    (pad + 16.0).min(256.0)
}

struct SfxGpuOverlayCallback {
    data: SfxData,
    dpi: f32,
    opacity: f32,
    xf: ViewXform,
    canvas_rect: egui::Rect,
}

impl CallbackTrait for SfxGpuOverlayCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<SfxGpuPreviewResource>() else { return Vec::new() };
        let dpi_bits = self.dpi.to_bits();
        let need_vec = res.cached_vec.as_ref().is_none_or(|c| c.dpi_bits != dpi_bits || c.data != self.data);
        if need_vec {
            let scene = {
                let mut text = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
                extract_sfx_vector_scene(&self.data, &mut text, self.dpi)
            };
            res.cached_vec = Some(CachedVector { data: self.data.clone(), dpi_bits, scene });
        }
        let target_format = res.target_format;
        let full = match res.cached_vec.as_ref() {
            Some(c) if !c.scene.full_bounds.is_empty() => c.scene.full_bounds,
            _ => {
                if let Some(r) = res.renderer.as_mut() {
                    r.prepare_blit(device, queue, target_format, None);
                }
                return Vec::new();
            }
        };

        // Visible canvas rectangle mapped into document space + effect apron, intersected with SFX bounds.
        let corners = [
            self.xf.to_doc(self.canvas_rect.left_top()),
            self.xf.to_doc(self.canvas_rect.right_top()),
            self.xf.to_doc(self.canvas_rect.right_bottom()),
            self.xf.to_doc(self.canvas_rect.left_bottom()),
        ];
        let mut vx0 = f64::INFINITY;
        let mut vy0 = f64::INFINITY;
        let mut vx1 = f64::NEG_INFINITY;
        let mut vy1 = f64::NEG_INFINITY;
        for c in corners {
            vx0 = vx0.min(c[0]);
            vy0 = vy0.min(c[1]);
            vx1 = vx1.max(c[0]);
            vy1 = vy1.max(c[1]);
        }
        let apron = max_preview_apron_doc(&self.data);
        let reg_x0 = (vx0 - apron).max(f64::from(full.x0)).floor();
        let reg_y0 = (vy0 - apron).max(f64::from(full.y0)).floor();
        let reg_x1 = (vx1 + apron).min(f64::from(full.x1)).ceil();
        let reg_y1 = (vy1 + apron).min(f64::from(full.y1)).ceil();
        let reg_w = reg_x1 - reg_x0;
        let reg_h = reg_y1 - reg_y0;
        if !(reg_w >= 1.0 && reg_h >= 1.0) {
            return Vec::new();
        }
        let reg_doc = [reg_x0, reg_y0, reg_x1, reg_y1];

        let ppp = screen.pixels_per_point.max(0.5);
        let max_dim = device.limits().max_texture_dimension_2d.min(MAX_PREVIEW_DIM);
        let desired_scale = f64::from((self.xf.zoom * ppp).clamp(0.125, 2.0));
        let mut tw = (reg_w * desired_scale).ceil().max(1.0) as u32;
        let mut th = (reg_h * desired_scale).ceil().max(1.0) as u32;
        if tw > max_dim || th > max_dim {
            let down = (f64::from(max_dim) / f64::from(tw.max(th))).min(1.0);
            tw = ((f64::from(tw) * down).floor() as u32).clamp(1, max_dim);
            th = ((f64::from(th) * down).floor() as u32).clamp(1, max_dim);
        }

        let key = (self.data.clone(), dpi_bits, reg_doc.map(f64::to_bits), [tw, th]);
        let need_render = res.last_render_key.as_ref() != Some(&key);

        if res.ensure_renderer(device).is_none() {
            return Vec::new();
        }
        if need_render {
            let (Some(renderer), Some(cached)) = (res.renderer.as_mut(), res.cached_vec.as_ref()) else {
                return Vec::new();
            };
            let gpu_scene = build_gpu_scene(&self.data, &cached.scene);
            if let Err(e) = renderer.render_region(device, queue, &gpu_scene, reg_doc, [tw, th]) {
                log::warn!("SFX GPU preview render_region failed: {}", e.0);
                return Vec::new();
            }
            res.last_render_key = Some(key);
        }

        // Exact affine transform from screen physical pixel (sx, sy) -> region UV (u, v).
        let p0 = self.xf.to_doc(egui::pos2(0.0, 0.0));
        let px1 = self.xf.to_doc(egui::pos2(1.0 / ppp, 0.0));
        let py1 = self.xf.to_doc(egui::pos2(0.0, 1.0 / ppp));
        let dx_dsx = (px1[0] - p0[0]) as f32;
        let dy_dsx = (px1[1] - p0[1]) as f32;
        let dx_dsy = (py1[0] - p0[0]) as f32;
        let dy_dsy = (py1[1] - p0[1]) as f32;
        let inv_w = 1.0 / (reg_w as f32).max(1e-4);
        let inv_h = 1.0 / (reg_h as f32).max(1e-4);
        let row_u = [dx_dsx * inv_w, dx_dsy * inv_w, ((p0[0] - reg_x0) as f32) * inv_w];
        let row_v = [dy_dsx * inv_h, dy_dsy * inv_h, ((p0[1] - reg_y0) as f32) * inv_h];

        if let Some(renderer) = res.renderer.as_mut() {
            renderer.prepare_blit(device, queue, target_format, Some((row_u, row_v, self.opacity)));
        }
        Vec::new()
    }

    fn paint(&self, info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &CallbackResources) {
        let Some(res) = resources.get::<SfxGpuPreviewResource>() else { return };
        let Some(renderer) = res.renderer.as_ref() else { return };
        let [sw, sh] = info.screen_size_px;
        let clip = info.clip_rect_in_pixels();
        if clip.width_px <= 0 || clip.height_px <= 0 || sw == 0 || sh == 0 {
            return;
        }
        pass.set_viewport(0.0, 0.0, sw as f32, sh as f32, 0.0, 1.0);
        pass.set_scissor_rect(clip.left_px.max(0) as u32, clip.top_px.max(0) as u32, clip.width_px as u32, clip.height_px as u32);
        renderer.draw_last_output_into_pass(pass, res.target_format);
    }
}

/// Schedules the zero-readback GPU SFX overlay inside `canvas_rect` (clipped to the canvas).
pub fn paint(painter: &egui::Painter, canvas_rect: egui::Rect, xf: ViewXform, data: SfxData, dpi: f32, opacity: f32) {
    if !canvas_rect.is_positive() {
        return;
    }
    painter.add(egui_wgpu::Callback::new_paint_callback(
        canvas_rect,
        SfxGpuOverlayCallback { data, dpi, opacity, xf, canvas_rect },
    ));
}
