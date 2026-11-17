//! Viewport projection and abstract pointer events mirrored from `ui-egui::canvas`.
//!
//! Keeps canvas gizmos (`comic_screentone_gizmo`, `balloon_gizmo`) inside `crates/manga-ui`
//! without importing anything from L6 `photocraft-ui-egui`.

use egui::{Pos2, Rect, Vec2, vec2};
use photocraft_doc::{Document, LayerId, LayerMask};
use photocraft_geom::Rect as DocRect;
use photocraft_raster::Surface;

/// Abstract pointer event forwarded from `ui-egui`'s canvas input loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CanvasToolEvent {
    Down { x: f64, y: f64, pressure: f32 },
    Move { x: f64, y: f64, pressure: f32 },
    Up { x: f64, y: f64 },
}

/// Document <-> screen mapping matching `ui-egui::canvas::ViewXform`.
#[derive(Clone, Copy, Debug)]
pub struct CanvasViewXform {
    pub rect: Rect,
    pub zoom: f32,
    pub center: [f32; 2],
    pub flip: bool,
    pub rotation: f32,
    pub aspect: f32,
}

impl CanvasViewXform {
    fn sincos(self) -> (f32, f32) {
        if self.rotation.abs() < 1e-8 {
            (1.0, 0.0)
        } else {
            let r = self.rotation.to_radians();
            (r.cos(), r.sin())
        }
    }

    fn sx(self) -> f32 {
        if self.aspect.is_finite() && self.aspect > 0.0 { self.aspect } else { 1.0 }
    }

    /// Screen points per document pixel along the X axis.
    pub fn zoom_x(&self) -> f32 {
        self.zoom * self.sx()
    }

    /// Maps a screen-space vector back into document space (without zoom).
    pub fn unmap_vec(self, d: Vec2) -> Vec2 {
        let d = if self.flip { vec2(-d.x, d.y) } else { d };
        let (c, s) = self.sincos();
        vec2((d.x * c + d.y * s) / self.sx(), -d.x * s + d.y * c)
    }

    /// Maps a document-space point into screen coordinates.
    pub fn to_screen(&self, x: f32, y: f32) -> Pos2 {
        let (c, s) = self.sincos();
        let dx = (x - self.center[0]) * self.sx();
        let dy = y - self.center[1];
        let rx = dx * c - dy * s;
        let ry = dx * s + dy * c;
        let sx = if self.flip { -1.0 } else { 1.0 };
        self.rect.center() + vec2(rx * self.zoom * sx, ry * self.zoom)
    }

    /// Maps a screen position back into document pixel coordinates.
    pub fn to_doc(&self, p: Pos2) -> [f64; 2] {
        let d = (p - self.rect.center()) / self.zoom;
        let u = self.unmap_vec(d);
        [(u.x + self.center[0]) as f64, (u.y + self.center[1]) as f64]
    }
}

/// Computes the screentone sheet's bounds in `doc`, matching `screentone_cmds::sheet_bounds`.
pub fn screentone_sheet_bounds(doc: &Document, target: Option<LayerId>) -> DocRect {
    let canvas = doc.bounds();
    if let Some(id) = target
        && let Some(layer) = doc.layer(id)
        && let Some(mask) = layer.mask.as_ref()
    {
        return revealed_bounds(mask, canvas).unwrap_or(canvas);
    }
    if let Some(sel) = doc.selection.as_ref().map(Surface::content_bounds).filter(|b| !b.is_empty()) {
        return sel.intersect(&canvas);
    }
    canvas
}

fn revealed_bounds(mask: &LayerMask, canvas: DocRect) -> Option<DocRect> {
    if !mask.enabled || canvas.is_empty() {
        return Some(canvas);
    }
    if mask.surface.content_bounds().is_empty() {
        return (mask.surface.default_pixel().first().copied().unwrap_or(1.0) > 0.0).then_some(canvas);
    }
    let ch = mask.surface.channels().max(1);
    let buf = mask.surface.read_region(canvas);
    let w = canvas.width() as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (canvas.x1, canvas.y1, canvas.x0, canvas.y0);
    for (i, px) in buf.chunks_exact(ch).enumerate() {
        if px[0] > 0.0 {
            let x = canvas.x0 + (i % w) as i32;
            let y = canvas.y0 + (i / w) as i32;
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0 && y1 > y0).then(|| DocRect::new(x0, y0, x1, y1))
}
