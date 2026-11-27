//! Wasm-pure manga domain logic shared across PhotoCraft (`S1`+).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod balloon_fit;
pub mod comic_screentone;
pub mod sfx_geom;
pub mod sfx_heavy_stack;
pub mod sfx_kakeami;
pub mod sfx_render;
pub mod sfx_shibufude;
mod sfx_stroke_flow;
pub mod sfx_sugomi;
pub mod sfx_v3_common;
pub mod snow_hyphenation;
pub mod snow_knuth_plass;
pub mod snow_typesetter;
