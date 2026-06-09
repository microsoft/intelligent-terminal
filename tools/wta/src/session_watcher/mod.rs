//! session_watcher — turn each agent CLI's on-disk session records into the
//! crate's existing [`crate::agent_sessions::SessionEvent`]s, hook-free.
//!
//! The per-CLI `classify_*` functions are the pure, testable core: they take
//! one parsed record (or, for Gemini, the rewritten snapshot) plus the
//! session key and return zero or more `SessionEvent`s. The watch loop
//! ([`watch`]) is the thin impure shell that tails files and feeds records
//! through them. Binding a discovered session to its pane lives in
//! [`bind`]; path → identity in [`discover`].

pub mod bind;
pub mod classify_claude;
pub mod classify_codex;
pub mod classify_copilot;
pub mod classify_gemini;
pub mod discover;
