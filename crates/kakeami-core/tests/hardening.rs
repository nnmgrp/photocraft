//! Hardening & correctness suite (Phase 5):
//! 1. Bit-exact multithreaded tile stitching (`diff == 0 px`) across 3 heavy modes.
//! 2. Anti-panic stress test (~100 extreme configs, tiny rects, huge offset rects).
//! 3. Cooperative cancellation (`AtomicBool`).
//! 4. Bit-exact equivalence of 16-column cache-blocked EDT vs naive 2D EDT.

use kakeami_core::raster::{compute_pencil_edt, EDT_HALF_INF, EDT_INF};
use kakeami_core::{
    compute_tile_pad, render_region_tiled, render_tile_gray, KakeamiConfig, PreparedScene,
    RasterScratch, Rect,
};
use std::sync::atomic::AtomicBool;

fn load_cfg(file: &str) -> KakeamiConfig {
    let json = match file {
        "t11-nawa-swarm-dense-600.json" => r#"{"schemaVersion":3,"seed":104829,"preset":"linear-bt","activeStyle":"white-nawa-dense","width":256,"height":256,"tileSize":256,"layerCap":"auto","geometryMode":"tutorial","parquetWeave":65,"darkenCap":55,"gamma":1.2,"gradStart":5,"gradEnd":95,"lineSpacing":8.5,"strokeWidth":1.3,"clusterSize":110,"baseAngle":10,"minLines":4,"maxLines":9,"overlapDensity":115,"angleRandomness":26,"crossAngle":90,"scatterJitter":30,"nibTaper":50,"wobble":8,"brokenInk":0,"oneBitMode":true,"transparentBg":false,"invertInk":false,"seamless":true,"oneBitThreshold":165,"paperTooth":0,"supersample2x":false,"ribbonSamples":8,"taperExponent":1,"fanSpread":2.5,"clusterSlant":0.35,"patternFamily":"nawa-ami","sizeGrade":"M","finishMode":"clean","inkPooling":0,"grainStrength":55,"macroDotPx":3,"nawaToneStyle":"denseWhite","nawaRopeWidth":42,"nawaCoilRadius":62,"nawaStrands":6,"nawaTwistStep":6.5,"nawaTwistAngle":42,"nawaOutlinePad":0,"nawaCoilStyle":"skein","nawaJoinStyle":"smooth-60l","nawaTipStyle":"blunt","nawaGradMode":"width","nawaLpiPreset":"60L","nawaScale":1,"nawaCurliness":50,"nawaFlowBias":0,"nawaFlowAngle":0,"nawaWidthVar":0,"nawaVortexMode":"off","nawaVortexX":0.5,"nawaVortexY":0.5,"nawaVortexStrength":65,"nawaVortexRadius":0.38,"stippleDotShape":"round","stippleDotSize":2.3,"stippleContrast":55,"kakePatchStyle":"closed-4l","kakeGridDivs":4,"kakeOccludeMode":"card","kakeFadeMode":"stipple","kakeFadeThreshold":0.36}"#,
        "t18a-shibufude-nawa-1bit.json" => r#"{"schemaVersion":3,"seed":104829,"preset":"linear-bt","activeStyle":"pencil-nawa","width":512,"height":512,"tileSize":256,"layerCap":"auto","geometryMode":"tutorial","parquetWeave":65,"darkenCap":45,"gamma":1.2,"gradStart":5,"gradEnd":95,"lineSpacing":8.5,"strokeWidth":1.3,"clusterSize":110,"baseAngle":10,"minLines":4,"maxLines":9,"overlapDensity":115,"angleRandomness":26,"crossAngle":90,"scatterJitter":30,"nibTaper":50,"wobble":6,"brokenInk":0,"oneBitMode":true,"transparentBg":false,"invertInk":false,"seamless":true,"oneBitThreshold":165,"paperTooth":0,"supersample2x":false,"ribbonSamples":8,"taperExponent":1,"fanSpread":2.5,"clusterSlant":0.35,"patternFamily":"nawa-ami","sizeGrade":"M","finishMode":"pencil","inkPooling":0,"grainStrength":55,"macroDotPx":3,"nawaToneStyle":"normal","nawaRopeWidth":42,"nawaCoilRadius":62,"nawaStrands":6,"nawaTwistStep":6.5,"nawaTwistAngle":42,"nawaOutlinePad":0,"nawaCoilStyle":"skein","nawaJoinStyle":"smooth-60l","nawaTipStyle":"blunt","nawaGradMode":"width","nawaLpiPreset":"60L","nawaScale":1,"nawaCurliness":50,"nawaFlowBias":0,"nawaFlowAngle":0,"nawaWidthVar":0,"nawaVortexMode":"off","nawaVortexX":0.5,"nawaVortexY":0.5,"nawaVortexStrength":65,"nawaVortexRadius":0.38,"stippleDotShape":"round","stippleDotSize":2.3,"stippleContrast":55,"kakePatchStyle":"closed-4l","kakeGridDivs":4,"kakeOccludeMode":"card","kakeFadeMode":"stipple","kakeFadeThreshold":0.36}"#,
        "t18b-shibufude-nawa-8bit.json" => r#"{"schemaVersion":3,"seed":104829,"preset":"linear-bt","activeStyle":"pencil-nawa","width":512,"height":512,"tileSize":256,"layerCap":"auto","geometryMode":"tutorial","parquetWeave":65,"darkenCap":45,"gamma":1.2,"gradStart":5,"gradEnd":95,"lineSpacing":8.5,"strokeWidth":1.3,"clusterSize":110,"baseAngle":10,"minLines":4,"maxLines":9,"overlapDensity":115,"angleRandomness":26,"crossAngle":90,"scatterJitter":30,"nibTaper":50,"wobble":6,"brokenInk":0,"oneBitMode":false,"transparentBg":false,"invertInk":false,"seamless":true,"oneBitThreshold":165,"paperTooth":0,"supersample2x":false,"ribbonSamples":8,"taperExponent":1,"fanSpread":2.5,"clusterSlant":0.35,"patternFamily":"nawa-ami","sizeGrade":"M","finishMode":"pencil","inkPooling":0,"grainStrength":55,"macroDotPx":3,"nawaToneStyle":"normal","nawaRopeWidth":42,"nawaCoilRadius":62,"nawaStrands":6,"nawaTwistStep":6.5,"nawaTwistAngle":42,"nawaOutlinePad":0,"nawaCoilStyle":"skein","nawaJoinStyle":"smooth-60l","nawaTipStyle":"blunt","nawaGradMode":"width","nawaLpiPreset":"60L","nawaScale":1,"nawaCurliness":50,"nawaFlowBias":0,"nawaFlowAngle":0,"nawaWidthVar":0,"nawaVortexMode":"off","nawaVortexX":0.5,"nawaVortexY":0.5,"nawaVortexStrength":65,"nawaVortexRadius":0.38,"stippleDotShape":"round","stippleDotSize":2.3,"stippleContrast":55,"kakePatchStyle":"closed-4l","kakeGridDivs":4,"kakeOccludeMode":"card","kakeFadeMode":"stipple","kakeFadeThreshold":0.36}"#,
        "t18c-shibufude-kakev8-pencil.json" => r#"{"schemaVersion":3,"seed":104829,"preset":"linear-bt","activeStyle":"kake-v8-gaussian-rough","width":1024,"height":1024,"tileSize":512,"layerCap":"auto","geometryMode":"tutorial","parquetWeave":65,"darkenCap":100,"gamma":1,"gradStart":6,"gradEnd":96,"lineSpacing":8.5,"strokeWidth":1.1,"clusterSize":110,"baseAngle":10,"minLines":4,"maxLines":9,"overlapDensity":90,"angleRandomness":75,"crossAngle":90,"scatterJitter":30,"nibTaper":50,"wobble":16,"brokenInk":0,"oneBitMode":true,"transparentBg":false,"invertInk":false,"seamless":false,"oneBitThreshold":165,"paperTooth":0,"supersample2x":false,"ribbonSamples":8,"taperExponent":1,"fanSpread":2.5,"clusterSlant":0.35,"patternFamily":"kakeami-v8","sizeGrade":"M","finishMode":"pencil","inkPooling":0,"grainStrength":55,"macroDotPx":3,"nawaToneStyle":"normal","nawaRopeWidth":42,"nawaCoilRadius":62,"nawaStrands":6,"nawaTwistStep":6.5,"nawaTwistAngle":42,"nawaOutlinePad":0,"nawaCoilStyle":"skein","nawaJoinStyle":"smooth-60l","nawaTipStyle":"blunt","nawaGradMode":"width","nawaLpiPreset":"60L","nawaScale":2,"nawaCurliness":50,"nawaFlowBias":0,"nawaFlowAngle":0,"nawaWidthVar":0,"nawaVortexMode":"off","nawaVortexX":0.5,"nawaVortexY":0.5,"nawaVortexStrength":65,"nawaVortexRadius":0.38,"stippleDotShape":"round","stippleDotSize":4.4,"stippleContrast":55,"kakePatchStyle":"open","kakeGridDivs":4,"kakeOccludeMode":"transparent","kakeFadeMode":"gaussian","kakeFadeThreshold":0.4}"#,
        other => panic!("unknown hardening fixture: {other}"),
    };
    serde_json::from_str::<KakeamiConfig>(json).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// Render `region` (512×512) once as a single call, then as 4 tiles (256×256) in reverse
/// order across `std::thread::scope` sharing `&PreparedScene`, and assert `0` differing pixels.
fn assert_mt_stitch_bit_exact(label: &str, cfg: &KakeamiConfig, x0: f64, y0: f64) {
    let scene = PreparedScene::prepare(cfg);
    let full_rect = Rect {
        x: x0,
        y: y0,
        w: 512.0,
        h: 512.0,
    };
    let mut scratch_full = RasterScratch::new();
    let full = render_tile_gray(&scene, cfg, full_rect, &mut scratch_full);

    // Reverse tile order: BR, BL, TR, TL
    let tile_coords = [
        (256usize, 256usize),
        (0usize, 256usize),
        (256usize, 0usize),
        (0usize, 0usize),
    ];

    let tiles: Vec<(usize, usize, Vec<u8>)> = std::thread::scope(|s| {
        let handles: Vec<_> = tile_coords
            .iter()
            .map(|&(tx, ty)| {
                let scene_ref = &scene;
                s.spawn(move || {
                    let mut sc = RasterScratch::new();
                    let buf = render_tile_gray(
                        scene_ref,
                        cfg,
                        Rect {
                            x: x0 + tx as f64,
                            y: y0 + ty as f64,
                            w: 256.0,
                            h: 256.0,
                        },
                        &mut sc,
                    );
                    (tx, ty, buf)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut stitched = vec![0u8; 512 * 512];
    for (tx, ty, buf) in tiles {
        for row in 0..256 {
            let dst_off = (ty + row) * 512 + tx;
            let src_off = row * 256;
            stitched[dst_off..dst_off + 256].copy_from_slice(&buf[src_off..src_off + 256]);
        }
    }

    let diff_count = full
        .iter()
        .zip(stitched.iter())
        .filter(|(&a, &b)| a != b)
        .count();
    assert_eq!(
        diff_count, 0,
        "{label}: multithreaded 4x256² stitch differed from single 512² by {diff_count} px (pad={})",
        compute_tile_pad(cfg)
    );
    eprintln!(
        "  stitch ok {:<32} pad={:<2} diff=0/262144 px",
        label,
        compute_tile_pad(cfg)
    );
}

#[test]
fn multithreaded_tile_stitching_is_bit_exact() {
    // 1. nawa-ami Dense White Swarm (t11 scaled to 512x512 canvas)
    let mut cfg_t11 = load_cfg("t11-nawa-swarm-dense-600.json");
    cfg_t11.width = 512;
    cfg_t11.height = 512;
    assert_mt_stitch_bit_exact("t11-nawa-swarm-dense-512", &cfg_t11, 0.0, 0.0);

    // 2. nawa-ami + Shibufude Pencil (1-bit t18a & 8-bit AA t18b)
    let cfg_t18a = load_cfg("t18a-shibufude-nawa-1bit.json");
    assert_mt_stitch_bit_exact("t18a-shibufude-nawa-1bit", &cfg_t18a, 0.0, 0.0);

    let cfg_t18b = load_cfg("t18b-shibufude-nawa-8bit.json");
    assert_mt_stitch_bit_exact("t18b-shibufude-nawa-8bit", &cfg_t18b, 0.0, 0.0);

    // 3. kakeami-v8 open + transparent + gaussian + Pencil @1200 DPI (fadeGridBuf + pad=9)
    let cfg_t18c = load_cfg("t18c-shibufude-kakev8-pencil.json");
    assert_eq!(compute_tile_pad(&cfg_t18c), 9, "t18c expected pad=9");
    assert_mt_stitch_bit_exact("t18c-kakev8-pencil-1200dpi", &cfg_t18c, 256.0, 256.0);
}

#[test]
fn anti_panic_stress_100_edge_configs() {
    let families = ["kakeami-cluster", "kakeami-4", "kakeami-v8", "nawa-ami"];
    let finishes = ["clean", "sugomi", "pencil", "macroDot"];
    let presets = [
        "linear-bt",
        "linear-tb",
        "radial",
        "vignette",
        "diagonal",
        "ramp-9",
        "ramp-white",
        "isolated-5",
        "bokeh",
    ];
    let nawa_tones = ["normal", "paleWhite", "denseWhite"];
    let nawa_coils = ["skein", "arcs"];
    let nawa_joins = ["smooth-60l", "crossed", "open"];
    let nawa_tips = ["blunt", "tapered", "asymmetric"];
    let nawa_grads = ["width", "stipple"];
    let nawa_vortices = ["off", "vortex", "attract", "repel"];
    let dot_shapes = ["round", "chain", "pixel1"];
    let kake_styles = ["closed-4l", "closed-2l", "open", "continuous"];
    let kake_occ = ["card", "dark-blend", "transparent"];
    let kake_fades = ["stipple", "gaussian", "layers"];

    let mut cases: Vec<(String, KakeamiConfig, Rect, bool)> = Vec::new();

    // 1. Systematic sweep across all enum combinations and extreme numeric bounds (~96 cases)
    let mut idx = 0usize;
    for &fam in &families {
        for &fin in &finishes {
            for variant in 0..6usize {
                let mut cfg = KakeamiConfig {
                    width: 256,
                    height: 256,
                    seed: 100_000 + (idx as u32) * 7919,
                    pattern_family: fam.into(),
                    finish_mode: fin.into(),
                    preset: presets[(idx + variant) % presets.len()].into(),
                    nawa_tone_style: nawa_tones[variant % nawa_tones.len()].into(),
                    nawa_coil_style: nawa_coils[(idx / 2) % nawa_coils.len()].into(),
                    nawa_join_style: nawa_joins[idx % nawa_joins.len()].into(),
                    nawa_tip_style: nawa_tips[variant % nawa_tips.len()].into(),
                    nawa_grad_mode: nawa_grads[variant % nawa_grads.len()].into(),
                    nawa_vortex_mode: nawa_vortices[(idx + variant) % nawa_vortices.len()].into(),
                    stipple_dot_shape: dot_shapes[(idx + variant) % dot_shapes.len()].into(),
                    kake_patch_style: kake_styles[(idx + variant) % kake_styles.len()].into(),
                    kake_occlude_mode: kake_occ[(idx + variant) % kake_occ.len()].into(),
                    kake_fade_mode: kake_fades[(idx + variant) % kake_fades.len()].into(),
                    ..Default::default()
                };

                match variant {
                    0 => {
                        // Minimum extremes
                        cfg.stroke_width = 0.05;
                        cfg.overlap_density = 0;
                        cfg.nawa_scale = 0.25;
                        cfg.darken_cap = 0;
                        cfg.kake_fade_threshold = 0.05;
                        cfg.one_bit_mode = true;
                        cfg.seamless = false;
                    }
                    1 => {
                        // Maximum extremes
                        cfg.stroke_width = 12.0;
                        cfg.overlap_density = 250;
                        cfg.nawa_scale = 8.0;
                        cfg.darken_cap = 100;
                        cfg.kake_fade_threshold = 0.95;
                        cfg.one_bit_mode = false;
                        cfg.seamless = true;
                        cfg.ink_pooling = 100.0;
                        cfg.paper_tooth = 100;
                    }
                    2 => {
                        // Tiny non-power-of-two rect 17x19 with odd offset
                        cfg.darken_cap = 55;
                        cfg.nawa_scale = 2.0;
                        cfg.nawa_width_var = 100.0;
                        cfg.nawa_flow_bias = 100.0;
                        cfg.nawa_curliness = 100.0;
                        cfg.one_bit_mode = true;
                        cfg.invert_ink = true;
                    }
                    3 => {
                        // Supersample2x + 8-bit + seamless
                        cfg.darken_cap = 75;
                        cfg.supersample2x = true;
                        cfg.one_bit_mode = false;
                        cfg.seamless = true;
                        cfg.nawa_outline_pad = 3.0;
                        cfg.broken_ink = 100;
                        cfg.wobble = 100;
                    }
                    4 => {
                        // Thin strokes + low fade threshold + high contrast
                        cfg.stroke_width = 0.2;
                        cfg.darken_cap = 35;
                        cfg.kake_fade_threshold = 0.05;
                        cfg.stipple_dot_size = 0.8;
                        cfg.stipple_contrast = 100.0;
                        cfg.nawa_strands = 2;
                        cfg.nawa_rope_width = 16.0;
                    }
                    _ => {
                        // Thick strokes + max strands + negative vortex
                        cfg.stroke_width = 4.5;
                        cfg.darken_cap = 90;
                        cfg.nawa_strands = 10;
                        cfg.nawa_vortex_strength = -100.0;
                        cfg.nawa_vortex_radius = 0.15;
                        cfg.grain_strength = 100.0;
                    }
                }

                let rect = if variant == 2 {
                    Rect { x: 13.0, y: 17.0, w: 17.0, h: 19.0 }
                } else {
                    Rect { x: 0.0, y: 0.0, w: 64.0, h: 64.0 }
                };
                cases.push((format!("{fam}/{fin}/v{variant}"), cfg, rect, true));
                idx += 1;
            }
        }
    }

    // 2. Giant offset document cases (origin = (16384, 24576), 17000x25000) across all families
    for &fam in &families {
        let cfg = KakeamiConfig {
            width: 17_000,
            height: 25_000,
            pattern_family: fam.into(),
            finish_mode: if fam == "kakeami-v8" || fam == "nawa-ami" {
                "pencil".into()
            } else {
                "sugomi".into()
            },
            preset: "linear-bt".into(),
            darken_cap: 65,
            nawa_scale: 2.0,
            kake_fade_mode: "gaussian".into(),
            ink_pooling: 25.0,
            // For nawa-ami on huge 17000x25000 canvas use arcs so skeleton stays lightweight
            nawa_coil_style: if fam == "nawa-ami" {
                "arcs".into()
            } else {
                "skein".into()
            },
            ..Default::default()
        };
        let rect = Rect {
            x: 16_384.0,
            y: 24_576.0,
            w: 64.0,
            h: 64.0,
        };
        cases.push((format!("giant-offset/{fam}"), cfg, rect, true));
    }

    assert!(cases.len() >= 100, "expected >= 100 stress cases, got {}", cases.len());
    let mut scratch = RasterScratch::new();
    for (name, cfg, rect, do_raster) in &cases {
        let scene = PreparedScene::prepare(cfg);
        let batch = scene.build_ribbons(cfg, *rect);

        // Zero NaN/Inf in vertices
        for (vi, &val) in batch.vertices.iter().enumerate() {
            assert!(
                val.is_finite(),
                "stress case {name}: non-finite vertex[{vi}] = {val}"
            );
        }
        // All meta ranges strictly in bounds
        for (mi, m) in batch.meta.iter().enumerate() {
            let end_pt = (m.offset as usize).saturating_add(m.point_count as usize);
            assert!(
                end_pt <= batch.point_count,
                "stress case {name}: meta[{mi}] end_pt={end_pt} > point_count={}",
                batch.point_count
            );
            assert!(
                end_pt * 4 <= batch.vertices.len(),
                "stress case {name}: meta[{mi}] exceeds vertices len"
            );
        }
        // All triangle indices strictly in bounds
        let max_vert_idx = (batch.point_count * 2) as u32;
        for (ii, &idx_v) in batch.indices.iter().enumerate() {
            assert!(
                idx_v < max_vert_idx,
                "stress case {name}: index[{ii}]={idx_v} >= {max_vert_idx}"
            );
        }

        if *do_raster {
            let gray = render_tile_gray(&scene, cfg, *rect, &mut scratch);
            assert_eq!(gray.len(), (rect.w as usize) * (rect.h as usize));
            if cfg.one_bit_mode {
                assert!(
                    gray.iter().all(|&b| b == 0 || b == 255),
                    "stress case {name}: 1-bit output contained non-binary pixel"
                );
            }
        }
    }
    eprintln!("  stress ok {} extreme configs (0 panics, 0 NaN/Inf, all indices in bounds)", cases.len());
}

#[test]
fn cooperative_cancel_returns_none() {
    let cfg = load_cfg("t11-nawa-swarm-dense-600.json");
    let scene = PreparedScene::prepare(&cfg);
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 256.0,
        h: 256.0,
    };

    let cancelled = AtomicBool::new(true);
    let res_none = render_region_tiled(&scene, &cfg, rect, 128, Some(&cancelled));
    assert!(res_none.is_none(), "render_region_tiled must return None immediately when cancelled");

    let active = AtomicBool::new(false);
    let res_tiled = render_region_tiled(&scene, &cfg, rect, 128, Some(&active))
        .expect("uncancelled render_region_tiled must return Some");
    let mut scratch = RasterScratch::new();
    let res_single = render_tile_gray(&scene, &cfg, rect, &mut scratch);
    assert_eq!(res_tiled, res_single, "render_region_tiled must match single-tile render bit-for-bit");
}

#[test]
fn edt_cache_blocked_matches_naive_2d() {
    // Non-multiple-of-16 dimensions to exercise partial block tails + interior-zero skipping
    let gw: usize = 73;
    let gh: usize = 61;
    let p: usize = 7;
    let tw: usize = 59; // p + tw = 66 <= gw
    let thr = 165.0;

    let mut gray = vec![255u8; gw * gh];
    // Synthetic control mask: thick block, thin diagonal stroke, isolated pixels, narrow 1-px gap
    for y in 0..gh {
        for x in 0..gw {
            let in_block = (12..=34).contains(&x) && (10..=28).contains(&y);
            let in_diag = ((x as isize - y as isize).abs() <= 2) && (5..=55).contains(&y);
            let in_gap = x == 23 && (10..=28).contains(&y); // 1-px white channel inside block
            let in_dot = (x == 58 && y == 14) || (x == 62 && y == 47) || (x == 9 && y == 52);
            let in_ring = {
                let dx = x as f64 - 48.0;
                let dy = y as f64 - 38.0;
                let r2 = dx * dx + dy * dy;
                (49.0..=100.0).contains(&r2)
            };
            if (in_block && !in_gap) || in_diag || in_dot || in_ring {
                gray[y * gw + x] = 0;
            }
        }
    }

    // Naive O((gw*gh)^2) exact 2D squared EDT reference
    let mut naive_in = vec![EDT_INF; gw * gh];
    let mut naive_out = vec![EDT_INF; gw * gh];
    let mut ink_pts: Vec<(isize, isize)> = Vec::new();
    let mut paper_pts: Vec<(isize, isize)> = Vec::new();
    for y in 0..gh {
        for x in 0..gw {
            if (gray[y * gw + x] as f64) < thr {
                ink_pts.push((x as isize, y as isize));
            } else {
                paper_pts.push((x as isize, y as isize));
            }
        }
    }
    for y in 0..gh {
        for x in p..(p + tw) {
            let xi = x as isize;
            let yi = y as isize;
            let idx = y * gw + x;
            if (gray[idx] as f64) < thr {
                naive_out[idx] = 0.0;
                let mut best = i64::MAX;
                for &(px, py) in &paper_pts {
                    let dx = (xi - px) as i64;
                    let dy = (yi - py) as i64;
                    let d2 = dx * dx + dy * dy;
                    if d2 < best {
                        best = d2;
                    }
                }
                naive_in[idx] = best as f32;
            } else {
                naive_in[idx] = 0.0;
                let mut best = i64::MAX;
                for &(ix, iy) in &ink_pts {
                    let dx = (xi - ix) as i64;
                    let dy = (yi - iy) as i64;
                    let d2 = dx * dx + dy * dy;
                    if d2 < best {
                        best = d2;
                    }
                }
                naive_out[idx] = best as f32;
            }
        }
    }

    // 1. Unbounded (max_in_r = 0, max_out_r = 0): bit-exact across ALL inner pixels
    let mut scratch = RasterScratch::new();
    let res_full = compute_pencil_edt(&mut scratch, &gray, gw, gh, p, tw, thr, 0, 0);
    assert!(res_full.has_ink && res_full.has_paper);
    for y in 0..gh {
        for x in p..(p + tw) {
            let idx = y * gw + x;
            assert_eq!(
                scratch.edt_d_in()[idx].to_bits(),
                naive_in[idx].to_bits(),
                "unbounded edt_d_in mismatch at ({x},{y}): got {} vs naive {}",
                scratch.edt_d_in()[idx],
                naive_in[idx]
            );
            assert_eq!(
                scratch.edt_d_out()[idx].to_bits(),
                naive_out[idx].to_bits(),
                "unbounded edt_d_out mismatch at ({x},{y}): got {} vs naive {}",
                scratch.edt_d_out()[idx],
                naive_out[idx]
            );
        }
    }

    // 2. Bounded narrow-band (max_in_r = 8, max_out_r = 5): bit-exact inside narrow band
    let lim_in = 8i32;
    let lim_out = 5i32;
    let res_bnd = compute_pencil_edt(&mut scratch, &gray, gw, gh, p, tw, thr, lim_in, lim_out);
    assert!(res_bnd.has_ink && res_bnd.has_paper);
    let lim_in_sq = (lim_in * lim_in) as f32;
    let lim_out_sq = (lim_out * lim_out) as f32;
    for y in 0..gh {
        for x in p..(p + tw) {
            let idx = y * gw + x;
            if naive_in[idx] <= lim_in_sq {
                assert_eq!(
                    scratch.edt_d_in()[idx].to_bits(),
                    naive_in[idx].to_bits(),
                    "bounded edt_d_in mismatch inside band at ({x},{y})"
                );
            } else {
                let got = scratch.edt_d_in()[idx];
                assert!(
                    got >= lim_in_sq || got >= EDT_HALF_INF,
                    "bounded edt_d_in underestimated outside band at ({x},{y}): got {got} < {lim_in_sq}"
                );
            }
            if naive_out[idx] <= lim_out_sq {
                assert_eq!(
                    scratch.edt_d_out()[idx].to_bits(),
                    naive_out[idx].to_bits(),
                    "bounded edt_d_out mismatch inside band at ({x},{y})"
                );
            } else {
                let got = scratch.edt_d_out()[idx];
                assert!(
                    got >= lim_out_sq || got >= EDT_HALF_INF,
                    "bounded edt_d_out underestimated outside band at ({x},{y}): got {got} < {lim_out_sq}"
                );
            }
        }
    }
    eprintln!("  edt ok    cache-blocked 16-col EDT == naive 2D EDT bit-for-bit ({tw}x{gh}, unbounded & bounded)");
}
