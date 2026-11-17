//! Local translation catalogs for the manga UI suitcase (`crates/manga-ui`).
//!
//! Uses `HostSnapshot.locale` synced per frame via [`set_locale`] / [`with_locale`] so that
//! `manga-ui` does not need to fork the external `storytold/craft-i18n` crate.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

struct Catalog {
    plain: HashMap<&'static str, &'static str>,
    ctx: HashMap<(&'static str, &'static str), &'static str>,
    plural: HashMap<&'static str, &'static str>,
}

fn unescape(raw: &'static str) -> &'static str {
    if !raw.contains('\\') {
        return raw;
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Box::leak(out.into_boxed_str())
}

fn parse_catalog(tsv: &'static str) -> Catalog {
    let mut plain = HashMap::new();
    let mut ctx = HashMap::new();
    let mut plural = HashMap::new();
    for line in tsv.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, '\t');
        let (Some(c), Some(src), Some(tr)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let src = unescape(src);
        let tr = unescape(tr);
        if c == "@plural" {
            plural.insert(src, tr);
        } else if c.is_empty() {
            plain.insert(src, tr);
        } else {
            ctx.insert((c, src), tr);
        }
    }
    Catalog { plain, ctx, plural }
}

const RAW_CATALOGS: &[(&str, &str)] = &[
    ("cs", include_str!("i18n/cs.tsv")),
    ("de", include_str!("i18n/de.tsv")),
    ("el", include_str!("i18n/el.tsv")),
    ("es", include_str!("i18n/es.tsv")),
    ("fr", include_str!("i18n/fr.tsv")),
    ("hi", include_str!("i18n/hi.tsv")),
    ("hu", include_str!("i18n/hu.tsv")),
    ("id", include_str!("i18n/id.tsv")),
    ("it", include_str!("i18n/it.tsv")),
    ("ja", include_str!("i18n/ja.tsv")),
    ("ko", include_str!("i18n/ko.tsv")),
    ("nl", include_str!("i18n/nl.tsv")),
    ("pl", include_str!("i18n/pl.tsv")),
    ("pt-br", include_str!("i18n/pt-br.tsv")),
    ("ru", include_str!("i18n/ru.tsv")),
    ("sr", include_str!("i18n/sr.tsv")),
    ("tr", include_str!("i18n/tr.tsv")),
    ("uk", include_str!("i18n/uk.tsv")),
    ("vi", include_str!("i18n/vi.tsv")),
    ("zh-hans", include_str!("i18n/zh-hans.tsv")),
    ("zh-hant", include_str!("i18n/zh-hant.tsv")),
];

static CATALOGS: LazyLock<HashMap<&'static str, Catalog>> = LazyLock::new(|| RAW_CATALOGS.iter().map(|&(code, raw)| (code, parse_catalog(raw))).collect());

thread_local! {
    static CURRENT_LOCALE: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sets the thread-local active locale used by [`t`], [`tr_ctx`], and [`trn`].
pub fn set_locale(locale: &str) {
    CURRENT_LOCALE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.clear();
        s.push_str(&locale.to_ascii_lowercase());
    });
}

/// Executes `f` with `locale` active, then restores the previous locale.
pub fn with_locale<R>(locale: &str, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT_LOCALE.with(|cell| cell.borrow().clone());
    set_locale(locale);
    let out = f();
    set_locale(&prev);
    out
}

/// Returns the active locale code (defaults to `"en"` when unset).
pub fn current_code() -> String {
    CURRENT_LOCALE.with(|cell| {
        let s = cell.borrow();
        if s.is_empty() { "en".to_owned() } else { s.clone() }
    })
}

fn catalog_for(locale: &str) -> Option<&'static Catalog> {
    if locale.is_empty() || locale.eq_ignore_ascii_case("en") {
        return None;
    }
    let lower = locale.to_ascii_lowercase();
    CATALOGS.get(lower.as_str()).or_else(|| {
        let base = lower.split(['-', '_']).next()?;
        CATALOGS.get(base)
    })
}

/// Translates `key` in the specified `locale`, falling back to English `key`.
pub fn tr_in<'a>(locale: &str, key: &'a str) -> &'a str {
    catalog_for(locale).and_then(|cat| cat.plain.get(key).copied()).unwrap_or(key)
}

/// Translates `key` using the current active locale.
pub fn t(key: &str) -> &str {
    CURRENT_LOCALE.with(|cell| tr_in(&cell.borrow(), key))
}

/// Context-disambiguated translation lookup.
pub fn tr_ctx<'a>(ctx: &str, key: &'a str) -> &'a str {
    CURRENT_LOCALE.with(|cell| {
        let loc = cell.borrow();
        let Some(cat) = catalog_for(&loc) else {
            return key;
        };
        cat.ctx.get(&(ctx, key)).copied().or_else(|| cat.plain.get(key).copied()).unwrap_or(key)
    })
}

/// CLDR-style plural selection from `@plural` entries.
pub fn trn(count: u64, one: &str, other: &str) -> String {
    let locale = current_code();
    let idx = plural_index(&locale, count);
    if let Some(cat) = catalog_for(&locale) {
        let joined = format!("{one}|{other}");
        if let Some(&forms) = cat.plural.get(joined.as_str()) {
            let parts: Vec<&str> = forms.split('|').collect();
            if let Some(chosen) = parts.get(idx).or_else(|| parts.last()) {
                return (*chosen).to_owned();
            }
        }
    }
    if count == 1 { one.to_owned() } else { other.to_owned() }
}

fn plural_index(locale: &str, n: u64) -> usize {
    let base = locale.split(['-', '_']).next().unwrap_or(locale);
    match base {
        "ja" | "ko" | "zh" | "vi" | "id" | "hi" => 0,
        "fr" => usize::from(n > 1),
        "ru" | "uk" | "sr" => {
            let m10 = n % 10;
            let m100 = n % 100;
            if m10 == 1 && m100 != 11 {
                0
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                1
            } else {
                2
            }
        }
        "pl" => {
            let m10 = n % 10;
            let m100 = n % 100;
            if n == 1 {
                0
            } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                1
            } else {
                2
            }
        }
        "cs" => match n {
            1 => 0,
            2..=4 => 1,
            _ => 2,
        },
        _ => usize::from(n != 1),
    }
}

/// Substitutes `{name}` placeholders in `template`.
pub fn fmt(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_owned();
    for &(name, val) in vars {
        let needle = format!("{{{name}}}");
        out = out.replace(&needle, val);
    }
    out
}

/// Converts a camelCase filter/preset key to title words (`centerX` -> `Center X`).
pub fn source_label(key: &str) -> String {
    if key == "localized" {
        return "Localized Color Clusters".to_owned();
    }
    if key == "bpc" {
        return "Black Point Compensation".to_owned();
    }
    if key.contains(' ') {
        return key.to_owned();
    }
    let mut s = String::new();
    for (i, ch) in key.chars().enumerate() {
        if i == 0 {
            s.extend(ch.to_uppercase());
        } else if ch.is_uppercase() {
            s.push(' ');
            s.push(ch);
        } else {
            s.push(ch);
        }
    }
    s
}

/// Converts a parameter key to a localized human-readable label.
pub fn param_label(key: &str) -> String {
    t(&source_label(key)).to_owned()
}

/// Translates string literals and dynamic string expressions in `manga-ui`.
#[macro_export]
macro_rules! tl {
    ($s:expr) => {
        $crate::i18n::t($s)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ru_catalog_translates_manga_strings() {
        with_locale("ru", || {
            assert_ne!(t("Screentones"), "");
            assert_eq!(tr_in("en", "Screentones"), "Screentones");
            assert_ne!(tr_in("ru", "Button Mode"), "Button Mode");
        });
    }
}
