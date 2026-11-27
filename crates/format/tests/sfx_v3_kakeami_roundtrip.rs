//! Wiring Gate integration test: `.pcraft` Save/Load bit-exact round-trip for `LayerContent::Kakeami`
//! (Phase 2A) and all 4 SFX v3 styles (`ShibufudeV3`, `SugomiV3`, `HeavyStackV3`, `KakeamiV4`).

use std::sync::Arc;

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::sfx::{
    SfxData, SfxFillSource, SfxHeavyStackMode, SfxKakeamiV4Mode, SfxLineStyle, SfxShibufudeProfile, SfxSugomiDirection, SfxV3EdgeMode,
};
use photocraft_doc::{Document, KakeamiConfig, KakeamiLayer, Layer, LayerContent};
use photocraft_format::{SaveOptions, load_from_bytes, save_to_bytes};
use photocraft_geom::Size;
use photocraft_raster::Surface;

fn sfx_layer(name: &str, data: SfxData) -> Layer {
    let mut layer = Layer::new(name, LayerContent::Raster(Surface::new(PixelFormat::RGBA8)));
    layer.sfx = Some(Arc::new(data));
    layer
}

#[test]
fn pcraft_roundtrip_preserves_kakeami_and_all_four_sfx_v3_styles_bit_for_bit() {
    let mut doc = Document::new("roundtrip", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);

    // 1. LayerContent::Kakeami with non-default parameters & custom tint
    let mut kcfg = KakeamiConfig::default();
    kcfg.pattern_family = "nawa-ami".to_owned();
    kcfg.finish_mode = "pencil".to_owned();
    kcfg.nawa_tone_style = "denseWhite".to_owned();
    kcfg.darken_cap = 88;
    kcfg.grad_start = 25;
    kcfg.grad_end = 92;
    kcfg.seed = 4242;
    let mut klayer = KakeamiLayer::new(kcfg.clone());
    klayer.color = [0.15, 0.25, 0.75, 0.9];
    klayer.dpi_sync = false;
    doc.layers.push(Layer::new("Kakeami-Custom", LayerContent::Kakeami(klayer)));

    // 2. Style 1: Shibufude v3 (Maru + Rough + knockout)
    let mut shib = SfxData { text: "ドン!".to_owned(), fill: SfxFillSource::None, ..SfxData::default() };
    shib.line_effect.style = SfxLineStyle::ShibufudeV3;
    shib.line_effect.line_width_px = 16.5;
    shib.line_effect.shibufude_profile = SfxShibufudeProfile::Maru;
    shib.line_effect.shibufude_edge_mode = SfxV3EdgeMode::Rough;
    shib.line_effect.shibufude_knockout = true;
    shib.line_effect.shibufude_roughness_pct = 83.0;
    shib.line_effect.shibufude_splatter_pct = 71.0;
    shib.line_effect.shibufude_spread_px = 52.0;
    shib.line_effect.shibufude_splat_size_pct = 135.0;
    shib.line_effect.shibufude_dryness_pct = 64.0;
    shib.line_effect.shibufude_sharpness_pct = 55.0;
    shib.line_effect.shibufude_seed = 77;
    doc.layers.push(sfx_layer("SFX-Shibufude", shib.clone()));

    // 3. Style 2: Sugomi v3 (Tate + Hollow + Rough)
    let mut sugo = SfxData { text: "ゴゴゴ".to_owned(), fill: SfxFillSource::None, ..SfxData::default() };
    sugo.line_effect.style = SfxLineStyle::SugomiV3;
    sugo.line_effect.sugomi_direction = SfxSugomiDirection::Tate;
    sugo.line_effect.sugomi_hollow = true;
    sugo.line_effect.sugomi_edge_mode = SfxV3EdgeMode::Rough;
    sugo.line_effect.sugomi_spike_amp_pct = 112.0;
    sugo.line_effect.sugomi_cleft_dip_pct = 46.0;
    sugo.line_effect.sugomi_dryness_pct = 53.0;
    sugo.line_effect.sugomi_sharpness_pct = 62.0;
    sugo.line_effect.sugomi_seed = 88;
    doc.layers.push(sfx_layer("SFX-Sugomi", sugo.clone()));

    // 4. Style 3: Heavy Stack v3 (FgsfdsStack + edge_bite_pct = 65.0 + Rough)
    let mut heavy = SfxData { text: "BAM!".to_owned(), fill: SfxFillSource::None, ..SfxData::default() };
    heavy.line_effect.style = SfxLineStyle::HeavyStackV3;
    heavy.line_effect.heavy_stack_mode = SfxHeavyStackMode::FgsfdsStack;
    heavy.line_effect.heavy_stack_edge_mode = SfxV3EdgeMode::Rough;
    heavy.line_effect.heavy_stack_intensity_pct = 84.0;
    heavy.line_effect.heavy_stack_edge_bite_pct = 65.0;
    heavy.line_effect.heavy_stack_dryness_pct = 49.0;
    heavy.line_effect.heavy_stack_sharpness_pct = 66.0;
    heavy.line_effect.heavy_stack_seed = 99;
    doc.layers.push(sfx_layer("SFX-HeavyStack", heavy.clone()));

    // 5. Style 4: Kakeami v4 (KakeEdge + custom gradient_angle_deg = 37.5)
    let mut kake = SfxData { text: "ズン".to_owned(), fill: SfxFillSource::None, ..SfxData::default() };
    kake.line_effect.style = SfxLineStyle::KakeamiV4;
    kake.line_effect.kakeami_mode = SfxKakeamiV4Mode::KakeEdge;
    kake.line_effect.kakeami_edge_mode = SfxV3EdgeMode::Rough;
    kake.line_effect.kakeami_angle_offset_deg = -14.5;
    kake.line_effect.kakeami_gradient_angle_deg = 37.5;
    kake.line_effect.kakeami_feather_pct = 128.0;
    kake.line_effect.kakeami_dryness_pct = 38.0;
    kake.line_effect.kakeami_sharpness_pct = 74.0;
    kake.line_effect.kakeami_seed = 123;
    doc.layers.push(sfx_layer("SFX-KakeamiV4", kake.clone()));

    let bytes = save_to_bytes(&doc, &SaveOptions::default()).expect("save .pcraft");
    let loaded = load_from_bytes(&bytes).expect("load .pcraft");

    // Verify LayerContent::Kakeami
    let k_loaded = loaded.layers.iter().find(|l| l.name == "Kakeami-Custom").expect("kakeami layer");
    match &k_loaded.content {
        LayerContent::Kakeami(k) => {
            assert_eq!(k.config.pattern_family, "nawa-ami");
            assert_eq!(k.config.finish_mode, "pencil");
            assert_eq!(k.config.nawa_tone_style, "denseWhite");
            assert_eq!(k.config.darken_cap, 88);
            assert_eq!(k.config.grad_start, 25);
            assert_eq!(k.config.grad_end, 92);
            assert_eq!(k.config.seed, 4242);
            assert_eq!(k.color, [0.15, 0.25, 0.75, 0.9]);
            assert!(!k.dpi_sync);
        }
        other => panic!("expected LayerContent::Kakeami, got {:?}", std::mem::discriminant(other)),
    }

    // Verify all 4 SFX v3 styles bit-for-bit
    let check_sfx = |name: &str, expected: &SfxData| {
        let l = loaded.layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("missing {name}"));
        let got = l.sfx.as_deref().unwrap_or_else(|| panic!("missing sfx on {name}"));
        assert_eq!(got, expected, "bit-exact round-trip mismatch on {name}");
    };
    check_sfx("SFX-Shibufude", &shib);
    check_sfx("SFX-Sugomi", &sugo);
    check_sfx("SFX-HeavyStack", &heavy);
    check_sfx("SFX-KakeamiV4", &kake);
}
