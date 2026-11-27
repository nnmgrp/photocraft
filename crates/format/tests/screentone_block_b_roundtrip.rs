//! Wiring Gate integration test: `.pcraft` Save/Load bit-exact round-trip for
//! `LayerContent::Screentone` with Block B fields (`PatternType::FocusLines`, `line_wobble`,
//! `line_wave_freq`, `line_dash`, `line_progressive`, `focus_inner_radius`, `scrape_angle`).

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, PatternType, ScreentoneBackend, ScreentoneLayer, ScreentoneMode, ScreentoneSpec};
use photocraft_format::{SaveOptions, load_from_bytes, save_to_bytes};
use photocraft_geom::Size;
use photocraft_raster::Surface;

#[test]
fn pcraft_roundtrip_preserves_screentone_block_b_focus_lines_and_fields() {
    let mut doc = Document::new("block_b_roundtrip", Size::new(512, 512), ColorMode::Rgb, SampleType::U8);

    let spec_focus = ScreentoneSpec {
        pattern_type: PatternType::FocusLines,
        spacing_x: 11.5,
        spacing_y: 14.0,
        line_width: 3.25,
        angle: 17.5,
        density: 0.85,
        mode: ScreentoneMode::Sheet,
        use_source_luminance: false,
        backend: ScreentoneBackend::Gpu,
        jitter_pos: 24.0,
        jitter_size: 38.0,
        line_wobble: 0.42,
        line_wave_freq: 2.75,
        line_dash: 0.35,
        line_progressive: true,
        focus_inner_radius: 135.5,
        scrape_strength: 0.55,
        scrape_angle: -33.5,
        seed: 7331,
        ..ScreentoneSpec::default()
    };
    let origin = (180.25, 310.75);
    let tone = ScreentoneLayer::new(spec_focus.clone(), origin, Surface::new(PixelFormat::RGBA8));
    doc.layers.push(Layer::new("Focus-BlockB", LayerContent::Screentone(tone)));

    let bytes = save_to_bytes(&doc, &SaveOptions::default()).expect("save .pcraft");
    let loaded = load_from_bytes(&bytes).expect("load .pcraft");

    let layer = loaded.layers.iter().find(|l| l.name == "Focus-BlockB").expect("focus layer present");
    match &layer.content {
        LayerContent::Screentone(st) => {
            assert_eq!(st.origin, origin, "origin mismatch");
            assert_eq!(st.spec, spec_focus, "ScreentoneSpec Block B round-trip mismatch");
            assert_eq!(st.spec.pattern_type, PatternType::FocusLines);
            assert_eq!(st.spec.line_wobble, 0.42);
            assert_eq!(st.spec.line_wave_freq, 2.75);
            assert_eq!(st.spec.line_dash, 0.35);
            assert!(st.spec.line_progressive);
            assert_eq!(st.spec.focus_inner_radius, 135.5);
            assert_eq!(st.spec.scrape_strength, 0.55);
            assert_eq!(st.spec.scrape_angle, -33.5);
        }
        other => panic!("expected LayerContent::Screentone, got {:?}", std::mem::discriminant(other)),
    }
}
