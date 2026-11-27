//! Viewport pages of a document too large for its whole-document texture set (see
//! `canvas::viewport_pages`): independent single-level textures at their own reduction, drawn
//! by the same tile pipeline. They live under their own key, never next to a texture set.

use super::budget::{OVER_BUDGET, OVER_CANVAS_BUDGET};
use super::doc_textures::{catch_gpu_errors, init_buffer, tile_uniform};
use super::resources::{DocTextures, Resources, Tile, f32_bytes};
use super::texels::texel_bytes;
use super::{FORMAT_HIGH, GpuCanvas, Ordering, texels, wgpu};
use photocraft_geom::Rect as DRect;

/// The largest page texture side [`GpuCanvas::upload_page`] accepts (a 512 px page and its apron).
pub const MAX_PAGE_SIDE: u32 = 1024;

/// Parameters for rendering a single viewport page directly on the GPU via `Compositor::render`
/// without allocating a full-document texture set.
pub struct GpuPageRender<'a> {
    pub key: u64,
    pub page: u64,
    pub size: [u32; 2],
    pub doc: &'a photocraft_doc::Document,
    pub factor: u32,
    pub core: DRect,
    pub source: DRect,
    pub region: Option<DRect>,
    pub encode_srgb: bool,
}

/// One page to store: `buf` (document pixels reduced by `factor`, its rect in reduced pixels)
/// drawn over document area `core` of the `size` document `key`.
pub struct PageUpload<'a> {
    pub key: u64,
    pub page: u64,
    pub size: [u32; 2],
    pub depth: photocraft_doc::SampleType,
    pub factor: u32,
    pub core: DRect,
    pub buf: &'a photocraft_compose::Buffer,
}

impl DocTextures {
    /// GPU bytes held by this set's textures (pages have one level, texture sets their mips).
    pub(super) fn page_bytes(&self) -> u64 {
        let bpp = texel_bytes(self.format);
        self.tiles.iter().filter(|t| t.page.is_some()).map(|t| u64::from(t.texture.width()) * u64::from(t.texture.height()) * bpp).sum()
    }
}

impl GpuCanvas {
    /// The GPU bytes one page of `w`×`h` texels takes for a `depth` document of `size`.
    pub fn page_texture_bytes(&self, depth: photocraft_doc::SampleType, size: [u32; 2], w: u32, h: u32) -> u64 {
        u64::from(w) * u64::from(h) * texel_bytes(self.format_for(depth, size))
    }

    /// Store (or replace) page `page` of `key`. False, with nothing changed, when the device is
    /// not healthy, the page is malformed, or the pages of `key` would exceed the canvas budget
    /// (the caller evicts its cold pages first; this is the backstop).
    /// Writes `buf` (texels at the page's reduction, a part of it) into the page `page` of `key`
    /// already resident: an edit's re-render, without a new texture. False when that page isn't
    /// held or `buf` falls outside it.
    pub fn update_page(&self, key: u64, page: u64, buf: &photocraft_compose::Buffer) -> bool {
        let (w, h) = (buf.rect.width(), buf.rect.height());
        if !self.health.is_ok() || w == 0 || h == 0 || buf.px.len() as u64 != u64::from(w) * u64::from(h) {
            return false;
        }
        let renderer = self.rs.renderer.read();
        let Some(res) = renderer.callback_resources.get::<Resources>() else { return false };
        let Some(tile) = res.docs.get(&key).and_then(|d| d.tiles.iter().find(|t| t.page.is_some_and(|(id, _)| id == page))) else {
            return false;
        };
        let Some((_, factor)) = tile.page.filter(|(_, f)| *f > 0) else { return false };
        // The texture's first texel, at the page's reduction.
        let (ox, oy) = (i64::from(tile.rect[0] / factor), i64::from(tile.rect[1] / factor));
        let (x, y) = (i64::from(buf.rect.x0) - ox, i64::from(buf.rect.y0) - oy);
        let texture = &tile.texture;
        if x < 0 || y < 0 || x + i64::from(w) > i64::from(texture.width()) || y + i64::from(h) > i64::from(texture.height()) {
            return false;
        }
        let format = texture.format();
        self.rs.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: x as u32, y: y as u32, z: 0 }, aspect: wgpu::TextureAspect::All },
            &texels(format, &buf.px),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * texel_bytes(format) as u32), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        true
    }

    pub fn upload_page(&self, p: PageUpload<'_>) -> bool {
        let PageUpload { key, page, size, depth, factor, core, buf } = p;
        let (w, h) = (buf.rect.width(), buf.rect.height());
        if !self.health.is_ok()
            || factor == 0
            || w == 0
            || h == 0
            || w > MAX_PAGE_SIDE
            || h > MAX_PAGE_SIDE
            || buf.rect.x0 < 0
            || buf.rect.y0 < 0
            || buf.px.len() as u64 != u64::from(w) * u64::from(h)
            || core.is_empty()
        {
            return false;
        }
        let (Some(x), Some(y), Some(dw), Some(dh)) =
            ((buf.rect.x0 as u32).checked_mul(factor), (buf.rect.y0 as u32).checked_mul(factor), w.checked_mul(factor), h.checked_mul(factor))
        else {
            return false;
        };
        let format = self.format_for(depth, size);
        let bytes = u64::from(w) * u64::from(h) * texel_bytes(format);
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.get_mut::<Resources>() else { return false };
        let budget = res.canvas_budget.load(Ordering::Relaxed);
        if res.docs.get(&key).is_none_or(|d| d.size != size || d.format != format || d.tiles.iter().any(|t| t.page.is_none())) {
            res.docs.insert(key, DocTextures { size, format, tiles: Vec::new() });
        }
        let Some(d) = res.docs.get_mut(&key) else { return false };
        let replaced = d.tiles.iter().position(|t| t.page.is_some_and(|(id, _)| id == page));
        let held =
            d.page_bytes() - replaced.map_or(0, |i| u64::from(d.tiles[i].texture.width()) * u64::from(d.tiles[i].texture.height()) * texel_bytes(format));
        if held.saturating_add(bytes) > budget {
            return false;
        }
        // Room among the other documents: those no window shows give theirs up first.
        res.make_room(key, held + bytes);
        let rect = [x, y, dw, dh];
        // A page the GPU refuses (out of memory) fails alone: it falls back to the CPU canvas
        // instead of the uncaptured error disabling the GPU canvas for the session.
        let ((texture, view, uniform, bind_group), error) = catch_gpu_errors(device, || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_viewport_page"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let uniform = init_buffer(
                device,
                "pc_viewport_page",
                &f32_bytes(&tile_uniform(rect, Some([core.x0, core.y0, core.x1, core.y1]))),
                wgpu::BufferUsages::UNIFORM,
            );
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_viewport_page"),
                layout: &res.tile_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                ],
            });
            queue.write_texture(
                texture.as_image_copy(),
                &texels(format, &buf.px),
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * texel_bytes(format) as u32), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
            (texture, view, uniform, bind_group)
        });
        if let Some(e) = error {
            log::warn!("a {w}×{h} viewport page couldn't be stored on the GPU: {e}");
            return false;
        }
        let Some(d) = res.docs.get_mut(&key) else { return false };
        if let Some(i) = replaced {
            d.tiles.remove(i);
        }
        let tile = Tile { rect, texture, levels: vec![view], bind_group, _uniform: uniform, page: Some((page, factor)) };
        // Coarser pages first: where pages of two zooms overlap, the finer one is drawn on top.
        let at = d.tiles.iter().position(|t| t.page.is_some_and(|(_, f)| f < factor)).unwrap_or(d.tiles.len());
        d.tiles.insert(at, tile);
        true
    }

    /// Renders one viewport page directly on the GPU via [`photocraft_gpu::Compositor::render`],
    /// allocating only that `<= 514×514` page texture within `canvas_budget` (never a full-document
    /// texture set). Supports `factor == 1` (including scissor-clipped partial updates into an
    /// already-resident page) and aligned `factor == 2` via GPU 2× box downsampling. Returns the
    /// stored page byte count (`0` for in-place partial updates), or `Err` so the caller falls
    /// back cleanly to the CPU `pc-viewport-page` workers.
    pub fn render_page_gpu(&self, req: GpuPageRender<'_>) -> Result<u64, photocraft_gpu::Unsupported> {
        let GpuPageRender { key, page, size, doc, factor, core, source, region, encode_srgb } = req;
        if std::env::var_os("PHOTOCRAFT_CPU_COMPOSE").is_some() {
            return Err(photocraft_gpu::Unsupported("disabled by PHOTOCRAFT_CPU_COMPOSE".into()));
        }
        if let Some(f) = self.fault() {
            return Err(photocraft_gpu::Unsupported(f.to_string()));
        }
        if core.is_empty() || source.is_empty() || source.x0 < 0 || source.y0 < 0 {
            return Err(photocraft_gpu::Unsupported("invalid viewport page bounds".into()));
        }
        let aligned_2x = factor == 2
            && region.is_none()
            && source.x0 % 2 == 0
            && source.y0 % 2 == 0
            && source.width().is_multiple_of(2)
            && source.height().is_multiple_of(2);
        if factor != 1 && !aligned_2x {
            return Err(photocraft_gpu::Unsupported("viewport page reduction handled on CPU".into()));
        }
        let (w, h) = (source.width() / factor, source.height() / factor);
        if w == 0 || h == 0 || w > MAX_PAGE_SIDE || h > MAX_PAGE_SIDE {
            return Err(photocraft_gpu::Unsupported("viewport page dimensions out of bounds".into()));
        }
        let (Some(x), Some(y), Some(dw), Some(dh)) = (
            u32::try_from(source.x0).ok(),
            u32::try_from(source.y0).ok(),
            w.checked_mul(factor),
            h.checked_mul(factor),
        ) else {
            return Err(photocraft_gpu::Unsupported("viewport page rect overflow".into()));
        };
        let render_rect = region.unwrap_or(source).intersect(&source);
        if render_rect.is_empty() {
            return Ok(0);
        }

        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let format = self.format_for(doc.depth, size);
        let bytes = u64::from(w) * u64::from(h) * texel_bytes(format);
        let keep_hdr = i32::from(format == FORMAT_HIGH);

        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.get_mut::<Resources>() else {
            return Err(photocraft_gpu::Unsupported("no GPU canvas".into()));
        };
        let mut comp = self.take_compositor(res)?;
        if let Err(e) = comp.supports(doc) {
            res.compositor = Some(comp);
            return Err(e);
        }
        let needed = comp.working_set(doc, render_rect);
        if needed > comp.fit_layers(needed, false) {
            res.compositor = Some(comp);
            return Err(photocraft_gpu::Unsupported(OVER_BUDGET.into()));
        }

        // In-place partial update into an already-resident `factor == 1` page texture.
        if factor == 1 && region.is_some() {
            let has_target = res.docs.get(&key).is_some_and(|d| {
                d.size == size
                    && d.format == format
                    && d.tiles.iter().any(|t| {
                        t.page == Some((page, 1))
                            && t.rect == [x, y, dw, dh]
                            && t.texture.usage().contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
                    })
            });
            if !has_target {
                res.compositor = Some(comp);
                return Err(photocraft_gpu::Unsupported("resident viewport page unavailable for GPU partial update".into()));
            }
            let (Some(pipe), Some(tile)) = (
                res.encode_pipeline(format),
                res.docs.get(&key).and_then(|d| d.tiles.iter().find(|t| t.page == Some((page, 1)))),
            ) else {
                res.compositor = Some(comp);
                return Err(photocraft_gpu::Unsupported("no encode pipeline for viewport page".into()));
            };
            let bgl = &res.encode_bgl;
            let view = &tile.levels[0];
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                comp.render(device, queue, doc, render_rect, |enc, out| {
                    let r = out.rect.intersect(&render_rect);
                    if r.is_empty() {
                        return;
                    }
                    let offset = [out.rect.x0 - source.x0, out.rect.y0 - source.y0, i32::from(encode_srgb), keep_hdr];
                    let ubuf = init_buffer(device, "pc_page_encode", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("pc_page_encode"),
                        layout: bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(out.view) },
                            wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
                        ],
                    });
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("pc_page_encode"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(pipe);
                    pass.set_bind_group(0, &bg, &[0]);
                    pass.set_scissor_rect((r.x0 - source.x0) as u32, (r.y0 - source.y0) as u32, r.width(), r.height());
                    pass.draw(0..3, 0..1);
                })
            }));
            res.compositor = Some(comp);
            let Ok(result) = result else {
                self.health.mark(photocraft_gpu::Fault::Lost("the GPU compositor stopped (viewport page partial)".into()));
                return Err(photocraft_gpu::Unsupported("GPU device lost".into()));
            };
            if let Some(f) = self.fault() {
                return Err(photocraft_gpu::Unsupported(f.to_string()));
            }
            result?;
            return Ok(0);
        }

        // Whole page allocation: enforce `canvas_budget` strictly over page textures only.
        let budget = res.canvas_budget.load(Ordering::Relaxed);
        let held = res.docs.get(&key).filter(|d| d.size == size && d.format == format && d.tiles.iter().all(|t| t.page.is_some())).map_or(0, |d| {
            let replaced = d.tiles.iter().position(|t| t.page.is_some_and(|(id, _)| id == page));
            d.page_bytes() - replaced.map_or(0, |i| u64::from(d.tiles[i].texture.width()) * u64::from(d.tiles[i].texture.height()) * texel_bytes(format))
        });
        if held.saturating_add(bytes) > budget {
            res.compositor = Some(comp);
            return Err(photocraft_gpu::Unsupported(OVER_CANVAS_BUDGET.into()));
        }
        res.make_room(key, held + bytes);

        let rect = [x, y, dw, dh];
        let ((texture, view, uniform, bind_group), alloc_err) = catch_gpu_errors(device, || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_viewport_page_gpu"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let uniform = init_buffer(
                device,
                "pc_viewport_page_gpu",
                &f32_bytes(&tile_uniform(rect, Some([core.x0, core.y0, core.x1, core.y1]))),
                wgpu::BufferUsages::UNIFORM,
            );
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_viewport_page_gpu"),
                layout: &res.tile_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: uniform.as_entire_binding() },
                ],
            });
            (texture, view, uniform, bind_group)
        });
        if let Some(e) = alloc_err {
            texture.destroy();
            uniform.destroy();
            res.compositor = Some(comp);
            return Err(photocraft_gpu::Unsupported(format!("viewport page alloc failed: {e}")));
        }

        let Some(encode_pipe) = res.encode_pipeline(format) else {
            texture.destroy();
            uniform.destroy();
            res.compositor = Some(comp);
            return Err(photocraft_gpu::Unsupported(format!("no {format:?} encode pipeline")));
        };
        let encode_bgl = &res.encode_bgl;

        let render_res = if factor == 1 {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                comp.render(device, queue, doc, source, |enc, out| {
                    let inter = out.rect.intersect(&source);
                    if inter.is_empty() {
                        return;
                    }
                    let offset = [out.rect.x0 - source.x0, out.rect.y0 - source.y0, i32::from(encode_srgb), keep_hdr];
                    let ubuf = init_buffer(device, "pc_page_encode", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("pc_page_encode"),
                        layout: encode_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(out.view) },
                            wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
                        ],
                    });
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("pc_page_encode"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(encode_pipe);
                    pass.set_bind_group(0, &bg, &[0]);
                    pass.set_scissor_rect((inter.x0 - source.x0) as u32, (inter.y0 - source.y0) as u32, inter.width(), inter.height());
                    pass.draw(0..3, 0..1);
                })
            }));
            r
        } else {
            let Some(mip_pipe) = res.mip_pipeline(format) else {
                texture.destroy();
                uniform.destroy();
                res.compositor = Some(comp);
                return Err(photocraft_gpu::Unsupported(format!("no {format:?} mip pipeline")));
            };
            let scratch = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_viewport_page_2x_scratch"),
                size: wgpu::Extent3d { width: source.width(), height: source.height(), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let scratch_view = scratch.create_view(&Default::default());
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let stats = comp.render(device, queue, doc, source, |enc, out| {
                    let inter = out.rect.intersect(&source);
                    if inter.is_empty() {
                        return;
                    }
                    let offset = [out.rect.x0 - source.x0, out.rect.y0 - source.y0, i32::from(encode_srgb), keep_hdr];
                    let ubuf = init_buffer(device, "pc_page_encode", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
                    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("pc_page_encode"),
                        layout: encode_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(out.view) },
                            wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
                        ],
                    });
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("pc_page_encode_2x"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &scratch_view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(encode_pipe);
                    pass.set_bind_group(0, &bg, &[0]);
                    pass.set_scissor_rect((inter.x0 - source.x0) as u32, (inter.y0 - source.y0) as u32, inter.width(), inter.height());
                    pass.draw(0..3, 0..1);
                })?;
                // 2× box-average downsample from `scratch_view` into the `w×h` page texture.
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_page_2x_down") });
                let mip_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("pc_page_2x_down"),
                    layout: &res.mip_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&scratch_view) },
                        wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&res.mip_sampler) },
                    ],
                });
                {
                    let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("pc_page_2x_down"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    pass.set_pipeline(mip_pipe);
                    pass.set_bind_group(0, &mip_bg, &[]);
                    pass.draw(0..3, 0..1);
                }
                queue.submit([enc.finish()]);
                Ok(stats)
            }));
            scratch.destroy();
            r
        };

        res.compositor = Some(comp);
        let Ok(render_res) = render_res else {
            texture.destroy();
            uniform.destroy();
            self.health.mark(photocraft_gpu::Fault::Lost("the GPU compositor stopped (viewport page)".into()));
            return Err(photocraft_gpu::Unsupported("GPU device lost".into()));
        };
        if let Some(f) = self.fault() {
            texture.destroy();
            uniform.destroy();
            return Err(photocraft_gpu::Unsupported(f.to_string()));
        }
        if let Err(e) = render_res {
            texture.destroy();
            uniform.destroy();
            return Err(e);
        }

        if res.docs.get(&key).is_none_or(|d| d.size != size || d.format != format || d.tiles.iter().any(|t| t.page.is_none())) {
            res.docs.insert(key, DocTextures { size, format, tiles: Vec::new() });
        }
        let Some(d) = res.docs.get_mut(&key) else {
            texture.destroy();
            uniform.destroy();
            return Err(photocraft_gpu::Unsupported("viewport page slot missing".into()));
        };
        if let Some(i) = d.tiles.iter().position(|t| t.page.is_some_and(|(id, _)| id == page)) {
            d.tiles.remove(i);
        }
        let tile = Tile { rect, texture, levels: vec![view], bind_group, _uniform: uniform, page: Some((page, factor)) };
        let at = d.tiles.iter().position(|t| t.page.is_some_and(|(_, f)| f < factor)).unwrap_or(d.tiles.len());
        d.tiles.insert(at, tile);
        Ok(bytes)
    }

    /// Drop the pages of `key` not in `live` (all of them, and the key, when `live` is empty).
    pub fn retain_pages(&self, key: u64, live: &[u64]) {
        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.get_mut::<Resources>() else { return };
        if live.is_empty() {
            res.docs.remove(&key);
        } else if let Some(d) = res.docs.get_mut(&key) {
            d.tiles.retain(|t| t.page.is_some_and(|(id, _)| live.contains(&id)));
        }
    }

    /// Free document `key`'s texture set and what the compositor holds for it, once it is drawn
    /// from pages instead: memory the pages are budgeted against.
    pub fn release_texture_set(&self, key: u64) {
        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.get_mut::<Resources>() else { return };
        res.docs.remove(&key);
        if let Some(c) = res.compositor.as_mut() {
            c.forget_doc(photocraft_doc::DocId(key));
        }
    }

    /// The pages of `key` the GPU holds (page ids), and the bytes they take.
    pub fn pages(&self, key: u64) -> (Vec<u64>, u64) {
        let r = self.rs.renderer.read();
        let Some(d) = r.callback_resources.get::<Resources>().and_then(|res| res.docs.get(&key)) else { return (Vec::new(), 0) };
        (d.tiles.iter().filter_map(|t| t.page.map(|(id, _)| id)).collect(), d.page_bytes())
    }

    /// The display texture budget all documents share ([`super::canvas_budget`]); a document's
    /// pages are held to its share ([`GpuCanvas::canvas_fit`]).
    pub fn canvas_budget(&self) -> u64 {
        self.canvas_budget.load(Ordering::Relaxed)
    }
}
