//! Wiring Gate integration test: `.pcraft` Save/Load bit-exact round-trip for
//! `LayerContent::Screentone` with Block C fields (`DotShape::Euclidean`, `PatternType::SandNoise`,
//! `PatternType::CustomTexture`, `sand_aspect_x`, `sand_aspect_y`, `sand_octaves_mix`,
//! `custom_pattern_id`, `custom_texture_scale`, `reflect_layer_opacity`).

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{
    Document, DotShape, Layer, LayerContent, Pattern, PatternType, ScreentoneBackend, ScreentoneLayer, ScreentoneMode, ScreentoneSpec,
};
use photocraft_format::{SaveOptions, load_from_bytes, save_to_bytes};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;

#[test]
fn pcraft_roundtrip_preserves_screentone_block_c_fields_and_pattern() {
    let mut doc = Document::new("block_c_roundtrip", Size::new(256, 256), ColorMode::Rgb, SampleType::U8);

    // Register a custom pattern tile in the document so CustomTexture references a real id.
    let mut tile = Surface::new(PixelFormat::RGBA8);
    tile.fill_rect(Rect::new(0, 0, 8, 8), &[1.0, 1.0, 1.0, 1.0]);
    tile.fill_rect(Rect::new(0, 0, 4, 4), &[0.0, 0.0, 0.0, 1.0]);
    let pat = Pattern::new("CrossTile", tile, 8, 8);
    let pat_id = pat.id.clone();
    doc.patterns.push(pat);

    let spec_euclid = ScreentoneSpec {
        pattern_type: PatternType::Dots,
        dot_shape: DotShape::Euclidean,
        spacing_x: 12.0,
        spacing_y: 12.0,
        density: 0.62,
        reflect_layer_opacity: true,
        mode: ScreentoneMode::Sheet,
        use_source_luminance: false,
        backend: ScreentoneBackend::Gpu,
        ..ScreentoneSpec::default()
    };
    let spec_sand = ScreentoneSpec {
        pattern_type: PatternType::SandNoise,
        spacing_x: 10.5,
        spacing_y: 10.5,
        density: 0.48,
        sand_aspect_x: 3.25,
        sand_aspect_y: 0.65,
        sand_octaves_mix: 0.42,
        mode: ScreentoneMode::Sheet,
        use_source_luminance: false,
        backend: ScreentoneBackend::Gpu,
        ..ScreentoneSpec::default()
    };
    let spec_custom = ScreentoneSpec {
        pattern_type: PatternType::CustomTexture,
        spacing_x: 16.0,
        spacing_y: 16.0,
        density: 0.72,
        custom_pattern_id: Some(pat_id.clone()),
        custom_texture_scale: 1.75,
        reflect_layer_opacity: true,
        mode: ScreentoneMode::Sheet,
        use_source_luminance: false,
        backend: ScreentoneBackend::Gpu,
        ..ScreentoneSpec::default()
    };

    for (name, spec, origin) in [
        ("Euclid-BlockC", spec_euclid.clone(), (64.0, 64.0)),
        ("Sand-BlockC", spec_sand.clone(), (128.0, 96.0)),
        ("CustomTex-BlockC", spec_custom.clone(), (32.5, 48.25)),
    ] {
        let tone = ScreentoneLayer::new(spec, origin, Surface::new(PixelFormat::RGBA8));
        let mut layer = Layer::new(name, LayerContent::Screentone(tone));
        layer.opacity = 0.6;
        doc.layers.push(layer);
    }

    let bytes = save_to_bytes(&doc, &SaveOptions::default()).expect("save .pcraft");
    let loaded = load_from_bytes(&bytes).expect("load .pcraft");

    let check = |name: &str, expected: &ScreentoneSpec| {
        let layer = loaded.layers.iter().find(|l| l.name == name).unwrap_or_else(|| panic!("layer {name} missing"));
        match &layer.content {
            LayerContent::Screentone(st) => assert_eq!(&st.spec, expected, "ScreentoneSpec mismatch for {name}"),
            other => panic!("expected LayerContent::Screentone for {name}, got {:?}", std::mem::discriminant(other)),
        }
    };

    check("Euclid-BlockC", &spec_euclid);
    check("Sand-BlockC", &spec_sand);
    check("CustomTex-BlockC", &spec_custom);
    assert!(
        photocraft_doc::pattern::find(&loaded.patterns, &pat_id, "CrossTile").is_some(),
        "embedded CustomTexture pattern must survive .pcraft round-trip"
    );
}
