//! Theme tokens mirroring `photocraft-ui-egui::theme::Tokens`.
//!
//! `ui-egui` calls [`Tokens::install`] once per frame before drawing `manga-ui` panels/gizmos so
//! every manga control follows the user's active theme (Pro Dark/Medium/Light or Studio) without
//! coupling `manga-ui` to `ui-egui`.

use egui::Color32;

const TOKENS_KEY: &str = "photocraft-manga-ui-theme-tokens";

/// Colour and spacing tokens used by `manga-ui`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub chrome: Color32,
    pub canvas: Color32,
    pub dock: Color32,
    pub card: Color32,
    pub card_border: Color32,
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub accent_border: Color32,
    pub accent_text: Color32,
    pub separator: Color32,
    pub shadow: Color32,
    pub primary_bg: Color32,
    pub primary_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
    pub bevel: bool,
    pub pro: bool,
    pub tab_strip: Color32,
    pub row_selected: Color32,
}

impl Default for Tokens {
    fn default() -> Self {
        Self {
            chrome: Color32::from_rgb(83, 83, 83),
            canvas: Color32::from_rgb(40, 40, 40),
            dock: Color32::from_rgb(66, 66, 66),
            card: Color32::from_rgb(83, 83, 83),
            card_border: Color32::from_rgb(66, 66, 66),
            field: Color32::from_rgb(69, 69, 69),
            field_border: Color32::from_rgb(104, 104, 104),
            hover: Color32::from_rgb(98, 98, 98),
            pressed: Color32::from_rgb(112, 112, 112),
            text: Color32::from_rgb(238, 238, 238),
            text_dim: Color32::from_rgb(212, 212, 212),
            text_faint: Color32::from_rgb(160, 160, 160),
            icon: Color32::from_rgb(226, 226, 226),
            accent: Color32::from_rgb(38, 128, 235),
            accent_soft: Color32::from_rgb(110, 110, 110),
            accent_border: Color32::from_rgb(64, 152, 250),
            accent_text: Color32::WHITE,
            separator: Color32::from_rgb(62, 62, 62),
            shadow: Color32::from_black_alpha(96),
            primary_bg: Color32::from_rgb(38, 128, 235),
            primary_text: Color32::WHITE,
            danger: Color32::from_rgb(220, 76, 70),
            warning: Color32::from_rgb(224, 160, 48),
            radius_sm: 3.0,
            radius: 4.0,
            radius_lg: 6.0,
            bevel: false,
            pro: true,
            tab_strip: Color32::from_rgb(66, 66, 66),
            row_selected: Color32::from_rgb(107, 107, 107),
        }
    }
}

impl Tokens {
    /// Reads the host-installed theme tokens from `ctx`, falling back to Pro Medium defaults.
    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(egui::Id::unique(TOKENS_KEY))).unwrap_or_default()
    }

    /// Stores `tokens` on `ctx` for the current frame.
    pub fn install(ctx: &egui::Context, tokens: Self) {
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(TOKENS_KEY), tokens));
    }

    /// Contrast outline colours for canvas overlays/cursors.
    pub fn cursor_outline() -> [Color32; 2] {
        [Color32::from_black_alpha(160), Color32::from_white_alpha(235)]
    }
}
