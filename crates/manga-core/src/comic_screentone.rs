//! Manga screentone presets, distance primitives, and raster renderer.
//!
//! Re-exported from [`photocraft_algo::comic_screentone`] so `manga-core` and
//! the CPU/GPU rasterizers share a single source of truth for presets, SDFs,
//! and gradient math.

pub use photocraft_algo::comic_screentone::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_presets_include_all_categories() {
        let presets = built_in_presets();
        assert!(presets.len() >= 26, "expected canonical presets, got {}", presets.len());
        assert!(presets.iter().any(|p| p.category == "Compound & Satellites"));
        assert!(presets.iter().any(|p| p.category == "Calibrated"));
    }
}
