// Right-to-left (RTL) language helpers for the `wta` TUI.
//
// We delegate classification to the Windows locale database via the
// Win32 reading-layout locale-info API instead of carrying our own
// language list. The C++ side (`RtlHelper.h`) uses the same Win32
// API — both stacks ask the same OS for the answer, so FRE and `wta`
// agree by construction.
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
// shaping. So the only useful WTA-side action is choosing a default
// UI text alignment for RTL locales — right-aligning the relevant
// `Paragraph` widgets, regardless of whether their content is purely
// translated copy or mixed (user / agent messages, code, etc.). The
// terminal handles the rest.

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
    use std::ffi::c_void;
    use windows_sys::Win32::Globalization::{EnumSystemLocalesEx, LOCALE_WINDOWS};

    /// Ask the OS for the reading-layout value via a *different* code
    /// path than the helper uses, so a flag / buffer-sizing mistake in
    /// `is_rtl_locale` can't mask itself when the test compares
    /// expected vs. actual. The helper uses `LOCALE_RETURN_NUMBER` to
    /// read the value as a binary `u32`; here we deliberately omit
    /// that flag so the OS returns the value as a decimal string
    /// ("0" .. "3"), which we then parse. Independent path, same
    /// underlying classifier.
    fn os_says_rtl(locale: &str) -> bool {
        let wide: Vec<u16> = locale.encode_utf16().chain(std::iter::once(0)).collect();
        let mut buf = [0u16; 8];
        let n = unsafe {
            GetLocaleInfoEx(
                wide.as_ptr(),
                READING_LAYOUT_INFO,
                buf.as_mut_ptr(),
                buf.len() as i32,
            )
        };
        if n <= 1 {
            return false;
        }
        // `n` includes the terminating null; the digit is at buf[0].
        let digit = char::from_u32(buf[0] as u32).unwrap_or('\0');
        digit == '1'
    }

    /// Collect every BCP-47 tag the OS knows about. We don't carry our
    /// own list — `EnumSystemLocalesEx` is the list, this is the loop.
    fn enumerate_installed_locales() -> Vec<String> {
        // Callback writes each name into the `Vec` whose pointer is
        // passed through `lParam`. `PCWSTR` is the read-only pointer
        // shape used by Win32 string-out callbacks.
        unsafe extern "system" fn cb(name: *const u16, _flags: u32, lparam: isize) -> i32 {
            let len = (0..).take_while(|&i| unsafe { *name.add(i) } != 0).count();
            let slice = unsafe { std::slice::from_raw_parts(name, len) };
            let v = unsafe { &mut *(lparam as *mut Vec<String>) };
            v.push(String::from_utf16_lossy(slice));
            1 // TRUE — keep enumerating
        }

        let mut locales: Vec<String> = Vec::new();
        let lparam = &mut locales as *mut Vec<String> as isize;
        let ok = unsafe {
            EnumSystemLocalesEx(
                Some(cb),
                LOCALE_WINDOWS,
                lparam,
                std::ptr::null::<c_void>(),
            )
        };
        assert!(ok != 0, "EnumSystemLocalesEx failed");
        locales
    }

    #[test]
    fn empty_string_is_ltr() {
        assert!(!is_rtl_locale(""));
    }

    #[test]
    fn en_us_is_ltr() {
        // Smoke-test anchor: en-US is the universal LTR baseline.
        // Catches a regression where the helper inverts its result or
        // returns true on success unconditionally, without relying on
        // OS enumeration to find any LTR locale to compare against.
        assert!(!os_says_rtl("en-US"));
        assert!(!is_rtl_locale("en-US"));
    }

    #[test]
    fn malformed_tags_are_ltr() {
        assert!(!is_rtl_locale("-"));
        assert!(!is_rtl_locale("-ar"));
        assert!(!is_rtl_locale("not a tag"));
        assert!(!is_rtl_locale("!!!"));
    }

    #[test]
    fn matches_os_classification_for_every_installed_locale() {
        // Enumerate every locale the OS knows about (no hardcoded
        // list) and assert the helper agrees with the OS for each.
        // If the OS ever ships a new RTL locale we automatically
        // cover it without touching this test.
        let locales = enumerate_installed_locales();
        assert!(
            !locales.is_empty(),
            "EnumSystemLocalesEx returned no locales; expected at least one"
        );
        let mut mismatches: Vec<String> = Vec::new();
        for tag in &locales {
            let expected = os_says_rtl(tag);
            let actual = is_rtl_locale(tag);
            if expected != actual {
                mismatches.push(format!("{tag}: helper={actual}, os={expected}"));
            }
        }
        assert!(
            mismatches.is_empty(),
            "Helper disagrees with OS for: {mismatches:?}"
        );
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
        // internally. Confirm we pass that through using one RTL and
        // one LTR pseudo-locale (avoids hardcoding any real language).
        assert_eq!(is_rtl_locale("qps-plocm"), is_rtl_locale("QPS-PLOCM"));
        assert_eq!(is_rtl_locale("qps-ploc"), is_rtl_locale("QPS-PLOC"));
    }

    #[test]
    fn text_alignment_for_locale_maps_correctly() {
        // Two known anchors from the OS pseudo-locale set — no
        // hardcoded "language X is RTL" knowledge.
        assert_eq!(text_alignment_for_locale("qps-plocm"), Alignment::Right);
        assert_eq!(text_alignment_for_locale("qps-ploc"), Alignment::Left);
        assert_eq!(text_alignment_for_locale(""), Alignment::Left);
    }
}
