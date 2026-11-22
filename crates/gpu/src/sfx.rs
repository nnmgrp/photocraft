//! Hardware-accelerated Manga SFX GPU renderer (`wgpu` fragment + Stencil-then-Cover).
//!
//! Works identically on WebGPU, WebGL2, DX12, Vulkan, and Metal (no compute shaders or storage
//! buffers, and every `Rgba32Float` texture is sampled strictly via `textureLoad`).
//!
//! Two entry points share one GPU pipeline:
//! - [`SfxGpuRenderer::render_region`]: renders any document-space rect into a GPU texture with
//!   **zero readback** — powers 60 FPS live viewport previews on 12K×17K canvases (Web + Native).
//! - [`SfxGpuRenderer::render_tiled_to_surface`] (native): renders arbitrarily large SFX bounds
//!   (including >8192 px lettering on 12 000 × 17 000 px pages) in overlapping padded GPU tiles
//!   and writes the finished straight-alpha pixels straight into a sparse [`Surface`].

use bytemuck::{Pod, Zeroable};
use photocraft_algo::comic_screentone::{DotShape, PatternType};
use photocraft_color::Color;
#[cfg(not(target_arch = "wasm32"))]
use photocraft_color::PixelFormat;
use photocraft_doc::sfx::{
    SfxBlendEffect, SfxChannelBlend, SfxData, SfxDryBrushEngine, SfxDryBrushSpec, SfxFillSource, SfxLineStyle, SfxShtackFillMode,
    SfxShtackFillSpec, SfxShtackVariant, SfxStrokePosition,
};
use photocraft_geom::Rect;
#[cfg(not(target_arch = "wasm32"))]
use photocraft_raster::{Surface, from_rgba_into};

use crate::Unsupported;

/// WGSL source for all Manga SFX GPU passes.
pub const SFX_SHADER: &str = include_str!("sfx.wgsl");

const MASK_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const JFA_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const RGBA_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const STENCIL_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24PlusStencil8;

/// Maximum single-pass tile dimension for tiled full-res renders on native (keeps VRAM bounded even on 12K×17K).
pub const MAX_SFX_TILE_DIM: u32 = 2048;
/// Maximum single-pass tile dimension on `wasm32` (4× smaller VRAM + staging readback footprint).
pub const MAX_SFX_TILE_DIM_WASM: u32 = 1024;
/// Capacity bucket step for [`WorkTargets`] reuse so sub-viewport drags perform 0 texture reallocations.
const WORK_TARGET_BUCKET_PX: u32 = 256;

/// Smoothed closed glyph contour with cumulative arc-length `pts: [x, y, s_arc]` for GPU JFA `Б1: Seg`.
#[derive(Clone, Copy, Debug)]
pub struct SfxGpuSegContour<'a> {
    pub pts: &'a [[f32; 3]],
    pub total_len: f32,
    pub phase: f32,
}

/// Document-space closed polygon ring `(x, y)`.
pub type SfxGpuPolyRing = Vec<(f64, f64)>;

/// Pure vector scene description handed to [`SfxGpuRenderer`] (zero pixel buffers, O(vertices)).
pub struct SfxGpuScene<'a> {
    pub data: &'a SfxData,
    pub full_bounds: Rect,
    /// Reference glyph height in document pixels (scales Dry Brush bristles & grain cleanly at 2400 DPI).
    pub glyph_em_px: f32,
    /// Glyph contours in document space (font winding preserved for NonZero Stencil-then-Cover).
    pub face_contours: &'a [SfxGpuPolyRing],
    /// Stylized outline / Shtack / Spikes / G-Pen / BrushStroke ink polygons (positive winding).
    pub line_polygons: &'a [SfxGpuPolyRing],
    /// Primary `ShtackHatch` ink polygons (positive winding).
    pub primary_shtack_polygons: &'a [SfxGpuPolyRing],
    /// Legacy secondary `ShtackHatch` ink polygons (positive winding).
    pub secondary_shtack_polygons: &'a [SfxGpuPolyRing],
    /// Per-effect `ShtackHatch` polygons keyed by `data.blending.effects` index.
    pub effect_shtack_polygons: &'a [(usize, Vec<SfxGpuPolyRing>)],
    /// Smoothed closed contours with arc-length `s` for GPU `Б1: Seg`.
    pub seg_contours: Vec<SfxGpuSegContour<'a>>,
    /// Primary `DryBrushEngine::Vector` cut polygons.
    pub primary_dry_vector_cuts: &'a [SfxGpuPolyRing],
    /// Per-effect `DryBrushEngine::Vector` cut polygons keyed by `data.blending.effects` index.
    pub effect_dry_vector_cuts: &'a [(usize, Vec<SfxGpuPolyRing>)],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct FxParams {
    p0: [f32; 4],
    p1: [f32; 4],
    p2: [f32; 4],
    p3: [f32; 4],
    c0: [f32; 4],
    c1: [f32; 4],
    u0: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PolyVert {
    pos: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SegVert {
    pos: [f32; 2],
    arc_phase: [f32; 2],
}

struct Tex2d {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Drop for Tex2d {
    fn drop(&mut self) {
        self.tex.destroy();
    }
}

impl Tex2d {
    fn new(device: &wgpu::Device, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, sample_count: u32, extra: wgpu::TextureUsages) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | extra,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self { tex, view }
    }
}

#[derive(Clone, Copy, Default)]
struct JfaUsage {
    ab: bool,
    ext: bool,
    int: bool,
    seg: bool,
}

impl JfaUsage {
    fn mark_slot(&mut self, slot: TexSlot) {
        match slot {
            TexSlot::JfaA | TexSlot::JfaB => self.ab = true,
            TexSlot::JfaExt => self.ext = true,
            TexSlot::JfaInt => self.int = true,
            TexSlot::JfaSeg => self.seg = true,
            _ => {}
        }
    }

    fn from_cmds(cmds: &[GpuCmd]) -> Self {
        let mut u = Self::default();
        for cmd in cmds {
            match *cmd {
                GpuCmd::RasterSegSeed { .. } => u.ab = true,
                GpuCmd::Quad { a, b, c, dst, .. } => {
                    u.mark_slot(a);
                    u.mark_slot(b);
                    u.mark_slot(c);
                    u.mark_slot(dst);
                }
                GpuCmd::CopyTex { src, dst } => {
                    u.mark_slot(src);
                    u.mark_slot(dst);
                }
                GpuCmd::ClearMask(_) | GpuCmd::RasterPolys { .. } => {}
            }
        }
        u
    }
}

fn round_cap_dim(dim: u32, max_dim: u32) -> u32 {
    let max_d = max_dim.max(1);
    let clamped = dim.clamp(1, max_d);
    clamped.div_ceil(WORK_TARGET_BUCKET_PX).saturating_mul(WORK_TARGET_BUCKET_PX).min(max_d)
}

struct WorkTargets {
    cap: [u32; 2],
    stencil: Tex2d,
    msaa_mask: Option<Tex2d>,
    face_mask: Tex2d,
    line_mask: Tex2d,
    union_mask: Tex2d,
    scratch_mask_a: Tex2d,
    scratch_mask_b: Tex2d,
    shtack_mask: Tex2d,
    jfa_a: Option<Tex2d>,
    jfa_b: Option<Tex2d>,
    jfa_ext: Option<Tex2d>,
    jfa_int: Option<Tex2d>,
    jfa_seg: Option<Tex2d>,
    accum_a: Tex2d,
    accum_b: Tex2d,
}

impl WorkTargets {
    fn new(device: &wgpu::Device, w: u32, h: u32, msaa_samples: u32, need_jfa: JfaUsage) -> Self {
        let bindable = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
        let stencil = Tex2d::new(device, "sfx-stencil", w, h, STENCIL_FMT, msaa_samples, wgpu::TextureUsages::empty());
        let msaa_mask = (msaa_samples > 1).then(|| Tex2d::new(device, "sfx-msaa-mask", w, h, MASK_FMT, msaa_samples, wgpu::TextureUsages::empty()));
        let mut wt = Self {
            cap: [w, h],
            stencil,
            msaa_mask,
            face_mask: Tex2d::new(device, "sfx-face", w, h, MASK_FMT, 1, bindable),
            line_mask: Tex2d::new(device, "sfx-line", w, h, MASK_FMT, 1, bindable),
            union_mask: Tex2d::new(device, "sfx-union", w, h, MASK_FMT, 1, bindable),
            scratch_mask_a: Tex2d::new(device, "sfx-scratch-a", w, h, MASK_FMT, 1, bindable),
            scratch_mask_b: Tex2d::new(device, "sfx-scratch-b", w, h, MASK_FMT, 1, bindable),
            shtack_mask: Tex2d::new(device, "sfx-shtack", w, h, MASK_FMT, 1, bindable),
            jfa_a: None,
            jfa_b: None,
            jfa_ext: None,
            jfa_int: None,
            jfa_seg: None,
            accum_a: Tex2d::new(device, "sfx-accum-a", w, h, RGBA_FMT, 1, bindable),
            accum_b: Tex2d::new(device, "sfx-accum-b", w, h, RGBA_FMT, 1, bindable),
        };
        wt.ensure_jfa(device, need_jfa);
        wt
    }

    fn ensure_jfa(&mut self, device: &wgpu::Device, need: JfaUsage) {
        let [w, h] = self.cap;
        let bindable = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
        if need.ab {
            if self.jfa_a.is_none() {
                self.jfa_a = Some(Tex2d::new(device, "sfx-jfa-a", w, h, JFA_FMT, 1, bindable));
            }
            if self.jfa_b.is_none() {
                self.jfa_b = Some(Tex2d::new(device, "sfx-jfa-b", w, h, JFA_FMT, 1, bindable));
            }
        }
        if need.ext && self.jfa_ext.is_none() {
            self.jfa_ext = Some(Tex2d::new(device, "sfx-jfa-ext", w, h, JFA_FMT, 1, bindable));
        }
        if need.int && self.jfa_int.is_none() {
            self.jfa_int = Some(Tex2d::new(device, "sfx-jfa-int", w, h, JFA_FMT, 1, bindable));
        }
        if need.seg && self.jfa_seg.is_none() {
            self.jfa_seg = Some(Tex2d::new(device, "sfx-jfa-seg", w, h, JFA_FMT, 1, bindable));
        }
    }
}

/// Dynamic uniform ring buffer so every pass in one `CommandEncoder` reads its own `FxParams`.
struct UniformRing {
    align: u32,
    bytes: Vec<u8>,
    buf: wgpu::Buffer,
    cap: usize,
}

impl Drop for UniformRing {
    fn drop(&mut self) {
        self.buf.destroy();
    }
}

impl UniformRing {
    fn new(device: &wgpu::Device) -> Self {
        let align = device.limits().min_uniform_buffer_offset_alignment.max(256);
        let cap = (align as usize) * 128;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sfx-fx-ring"),
            size: cap as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { align, bytes: Vec::with_capacity(cap), buf, cap }
    }

    fn reset(&mut self) {
        self.bytes.clear();
    }

    fn push(&mut self, params: FxParams) -> u32 {
        let off = self.bytes.len();
        let stride = self.align as usize;
        self.bytes.resize(off + stride, 0);
        let raw = bytemuck::bytes_of(&params);
        self.bytes[off..off + raw.len()].copy_from_slice(raw);
        off as u32
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.bytes.is_empty() {
            return;
        }
        if self.bytes.len() > self.cap {
            self.buf.destroy();
            self.cap = self.bytes.len().next_power_of_two();
            self.buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sfx-fx-ring"),
                size: self.cap as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buf, 0, &self.bytes);
    }
}

struct VertexArena {
    data: Vec<u8>,
    buf: wgpu::Buffer,
    cap: usize,
}

impl Drop for VertexArena {
    fn drop(&mut self) {
        self.buf.destroy();
    }
}

impl VertexArena {
    fn new(device: &wgpu::Device, label: &str) -> Self {
        let cap = 64 * 1024;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: cap as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { data: Vec::with_capacity(cap), buf, cap }
    }

    fn reset(&mut self) {
        self.data.clear();
    }

    fn push_slice<T: Pod>(&mut self, items: &[T]) -> (u64, u32) {
        let align = 4usize;
        let pad = (align - (self.data.len() % align)) % align;
        self.data.resize(self.data.len() + pad, 0);
        let off = self.data.len() as u64;
        self.data.extend_from_slice(bytemuck::cast_slice(items));
        (off, items.len() as u32)
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.data.is_empty() {
            return;
        }
        if self.data.len() > self.cap {
            self.buf.destroy();
            self.cap = self.data.len().next_power_of_two();
            self.buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sfx-vbuf"),
                size: self.cap as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buf, 0, &self.data);
    }
}

/// Hardware-accelerated Manga SFX GPU renderer (`wgpu`).
pub struct SfxGpuRenderer {
    max_dim: u32,
    msaa_samples: u32,
    shader: wgpu::ShaderModule,
    fx_bgl: wgpu::BindGroupLayout,
    raster_bgl: wgpu::BindGroupLayout,
    stencil_pipe: wgpu::RenderPipeline,
    cover_pipe: wgpu::RenderPipeline,
    seg_seed_pipe: wgpu::RenderPipeline,
    union_pipe: wgpu::RenderPipeline,
    mul_pipe: wgpu::RenderPipeline,
    one_bit_face_pipe: wgpu::RenderPipeline,
    one_bit_line_pipe: wgpu::RenderPipeline,
    jfa_init_pipe: wgpu::RenderPipeline,
    jfa_step_pipe: wgpu::RenderPipeline,
    blur_mask_pipe: wgpu::RenderPipeline,
    blur_jfa_pipe: wgpu::RenderPipeline,
    halo_pipe: wgpu::RenderPipeline,
    stroke_ring_pipe: wgpu::RenderPipeline,
    shadow_extrude_pipe: wgpu::RenderPipeline,
    dry_physics_pipe: wgpu::RenderPipeline,
    dry_seg_pipe: wgpu::RenderPipeline,
    etf_seed_pipe: wgpu::RenderPipeline,
    dry_etf_lic_pipe: wgpu::RenderPipeline,
    dry_grain_pipe: wgpu::RenderPipeline,
    composite_pipe: wgpu::RenderPipeline,
    blit_pipes: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    blit_uni: wgpu::Buffer,
    blit_bg: Option<wgpu::BindGroup>,
    dummy_tex: Tex2d,
    ring: UniformRing,
    vbuf: VertexArena,
    targets: Option<WorkTargets>,
    targets_alloc_count: u32,
    last_accum_is_a: bool,
    last_size: [u32; 2],
    #[cfg(not(target_arch = "wasm32"))]
    readback: Option<(wgpu::Buffer, u64)>,
}

impl Drop for SfxGpuRenderer {
    fn drop(&mut self) {
        self.blit_bg = None;
        self.targets = None;
        self.blit_uni.destroy();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((buf, _)) = self.readback.take() {
            buf.destroy();
        }
    }
}

fn color_rgba(c: Color) -> [f32; 4] {
    let rgb = c.to_rgb();
    [rgb[0], rgb[1], rgb[2], c.alpha]
}

fn blend_mode_code(m: SfxChannelBlend) -> u32 {
    match m {
        SfxChannelBlend::Normal => 0,
        SfxChannelBlend::Multiply => 1,
        SfxChannelBlend::ColorBurn => 2,
        SfxChannelBlend::Darken => 3,
        SfxChannelBlend::Screen => 4,
        SfxChannelBlend::ColorDodge => 5,
        SfxChannelBlend::Lighten => 6,
        SfxChannelBlend::Overlay => 7,
        SfxChannelBlend::SoftLight => 8,
        SfxChannelBlend::HardLight => 9,
        SfxChannelBlend::Difference => 10,
        SfxChannelBlend::Exclusion => 11,
        SfxChannelBlend::Knockout => 12,
    }
}

fn screentone_pattern_code(p: PatternType) -> u32 {
    match p {
        PatternType::Dots => 0,
        PatternType::Lines => 1,
        PatternType::Crosshatch => 2,
        PatternType::Noise => 3,
        _ => 0,
    }
}

fn screentone_shape_code(s: DotShape) -> u32 {
    match s {
        DotShape::Circle => 0,
        DotShape::Square => 1,
        DotShape::Diamond => 2,
        DotShape::Hexagon => 3,
    }
}

fn make_quad_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    fs_entry: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState { module: shader, entry_point: Some("vs_quad"), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fs_entry),
            targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn build_stencil_pipelines(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    raster_layout: &wgpu::PipelineLayout,
    empty_layout: &wgpu::PipelineLayout,
    msaa_samples: u32,
) -> (wgpu::RenderPipeline, wgpu::RenderPipeline) {
    let stencil_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sfx-stencil-wind"),
        layout: Some(raster_layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs_poly"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<PolyVert>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 }],
            })],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_stencil_noop"),
            targets: &[Some(wgpu::ColorTargetState { format: MASK_FMT, blend: None, write_mask: wgpu::ColorWrites::empty() })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: STENCIL_FMT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementWrap,
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::DecrementWrap,
                },
                read_mask: 0xFF,
                write_mask: 0xFF,
            },
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState { count: msaa_samples, ..Default::default() },
        multiview_mask: None,
        cache: None,
    });

    let cover_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("sfx-stencil-cover"),
        layout: Some(empty_layout),
        vertex: wgpu::VertexState { module: shader, entry_point: Some("vs_quad"), buffers: &[], compilation_options: Default::default() },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs_cover_solid"),
            targets: &[Some(wgpu::ColorTargetState { format: MASK_FMT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: STENCIL_FMT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::NotEqual,
                    fail_op: wgpu::StencilOperation::Zero,
                    depth_fail_op: wgpu::StencilOperation::Zero,
                    pass_op: wgpu::StencilOperation::Zero,
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::NotEqual,
                    fail_op: wgpu::StencilOperation::Zero,
                    depth_fail_op: wgpu::StencilOperation::Zero,
                    pass_op: wgpu::StencilOperation::Zero,
                },
                read_mask: 0xFF,
                write_mask: 0xFF,
            },
            bias: Default::default(),
        }),
        multisample: wgpu::MultisampleState { count: msaa_samples, ..Default::default() },
        multiview_mask: None,
        cache: None,
    });

    (stencil_pipe, cover_pipe)
}

fn polygons_to_fan_verts(polys: &[Vec<(f64, f64)>]) -> Vec<PolyVert> {
    let total_tris: usize = polys.iter().map(|p| p.len().saturating_sub(2)).sum();
    let mut out = Vec::with_capacity(total_tris * 3);
    for poly in polys {
        if poly.len() < 3 {
            continue;
        }
        let p0 = [poly[0].0 as f32, poly[0].1 as f32];
        for i in 1..(poly.len() - 1) {
            let p1 = [poly[i].0 as f32, poly[i].1 as f32];
            let p2 = [poly[i + 1].0 as f32, poly[i + 1].1 as f32];
            if p0[0].is_finite() && p0[1].is_finite() && p1[0].is_finite() && p1[1].is_finite() && p2[0].is_finite() && p2[1].is_finite() {
                out.push(PolyVert { pos: p0 });
                out.push(PolyVert { pos: p1 });
                out.push(PolyVert { pos: p2 });
            }
        }
    }
    out
}

fn seg_contours_to_line_verts(contours: &[SfxGpuSegContour<'_>]) -> Vec<SegVert> {
    let total_segs: usize = contours.iter().map(|c| c.pts.len().saturating_sub(1)).sum();
    let mut out = Vec::with_capacity(total_segs * 2);
    for c in contours {
        if c.pts.len() < 2 {
            continue;
        }
        for w in c.pts.windows(2) {
            out.push(SegVert { pos: [w[0][0], w[0][1]], arc_phase: [w[0][2], c.phase] });
            out.push(SegVert { pos: [w[1][0], w[1][1]], arc_phase: [w[1][2], c.phase] });
        }
    }
    out
}

#[derive(Clone, Copy)]
struct RegionInfo {
    doc_x0: f32,
    doc_y0: f32,
    doc_w: f32,
    doc_h: f32,
    px_w: u32,
    px_h: u32,
}

impl RegionInfo {
    fn doc_per_px_x(self) -> f32 {
        self.doc_w / (self.px_w.max(1) as f32)
    }

    fn doc_per_px_y(self) -> f32 {
        self.doc_h / (self.px_h.max(1) as f32)
    }

    fn px_per_doc(self) -> f32 {
        (self.px_w.max(1) as f32) / self.doc_w.max(1e-4)
    }

    fn base_params(self) -> FxParams {
        FxParams {
            p0: [self.doc_x0, self.doc_y0, self.doc_per_px_x(), self.doc_per_px_y()],
            p1: [self.px_w as f32, self.px_h as f32, 0.0, 0.0],
            ..Default::default()
        }
    }

    fn raster_params(self) -> FxParams {
        FxParams {
            p0: [self.doc_x0, self.doc_y0, 1.0 / self.doc_w.max(1e-4), 1.0 / self.doc_h.max(1e-4)],
            p1: [self.px_w as f32, self.px_h as f32, 0.0, 0.0],
            ..Default::default()
        }
    }
}

/// Deferred pass recorded against `WorkTargets` so all vertex/uniform uploads happen once before submit.
enum GpuCmd {
    ClearMask(MaskSlot),
    RasterPolys { target: MaskSlot, v_off: u64, v_count: u32, uni_off: u32 },
    RasterSegSeed { v_off: u64, v_count: u32, uni_off: u32 },
    Quad { pipe: QuadPipe, a: TexSlot, b: TexSlot, c: TexSlot, dst: TexSlot, uni_off: u32 },
    CopyTex { src: TexSlot, dst: TexSlot },
}

#[derive(Clone, Copy)]
enum MaskSlot {
    Face,
    Line,
    Union,
    Shtack,
    ScratchA,
    ScratchB,
}

#[derive(Clone, Copy)]
enum TexSlot {
    Dummy,
    Face,
    Line,
    Union,
    Shtack,
    ScratchA,
    ScratchB,
    JfaA,
    JfaB,
    JfaExt,
    JfaInt,
    JfaSeg,
    AccumA,
    AccumB,
}

#[derive(Clone, Copy)]
enum QuadPipe {
    Union,
    Mul,
    OneBitFace,
    OneBitLine,
    JfaInit,
    JfaStep,
    BlurMask,
    BlurJfa,
    Halo,
    StrokeRing,
    ShadowExtrude,
    DryPhysics,
    DrySeg,
    EtfSeed,
    DryEtfLic,
    DryGrain,
    Composite,
}

struct CmdRecorder<'a> {
    ring: &'a mut UniformRing,
    vbuf: &'a mut VertexArena,
    cmds: Vec<GpuCmd>,
    accum_is_a: bool,
    region: RegionInfo,
    full_bounds: Rect,
    auto_scale: f32,
}

impl<'a> CmdRecorder<'a> {
    fn cur_accum(&self) -> TexSlot {
        if self.accum_is_a { TexSlot::AccumA } else { TexSlot::AccumB }
    }

    fn next_accum(&mut self) -> (TexSlot, TexSlot) {
        let src = self.cur_accum();
        self.accum_is_a = !self.accum_is_a;
        let dst = self.cur_accum();
        (src, dst)
    }

    fn push_quad(&mut self, pipe: QuadPipe, a: TexSlot, b: TexSlot, c: TexSlot, dst: TexSlot, params: FxParams) {
        let uni_off = self.ring.push(params);
        self.cmds.push(GpuCmd::Quad { pipe, a, b, c, dst, uni_off });
    }

    fn raster_polys(&mut self, target: MaskSlot, polys: &[Vec<(f64, f64)>]) {
        let verts = polygons_to_fan_verts(polys);
        if verts.is_empty() {
            self.cmds.push(GpuCmd::ClearMask(target));
            return;
        }
        let (v_off, v_count) = self.vbuf.push_slice(&verts);
        let uni_off = self.ring.push(self.region.raster_params());
        self.cmds.push(GpuCmd::RasterPolys { target, v_off, v_count, uni_off });
    }

    fn run_jfa_from_mask(&mut self, src_mask: TexSlot, invert: bool, max_radius_px: f32, dst_jfa: TexSlot) {
        let mut init_p = self.region.base_params();
        init_p.u0[0] = u32::from(invert);
        self.push_quad(QuadPipe::JfaInit, src_mask, TexSlot::Dummy, TexSlot::Dummy, TexSlot::JfaA, init_p);
        self.run_jfa_steps(max_radius_px, dst_jfa);
    }

    fn run_jfa_steps(&mut self, max_radius_px: f32, dst_jfa: TexSlot) {
        let max_side = self.region.px_w.max(self.region.px_h).max(1);
        let reach = (max_radius_px.ceil() as u32).clamp(2, max_side);
        let mut step = reach.next_power_of_two() >> 1;
        step = step.max(1);
        let mut src_is_a = true;
        while step >= 1 {
            let mut p = self.region.base_params();
            p.u0[3] = step;
            let (s, d) = if src_is_a { (TexSlot::JfaA, TexSlot::JfaB) } else { (TexSlot::JfaB, TexSlot::JfaA) };
            self.push_quad(QuadPipe::JfaStep, s, TexSlot::Dummy, TexSlot::Dummy, d, p);
            src_is_a = !src_is_a;
            if step == 1 {
                break;
            }
            step >>= 1;
        }
        // Final 1+JFA refinement step for exact 1px Voronoi edges.
        let mut p = self.region.base_params();
        p.u0[3] = 1;
        let final_src = if src_is_a { TexSlot::JfaA } else { TexSlot::JfaB };
        self.push_quad(QuadPipe::JfaStep, final_src, TexSlot::Dummy, TexSlot::Dummy, dst_jfa, p);
    }

    fn blur_mask_in_place(&mut self, slot_a: TexSlot, slot_b: TexSlot, sigma_doc_px: f32) {
        let rad = ((sigma_doc_px * self.region.px_per_doc()).clamp(0.0, 96.0) * 0.75).round() as u32;
        if rad == 0 {
            return;
        }
        for _ in 0..2 {
            let mut ph = self.region.base_params();
            ph.u0 = [0, 0, 0, rad];
            self.push_quad(QuadPipe::BlurMask, slot_a, TexSlot::Dummy, TexSlot::Dummy, slot_b, ph);
            let mut pv = self.region.base_params();
            pv.u0 = [1, 0, 0, rad];
            self.push_quad(QuadPipe::BlurMask, slot_b, TexSlot::Dummy, TexSlot::Dummy, slot_a, pv);
        }
    }

    fn composite_solid(&mut self, cov: TexSlot, color: Color, opacity: f32, blend: SfxChannelBlend) {
        if opacity <= 1e-4 {
            return;
        }
        let (src_acc, dst_acc) = self.next_accum();
        let mut p = self.region.base_params();
        p.p1[2] = opacity.clamp(0.0, 1.0);
        p.c0 = color_rgba(color);
        p.u0 = [blend_mode_code(blend), 0, 0, 0];
        self.push_quad(QuadPipe::Composite, src_acc, cov, TexSlot::Dummy, dst_acc, p);
    }

    fn composite_fill_paint(&mut self, fill: &SfxFillSource, cov: TexSlot, opacity: f32, blend: SfxChannelBlend) {
        if opacity <= 1e-4 {
            return;
        }
        let (src_acc, dst_acc) = self.next_accum();
        let mut p = self.region.base_params();
        p.p1[2] = opacity.clamp(0.0, 1.0);
        match fill {
            SfxFillSource::None => return,
            SfxFillSource::Solid { color } | SfxFillSource::DryBrush { color, .. } => {
                p.c0 = color_rgba(*color);
                p.u0 = [blend_mode_code(blend), 0, 0, 0];
            }
            SfxFillSource::ShtackHatch { spec } => {
                p.c0 = color_rgba(spec.color);
                p.u0 = [blend_mode_code(blend), 0, 0, 0];
            }
            SfxFillSource::Gradient { start, end, angle_deg } => {
                let w = f64::from(self.full_bounds.width()).max(1.0);
                let h = f64::from(self.full_bounds.height()).max(1.0);
                let cx = f64::from(self.full_bounds.x0) + w * 0.5;
                let cy = f64::from(self.full_bounds.y0) + h * 0.5;
                let (sin_a, cos_a) = f64::from(*angle_deg).to_radians().sin_cos();
                let span = (w * cos_a.abs() + h * sin_a.abs()).max(1.0);
                p.p2 = [cx as f32, cy as f32, cos_a as f32, sin_a as f32];
                p.p3[0] = (1.0 / span) as f32;
                p.c0 = color_rgba(*start);
                p.c1 = color_rgba(*end);
                p.u0 = [blend_mode_code(blend), 0, 0, 1];
            }
            SfxFillSource::Screentone { spec, opacity: tone_op } => {
                let (sin_a, cos_a) = spec.angle.to_radians().sin_cos();
                p.p2 = [spec.dot_size, spec.spacing_x, spec.spacing_y, spec.density];
                p.p3 = [cos_a, sin_a, spec.line_width, tone_op.clamp(0.0, 1.0)];
                p.c0 = [0.0, 0.0, 0.0, 1.0];
                p.u0 = [blend_mode_code(blend), screentone_pattern_code(spec.pattern_type), screentone_shape_code(spec.dot_shape), 2];
            }
        }
        self.push_quad(QuadPipe::Composite, src_acc, cov, TexSlot::Dummy, dst_acc, p);
    }

    fn apply_dry_brush(
        &mut self,
        src_cov: TexSlot,
        dst_cov: TexSlot,
        db: &SfxDryBrushSpec,
        is_binary: bool,
        vec_cuts: &[Vec<(f64, f64)>],
        has_seg_jfa: bool,
    ) {
        let bristle_scale = (self.auto_scale * (db.bristle_size_pct / 100.0).clamp(0.1, 5.0)).max(0.1);
        let inv_bs = 1.0 / bristle_scale;
        let mut p = self.region.base_params();
        p.p1[2] = db.dryness_pct / 100.0;
        p.p2[0] = (db.warp_pct / 100.0) * 12.0;
        p.p2[1] = db.paper_grain_pct / 100.0;
        p.p2[2] = db.edge_wear_pct / 100.0;
        p.p2[3] = (db.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
        p.u0[2] = u32::from(is_binary);

        match db.engine {
            SfxDryBrushEngine::SdfPhysics => {
                let (sin_a, cos_a) = db.angle_deg.to_radians().sin_cos();
                p.p1[3] = 0.08 + (db.freq_pct / 100.0) * 0.42;
                p.p3 = [cos_a, sin_a, inv_bs, 0.0];
                p.u0[1] = db.seed.wrapping_mul(7_919);
                self.push_quad(QuadPipe::DryPhysics, src_cov, TexSlot::JfaInt, TexSlot::Dummy, dst_cov, p);
            }
            SfxDryBrushEngine::SdfSeg if has_seg_jfa => {
                p.p1[3] = 0.09 + (db.freq_pct / 100.0) * 0.42;
                p.p3 = [0.0, 0.0, inv_bs, 0.0];
                p.u0[1] = db.seed.wrapping_mul(13_109);
                self.push_quad(QuadPipe::DrySeg, src_cov, TexSlot::JfaSeg, TexSlot::JfaInt, dst_cov, p);
            }
            SfxDryBrushEngine::SdfSeg => {
                let (sin_a, cos_a) = db.angle_deg.to_radians().sin_cos();
                p.p1[3] = 0.08 + (db.freq_pct / 100.0) * 0.42;
                p.p3 = [cos_a, sin_a, inv_bs, 0.0];
                p.u0[1] = db.seed.wrapping_mul(7_919);
                self.push_quad(QuadPipe::DryPhysics, src_cov, TexSlot::JfaInt, TexSlot::Dummy, dst_cov, p);
            }
            SfxDryBrushEngine::SdfEtf => {
                // Seed tangent field from interior JFA -> 2-pass harmonic box blur -> LIC integration.
                let sp = self.region.base_params();
                self.push_quad(QuadPipe::EtfSeed, TexSlot::JfaInt, TexSlot::Dummy, TexSlot::Dummy, TexSlot::JfaA, sp);
                for _ in 0..2 {
                    let mut ph = self.region.base_params();
                    ph.u0 = [0, 0, 0, 5];
                    self.push_quad(QuadPipe::BlurJfa, TexSlot::JfaA, TexSlot::Dummy, TexSlot::Dummy, TexSlot::JfaB, ph);
                    let mut pv = self.region.base_params();
                    pv.u0 = [1, 0, 0, 5];
                    self.push_quad(QuadPipe::BlurJfa, TexSlot::JfaB, TexSlot::Dummy, TexSlot::Dummy, TexSlot::JfaA, pv);
                }
                p.p1[3] = 0.12 + (db.freq_pct / 100.0) * 0.45;
                p.u0[1] = db.seed.wrapping_mul(19_417);
                let lic_target_px = (db.lic_length_px * self.region.px_per_doc()).max(4.0);
                let steps = (lic_target_px * 0.5).round().clamp(2.0, 13.0) as u32;
                let stride_px = (lic_target_px / (2.0 * steps as f32)).max(1.15);
                p.p3 = [0.0, 0.0, inv_bs, stride_px];
                p.u0[3] = steps;
                self.push_quad(QuadPipe::DryEtfLic, src_cov, TexSlot::JfaA, TexSlot::JfaInt, dst_cov, p);
            }
            SfxDryBrushEngine::Vector => {
                self.raster_polys(MaskSlot::ScratchB, vec_cuts);
                let mut mp = self.region.base_params();
                mp.u0 = [1, 0, u32::from(is_binary), 0];
                self.push_quad(QuadPipe::Mul, src_cov, TexSlot::ScratchB, TexSlot::Dummy, dst_cov, mp);
            }
        }
    }

    fn raster_shtack_fill_mask(&mut self, polys: &[Vec<(f64, f64)>], spec: &SfxShtackFillSpec, face_slot: TexSlot, dst: MaskSlot) {
        self.raster_polys(dst, polys);
        let dst_tex = match dst {
            MaskSlot::Shtack => TexSlot::Shtack,
            _ => TexSlot::ScratchA,
        };
        if spec.overshoot_pct <= 0.1 {
            let mp = self.region.base_params();
            self.push_quad(QuadPipe::Mul, dst_tex, face_slot, TexSlot::Dummy, TexSlot::ScratchB, mp);
            self.cmds.push(GpuCmd::CopyTex { src: TexSlot::ScratchB, dst: dst_tex });
        }
        let inv_gs = 1.0 / self.auto_scale.max(0.75);
        if spec.mode == SfxShtackFillMode::UkiyoBrush {
            let eff_dry = spec.dryness_pct.max(18.0);
            self.run_jfa_from_mask(dst_tex, true, (16.0 * self.auto_scale * self.region.px_per_doc()).clamp(16.0, 128.0), TexSlot::JfaB);
            let mut gp = self.region.base_params();
            gp.p1[2] = (eff_dry / 100.0).clamp(0.0, 1.0);
            gp.p1[3] = (spec.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
            gp.p3[2] = inv_gs;
            gp.u0 = [1, spec.seed.wrapping_mul(11_173), 0, 0];
            self.push_quad(QuadPipe::DryGrain, dst_tex, TexSlot::JfaB, TexSlot::Dummy, TexSlot::ScratchB, gp);
            self.cmds.push(GpuCmd::CopyTex { src: TexSlot::ScratchB, dst: dst_tex });
        } else if spec.dryness_pct > 0.5 {
            let mut gp = self.region.base_params();
            gp.p1[2] = (spec.dryness_pct / 100.0).clamp(0.0, 1.0);
            gp.p1[3] = (spec.grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
            gp.p3[2] = inv_gs;
            gp.u0 = [0, spec.seed.wrapping_mul(7_919), 0, 0];
            self.push_quad(QuadPipe::DryGrain, dst_tex, TexSlot::Dummy, TexSlot::Dummy, TexSlot::ScratchB, gp);
            self.cmds.push(GpuCmd::CopyTex { src: TexSlot::ScratchB, dst: dst_tex });
        }
    }
}

struct FillPassArgs<'a> {
    fill: &'a SfxFillSource,
    base_cov: TexSlot,
    dry_override: Option<&'a SfxDryBrushSpec>,
    shtack_polys: &'a [Vec<(f64, f64)>],
    vec_cuts: &'a [Vec<(f64, f64)>],
    opacity: f32,
    blend: SfxChannelBlend,
    is_binary: bool,
    has_seg_jfa: bool,
}

struct ShadowPassArgs {
    union_slot: TexSlot,
    angle_deg: f32,
    dist: f32,
    extrude: f32,
    blur: f32,
    color: Color,
    opacity: f32,
    blend: SfxChannelBlend,
}

impl<'a> CmdRecorder<'a> {
    fn render_fill_pass(&mut self, args: FillPassArgs<'_>) {
        if args.opacity <= 1e-4 {
            return;
        }
        match args.fill {
            SfxFillSource::None => {}
            SfxFillSource::ShtackHatch { spec } => {
                self.raster_shtack_fill_mask(args.shtack_polys, spec, args.base_cov, MaskSlot::ScratchA);
                self.composite_solid(TexSlot::ScratchA, spec.color, args.opacity, args.blend);
            }
            SfxFillSource::DryBrush { color, spec } => {
                let mut db = *spec;
                db.enabled = true;
                self.apply_dry_brush(args.base_cov, TexSlot::ScratchA, &db, args.is_binary, args.vec_cuts, args.has_seg_jfa);
                self.composite_solid(TexSlot::ScratchA, *color, args.opacity, args.blend);
            }
            SfxFillSource::Solid { .. } | SfxFillSource::Gradient { .. } | SfxFillSource::Screentone { .. } => {
                let cov_slot = if let Some(db) = args.dry_override.filter(|d| d.enabled) {
                    self.apply_dry_brush(args.base_cov, TexSlot::ScratchA, db, args.is_binary, args.vec_cuts, args.has_seg_jfa);
                    TexSlot::ScratchA
                } else {
                    args.base_cov
                };
                self.composite_fill_paint(args.fill, cov_slot, args.opacity, args.blend);
            }
        }
    }
}

impl SfxGpuRenderer {
    /// True when `adapter` supports rendering `Rgba32Float`, `Rgba8Unorm`, and `Depth24PlusStencil8`.
    pub fn supported(adapter: &wgpu::Adapter) -> bool {
        let ok = |fmt: wgpu::TextureFormat| {
            adapter
                .get_texture_format_features(fmt)
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        };
        ok(MASK_FMT) && ok(JFA_FMT) && ok(STENCIL_FMT)
    }

    /// Returns 4 when `adapter` supports 4× MSAA on both mask and stencil formats, otherwise 1.
    pub fn preferred_msaa(adapter: &wgpu::Adapter) -> u32 {
        let has_x4 = |fmt: wgpu::TextureFormat| {
            adapter.get_texture_format_features(fmt).flags.contains(wgpu::TextureFormatFeatureFlags::MULTISAMPLE_X4)
        };
        if has_x4(MASK_FMT) && has_x4(STENCIL_FMT) { 4 } else { 1 }
    }

    /// Compiles all SFX fragment + Stencil-then-Cover pipelines using `adapter`'s optimal MSAA.
    pub fn new_for_adapter(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Result<Self, Unsupported> {
        if !Self::supported(adapter) {
            return Err(Unsupported("adapter cannot render SFX formats (Rgba32Float/Rgba8Unorm/Depth24PlusStencil8)".into()));
        }
        Self::new_with_msaa(device, Self::preferred_msaa(adapter))
    }

    /// Compiles all SFX fragment + Stencil-then-Cover pipelines on `device` (4× MSAA).
    pub fn new(device: &wgpu::Device) -> Result<Self, Unsupported> {
        Self::new_with_msaa(device, 4)
    }

    fn new_with_msaa(device: &wgpu::Device, msaa_samples: u32) -> Result<Self, Unsupported> {
        let msaa_samples = if msaa_samples >= 4 { 4 } else { 1 };
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("photocraft-sfx-wgsl"),
            source: wgpu::ShaderSource::Wgsl(SFX_SHADER.into()),
        });
        let fx_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sfx-fx-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<FxParams>() as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let raster_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sfx-raster-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(std::mem::size_of::<FxParams>() as u64),
                },
                count: None,
            }],
        });
        let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sfx-fx-layout"),
            bind_group_layouts: &[Some(&fx_bgl)],
            immediate_size: 0,
        });
        let raster_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sfx-raster-layout"),
            bind_group_layouts: &[Some(&raster_bgl)],
            immediate_size: 0,
        });
        let empty_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sfx-empty-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });

        let (stencil_pipe, cover_pipe) = build_stencil_pipelines(device, &shader, &raster_layout, &empty_layout, msaa_samples);

        let seg_seed_pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sfx-seg-seed"),
            layout: Some(&raster_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_seg_line"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SegVert>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 8, shader_location: 1 },
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_jfa_init_seg"),
                targets: &[Some(wgpu::ColorTargetState { format: JFA_FMT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let qp = |lbl: &str, fs: &str, fmt: wgpu::TextureFormat| make_quad_pipeline(device, lbl, &fx_layout, &shader, fs, fmt);
        let dummy_tex = Tex2d::new(device, "sfx-dummy", 1, 1, MASK_FMT, 1, wgpu::TextureUsages::TEXTURE_BINDING);
        let blit_uni = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sfx-blit-uni"),
            size: std::mem::size_of::<FxParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let max_dim = device.limits().max_texture_dimension_2d.min(8192);

        Ok(Self {
            max_dim,
            msaa_samples,
            union_pipe: qp("sfx-union", "fs_union_mask", MASK_FMT),
            mul_pipe: qp("sfx-mul", "fs_mul_mask", MASK_FMT),
            one_bit_face_pipe: qp("sfx-1bit-face", "fs_one_bit_face", MASK_FMT),
            one_bit_line_pipe: qp("sfx-1bit-line", "fs_one_bit_line_mask", MASK_FMT),
            jfa_init_pipe: qp("sfx-jfa-init", "fs_jfa_init_mask", JFA_FMT),
            jfa_step_pipe: qp("sfx-jfa-step", "fs_jfa_step", JFA_FMT),
            blur_mask_pipe: qp("sfx-blur-mask", "fs_box_blur_1d", MASK_FMT),
            blur_jfa_pipe: qp("sfx-blur-jfa", "fs_box_blur_1d", JFA_FMT),
            halo_pipe: qp("sfx-halo", "fs_halo_mask", MASK_FMT),
            stroke_ring_pipe: qp("sfx-stroke-ring", "fs_stroke_ring_mask", MASK_FMT),
            shadow_extrude_pipe: qp("sfx-shadow-ext", "fs_shadow_extrude_mask", MASK_FMT),
            dry_physics_pipe: qp("sfx-dry-phys", "fs_dry_physics", MASK_FMT),
            dry_seg_pipe: qp("sfx-dry-seg", "fs_dry_seg", MASK_FMT),
            etf_seed_pipe: qp("sfx-etf-seed", "fs_etf_seed", JFA_FMT),
            dry_etf_lic_pipe: qp("sfx-dry-lic", "fs_dry_etf_lic", MASK_FMT),
            dry_grain_pipe: qp("sfx-dry-grain", "fs_dry_grain_breakup", MASK_FMT),
            composite_pipe: qp("sfx-composite", "fs_composite_fill", RGBA_FMT),
            blit_pipes: Vec::new(),
            blit_uni,
            blit_bg: None,
            shader,
            fx_bgl,
            raster_bgl,
            stencil_pipe,
            cover_pipe,
            seg_seed_pipe,
            dummy_tex,
            ring: UniformRing::new(device),
            vbuf: VertexArena::new(device, "sfx-vbuf"),
            targets: None,
            targets_alloc_count: 0,
            last_accum_is_a: true,
            last_size: [0, 0],
            #[cfg(not(target_arch = "wasm32"))]
            readback: None,
        })
    }

    pub fn max_texture_dim(&self) -> u32 {
        self.max_dim
    }

    /// Caps the maximum texture capacity allowed for this renderer (e.g. `1280` for interactive
    /// previews or `1024` for `wasm32` tiled bakes). Releases existing targets if they exceed it.
    pub fn set_max_dim(&mut self, max_dim: u32) {
        let clamped = max_dim.max(WORK_TARGET_BUCKET_PX);
        self.max_dim = self.max_dim.min(clamped);
        if self.targets.as_ref().is_some_and(|t| t.cap[0] > self.max_dim || t.cap[1] > self.max_dim) {
            self.release_targets();
        }
    }

    /// Immediately destroys all pooled working textures (freeing VRAM right away on WebGPU/Dawn).
    pub fn release_targets(&mut self) {
        self.blit_bg = None;
        self.targets = None;
        self.last_size = [0, 0];
        #[cfg(not(target_arch = "wasm32"))]
        if let Some((buf, _)) = self.readback.take() {
            buf.destroy();
        }
    }

    /// Number of times [`WorkTargets`] has been freshly allocated (monotonic; used to verify reuse).
    pub fn targets_alloc_count(&self) -> u32 {
        self.targets_alloc_count
    }

    /// Current allocated capacity `[cap_w, cap_h]` of the working target pool, if any.
    pub fn targets_capacity(&self) -> Option<[u32; 2]> {
        self.targets.as_ref().map(|t| t.cap)
    }

    /// Returns the view of the most recently rendered straight-alpha RGBA texture.
    pub fn last_output_view(&self) -> Option<&wgpu::TextureView> {
        let t = self.targets.as_ref()?;
        if self.last_size[0] == 0 || self.last_size[1] == 0 {
            return None;
        }
        Some(if self.last_accum_is_a { &t.accum_a.view } else { &t.accum_b.view })
    }

    /// Prepares the premultiplied blit pipeline, uniform transform, and bind group for
    /// [`Self::draw_last_output_into_pass`]. Call inside `egui_wgpu::CallbackTrait::prepare`.
    ///
    /// When `screen_to_uv` is `Some((row_u, row_v, opacity))`, the blit shader maps each screen
    /// pixel `(sx, sy)` via `u = row_u[0]*sx + row_u[1]*sy + row_u[2]`,
    /// `v = row_v[0]*sx + row_v[1]*sy + row_v[2]`, handling pan, zoom, rotation, flip, and opacity.
    pub fn prepare_blit(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target_format: wgpu::TextureFormat,
        screen_to_uv: Option<([f32; 3], [f32; 3], f32)>,
    ) {
        if self.targets.is_none() || self.last_size[0] == 0 || self.last_size[1] == 0 {
            self.blit_bg = None;
            return;
        }
        if !self.blit_pipes.iter().any(|(f, _)| *f == target_format) {
            let fx_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("sfx-blit-layout"),
                bind_group_layouts: &[Some(&self.fx_bgl)],
                immediate_size: 0,
            });
            let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("sfx-blit-premul"),
                layout: Some(&fx_layout),
                vertex: wgpu::VertexState {
                    module: &self.shader,
                    entry_point: Some("vs_quad"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.shader,
                    entry_point: Some("fs_blit_premul"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: target_format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            self.blit_pipes.push((target_format, pipe));
        }
        let mut params = FxParams::default();
        params.u0[1] = u32::from(target_format.is_srgb());
        params.p2[1] = self.last_size[0] as f32;
        params.p2[2] = self.last_size[1] as f32;
        if let Some((ru, rv, opacity)) = screen_to_uv {
            params.u0[0] = 1;
            params.p0 = [ru[0], ru[1], ru[2], 0.0];
            params.p1 = [rv[0], rv[1], rv[2], 0.0];
            params.p2[0] = opacity.clamp(0.0, 1.0);
        }
        queue.write_buffer(&self.blit_uni, 0, bytemuck::bytes_of(&params));
        let Some(t) = self.targets.as_ref() else {
            self.blit_bg = None;
            return;
        };
        let out_view = if self.last_accum_is_a { &t.accum_a.view } else { &t.accum_b.view };
        self.blit_bg = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sfx-blit-bg"),
            layout: &self.fx_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.blit_uni,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<FxParams>() as u64),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(out_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.dummy_tex.view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&self.dummy_tex.view) },
            ],
        }));
    }

    /// Draws the last rendered GPU SFX output prepared by [`Self::prepare_blit`] onto an
    /// egui/canvas render pass with premultiplied alpha. Safe to call with `&self` in `paint`.
    pub fn draw_last_output_into_pass(&self, rpass: &mut wgpu::RenderPass<'static>, target_format: wgpu::TextureFormat) {
        let Some(bg) = self.blit_bg.as_ref() else { return };
        let Some((_, pipe)) = self.blit_pipes.iter().find(|(f, _)| *f == target_format) else { return };
        rpass.set_pipeline(pipe);
        rpass.set_bind_group(0, bg, &[0]);
        rpass.draw(0..3, 0..1);
    }

    /// Renders `region_doc = [x0, y0, x1, y1]` into an internal `target_px = [w, h]` GPU texture
    /// with **zero readback** (works on both WebGPU/WebGL2 and Native).
    pub fn render_region(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &SfxGpuScene<'_>,
        region_doc: [f64; 4],
        target_px: [u32; 2],
    ) -> Result<&wgpu::TextureView, Unsupported> {
        let w = target_px[0].clamp(1, self.max_dim);
        let h = target_px[1].clamp(1, self.max_dim);
        let doc_w = (region_doc[2] - region_doc[0]).abs().max(1e-3);
        let doc_h = (region_doc[3] - region_doc[1]).abs().max(1e-3);
        let region = RegionInfo {
            doc_x0: region_doc[0] as f32,
            doc_y0: region_doc[1] as f32,
            doc_w: doc_w as f32,
            doc_h: doc_h as f32,
            px_w: w,
            px_h: h,
        };
        let auto_scale = (scene.glyph_em_px / 120.0).clamp(0.75, 64.0);
        self.ring.reset();
        self.vbuf.reset();
        let (cmds, accum_is_a) = {
            let mut rec = CmdRecorder {
                ring: &mut self.ring,
                vbuf: &mut self.vbuf,
                cmds: Vec::with_capacity(64),
                accum_is_a: true,
                region,
                full_bounds: scene.full_bounds,
                auto_scale,
            };
            record_scene_passes(&mut rec, scene);
            (rec.cmds, rec.accum_is_a)
        };
        let jfa_need = JfaUsage::from_cmds(&cmds);

        // Reuse pooled WorkTargets whenever the requested active `[w, h]` fits inside current
        // capacity. Any resize drops (and explicitly `.destroy()`s) the old textures BEFORE
        // creating the new `CommandEncoder`, so no in-flight encoder ever references a destroyed
        // texture.
        let fits = self.targets.as_ref().is_some_and(|t| t.cap[0] >= w && t.cap[1] >= h);
        if fits {
            if let Some(t) = self.targets.as_mut() {
                t.ensure_jfa(device, jfa_need);
            }
        } else {
            let prev_cap = self.targets.as_ref().map_or([0, 0], |t| t.cap);
            self.blit_bg = None;
            self.targets = None;
            let cap_w = round_cap_dim(w.max(prev_cap[0]), self.max_dim);
            let cap_h = round_cap_dim(h.max(prev_cap[1]), self.max_dim);
            self.targets = Some(WorkTargets::new(device, cap_w, cap_h, self.msaa_samples, jfa_need));
            self.targets_alloc_count = self.targets_alloc_count.saturating_add(1);
        }

        self.ring.upload(device, queue);
        self.vbuf.upload(device, queue);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sfx-gpu-encoder") });
        self.execute_commands(device, &mut encoder, &cmds, [w, h]);
        queue.submit(Some(encoder.finish()));

        self.last_accum_is_a = accum_is_a;
        self.last_size = [w, h];
        self.last_output_view().ok_or_else(|| Unsupported("SFX GPU target missing".into()))
    }

    fn execute_commands(&self, device: &wgpu::Device, enc: &mut wgpu::CommandEncoder, cmds: &[GpuCmd], active_size: [u32; 2]) {
        let Some(t) = self.targets.as_ref() else { return };
        let [w, h] = active_size;
        // Clear initial RGBA accumulator A to transparent black.
        {
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sfx-clear-accum"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.accum_a.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }

        let raster_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sfx-raster-bg"),
            layout: &self.raster_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &self.ring.buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(std::mem::size_of::<FxParams>() as u64),
                }),
            }],
        });

        let mask_tex = |m: MaskSlot| -> &Tex2d {
            match m {
                MaskSlot::Face => &t.face_mask,
                MaskSlot::Line => &t.line_mask,
                MaskSlot::Union => &t.union_mask,
                MaskSlot::Shtack => &t.shtack_mask,
                MaskSlot::ScratchA => &t.scratch_mask_a,
                MaskSlot::ScratchB => &t.scratch_mask_b,
            }
        };
        let slot_tex = |s: TexSlot| -> &Tex2d {
            match s {
                TexSlot::Dummy => &self.dummy_tex,
                TexSlot::Face => &t.face_mask,
                TexSlot::Line => &t.line_mask,
                TexSlot::Union => &t.union_mask,
                TexSlot::Shtack => &t.shtack_mask,
                TexSlot::ScratchA => &t.scratch_mask_a,
                TexSlot::ScratchB => &t.scratch_mask_b,
                TexSlot::JfaA => t.jfa_a.as_ref().unwrap_or(&self.dummy_tex),
                TexSlot::JfaB => t.jfa_b.as_ref().unwrap_or(&self.dummy_tex),
                TexSlot::JfaExt => t.jfa_ext.as_ref().unwrap_or(&self.dummy_tex),
                TexSlot::JfaInt => t.jfa_int.as_ref().unwrap_or(&self.dummy_tex),
                TexSlot::JfaSeg => t.jfa_seg.as_ref().unwrap_or(&self.dummy_tex),
                TexSlot::AccumA => &t.accum_a,
                TexSlot::AccumB => &t.accum_b,
            }
        };

        for cmd in cmds {
            match *cmd {
                GpuCmd::ClearMask(target) => {
                    let dst = mask_tex(target);
                    let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sfx-clear-mask"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &dst.view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                GpuCmd::RasterPolys { target, v_off, v_count, uni_off } => {
                    let dst = mask_tex(target);
                    let (color_view, resolve_target) = match &t.msaa_mask {
                        Some(ms) => (&ms.view, Some(&dst.view)),
                        None => (&dst.view, None),
                    };
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sfx-stencil-cover"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: color_view,
                            depth_slice: None,
                            resolve_target,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &t.stencil.view,
                            depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                            stencil_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(0), store: wgpu::StoreOp::Discard }),
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
                    rp.set_scissor_rect(0, 0, w, h);
                    rp.set_stencil_reference(0);
                    rp.set_pipeline(&self.stencil_pipe);
                    rp.set_bind_group(0, &raster_bg, &[uni_off]);
                    rp.set_vertex_buffer(0, self.vbuf.buf.slice(v_off..));
                    rp.draw(0..v_count, 0..1);

                    rp.set_pipeline(&self.cover_pipe);
                    rp.draw(0..3, 0..1);
                }
                GpuCmd::RasterSegSeed { v_off, v_count, uni_off } => {
                    let Some(jfa_a) = t.jfa_a.as_ref() else { continue };
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sfx-seg-seed"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &jfa_a.view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color { r: -1e9, g: -1e9, b: 0.0, a: 0.0 }),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
                    rp.set_scissor_rect(0, 0, w, h);
                    rp.set_pipeline(&self.seg_seed_pipe);
                    rp.set_bind_group(0, &raster_bg, &[uni_off]);
                    rp.set_vertex_buffer(0, self.vbuf.buf.slice(v_off..));
                    rp.draw(0..v_count, 0..1);
                }
                GpuCmd::CopyTex { src, dst } => {
                    enc.copy_texture_to_texture(
                        slot_tex(src).tex.as_image_copy(),
                        slot_tex(dst).tex.as_image_copy(),
                        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    );
                }
                GpuCmd::Quad { pipe, a, b, c, dst, uni_off } => {
                    let rp_pipe = match pipe {
                        QuadPipe::Union => &self.union_pipe,
                        QuadPipe::Mul => &self.mul_pipe,
                        QuadPipe::OneBitFace => &self.one_bit_face_pipe,
                        QuadPipe::OneBitLine => &self.one_bit_line_pipe,
                        QuadPipe::JfaInit => &self.jfa_init_pipe,
                        QuadPipe::JfaStep => &self.jfa_step_pipe,
                        QuadPipe::BlurMask => &self.blur_mask_pipe,
                        QuadPipe::BlurJfa => &self.blur_jfa_pipe,
                        QuadPipe::Halo => &self.halo_pipe,
                        QuadPipe::StrokeRing => &self.stroke_ring_pipe,
                        QuadPipe::ShadowExtrude => &self.shadow_extrude_pipe,
                        QuadPipe::DryPhysics => &self.dry_physics_pipe,
                        QuadPipe::DrySeg => &self.dry_seg_pipe,
                        QuadPipe::EtfSeed => &self.etf_seed_pipe,
                        QuadPipe::DryEtfLic => &self.dry_etf_lic_pipe,
                        QuadPipe::DryGrain => &self.dry_grain_pipe,
                        QuadPipe::Composite => &self.composite_pipe,
                    };
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("sfx-fx-bg"),
                        layout: &self.fx_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &self.ring.buf,
                                    offset: 0,
                                    size: wgpu::BufferSize::new(std::mem::size_of::<FxParams>() as u64),
                                }),
                            },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&slot_tex(a).view) },
                            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&slot_tex(b).view) },
                            wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&slot_tex(c).view) },
                        ],
                    });
                    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("sfx-quad"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &slot_tex(dst).view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    rp.set_viewport(0.0, 0.0, w as f32, h as f32, 0.0, 1.0);
                    rp.set_scissor_rect(0, 0, w, h);
                    rp.set_pipeline(rp_pipe);
                    rp.set_bind_group(0, &bg, &[uni_off]);
                    rp.draw(0..3, 0..1);
                }
            }
        }
    }

    /// Recommended seamless tile overlap apron (in document pixels) for `data` given `tile_dim`.
    /// Always leaves at least `max(256, tile_dim / 2)` useful inner pixels per tile.
    pub fn tile_apron_px_for_tile(data: &SfxData, tile_dim: u32) -> u32 {
        let max_apron = (tile_dim / 4).min(512);
        max_tile_apron_px(data).min(max_apron)
    }

    /// Recommended seamless tile overlap apron (in document pixels) for `data`.
    pub fn tile_apron_px(data: &SfxData) -> u32 {
        Self::tile_apron_px_for_tile(data, MAX_SFX_TILE_DIM)
    }

    /// Copies the most recently rendered straight-alpha RGBA8 output into a caller-owned
    /// `MAP_READ | COPY_DST` staging buffer and returns `(padded_bytes_per_row, width, height)`.
    /// Works on BOTH WebGPU/WebGL2 (`wasm32`) and Native so async non-blocking tiled commits can
    /// drive `buffer.slice(..).map_async(Read, ...)` without blocking the UI thread.
    pub fn copy_last_output_to_buffer(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<(wgpu::Buffer, u32, u32, u32), Unsupported> {
        let Some(t) = self.targets.as_ref() else {
            return Err(Unsupported("missing GPU target".into()));
        };
        let [w, h] = self.last_size;
        if w == 0 || h == 0 {
            return Err(Unsupported("empty GPU target".into()));
        }
        let unpadded_row = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(align) * align;
        let total_bytes = u64::from(padded_row) * u64::from(h);
        let rb_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sfx-async-readback"),
            size: total_bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let src_tex = if self.last_accum_is_a { &t.accum_a.tex } else { &t.accum_b.tex };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sfx-async-rb") });
        enc.copy_texture_to_buffer(
            src_tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &rb_buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc.finish()));
        Ok((rb_buf, padded_row, w, h))
    }

    /// Native full-resolution tiled GPU render straight into a sparse [`Surface`].
    /// Handles arbitrarily large lettering (even 8000×5000 px on 12 000 × 17 000 px pages)
    /// with seamless padded aprons for Halo / DropShadow / 3D Extrude / Blur / Stroke.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn render_tiled_to_surface(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &SfxGpuScene<'_>,
        format: PixelFormat,
    ) -> Result<Surface, Unsupported> {
        let full = scene.full_bounds;
        if full.is_empty() {
            return Ok(Surface::new(format));
        }
        let apron = Self::tile_apron_px(scene.data) as i32;
        let max_inner = self.max_dim.min(MAX_SFX_TILE_DIM).saturating_sub((apron as u32) * 2).max(256) as i32;

        let mut surface = Surface::new(format);
        let ch = format.channels();

        let mut ty = full.y0;
        while ty < full.y1 {
            let inner_h = (full.y1 - ty).min(max_inner);
            let mut tx = full.x0;
            while tx < full.x1 {
                let inner_w = (full.x1 - tx).min(max_inner);
                let pad_x0 = tx - apron;
                let pad_y0 = ty - apron;
                let pad_w = (inner_w + apron * 2) as u32;
                let pad_h = (inner_h + apron * 2) as u32;
                let reg_doc = [f64::from(pad_x0), f64::from(pad_y0), f64::from(pad_x0) + f64::from(pad_w), f64::from(pad_y0) + f64::from(pad_h)];
                let _ = self.render_region(device, queue, scene, reg_doc, [pad_w, pad_h])?;

                let rgba = self.readback_last_rgba8(device, queue)?;
                let tile_rect = Rect::from_xywh(tx, ty, inner_w as u32, inner_h as u32);
                let mut any = false;
                let iw = inner_w as usize;
                let ih = inner_h as usize;
                let ap = apron as usize;
                let pw = pad_w as usize;
                'scan: for row in 0..ih {
                    let row_start = ((row + ap) * pw + ap) * 4;
                    let slice = &rgba[row_start..row_start + iw * 4];
                    for px in slice.chunks_exact(4) {
                        if px[3] != 0 {
                            any = true;
                            break 'scan;
                        }
                    }
                }
                if any {
                    if format == PixelFormat::RGBA8 {
                        let _ = surface.try_write_rows_with(tile_rect, |y, x0, dst| {
                            let row = (y - ty) as usize;
                            let col = (x0 - tx) as usize;
                            let src_start = ((row + ap) * pw + (col + ap)) * 4;
                            let len = dst.len();
                            if let Some(src) = rgba.get(src_start..src_start + len) {
                                dst.copy_from_slice(src);
                            }
                        });
                    } else {
                        let mut vals = vec![0.0_f32; iw * ih * ch];
                        for row in 0..ih {
                            let src_y = row + ap;
                            for col in 0..iw {
                                let si = (src_y * pw + col + ap) * 4;
                                let a = f32::from(rgba[si + 3]) / 255.0;
                                if a > 1e-6 {
                                    let px = [
                                        f32::from(rgba[si]) / 255.0,
                                        f32::from(rgba[si + 1]) / 255.0,
                                        f32::from(rgba[si + 2]) / 255.0,
                                        a,
                                    ];
                                    let di = (row * iw + col) * ch;
                                    from_rgba_into(&format, px, &mut vals[di..di + ch]);
                                }
                            }
                        }
                        surface.write_region(tile_rect, &vals);
                    }
                }
                tx += inner_w;
            }
            ty += inner_h;
        }
        surface.prune();
        Ok(surface)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn readback_last_rgba8(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Vec<u8>, Unsupported> {
        let Some(t) = self.targets.as_ref() else {
            return Err(Unsupported("missing GPU target".into()));
        };
        let [w, h] = self.last_size;
        let unpadded_row = w * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_row = unpadded_row.div_ceil(align) * align;
        let total_bytes = u64::from(padded_row) * u64::from(h);
        let need_new = self.readback.as_ref().is_none_or(|(_, sz)| *sz < total_bytes);
        if need_new {
            if let Some((old, _)) = self.readback.take() {
                old.destroy();
            }
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sfx-readback"),
                size: total_bytes,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.readback = Some((buf, total_bytes));
        }
        let Some((rb_buf, _)) = self.readback.as_ref() else {
            return Err(Unsupported("missing readback buffer".into()));
        };
        let src_tex = if self.last_accum_is_a { &t.accum_a.tex } else { &t.accum_b.tex };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sfx-rb") });
        enc.copy_texture_to_buffer(
            src_tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: rb_buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded_row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit(Some(enc.finish()));

        let slice = rb_buf.slice(0..total_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        match rx.recv() {
            Ok(Ok(())) => {}
            _ => return Err(Unsupported("SFX GPU readback failed".into())),
        }
        let Ok(mapped) = slice.get_mapped_range() else {
            rb_buf.unmap();
            return Err(Unsupported("SFX GPU readback map range failed".into()));
        };
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        for row in 0..(h as usize) {
            let src_start = row * (padded_row as usize);
            let dst_start = row * (unpadded_row as usize);
            out[dst_start..dst_start + (unpadded_row as usize)].copy_from_slice(&mapped[src_start..src_start + (unpadded_row as usize)]);
        }
        drop(mapped);
        rb_buf.unmap();
        Ok(out)
    }
}

fn max_tile_apron_px(data: &SfxData) -> u32 {
    let mut pad = data.line_effect.halo_width_px.max(0.0);
    if data.vector_shadow.enabled {
        pad = pad.max(data.vector_shadow.distance_px.max(data.vector_shadow.extrude_depth_px));
    }
    for fx in &data.blending.effects {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::Stroke { width_px, .. } => pad = pad.max(*width_px),
            SfxBlendEffect::DropShadow { distance_px, extrude_depth_px, blur_px, .. } => {
                pad = pad.max(distance_px.max(*extrude_depth_px) + blur_px * 2.5);
            }
            SfxBlendEffect::OuterGlow { spread_px, blur_px, .. } => pad = pad.max(*spread_px + blur_px * 2.5),
            SfxBlendEffect::InnerShadow { distance_px, blur_px, .. } => pad = pad.max(*distance_px + blur_px * 2.5),
            SfxBlendEffect::InnerGlow { width_px, .. } => pad = pad.max(*width_px * 2.5),
            SfxBlendEffect::Bevel { depth_px, .. } => pad = pad.max(*depth_px),
            SfxBlendEffect::FillOverlay { .. } => {}
        }
    }
    (pad.ceil() as u32).saturating_add(8)
}

fn sfx_has_hollow_interior(data: &SfxData) -> bool {
    matches!(data.fill, SfxFillSource::None)
        && !data.dry_brush.enabled
        && data.blending.secondary_fill.is_none()
        && !data.blending.effects.iter().any(|fx| {
            fx.enabled()
                && matches!(
                    fx,
                    SfxBlendEffect::FillOverlay { .. }
                        | SfxBlendEffect::InnerShadow { .. }
                        | SfxBlendEffect::InnerGlow { .. }
                        | SfxBlendEffect::Bevel { .. }
                        | SfxBlendEffect::Stroke { position: SfxStrokePosition::Inside | SfxStrokePosition::Center, .. }
                )
        })
}

fn scene_needs_interior_sdf(data: &SfxData) -> bool {
    let fill_needs = |f: &SfxFillSource| {
        matches!(
            f,
            SfxFillSource::DryBrush {
                spec: SfxDryBrushSpec { engine: SfxDryBrushEngine::SdfPhysics | SfxDryBrushEngine::SdfSeg | SfxDryBrushEngine::SdfEtf, .. },
                ..
            }
        )
    };
    let prim_db = data.dry_brush.enabled
        && matches!(data.dry_brush.engine, SfxDryBrushEngine::SdfPhysics | SfxDryBrushEngine::SdfSeg | SfxDryBrushEngine::SdfEtf);
    prim_db
        || fill_needs(&data.fill)
        || data.blending.secondary_fill.as_ref().is_some_and(fill_needs)
        || data.blending.effects.iter().any(|fx| {
            fx.enabled()
                && match fx {
                    SfxBlendEffect::Stroke { position: SfxStrokePosition::Inside | SfxStrokePosition::Center, .. } => true,
                    SfxBlendEffect::Stroke { fill, .. } | SfxBlendEffect::FillOverlay { fill, .. } => fill_needs(fill),
                    _ => false,
                }
        })
}

fn max_exterior_jfa_radius_px(data: &SfxData, px_per_doc: f32) -> f32 {
    let mut r = data.line_effect.halo_width_px.max(0.0);
    if data.line_effect.style == SfxLineStyle::OneBit {
        r = r.max(data.line_effect.line_width_px * 0.5);
    }
    for fx in &data.blending.effects {
        if !fx.enabled() {
            continue;
        }
        match fx {
            SfxBlendEffect::Stroke { width_px, position: SfxStrokePosition::Outside, .. } => r = r.max(*width_px),
            SfxBlendEffect::Stroke { width_px, position: SfxStrokePosition::Center, .. } => r = r.max(*width_px * 0.5),
            SfxBlendEffect::OuterGlow { spread_px, .. } => r = r.max(*spread_px),
            _ => {}
        }
    }
    r * px_per_doc
}

fn record_shadow_and_extrude(rec: &mut CmdRecorder<'_>, a: ShadowPassArgs) {
    if a.opacity <= 1e-4 {
        return;
    }
    let scale = rec.region.px_per_doc();
    let (sin_a, cos_a) = a.angle_deg.to_radians().sin_cos();
    if a.extrude > 0.2 {
        let mut p = rec.region.base_params();
        p.p1[2] = cos_a;
        p.p1[3] = sin_a;
        p.p2[0] = (a.extrude * scale).max(1.0);
        p.u0[0] = 0;
        rec.push_quad(QuadPipe::ShadowExtrude, a.union_slot, TexSlot::Dummy, TexSlot::Dummy, TexSlot::ScratchA, p);
        rec.composite_solid(TexSlot::ScratchA, a.color, a.opacity, a.blend);
    }
    if a.dist > 0.2 || a.blur > 0.1 {
        let mut p = rec.region.base_params();
        p.p1[2] = cos_a;
        p.p1[3] = sin_a;
        p.p2[1] = a.dist * scale;
        p.u0[0] = 1;
        rec.push_quad(QuadPipe::ShadowExtrude, a.union_slot, TexSlot::Dummy, TexSlot::Dummy, TexSlot::ScratchA, p);
        if a.blur > 0.1 {
            rec.blur_mask_in_place(TexSlot::ScratchA, TexSlot::ScratchB, a.blur);
        }
        rec.composite_solid(TexSlot::ScratchA, a.color, a.opacity, a.blend);
    }
}

fn record_fill_and_blending_passes(rec: &mut CmdRecorder<'_>, scene: &SfxGpuScene<'_>, has_line: bool, is_binary: bool, has_seg_jfa: bool) {
    let data = scene.data;
    let le = &data.line_effect;
    let bl = &data.blending;
    let px_per_doc = rec.region.px_per_doc();

    if bl.line_behind_fill && has_line {
        rec.composite_solid(TexSlot::Line, le.line_color, bl.line_opacity, bl.line_blend);
    }
    if let SfxFillSource::ShtackHatch { spec } = &data.fill {
        rec.composite_solid(TexSlot::Shtack, spec.color, bl.fill_opacity, bl.fill_blend);
    } else {
        rec.render_fill_pass(FillPassArgs {
            fill: &data.fill,
            base_cov: TexSlot::Face,
            dry_override: Some(&data.dry_brush),
            shtack_polys: scene.primary_shtack_polygons,
            vec_cuts: scene.primary_dry_vector_cuts,
            opacity: bl.fill_opacity,
            blend: bl.fill_blend,
            is_binary,
            has_seg_jfa,
        });
    }
    if let Some(sec) = &bl.secondary_fill {
        rec.render_fill_pass(FillPassArgs {
            fill: sec,
            base_cov: TexSlot::Face,
            dry_override: None,
            shtack_polys: scene.secondary_shtack_polygons,
            vec_cuts: &[],
            opacity: bl.secondary_opacity,
            blend: bl.secondary_blend,
            is_binary,
            has_seg_jfa,
        });
    }
    for (idx, fx) in bl.effects.iter().enumerate().rev() {
        if !fx.enabled() {
            continue;
        }
        let s_polys = scene.effect_shtack_polygons.iter().find(|(i, _)| *i == idx).map_or(&[][..], |(_, v)| v.as_slice());
        let v_cuts = scene.effect_dry_vector_cuts.iter().find(|(i, _)| *i == idx).map_or(&[][..], |(_, v)| v.as_slice());
        match fx {
            SfxBlendEffect::FillOverlay { fill, blend, opacity, .. } => {
                rec.render_fill_pass(FillPassArgs {
                    fill,
                    base_cov: TexSlot::Face,
                    dry_override: None,
                    shtack_polys: s_polys,
                    vec_cuts: v_cuts,
                    opacity: *opacity,
                    blend: *blend,
                    is_binary,
                    has_seg_jfa,
                });
            }
            SfxBlendEffect::Stroke { width_px, position: pos @ (SfxStrokePosition::Inside | SfxStrokePosition::Center), fill, blend, opacity, .. }
                if *width_px > 0.05 && *opacity > 1e-4 =>
            {
                let mut sp = rec.region.base_params();
                sp.p1[2] = *width_px * px_per_doc;
                sp.u0[0] = if *pos == SfxStrokePosition::Inside { 2 } else { 1 };
                let base = if *pos == SfxStrokePosition::Inside { TexSlot::Face } else { TexSlot::Union };
                rec.push_quad(QuadPipe::StrokeRing, base, TexSlot::JfaExt, TexSlot::JfaInt, TexSlot::ScratchB, sp);
                rec.render_fill_pass(FillPassArgs {
                    fill,
                    base_cov: TexSlot::ScratchB,
                    dry_override: None,
                    shtack_polys: s_polys,
                    vec_cuts: v_cuts,
                    opacity: *opacity,
                    blend: *blend,
                    is_binary,
                    has_seg_jfa,
                });
            }
            _ => {}
        }
    }
    if !bl.line_behind_fill && has_line {
        rec.composite_solid(TexSlot::Line, le.line_color, bl.line_opacity, bl.line_blend);
    }
}

fn record_outer_passes(rec: &mut CmdRecorder<'_>, scene: &SfxGpuScene<'_>, is_binary: bool, has_seg_jfa: bool) {
    let data = scene.data;
    let le = &data.line_effect;
    let bl = &data.blending;
    let px_per_doc = rec.region.px_per_doc();

    if data.vector_shadow.enabled {
        let sh = &data.vector_shadow;
        record_shadow_and_extrude(
            rec,
            ShadowPassArgs {
                union_slot: TexSlot::Face,
                angle_deg: sh.angle_deg,
                dist: sh.distance_px,
                extrude: sh.extrude_depth_px,
                blur: 0.0,
                color: sh.color,
                opacity: 0.96,
                blend: SfxChannelBlend::Normal,
            },
        );
    }
    for fx in bl.effects.iter().rev() {
        if let SfxBlendEffect::DropShadow { enabled: true, angle_deg, distance_px, extrude_depth_px, blur_px, color, blend, opacity } = fx {
            record_shadow_and_extrude(
                rec,
                ShadowPassArgs {
                    union_slot: TexSlot::Union,
                    angle_deg: *angle_deg,
                    dist: *distance_px,
                    extrude: *extrude_depth_px,
                    blur: *blur_px,
                    color: *color,
                    opacity: *opacity,
                    blend: *blend,
                },
            );
        }
    }
    if le.halo_width_px > 0.05 && bl.halo_opacity > 1e-4 {
        let mut hp = rec.region.base_params();
        hp.p1[2] = le.halo_width_px * px_per_doc;
        hp.p1[3] = le.one_bit_intensity / 100.0;
        hp.u0[2] = u32::from(is_binary);
        rec.push_quad(QuadPipe::Halo, TexSlot::Union, TexSlot::JfaExt, TexSlot::Dummy, TexSlot::ScratchA, hp);
        rec.composite_solid(TexSlot::ScratchA, le.halo_color, bl.halo_opacity, bl.halo_blend);
    }
    for (idx, fx) in bl.effects.iter().enumerate().rev() {
        if let SfxBlendEffect::Stroke { enabled: true, width_px, position: SfxStrokePosition::Outside, fill, blend, opacity, .. } = fx
            && *width_px > 0.05
            && *opacity > 1e-4
        {
            let mut sp = rec.region.base_params();
            sp.p1[2] = *width_px * px_per_doc;
            sp.u0[0] = 0;
            rec.push_quad(QuadPipe::StrokeRing, TexSlot::Union, TexSlot::JfaExt, TexSlot::JfaInt, TexSlot::ScratchB, sp);
            let s_polys = scene.effect_shtack_polygons.iter().find(|(i, _)| *i == idx).map_or(&[][..], |(_, v)| v.as_slice());
            let v_cuts = scene.effect_dry_vector_cuts.iter().find(|(i, _)| *i == idx).map_or(&[][..], |(_, v)| v.as_slice());
            rec.render_fill_pass(FillPassArgs {
                fill,
                base_cov: TexSlot::ScratchB,
                dry_override: None,
                shtack_polys: s_polys,
                vec_cuts: v_cuts,
                opacity: *opacity,
                blend: *blend,
                is_binary,
                has_seg_jfa,
            });
        }
    }
}

fn record_scene_passes(rec: &mut CmdRecorder<'_>, scene: &SfxGpuScene<'_>) {
    let data = scene.data;
    let le = &data.line_effect;
    let is_binary = le.style == SfxLineStyle::OneBit;
    let hollow = sfx_has_hollow_interior(data);
    let px_per_doc = rec.region.px_per_doc();

    // 1. Hardware Stencil-then-Cover vector rasterization directly in VRAM (0 CPU pixel upload!)
    rec.raster_polys(MaskSlot::Face, scene.face_contours);
    if is_binary {
        let mut op = rec.region.base_params();
        op.p1[2] = le.one_bit_intensity / 100.0;
        rec.push_quad(QuadPipe::OneBitFace, TexSlot::Face, TexSlot::Dummy, TexSlot::Dummy, TexSlot::ScratchA, op);
        rec.cmds.push(GpuCmd::CopyTex { src: TexSlot::ScratchA, dst: TexSlot::Face });
    }

    let has_line = if is_binary {
        if le.line_width_px > 0.05 {
            rec.run_jfa_from_mask(TexSlot::Face, false, (le.line_width_px * 0.5 * px_per_doc).max(2.0), TexSlot::JfaExt);
            let mut lp = rec.region.base_params();
            lp.p1[2] = le.line_width_px * 0.5 * px_per_doc;
            lp.p1[3] = le.one_bit_intensity / 100.0;
            rec.push_quad(QuadPipe::OneBitLine, TexSlot::Face, TexSlot::JfaExt, TexSlot::Dummy, TexSlot::Line, lp);
            true
        } else {
            false
        }
    } else if !scene.line_polygons.is_empty() {
        rec.raster_polys(MaskSlot::Line, scene.line_polygons);
        if le.style == SfxLineStyle::Shtack && le.shtack_variant == SfxShtackVariant::DryPencil && le.shtack_dryness_pct > 0.5 {
            let mut gp = rec.region.base_params();
            gp.p1[2] = (le.shtack_dryness_pct / 100.0).clamp(0.0, 1.0);
            gp.p1[3] = (le.shtack_grain_sharpness_pct / 100.0).clamp(0.0, 1.0);
            gp.p3[2] = 1.0 / rec.auto_scale.max(0.75);
            gp.u0 = [0, le.shtack_seed.wrapping_mul(7_919), 0, 0];
            rec.push_quad(QuadPipe::DryGrain, TexSlot::Line, TexSlot::Dummy, TexSlot::Dummy, TexSlot::ScratchA, gp);
            rec.cmds.push(GpuCmd::CopyTex { src: TexSlot::ScratchA, dst: TexSlot::Line });
        }
        true
    } else {
        false
    };

    let primary_is_shtack = matches!(data.fill, SfxFillSource::ShtackHatch { .. });
    if let SfxFillSource::ShtackHatch { spec } = &data.fill {
        rec.raster_shtack_fill_mask(scene.primary_shtack_polygons, spec, TexSlot::Face, MaskSlot::Shtack);
    }

    let face_for_union = if primary_is_shtack { Some(TexSlot::Shtack) } else { (!hollow).then_some(TexSlot::Face) };
    match (face_for_union, has_line) {
        (Some(f), true) => {
            let up = rec.region.base_params();
            rec.push_quad(QuadPipe::Union, f, TexSlot::Line, TexSlot::Dummy, TexSlot::Union, up);
        }
        (Some(f), false) => rec.cmds.push(GpuCmd::CopyTex { src: f, dst: TexSlot::Union }),
        (None, true) => rec.cmds.push(GpuCmd::CopyTex { src: TexSlot::Line, dst: TexSlot::Union }),
        (None, false) => rec.cmds.push(GpuCmd::ClearMask(MaskSlot::Union)),
    }

    // 2. GPU Jump Flooding (Exterior SDF, Interior SDF, and `Б1: Seg` curvilinear `(s, d)`).
    let ext_r = max_exterior_jfa_radius_px(data, px_per_doc);
    if ext_r > 0.2 {
        rec.run_jfa_from_mask(TexSlot::Union, false, ext_r + 2.0, TexSlot::JfaExt);
    }
    if scene_needs_interior_sdf(data) {
        rec.run_jfa_from_mask(TexSlot::Face, true, (rec.region.px_w.max(rec.region.px_h) as f32).min(512.0), TexSlot::JfaInt);
    }
    let has_seg_jfa = if !scene.seg_contours.is_empty() {
        let seg_verts = seg_contours_to_line_verts(&scene.seg_contours);
        if !seg_verts.is_empty() {
            let (v_off, v_count) = rec.vbuf.push_slice(&seg_verts);
            let uni_off = rec.ring.push(rec.region.raster_params());
            rec.cmds.push(GpuCmd::RasterSegSeed { v_off, v_count, uni_off });
            rec.run_jfa_steps((rec.region.px_w.max(rec.region.px_h) as f32).min(512.0), TexSlot::JfaSeg);
            true
        } else {
            false
        }
    } else {
        false
    };

    // 3. Outer passes (Vector Shadow, DropShadow/3D, Halo, Outside Strokes) + Fill/Blending/Line
    record_outer_passes(rec, scene, is_binary, has_seg_jfa);
    record_fill_and_blending_passes(rec, scene, has_line, is_binary, has_seg_jfa);
}
