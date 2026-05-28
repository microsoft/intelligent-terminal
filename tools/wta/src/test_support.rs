//! Test-only helpers shared across modules.
//!
//! These helpers are compiled only under `cfg(test)`. They centralize patterns
//! that would otherwise have to be duplicated in every `mod tests` block —
//! most importantly the global-locale lock, which must be a single mutex
//! shared across the whole crate to actually serialize parallel tests.

#![cfg(test)]

use std::sync::{Mutex, MutexGuard};

/// All tests that read or write `rust_i18n`'s global locale share this lock.
///
/// Cargo runs unit tests in parallel by default. Without a single shared
/// mutex, one test's `rust_i18n::set_locale("zh-CN")` can race with another
/// test's en-US assertions. Module-local mutexes are NOT sufficient, because
/// two tests in different modules would hold two different locks and still
/// race on the global `rust_i18n` state.
static LOCALE_LOCK: Mutex<()> = Mutex::new(());

/// Shared lock for tests that mutate process-wide environment variables.
///
/// Like the locale lock, env-var mutation is a global side effect — two
/// tests that simultaneously `set_var(X, ...)` and `var(X)` will race.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII guard returned by [`lock_locale`].
///
/// While alive it (a) holds the crate-wide `LOCALE_LOCK`, serializing all
/// locale-sensitive tests, and (b) on drop restores whatever locale was
/// active when the guard was acquired, so the test suite remains
/// order-independent even when individual tests mutate the global locale.
pub(crate) struct LocaleGuard {
    _lock: MutexGuard<'static, ()>,
    previous: String,
}

impl Drop for LocaleGuard {
    fn drop(&mut self) {
        rust_i18n::set_locale(&self.previous);
    }
}

/// Acquire the shared locale lock and capture the current locale for restore.
///
/// Any test that calls `rust_i18n::set_locale(...)` — directly or transitively
/// via code that reads the global locale — must hold the returned guard for
/// the duration of its assertions.
pub(crate) fn lock_locale() -> LocaleGuard {
    // `unwrap` is fine: a poisoned mutex here just means a previous test
    // panicked while holding it, and we don't care about the `()` payload.
    let lock = LOCALE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    LocaleGuard {
        _lock: lock,
        previous: rust_i18n::locale().to_string(),
    }
}

/// RAII guard returned by [`lock_env`].
///
/// While alive it holds the crate-wide `ENV_LOCK`, serializing tests
/// that mutate process-wide environment variables.
pub(crate) struct EnvGuard {
    _lock: MutexGuard<'static, ()>,
}

/// Acquire the shared env-var lock.
pub(crate) fn lock_env() -> EnvGuard {
    let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    EnvGuard { _lock: lock }
}
