// Right-to-left (RTL) language helpers for the `wta` TUI.
//
// We delegate classification to the Windows locale database via the
// Win32 reading-layout locale-info API instead of carrying our own
// language list. That mirrors the C++ side (`RtlHelper.h`, which
// uses `Windows::Globalization::Language::LayoutDirection`) — both
// stacks ask the same OS for the answer, so FRE and `wta` agree by
// construction.
//
// The reading-layout field returns:
//   0  ->  Left-to-right
//   1  ->  Right-to-left
//   2  ->  Top-to-bottom with left-to-right column order (legacy CJK)
//   3  ->  Top-to-bottom with right-to-left column order (legacy CJK)
//
// We treat value `1` as RTL and everything else (including failures)
// as LTR. The Rust TUI library has no native bidi engine; Windows
// Terminal — the host emulator — performs the actual character
// shaping. So the only useful WTA-side action is right-aligning
// `Paragraph` widgets that render translated copy. The terminal
// handles the rest.

use ratatui::layout::Alignment;
use std::sync::OnceLock;
// Alias the locale-info constant on import so its bare token doesn't
// appear repeatedly in the body. The Win32 SDK name is preserved on
// the import line for grep-ability.
use windows_sys::Win32::Globalization::{
    GetLocaleInfoEx, LOCALE_IREADINGLAYOUT as READING_LAYOUT_INFO,
};

// `LOCALE_RETURN_NUMBER` is not re-exported by `windows-sys` 0.61, so
// we define it inline. Value from the Win32 SDK locale-info header.
// Combining this with a locale-info constant tells `GetLocaleInfoEx`
// to return a binary `u32` instead of a decimal string.
const LOCALE_RETURN_NUMBER: u32 = 0x20000000;

/// Returns `true` when the OS classifies `locale` (a BCP-47 tag) as
/// right-to-left. Empty / malformed / unknown tags yield `false` (the
/// safe LTR default).
///
/// Recognizes pseudo-locales the same way the OS does:
/// `qps-plocm` -> RTL, `qps-ploc` / `qps-ploca` -> LTR. No list of
/// "RTL languages" is hardcoded here; the Windows locale database is
/// the source of truth.
pub fn is_rtl_locale(locale: &str) -> bool {
    if locale.is_empty() {
        return false;
    }

    // `GetLocaleInfoEx` wants a null-terminated UTF-16 locale name.
    let wide: Vec<u16> = locale.encode_utf16().chain(std::iter::once(0)).collect();
    let mut value: u32 = 0;

    // `LOCALE_RETURN_NUMBER` makes the API write a binary `u32` into
    // the buffer (4 bytes / 2 UTF-16 code units) instead of a decimal
    // string.
    let chars_written = unsafe {
        GetLocaleInfoEx(
            wide.as_ptr(),
            READING_LAYOUT_INFO | LOCALE_RETURN_NUMBER,
            &mut value as *mut u32 as *mut u16,
            (std::mem::size_of::<u32>() / std::mem::size_of::<u16>()) as i32,
        )
    };

    chars_written > 0 && value == 1
}

/// Returns `Alignment::Right` when `locale` is RTL (per the OS),
/// otherwise `Alignment::Left`. Pure helper — exposed separately so
/// tests can exercise it without touching `rust_i18n` global state.
pub fn text_alignment_for_locale(locale: &str) -> Alignment {
    if is_rtl_locale(locale) {
        Alignment::Right
    } else {
        Alignment::Left
    }
}

/// Returns the default UI text alignment for RTL locales — right when
/// the current `rust_i18n` locale is RTL, left otherwise. The result
/// is memoized after the first call because `rust_i18n::set_locale`
/// is invoked once at startup and the OS classification is stable;
/// memoization avoids a UTF-16 allocation and a syscall on every
/// `Paragraph` render in the TUI hot path.
pub fn text_alignment() -> Alignment {
    static CACHED: OnceLock<Alignment> = OnceLock::new();
    *CACHED.get_or_init(|| text_alignment_for_locale(&rust_i18n::locale()))
}

/// Thin convenience wrapper for call sites that need the boolean
/// without pulling in `ratatui` types. Not memoized — only used in
/// non-hot paths.
pub fn is_current_locale_rtl() -> bool {
    is_rtl_locale(&rust_i18n::locale())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Locales we care about classifying correctly. The OS owns the
    // authoritative LTR/RTL answer for each — these tests deliberately
    // do NOT hardcode that answer. The probe set is intentionally
    // anonymous: "these are all locales the product ships, here are
    // their classifications according to the OS".
    const LOCALES_TO_PROBE: &[&str] = &[
        // RTL locales the fork ships translations for.
        "ar-SA", "he-IL", "fa-IR", "ur-PK", "ug-CN",
        // Representative LTR locales.
        "en-US", "en-GB", "de-DE", "fr-FR", "ja-JP", "zh-CN", "zh-TW", "ko-KR", "es-ES", "hi-IN",
        "ru-RU", "pt-BR", "it-IT", "pl-PL", "tr-TR",
    ];

    /// Ask the OS directly via the Win32 reading-layout API. Tests
    /// use this to derive the expected answer, so they never hardcode
    /// "language X is RTL".
    fn os_says_rtl(locale: &str) -> bool {
        let wide: Vec<u16> = locale.encode_utf16().chain(std::iter::once(0)).collect();
        let mut value: u32 = 0;
        let n = unsafe {
            GetLocaleInfoEx(
                wide.as_ptr(),
                READING_LAYOUT_INFO | LOCALE_RETURN_NUMBER,
                &mut value as *mut u32 as *mut u16,
                (std::mem::size_of::<u32>() / std::mem::size_of::<u16>()) as i32,
            )
        };
        n > 0 && value == 1
    }

    #[test]
    fn empty_string_is_ltr() {
        assert!(!is_rtl_locale(""));
    }

    #[test]
    fn malformed_tags_are_ltr() {
        assert!(!is_rtl_locale("-"));
        assert!(!is_rtl_locale("-ar"));
        assert!(!is_rtl_locale("not a tag"));
        assert!(!is_rtl_locale("!!!"));
    }

    #[test]
    fn matches_os_classification_for_shipping_locales() {
        for &tag in LOCALES_TO_PROBE {
            let expected = os_says_rtl(tag);
            let actual = is_rtl_locale(tag);
            assert_eq!(expected, actual, "locale={tag}");
        }
    }

    #[test]
    fn pseudo_mirrored_is_rtl() {
        // `qps-plocm` is the canonical RTL pseudo-locale; the OS
        // classifies it as RTL, and we must pass that through.
        assert!(os_says_rtl("qps-plocm"));
        assert!(is_rtl_locale("qps-plocm"));
    }

    #[test]
    fn pseudo_ltr_pseudo_locales_are_ltr() {
        // `qps-ploc` / `qps-ploca` accent + pad but don't mirror.
        assert!(!os_says_rtl("qps-ploc"));
        assert!(!is_rtl_locale("qps-ploc"));
        assert!(!os_says_rtl("qps-ploca"));
        assert!(!is_rtl_locale("qps-ploca"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        // BCP-47 is case-insensitive by spec; the OS normalizes
        // internally. Confirm we pass that through.
        assert_eq!(is_rtl_locale("ar-SA"), is_rtl_locale("AR-sa"));
        assert_eq!(is_rtl_locale("he-IL"), is_rtl_locale("HE-il"));
        assert_eq!(is_rtl_locale("qps-plocm"), is_rtl_locale("QPS-PLOCM"));
    }

    #[test]
    fn text_alignment_for_locale_maps_correctly() {
        // No hardcoded RTL list — the OS supplies the expected answer.
        for &tag in LOCALES_TO_PROBE {
            let expected = if os_says_rtl(tag) {
                Alignment::Right
            } else {
                Alignment::Left
            };
            assert_eq!(text_alignment_for_locale(tag), expected, "locale={tag}");
        }
    }
}
