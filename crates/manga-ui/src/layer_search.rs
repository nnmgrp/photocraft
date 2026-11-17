//! Global layer & text search across every open document tab (Edit › Find in Open Pages…).
//!
//! A floating palette (Spotlight / VS Code Quick Open style) that searches every layer name and
//! every text-layer content across all open documents, with prefix filters (`@` text only, `#`
//! groups only, `!` hidden only), fuzzy matching and highlighted hits. Wrong keyboard layouts are
//! handled too: the query is also tried re-mapped through the QWERTY ↔ ЙЦУКЕН layout (`ghbdtn`
//! finds «привет», `игииду` finds "Bubble"), so a typesetter who forgot to switch still finds what
//! they meant.
//!
//! This module is deliberately decoupled from the application: the host passes a slice of
//! [`SearchDoc`]s and receives an [`SearchActivation`] describing the jump (document, layer,
//! parent groups to expand and viewport centre). No `PhotocraftApp`, session or command dispatch
//! is referenced here. See [`show`] for the UI and [`build_activation`] for the pure result
//! computation.

use egui::{Align2, Color32, CornerRadius, RichText, Sense, Stroke, TextFormat, vec2};
use photocraft_doc::{Document, Layer, LayerContent, LayerId, comps::position_bounds};

use crate::BridgeDocId;
use crate::theme::Tokens;

/// Width of the floating palette (px).
const WIDTH: f32 = 580.0;
/// How many results the list shows at most.
const CAP: usize = 12;
/// Vertical offset of the palette from the top of the screen (px).
const TOP_OFFSET: f32 = 100.0;
/// Maximum chars on each side of a text hit's match point, for the snippet shown in the row.
const SNIPPET_RADIUS: usize = 40;
/// How far a wrong-layout match ranks below the same match for the raw query.
const TRANSLIT_PENALTY: i32 = 25;
/// State keys (egui 0.37 uses `Id::unique` in place of the old `Id::new`).
const QUERY_ID: &str = "layer-search-query";
const SEL_ID: &str = "layer-search-sel";
/// Fixed column widths inside a result row.
const ROW_PAD: f32 = 10.0;
const DOC_BADGE_W: f32 = 90.0;
const TYPE_BADGE_W: f32 = 34.0;
const NAME_COL_W: f32 = 200.0;

/// One open document handed to the palette by the host.
pub struct SearchDoc<'a> {
    /// Stable bridge id of the document.
    pub id: BridgeDocId,
    /// Index of the document in the host's document list.
    pub index: usize,
    /// Tab title (e.g. `01.png`).
    pub name: &'a str,
    /// Whether this document is the active tab (its hits sort first).
    pub active: bool,
    /// The document model to search.
    pub doc: &'a Document,
}

/// What the host should do when a result is activated.
pub struct SearchActivation {
    /// Bridge id of the target document.
    pub doc_id: BridgeDocId,
    /// Index of the target document in the host's document list.
    pub doc_index: usize,
    /// Layer to select.
    pub layer: LayerId,
    /// Collapsed ancestor groups to expand, outermost first.
    pub groups_to_expand: Vec<LayerId>,
    /// Viewport centre in document pixels, derived from the layer's content bounds.
    pub center: [f32; 2],
}

/// One search hit. `name_matches` / `text_matches` carry the byte offsets that the query matched,
/// so the UI can highlight them.
#[derive(Clone)]
struct Hit {
    /// Index of the document in the `docs` slice.
    doc: usize,
    /// The tab title.
    doc_name: String,
    /// The matched layer's id.
    layer: LayerId,
    /// The layer's name, as a single-line label.
    label: String,
    /// Flattened text content of a text layer, if any.
    text: Option<String>,
    /// Layer-type badge: `[T]` text, `[G]` group, `[Px]` raster, `[Tone]` screentone, …
    badge: &'static str,
    /// Whether the layer is hidden.
    hidden: bool,
    /// Whether the layer belongs to the active document.
    active_doc: bool,
    /// Byte offsets in `label` the query matched.
    name_matches: Vec<usize>,
    /// Byte offsets in `text` the query matched.
    text_matches: Vec<usize>,
    /// Score (higher is better); see [`score_hit`].
    score: i32,
}

/// The query, with its leading `@` / `#` / `!` filters split off and the body normalized.
struct Query {
    text_only: bool,
    groups_only: bool,
    hidden_only: bool,
    body: String,
    variants: Vec<(String, i32)>,
}

/// Keyboard state read once per frame.
struct Keys {
    down: bool,
    up: bool,
    enter: bool,
    shift_enter: bool,
    esc: bool,
}

/// Pre-built text formats for a result row.
struct Formats {
    plain: TextFormat,
    high: TextFormat,
    tplain: TextFormat,
    thigh: TextFormat,
}

/// QWERTY ↔ ЙЦУКЕН wrong-layout pairs as `(latin, cyrillic)`.
const LAYOUT_PAIRS: &[(char, char)] = &[
    ('q', 'й'),
    ('w', 'ц'),
    ('e', 'у'),
    ('r', 'к'),
    ('t', 'е'),
    ('y', 'н'),
    ('u', 'г'),
    ('i', 'ш'),
    ('o', 'щ'),
    ('p', 'з'),
    ('[', 'х'),
    (']', 'ъ'),
    ('a', 'ф'),
    ('s', 'ы'),
    ('d', 'в'),
    ('f', 'а'),
    ('g', 'п'),
    ('h', 'р'),
    ('j', 'о'),
    ('k', 'л'),
    ('l', 'д'),
    (';', 'ж'),
    ('\'', 'э'),
    ('z', 'я'),
    ('x', 'ч'),
    ('c', 'с'),
    ('v', 'м'),
    ('b', 'и'),
    ('n', 'т'),
    ('m', 'ь'),
    (',', 'б'),
    ('.', 'ю'),
    ('`', 'ё'),
];

/// Case-insensitive subsequence match score (higher is better); `None` = no match.
///
/// Adapted from `photocraft-ui-egui::palette::fuzzy_score` so callers here need no dependency on
/// the command palette.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut last = None;
    for qc in query.to_lowercase().chars() {
        let mut found = false;
        while ti < t.len() {
            if t[ti] == qc {
                score += if last == Some(ti.wrapping_sub(1)) { 8 } else { 1 };
                if ti == 0 || t[ti - 1] == ' ' {
                    score += 5;
                }
                last = Some(ti);
                ti += 1;
                found = true;
                break;
            }
            ti += 1;
        }
        if !found {
            return None;
        }
    }
    Some(score - (t.len() as i32 / 8))
}

/// Re-map each char of `text` through the keyboard layout: QWERTY → ЙЦУКЕН when `to_cyr`, the
/// reverse otherwise. Chars with no mapping pass through.
fn remap_layout(text: &str, to_cyr: bool) -> String {
    text.chars()
        .map(|c| {
            for &(latin, cyr) in LAYOUT_PAIRS {
                if to_cyr && c == latin {
                    return cyr;
                }
                if !to_cyr && c == cyr {
                    return latin;
                }
            }
            c
        })
        .collect()
}

/// The query re-mapped through the keyboard layout(s): a latin body also tries cyrillic, a
/// cyrillic body also tries latin; a mixed body tries both directions.
fn layout_variants(body: &str) -> Vec<String> {
    let has_latin = body.chars().any(|c| c.is_ascii_alphabetic());
    let has_cyr = body.chars().any(|c| matches!(c, '\u{0410}'..='\u{044F}' | '\u{0401}' | '\u{0451}'));
    let mut out = Vec::with_capacity(2);
    if has_latin {
        out.push(remap_layout(body, true));
    }
    if has_cyr {
        out.push(remap_layout(body, false));
    }
    out
}

impl Query {
    /// Parse the user input: peel leading `@`/`#`/`!` (in any order), then trim and lower-case.
    fn parse(raw: &str) -> Self {
        let mut text_only = false;
        let mut groups_only = false;
        let mut hidden_only = false;
        let mut s = raw.trim_start();
        loop {
            match s.chars().next() {
                Some('@') => {
                    text_only = true;
                    s = &s[1..];
                }
                Some('#') => {
                    groups_only = true;
                    s = &s[1..];
                }
                Some('!') => {
                    hidden_only = true;
                    s = &s[1..];
                }
                _ => break,
            }
            s = s.trim_start();
        }
        let body = s.trim_end().to_lowercase();
        let mut variants: Vec<(String, i32)> = Vec::with_capacity(3);
        variants.push((body.clone(), 0));
        variants.extend(layout_variants(&body).into_iter().map(|v| (v, TRANSLIT_PENALTY)));
        Self { text_only, groups_only, hidden_only, body, variants }
    }
}

/// Flatten line breaks (and other whitespace runs) into single spaces so a multi-line manga
/// dialogue bubble matches a single-line query.
fn flatten_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
        }
    }
    out.trim().to_string()
}

/// Lower-cased form of a layer's name (the comparison key).
fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Substring search: byte offsets of every char of `query` in `text`, or `None`.
fn substring_matches(query: &str, text: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let q: Vec<char> = query.chars().collect();
    let t: Vec<char> = text.chars().collect();
    if q.len() > t.len() {
        return None;
    }
    for window in 0..=t.len().saturating_sub(q.len()) {
        if t[window..window + q.len()] == q[..] {
            let mut byte_offs = Vec::with_capacity(q.len());
            let mut at = text.char_indices().nth(window).map(|(b, _)| b).unwrap_or(0);
            for c in &q {
                byte_offs.push(at);
                at += c.len_utf8();
            }
            return Some(byte_offs);
        }
    }
    None
}

/// Fuzzy subsequence match: byte offsets of the matched chars plus a score, or `None`.
fn fuzzy_matches(query: &str, text: &str) -> Option<(Vec<usize>, i32)> {
    if query.is_empty() {
        return Some((Vec::new(), 0));
    }
    let t: Vec<char> = text.chars().collect();
    let byte_offs: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    let mut score = 0;
    let mut ti = 0;
    let mut last: Option<usize> = None;
    let mut matched: Vec<usize> = Vec::with_capacity(query.chars().count());
    for qc in query.chars() {
        let mut found = false;
        while ti < t.len() {
            if t[ti] == qc {
                score += if last == Some(ti.wrapping_sub(1)) { 8 } else { 1 };
                if ti == 0 || t[ti - 1] == ' ' {
                    score += 5;
                }
                matched.push(byte_offs[ti]);
                last = Some(ti);
                ti += 1;
                found = true;
                break;
            }
            ti += 1;
        }
        if !found {
            return None;
        }
    }
    Some((matched, score - (t.len() as i32 / 8)))
}

/// How much a shorter name is worth within one rank tier.
fn specificity(label_lower: &str) -> i32 {
    99 - (label_lower.len() as i32).min(99)
}

/// The first variant that is a contiguous substring of `text`, with its rank penalty.
fn best_substring(variants: &[(String, i32)], text: &str) -> Option<(Vec<usize>, i32)> {
    variants.iter().filter_map(|(v, penalty)| substring_matches(v, text).map(|offs| (offs, *penalty))).min_by_key(|&(_, penalty)| penalty)
}

/// Score and pick the best match for one layer: name substring, text substring, name fuzzy, text
/// fuzzy, in that order. An empty body returns a baseline hit so every layer can be listed.
fn score_hit(query: &Query, label_lower: &str, text_lower: Option<&str>) -> Option<(Vec<usize>, Vec<usize>, i32)> {
    if query.body.is_empty() {
        return Some((Vec::new(), Vec::new(), 0));
    }
    if let Some((offs, penalty)) = best_substring(&query.variants, label_lower) {
        return Some((offs, Vec::new(), 1000 + specificity(label_lower) - penalty));
    }
    if let Some(text_lower) = text_lower
        && let Some((offs, penalty)) = best_substring(&query.variants, text_lower)
    {
        return Some((Vec::new(), offs, 900 + specificity(label_lower) - penalty));
    }
    if let Some((offs, s)) = fuzzy_matches(&query.body, label_lower) {
        return Some((offs, Vec::new(), 100 + s));
    }
    if let Some(text_lower) = text_lower
        && let Some((offs, s)) = fuzzy_matches(&query.body, text_lower)
    {
        return Some((Vec::new(), offs, s));
    }
    None
}

/// Build a hit for one layer if it passes the filters and matches the query.
fn build_hit(layer: &Layer, doc: &SearchDoc<'_>, query: &Query) -> Option<Hit> {
    let (badge, text_content): (&'static str, Option<&str>) = match &layer.content {
        LayerContent::Text(t) => ("[T]", Some(t.text.as_str())),
        LayerContent::Group(_) => ("[G]", None),
        LayerContent::Raster(_) => ("[Px]", None),
        LayerContent::Shape(_) => ("[S]", None),
        LayerContent::Smart(_) => ("[Sm]", None),
        LayerContent::Adjustment(_) => ("[Adj]", None),
        LayerContent::Fill(_) => ("[Fill]", None),
        LayerContent::Screentone(_) => ("[Tone]", None),
    };
    if query.text_only && !matches!(layer.content, LayerContent::Text(_)) {
        return None;
    }
    if query.groups_only && !matches!(layer.content, LayerContent::Group(_)) {
        return None;
    }
    if query.hidden_only && layer.visible {
        return None;
    }
    let label = layer.name.clone();
    let label_lower = lower(&label);
    let flat_text = text_content.map(flatten_text);
    let text_lower = flat_text.as_ref().map(|s| lower(s));
    let (name_matches, text_matches, score) = score_hit(query, &label_lower, text_lower.as_deref())?;
    Some(Hit {
        doc: doc.index,
        doc_name: doc.name.to_string(),
        layer: layer.id,
        label,
        text: flat_text.filter(|s| !s.is_empty()),
        badge,
        hidden: !layer.visible,
        active_doc: doc.active,
        name_matches,
        text_matches,
        score,
    })
}

/// Collect every layer across every open document that passes the filters and matches the body.
fn collect_hits(docs: &[SearchDoc<'_>], query: &Query) -> Vec<Hit> {
    let mut out = Vec::new();
    for doc in docs {
        for (_, _, layer) in doc.doc.walk() {
            if let Some(hit) = build_hit(layer, doc, query) {
                out.push(hit);
            }
        }
    }
    out.sort_by(|a, b| b.active_doc.cmp(&a.active_doc).then_with(|| b.score.cmp(&a.score)).then_with(|| a.label.cmp(&b.label)));
    out.truncate(CAP);
    out
}

/// Compute the jump for a chosen hit purely from the documents passed to [`show`].
fn build_activation(docs: &[SearchDoc<'_>], hit: &Hit) -> Option<SearchActivation> {
    let doc = docs.get(hit.doc)?;
    let layer = doc.doc.layer(hit.layer)?;
    let path = doc.doc.path_of(hit.layer).unwrap_or_default();
    let groups_to_expand: Vec<LayerId> = (1..path.len())
        .filter_map(|n| {
            let parent_path = path.get(..n)?;
            let parent = doc.doc.layer_at(parent_path)?;
            matches!(&parent.content, LayerContent::Group(g) if !g.expanded).then_some(parent.id)
        })
        .collect();
    let bounds = position_bounds(layer);
    let size = doc.doc.size;
    let center = if bounds.is_empty() {
        [size.width as f32 / 2.0, size.height as f32 / 2.0]
    } else {
        [((bounds.x0 as f32) + (bounds.x1 as f32)) / 2.0, ((bounds.y0 as f32) + (bounds.y1 as f32)) / 2.0]
    };
    Some(SearchActivation { doc_id: doc.id, doc_index: doc.index, layer: hit.layer, groups_to_expand, center })
}

/// Show the floating palette if it is open. Returns the activation to apply on a plain `Enter`
/// (jump) or click; `Shift+Enter` (peek) returns one but keeps the palette open. `Escape` closes.
pub fn show(ctx: &egui::Context, open: &mut bool, docs: &[SearchDoc<'_>]) -> Option<SearchActivation> {
    if !*open {
        return None;
    }
    let tokens = Tokens::get(ctx);
    let screen = ctx.content_rect();
    draw_scrim(ctx, &screen, open);
    let mut chosen: Option<(Hit, bool)> = None;
    draw_palette(ctx, &tokens, &screen, docs, open, &mut chosen);
    let (hit, peek) = chosen?;
    if !peek {
        *open = false;
    }
    build_activation(docs, &hit)
}

/// Full-screen click-to-dismiss scrim.
fn draw_scrim(ctx: &egui::Context, screen: &egui::Rect, open: &mut bool) {
    egui::Area::new(egui::Id::unique("layer-search-scrim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let response = ui.allocate_exact_size(screen.size(), Sense::click()).1;
        ui.painter().rect_filled(*screen, 0.0, Color32::from_black_alpha(110));
        if response.clicked() {
            *open = false;
        }
    });
}

/// The floating window itself.
fn draw_palette(ctx: &egui::Context, tokens: &Tokens, screen: &egui::Rect, docs: &[SearchDoc<'_>], open: &mut bool, chosen: &mut Option<(Hit, bool)>) {
    egui::Area::new(egui::Id::unique("layer-search"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(egui::pos2(screen.center().x, screen.top() + TOP_OFFSET))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(tokens.card)
                .stroke(Stroke::new(1.0, tokens.card_border))
                .corner_radius(CornerRadius::same(tokens.radius_lg as u8))
                .shadow(egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: tokens.shadow })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(WIDTH);
                    draw_body(ui, tokens, docs, open, chosen);
                });
        });
}

/// Query field, hit list, keyboard navigation and footer.
fn draw_body(ui: &mut egui::Ui, tokens: &Tokens, docs: &[SearchDoc<'_>], open: &mut bool, chosen: &mut Option<(Hit, bool)>) {
    let mut query_text: String = ui.data_mut(|d| d.get_temp::<String>(egui::Id::unique(QUERY_ID)).unwrap_or_default());
    draw_query_row(ui, tokens, &mut query_text);
    ui.add_space(6.0);
    crate::widgets::hairline(ui);
    ui.add_space(6.0);
    let query = Query::parse(&query_text);
    let hits = collect_hits(docs, &query);
    let mut sel: usize = ui.data_mut(|d| d.get_temp::<usize>(egui::Id::unique(SEL_ID)).unwrap_or(0));
    let keys = read_keys(ui);
    navigate(&keys, hits.len(), &mut sel);
    if keys.esc {
        *open = false;
    }
    let formats = Formats::new(tokens);
    draw_hits(ui, tokens, &formats, &hits, sel, &keys, chosen);
    if keys.enter {
        if chosen.is_none()
            && let Some(hit) = hits.get(sel)
        {
            *chosen = Some((hit.clone(), false));
        }
        // A dialog opened below would read this Enter as OK: consume it.
        ui.ctx().input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    }
    ui.add_space(4.0);
    ui.label(RichText::new(tl!("Enter: Jump • Shift+Enter: Peek • Esc: Close")).small().color(tokens.text_faint));
    ui.data_mut(|d| {
        d.insert_temp(egui::Id::unique(QUERY_ID), query_text);
        d.insert_temp(egui::Id::unique(SEL_ID), sel);
    });
}

/// Input row with a small, crisp magnifying glass drawn directly with the painter.
fn draw_query_row(ui: &mut egui::Ui, tokens: &Tokens, query_text: &mut String) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        paint_search_icon(ui, rect, tokens.text_dim);
        let edit = egui::TextEdit::singleline(query_text)
            .hint_text(tl!("Find layer or text across open pages (@text, #group)..."))
            .frame(egui::Frame::NONE)
            .font(egui::FontId::proportional(15.0))
            .desired_width(WIDTH - 40.0);
        let response = ui.add(edit);
        // Only request focus when missing: every request_focus interrupts the IME.
        if !response.has_focus() {
            response.request_focus();
        }
    });
}

/// Circle plus handle, sized to the 22×22 icon slot.
fn paint_search_icon(ui: &mut egui::Ui, rect: egui::Rect, color: Color32) {
    let painter = ui.painter();
    let center = rect.center() - vec2(2.0, 2.0);
    let radius = 5.0;
    painter.circle_stroke(center, radius, Stroke::new(1.6, color));
    let diagonal = radius * 0.71;
    let joint = center + vec2(diagonal, diagonal);
    painter.line_segment([joint, joint + vec2(4.5, 4.5)], Stroke::new(1.8, color));
}

/// Read the navigation keys once per frame.
fn read_keys(ui: &egui::Ui) -> Keys {
    ui.input(|i| Keys {
        down: i.key_pressed(egui::Key::ArrowDown),
        up: i.key_pressed(egui::Key::ArrowUp),
        enter: i.key_pressed(egui::Key::Enter) && !i.modifiers.shift,
        shift_enter: i.key_pressed(egui::Key::Enter) && i.modifiers.shift,
        esc: i.key_pressed(egui::Key::Escape),
    })
}

/// Clamp the selection to the current result count.
fn navigate(keys: &Keys, len: usize, sel: &mut usize) {
    if keys.down {
        *sel = (*sel + 1).min(len.saturating_sub(1));
    }
    if keys.up {
        *sel = (*sel).saturating_sub(1);
    }
    *sel = (*sel).min(len.saturating_sub(1));
}

impl Formats {
    fn new(tokens: &Tokens) -> Self {
        let medium = |size: f32| egui::FontId::new(size, egui::FontFamily::Name("medium".into()));
        Self {
            plain: TextFormat { font_id: medium(13.0), color: tokens.text, ..Default::default() },
            high: TextFormat { font_id: medium(13.0), color: tokens.accent, background: tokens.accent_soft, ..Default::default() },
            tplain: TextFormat { font_id: egui::FontId::proportional(11.5), color: tokens.text_dim, ..Default::default() },
            thigh: TextFormat { font_id: egui::FontId::proportional(11.5), color: tokens.accent, background: tokens.accent_soft, ..Default::default() },
        }
    }
}

/// Paint the result list and record keyboard activation.
fn draw_hits(ui: &mut egui::Ui, tokens: &Tokens, formats: &Formats, hits: &[Hit], sel: usize, keys: &Keys, chosen: &mut Option<(Hit, bool)>) {
    if hits.is_empty() {
        ui.label(RichText::new(tl!("No matching layers or text in open pages")).color(tokens.text_faint));
        return;
    }
    for (i, hit) in hits.iter().enumerate() {
        draw_row(ui, tokens, formats, hit, i == sel, chosen);
    }
    if (keys.enter || keys.shift_enter)
        && chosen.is_none()
        && let Some(hit) = hits.get(sel)
    {
        *chosen = Some((hit.clone(), keys.shift_enter));
    }
}

/// Paint one result row (document badge, type badge, layer name, optional text snippet).
fn draw_row(ui: &mut egui::Ui, tokens: &Tokens, formats: &Formats, hit: &Hit, selected: bool, chosen: &mut Option<(Hit, bool)>) {
    let (rect, response) = ui.allocate_exact_size(vec2(WIDTH, 30.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(rect, tokens.radius_sm, if selected { tokens.accent_soft } else { tokens.hover });
    }
    let badge_color = if hit.hidden { tokens.text_faint } else { tokens.text_dim };
    let max_doc_chars = 13;
    let doc_display = if hit.doc_name.chars().count() > max_doc_chars {
        let prefix: String = hit.doc_name.chars().take(max_doc_chars - 1).collect();
        format!("{prefix}…")
    } else {
        hit.doc_name.clone()
    };
    let doc_label = format!("[{doc_display}]");
    let doc_font = egui::FontId::proportional(11.0);
    ui.painter().text(rect.left_center() + vec2(ROW_PAD, 0.0), Align2::LEFT_CENTER, doc_label, doc_font.clone(), badge_color);
    ui.painter().text(rect.left_center() + vec2(ROW_PAD + DOC_BADGE_W, 0.0), Align2::LEFT_CENTER, hit.badge, doc_font, badge_color);
    let name_left = ROW_PAD + DOC_BADGE_W + TYPE_BADGE_W;
    let mut job = egui::text::LayoutJob::default();
    let max_name_w = if hit.text.is_some() { 190.0 } else { (rect.right() - 12.0 - (rect.left() + name_left)).max(60.0) };
    job.wrap.max_width = max_name_w;
    job.wrap.max_rows = 1;
    let name_plain = TextFormat { color: if hit.hidden { tokens.text_faint } else { formats.plain.color }, ..formats.plain.clone() };
    apply_matches(&mut job, &hit.label, &hit.name_matches, &name_plain, &formats.high);
    let galley = ui.painter().layout_job(job);
    let pos = rect.left_center() + vec2(name_left, -galley.size().y / 2.0);
    ui.painter().galley(pos, galley, name_plain.color);
    if let Some(text) = hit.text.as_ref() {
        draw_snippet(ui, formats, rect, text, &hit.text_matches);
    }
    if response.clicked() {
        *chosen = Some((hit.clone(), false));
    }
}

/// Paint the highlighted snippet of a text-layer hit, to the right of the layer name.
fn draw_snippet(ui: &mut egui::Ui, formats: &Formats, rect: egui::Rect, text: &str, matches: &[usize]) {
    let text_left = ROW_PAD + DOC_BADGE_W + TYPE_BADGE_W + NAME_COL_W;
    if text_left >= rect.right() - 12.0 {
        return;
    }
    let max_width = (rect.right() - 12.0 - text_left).max(60.0);
    let (snippet, start) = snippet_around(text, matches, SNIPPET_RADIUS);
    let relative = relative_matches(&snippet, matches, start);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_width;
    apply_matches(&mut job, &snippet, &relative, &formats.tplain, &formats.thigh);
    let galley = ui.painter().layout_job(job);
    let pos = rect.left_center() + vec2(text_left, -galley.size().y / 2.0);
    ui.painter().galley(pos, galley, formats.tplain.color);
}

/// Append `text` to `job`, splitting it so matched byte ranges use `high` and the rest `plain`.
fn apply_matches(job: &mut egui::text::LayoutJob, text: &str, matches: &[usize], plain: &TextFormat, high: &TextFormat) {
    if matches.is_empty() {
        if !text.is_empty() {
            job.append(text, 0.0, plain.clone());
        }
        return;
    }
    let mut cursor = 0usize;
    for &m in matches {
        if m < cursor || m > text.len() || !text.is_char_boundary(m) {
            continue;
        }
        if m > cursor {
            job.append(&text[cursor..m], 0.0, plain.clone());
        }
        let next = text[m..].char_indices().nth(1).map(|(b, _)| m + b).unwrap_or(text.len());
        job.append(&text[m..next], 0.0, high.clone());
        cursor = next;
    }
    if cursor < text.len() {
        job.append(&text[cursor..], 0.0, plain.clone());
    }
}

/// A snippet of `text` around the first matched char, plus its byte offset in `text`.
fn snippet_around(text: &str, matches: &[usize], radius: usize) -> (String, usize) {
    let first = match matches.first() {
        Some(&m) => m,
        None => return (text.to_string(), 0),
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mid = chars.iter().position(|(b, _)| *b == first).unwrap_or(0);
    let start = mid.saturating_sub(radius);
    let end = (mid + radius + 1).min(chars.len());
    let start_byte = chars.get(start).map(|(b, _)| *b).unwrap_or(0);
    let snippet: String = chars[start..end].iter().map(|(_, c)| *c).collect();
    (snippet, start_byte)
}

/// Translate absolute byte offsets in `text` to offsets in `snippet`.
fn relative_matches(snippet: &str, matches: &[usize], start: usize) -> Vec<usize> {
    let end = start + snippet.len();
    matches.iter().copied().filter(|&m| m >= start && m <= end).map(|m| m - start).collect()
}

#[cfg(test)]
mod tests {
    use photocraft_doc::{ColorMode, Document, Layer, LayerContent, SampleType, Size};

    use super::{Hit, Query, SearchDoc, build_activation, collect_hits, fuzzy_score, layout_variants};
    use crate::BridgeDocId;

    fn sample_doc(name: &str) -> Document {
        Document::new(name, Size::new(100, 80), ColorMode::Rgb, SampleType::U8)
    }

    #[test]
    fn query_parse_peels_prefix_flags_in_any_order() {
        let q = Query::parse("  @#!  Bubble  ");
        assert!(q.text_only);
        assert!(q.groups_only);
        assert!(q.hidden_only);
        assert_eq!(q.body, "bubble");

        let q = Query::parse("#@bubble");
        assert!(q.text_only);
        assert!(q.groups_only);
        assert!(!q.hidden_only);
        assert_eq!(q.body, "bubble");

        let q = Query::parse("   ");
        assert!(!q.text_only && !q.groups_only && !q.hidden_only);
        assert_eq!(q.body, "");
    }

    #[test]
    fn layout_variants_remap_qwerty_and_jcuken() {
        assert_eq!(layout_variants("ghbdtn"), vec!["привет".to_owned()]);
        assert_eq!(layout_variants("привет"), vec!["ghbdtn".to_owned()]);
        assert!(layout_variants("12345").is_empty());
        let mixed = layout_variants("ghbdtn bubble");
        assert!(mixed.iter().any(|v| v.contains("привет")));
    }

    #[test]
    fn fuzzy_score_matches_are_case_insensitive_and_reward_contiguity() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("ab", "ab"), Some(14));
        assert_eq!(fuzzy_score("AB", "aXb"), Some(7));
        assert_eq!(fuzzy_score("zz", "abc"), None);
    }

    #[test]
    fn collect_hits_from_docs_sorts_and_filters() {
        let mut first = sample_doc("01.png");
        let mut second = sample_doc("02.png");
        let first_format = first.pixel_format();
        let second_format = second.pixel_format();
        first.layers.push(Layer::raster("Bubble", first_format));
        second.layers.push(Layer::raster("Bubble", second_format));
        second.layers.push(Layer::raster("Sparkle", second_format));
        second.layers.push(Layer::raster("привет", second_format));
        let docs = [
            SearchDoc { id: BridgeDocId::default(), index: 0, name: "01.png", active: true, doc: &first },
            SearchDoc { id: BridgeDocId::default(), index: 1, name: "02.png", active: false, doc: &second },
        ];

        let hits = collect_hits(&docs, &Query::parse("bubble"));
        assert_eq!(hits.len(), 2);
        assert!(hits[0].active_doc);
        assert_eq!(hits[0].doc, 0);
        assert_eq!(hits[1].doc, 1);
        assert_eq!(hits[0].badge, "[Px]");

        let translit = collect_hits(&docs, &Query::parse("ghbdtn"));
        assert_eq!(translit.len(), 1);
        assert_eq!(translit[0].label, "привет");

        assert!(collect_hits(&docs, &Query::parse("!bubble")).is_empty());
        assert!(collect_hits(&docs, &Query::parse("#bubble")).is_empty());
    }

    #[test]
    fn build_activation_reports_groups_bounds_and_identity() {
        let mut doc = sample_doc("p.png");
        let format = doc.pixel_format();
        let child = Layer::raster("Tone", format);
        let child_id = child.id;
        let mut group = Layer::group("Folds", vec![child]);
        let group_id = group.id;
        if let LayerContent::Group(g) = &mut group.content {
            g.expanded = false;
        }
        doc.layers.push(group);

        let id = BridgeDocId::default();
        let docs = [SearchDoc { id, index: 3, name: "p.png", active: false, doc: &doc }];
        let hit = Hit {
            doc: 0,
            doc_name: "p.png".to_owned(),
            layer: child_id,
            label: "Tone".to_owned(),
            text: None,
            badge: "[Px]",
            hidden: false,
            active_doc: false,
            name_matches: Vec::new(),
            text_matches: Vec::new(),
            score: 0,
        };

        let activation = match build_activation(&docs, &hit) {
            Some(activation) => activation,
            None => panic!("the hit's document and layer exist"),
        };
        assert_eq!(activation.doc_id, id);
        assert_eq!(activation.doc_index, 3);
        assert_eq!(activation.layer, child_id);
        assert_eq!(activation.groups_to_expand, vec![group_id]);
        assert_eq!(activation.center, [50.0, 40.0]);

        let missing = Hit { doc: 9, ..hit };
        assert!(build_activation(&docs, &missing).is_none());
    }
}
