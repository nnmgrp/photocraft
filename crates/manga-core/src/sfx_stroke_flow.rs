//! Clean Scale-Axis Medial Axis + Taubin/Catmull-Rom Smoothing + Tangent Chaining
//! shared by `Shtack Fill: StrokeFlow` (`⚡ Вдоль (SAI)`) and `UkiyoBrush` (`🖌 Ukiyo-e Кисть`).
//!
//! Pipeline per glyph:
//! 1. Rasterize into a canonical-height binary mask (~160 px) + exact 2-pass Felzenszwalb EDT (`sdf`).
//! 2. Peel off compact punctuation dots (`.` / `!` / `?` / `i` dots) as smooth dot descriptors.
//! 3. Zhang-Suen thinning + 4-connected L-corner removal -> pixel junction graph.
//! 4. Collapse short junction-to-junction bridges (`<= 1.98 * r`) so split X/K junctions (`К`, `Ж`,
//!    `M`, `B`) merge cleanly without parasitic horizontal bars; run simultaneous per-junction
//!    Scale-Axis spur pruning (`d_jt <= 1.82 * r_j`).
//! 5. Run 20-pass non-shrinking Taubin smoothing, pair collinear arms across T/X-junctions, split
//!    genuine sharp corners (>62°), trim acute terminal corner bends (e.g. `A` leg feet) where `r`
//!    drops sharply at free tips, and extend open ends straight to the true glyph boundary.

use photocraft_doc::{FillRule, PathOp, Rect};
use photocraft_vector::Rasterizer;

use crate::sfx_geom::PreparedGlyph;
use crate::sfx_render::compute_mask_sdf;

/// Topological classification of an open centerline stroke endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlowEndKind {
    /// Genuine free tip of a letter stroke (e.g. top/bottom of `I`/`B` stem, legs of `A`/`К`).
    FreeTip,
    /// Terminates at a T/L/V junction or sharp corner split.
    Junction,
    /// Terminates on a short internal junction-to-junction bridge (never shoots needles).
    ShortBridge,
}

/// One extracted vector centerline stroke of a glyph, in document pixels.
#[derive(Clone, Debug)]
pub(crate) struct FlowStroke {
    /// Sequence of `(doc_x, doc_y, local_radius_doc_px)` samples along the centerline.
    pub pts: Vec<(f64, f64, f64)>,
    /// True for smooth closed loops (`O`, `0`) and compact dots.
    pub closed: bool,
    /// True for compact punctuation dots (`.` / `!` / `?` / `i` dots).
    pub is_dot: bool,
    pub start_kind: FlowEndKind,
    pub end_kind: FlowEndKind,
}

const TARGET_CANONICAL_H: f64 = 160.0;
const MAX_CANONICAL_W: f64 = 240.0;
const GRID_PAD: usize = 4;
const PRUNE_SCALE: f64 = 1.82;
const BRIDGE_COLLAPSE_SCALE: f64 = 1.98;
const TAUBIN_LAMBDA: f64 = 0.50;
const TAUBIN_MU: f64 = -0.53;

/// Evaluates an open `(x, y, r)` polyline along a smooth Catmull-Rom spline with `count` even arc samples.
pub(crate) fn catmull_rom_resample_flow(pts: &[(f64, f64, f64)], count: usize) -> Vec<(f64, f64, f64)> {
    let n = pts.len();
    if n == 0 || count < 2 {
        return Vec::new();
    }
    if n == 1 {
        return vec![pts[0]; count];
    }
    if n == 2 {
        return (0..count)
            .map(|k| {
                let u = k as f64 / (count - 1) as f64;
                (
                    pts[0].0 + (pts[1].0 - pts[0].0) * u,
                    pts[0].1 + (pts[1].1 - pts[0].1) * u,
                    pts[0].2 + (pts[1].2 - pts[0].2) * u,
                )
            })
            .collect();
    }
    // First upsample every span with Catmull-Rom (4 substeps per control segment), then arc-resample.
    let sub = 4usize;
    let mut dense = Vec::with_capacity((n - 1) * sub + 1);
    for i in 0..(n - 1) {
        let p0 = pts[i.saturating_sub(1)];
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = pts[(i + 2).min(n - 1)];
        let steps = if i + 2 == n { sub + 1 } else { sub };
        for s in 0..steps {
            let t = s as f64 / sub as f64;
            dense.push(eval_catmull_rom(p0, p1, p2, p3, t));
        }
    }
    arc_resample_flow(&dense, count)
}

/// Evaluates a closed `(x, y, r)` ring along a periodic Catmull-Rom spline, returning `count` samples
/// starting at arc distance `s_start` and sweeping `sweep_len`.
pub(crate) fn catmull_rom_sample_closed_sweep(
    pts: &[(f64, f64, f64)],
    s_start: f64,
    sweep_len: f64,
    count: usize,
) -> Vec<(f64, f64, f64)> {
    let n = pts.len();
    if n < 3 || count < 2 {
        return Vec::new();
    }
    let sub = 4usize;
    let mut dense = Vec::with_capacity(n * sub);
    for i in 0..n {
        let p0 = pts[(i + n - 1) % n];
        let p1 = pts[i];
        let p2 = pts[(i + 1) % n];
        let p3 = pts[(i + 2) % n];
        for s in 0..sub {
            let t = s as f64 / sub as f64;
            dense.push(eval_catmull_rom(p0, p1, p2, p3, t));
        }
    }
    let dn = dense.len();
    let mut cum = Vec::with_capacity(dn + 1);
    cum.push(0.0_f64);
    for i in 0..dn {
        let a = dense[i];
        let b = dense[(i + 1) % dn];
        cum.push(cum[i] + (b.0 - a.0).hypot(b.1 - a.1));
    }
    let total = cum[dn];
    if !total.is_finite() || total < 1e-4 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count);
    for k in 0..count {
        let s = (s_start + sweep_len * (k as f64 / (count - 1) as f64)).rem_euclid(total);
        let seg = cum.partition_point(|&c| c <= s).saturating_sub(1).min(dn - 1);
        let a = cum[seg];
        let b = cum[seg + 1];
        let u = if b > a { ((s - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
        let p0 = dense[seg];
        let p1 = dense[(seg + 1) % dn];
        out.push((p0.0 + (p1.0 - p0.0) * u, p0.1 + (p1.1 - p0.1) * u, p0.2 + (p1.2 - p0.2) * u));
    }
    out
}

#[inline]
fn eval_catmull_rom(
    p0: (f64, f64, f64),
    p1: (f64, f64, f64),
    p2: (f64, f64, f64),
    p3: (f64, f64, f64),
    t: f64,
) -> (f64, f64, f64) {
    let t2 = t * t;
    let t3 = t2 * t;
    let cr = |v0: f64, v1: f64, v2: f64, v3: f64| -> f64 {
        0.5 * ((2.0 * v1)
            + (-v0 + v2) * t
            + (2.0 * v0 - 5.0 * v1 + 4.0 * v2 - v3) * t2
            + (-v0 + 3.0 * v1 - 3.0 * v2 + v3) * t3)
    };
    let r = cr(p0.2, p1.2, p2.2, p3.2).max(p1.2.min(p2.2) * 0.75).max(0.5);
    (cr(p0.0, p1.0, p2.0, p3.0), cr(p0.1, p1.1, p2.1, p3.1), r)
}

fn arc_resample_flow(pts: &[(f64, f64, f64)], count: usize) -> Vec<(f64, f64, f64)> {
    let n = pts.len();
    if n < 2 || count < 2 {
        return pts.to_vec();
    }
    let mut cum = vec![0.0_f64; n];
    for i in 1..n {
        cum[i] = cum[i - 1] + (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
    }
    let total = cum[n - 1];
    if !total.is_finite() || total < 1e-6 {
        return vec![pts[0]; count];
    }
    let mut out = Vec::with_capacity(count);
    let mut seg = 1usize;
    for k in 0..count {
        let t = total * k as f64 / (count - 1) as f64;
        while seg < n - 1 && cum[seg] < t {
            seg += 1;
        }
        let a = cum[seg - 1];
        let b = cum[seg];
        let u = if b > a { ((t - a) / (b - a)).clamp(0.0, 1.0) } else { 0.0 };
        let p0 = pts[seg - 1];
        let p1 = pts[seg];
        out.push((p0.0 + (p1.0 - p0.0) * u, p0.1 + (p1.1 - p0.1) * u, p0.2 + (p1.2 - p0.2) * u));
    }
    out
}

/// Extracts cleaned, Taubin-smoothed centerline strokes `(x, y, local_radius)` in document pixels.
pub(crate) fn extract_glyph_flow_strokes(glyph: &PreparedGlyph) -> Vec<FlowStroke> {
    if glyph.polylines.is_empty() {
        return Vec::new();
    }
    let gx0 = glyph.warped_bounds[0];
    let gy0 = glyph.warped_bounds[1];
    let gw = (glyph.warped_bounds[2] - gx0).abs();
    let gh = (glyph.warped_bounds[3] - gy0).abs();
    if !gw.is_finite() || !gh.is_finite() || gw < 1.0 || gh < 1.0 {
        return Vec::new();
    }

    let scale = (TARGET_CANONICAL_H / gh.max(12.0)).min(MAX_CANONICAL_W / gw.max(12.0)).clamp(0.08, 2.5);
    let w = ((gw * scale).ceil() as usize + GRID_PAD * 2).clamp(12, 300);
    let h = ((gh * scale).ceil() as usize + GRID_PAD * 2).clamp(12, 240);
    let pad_f = GRID_PAD as f64;

    let polys: Vec<Vec<(f64, f64)>> = glyph
        .polylines
        .iter()
        .filter(|pl| pl.pts.len() >= 3)
        .map(|pl| pl.pts.iter().map(|&(x, y)| ((x - gx0) * scale + pad_f, (y - gy0) * scale + pad_f)).collect())
        .collect();
    if polys.is_empty() {
        return Vec::new();
    }

    let mut ras = Rasterizer::new(false);
    ras.add_component(&polys, PathOp::Combine, FillRule::NonZero);
    let cov = ras.render(Rect::from_xywh(0, 0, w as u32, h as u32));

    let mut mask = vec![false; w * h];
    for (m, &c) in mask.iter_mut().zip(cov.iter()) {
        *m = c >= 0.5;
    }
    despeckle_mask(&mut mask, w, h);

    let mask_f32: Vec<f32> = mask.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect();
    let sdf = compute_mask_sdf(&mask_f32, w, h);

    let to_doc = |x: f64, y: f64, r: f64| -> (f64, f64, f64) {
        (gx0 + (x - pad_f) / scale, gy0 + (y - pad_f) / scale, (r / scale).max(0.75))
    };

    let gh_grid = (gh * scale).max(12.0);
    let mut out = extract_compact_dot_strokes(&mut mask, &sdf, w, h, gh_grid, &to_doc);

    let mut skel = zhang_suen_thin(&mask, w, h);
    remove_l_corners(&mut skel, w, h);

    let mut graph = build_skeleton_graph(&skel, &sdf, w, h);
    collapse_short_junction_bridges(&mut graph);
    prune_scale_axis_spurs(&mut graph);
    collapse_short_junction_bridges(&mut graph);

    let raw_chains = chain_and_split_graph(&graph, &sdf, w, h);
    for ch in raw_chains {
        if ch.pts.len() < 2 {
            continue;
        }
        let doc_pts: Vec<(f64, f64, f64)> = ch.pts.into_iter().map(|(x, y, r)| to_doc(x, y, r)).collect();
        out.push(FlowStroke {
            pts: doc_pts,
            closed: ch.closed,
            is_dot: false,
            start_kind: ch.start_kind,
            end_kind: ch.end_kind,
        });
    }
    out
}

fn despeckle_mask(mask: &mut [bool], w: usize, h: usize) {
    if w < 3 || h < 3 {
        return;
    }
    let src = mask.to_vec();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let idx = y * w + x;
            let mut cnt = 0u8;
            for dy in -1..=1_isize {
                for dx in -1..=1_isize {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let ni = (y as isize + dy) as usize * w + (x as isize + dx) as usize;
                    if src[ni] {
                        cnt += 1;
                    }
                }
            }
            if src[idx] && cnt <= 2 {
                mask[idx] = false;
            } else if !src[idx] && cnt >= 7 {
                mask[idx] = true;
            }
        }
    }
}

fn extract_compact_dot_strokes<F>(
    mask: &mut [bool],
    sdf: &[f32],
    w: usize,
    h: usize,
    gh_grid: f64,
    to_doc: &F,
) -> Vec<FlowStroke>
where
    F: Fn(f64, f64, f64) -> (f64, f64, f64),
{
    let mut visited = vec![false; w * h];
    let mut dots = Vec::new();
    let mut queue = Vec::new();

    for sy in 0..h {
        for sx in 0..w {
            let start = sy * w + sx;
            if !mask[start] || visited[start] {
                continue;
            }
            visited[start] = true;
            queue.clear();
            queue.push(start);
            let mut comp = Vec::new();
            let (mut x0, mut y0, mut x1, mut y1) = (sx, sy, sx, sy);
            let mut sum_x = 0.0_f64;
            let mut sum_y = 0.0_f64;
            let mut max_r = 0.0_f64;

            let mut head = 0usize;
            while head < queue.len() {
                let cur = queue[head];
                head += 1;
                comp.push(cur);
                let cx = cur % w;
                let cy = cur / w;
                x0 = x0.min(cx);
                y0 = y0.min(cy);
                x1 = x1.max(cx);
                y1 = y1.max(cy);
                sum_x += cx as f64;
                sum_y += cy as f64;
                max_r = max_r.max(f64::from(sdf[cur]));

                for dy in -1..=1_isize {
                    for dx in -1..=1_isize {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = cx as isize + dx;
                        let ny = cy as isize + dy;
                        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                            continue;
                        }
                        let ni = ny as usize * w + nx as usize;
                        if mask[ni] && !visited[ni] {
                            visited[ni] = true;
                            queue.push(ni);
                        }
                    }
                }
            }

            let cw = (x1 - x0 + 1) as f64;
            let ch = (y1 - y0 + 1) as f64;
            let span = cw.max(ch);
            let aspect = cw.max(ch) / cw.min(ch).max(1.0);
            if span <= gh_grid * 0.29 && span <= max_r.max(1.5) * 3.6 && aspect <= 1.85 {
                let n = comp.len().max(1) as f64;
                let cx = sum_x / n;
                let cy = sum_y / n;
                let rx = (cw * 0.38).max(1.0);
                let ry = (ch * 0.38).max(1.0);
                let dot_r = max_r.max(1.5);
                let mut pts = Vec::with_capacity(16);
                for k in 0..16 {
                    let ang = std::f64::consts::TAU * (k as f64) / 16.0;
                    pts.push(to_doc(cx + rx * ang.cos(), cy + ry * ang.sin(), dot_r));
                }
                dots.push(FlowStroke {
                    pts,
                    closed: true,
                    is_dot: true,
                    start_kind: FlowEndKind::FreeTip,
                    end_kind: FlowEndKind::FreeTip,
                });
                for idx in comp {
                    mask[idx] = false;
                }
            }
        }
    }
    dots
}

#[inline]
fn zhang_suen_transitions(nb: [bool; 8]) -> u8 {
    let mut t = 0u8;
    for i in 0..8 {
        if !nb[i] && nb[(i + 1) & 7] {
            t += 1;
        }
    }
    t
}

#[inline]
fn neighbors_cw(img: &[bool], w: usize, x: usize, y: usize) -> [bool; 8] {
    [
        img[(y - 1) * w + x],
        img[(y - 1) * w + (x + 1)],
        img[y * w + (x + 1)],
        img[(y + 1) * w + (x + 1)],
        img[(y + 1) * w + x],
        img[(y + 1) * w + (x - 1)],
        img[y * w + (x - 1)],
        img[(y - 1) * w + (x - 1)],
    ]
}

fn zhang_suen_thin(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut img = mask.to_vec();
    if w < 3 || h < 3 {
        return img;
    }
    let mut to_clear = Vec::new();
    for _ in 0..128 {
        let mut changed = false;
        for step in 0..2 {
            to_clear.clear();
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let idx = y * w + x;
                    if !img[idx] {
                        continue;
                    }
                    let nb = neighbors_cw(&img, w, x, y);
                    let b = nb.iter().filter(|&&v| v).count();
                    if !(2..=6).contains(&b) || zhang_suen_transitions(nb) != 1 {
                        continue;
                    }
                    let cond = if step == 0 {
                        !nb[2] || !nb[4] || (!nb[0] && !nb[6])
                    } else {
                        !nb[0] || !nb[6] || (!nb[2] && !nb[4])
                    };
                    if cond {
                        to_clear.push(idx);
                    }
                }
            }
            if !to_clear.is_empty() {
                changed = true;
                for &idx in &to_clear {
                    img[idx] = false;
                }
            }
        }
        if !changed {
            break;
        }
    }
    img
}

fn removing_keeps_connectivity(nb: [bool; 8]) -> bool {
    let count = nb.iter().filter(|&&v| v).count();
    if count <= 1 {
        return false;
    }
    let mut visited = [false; 8];
    let Some(first) = nb.iter().position(|&v| v) else {
        return false;
    };
    let mut stack = [0usize; 8];
    let mut sp = 0usize;
    visited[first] = true;
    stack[sp] = first;
    sp += 1;
    let mut seen = 0usize;
    while sp > 0 {
        sp -= 1;
        let cur = stack[sp];
        seen += 1;
        let adj: &[usize] = if cur.is_multiple_of(2) {
            &[(cur + 7) & 7, (cur + 1) & 7, (cur + 6) & 7, (cur + 2) & 7]
        } else {
            &[(cur + 7) & 7, (cur + 1) & 7]
        };
        for &n in adj {
            if nb[n] && !visited[n] {
                visited[n] = true;
                stack[sp] = n;
                sp += 1;
            }
        }
    }
    seen == count
}

fn remove_l_corners(skel: &mut [bool], w: usize, h: usize) {
    if w < 3 || h < 3 {
        return;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let idx = y * w + x;
            if !skel[idx] {
                continue;
            }
            let nb = neighbors_cw(skel, w, x, y);
            let is_l = (nb[0] && nb[2] && !nb[5])
                || (nb[2] && nb[4] && !nb[7])
                || (nb[4] && nb[6] && !nb[1])
                || (nb[6] && nb[0] && !nb[3]);
            if is_l && removing_keeps_connectivity(nb) {
                skel[idx] = false;
            }
        }
    }
}

struct GraphNode {
    pos: (f64, f64),
    r: f64,
    alive: bool,
}

struct GraphEdge {
    a: usize,
    b: usize,
    pts: Vec<(f64, f64)>,
    closed_loop: bool,
    alive: bool,
}

struct SkelGraph {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

impl SkelGraph {
    fn node_degree(&self, u: usize) -> usize {
        let mut deg = 0usize;
        for e in &self.edges {
            if !e.alive || e.closed_loop {
                continue;
            }
            if e.a == u {
                deg += 1;
            }
            if e.b == u {
                deg += 1;
            }
        }
        deg
    }
}

#[inline]
fn poly_len(pts: &[(f64, f64)]) -> f64 {
    pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum()
}

fn neighbor_indices(skel: &[bool], w: usize, h: usize, idx: usize) -> Vec<usize> {
    let x = (idx % w) as isize;
    let y = (idx / w) as isize;
    let mut out = Vec::with_capacity(8);
    for dy in -1..=1_isize {
        for dx in -1..=1_isize {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x + dx;
            let ny = y + dy;
            if nx >= 0 && ny >= 0 && nx < w as isize && ny < h as isize {
                let ni = ny as usize * w + nx as usize;
                if skel[ni] {
                    out.push(ni);
                }
            }
        }
    }
    out
}

fn build_skeleton_graph(skel: &[bool], sdf: &[f32], w: usize, h: usize) -> SkelGraph {
    let n = w * h;
    let mut deg = vec![0u8; n];
    for idx in 0..n {
        if skel[idx] {
            deg[idx] = neighbor_indices(skel, w, h, idx).len() as u8;
        }
    }

    let mut pixel_node: Vec<Option<usize>> = vec![None; n];
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut queue = Vec::new();

    for start in 0..n {
        if !skel[start] || deg[start] < 3 || pixel_node[start].is_some() {
            continue;
        }
        let nid = nodes.len();
        pixel_node[start] = Some(nid);
        queue.clear();
        queue.push(start);
        let (mut sx, mut sy, mut cnt, mut max_r) = (0.0_f64, 0.0_f64, 0.0_f64, 1.0_f64);
        let mut head = 0usize;
        while head < queue.len() {
            let cur = queue[head];
            head += 1;
            sx += (cur % w) as f64;
            sy += (cur / w) as f64;
            cnt += 1.0;
            max_r = max_r.max(f64::from(sdf[cur]));
            for nb in neighbor_indices(skel, w, h, cur) {
                if deg[nb] >= 3 && pixel_node[nb].is_none() {
                    pixel_node[nb] = Some(nid);
                    queue.push(nb);
                }
            }
        }
        nodes.push(GraphNode { pos: (sx / cnt, sy / cnt), r: max_r, alive: true });
    }

    for idx in 0..n {
        if !skel[idx] || deg[idx] != 1 || pixel_node[idx].is_some() {
            continue;
        }
        let nbs = neighbor_indices(skel, w, h, idx);
        if let Some(&adj_j) = nbs.iter().find(|&&nb| pixel_node[nb].is_some()) {
            pixel_node[idx] = pixel_node[adj_j];
            continue;
        }
        let nid = nodes.len();
        pixel_node[idx] = Some(nid);
        nodes.push(GraphNode {
            pos: ((idx % w) as f64, (idx / w) as f64),
            r: f64::from(sdf[idx]).max(1.0),
            alive: true,
        });
    }

    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut visited_mid = vec![false; n];

    for start in 0..n {
        if !skel[start] || pixel_node[start].is_some() || visited_mid[start] {
            continue;
        }
        visited_mid[start] = true;
        let nbs = neighbor_indices(skel, w, h, start);
        if nbs.is_empty() {
            continue;
        }
        let (fwd, end_b, looped) = walk_branch(skel, &pixel_node, &mut visited_mid, w, h, start, nbs[0]);
        if looped {
            let mut pts = Vec::with_capacity(fwd.len() + 1);
            pts.push(((start % w) as f64, (start / w) as f64));
            pts.extend(fwd);
            if pts.len() >= 4 {
                edges.push(GraphEdge { a: usize::MAX, b: usize::MAX, pts, closed_loop: true, alive: true });
            }
            continue;
        }
        let (bwd, end_a) = if nbs.len() >= 2 {
            let (chain, node_a, _) = walk_branch(skel, &pixel_node, &mut visited_mid, w, h, start, nbs[1]);
            (chain, node_a)
        } else {
            (Vec::new(), None)
        };
        if let (Some(na), Some(nb)) = (end_a, end_b) {
            let mut pts = Vec::with_capacity(bwd.len() + fwd.len() + 3);
            pts.push(nodes[na].pos);
            for &p in bwd.iter().rev() {
                pts.push(p);
            }
            pts.push(((start % w) as f64, (start / w) as f64));
            pts.extend(fwd);
            pts.push(nodes[nb].pos);
            edges.push(GraphEdge { a: na, b: nb, pts, closed_loop: false, alive: true });
        }
    }

    for idx in 0..n {
        let Some(na) = pixel_node[idx] else {
            continue;
        };
        if deg[idx] != 1 {
            continue;
        }
        for nb in neighbor_indices(skel, w, h, idx) {
            if nb > idx
                && deg[nb] == 1
                && let Some(nb_id) = pixel_node[nb]
                && nb_id != na
            {
                edges.push(GraphEdge {
                    a: na,
                    b: nb_id,
                    pts: vec![nodes[na].pos, nodes[nb_id].pos],
                    closed_loop: false,
                    alive: true,
                });
            }
        }
    }

    SkelGraph { nodes, edges }
}

fn walk_branch(
    skel: &[bool],
    pixel_node: &[Option<usize>],
    visited_mid: &mut [bool],
    w: usize,
    h: usize,
    origin: usize,
    first: usize,
) -> (Vec<(f64, f64)>, Option<usize>, bool) {
    let mut pts = Vec::new();
    let mut prev = origin;
    let mut cur = first;
    for step in 0..(w * h) {
        if let Some(nid) = pixel_node[cur] {
            return (pts, Some(nid), false);
        }
        if cur == origin && step > 0 {
            return (pts, None, true);
        }
        visited_mid[cur] = true;
        pts.push(((cur % w) as f64, (cur / w) as f64));

        let nbs = neighbor_indices(skel, w, h, cur);
        let mut next_mid = None;
        let mut hit_node = None;
        for nb in nbs {
            if nb == prev {
                continue;
            }
            if let Some(nid) = pixel_node[nb] {
                hit_node = Some(nid);
                break;
            }
            if nb == origin && step >= 2 {
                return (pts, None, true);
            }
            if !visited_mid[nb] {
                next_mid = Some(nb);
            }
        }
        if let Some(nid) = hit_node {
            return (pts, Some(nid), false);
        }
        match next_mid {
            Some(nx) => {
                prev = cur;
                cur = nx;
            }
            None => return (pts, None, false),
        }
    }
    (pts, None, false)
}

/// Merges pairs of high-degree junctions separated by a short internal bridge (`<= ~2.0 * r`),
/// collapsing split X/K-waists (`К`, `Ж`, `M`, `B`) into a single crisp crossing node.
fn collapse_short_junction_bridges(g: &mut SkelGraph) {
    for _ in 0..32 {
        let mut merge_edge = None;
        let mut best_ratio = f64::INFINITY;
        for (ei, e) in g.edges.iter().enumerate() {
            if !e.alive || e.closed_loop || e.a == e.b {
                continue;
            }
            if g.node_degree(e.a) >= 3 && g.node_degree(e.b) >= 3 {
                let min_r = g.nodes[e.a].r.min(g.nodes[e.b].r).max(2.0);
                let lim = (min_r * BRIDGE_COLLAPSE_SCALE).max(5.0);
                let len = poly_len(&e.pts);
                if len <= lim && (len / min_r) < best_ratio {
                    best_ratio = len / min_r;
                    merge_edge = Some(ei);
                }
            }
        }
        let Some(ei) = merge_edge else {
            break;
        };
        let keep = g.edges[ei].a;
        let drop = g.edges[ei].b;
        let mid = (
            (g.nodes[keep].pos.0 + g.nodes[drop].pos.0) * 0.5,
            (g.nodes[keep].pos.1 + g.nodes[drop].pos.1) * 0.5,
        );
        let new_r = g.nodes[keep].r.max(g.nodes[drop].r);
        g.nodes[keep].pos = mid;
        g.nodes[keep].r = new_r;
        g.nodes[drop].alive = false;
        g.edges[ei].alive = false;

        for e in &mut g.edges {
            if !e.alive || e.closed_loop {
                continue;
            }
            if e.a == drop {
                e.a = keep;
                if let Some(first) = e.pts.first_mut() {
                    *first = mid;
                }
            } else if e.a == keep
                && let Some(first) = e.pts.first_mut()
            {
                *first = mid;
            }
            if e.b == drop {
                e.b = keep;
                if let Some(last) = e.pts.last_mut() {
                    *last = mid;
                }
            } else if e.b == keep
                && let Some(last) = e.pts.last_mut()
            {
                *last = mid;
            }
            if e.a == e.b && poly_len(&e.pts) < new_r * 3.4 {
                e.alive = false;
            }
        }
    }
}

fn prune_scale_axis_spurs(g: &mut SkelGraph) {
    for _ in 0..16 {
        let mut to_kill: Vec<(usize, usize)> = Vec::new();
        for u in 0..g.nodes.len() {
            if !g.nodes[u].alive {
                continue;
            }
            let deg_u = g.node_degree(u);
            if deg_u < 3 {
                continue;
            }
            let r_u = g.nodes[u].r;
            let p_u = g.nodes[u].pos;
            let mut spurs: Vec<(usize, usize, f64)> = Vec::new();
            for (ei, e) in g.edges.iter().enumerate() {
                if !e.alive || e.closed_loop || e.a == e.b {
                    continue;
                }
                let v = if e.a == u {
                    e.b
                } else if e.b == u {
                    e.a
                } else {
                    continue;
                };
                if g.node_degree(v) != 1 {
                    continue;
                }
                let p_v = g.nodes[v].pos;
                let r_v = g.nodes[v].r;
                let d_uv = (p_v.0 - p_u.0).hypot(p_v.1 - p_u.1);
                let arc = poly_len(&e.pts);
                if (d_uv + r_v * 0.60 <= PRUNE_SCALE * r_u + 1.5) && (arc <= PRUNE_SCALE * 1.28 * r_u + 2.5) {
                    spurs.push((ei, v, arc));
                }
            }
            if spurs.is_empty() {
                continue;
            }
            if spurs.len() >= deg_u {
                spurs.sort_by(|a, b| a.2.total_cmp(&b.2));
                spurs.pop();
            }
            for (ei, v, _) in spurs {
                to_kill.push((ei, v));
            }
        }
        if to_kill.is_empty() {
            break;
        }
        for (ei, v) in to_kill {
            g.edges[ei].alive = false;
            g.nodes[v].alive = false;
        }
        splice_degree_two_nodes(g);
    }
    splice_degree_two_nodes(g);
}

fn splice_degree_two_nodes(g: &mut SkelGraph) {
    loop {
        let mut spliced = false;
        for u in 0..g.nodes.len() {
            if !g.nodes[u].alive {
                continue;
            }
            let mut inc = Vec::new();
            for (ei, e) in g.edges.iter().enumerate() {
                if e.alive && !e.closed_loop && (e.a == u || e.b == u) {
                    inc.push(ei);
                }
            }
            if inc.len() == 1 {
                let ei = inc[0];
                if g.edges[ei].a == u && g.edges[ei].b == u {
                    g.edges[ei].closed_loop = true;
                    g.edges[ei].a = usize::MAX;
                    g.edges[ei].b = usize::MAX;
                    g.nodes[u].alive = false;
                    spliced = true;
                }
                continue;
            }
            if inc.len() != 2 {
                continue;
            }
            let (e1, e2) = (inc[0], inc[1]);
            if g.edges[e1].a == g.edges[e1].b || g.edges[e2].a == g.edges[e2].b {
                continue;
            }
            let mut p1 = g.edges[e1].pts.clone();
            let other1 = if g.edges[e1].b == u {
                g.edges[e1].a
            } else {
                p1.reverse();
                g.edges[e1].b
            };
            let mut p2 = g.edges[e2].pts.clone();
            let other2 = if g.edges[e2].a == u {
                g.edges[e2].b
            } else {
                p2.reverse();
                g.edges[e2].a
            };
            if p2.len() > 1 {
                p1.extend_from_slice(&p2[1..]);
            }
            g.edges[e1].a = other1;
            g.edges[e1].b = other2;
            g.edges[e1].pts = p1;
            g.edges[e2].alive = false;
            g.nodes[u].alive = false;
            spliced = true;
        }
        if !spliced {
            break;
        }
    }
}

fn sample_sdf_bilinear(sdf: &[f32], w: usize, h: usize, x: f64, y: f64) -> f64 {
    if w == 0 || h == 0 {
        return 1.0;
    }
    let xf = x.clamp(0.0, (w - 1) as f64);
    let yf = y.clamp(0.0, (h - 1) as f64);
    let x0 = xf.floor() as usize;
    let y0 = yf.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = xf - x0 as f64;
    let ty = yf - y0 as f64;
    let v00 = f64::from(sdf[y0 * w + x0]);
    let v10 = f64::from(sdf[y0 * w + x1]);
    let v01 = f64::from(sdf[y1 * w + x0]);
    let v11 = f64::from(sdf[y1 * w + x1]);
    let top = v00 + (v10 - v00) * tx;
    let bot = v01 + (v11 - v01) * tx;
    (top + (bot - top) * ty).max(1.0)
}

/// Non-shrinking Taubin smoothing (`lambda = 0.50`, `mu = -0.53`) eliminates pixel-grid
/// staircases without shrinking loops or flattening curved letter bowls.
fn taubin_smooth_polyline(pts: &mut [(f64, f64)], closed: bool, iters: usize) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    let mut tmp = vec![(0.0_f64, 0.0_f64); n];
    for _ in 0..iters {
        for weight in [TAUBIN_LAMBDA, TAUBIN_MU] {
            tmp.copy_from_slice(pts);
            if closed {
                for i in 0..n {
                    let p = tmp[(i + n - 1) % n];
                    let c = tmp[i];
                    let q = tmp[(i + 1) % n];
                    let avg_x = 0.5 * (p.0 + q.0);
                    let avg_y = 0.5 * (p.1 + q.1);
                    pts[i] = (c.0 + weight * (avg_x - c.0), c.1 + weight * (avg_y - c.1));
                }
            } else {
                for i in 1..n - 1 {
                    let p = tmp[i - 1];
                    let c = tmp[i];
                    let q = tmp[i + 1];
                    let avg_x = 0.5 * (p.0 + q.0);
                    let avg_y = 0.5 * (p.1 + q.1);
                    pts[i] = (c.0 + weight * (avg_x - c.0), c.1 + weight * (avg_y - c.1));
                }
            }
        }
    }
}

fn outgoing_tangent(pts: &[(f64, f64)], window: f64) -> (f64, f64) {
    if pts.len() < 2 {
        return (1.0, 0.0);
    }
    let p0 = pts[0];
    let mut acc = 0.0_f64;
    let mut target = pts[pts.len() - 1];
    for i in 1..pts.len() {
        let seg = (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
        acc += seg;
        if acc >= window {
            target = pts[i];
            break;
        }
    }
    let dx = target.0 - p0.0;
    let dy = target.1 - p0.1;
    let l = dx.hypot(dy).max(1e-6);
    (dx / l, dy / l)
}

struct RawFlowChain {
    pts: Vec<(f64, f64, f64)>,
    closed: bool,
    start_kind: FlowEndKind,
    end_kind: FlowEndKind,
}

type EdgePartner = [Option<(usize, usize)>; 2];

fn build_junction_partners(g: &SkelGraph, smoothed: &[Option<Vec<(f64, f64)>>]) -> Vec<EdgePartner> {
    let mut partner: Vec<EdgePartner> = vec![[None, None]; g.edges.len()];
    for u in 0..g.nodes.len() {
        if !g.nodes[u].alive {
            continue;
        }
        let win = (g.nodes[u].r * 1.35).max(6.0);
        let mut arms: Vec<(usize, usize, (f64, f64))> = Vec::new();
        for (ei, e) in g.edges.iter().enumerate() {
            if !e.alive || e.closed_loop {
                continue;
            }
            let Some(pts) = smoothed[ei].as_ref() else {
                continue;
            };
            if e.a == u {
                arms.push((ei, 0, outgoing_tangent(pts, win)));
            }
            if e.b == u {
                let rev: Vec<(f64, f64)> = pts.iter().copied().rev().collect();
                arms.push((ei, 1, outgoing_tangent(&rev, win)));
            }
        }
        if arms.len() < 2 {
            continue;
        }
        let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
        for i in 0..arms.len() {
            for j in (i + 1)..arms.len() {
                if arms[i].0 == arms[j].0 {
                    continue;
                }
                let dot = arms[i].2.0 * arms[j].2.0 + arms[i].2.1 * arms[j].2.1;
                if dot <= -0.72 {
                    pairs.push((dot, i, j));
                }
            }
        }
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut used = vec![false; arms.len()];
        for (_, i, j) in pairs {
            if used[i] || used[j] {
                continue;
            }
            used[i] = true;
            used[j] = true;
            let (ei, si, _) = arms[i];
            let (ej, sj, _) = arms[j];
            partner[ei][si] = Some((ej, sj));
            partner[ej][sj] = Some((ei, si));
        }
    }
    partner
}

struct ChainedRaw {
    pts: Vec<(f64, f64)>,
    closed: bool,
    start_kind: FlowEndKind,
    end_kind: FlowEndKind,
}

fn finalize_open_segment(mut seg: Vec<(f64, f64, f64)>, mut start_kind: FlowEndKind, mut end_kind: FlowEndKind, out: &mut Vec<RawFlowChain>) {
    if seg.len() < 2 {
        return;
    }
    // Any remaining short isolated junction-to-junction bridge must never shoot needles.
    if start_kind == FlowEndKind::Junction && end_kind == FlowEndKind::Junction {
        let len: f64 = seg.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
        let mut rs: Vec<f64> = seg.iter().map(|p| p.2).collect();
        rs.sort_by(f64::total_cmp);
        let r_med = rs[rs.len() / 2].max(1.5);
        if len <= r_med * 2.95 {
            start_kind = FlowEndKind::ShortBridge;
            end_kind = FlowEndKind::ShortBridge;
        }
    }
    trim_terminal_corner_bend(&mut seg, start_kind, end_kind);
    extend_open_stroke_to_boundary(&mut seg, start_kind, end_kind);
    if seg.len() >= 2 {
        out.push(RawFlowChain { pts: seg, closed: false, start_kind, end_kind });
    }
}

fn split_chain_at_corners(
    with_r: Vec<(f64, f64, f64)>,
    closed: bool,
    start_kind: FlowEndKind,
    end_kind: FlowEndKind,
    out: &mut Vec<RawFlowChain>,
) {
    let splits = find_sharp_corners(&with_r, closed);
    if splits.is_empty() {
        if closed {
            out.push(RawFlowChain { pts: with_r, closed: true, start_kind, end_kind });
        } else {
            finalize_open_segment(with_r, start_kind, end_kind, out);
        }
        return;
    }
    if closed {
        let n = with_r.len();
        for k in 0..splits.len() {
            let s = splits[k];
            let e = splits[(k + 1) % splits.len()];
            let mut seg = Vec::new();
            let mut idx = s;
            for _ in 0..=n {
                seg.push(with_r[idx]);
                if idx == e && seg.len() >= 2 {
                    break;
                }
                idx = (idx + 1) % n;
            }
            finalize_open_segment(seg, FlowEndKind::Junction, FlowEndKind::Junction, out);
        }
        return;
    }
    let mut prev = 0usize;
    let mut cur_start = start_kind;
    for sp in splits {
        let seg = with_r[prev..=sp].to_vec();
        finalize_open_segment(seg, cur_start, FlowEndKind::Junction, out);
        prev = sp;
        cur_start = FlowEndKind::Junction;
    }
    let tail = with_r[prev..].to_vec();
    finalize_open_segment(tail, cur_start, end_kind, out);
}

fn chain_and_split_graph(g: &SkelGraph, sdf: &[f32], w: usize, h: usize) -> Vec<RawFlowChain> {
    let mut smoothed: Vec<Option<Vec<(f64, f64)>>> = vec![None; g.edges.len()];
    let mut out_chains: Vec<ChainedRaw> = Vec::new();
    for (ei, e) in g.edges.iter().enumerate() {
        if !e.alive || e.pts.len() < 2 {
            continue;
        }
        let mut pts = e.pts.clone();
        if e.closed_loop {
            if pts.len() >= 2
                && let (Some(&f), Some(&l)) = (pts.first(), pts.last())
                && (f.0 - l.0).hypot(f.1 - l.1) < 1.5
            {
                pts.pop();
            }
            taubin_smooth_polyline(&mut pts, true, 20);
            out_chains.push(ChainedRaw {
                pts,
                closed: true,
                start_kind: FlowEndKind::FreeTip,
                end_kind: FlowEndKind::FreeTip,
            });
        } else {
            taubin_smooth_polyline(&mut pts, false, 20);
            smoothed[ei] = Some(pts);
        }
    }

    let partner = build_junction_partners(g, &smoothed);
    let mut used_edge = vec![false; g.edges.len()];
    let node_kind = |node_id: usize| -> FlowEndKind {
        if g.node_degree(node_id) <= 1 { FlowEndKind::FreeTip } else { FlowEndKind::Junction }
    };

    for ei in 0..g.edges.len() {
        if used_edge[ei] || smoothed[ei].is_none() {
            continue;
        }
        let start_side = if partner[ei][0].is_none() {
            Some(0)
        } else if partner[ei][1].is_none() {
            Some(1)
        } else {
            None
        };
        if let Some(side) = start_side {
            let (mut chain, start_node, end_node) = trace_partner_chain(ei, side, g, &smoothed, &partner, &mut used_edge);
            if chain.len() >= 2 {
                taubin_smooth_polyline(&mut chain, false, 12);
                out_chains.push(ChainedRaw {
                    pts: chain,
                    closed: false,
                    start_kind: node_kind(start_node),
                    end_kind: node_kind(end_node),
                });
            }
        }
    }
    for ei in 0..g.edges.len() {
        if used_edge[ei] || smoothed[ei].is_none() {
            continue;
        }
        let (mut chain, _, _) = trace_partner_chain(ei, 0, g, &smoothed, &partner, &mut used_edge);
        if chain.len() >= 4 {
            taubin_smooth_polyline(&mut chain, true, 16);
            out_chains.push(ChainedRaw {
                pts: chain,
                closed: true,
                start_kind: FlowEndKind::FreeTip,
                end_kind: FlowEndKind::FreeTip,
            });
        }
    }

    let mut final_strokes = Vec::new();
    for ch in out_chains {
        let with_r = attach_smoothed_radii(&ch.pts, sdf, w, h, ch.closed);
        split_chain_at_corners(with_r, ch.closed, ch.start_kind, ch.end_kind, &mut final_strokes);
    }
    final_strokes
}

fn trace_partner_chain(
    start_ei: usize,
    enter_side: usize,
    g: &SkelGraph,
    smoothed: &[Option<Vec<(f64, f64)>>],
    partner: &[EdgePartner],
    used: &mut [bool],
) -> (Vec<(f64, f64)>, usize, usize) {
    let mut out = Vec::new();
    let mut cur_ei = start_ei;
    let mut cur_enter = enter_side;
    let start_node = if cur_enter == 0 { g.edges[cur_ei].a } else { g.edges[cur_ei].b };
    let mut end_node = start_node;
    loop {
        if used[cur_ei] {
            break;
        }
        used[cur_ei] = true;
        let Some(pts) = smoothed[cur_ei].as_ref() else {
            break;
        };
        let exit_side = 1 - cur_enter;
        end_node = if exit_side == 0 { g.edges[cur_ei].a } else { g.edges[cur_ei].b };
        if cur_enter == 0 {
            if out.is_empty() {
                out.extend_from_slice(pts);
            } else if pts.len() > 1 {
                out.extend_from_slice(&pts[1..]);
            }
        } else if out.is_empty() {
            out.extend(pts.iter().copied().rev());
        } else if pts.len() > 1 {
            out.extend(pts.iter().rev().skip(1).copied());
        }
        match partner[cur_ei][exit_side] {
            Some((next_ei, next_enter)) if !used[next_ei] => {
                cur_ei = next_ei;
                cur_enter = next_enter;
            }
            _ => break,
        }
    }
    (out, start_node, end_node)
}

fn attach_smoothed_radii(pts: &[(f64, f64)], sdf: &[f32], w: usize, h: usize, closed: bool) -> Vec<(f64, f64, f64)> {
    let mut out: Vec<(f64, f64, f64)> = pts.iter().map(|&(x, y)| (x, y, sample_sdf_bilinear(sdf, w, h, x, y))).collect();
    let n = out.len();
    if n < 3 {
        return out;
    }
    let mut tmp = vec![0.0_f64; n];
    for _ in 0..4 {
        for (t, p) in tmp.iter_mut().zip(out.iter()) {
            *t = p.2;
        }
        for i in 0..n {
            let (p, q) = if closed {
                (tmp[(i + n - 1) % n], tmp[(i + 1) % n])
            } else {
                (tmp[i.saturating_sub(1)], tmp[(i + 1).min(n - 1)])
            };
            out[i].2 = (tmp[i] * 0.5 + (p + q) * 0.25).max(1.0);
        }
    }
    out
}

/// Trims acute corner bends at free stroke ends (such as the flat-cut slanted bottom feet of
/// `A`, `M`, `V`, `W` where the medial axis curves sideways as `r` collapses toward `1px`),
/// then projects straight forward along the steady stem tangent to the original tip extent.
fn trim_terminal_corner_bend(pts: &mut Vec<(f64, f64, f64)>, start_kind: FlowEndKind, end_kind: FlowEndKind) {
    if pts.len() < 8 {
        return;
    }
    let mut rs: Vec<f64> = pts.iter().map(|p| p.2).collect();
    rs.sort_by(f64::total_cmp);
    let r_steady = rs[rs.len() / 2];
    let total_len: f64 = pts.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).sum();
    if r_steady < 2.2 || total_len < r_steady * 3.4 {
        return;
    }

    if start_kind == FlowEndKind::FreeTip && pts[0].2 < r_steady * 0.78 {
        let mut acc = 0.0_f64;
        let mut cut = 0usize;
        for i in 1..(pts.len() / 2) {
            acc += (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
            if pts[i].2 >= r_steady * 0.82 {
                cut = i;
                break;
            }
            if acc > r_steady * 2.3 {
                break;
            }
        }
        if cut >= 2 && cut + 4 < pts.len() {
            let old_tip = pts[0];
            pts.drain(0..cut);
            let xy: Vec<(f64, f64)> = pts.iter().map(|&(x, y, _)| (x, y)).collect();
            let t_in = outgoing_tangent(&xy, (r_steady * 1.4).max(6.0));
            let anchor = pts[0];
            let proj = ((anchor.0 - old_tip.0) * t_in.0 + (anchor.1 - old_tip.1) * t_in.1).clamp(0.0, r_steady * 2.1);
            if proj > 0.5 {
                pts.insert(0, (anchor.0 - t_in.0 * proj, anchor.1 - t_in.1 * proj, anchor.2));
            }
        }
    }

    if end_kind == FlowEndKind::FreeTip
        && let Some(&last_p) = pts.last()
        && last_p.2 < r_steady * 0.78
    {
        let n = pts.len();
        let mut acc = 0.0_f64;
        let mut cut = n - 1;
        for i in (n / 2..n - 1).rev() {
            acc += (pts[i + 1].0 - pts[i].0).hypot(pts[i + 1].1 - pts[i].1);
            if pts[i].2 >= r_steady * 0.82 {
                cut = i;
                break;
            }
            if acc > r_steady * 2.3 {
                break;
            }
        }
        if cut + 2 < n && cut >= 4 {
            let old_tip = pts[n - 1];
            pts.truncate(cut + 1);
            let rev_xy: Vec<(f64, f64)> = pts.iter().rev().map(|&(x, y, _)| (x, y)).collect();
            let t_in = outgoing_tangent(&rev_xy, (r_steady * 1.4).max(6.0));
            let anchor = pts[pts.len() - 1];
            let proj = ((anchor.0 - old_tip.0) * t_in.0 + (anchor.1 - old_tip.1) * t_in.1).clamp(0.0, r_steady * 2.1);
            if proj > 0.5 {
                pts.push((anchor.0 - t_in.0 * proj, anchor.1 - t_in.1 * proj, anchor.2));
            }
        }
    }
}

/// Finds genuine sharp corner indices (`turn > ~62°`) along a smoothed centerline.
fn find_sharp_corners(pts: &[(f64, f64, f64)], closed: bool) -> Vec<usize> {
    const COS_SPLIT: f64 = 0.46;
    let n = pts.len();
    if n < 7 {
        return Vec::new();
    }
    let step_idx = |start: usize, dir: isize, target: f64| -> Option<(f64, f64)> {
        let mut cur = start;
        let mut acc = 0.0_f64;
        for _ in 0..n {
            let next_i = cur as isize + dir;
            let next = if closed {
                next_i.rem_euclid(n as isize) as usize
            } else {
                if next_i < 0 || next_i >= n as isize {
                    return (acc >= target * 0.45).then_some((pts[cur].0, pts[cur].1));
                }
                next_i as usize
            };
            acc += (pts[next].0 - pts[cur].0).hypot(pts[next].1 - pts[cur].1);
            cur = next;
            if acc >= target {
                return Some((pts[cur].0, pts[cur].1));
            }
        }
        None
    };

    let mut scores = vec![1.0_f64; n];
    let range = if closed { 0..n } else { 3..(n.saturating_sub(3)) };
    for i in range.clone() {
        let win = (pts[i].2 * 0.95).clamp(5.0, 18.0);
        let (Some(p_back), Some(p_fwd)) = (step_idx(i, -1, win), step_idx(i, 1, win)) else {
            continue;
        };
        let v1 = (pts[i].0 - p_back.0, pts[i].1 - p_back.1);
        let v2 = (p_fwd.0 - pts[i].0, p_fwd.1 - pts[i].1);
        let l1 = v1.0.hypot(v1.1);
        let l2 = v2.0.hypot(v2.1);
        if l1 > 1.0 && l2 > 1.0 {
            scores[i] = ((v1.0 * v2.0 + v1.1 * v2.1) / (l1 * l2)).clamp(-1.0, 1.0);
        }
    }

    let mut raw_corners = Vec::new();
    for i in range {
        if scores[i] >= COS_SPLIT {
            continue;
        }
        let prev_i = if closed { (i + n - 1) % n } else { i - 1 };
        let next_i = if closed { (i + 1) % n } else { i + 1 };
        if scores[i] <= scores[prev_i] && scores[i] <= scores[next_i] {
            raw_corners.push(i);
        }
    }

    let mut kept: Vec<usize> = Vec::new();
    for c in raw_corners {
        let min_sep = (pts[c].2 * 1.25).max(6.0);
        let too_close = kept.iter().any(|&k| (pts[c].0 - pts[k].0).hypot(pts[c].1 - pts[k].1) < min_sep);
        if !too_close {
            kept.push(c);
        }
    }
    kept.sort_unstable();
    kept
}

/// Extends open stroke ends forward along their end tangents by `~0.92 * r_end` so the
/// centerline reaches the true glyph silhouette (short bridges keep a tiny seal only).
fn extend_open_stroke_to_boundary(pts: &mut Vec<(f64, f64, f64)>, start_kind: FlowEndKind, end_kind: FlowEndKind) {
    if pts.len() < 2 {
        return;
    }
    let xy: Vec<(f64, f64)> = pts.iter().map(|&(x, y, _)| (x, y)).collect();
    if start_kind != FlowEndKind::ShortBridge {
        let r0 = pts[0].2;
        let t_start = outgoing_tangent(&xy, (r0 * 1.2).max(5.0));
        let ext = r0 * if start_kind == FlowEndKind::FreeTip { 0.94 } else { 0.82 };
        let head = (pts[0].0 - t_start.0 * ext, pts[0].1 - t_start.1 * ext, (r0 * 0.88).max(1.0));
        if head.0.is_finite() && head.1.is_finite() {
            pts.insert(0, head);
        }
    }
    if end_kind != FlowEndKind::ShortBridge {
        let rev_xy: Vec<(f64, f64)> = xy.into_iter().rev().collect();
        let last = pts[pts.len() - 1];
        let r1 = last.2;
        let t_end = outgoing_tangent(&rev_xy, (r1 * 1.2).max(5.0));
        let ext = r1 * if end_kind == FlowEndKind::FreeTip { 0.94 } else { 0.82 };
        let tail = (last.0 - t_end.0 * ext, last.1 - t_end.1 * ext, (r1 * 0.88).max(1.0));
        if tail.0.is_finite() && tail.1.is_finite() {
            pts.push(tail);
        }
    }
}
