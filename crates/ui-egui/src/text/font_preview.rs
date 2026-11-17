//! Small font-menu samples, rendered by the same font database as document text.

use std::collections::VecDeque;
use std::sync::PoisonError;

use egui::{ColorImage, Rect, TextureHandle, TextureOptions, pos2, vec2};
use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::TextLayer;
use photocraft_engine::prefs::FontPreview;
use photocraft_geom::Affine;

const SAMPLE: &str = "AaBbCc";
/// Alef, lam, meem and ain: a face lacking any of them is not treated as an Arabic face.
const ARABIC_CORE: [char; 4] = ['\u{627}', '\u{644}', '\u{645}', '\u{639}'];
/// Short, recognisable, and joins letters, so it shows the face's shaping.
const ARABIC_SAMPLE: &str = "أبجد هوز";
/// The Latin and Arabic sample widths and the sample height at the menu's medium (18 pt).
const WIDTH: f32 = 100.0;
const ARABIC_WIDTH: f32 = 72.0;
const GAP: f32 = 6.0;
const HEIGHT: f32 = 24.0;
/// The medium point size the widths above are measured at.
const MEDIUM: f32 = 18.0;
const CACHE_LIMIT: usize = 128;
const CACHE_ID: &str = "font-menu-previews";

/// A width or height measured at 18 pt, scaled to the sample's point size.
fn scaled(v: f32, size: f32) -> f32 {
    (v * size / MEDIUM).round()
}

/// The Latin sample's `[width, height]` box for a point size: [`WIDTH`]×[`HEIGHT`] at 18 pt (the
/// menu's medium), proportional otherwise.
fn box_for(size: f32) -> [f32; 2] {
    [scaled(WIDTH, size), scaled(HEIGHT, size)]
}

/// The font menu's sample geometry for Preferences ▸ Type ▸ Font Preview Size: the sample's point
/// size and its Latin box; `None` when the preview is off.
pub(crate) fn sample(preview: FontPreview) -> Option<(f32, [f32; 2])> {
    let size = match preview {
        FontPreview::Off => return None,
        FontPreview::Small => 14.0,
        FontPreview::Medium => MEDIUM,
        FontPreview::Large => 24.0,
        FontPreview::ExtraLarge => 32.0,
        FontPreview::Huge => 40.0,
    };
    Some((size, box_for(size)))
}

/// Family, display scale, the shared font database's generation (so a family installed or
/// fetched (served fonts) after its first miss gets a sample once it is available) and the point
/// size. Not the served catalog's generation: a listed family has no sample until its fonts are
/// registered, which moves the database's generation, and keying on the catalog re-rendered
/// every sample whenever it changed.
type Key = (String, u32, u64, u32);
#[derive(Clone)]
struct Samples {
    latin: TextureHandle,
    arabic: Option<TextureHandle>,
}
#[derive(Clone, Default)]
struct Cache(VecDeque<(Key, Option<Samples>)>);

struct SampleImages {
    latin: ColorImage,
    arabic: Option<ColorImage>,
}

/// The Latin sample text, and the Arabic one for a face that has both scripts. `chars` is the
/// face's ascending charmap. Arabic-only and symbol faces show their own glyphs instead of the
/// Latin sample and get no second sample.
fn sample_texts(chars: &[char]) -> (String, Option<&'static str>) {
    let has = |c: char| chars.binary_search(&c).is_ok();
    if !SAMPLE.chars().all(has) {
        return (chars.iter().copied().filter(|c| !c.is_whitespace()).take(6).collect(), None);
    }
    let arabic = ARABIC_CORE.into_iter().chain(ARABIC_SAMPLE.chars().filter(|c| !c.is_whitespace())).all(has);
    (SAMPLE.into(), arabic.then_some(ARABIC_SAMPLE))
}

fn render(engine: &mut photocraft_text::TextEngine, family: &str, text: &str, slot: f32, scale: u32, size: f32) -> Option<ColorImage> {
    if text.is_empty() {
        return None;
    }
    let layer = TextLayer { text: text.into(), font_family: family.into(), size_pt: size * scale as f32, ..Default::default() };
    let layout = engine.layout(&layer, 72.0);
    let bounds = layout.bounds()?;
    let width = (slot as u32 * scale) as usize;
    let height = (scaled(HEIGHT, size) as u32 * scale) as usize;
    let transform = Affine::translate(f64::from(-bounds[0]), f64::from((height as f32 - (bounds[3] - bounds[1])) / 2.0 - bounds[1]));
    // Reject oversized ink before the rasterizer allocates a surface (malformed installed fonts).
    let ink = photocraft_text::render::ink_rect(&layout, &transform);
    if ink.is_empty() || ink.width() > width as u32 * 4 || ink.height() > height as u32 * 4 {
        return None;
    }
    let rendered = photocraft_text::render::rasterize(&layout, &transform, PixelFormat::new(ColorMode::Rgb, SampleType::U8, true), layer.antialias);
    let mut rgba = vec![[0u8; 4]; width * height];
    rendered.surface.read_rgba8_into(photocraft_geom::Rect::new(0, 0, width as i32, height as i32), &mut rgba);
    Some(ColorImage::from_rgba_unmultiplied([width, height], &rgba.into_iter().flat_map(|p| [255, 255, 255, p[3]]).collect::<Vec<_>>()))
}

fn images(engine: &mut photocraft_text::TextEngine, family: &str, scale: u32, size: f32) -> Option<SampleImages> {
    // Unknown/unreadable families keep their plain menu label, rather than a misleading fallback.
    if !engine.fonts.has_family(family) {
        return None;
    }
    let (latin, arabic) = sample_texts(&engine.fonts.charmap(family, 400, false));
    let latin = render(engine, family, &latin, scaled(WIDTH, size), scale, size)?;
    let arabic = arabic.and_then(|text| render(engine, family, text, scaled(ARABIC_WIDTH, size), scale, size));
    Some(SampleImages { latin, arabic })
}

fn key(ctx: &egui::Context, family: &str, size: f32) -> Key {
    let scale = ctx.pixels_per_point().ceil().clamp(1.0, 4.0) as u32;
    (family.to_owned(), scale, photocraft_text::fonts::shared_generation(), size as u32)
}

/// Width a row reserves for its samples: wider once a painted row showed the family covers
/// Arabic, so the label truncates before the samples. Never renders, so rows scrolled out of
/// view cost nothing.
pub(crate) fn slot_width(ctx: &egui::Context, family: &str, size: f32) -> f32 {
    let key = key(ctx, family, size);
    let arabic = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<Cache>(egui::Id::unique(CACHE_ID));
        cache.0.iter().any(|(k, samples)| k == &key && samples.as_ref().is_some_and(|s| s.arabic.is_some()))
    });
    if arabic { scaled(WIDTH, size) + GAP + scaled(ARABIC_WIDTH, size) } else { scaled(WIDTH, size) }
}

pub(crate) fn paint(ui: &egui::Ui, family: &str, row: Rect, size: f32) {
    let ctx = ui.ctx();
    let scale = ctx.pixels_per_point().ceil().clamp(1.0, 4.0) as u32;
    let key = key(ctx, family, size);
    let id = egui::Id::unique(CACHE_ID);
    let cached = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<Cache>(id);
        let index = cache.0.iter().position(|(k, _)| k == &key)?;
        let entry = cache.0.remove(index)?;
        let samples = entry.1.clone();
        cache.0.push_back(entry);
        Some(samples)
    });
    let samples = cached.unwrap_or_else(|| {
        let samples = images(&mut photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner), family, scale, size).map(|img| {
            let load = |image| ctx.load_texture(format!("font-sample-{family}"), image, TextureOptions::LINEAR);
            Samples { latin: load(img.latin), arabic: img.arabic.map(load) }
        });
        if samples.as_ref().is_some_and(|s| s.arabic.is_some()) {
            // The row reserved only the Latin width until now.
            ctx.request_repaint();
        }
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<Cache>(id);
            if cache.0.len() >= CACHE_LIMIT {
                cache.0.pop_front();
            }
            cache.0.push_back((key, samples.clone()));
        });
        samples
    });
    if let Some(samples) = samples {
        let tint = crate::theme::Tokens::get(ctx).text;
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let [w, h] = box_for(size);
        let latin = Rect::from_min_size(pos2(row.right() - w - 8.0, row.center().y - h / 2.0), vec2(w, h));
        ui.painter().image(samples.latin.id(), latin, uv, tint);
        if let Some(arabic) = samples.arabic {
            let aw = scaled(ARABIC_WIDTH, size);
            let rect = Rect::from_min_size(pos2(latin.left() - GAP - aw, latin.top()), vec2(aw, h));
            ui.painter().image(arabic.id(), rect, uv, tint);
        }
    }
}

/// The few lines of the user's own text the font menu's preview keeps rendered: family, text,
/// display scale, font generation, point size and width.
type TextKey = (String, String, u32, u64, u32, u32);
#[derive(Clone, Default)]
struct TextCache(VecDeque<(TextKey, Option<TextureHandle>)>);
const TEXT_CACHE_LIMIT: usize = 16;

/// `text` set in `family` at `size`, from the left of `rect` and vertically centred; what doesn't
/// fit is cut at the right. The font menu's preview of the user's own text.
pub(crate) fn paint_text(ui: &egui::Ui, family: &str, text: &str, rect: Rect, size: f32) {
    let ctx = ui.ctx();
    let scale = ctx.pixels_per_point().ceil().clamp(1.0, 4.0) as u32;
    let slot = rect.width().clamp(1.0, 4096.0);
    let key: TextKey = (family.to_owned(), text.to_owned(), scale, photocraft_text::fonts::shared_generation(), size as u32, slot as u32);
    let id = egui::Id::unique("font-menu-text-previews");
    let cached = ctx.data_mut(|d| d.get_temp_mut_or_default::<TextCache>(id).0.iter().find(|(k, _)| *k == key).map(|(_, t)| t.clone()));
    let texture = cached.unwrap_or_else(|| {
        let mut engine = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
        let image = engine.fonts.has_family(family).then(|| render(&mut engine, family, text, slot, scale, size)).flatten();
        drop(engine);
        let texture = image.map(|image| ctx.load_texture(format!("font-text-{family}"), image, TextureOptions::LINEAR));
        ctx.data_mut(|d| {
            let cache = d.get_temp_mut_or_default::<TextCache>(id);
            if cache.0.len() >= TEXT_CACHE_LIMIT {
                cache.0.pop_front();
            }
            cache.0.push_back((key, texture.clone()));
        });
        texture
    });
    if let Some(texture) = texture {
        let h = scaled(HEIGHT, size);
        let at = Rect::from_min_size(pos2(rect.left(), rect.center().y - h / 2.0), vec2(slot, h));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        ui.painter().image(texture.id(), at, uv, crate::theme::Tokens::get(ctx).text);
    }
}

#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) struct NameStyle<'a> {
    pub family: &'a str,
    pub text: &'a str,
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn render_scale(ctx: &egui::Context) -> f32 {
    ctx.pixels_per_point().clamp(1.0, 4.0)
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn scale_key(scale: f32) -> u32 {
    (scale * 100.0).round().clamp(100.0, 400.0) as u32
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn snap(ctx: &egui::Context, v: f32) -> f32 {
    let ppp = ctx.pixels_per_point();
    (v * ppp).round() / ppp
}

/// A style name rasterized with the style's own face, cached per family, text, UI size and
/// scale. `size` is the sample's point size in the UI.
#[cfg(any(not(target_arch = "wasm32"), test))]
#[derive(Clone)]
struct LabelSample {
    texture: TextureHandle,
    size: [f32; 2],
}

#[cfg(any(not(target_arch = "wasm32"), test))]
type LabelKey = (String, String, u32, u64, u32, bool, bool);

#[cfg(any(not(target_arch = "wasm32"), test))]
#[derive(Clone, Default)]
struct LabelCache(VecDeque<(LabelKey, Option<LabelSample>)>);

#[cfg(any(not(target_arch = "wasm32"), test))]
const LABEL_CACHE_ID: &str = "font-name-labels";

#[cfg(any(not(target_arch = "wasm32"), test))]
fn label_key(ctx: &egui::Context, style: &NameStyle<'_>) -> LabelKey {
    (
        style.family.to_owned(),
        style.text.to_owned(),
        scale_key(render_scale(ctx)),
        photocraft_text::fonts::shared_generation(),
        style.size as u32,
        style.bold,
        style.italic,
    )
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn render_label(engine: &mut photocraft_text::TextEngine, style: &NameStyle<'_>, scale: f32) -> Option<(ColorImage, [f32; 2])> {
    use photocraft_doc::text::{ParagraphRun, ParagraphStyle, TextAlign, TextShape};

    if style.text.trim().is_empty() || !engine.fonts.has_family(style.family) {
        return None;
    }
    let mut layer = TextLayer {
        text: style.text.into(),
        font_family: style.family.into(),
        size_pt: style.size * scale,
        runs: if style.bold || style.italic {
            vec![photocraft_doc::text::TextRun {
                len: style.text.len(),
                style: photocraft_doc::text::CharStyle {
                    font_family: style.family.into(),
                    size_pt: style.size * scale,
                    weight: if style.bold { 700 } else { 400 },
                    italic: style.italic,
                    faux_bold: style.bold,
                    faux_italic: style.italic,
                    ..Default::default()
                },
            }]
        } else {
            Vec::new()
        },
        ..Default::default()
    };
    let mut layout = engine.layout(&layer, 72.0);
    // Point text is always left-aligned. For a multi-line sample, measure the widest line
    // once, then re-lay it as centred box text so each line is centred like TypeR's cards.
    if style.text.contains('\n')
        && let Some(measured) = layout.bounds()
    {
        let width = (measured[2] - measured[0]).abs().max(1.0);
        let lines = style.text.lines().count().max(1) as f32;
        layer.shape = TextShape::Box { x: 0.0, y: 0.0, width: width + 1.0, height: (style.size * scale * lines * 2.0).max(1.0) };
        layer.paragraphs = vec![ParagraphRun { len: style.text.len(), style: ParagraphStyle { align: TextAlign::Center, ..Default::default() } }];
        layout = engine.layout(&layer, 72.0);
    }
    let bounds = layout.bounds()?;
    let width = ((bounds[2] - bounds[0]).abs().ceil() as usize).saturating_add(2);
    let height = ((bounds[3] - bounds[1]).abs().ceil() as usize).saturating_add(2);
    if width <= 2 || height <= 2 || width > 4096 || height > 512 {
        return None;
    }
    let transform = Affine::translate(f64::from(1.0 - bounds[0]), f64::from(1.0 - bounds[1]));
    let ink = photocraft_text::render::ink_rect(&layout, &transform);
    if ink.is_empty() {
        return None;
    }
    let rendered = photocraft_text::render::rasterize(&layout, &transform, PixelFormat::new(ColorMode::Rgb, SampleType::U8, true), layer.antialias);
    let mut rgba = vec![[0u8; 4]; width * height];
    rendered.surface.read_rgba8_into(photocraft_geom::Rect::new(0, 0, width as i32, height as i32), &mut rgba);
    let image = ColorImage::from_rgba_unmultiplied([width, height], &rgba.into_iter().flat_map(|p| [255, 255, 255, p[3]]).collect::<Vec<_>>());
    Some((image, [width as f32 / scale, height as f32 / scale]))
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn label_sample(ctx: &egui::Context, style: &NameStyle<'_>) -> Option<LabelSample> {
    let id = egui::Id::unique(LABEL_CACHE_ID);
    let key = label_key(ctx, style);
    let cached: Option<Option<LabelSample>> = ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<LabelCache>(id);
        let index = cache.0.iter().position(|(k, _)| k == &key)?;
        let entry = cache.0.remove(index)?;
        let sample = entry.1.clone();
        cache.0.push_back(entry);
        Some(sample)
    });
    if let Some(sample) = cached {
        return sample;
    }
    let scale = render_scale(ctx);
    let mut engine = photocraft_text::shared().lock().unwrap_or_else(PoisonError::into_inner);
    let sample = render_label(&mut engine, style, scale).map(|(image, logical)| {
        let name = format!("font-name-{}-{}-{}-{scale}-{}-{}", style.family, style.text, style.size, style.bold, style.italic);
        LabelSample { texture: ctx.load_texture(name, image, TextureOptions::LINEAR), size: logical }
    });
    drop(engine);
    ctx.data_mut(|d| {
        let cache = d.get_temp_mut_or_default::<LabelCache>(id);
        if cache.0.len() >= CACHE_LIMIT {
            cache.0.pop_front();
        }
        cache.0.push_back((key, sample.clone()));
    });
    sample
}

/// The on-screen size a style name needs in its own face, or `None` when the family is missing
/// (the caller then keeps the plain label).
#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) fn name_size(ctx: &egui::Context, style: &NameStyle<'_>) -> Option<[f32; 2]> {
    label_sample(ctx, style).map(|s| s.size)
}

/// Paint a style name in its own face inside `rect`, left-aligned and vertically centred.
#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) fn paint_name(ui: &egui::Ui, style: &NameStyle<'_>, rect: Rect, color: egui::Color32) {
    let Some(sample) = label_sample(ui.ctx(), style) else {
        let mut job = egui::text::LayoutJob::default();
        let format = egui::TextFormat { font_id: egui::FontId::proportional(style.size), color, italics: style.italic, ..Default::default() };
        job.append(style.text, 0.0, format);
        job.wrap.max_width = rect.width();
        let galley = ui.painter().layout_job(job);
        let y = snap(ui.ctx(), rect.center().y - galley.size().y / 2.0);
        let target = Rect::from_min_size(pos2(snap(ui.ctx(), rect.left()), y), galley.size());
        ui.painter().galley(target.min, galley, color);
        return;
    };
    let [w, h] = sample.size;
    let target = Rect::from_min_size(pos2(snap(ui.ctx(), rect.left()), snap(ui.ctx(), rect.center().y - h / 2.0)), vec2(w, h));
    ui.painter().image(sample.texture.id(), target, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), color);
}

/// Paint a multi-line sample centred in `rect`, scaled down to fit.
#[cfg(any(not(target_arch = "wasm32"), test))]
pub(crate) fn paint_centered(ui: &egui::Ui, family: &str, text: &str, size: f32, rect: Rect, color: egui::Color32) {
    let fallback = |ui: &egui::Ui| {
        let mut job = egui::text::LayoutJob::default();
        job.append(text, 0.0, egui::TextFormat { font_id: egui::FontId::proportional(11.0), color, ..Default::default() });
        job.wrap.max_width = rect.width().max(10.0);
        job.wrap.break_anywhere = false;
        let galley = ui.painter().layout_job(job);
        let pos = pos2((rect.center().x - galley.size().x * 0.5).round(), (rect.center().y - galley.size().y * 0.5).round());
        ui.painter().galley(pos, galley, color);
    };

    let style = NameStyle { family, text, size, bold: false, italic: false };
    let Some(sample) = label_sample(ui.ctx(), &style) else {
        fallback(ui);
        return;
    };
    let [w, h] = sample.size;
    if w <= 0.0 || h <= 0.0 || rect.width() <= 1.0 || rect.height() <= 1.0 {
        fallback(ui);
        return;
    }
    let scale = (rect.width() / w).min(rect.height() / h).min(1.0);
    let target = Rect::from_center_size(rect.center(), vec2(w * scale, h * scale));
    let painter = ui.painter().with_clip_rect(rect);
    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    painter.image(sample.texture.id(), target, uv, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(engine: &mut photocraft_text::TextEngine, family: &str, scale: u32) -> Option<ColorImage> {
        images(engine, family, scale, MEDIUM).map(|i| i.latin)
    }

    fn charmap(text: &str) -> Vec<char> {
        let mut chars: Vec<char> = text.chars().collect();
        chars.sort_unstable();
        chars
    }

    #[test]
    fn previews_use_the_requested_face_and_scale() {
        let mut engine = photocraft_text::TextEngine::new();
        let sans = image(&mut engine, "Inter", 1).expect("bundled sans");
        let mono = image(&mut engine, "JetBrains Mono", 1).expect("bundled mono");
        assert_eq!(sans.size, [100, 24]);
        assert!(sans.pixels.iter().any(|p| p.a() > 0));
        assert_ne!(sans.pixels, mono.pixels, "samples use different font outlines");
        assert_eq!(image(&mut engine, "Inter", 2).unwrap().size, [200, 48]);
        assert!(image(&mut engine, "Missing font", 1).is_none());
    }

    /// Preferences ▸ Type ▸ Font Preview Size: Small…Huge scale the sample, Off removes it.
    #[test]
    fn the_preview_size_preference_scales_the_sample() {
        assert_eq!(sample(FontPreview::Off), None);
        assert_eq!(sample(FontPreview::Medium), Some((18.0, [100.0, 24.0])));
        let Some((_, [w, h])) = sample(FontPreview::Huge) else { panic!("Huge previews") };
        assert!(w > 100.0 && h > 24.0, "Huge is bigger than Medium: {w}×{h}");
        assert!(sample(FontPreview::Small).unwrap().1[0] < sample(FontPreview::ExtraLarge).unwrap().1[0], "sizes are ordered");
        let mut engine = photocraft_text::TextEngine::new();
        let big = images(&mut engine, "Inter", 1, 32.0).unwrap().latin;
        assert_eq!(big.size, [178, 43], "the box follows the size");
    }

    #[test]
    fn latin_only_families_get_no_arabic_sample() {
        let mut engine = photocraft_text::TextEngine::new();
        for family in ["Inter", "JetBrains Mono"] {
            assert!(images(&mut engine, family, 1, MEDIUM).expect("bundled").arabic.is_none(), "{family}");
        }
        assert!(images(&mut engine, "", 1, MEDIUM).is_none());
        assert!(images(&mut engine, "Missing font", 1, MEDIUM).is_none());
    }

    #[test]
    fn only_faces_with_both_scripts_get_the_arabic_sample() {
        let latin = format!("{SAMPLE} ");
        let arabic = format!("{ARABIC_SAMPLE}{}", ARABIC_CORE.iter().collect::<String>());
        let (text, second) = sample_texts(&charmap(&format!("{latin}{arabic}")));
        assert_eq!((text.as_str(), second), (SAMPLE, Some(ARABIC_SAMPLE)));
        assert_eq!(sample_texts(&charmap(&latin)), (SAMPLE.into(), None));
        let (own_glyphs, none) = sample_texts(&charmap(&arabic));
        assert_eq!((own_glyphs.chars().count(), none), (6, None));
        assert!(own_glyphs.chars().all(|c| arabic.contains(c)));
        assert_eq!(sample_texts(&[]), (String::new(), None));
        for missing in ARABIC_CORE.into_iter().chain(ARABIC_SAMPLE.chars().filter(|c| !c.is_whitespace())) {
            let partial: String = format!("{latin}{arabic}").chars().filter(|&c| c != missing).collect();
            assert_eq!(sample_texts(&charmap(&partial)).1, None, "without U+{:04X}", missing as u32);
        }
    }

    #[test]
    fn a_system_arabic_face_renders_both_samples() {
        let mut engine = photocraft_text::TextEngine::new();
        engine.fonts.load_system_fonts();
        let candidates: Vec<String> = engine.fonts.families().into_iter().filter(|f| sample_texts(&engine.fonts.charmap(f, 400, false)).1.is_some()).collect();
        // A face can map the Arabic characters to blank glyphs (#5259): `render` then rightly gives
        // no sample, so every candidate is rendered and the first one with an Arabic sample is checked.
        let mut rendered = None;
        for family in &candidates {
            if let Some(arabic) = images(&mut engine, family, 1, MEDIUM).and_then(|i| i.arabic) {
                rendered.get_or_insert((family.clone(), arabic));
            }
        }
        let Some((family, arabic)) = rendered else {
            eprintln!("skipped: no installed family renders both Latin and Arabic ({} claim both)", candidates.len());
            return;
        };
        assert_eq!(arabic.size, [ARABIC_WIDTH as usize, HEIGHT as usize], "{family}");
        assert!(arabic.pixels.iter().any(|p| p.a() > 0), "{family}");
    }
}
