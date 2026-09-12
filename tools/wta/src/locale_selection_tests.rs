// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::{normalize_locale, AVAILABLE_LOCALE_NAMES};

#[test]
fn normalization_does_not_initialize_translation_backend() {
    const CHILD_ENV: &str = "WTA_TEST_LOCALE_SELECTION_CHILD";
    if std::env::var_os(CHILD_ENV).is_some() {
        assert!(rust_i18n::once_cell::sync::Lazy::get(&super::_RUST_I18N_BACKEND).is_none());
        for locale in ["en-US", "zh-Hant-HK", "fr", "xx-YY"] {
            let _ = normalize_locale(locale);
        }
        assert!(rust_i18n::once_cell::sync::Lazy::get(&super::_RUST_I18N_BACKEND).is_none());
        return;
    }

    // Other tests translate in parallel. A fresh test process proves that
    // selection alone leaves the backend cold without depending on test order.
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "locale_selection_tests::normalization_does_not_initialize_translation_backend",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run isolated locale selection test");
    assert!(
        output.status.success(),
        "isolated locale selection failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generated_names_match_translation_backend_including_order() {
    // Exercise the real backend here, never on the production selection path.
    // Ordering matters because language-only matches select the first name.
    assert_eq!(
        AVAILABLE_LOCALE_NAMES,
        rust_i18n::available_locales!().as_slice()
    );
}

#[test]
fn every_supported_locale_preserves_exact_match_and_input_case() {
    for locale in AVAILABLE_LOCALE_NAMES {
        assert_eq!(normalize_locale(locale), *locale);
        let uppercase = locale.to_ascii_uppercase();
        assert_eq!(normalize_locale(&uppercase), uppercase);
        let lowercase = locale.to_ascii_lowercase();
        assert_eq!(normalize_locale(&lowercase), lowercase);
    }
}

#[test]
fn normalize_locale_preserves_affinity_and_exact_match_precedence() {
    for (input, expected) in [
        ("zh-HK", "zh-TW"),
        ("zh-MO", "zh-TW"),
        ("zh-Hant", "zh-TW"),
        ("ZH-HANT-HK", "zh-TW"),
        ("zh-SG", "zh-CN"),
        ("zh-Hans", "zh-CN"),
        ("zh-Hans-SG", "zh-CN"),
        ("en-AU", "en-GB"),
        ("EN-IN", "en-GB"),
        ("es-AR", "es-MX"),
        ("es-419", "es-MX"),
        ("fr-BE", "fr-FR"),
        ("fr-MA", "fr-FR"),
        ("pt-AO", "pt-PT"),
        ("sr-Latn-BA", "sr-Latn-RS"),
        ("sr-Latn-XK", "sr-Latn-RS"),
        ("sr-Cyrl-ME", "sr-Cyrl-RS"),
        ("sr-Cyrl-XK", "sr-Cyrl-RS"),
        // An actual locale file wins even when an affinity mapping exists.
        ("sr-Cyrl-BA", "sr-Cyrl-BA"),
        ("SR-CYRL-BA", "SR-CYRL-BA"),
        ("fr-CA", "fr-CA"),
        ("pt-BR", "pt-BR"),
    ] {
        assert_eq!(normalize_locale(input), expected, "input: {input}");
    }
}

#[test]
fn normalize_locale_preserves_sorted_prefix_and_unsupported_fallbacks() {
    for (input, expected) in [
        ("en", "en-GB"),
        ("en-XX", "en-GB"),
        ("es", "es-ES"),
        ("fr-XX", "fr-CA"),
        ("pt", "pt-BR"),
        ("sr", "sr-Cyrl-BA"),
        ("zh", "zh-CN"),
        ("ZH-XX", "zh-CN"),
        ("ca", "ca-ES"),
        ("qps", "qps-ploc"),
        ("", "en-US"),
        ("xx-YY", "en-US"),
        ("not-a-locale", "en-US"),
        ("zh_TW", "en-US"),
        (" zh-TW", "en-US"),
    ] {
        assert_eq!(normalize_locale(input), expected, "input: {input}");
    }
}
