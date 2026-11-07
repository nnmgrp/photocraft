//! Алгоритм вёрстки строк Кнута—Пласса (Knuth–Plass line-breaking / Boxes & Glue)
//! и типографической подготовки текста (устранение «висячих» предлогов и союзов)
//! с оптимизацией под форму манга-баллона (овал / ромб).

/// Символ неразрывного пробела (Non-Breaking Space, NBSP)
pub const NBSP: char = '\u{00A0}';

/// Привязывает короткие предлоги, союзы и частицы неразрывным пробелом (NBSP),
/// устраняя «висячие» предлоги на концах строк и одинокие частицы в начале строк.
pub fn bind_orphan_prepositions(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }

    // Предлоги и союзы, которые привязываются к СЛЕДУЮЩЕМУ слову
    const FORWARD_BINDERS: &[&str] = &[
        // Однобуквенные и двухбуквенные предлоги/союзы (RU)
        "а", "в", "и", "к", "о", "с", "у", "я", "не", "ни", "на", "но", "по", "за", "из", "от", "до", "во", "со", "ко", "об", "под", "над", "при", "про", "для",
        "без", // English common prepositions & articles
        "a", "an", "the", "in", "on", "at", "to", "by", "or", "as", "if", "is", "of", "it", "no", "so", "my", "he", "we",
    ];

    // Частицы, которые привязываются к ПРЕДЫДУЩЕМУ слову
    const BACKWARD_BINDERS: &[&str] = &["же", "бы", "ли", "ж", "б", "ль", "то", "ка"];

    let mut result = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let ch = chars[i];

        // Проверяем пробельные разделители
        if ch == ' ' {
            // Заглядываем вперед: что за слово идет после пробела?
            let mut next_start = i + 1;
            while next_start < n && chars[next_start] == ' ' {
                next_start += 1;
            }

            if next_start < n {
                // Извлекаем следующее слово (буквенно-цифровую часть)
                let mut next_end = next_start;
                while next_end < n && (chars[next_end].is_alphanumeric() || chars[next_end] == '-') {
                    next_end += 1;
                }

                let next_word: String = chars[next_start..next_end].iter().collect();
                let next_lower = next_word.to_lowercase();

                // 1. Проверяем обратную привязку: если следующее слово - частица (же, бы, ли...)
                // то пробел перед ней заменяем на неразрывный пробел (NBSP)
                if BACKWARD_BINDERS.iter().any(|&b| b == next_lower) {
                    result.push(NBSP);
                    i += 1;
                    continue;
                }

                // 2. Проверяем прямую привязку: если следующее слово - предлог/союз,
                // то после него пробел будет заменен на NBSP.
            }

            result.push(ch);
            i += 1;
            continue;
        }

        // Если это начало слова: проверим, не является ли оно прямым предлогом/союзом
        if (i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == '«' || chars[i - 1] == '"' || chars[i - 1] == '(' || chars[i - 1] == '[')
            && (ch.is_alphanumeric() || ch == '-')
        {
            let mut word_end = i;
            while word_end < n && (chars[word_end].is_alphanumeric() || chars[word_end] == '-') {
                word_end += 1;
            }

            let word: String = chars[i..word_end].iter().collect();
            let word_lower = word.to_lowercase();

            // Проверяем, является ли слово предлогом/союзом
            let is_forward = FORWARD_BINDERS.iter().any(|&b| b == word_lower);

            for c in chars[i..word_end].iter() {
                result.push(*c);
            }
            i = word_end;

            // Если это предлог и дальше идет обычный пробел — заменяем его на неразрывный пробел!
            if is_forward && i < n && chars[i] == ' ' {
                result.push(NBSP);
                i += 1;
                // Пропускаем возможные лишние пробелы
                while i < n && chars[i] == ' ' {
                    i += 1;
                }
            }
            continue;
        }

        result.push(ch);
        i += 1;
    }

    result
}

/// Приблизительный коэффициент ширины символа относительно кегля шрифта (em)
pub fn char_width_factor(c: char) -> f32 {
    match c {
        ' ' | '\u{00A0}' => 0.28,
        '.' | ',' | ':' | ';' | '!' | '|' | '\'' | '`' | 'i' | 'l' | 'j' => 0.25,
        'f' | 'r' | 't' | '(' | ')' | '[' | ']' | '{' | '}' => 0.35,
        'm' | 'w' | 'M' | 'W' | '—' | '…' => 0.85,
        'Ж' | 'Ш' | 'Щ' | 'Ю' | 'Ф' => 0.90,
        'ж' | 'ш' | 'щ' | 'ю' | 'ф' => 0.75,
        c if c.is_ascii_uppercase() => 0.65,
        c if ('А'..='Я').contains(&c) || c == 'Ё' => 0.65,
        c if ('а'..='я').contains(&c) || c == 'ё' => 0.52,
        c if c.is_ascii_digit() => 0.55,
        c if c >= '\u{3000}' && c <= '\u{9FFF}' => 1.0, // CJK
        _ => 0.50,
    }
}

/// Оценивает визуальную ширину слова или токена в пикселях с учетом шрифта и масштаба
pub fn measure_token_width(token: &str, font_size_px: f32, h_scale: f32) -> f32 {
    let mut w = 0.0;
    for c in token.chars() {
        w += char_width_factor(c) * font_size_px * h_scale;
    }
    w
}

/// Оценивает величину оптической компенсации (выноса за границу строки) для знаков препинания на концах слов
pub fn punctuation_compensation_px(token: &str, font_size_px: f32, h_scale: f32) -> f32 {
    let mut comp = 0.0;
    if token.is_empty() {
        return 0.0;
    }
    if token.ends_with("...") || token.ends_with("…") {
        comp += 0.50 * font_size_px * h_scale;
    } else if let Some(last) = token.chars().last() {
        match last {
            '.' | ',' | ':' | ';' | '!' | '?' => {
                comp += 0.25 * font_size_px * h_scale;
            }
            '—' | '–' | '-' => {
                comp += 0.35 * font_size_px * h_scale;
            }
            '»' | ')' | ']' | '}' | '"' | '\'' => {
                comp += 0.25 * font_size_px * h_scale;
            }
            _ => {}
        }
    }
    comp
}

/// Солвер переносов строк Кнута—Пласса (Knuth–Plass / Boxes & Glue)
/// с оптимизацией распределения слов под форму овального или ромбовидного баббла манги.
pub fn knuth_plass_bubble_wrap(text: &str, bubble_width: f32, bubble_height: f32, font_size_pt: f32, dpi: f32, horizontal_scale_pct: f32) -> String {
    knuth_plass_bubble_wrap_ext(text, bubble_width, bubble_height, font_size_pt, dpi, horizontal_scale_pct, true)
}

/// Расширенная версия солвера Кнута—Пласса с поддержкой оптической компенсации знаков в строках (висячая пунктуация)
pub fn knuth_plass_bubble_wrap_ext(
    text: &str,
    bubble_width: f32,
    bubble_height: f32,
    font_size_pt: f32,
    dpi: f32,
    horizontal_scale_pct: f32,
    hanging_punctuation: bool,
) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() || bubble_width <= 10.0 {
        return text.to_string();
    }

    // Если текст уже содержит явные переносы строк, применяем висячку к каждой строке отдельно
    if trimmed.contains('\n') {
        let lines: Vec<String> = trimmed.split('\n').map(bind_orphan_prepositions).collect();
        return lines.join("\n");
    }

    // 1. Применяем «висячку» (NBSP для предлогов и союзов)
    let bound_text = bind_orphan_prepositions(trimmed);

    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let font_size_px = font_size_pt * dpi_scale;
    let h_scale = (horizontal_scale_pct / 100.0).clamp(0.1, 5.0);

    // Максимальная безопасная ширина строки внутри баббла (с учетом внутренних отступов)
    let max_safe_width = (bubble_width * 0.85).max(font_size_px * 2.0);

    // Токенизация: разделяем по обычным пробелам (токены с NBSP внутри остаются едиными блоками!)
    let tokens: Vec<&str> = bound_text.split(' ').filter(|s| !s.is_empty()).collect();
    if tokens.len() <= 1 {
        return bound_text;
    }

    let space_width = char_width_factor(' ') * font_size_px * h_scale;
    let token_widths: Vec<f32> = tokens.iter().map(|&t| measure_token_width(t, font_size_px, h_scale)).collect();
    let total_text_width: f32 = token_widths.iter().sum::<f32>() + (tokens.len() - 1) as f32 * space_width;

    // Если весь текст комфортно помещается в одну строку
    if total_text_width <= max_safe_width * 0.80 {
        return bound_text;
    }

    // Оцениваем диапазон количества строк для перебора
    let est_lines = (total_text_width / (max_safe_width * 0.72)).round() as usize;
    let min_lines = est_lines.max(2).saturating_sub(1).max(2);
    let max_lines = (est_lines + 2).min(tokens.len()).max(min_lines);

    let line_height_px = font_size_px * 1.25;
    let max_fitting_lines = if bubble_height > 10.0 { ((bubble_height * 0.92) / line_height_px).floor() as usize } else { usize::MAX };

    let mut best_demerits = f64::MAX;
    let mut best_break_indices: Vec<usize> = Vec::new();

    // Профиль целевой ширины для эллипса/ромба манга-баллона:
    // Центральные строки имеют максимальную ширину, верхние и нижние — более компактные.
    let target_width_for_line = |line_idx: usize, total_k: usize| -> f32 {
        if total_k <= 1 {
            return max_safe_width;
        }
        let t = (line_idx as f32 + 0.5) / total_k as f32; // 0.0 .. 1.0
        let y = (t * 2.0 - 1.0).clamp(-1.0, 1.0); // -1.0 .. +1.0
        // Профиль эллипса: W(y) = W_max * sqrt(1 - y^2)
        let ellipse_factor = (1.0 - y * y).max(0.0).sqrt();
        // Сглаженный профиль (минимум 60% ширины по краям, 95% в центре)
        max_safe_width * (0.60 + 0.35 * ellipse_factor)
    };

    // Перебираем количество строк k от min_lines до max_lines
    for k in min_lines..=max_lines {
        // Динамическое программирование: dp[i][line_num] = (min_demerits, prev_word_idx)
        let n = tokens.len();
        // dp[w][l] = (demerits, prev_w)
        let mut dp = vec![vec![(f64::MAX, 0usize); k + 1]; n + 1];
        dp[0][0] = (0.0, 0);

        for line_num in 1..=k {
            let target_w = target_width_for_line(line_num - 1, k);

            for j in 1..=n {
                // Ищем лучший предыдущий разрыв i
                let mut line_w = 0.0;
                for i in (0..j).rev() {
                    let w_tok = token_widths[i];
                    line_w = if i == j - 1 { w_tok } else { line_w + space_width + w_tok };

                    if dp[i][line_num - 1].0 == f64::MAX {
                        continue;
                    }

                    // Оптическая компенсация знаков в строках (вынос концевой пунктуации за границу строки)
                    let comp = if hanging_punctuation { punctuation_compensation_px(tokens[j - 1], font_size_px, h_scale) } else { 0.0 };
                    let effective_w = (line_w - comp).max(0.0);

                    // Штрафы Кнута—Пласса (Demerits):
                    // 1. Разница с целевой шириной строки
                    let diff = (effective_w - target_w).abs();
                    let relative_diff = diff / target_w;
                    let mut demerits = (relative_diff * 100.0) as f64 * (relative_diff * 100.0) as f64;

                    // 2. Жесткий штраф за превышение максимальной безопасной ширины баббла
                    if effective_w > max_safe_width {
                        let overflow = effective_w - max_safe_width;
                        demerits += (overflow * 500.0) as f64;
                    }

                    // 3. Штраф за слишком короткую строку из одного короткого слова (одиночка/висячка)
                    if j - i == 1 && tokens[i].chars().count() <= 2 {
                        demerits += 25000.0;
                    }

                    // 4. Штраф за перенос дефисного слова на конце строки
                    if tokens[j - 1].ends_with('-') || tokens[j - 1].ends_with('—') {
                        demerits += 3000.0;
                        // Дополнительный штраф за два дефисных переноса подряд на соседних строках
                        if i > 0 && (tokens[i - 1].ends_with('-') || tokens[i - 1].ends_with('—')) {
                            demerits += 12000.0;
                        }
                    }

                    let total_demerits = dp[i][line_num - 1].0 + demerits;
                    if total_demerits < dp[j][line_num].0 {
                        dp[j][line_num] = (total_demerits, i);
                    }
                }
            }
        }

        let mut cand_demerits = dp[n][k].0;
        if cand_demerits < f64::MAX && k > max_fitting_lines {
            let overflow = (k - max_fitting_lines) as f64;
            cand_demerits += overflow * 50000.0;
        }
        if cand_demerits < best_demerits {
            best_demerits = cand_demerits;
            // Восстанавливаем разрывы строк
            let mut breaks = Vec::new();
            let mut curr_w = n;
            let mut curr_l = k;
            while curr_l > 0 && curr_w > 0 {
                let prev_w = dp[curr_w][curr_l].1;
                breaks.push(prev_w);
                curr_w = prev_w;
                curr_l -= 1;
            }
            breaks.reverse();
            best_break_indices = breaks;
        }
    }

    if best_break_indices.is_empty() {
        return bound_text;
    }

    // Собираем итоговые строки
    let mut lines = Vec::new();
    for window in best_break_indices.windows(2) {
        let start = window[0];
        let end = window[1];
        lines.push(tokens[start..end].join(" "));
    }
    if let Some(&last_start) = best_break_indices.last() {
        if last_start < tokens.len() {
            lines.push(tokens[last_start..].join(" "));
        }
    }

    lines.join("\n")
}

// ============================================================================
// Bubble Contour: multi-criterion line breaking for manga speech bubbles
//
// The line grid is chosen by the classical Knuth–Plass demerits model, extended
// with a silhouette profile: rows near the top and bottom of a bubble are
// narrower than its waist, so every candidate is solved for a vector of target
// widths instead of one common measure. The implementation is written from the
// published algorithm and the geometry of oval/diamond bubbles; no third-party
// source code or naming is used.
// ============================================================================

/// Geometric silhouette of a speech bubble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BubbleShape {
    VerticalOval,
    HorizontalOval,
    Rectangle,
    Diamond,
    RoundOval,
}

impl BubbleShape {
    /// Classify a bubble by its aspect ratio. Very small boxes are treated as rectangles.
    pub fn from_aspect_ratio(width: f32, height: f32) -> Self {
        if !width.is_finite() || !height.is_finite() || width <= 10.0 || height <= 10.0 {
            return BubbleShape::Rectangle;
        }
        let ratio = width / height;
        if ratio > 1.35 {
            BubbleShape::HorizontalOval
        } else if ratio < 0.85 {
            BubbleShape::VerticalOval
        } else {
            BubbleShape::RoundOval
        }
    }
}

/// Optical width of one character in em units (an ordinary lowercase letter is 1.0).
pub fn visible_char_width(c: char) -> f32 {
    if c.is_whitespace()
        || matches!(c, 'i' | 'l' | 'I' | '.' | ',' | ';' | ':' | '!' | '|' | '\'' | '’' | '«' | '»' | '‹' | '›' | '(' | ')' | '[' | ']' | '{' | '}' | '…')
    {
        0.45
    } else if matches!(c, 'm' | 'w' | 'M' | 'W' | '@' | '#' | '%' | '&' | 'ж' | 'ш' | 'щ' | 'ю' | 'Ж' | 'Ш' | 'Щ' | 'Ю') {
        1.32
    } else if c.is_ascii_uppercase() || ('А'..='Я').contains(&c) || matches!(c, 'Ё' | 'Ї' | 'І' | 'Є' | 'Ґ') {
        1.12
    // Full-width CJK: kana, han and hangul.
    } else if ('\u{3040}'..='\u{30FF}').contains(&c) || ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{AC00}'..='\u{D7AF}').contains(&c) {
        1.10
    } else {
        1.0
    }
}

/// Sum of the optical character widths of `text`, in em units.
pub fn measure_visible_width(text: &str) -> f32 {
    text.chars().map(visible_char_width).sum()
}

/// Width of a rendered line, including the 0.45 em inter-word spaces.
pub fn line_visible_width(line: &str) -> f32 {
    let mut words = line.split(' ').filter(|word| !word.is_empty());
    let Some(first) = words.next() else {
        return 0.0;
    };
    let mut width = measure_visible_width(first);
    let mut gaps = 0usize;
    for word in words {
        width += measure_visible_width(word);
        gaps += 1;
    }
    width + gaps as f32 * 0.45
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

/// Relative target widths of `line_count` rows of a bubble silhouette.
///
/// `curve` controls how much the edge rows narrow, `shift` moves the widest row
/// above or below the geometric center, and `min_weight` keeps edge rows readable.
pub fn bubble_contour_weights(line_count: usize, curve: f32, shift: f32, min_weight: f32) -> Vec<f32> {
    if line_count <= 1 {
        return vec![1.0];
    }
    let curve = finite_or(curve, 0.0).clamp(0.0, 1.0);
    let shift = finite_or(shift, 0.0).clamp(-1.5, 1.5);
    let min_weight = finite_or(min_weight, 0.35).clamp(0.05, 1.0);
    let center = (line_count as f32 - 1.0) / 2.0 + shift;
    let max_dist = center.max(line_count as f32 - 1.0 - center).max(1.0);
    (0..line_count)
        .map(|idx| {
            let dist = (idx as f32 - center).abs() / max_dist;
            (1.0 - dist * curve).max(min_weight)
        })
        .collect()
}

/// Target widths, in em units, of every row of one candidate line breaking.
#[allow(clippy::too_many_arguments)]
pub fn bubble_contour_targets(tokens: &[String], line_count: usize, curve: f32, shift: f32, bias: f32, min_weight: f32) -> Vec<f32> {
    let line_count = line_count.max(1);
    let total: f32 = tokens.iter().map(|token| measure_visible_width(token)).sum::<f32>() + tokens.len().saturating_sub(line_count) as f32 * 0.45;
    if !total.is_finite() || total <= 0.0 {
        return vec![1.0; line_count];
    }
    let weights = bubble_contour_weights(line_count, curve, shift, min_weight);
    let weight_sum = weights.iter().sum::<f32>().max(0.001);
    let bias = finite_or(bias, 0.0).clamp(-8.0, 8.0);
    weights.into_iter().map(|weight| ((total * weight) / weight_sum + bias).max(1.0)).collect()
}

/// Solve one candidate with a two-dimensional Knuth–Plass dynamic program.
///
/// Rows are filled from top to bottom; a state is `(line, first token of the next
/// row)`, so the solver can honor a different target width for every row while
/// still minimizing the sum of demerits. Returns the finished rows, or `None`
/// when `line_count` rows cannot be formed.
pub fn knuth_plass_contour_dp(tokens: &[String], line_count: usize, targets: &[f32]) -> Option<Vec<String>> {
    let n = tokens.len();
    if line_count < 1 || n < line_count {
        return None;
    }
    let cells = (line_count + 1).checked_mul(n + 1)?;
    // A single dialogue line should never need a giant grid; reject absurd input
    // instead of allocating gigabytes for a hostile script.
    if cells > 4_000_000 {
        return None;
    }

    let token_widths: Vec<f32> = tokens.iter().map(|token| measure_visible_width(token)).collect();
    let mut prefix = vec![0.0f64; n + 1];
    let mut ends_break = vec![false; n];
    for (idx, token) in tokens.iter().enumerate() {
        let width = finite_or(token_widths.get(idx).copied().unwrap_or(0.0), 0.0).max(0.0);
        prefix[idx + 1] = prefix[idx] + f64::from(width);
        ends_break[idx] = token.ends_with(['.', ',', ';', ':', '!', '?', '…', '»', ')', ']', '}']);
    }

    let stride = n + 1;
    let mut dp = vec![f64::INFINITY; (line_count + 1) * stride];
    let mut prev = vec![-1i32; (line_count + 1) * stride];
    dp[0] = 0.0;

    for line_idx in 0..line_count {
        let target = targets.get(line_idx).copied().filter(|value| value.is_finite() && *value > 0.0).unwrap_or(1.0) as f64;
        let prune_width = target * 3.5;
        let is_last = line_idx + 1 == line_count;
        let row_offset = line_idx * stride;
        let next_row_offset = (line_idx + 1) * stride;
        let max_to = n - (line_count - line_idx - 1);

        for from_idx in line_idx..=n {
            let base_cost = dp[row_offset + from_idx];
            if base_cost.is_infinite() {
                continue;
            }
            let from_prefix = prefix[from_idx];
            for to_idx in (from_idx + 1)..=max_to {
                let width = (prefix[to_idx] - from_prefix) + 0.45 * (to_idx - from_idx - 1) as f64;
                if !width.is_finite() {
                    continue;
                }
                // One word wider than the pruning window is still kept: it is the
                // only legal row start and will be penalized by the mismatch below.
                if width > prune_width && to_idx > from_idx + 1 {
                    break;
                }
                let ratio = (width - target) / target;
                let mut cost = ratio * ratio;
                if !is_last && ends_break[to_idx - 1] {
                    cost = (cost - 0.09).max(0.0);
                }
                let next_cost = base_cost + cost;
                if next_cost < dp[next_row_offset + to_idx] {
                    dp[next_row_offset + to_idx] = next_cost;
                    prev[next_row_offset + to_idx] = i32::try_from(from_idx).unwrap_or(-1);
                }
            }
        }
    }

    if !dp[line_count * stride + n].is_finite() {
        return None;
    }

    let mut lines = Vec::with_capacity(line_count);
    let mut cursor = n;
    for line_idx in (1..=line_count).rev() {
        let from_idx = *prev.get(line_idx * stride + cursor)?;
        let from_idx = usize::try_from(from_idx).ok()?;
        lines.push(tokens.get(from_idx..cursor)?.join(" "));
        cursor = from_idx;
    }
    lines.reverse();
    Some(lines)
}

/// Smoothness score of a silhouette (lower is better).
///
/// A well-set manga bubble has short top and bottom rows, a wide waist and no
/// abrupt width jumps between neighbors. The score compares the rows with that
/// ideal arch and penalizes a peak away from the center.
pub fn score_contour_silhouette(lengths: &[f32], smoothness_weight: f32, adjacent_slack: f32) -> f32 {
    let line_count = lengths.len();
    let max_len = lengths.iter().copied().filter(|value| value.is_finite()).fold(0.0f32, f32::max);
    if line_count < 2 || max_len <= 0.0 {
        return 0.0;
    }
    let smoothness_weight = finite_or(smoothness_weight, 160.0).max(0.0);
    let adjacent_slack = finite_or(adjacent_slack, 0.3).clamp(0.0, 1.0);
    let ideal = bubble_contour_weights(line_count, 0.55, 0.0, 0.4);
    let ideal_max = ideal.iter().copied().fold(0.0f32, f32::max).max(0.001);
    let clean = |value: f32| if value.is_finite() { value.max(0.0) } else { 0.0 };

    let mut score = 0.0f32;
    for (idx, &length) in lengths.iter().enumerate() {
        let length = clean(length);
        let target = max_len * ideal.get(idx).copied().unwrap_or(1.0) / ideal_max;
        let relative = (length - target) / max_len;
        score += relative * relative * 180.0;
        if idx > 0 {
            let previous = clean(lengths.get(idx - 1).copied().unwrap_or(0.0));
            let jump = (length - previous).abs() / max_len;
            let excess = (jump - adjacent_slack).max(0.0);
            score += excess * excess * smoothness_weight;
        }
    }

    let interior_max = lengths.get(1..line_count - 1).and_then(|interior| interior.iter().copied().reduce(f32::max)).unwrap_or(max_len);
    let edge_max = clean(lengths.first().copied().unwrap_or(0.0)).max(clean(lengths.last().copied().unwrap_or(0.0)));
    if edge_max > interior_max {
        let excess = (edge_max - interior_max) / max_len;
        score += excess * excess * 520.0;
    }

    if let Some(peak) = lengths.iter().enumerate().max_by(|left, right| left.1.partial_cmp(right.1).unwrap_or(std::cmp::Ordering::Equal)).map(|(idx, _)| idx) {
        let center = (line_count as f32 - 1.0) / 2.0;
        let offset = (peak as f32 - center).abs() / center.max(1.0);
        score += offset * offset * 70.0;
    }

    if score.is_finite() { score } else { f32::MAX }
}

/// Total demerits of one candidate line breaking.
#[allow(clippy::too_many_arguments)]
pub fn score_contour_candidate(
    lines: &[String],
    hyphen_count: usize,
    target_lines: usize,
    bubble_w: Option<f32>,
    bubble_h: Option<f32>,
    unit_px: f32,
    line_px: f32,
) -> f32 {
    let line_count = lines.len();
    if line_count == 0 {
        return f32::MAX;
    }
    let lengths: Vec<f32> = lines.iter().map(|line| line_visible_width(line)).collect();
    let total_len: f32 = lengths.iter().sum();
    if !total_len.is_finite() || total_len <= 0.0 {
        return f32::MAX;
    }
    let target_lines = target_lines.max(1);
    let unit_px = finite_or(unit_px, 1.0).max(1.0);
    let line_px = finite_or(line_px, 1.0).max(1.0);

    let weights = bubble_contour_weights(line_count, 0.65, 0.0, 0.35);
    let weight_sum = weights.iter().sum::<f32>().max(0.001);
    let targets: Vec<f32> = weights.iter().map(|weight| total_len * weight / weight_sum).collect();

    let line_delta = line_count as f32 - target_lines as f32;
    let delta_factor = if line_delta < 0.0 { 3.0 } else { 1.0 };
    let mut score = hyphen_count as f32 * 130.0 + line_delta.abs().powf(1.5) * 16.0 * delta_factor;

    if let (Some(bubble_w), Some(bubble_h)) = (bubble_w, bubble_h) {
        let bubble_w = finite_or(bubble_w, 0.0).max(1.0);
        let bubble_h = finite_or(bubble_h, 0.0).max(1.0);
        let height_excess = ((line_count as f32 * line_px) / (bubble_h * 0.90) - 1.0).max(0.0);
        if height_excess > 0.0 {
            score += 80.0 + height_excess * height_excess * 900.0;
        }
        let available_units = (bubble_w * 0.90) / unit_px;
        for &length in &lengths {
            let width_excess = (length / available_units - 1.0).max(0.0);
            if width_excess > 0.0 {
                score += 60.0 + width_excess * width_excess * 1600.0;
            }
        }
    }

    for (idx, &length) in lengths.iter().enumerate() {
        let target = targets.get(idx).copied().unwrap_or(1.0).max(1.0);
        let relative = (length - target) / target;
        score += relative * relative * 120.0;
        if idx + 1 < line_count && lines.get(idx).is_some_and(|line| line.ends_with(['.', ',', ';', ':', '!', '?', '…', '»'])) {
            score -= 14.0;
        }
    }

    score += score_contour_silhouette(&lengths, 160.0, 0.3);
    if score.is_finite() { score.max(0.0) } else { f32::MAX }
}

/// Generate a diverse set of line breakings for a bubble silhouette.
///
/// The search walks a bounded grid of silhouette curves, vertical shifts and
/// target biases. It keeps the best candidate of every line count first (so the
/// menu always offers visibly different heights), then fills the remaining slots
/// by score. No font file is needed: widths are optical em estimates.
pub fn generate_contour_variants(text: &str, bubble_w: Option<f32>, bubble_h: Option<f32>, font_size_pt: f32, dpi: f32, limit: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let bound_text = bind_orphan_prepositions(trimmed);
    let tokens: Vec<String> = bound_text.split(' ').filter(|word| !word.is_empty()).map(str::to_string).collect();
    let n = tokens.len();
    if n <= 1 {
        return vec![bound_text];
    }
    let limit = limit.clamp(1, 64);

    let dpi_scale = if dpi.is_finite() && dpi > 0.0 { dpi / 72.0 } else { 1.0 };
    let font_size_px = if font_size_pt.is_finite() && font_size_pt > 0.0 { font_size_pt * dpi_scale } else { 14.0 * dpi_scale.max(1.0) };
    let unit_px = (font_size_px * 0.52).max(1.0);
    let line_px = (font_size_px * 1.25).max(1.0);
    let total_units: f32 = tokens.iter().map(|token| measure_visible_width(token)).sum::<f32>() + tokens.len().saturating_sub(1) as f32 * 0.45;
    let bubble_w = bubble_w.filter(|value| value.is_finite() && *value >= 10.0);
    let bubble_h = bubble_h.filter(|value| value.is_finite() && *value >= 10.0);

    let (min_lines, max_lines, target_lines) = match (bubble_w, bubble_h) {
        (Some(width), Some(height)) => {
            let available_units = ((width * 0.90) / unit_px).max(6.0);
            let estimate = (total_units / (available_units * 0.75)).ceil() as usize;
            let aspect = (height / width.max(1.0)).clamp(0.25, 3.0);
            let target = if aspect >= 1.15 { estimate.max(((height / line_px) * 0.7).round() as usize).max(1) } else { estimate.max(1) };
            let minimum = target.saturating_sub(1).max(1).min(n);
            let maximum = (target + 2).max(minimum).min(n);
            (minimum, maximum, target)
        }
        _ => {
            let volume = (total_units / 2.6).sqrt().round() as usize;
            let target = volume.clamp(if n >= 3 { 2 } else { 1 }, 6);
            let minimum = if n <= 2 { 1 } else { 2 };
            let maximum = (target + 2).clamp(minimum, n.min(8));
            (minimum, maximum, target)
        }
    };

    const CURVES: [f32; 4] = [0.35, 0.45, 0.6, 0.75];
    const SHIFTS: [f32; 5] = [0.0, -0.12, 0.12, -0.2, 0.2];
    const BIASES: [f32; 3] = [-1.0, 0.0, 1.0];

    let mut candidates: std::collections::HashMap<String, (f32, usize)> = std::collections::HashMap::new();
    for line_count in min_lines..=max_lines {
        for &curve in &CURVES {
            for &shift in &SHIFTS {
                for &bias in &BIASES {
                    let targets = bubble_contour_targets(&tokens, line_count, curve, shift, bias, 0.35);
                    let Some(lines) = knuth_plass_contour_dp(&tokens, line_count, &targets) else {
                        continue;
                    };
                    let candidate_text = lines.join("\n");
                    candidates.entry(candidate_text).or_insert_with(|| {
                        let score = score_contour_candidate(&lines, 0, target_lines, bubble_w, bubble_h, unit_px, line_px);
                        (score, line_count)
                    });
                }
            }
        }
    }

    if candidates.is_empty() {
        return vec![bound_text];
    }

    let mut sorted: Vec<(String, f32, usize)> = candidates.into_iter().map(|(text, (score, lines))| (text, score, lines)).collect();
    sorted.sort_by(|left, right| left.1.partial_cmp(&right.1).unwrap_or(std::cmp::Ordering::Equal));

    line_count_diversity(&sorted, limit)
}

/// Pick the best candidate of every distinct line count first, then fill the
/// remaining slots by score. A menu that offers only near-identical heights is
/// useless for choosing a silhouette, so height diversity is guaranteed before
/// duplicate quality is considered.
fn line_count_diversity(sorted: &[(String, f32, usize)], limit: usize) -> Vec<String> {
    let mut picked = Vec::new();
    let mut seen_text = std::collections::HashSet::new();
    let mut seen_lines = std::collections::HashSet::new();
    for (text, _, line_count) in sorted {
        if picked.len() >= limit {
            break;
        }
        if seen_lines.insert(*line_count) {
            seen_text.insert(text.clone());
            picked.push(text.clone());
        }
    }
    for (text, _, _) in sorted {
        if picked.len() >= limit {
            break;
        }
        if seen_text.insert(text.clone()) {
            picked.push(text.clone());
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bind_orphan_prepositions() {
        let input = "СВОБОДОЙ, Я В ИТОГЕ";
        let res = bind_orphan_prepositions(input);
        assert!(res.contains(&format!("Я{}", NBSP)));
        assert!(res.contains(&format!("В{}", NBSP)));

        let particles = "что же это было бы или нет ли";
        let p_res = bind_orphan_prepositions(particles);
        assert!(p_res.contains(&format!("{}же", NBSP)));
        assert!(p_res.contains(&format!("{}бы", NBSP)));
        assert!(p_res.contains(&format!("{}ли", NBSP)));
    }

    #[test]
    fn test_knuth_plass_bubble_wrap_shape() {
        let text = "Это длинный тестовый текст для проверки солвера Кнута-Пласса в баллонах манги";
        let wrapped = knuth_plass_bubble_wrap(text, 180.0, 200.0, 14.0, 72.0, 100.0);
        let lines: Vec<&str> = wrapped.split('\n').collect();
        assert!(lines.len() >= 2);
        // Ни одна строка не должна быть пустой
        for line in &lines {
            assert!(!line.trim().is_empty());
        }
    }

    #[test]
    fn test_knuth_plass_bubble_wrap_height_constrained() {
        let text = "Короткий текст в баббле манги для проверки высоты";
        // При высокой высоте баббла допускается больше строк
        let tall = knuth_plass_bubble_wrap(text, 120.0, 300.0, 14.0, 72.0, 100.0);
        // При жестком ограничении по высоте солвер предпочитает минимальное число строк
        let flat = knuth_plass_bubble_wrap(text, 120.0, 40.0, 14.0, 72.0, 100.0);
        let tall_lines: Vec<&str> = tall.split('\n').collect();
        let flat_lines: Vec<&str> = flat.split('\n').collect();
        assert!(flat_lines.len() <= tall_lines.len());
    }

    #[test]
    fn test_punctuation_compensation() {
        let comp_ellipsis = punctuation_compensation_px("Привет...", 16.0, 1.0);
        assert!(comp_ellipsis > 0.0);
        let comp_dot = punctuation_compensation_px("Конец.", 16.0, 1.0);
        assert!(comp_dot > 0.0);
        let comp_quote = punctuation_compensation_px("Слово»", 16.0, 1.0);
        assert!(comp_quote > 0.0);
        let comp_plain = punctuation_compensation_px("ОбычноеСлово", 16.0, 1.0);
        assert_eq!(comp_plain, 0.0);
    }

    #[test]
    fn test_bubble_shape_from_aspect_ratio() {
        assert_eq!(BubbleShape::from_aspect_ratio(4.0, 4.0), BubbleShape::Rectangle);
        assert_eq!(BubbleShape::from_aspect_ratio(300.0, 100.0), BubbleShape::HorizontalOval);
        assert_eq!(BubbleShape::from_aspect_ratio(100.0, 300.0), BubbleShape::VerticalOval);
        assert_eq!(BubbleShape::from_aspect_ratio(120.0, 120.0), BubbleShape::RoundOval);
        assert_eq!(BubbleShape::from_aspect_ratio(f32::NAN, 120.0), BubbleShape::Rectangle);
    }

    #[test]
    fn test_visible_width_counts_cjk_as_full_width() {
        assert!(visible_char_width('漢') > visible_char_width('i'));
        assert!(visible_char_width('あ') > visible_char_width('.'));
        assert_eq!(measure_visible_width("ab"), 2.0);
        assert!(line_visible_width("ab cd") > measure_visible_width("abcd"), "the word space adds width");
    }

    #[test]
    fn test_contour_weights_form_an_arch() {
        let weights = bubble_contour_weights(5, 0.6, 0.0, 0.35);
        assert_eq!(weights.len(), 5);
        assert!(weights[2] > weights[0]);
        assert!(weights[2] > weights[4]);
        assert!(weights.iter().all(|weight| (0.35..=1.0).contains(weight)));

        let shifted = bubble_contour_weights(5, 0.6, 0.4, 0.35);
        assert!(shifted[3] > shifted[1], "a positive shift moves the widest row down");
    }

    #[test]
    fn test_contour_dp_keeps_every_token() {
        let tokens: Vec<String> = "один два три четыре пять шесть семь".split(' ').map(str::to_string).collect();
        let targets = vec![4.0, 5.0, 4.0];
        let lines = knuth_plass_contour_dp(&tokens, 3, &targets).expect("three legal rows");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines.join(" "), tokens.join(" "));
        assert!(knuth_plass_contour_dp(&tokens, tokens.len() + 1, &targets).is_none());
    }

    #[test]
    fn test_generate_contour_variants_are_diverse() {
        let text = "Это длинная реплика персонажа которая должна красиво переноситься внутри баббла";
        let variants = generate_contour_variants(text, Some(260.0), Some(200.0), 12.0, 72.0, 8);
        assert!(!variants.is_empty());
        assert!(variants.len() <= 8);
        let original: Vec<&str> = text.split_whitespace().collect();
        let mut line_counts = std::collections::BTreeSet::new();
        for variant in &variants {
            assert_eq!(variant.split_whitespace().collect::<Vec<_>>(), original);
            line_counts.insert(variant.lines().count());
        }
        assert!(line_counts.len() >= 2, "the generator keeps line-count diversity: {line_counts:?}");
    }
}
