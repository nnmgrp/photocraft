//! Wasm-pure manga domain logic shared across PhotoCraft (`S1`+).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod comic_screentone;
pub mod snow_hyphenation;
pub mod snow_knuth_plass;
