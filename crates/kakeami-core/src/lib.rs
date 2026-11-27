#![forbid(unsafe_code)]

//! # kakeami-core
//!
//! Pure, DOM-free, `wasm32`-compatible Rust port of the Kakeami 4K Tone Studio engine
//! (Nawa v7.4b + Kakeami v8.0c + Shibufude v3 Pencil).
//! The JavaScript oracle lives in `src/01-core.js` … `src/03-raster.js`;
//! `docs/geometry-format.md` is the normative data contract.

pub mod config;
pub mod density;
pub mod geometry;
mod geometry_kakeami;
pub mod grid;
pub mod math;
mod nawa;
pub mod raster;

pub use config::{KakeamiConfig, KEY_ORDER};
pub use density::{sample_density, RAMP_9, RAMP_WHITE};
pub use geometry::{build_ribbons, PreparedScene, RibbonBatch, RibbonMeta};
pub use grid::{Grid, Rect};
pub use math::{hash2d, hash_u32, js_round, mod_f64, Mulberry32, ValueNoise2D};
pub use nawa::NawaSkeletonStats;
pub use raster::{
    compute_tile_pad, render_region_tiled, render_tile_gray, render_tile_gray_into, RasterScratch,
    Rgba,
};
