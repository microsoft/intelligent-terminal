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

    fn locale_value(path: &Path, key: &str) -> Option<String> {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        body.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}: ")))
            .and_then(|value| value.split("  #").next())
            .map(|value| value.trim_matches('"').to_string())
    }

    fn collect_rust_files(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read Rust source directory") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                collect_rust_files(&path, files);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    fn literal_translation_keys(body: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        let mut search_from = 0;
        while let Some(relative_offset) = body[search_from..].find("t!(\"") {
            let macro_offset = search_from + relative_offset;
            let preceded_by_identifier = body[..macro_offset]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_');
            let value_start = macro_offset + 4;
            let Some(relative_end) = body[value_start..].find('"') else {
                break;
            };
            let candidate = &body[value_start..value_start + relative_end];
            if !preceded_by_identifier
                && !candidate.is_empty()
                && candidate.chars().all(|character| {
                    character.is_ascii_alphanumeric() || character == '_' || character == '.'
                })
            {
                keys.insert(candidate.to_string());
            }
            search_from = value_start + relative_end + 1;
        }
        keys
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
    fn every_literal_translation_key_used_by_rust_exists_in_en_us() {
        let catalog = keys_of(&locales_dir().join("en-US.yml"));
        let mut source_files = Vec::new();
        collect_rust_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut source_files,
        );

        let mut missing = Vec::new();
        for path in source_files {
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
            for key in literal_translation_keys(&body) {
                if !catalog.contains(&key) {
                    missing.push(format!("{}: {key}", path.display()));
                }
            }
        }

        assert!(
            missing.is_empty(),
            "literal t!() keys missing from en-US.yml:\n{}",
            missing.join("\n")
        );
    }

    #[test]
    fn setup_install_localizations_preserve_runtime_and_locked_tokens() {
        let mut failures = Vec::new();
        for entry in std::fs::read_dir(locales_dir()).expect("read locales dir") {
            let path = entry.expect("locale entry").path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("yml") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy();
            let connection_failed =
                locale_value(&path, "setup.subtitle.connection_failed").unwrap_or_default();
            let installing =
                locale_value(&path, "setup.title.installing_copilot").unwrap_or_default();
            let starting = locale_value(&path, "setup.title.starting_copilot").unwrap_or_default();
            let installing_cli =
                locale_value(&path, "setup.status.installing_copilot_cli").unwrap_or_default();
            let detection_timed_out =
                locale_value(&path, "setup.error.install_detection_timed_out").unwrap_or_default();

            if !connection_failed.contains("%{agent}")
                || !installing.contains("GitHub Copilot")
                || !starting.contains("GitHub Copilot")
                || !installing_cli.contains("GitHub Copilot")
                || !installing_cli.contains("CLI")
                || !detection_timed_out.contains("Copilot")
            {
                failures.push(name.to_string());
            }
        }

        assert!(
            failures.is_empty(),
            "setup install localization lost a placeholder or locked token in: {}",
            failures.join(", ")
        );
    }

    #[test]
    fn provider_command_policy_values_drop_yolo_preserve_placeholder_and_are_not_mojibake() {
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
            let contains_legacy_yolo_token = value
                .split(|character: char| !character.is_ascii_alphanumeric())
                .any(|token| token == "Yolo");
            if !value.contains("%{command}")
                || contains_legacy_yolo_token
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
