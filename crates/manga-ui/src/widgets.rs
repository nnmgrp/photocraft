//! Reusable egui / `craft_ui` controls for `crates/manga-ui`.
//!
//! Uses `craft_ui::ComboBox` everywhere (`egui::ComboBox` is banned workspace-wide by
//! `clippy.toml`).

use std::ops::RangeInclusive;

use egui::{Response, Stroke, Ui, vec2};

use crate::i18n;
use crate::theme::Tokens;

/// Draws a 1 px horizontal divider across the available panel width.
pub fn hairline(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 5.0), egui::Sense::hover());
    ui.painter().line_segment([rect.left_center(), rect.right_center()], Stroke::new(1.0, t.separator));
}

/// Draws a localized checkbox control.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    ui.checkbox(on, label)
}

/// Draws a compact numeric scrub/drag field with suffix and clamping to `range`.
pub fn value_field(ui: &mut Ui, value: &mut f32, range: RangeInclusive<f32>, suffix: &str, width: f32) -> Response {
    let speed = ((*range.end() - *range.start()).abs() / 600.0).clamp(0.02, 0.22);
    let mut drag = egui::DragValue::new(value).range(range).speed(speed).max_decimals(2);
    if !suffix.is_empty() {
        drag = drag.suffix(suffix);
    }
    ui.add_sized([width, 20.0], drag)
}

/// Draws a `craft_ui::ComboBox` dropdown translating each label via `i18n::t`.
pub fn dropdown<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    dropdown_inner(ui, id, current, options, width, true)
}

/// Draws a `craft_ui::ComboBox` dropdown keeping option labels verbatim.
pub fn dropdown_literal<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32) -> bool {
    dropdown_inner(ui, id, current, options, width, false)
}

fn dropdown_inner<T: PartialEq + Clone>(ui: &mut Ui, id: &str, current: &mut T, options: &[(T, &str)], width: f32, translate: bool) -> bool {
    let label_for = |raw: &str| -> String { if translate { i18n::t(raw).to_owned() } else { raw.to_owned() } };
    let selected = options.iter().find(|(v, _)| v == current).map(|(_, l)| label_for(l)).unwrap_or_default();
    let mut changed = false;
    craft_ui::ComboBox::from_id_salt(id).width(width).selected_text(selected).show_ui(ui, |ui| {
        for (val, text) in options {
            let shown = label_for(text);
            if ui.selectable_label(*current == *val, shown).clicked() && *current != *val {
                *current = val.clone();
                changed = true;
            }
        }
    });
    changed
}
