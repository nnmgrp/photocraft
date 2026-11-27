//! Port of `nawa-ami` (v3–v7.4b) geometry engine from `src/02-geometry.js`.
//!
//! Submodules:
//! * `skeleton` — XP-Pipes continuous streams (`skein`) + legacy `arcs` skeleton;
//! * `swarm`    — Strip Poisson-Disc `Swarm` dot cache (`OnceLock`) & splice-tail emitter;
//! * `emit`     — painter's order chunk emission (`Normal`, `Arcs`, `Chain`, `Swarm`, `pixel1`, `pencil`).

pub(crate) mod emit;
pub(crate) mod skeleton;
pub(crate) mod swarm;

use crate::config::KakeamiConfig;
use crate::geometry::Builder;
use crate::grid::{Grid, Rect};
use crate::math::{clamp, ValueNoise2D};

pub use skeleton::{build_nawa_skeleton, NawaSkeleton, NawaSkeletonStats};

pub(crate) fn build_nawa_ami(
    b: &mut Builder,
    cfg: &KakeamiConfig,
    grid: &Grid,
    noise: &ValueNoise2D,
    rect: &Rect,
    skeleton: &NawaSkeleton,
) {
    let n_scale_cull = clamp(cfg.nawa_scale, 0.40, 4.50);
    let cull_pad = (cfg.nawa_rope_width * n_scale_cull * 0.15).max(4.0);
    let rx0 = rect.x - cull_pad;
    let rx1 = rect.x + rect.w + cull_pad;
    let ry0 = rect.y - cull_pad;
    let ry1 = rect.y + rect.h + cull_pad;

    for ck in &skeleton.chunks {
        if ck.max_x < rx0 || ck.min_x > rx1 || ck.max_y < ry0 || ck.min_y > ry1 {
            continue;
        }
        emit::emit_nawa_polygonal_chunk(b, cfg, grid, noise, skeleton, ck, rect);
    }
}
