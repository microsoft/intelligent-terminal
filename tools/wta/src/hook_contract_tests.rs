//! Cross-language contract test for the agent hook bridge.
//!
//! Agent CLI lifecycle hooks reach WTA through a producer that lives in a
//! different language: `BuildAgentHookEventJson` in
//! `src/tools/wtcli/wtcli_functions.h` reads the hook JSON from stdin, redacts
//! it, and publishes an `agent_event` over COM. The consumer —
//! `app::route_agent_event_to_registry_with_hook_sink` — then reads specific
//! members back out of that payload.
//!
//! The producer therefore encodes, in C++, knowledge that is owned by the Rust
//! consumer:
//!
//!   * which payload members survive redaction, and
//!   * which `tool_name` values are "the agent is asking the user something"
//!     (only those keep `tool_input`).
//!
//! Nothing in the type system, the build, or the fuzzer ties those two sides
//! together, and every failure mode is **silent**: over-redaction doesn't
//! error, it just makes a field arrive empty, so the session row quietly shows
//! a blank cwd or "waiting for user input" instead of the real question. This
//! module is the missing red light.
//!
//! Like `locale_parity_tests`, the check is intentionally dependency-free. The
//! two C++ arrays are plain lists of string literals with no macros or
//! conditional compilation, so scanning for quoted tokens between `name[] = {`
//! and `};` is exact. If that ever stops being true, the extraction panics
//! with a pointer to this comment rather than silently matching nothing.

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Path of the producer relative to the repository root, for error text.
    const HEADER_REL: &str = "src/tools/wtcli/wtcli_functions.h";

    fn header_path() -> PathBuf {
        // CARGO_MANIFEST_DIR is `<repo>/tools/wta` at both compile and test
        // time, so the producer resolves regardless of the cwd the suite runs
        // from.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("src")
            .join("tools")
            .join("wtcli")
            .join("wtcli_functions.h")
    }

    fn header_source() -> String {
        let path = header_path();
        std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "failed to read the hook producer at {} ({e}).\n\
                 This test asserts the C++ hook bridge and the Rust consumer \
                 agree; if {HEADER_REL} moved, update `header_path()`.",
                path.display()
            )
        })
    }

    /// Collect the string literals of a C++ `static constexpr const char* NAME[] = { ... };`
    /// array. Text after `//` on each line is ignored so a commented-out entry
    /// is not counted as live.
    fn cpp_string_array(source: &str, name: &str) -> BTreeSet<String> {
        let anchor = format!("{name}[] = {{");
        let start = source.find(&anchor).unwrap_or_else(|| {
            panic!(
                "could not find `{anchor}` in {HEADER_REL}.\n\
                 The hook producer was refactored. Re-point this extraction at \
                 the new shape, or replace it with whatever mechanism now keeps \
                 the producer and consumer in sync."
            )
        });
        let body_start = start + anchor.len();
        let body_len = source[body_start..]
            .find("};")
            .unwrap_or_else(|| panic!("`{name}` array in {HEADER_REL} is not terminated by `}};`"));
        let body = &source[body_start..body_start + body_len];

        let mut found = BTreeSet::new();
        for line in body.lines() {
            let code = line.split("//").next().unwrap_or("");
            let mut rest = code;
            while let Some(open) = rest.find('"') {
                let after = &rest[open + 1..];
                let Some(close) = after.find('"') else {
                    break;
                };
                found.insert(after[..close].to_string());
                rest = &after[close + 1..];
            }
        }
        assert!(
            !found.is_empty(),
            "extracted zero entries from `{name}` in {HEADER_REL} — the \
             extraction is matching the wrong thing, which would make every \
             assertion in this module vacuously pass"
        );
        found
    }

    fn rust_set(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|v| (*v).to_string()).collect()
    }

    /// The producer keeps `tool_input` only for tools this list matches, and
    /// the consumer reads `tool_input.question` only for tools the Rust list
    /// matches. A name in one list but not the other is a silent bug: either
    /// the question text is stripped before WTA can read it (producer missing
    /// an entry), or a multi-KB tool argument rides the broadcast for nothing
    /// (consumer missing an entry).
    #[test]
    fn user_input_tool_list_matches_the_wtcli_producer() {
        let producer = cpp_string_array(&header_source(), "userInputTools");
        let consumer = rust_set(crate::agent_sessions::USER_INPUT_TOOL_NAMES);

        let missing_in_producer: Vec<_> = consumer.difference(&producer).collect();
        let missing_in_consumer: Vec<_> = producer.difference(&consumer).collect();

        assert!(
            missing_in_producer.is_empty() && missing_in_consumer.is_empty(),
            "user-input tool lists drifted between the hook producer and WTA.\n\
             \n\
             In agent_sessions::USER_INPUT_TOOL_NAMES but NOT in `userInputTools`\n\
             ({HEADER_REL}): {missing_in_producer:?}\n\
             -> the producer strips `tool_input` for these, so the consumer's\n\
                `tool_input.question` lookup falls back to placeholder text.\n\
             \n\
             In `userInputTools` but NOT in USER_INPUT_TOOL_NAMES: {missing_in_consumer:?}\n\
             -> the producer keeps `tool_input` for these, but nothing reads it.\n\
             \n\
             Fix by editing whichever side is wrong; the two must stay equal."
        );
    }

    /// Guards the direction that loses data outright: the producer's
    /// unconditional strip list must never contain a member the consumer reads.
    #[test]
    fn wtcli_never_strips_a_payload_key_wta_reads() {
        let stripped = cpp_string_array(&header_source(), "alwaysStrip");
        let consumed = rust_set(crate::app::CONSUMED_PAYLOAD_KEYS);

        let collisions: Vec<_> = consumed.intersection(&stripped).collect();
        assert!(
            collisions.is_empty(),
            "`alwaysStrip` in {HEADER_REL} redacts payload members that \
             route_agent_event_to_registry_with_hook_sink reads: {collisions:?}.\n\
             These arrive empty with no error and no log — a blank cwd, a lost \
             notification message, or an unnamed tool. Remove them from the \
             producer's strip list, or stop reading them and drop them from \
             app::CONSUMED_PAYLOAD_KEYS."
        );
    }

    /// When an event overflows its wire budget the producer rebuilds the
    /// payload from `kConsumedPayloadKeys` alone. A member the consumer reads
    /// but that list omits survives normal events and vanishes only on
    /// oversized ones — the least reproducible failure shape available.
    #[test]
    fn oversize_reduction_keeps_every_payload_key_wta_reads() {
        let retained = cpp_string_array(&header_source(), "kConsumedPayloadKeys");
        let consumed = rust_set(crate::app::CONSUMED_PAYLOAD_KEYS);
        assert_eq!(
            retained, consumed,
            "`kConsumedPayloadKeys` in {HEADER_REL} and app::CONSUMED_PAYLOAD_KEYS \
             disagree. The producer keeps only its own list when an event exceeds \
             kMaxHookEventChars, so anything missing there is dropped from \
             oversized events while working fine on normal ones."
        );
    }

    /// Same contract one level down: `tool_input` is projected to these before
    /// an oversized event is published.
    #[test]
    fn oversize_reduction_keeps_every_tool_input_key_wta_reads() {
        let retained = cpp_string_array(&header_source(), "kConsumedToolInputKeys");
        let consumed = rust_set(crate::app::CONSUMED_TOOL_INPUT_KEYS);
        assert_eq!(
            retained, consumed,
            "`kConsumedToolInputKeys` in {HEADER_REL} and \
             app::CONSUMED_TOOL_INPUT_KEYS disagree. The producer projects \
             `tool_input` down to its own list on oversized events, so a name \
             missing there costs the user-input notification its question text."
        );
    }

    /// The producer lowercases `tool_name` before comparing, so a mixed-case
    /// entry on either side can never match anything.
    #[test]
    fn user_input_tool_names_are_lowercase_on_both_sides() {
        for name in crate::agent_sessions::USER_INPUT_TOOL_NAMES {
            assert_eq!(
                *name,
                name.to_ascii_lowercase(),
                "USER_INPUT_TOOL_NAMES entry {name:?} is not lowercase; \
                 is_user_input_tool lowercases its input, so this entry is dead"
            );
        }
        for name in cpp_string_array(&header_source(), "userInputTools") {
            assert_eq!(
                name,
                name.to_ascii_lowercase(),
                "`userInputTools` entry {name:?} in {HEADER_REL} is not \
                 lowercase; the producer lowercases toolName before comparing, \
                 so this entry is dead"
            );
        }
    }

    /// Keeps the constant load-bearing: if `is_user_input_tool` stops reading
    /// it, the parity assertions above would still pass while the runtime
    /// behavior silently diverged.
    #[test]
    fn is_user_input_tool_is_driven_by_the_shared_constant() {
        for name in crate::agent_sessions::USER_INPUT_TOOL_NAMES {
            assert!(
                crate::agent_sessions::is_user_input_tool(name),
                "is_user_input_tool({name:?}) is false despite the name being \
                 in USER_INPUT_TOOL_NAMES — the function no longer consumes \
                 the constant this contract is asserted against"
            );
            assert!(
                crate::agent_sessions::is_user_input_tool(&name.to_ascii_uppercase()),
                "is_user_input_tool is no longer case-insensitive for {name:?}"
            );
        }
        assert!(
            !crate::agent_sessions::is_user_input_tool("Bash"),
            "is_user_input_tool matched an ordinary tool"
        );
    }
}
