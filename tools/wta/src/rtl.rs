// Right-to-left (RTL) language helpers for the wta TUI.
//
// Design notes
// ------------
// ratatui draws monospaced cells left-to-right; it has no native bidi
// engine. The terminal emulator hosting wta (Windows Terminal since
// 1.21 / preview) is what actually performs bidi shaping on the
// characters we emit. So for wta the "natural" RTL fix is *not* to
// reorder runs ourselves — it is to:
//
//   1. detect that the current `rust_i18n` locale is RTL, and
//   2. right-align the `Paragraph` widgets that render translated,
//      user-facing prose (auth view, setup screen, permission body,
//      recommendations hint, agents view header, chat lines).
//
// Status bars, fixed-width hint rows, and command tokens stay left-
// aligned: they are visually anchored elements where mirroring would
// just confuse non-RTL readers of mixed-language UIs. Keeping the
// change to translated prose mirrors how WT itself ships its FRE — the
// XAML FlowDirection cascade flips the layout, but the few intentionally
// LTR controls keep their explicit alignment.
//
// Test coverage lives at the bottom of this file. It is deliberately
// pure — every test exercises `is_rtl_locale` / `text_alignment_for_locale`
// directly with explicit locale arguments, so nothing here touches
// `rust_i18n::set_locale`. That makes the suite race-free under
// `cargo test`'s default parallel runner.

use ratatui::layout::Alignment;

/// BCP-47 primary language subtags whose scripts are written
/// right-to-left. Matching is case-insensitive against the bit of the
/// locale before the first `-`.
///
/// Mirrors `Microsoft::Terminal::RtlHelper::kRtlLanguageSubtags` on the
/// C++ side (`src/cascadia/inc/RtlHelper.h`) so FRE and wta agree on
/// what counts as RTL.
const RTL_LANGUAGE_SUBTAGS: &[&str] = &[
    "ar",  // Arabic
    "he",  // Hebrew
    "iw",  // Hebrew (legacy ISO-639-1 code)
    "fa",  // Persian / Farsi
    "ur",  // Urdu
    "ug",  // Uyghur
    "ps",  // Pashto
    "sd",  // Sindhi (Perso-Arabic)
    "ckb", // Central Kurdish (Sorani)
    "yi",  // Yiddish
    "dv",  // Divehi / Maldivian
];

/// Returns `true` if `locale` is a BCP-47 tag whose primary language
/// subtag is written right-to-left. Matching is case-insensitive.
/// Empty strings, malformed input, and unknown languages all yield
/// `false` (i.e. the safe LTR default).
///
/// Also recognizes Microsoft's pseudo-mirrored pseudo-locale
/// `qps-plocm` (used by localization engineers to validate RTL plumbing
/// without a real RTL build).
pub fn is_rtl_locale(locale: &str) -> bool {
    if locale.is_empty() {
        return false;
    }

    // Pseudo-mirrored pseudo-locale — `qps` is the pseudo-language
    // prefix, so we match the whole tag rather than just the primary
    // subtag (which would falsely flag `qps-ploc` / `qps-ploca`).
    if locale.eq_ignore_ascii_case("qps-plocm") {
        return true;
    }

    let primary = match locale.split_once('-') {
        Some((head, _)) => head,
        None => locale,
    };
    if primary.is_empty() {
        return false;
    }

    RTL_LANGUAGE_SUBTAGS
        .iter()
        .any(|tag| primary.eq_ignore_ascii_case(tag))
}

/// Returns `Alignment::Right` when `locale` is RTL, otherwise
/// `Alignment::Left`. Pure helper used by the global-state wrapper
/// below; exposed separately so tests don't have to touch
/// `rust_i18n::set_locale`.
pub fn text_alignment_for_locale(locale: &str) -> Alignment {
    if is_rtl_locale(locale) {
        Alignment::Right
    } else {
        Alignment::Left
    }
}

/// Returns the default text alignment for the *current* `rust_i18n`
/// locale: `Alignment::Right` when the locale is RTL, otherwise
/// `Alignment::Left`.
///
/// Call sites that render user-facing translated prose should use this
/// to set Paragraph alignment. Fixed-width status / token rows can stay
/// left-aligned regardless.
pub fn text_alignment() -> Alignment {
    text_alignment_for_locale(&rust_i18n::locale())
}

/// Returns `true` if the *current* `rust_i18n` locale is RTL. Thin
/// wrapper for call sites that need the boolean (e.g. to swap layout
/// children) without taking a dependency on ratatui types.
pub fn is_current_locale_rtl() -> bool {
    is_rtl_locale(&rust_i18n::locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_is_ltr() {
        assert!(!is_rtl_locale(""));
    }

    #[test]
    fn malformed_tags_are_ltr() {
        assert!(!is_rtl_locale("-"));
        assert!(!is_rtl_locale("-ar"));
        assert!(!is_rtl_locale("-bogus"));
    }

    #[test]
    fn english_variants_are_ltr() {
        assert!(!is_rtl_locale("en"));
        assert!(!is_rtl_locale("en-US"));
        assert!(!is_rtl_locale("en-GB"));
    }

    #[test]
    fn common_ltr_languages_are_ltr() {
        for tag in [
            "de-DE", "fr-FR", "ja-JP", "zh-CN", "zh-TW", "ru-RU", "ko-KR", "es-ES", "hi-IN",
            "pt-BR", "it-IT",
        ] {
            assert!(!is_rtl_locale(tag), "{tag} should be LTR");
        }
    }

    #[test]
    fn arabic_variants_are_rtl() {
        assert!(is_rtl_locale("ar"));
        assert!(is_rtl_locale("ar-SA"));
        assert!(is_rtl_locale("ar-EG"));
        assert!(is_rtl_locale("ar-AE"));
    }

    #[test]
    fn hebrew_variants_are_rtl() {
        assert!(is_rtl_locale("he"));
        assert!(is_rtl_locale("he-IL"));
        // Legacy ISO-639-1 code for Hebrew.
        assert!(is_rtl_locale("iw"));
        assert!(is_rtl_locale("iw-IL"));
    }

    #[test]
    fn persian_is_rtl() {
        assert!(is_rtl_locale("fa"));
        assert!(is_rtl_locale("fa-IR"));
    }

    #[test]
    fn urdu_is_rtl() {
        assert!(is_rtl_locale("ur"));
        assert!(is_rtl_locale("ur-PK"));
    }

    #[test]
    fn uyghur_is_rtl() {
        assert!(is_rtl_locale("ug"));
        assert!(is_rtl_locale("ug-CN"));
    }

    #[test]
    fn other_rtl_scripts_are_rtl() {
        assert!(is_rtl_locale("ps-AF"));
        assert!(is_rtl_locale("sd-PK"));
        assert!(is_rtl_locale("ckb-IQ"));
        assert!(is_rtl_locale("yi"));
        assert!(is_rtl_locale("dv-MV"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(is_rtl_locale("AR"));
        assert!(is_rtl_locale("AR-sa"));
        assert!(is_rtl_locale("Ar-Sa"));
        assert!(is_rtl_locale("HE-IL"));
        assert!(!is_rtl_locale("EN-us"));
    }

    #[test]
    fn pseudo_mirrored_is_rtl() {
        // qps-plocm is the canonical RTL pseudo-locale.
        assert!(is_rtl_locale("qps-plocm"));
        assert!(is_rtl_locale("QPS-PLOCM"));
        assert!(is_rtl_locale("Qps-Plocm"));
    }

    #[test]
    fn pseudo_ltr_pseudo_locales_are_ltr() {
        // qps-ploc and qps-ploca accent + pad but do not mirror.
        assert!(!is_rtl_locale("qps-ploc"));
        assert!(!is_rtl_locale("qps-ploca"));
    }

    #[test]
    fn subtag_prefix_only_matches_primary() {
        // Tags that *start with* an RTL prefix but aren't themselves
        // RTL must not match. Guards against a naive `starts_with`
        // implementation.
        assert!(!is_rtl_locale("aru"));
        assert!(!is_rtl_locale("arn-CL"));
        assert!(!is_rtl_locale("her"));
    }

    #[test]
    fn bare_language_without_region_works() {
        assert!(is_rtl_locale("ar"));
        assert!(is_rtl_locale("he"));
        assert!(!is_rtl_locale("en"));
        assert!(!is_rtl_locale("de"));
    }

    #[test]
    fn text_alignment_for_locale_maps_correctly() {
        // Pure helper — no rust_i18n global state poked. Race-free
        // under cargo test's default parallel runner.
        assert_eq!(text_alignment_for_locale("ar-SA"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("he-IL"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("fa-IR"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("ur-PK"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("ug-CN"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("qps-plocm"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("en-US"), Alignment::Left);
        assert_eq!(text_alignment_for_locale("de-DE"), Alignment::Left);
        assert_eq!(text_alignment_for_locale("ja-JP"), Alignment::Left);
        assert_eq!(text_alignment_for_locale(""), Alignment::Left);
    }
}
