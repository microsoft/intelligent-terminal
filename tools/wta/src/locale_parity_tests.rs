//! Locale key-parity test for the WTA rust-i18n catalog.
//!
//! Convention (see `.github/instructions/rust-localization.instructions.md`):
//! every key defined in `tools/wta/locales/en-US.yml` (the source of truth)
//! must also exist in **every** other `tools/wta/locales/*.yml` file. A key
//! missing from a locale is a localization bug — at runtime rust-i18n would
//! fall back to the key string itself, surfacing a raw `commands.fix.summary`
//! to the user in that language.
//!
//! This guards the gap that shipped once already: all seven
//! `commands.*.summary` strings (the slash-command descriptions) were missing
//! from the translated locales while present in en-US.
//!
//! The check is intentionally dependency-free (no YAML crate): the locale
//! files are flat `dotted.key: "value"` pairs with no block scalars, so a
//! line scan that takes the token before the first `:` is exact. If a future
//! edit introduces multi-line YAML values, switch this to a real YAML parser.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn locales_dir() -> PathBuf {
        // CARGO_MANIFEST_DIR is the crate root (tools/wta) at both compile and
        // test time, so the on-disk source locales resolve regardless of cwd.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("locales")
    }

    /// Extract the set of top-level dotted keys from a locale file. Skips blank
    /// lines and `#` comments; a key is the run of `[A-Za-z0-9_.]` before the
    /// first `:` on a line (value text after the colon is ignored, so colons
    /// inside quoted values don't matter).
    fn keys_of(path: &Path) -> BTreeSet<String> {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        let mut keys = BTreeSet::new();
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some(colon) = trimmed.find(':') else {
                continue;
            };
            let candidate = &trimmed[..colon];
            if !candidate.is_empty()
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            {
                keys.insert(candidate.to_string());
            }
        }
        keys
    }

    fn provider_command_policy_value(path: &Path) -> Option<String> {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        body.lines()
            .find_map(|line| line.strip_prefix("system.provider_command_blocked_by_policy: "))
            .and_then(|value| value.split("  #").next())
            .map(|value| value.trim_matches('"').to_string())
    }

    #[test]
    fn every_locale_has_all_en_us_keys() {
        let dir = locales_dir();
        let en_us = dir.join("en-US.yml");
        assert!(en_us.exists(), "en-US.yml not found at {}", en_us.display());

        let base = keys_of(&en_us);
        assert!(
            base.len() > 50,
            "en-US.yml parsed only {} keys — the scanner is likely broken",
            base.len()
        );

        let mut locale_count = 0usize;
        let mut failures: Vec<String> = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("read locales dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("yml") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "en-US.yml" {
                continue;
            }
            locale_count += 1;

            let keys = keys_of(&path);
            let missing: Vec<&str> = base
                .iter()
                .filter(|k| !keys.contains(*k))
                .map(|s| s.as_str())
                .collect();
            if !missing.is_empty() {
                failures.push(format!(
                    "  {name}: missing {} -> {}",
                    missing.len(),
                    missing.join(", ")
                ));
            }
        }

        assert!(
            locale_count > 0,
            "no non-en-US locale files found in {}",
            dir.display()
        );

        assert!(
            failures.is_empty(),
            "{} locale file(s) are missing en-US keys (every en-US key must be \
             present in every locale — translate the value or seed the English \
             string):\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn provider_command_policy_values_preserve_tokens_and_are_not_mojibake() {
        let dir = locales_dir();
        let mut failures = Vec::new();

        for entry in std::fs::read_dir(&dir).expect("read locales dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("yml") {
                continue;
            }
            let Some(value) = provider_command_policy_value(&path) else {
                continue;
            };
            let contains_yolo_token = value
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| token == "Yolo");
            if !value.contains("%{command}")
                || !contains_yolo_token
                || value
                    .chars()
                    .any(|character| ('\u{2500}'..='\u{259f}').contains(&character))
                || value.contains("ΓÇ")
                || value.contains("ßâ")
            {
                failures.push(path.file_name().unwrap().to_string_lossy().to_string());
            }
        }

        assert!(
            failures.is_empty(),
            "provider-command policy text has missing locked tokens or mojibake in: {}",
            failures.join(", ")
        );
    }
}
