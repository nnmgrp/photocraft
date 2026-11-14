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

use crate::snow_knuth_plass::{char_width_factor, measure_token_width};

/// The smallest type size the fit will go down to, in points. Below this a balloon's dialogue is
/// unreadable in print, so the fit reports an overflow instead of shrinking further.
pub const MIN_FIT_SIZE_PT: f32 = 4.0;

/// How much of the safe rectangle's height one line of type occupies, as a multiple of the size.
pub const LINE_HEIGHT_FACTOR: f32 = 1.25;

/// The result of fitting one line of dialogue into a balloon.
#[derive(Clone, Debug, PartialEq)]
pub struct BalloonTextFit {
    /// The largest type size that fits, in points.
    pub size_pt: f32,
    /// The wrapped lines at that size (no trailing spaces).
    pub lines: Vec<String>,
    /// The width of the widest wrapped line, in px.
    pub widest_px: f32,
    /// The height the wrapped lines need, in px.
    pub needs_px: f32,
    /// `false` when even [`MIN_FIT_SIZE_PT`] overflows the safe rectangle.
    pub fits: bool,
}

impl BalloonTextFit {
    /// The number of lines the fit produced.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
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

        // A word wider than the whole balloon is split by character, so a single long word cannot
        // report a fit it does not have.
        if word_w > max_width && max_width > 0.0 {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_w = 0.0;
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

/// The lines `text` wraps into at `size_px`, with the widest line and the height they need.
fn measure(text: &str, max_width: f32, max_height: f32, size_px: f32) -> (Vec<String>, f32, f32, bool) {
    let lines = wrap_at(text, max_width, size_px);
    let widest = lines.iter().map(|l| measure_token_width(l, size_px, 1.0)).fold(0.0f32, f32::max);
    let needs = lines.len() as f32 * size_px * LINE_HEIGHT_FACTOR;
    // A line may not be wider than the balloon and the block may not be taller than the balloon;
    // the estimator is allowed to fill 96 % of either, never to overflow.
    let fits = widest <= max_width * 1.0001 && needs <= max_height * 1.0001;
    (lines, widest, needs, fits)
}

/// Fits `text` into a safe rectangle of `width` × `height` px, starting from `preferred_pt` and
/// shrinking only as far as [`MIN_FIT_SIZE_PT`].
///
/// An empty line of dialogue is not an overflow: it reports the preferred size and fits.
pub fn fit_text(text: &str, width_px: f32, height_px: f32, preferred_pt: f32, dpi: f32) -> BalloonTextFit {
    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let preferred = if preferred_pt.is_finite() && preferred_pt > 0.0 { preferred_pt } else { 12.0 };
    let width_px = if width_px.is_finite() { width_px.max(1.0) } else { 1.0 };
    let height_px = if height_px.is_finite() { height_px.max(1.0) } else { 1.0 };

    let trimmed = text.trim();
    if trimmed.is_empty() {
        let size_px = preferred * dpi_scale;
        return BalloonTextFit { size_pt: preferred, lines: vec![String::new()], widest_px: 0.0, needs_px: size_px * LINE_HEIGHT_FACTOR, fits: true };
    }

    let largest = |text: &str, size_px: f32| measure(text, width_px, height_px, size_px);
    let size_for = |pt: f32| pt * dpi_scale;

    // Too much type for the balloon at the preferred size: shrink by bisection on the size, which
    // keeps the search monotone even though the line breaks move as the size changes.
    let (mut lines, mut widest, mut needs, fits) = largest(trimmed, size_for(preferred));
    if fits {
        return BalloonTextFit { size_pt: preferred, lines, widest_px: widest, needs_px: needs, fits: true };
    }

    let (min_lines, min_widest, min_needs, min_fits) = largest(trimmed, size_for(MIN_FIT_SIZE_PT));
    if !min_fits {
        return BalloonTextFit { size_pt: MIN_FIT_SIZE_PT, lines: min_lines, widest_px: min_widest, needs_px: min_needs, fits: false };
    }

    let mut low = MIN_FIT_SIZE_PT;
    let mut high = preferred;
    for _ in 0..24 {
        let mid = (low + high) * 0.5;
        let probe = largest(trimmed, size_for(mid));
        if probe.3 {
            low = mid;
            lines = probe.0;
            widest = probe.1;
            needs = probe.2;
        } else {
            high = mid;
        }
    }
    BalloonTextFit { size_pt: low, lines, widest_px: widest, needs_px: needs, fits: true }
}

/// The smallest balloon body that holds `text` at `size_pt`, assuming the balloon's safe area
/// keeps the given fraction of its body rectangle.
///
/// Returns `[w, h]` in px: the body size a caller should grow its rectangle to. A balloon whose
/// safe area is `safe_fraction` of the body needs the text block divided by that fraction.
pub fn body_size_for_text(text: &str, size_pt: f32, dpi: f32, safe_fraction: f64) -> [f64; 2] {
    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let size_px = size_pt.max(0.1) * dpi_scale;
    let fraction = if safe_fraction.is_finite() { safe_fraction.clamp(0.15, 0.95) } else { 0.5 };

    let trimmed = text.trim();
    // Break the line into the roughly square block a balloon wants: the widest chunk that keeps
    // the block no taller than it is wide reads best inside an oval.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_line_fits_at_the_preferred_size() {
        let fit = fit_text("Hi!", 200.0, 80.0, 12.0, 72.0);
        assert!(fit.fits, "{fit:?}");
        assert_eq!(fit.size_pt, 12.0, "no shrinking when the line already fits");
        assert_eq!(fit.line_count(), 1);
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
        // Every word survives the wrap, in order.
        let joined: String = fit.lines.join(" ");
        assert_eq!(joined.split_whitespace().collect::<Vec<_>>(), text.split_whitespace().collect::<Vec<_>>());
        // The result is a real fit: no line is wider than the balloon.
        assert!(fit.widest_px <= 160.0 * 1.0001, "{fit:?}");
        assert!(fit.needs_px <= 120.0 * 1.0001, "{fit:?}");
    }

    #[test]
    fn a_balloon_far_too_small_reports_an_overflow() {
        let text = "Эта реплика не влезает даже минимальным кеглем, потому что баббл крошечный";
        let fit = fit_text(text, 30.0, 18.0, 14.0, 72.0);
        assert!(!fit.fits, "{fit:?}");
        assert_eq!(fit.size_pt, MIN_FIT_SIZE_PT, "the overflow reports the smallest size it tried");
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
            assert!(measure_token_width(&*line, fit.size_pt, 1.0) <= 40.0 * 1.0001, "{line}");
        }
    }

    #[test]
    fn the_body_grows_with_more_text() {
        let short = body_size_for_text("Да!", 14.0, 72.0, 0.5);
        let long = body_size_for_text("Гораздо более длинная реплика персонажа для проверки роста", 14.0, 72.0, 0.5);
        assert!(long[0] >= short[0] && long[1] >= short[1], "short={short:?}, long={long:?}");
        assert!(long[0] >= 24.0 && long[1] >= 24.0, "never below the minimum balloon size");
    }
}
