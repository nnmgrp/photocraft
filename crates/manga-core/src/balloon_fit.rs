//! Fitting a balloon's line of dialogue into its safe text area (`crates/engine/src/balloon_fit.rs`).
//!
//! The Manga Balloons panel needs to answer two questions without opening a window and without
//! touching a font file: *which type size makes this line fit the balloon*, and *does it fit at
//! all*. Both answers have to be deterministic, so the same document always reports the same
//! numbers and a test can pin them down.
//!
//! The measurement is the estimator the typesetter already uses for bubble wrapping
//! ([`crate::snow_knuth_plass`]): per-character width factors in em, times the type size. It is an
//! estimate, not a hinting-exact layout, which is exactly what a fit warning wants: it errs a
//! little on the wide side, so a line reported as fitting has room to spare.

use photocraft_doc::vector::{BalloonVoice, TextDirection};

use crate::snow_knuth_plass::{char_width_factor, measure_token_width};

/// The smallest type size the fit will go down to, in points. Below this a balloon's dialogue is
/// unreadable in print, so the fit reports an overflow instead of shrinking further.
pub const MIN_FIT_SIZE_PT: f32 = 4.0;

/// How much of the safe rectangle's height one line of type occupies, as a multiple of the size.
pub const LINE_HEIGHT_FACTOR: f32 = 1.25;

/// How much of the safe rectangle's width one vertical column occupies, as a multiple of the size.
pub const VERTICAL_COLUMN_WIDTH_FACTOR: f32 = 1.32;

/// Which axis caused the dialogue to overflow the safe text rectangle (`TZ 4.2.2`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowAxis {
    #[default]
    None,
    Width,
    Height,
    Both,
}

/// How the panel resolves a dialogue line against the balloon (`TZ 4.2.2`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BalloonFitMode {
    /// Enlarge the balloon body so the text fits at its preferred point size.
    #[default]
    GrowBalloon,
    /// Keep the balloon body fixed and shrink the type down to [`MIN_FIT_SIZE_PT`].
    ShrinkTextToShape,
}

/// The result of fitting one line of dialogue into a balloon.
#[derive(Clone, Debug, PartialEq)]
pub struct BalloonTextFit {
    /// The largest type size that fits, in points.
    pub size_pt: f32,
    /// The wrapped lines (or vertical columns) at that size (no trailing spaces).
    pub lines: Vec<String>,
    /// The width of the widest wrapped line (or total width of vertical columns), in px.
    pub widest_px: f32,
    /// The height the wrapped lines (or tallest vertical column) need, in px.
    pub needs_px: f32,
    /// `false` when even [`MIN_FIT_SIZE_PT`] overflows the safe rectangle.
    pub fits: bool,
    /// Which axis overflowed when `fits == false` (or `OverflowAxis::None` when `fits == true`).
    pub overflow_axis: OverflowAxis,
    /// Extra width needed in the safe rectangle (px) when `overflow_axis` includes `Width`.
    pub shortage_w_px: f32,
    /// Extra height needed in the safe rectangle (px) when `overflow_axis` includes `Height`.
    pub shortage_h_px: f32,
}

impl BalloonTextFit {
    /// The number of lines (or vertical columns) the fit produced.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
}

/// Coupled visual and typographic modifiers driven by a balloon's [`BalloonVoice`] and intensity
/// (`0.0..=1.0`, `TZ 4.2.3`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceModifiers {
    /// Multiplier applied to the base stroke width (`0.55..=1.85`).
    pub stroke_scale: f64,
    /// Multiplier applied to the preferred type size (`0.80..=1.40`).
    pub font_scale: f32,
    /// Additional fine hand-drawn contour tremor (`0.0..=0.50`).
    pub fine_noise_add: f64,
    /// Additional deep contour warp (`0.0..=0.55`).
    pub warp_noise_add: f64,
    /// Suggested dash duty when the voice is `Whisper` (`None` for solid voices).
    pub dashed_duty: Option<f64>,
}

/// Computes the coupled stroke, font, and noise modifiers for `voice` at `intensity` (`0..=1`).
pub fn voice_modifiers(voice: BalloonVoice, intensity: f64) -> VoiceModifiers {
    let k = if intensity.is_finite() { intensity.clamp(0.0, 1.0) } else { 0.5 };
    match voice {
        BalloonVoice::Neutral => VoiceModifiers { stroke_scale: 1.0, font_scale: 1.0, fine_noise_add: 0.0, warp_noise_add: 0.0, dashed_duty: None },
        BalloonVoice::Whisper => VoiceModifiers {
            stroke_scale: 1.0 - 0.38 * k,
            font_scale: (1.0 - 0.18 * k) as f32,
            fine_noise_add: 0.05 * k,
            warp_noise_add: 0.0,
            dashed_duty: Some((0.62 - 0.18 * k).clamp(0.35, 0.75)),
        },
        BalloonVoice::Shout => VoiceModifiers {
            stroke_scale: 1.0 + 0.65 * k,
            font_scale: (1.0 + 0.32 * k) as f32,
            fine_noise_add: 0.15 * k,
            warp_noise_add: 0.08 * k,
            dashed_duty: None,
        },
        BalloonVoice::Irritated => VoiceModifiers {
            stroke_scale: 1.0 + 0.30 * k,
            font_scale: (1.0 + 0.12 * k) as f32,
            fine_noise_add: 0.35 * k,
            warp_noise_add: 0.20 * k,
            dashed_duty: None,
        },
        BalloonVoice::Broadcast => {
            VoiceModifiers { stroke_scale: 1.0 + 0.15 * k, font_scale: 1.0, fine_noise_add: 0.0, warp_noise_add: 0.0, dashed_duty: None }
        }
        BalloonVoice::Eerie => VoiceModifiers {
            stroke_scale: 1.0 + 0.10 * k,
            font_scale: (1.0 - 0.06 * k) as f32,
            fine_noise_add: 0.20 * k,
            warp_noise_add: 0.45 * k,
            dashed_duty: None,
        },
    }
}

fn classify_overflow(widest: f32, needs: f32, max_width: f32, max_height: f32) -> (OverflowAxis, f32, f32) {
    let over_w = (widest - max_width).max(0.0);
    let over_h = (needs - max_height).max(0.0);
    let w_bad = over_w > max_width * 0.0001;
    let h_bad = over_h > max_height * 0.0001;
    let axis = match (w_bad, h_bad) {
        (false, false) => OverflowAxis::None,
        (true, false) => OverflowAxis::Width,
        (false, true) => OverflowAxis::Height,
        (true, true) => OverflowAxis::Both,
    };
    (axis, over_w, over_h)
}

/// The width and height one line of `text` needs at `size_px`, wrapping at `max_width`.
///
/// Words are kept whole: a line that does not fit is broken by the estimator's own hyphenation
/// only when a single word is wider than the balloon, which is what a typesetter does too.
fn wrap_at(text: &str, max_width: f32, size_px: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0.0f32;

    for word in text.split_whitespace() {
        let word_w = measure_token_width(word, size_px, 1.0);
        let space_w = char_width_factor(' ') * size_px;

        if word_w > max_width && max_width > 0.0 {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            let mut chunk_w = 0.0f32;
            for ch in word.chars() {
                let cw = char_width_factor(ch) * size_px;
                if chunk_w + cw > max_width && !chunk.is_empty() {
                    lines.push(std::mem::take(&mut chunk));
                    chunk_w = 0.0;
                }
                chunk.push(ch);
                chunk_w += cw;
            }
            current = chunk;
            current_w = chunk_w;
            continue;
        }

        let with_space = if current.is_empty() { word_w } else { current_w + space_w + word_w };
        if with_space > max_width && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
            current_w = word_w;
        } else {
            if !current.is_empty() {
                current.push(' ');
                current_w += space_w;
            }
            current.push_str(word);
            current_w += word_w;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Vertical column wrapping (`tategaki` / top-to-bottom columns stacked right-to-left, `TZ 4.2.1`).
///
/// Each character occupies one vertical cell of height `size_px * 1.05`; columns advance when a
/// column reaches `max_height` or encounters an explicit newline.
fn wrap_vertical_columns(text: &str, max_height: f32, size_px: f32) -> Vec<String> {
    let cell_h = (size_px * 1.05).max(1.0);
    let max_chars_per_col = ((max_height / cell_h).floor() as usize).max(1);
    let mut cols: Vec<String> = Vec::new();

    for paragraph in text.lines() {
        let trimmed = paragraph.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut cur_col = String::new();
        let mut cur_count = 0usize;
        for ch in trimmed.chars() {
            if cur_count >= max_chars_per_col && !cur_col.is_empty() {
                cols.push(std::mem::take(&mut cur_col));
                cur_count = 0;
            }
            // Skip leading spaces at the top of a new vertical column.
            if cur_count == 0 && ch.is_whitespace() {
                continue;
            }
            cur_col.push(ch);
            cur_count += 1;
        }
        if !cur_col.is_empty() {
            cols.push(cur_col);
        }
    }
    if cols.is_empty() {
        cols.push(String::new());
    }
    cols
}

/// The lines `text` wraps into at `size_px`, with the widest line and the height they need.
fn measure(text: &str, max_width: f32, max_height: f32, size_px: f32) -> (Vec<String>, f32, f32, bool) {
    let lines = wrap_at(text, max_width, size_px);
    let widest = lines.iter().map(|l| measure_token_width(l, size_px, 1.0)).fold(0.0f32, f32::max);
    let needs = lines.len() as f32 * size_px * LINE_HEIGHT_FACTOR;
    let fits = widest <= max_width * 1.0001 && needs <= max_height * 1.0001;
    (lines, widest, needs, fits)
}

/// Vertical column measurement (`TZ 4.2.1`): `widest` is total width across columns, `needs` is
/// the height of the tallest column.
fn measure_vertical(text: &str, max_width: f32, max_height: f32, size_px: f32) -> (Vec<String>, f32, f32, bool) {
    let cols = wrap_vertical_columns(text, max_height, size_px);
    let cell_h = size_px * 1.05;
    let tallest_chars = cols.iter().map(|c| c.chars().count()).max().unwrap_or(1);
    let needs_h = (tallest_chars as f32) * cell_h;
    let total_w = (cols.len() as f32) * size_px * VERTICAL_COLUMN_WIDTH_FACTOR;
    let fits = total_w <= max_width * 1.0001 && needs_h <= max_height * 1.0001;
    (cols, total_w, needs_h, fits)
}

/// Fits `text` into a safe rectangle of `width` × `height` px, starting from `preferred_pt` and
/// shrinking only as far as [`MIN_FIT_SIZE_PT`].
///
/// An empty line of dialogue is not an overflow: it reports the preferred size and fits.
pub fn fit_text(text: &str, width_px: f32, height_px: f32, preferred_pt: f32, dpi: f32) -> BalloonTextFit {
    fit_text_directed(text, width_px, height_px, preferred_pt, dpi, TextDirection::Horizontal)
}

/// Fits `text` into a safe rectangle in either horizontal or vertical (`tategaki`) direction.
pub fn fit_vertical_text(text: &str, width_px: f32, height_px: f32, preferred_pt: f32, dpi: f32) -> BalloonTextFit {
    fit_text_directed(text, width_px, height_px, preferred_pt, dpi, TextDirection::Vertical)
}

/// Unified directional text fitter (`Horizontal` or `Vertical`).
pub fn fit_text_directed(text: &str, width_px: f32, height_px: f32, preferred_pt: f32, dpi: f32, direction: TextDirection) -> BalloonTextFit {
    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let preferred = if preferred_pt.is_finite() && preferred_pt > 0.0 { preferred_pt } else { 12.0 };
    let width_px = if width_px.is_finite() { width_px.max(1.0) } else { 1.0 };
    let height_px = if height_px.is_finite() { height_px.max(1.0) } else { 1.0 };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        let size_px = preferred * dpi_scale;
        return BalloonTextFit {
            size_pt: preferred,
            lines: vec![String::new()],
            widest_px: 0.0,
            needs_px: size_px * LINE_HEIGHT_FACTOR,
            fits: true,
            overflow_axis: OverflowAxis::None,
            shortage_w_px: 0.0,
            shortage_h_px: 0.0,
        };
    }

    let probe_fn = |t: &str, size_px: f32| match direction {
        TextDirection::Horizontal => measure(t, width_px, height_px, size_px),
        TextDirection::Vertical => measure_vertical(t, width_px, height_px, size_px),
    };
    let size_for = |pt: f32| pt * dpi_scale;

    let (mut lines, mut widest, mut needs, fits) = probe_fn(trimmed, size_for(preferred));
    if fits {
        return BalloonTextFit {
            size_pt: preferred,
            lines,
            widest_px: widest,
            needs_px: needs,
            fits: true,
            overflow_axis: OverflowAxis::None,
            shortage_w_px: 0.0,
            shortage_h_px: 0.0,
        };
    }

    let (min_lines, min_widest, min_needs, min_fits) = probe_fn(trimmed, size_for(MIN_FIT_SIZE_PT));
    if !min_fits {
        let (overflow_axis, shortage_w_px, shortage_h_px) = classify_overflow(min_widest, min_needs, width_px, height_px);
        return BalloonTextFit {
            size_pt: MIN_FIT_SIZE_PT,
            lines: min_lines,
            widest_px: min_widest,
            needs_px: min_needs,
            fits: false,
            overflow_axis,
            shortage_w_px,
            shortage_h_px,
        };
    }

    let mut low = MIN_FIT_SIZE_PT;
    let mut high = preferred;
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        let probe = probe_fn(trimmed, size_for(mid));
        if probe.3 {
            low = mid;
            lines = probe.0;
            widest = probe.1;
            needs = probe.2;
        } else {
            high = mid;
        }
    }
    BalloonTextFit {
        size_pt: low,
        lines,
        widest_px: widest,
        needs_px: needs,
        fits: true,
        overflow_axis: OverflowAxis::None,
        shortage_w_px: 0.0,
        shortage_h_px: 0.0,
    }
}

/// The smallest balloon body that holds `text` at `size_pt`, assuming the balloon's safe area
/// keeps the given fraction of its body rectangle.
///
/// Returns `[w, h]` in px: the body size a caller should grow its rectangle to.
pub fn body_size_for_text(text: &str, size_pt: f32, dpi: f32, safe_fraction: f64) -> [f64; 2] {
    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let size_px = size_pt.max(0.1) * dpi_scale;
    let fraction = if safe_fraction.is_finite() { safe_fraction.clamp(0.15, 0.95) } else { 0.5 };

    let trimmed = text.trim();
    let total_w: f32 = if trimmed.is_empty() { 0.0 } else { measure_token_width(trimmed, size_px, 1.0) };
    let target_w = if total_w <= 0.0 { size_px * 6.0 } else { total_w.sqrt().max(size_px * 2.0) * (size_px * 2.0).sqrt() };
    let lines = if trimmed.is_empty() { 1 } else { wrap_at(trimmed, target_w.max(size_px * 2.0), size_px).len() };
    let widest = if trimmed.is_empty() {
        size_px * 2.0
    } else {
        wrap_at(trimmed, target_w.max(size_px * 2.0), size_px).iter().map(|l| measure_token_width(l, size_px, 1.0)).fold(0.0f32, f32::max)
    };

    let text_w = f64::from(widest.max(size_px * 2.0));
    let text_h = f64::from(lines as f32 * size_px * LINE_HEIGHT_FACTOR);
    [(text_w / fraction).max(24.0), (text_h / fraction).max(24.0)]
}

/// Aspect-aware body sizing (`TZ 4.2.2`):
/// * For `TextDirection::Horizontal`, short exclamations stay near-round while multi-clause lines
///   adopt a comfortable wider aspect (`1.25..=1.65`).
/// * For `TextDirection::Vertical`, balloons adopt the tall manga `tategaki` aspect (`w < h`).
pub fn body_size_for_text_oriented(text: &str, size_pt: f32, dpi: f32, safe_fraction: f64, direction: TextDirection) -> [f64; 2] {
    let base = body_size_for_text(text, size_pt, dpi, safe_fraction);
    let char_count = text.trim().chars().count();
    match direction {
        TextDirection::Horizontal => {
            if char_count <= 4 {
                let s = base[0].max(base[1]).max(32.0);
                [s, s]
            } else {
                [base[0] * 1.08, base[1] * 0.96]
            }
        }
        TextDirection::Vertical => {
            // Swap major axis so vertical columns sit inside a tall manga oval.
            let tall_h = base[0].max(base[1] * 1.35).max(48.0);
            let narrow_w = base[1].min(tall_h * 0.72).max(32.0);
            [narrow_w, tall_h]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_line_fits_at_the_preferred_size() {
        let fit = fit_text("Hi!", 200.0, 80.0, 12.0, 72.0);
        assert!(fit.fits, "{fit:?}");
        assert_eq!(fit.size_pt, 12.0, "no shrinking when the line already fits");
        assert_eq!(fit.line_count(), 1);
        assert_eq!(fit.overflow_axis, OverflowAxis::None);
    }

    #[test]
    fn an_empty_line_is_not_an_overflow() {
        let fit = fit_text("   ", 40.0, 20.0, 12.0, 72.0);
        assert!(fit.fits);
        assert_eq!(fit.size_pt, 12.0);
        assert_eq!(fit.lines, vec![String::new()]);
    }

    #[test]
    fn a_long_line_shrinks_the_type_until_it_fits() {
        let text = "Это довольно длинная реплика персонажа, которая не влезает в маленький баббл";
        let fit = fit_text(text, 160.0, 120.0, 24.0, 72.0);
        assert!(fit.fits, "{fit:?}");
        assert!(fit.size_pt < 24.0, "the type must shrink: {fit:?}");
        assert!(fit.size_pt >= MIN_FIT_SIZE_PT);
        let joined: String = fit.lines.join(" ");
        assert_eq!(joined.split_whitespace().collect::<Vec<_>>(), text.split_whitespace().collect::<Vec<_>>());
        assert!(fit.widest_px <= 160.0 * 1.0001, "{fit:?}");
        assert!(fit.needs_px <= 120.0 * 1.0001, "{fit:?}");
    }

    #[test]
    fn a_balloon_far_too_small_reports_an_overflow() {
        let text = "Эта реплика не влезает даже минимальным кеглем, потому что баббл крошечный";
        let fit = fit_text(text, 30.0, 18.0, 14.0, 72.0);
        assert!(!fit.fits, "{fit:?}");
        assert_eq!(fit.size_pt, MIN_FIT_SIZE_PT, "the overflow reports the smallest size it tried");
        assert_ne!(fit.overflow_axis, OverflowAxis::None);
        assert!(fit.shortage_h_px > 0.0 || fit.shortage_w_px > 0.0);
    }

    #[test]
    fn the_fit_is_deterministic() {
        let text = "Повторяемость важна: один документ — один результат";
        let a = fit_text(text, 180.0, 100.0, 18.0, 72.0);
        let b = fit_text(text, 180.0, 100.0, 18.0, 72.0);
        assert_eq!(a, b);
    }

    #[test]
    fn a_single_word_wider_than_the_balloon_is_broken_by_character() {
        let fit = fit_text("Невероятнодлинноеслово", 40.0, 200.0, 12.0, 72.0);
        assert!(fit.line_count() > 1, "the word is split: {fit:?}");
        for line in &fit.lines {
            assert!(measure_token_width(line, fit.size_pt, 1.0) <= 40.0 * 1.0001, "{line}");
        }
    }

    #[test]
    fn the_body_grows_with_more_text() {
        let short = body_size_for_text("Да!", 14.0, 72.0, 0.5);
        let long = body_size_for_text("Гораздо более длинная реплика персонажа для проверки роста", 14.0, 72.0, 0.5);
        assert!(long[0] >= short[0] && long[1] >= short[1], "short={short:?}, long={long:?}");
        assert!(long[0] >= 24.0 && long[1] >= 24.0, "never below the minimum balloon size");
    }

    #[test]
    fn vertical_text_fit_wraps_columns_and_grows_tall_aspect() {
        let text = "Вертикальный набор для манги";
        let v_fit = fit_vertical_text(text, 90.0, 180.0, 14.0, 72.0);
        assert!(v_fit.fits, "{v_fit:?}");
        assert!(v_fit.line_count() >= 2, "vertical text splits into multiple columns: {v_fit:?}");

        let [vw, vh] = body_size_for_text_oriented(text, 14.0, 72.0, 0.65, TextDirection::Vertical);
        assert!(vh > vw, "vertical manga balloon should be taller than wide: w={vw}, h={vh}");
    }

    #[test]
    fn voice_modifiers_scale_stroke_and_font_monotonically() {
        let whisper = voice_modifiers(BalloonVoice::Whisper, 1.0);
        let neutral = voice_modifiers(BalloonVoice::Neutral, 1.0);
        let shout = voice_modifiers(BalloonVoice::Shout, 1.0);
        assert!(whisper.stroke_scale < neutral.stroke_scale);
        assert!(shout.stroke_scale > neutral.stroke_scale);
        assert!(shout.font_scale > whisper.font_scale);
        assert!(whisper.dashed_duty.is_some());
    }
}
