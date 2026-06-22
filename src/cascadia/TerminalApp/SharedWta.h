// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

// Process-scope singleton for the wta-master half of the helper +
// master architecture. One wta-master process per Terminal process,
// spawned lazily on the first agent-pane request, contained in a
// Job Object with KILL_ON_JOB_CLOSE so it dies with Terminal.
//
// See doc/specs/Multi-window-agent-pane.md for the full design.
//
// This class only owns the master's *process lifecycle* and the
// allocation of the master ↔ helpers named pipe path. Helpers are
// spawned by TerminalPage as ordinary conpty children (legacy
// ConptyConnection path) and connect to the master via the pipe
// whose name `MasterPipeName()` exposes.
//
// Lifecycle model: reference-counted. Each agent pane calls
// `AcquirePane` on creation and `ReleasePane` when it closes. The
// first acquire spawns the master; the last release terminates it
// via the Job Object. master crashes are detected via
// RegisterWaitForSingleObject; state clears so the next acquire
// respawns cleanly, reusing the same pipe name so previously-spawned
// helpers can reconnect.

#include <atomic>
#include <chrono>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include <wil/resource.h>

namespace winrt::TerminalApp::implementation
{
    class SharedWta
    {
    public:
        /// Access the process-singleton instance. The first call lazily
        /// constructs the object; subsequent calls return the same
        /// instance. Thread-safe via magic-statics.
        static SharedWta& Instance();

        SharedWta(const SharedWta&) = delete;
        SharedWta& operator=(const SharedWta&) = delete;

        /// Acquire a reference to the shared wta process. Spawns wta
        /// on the first acquire; subsequent acquires just bump an
        /// internal counter. Returns true on success.
        ///
        /// `wtaPath` is the full path to wta.exe — see
        /// `TerminalPage::_DetectWtaPath()`.
        ///
        /// `extraArgs` is a list of already-tokenized command-line
        /// arguments appended to the wta command line at spawn time
        /// (after `--master <pipe>`). Each element is shell-escaped
        /// internally via `QuoteAndEscapeCommandlineArg`, so callers
        /// can pass raw values (paths with spaces, settings strings
        /// with quotes) without any pre-escaping. For flag/value
        /// pairs, push them as two separate elements (`--agent`,
        /// `<path>`); bare flags are a single element (`--no-autofix`).
        /// Used to bake per-process settings (`--no-autofix`,
        /// `--language`, `--acp-model`, etc.) at the first spawn.
        /// **Ignored on subsequent acquires** — the singleton is
        /// already running by then. Runtime settings updates flow
        /// over the existing event channels
        /// (e.g. `autofix_enabled_changed`).
        ///
        /// Every successful `AcquirePane` MUST be paired with exactly
        /// one `ReleasePane` when the caller's agent pane closes.
        /// When the count reaches zero the Job Object is closed,
        /// terminating wta and every descendant it spawned.
        bool AcquirePane(const std::wstring_view wtaPath,
                         std::span<const std::wstring> extraArgs = {});

        /// Release a previously acquired reference. Calling without a
        /// matching `AcquirePane` is a no-op (safe to call from
        /// teardown paths that aren't sure whether they acquired).
        void ReleasePane();

        /// Force-restart the wta-master process, bypassing the
        /// `AcquirePane`/`ReleasePane` reference count. Used by the
        /// `/restart` slash command path: the caller (TerminalPage)
        /// tears down every agent pane around this call, so the
        /// refcount-based teardown isn't enough — there may be other
        /// panes' Closed handlers that have yet to fire. After this
        /// returns, the next `AcquirePane` finds a fresh master
        /// listening on the same `_masterPipeName` (intentionally
        /// stable across respawns so any helpers spawned with the old
        /// cmdline can still find it).
        ///
        /// Replays the `wtaPath` + `extraArgs` cached from the first
        /// successful spawn so the new master inherits the same
        /// per-process settings as the one being replaced. This makes
        /// `/restart` semantically "give me the same agent CLI but
        /// fresh".
        ///
        /// Settings changes (acpAgent / acpModel / etc.) need to spawn
        /// the master with a *different* cmdline. For that case, call
        /// the overload that takes a fresh `wtaPath` + `extraArgs` —
        /// it replaces the cached spawn args before respawning, so
        /// the new master inherits the new per-process settings and
        /// any subsequent crash-recovery respawn uses the same.
        ///
        /// No-op if the master isn't running, or if there were no
        /// cached spawn args (no AcquirePane has succeeded this
        /// process) and no fresh args were supplied. Returns true on
        /// successful respawn or no-op.
        bool Restart();
        bool Restart(const std::wstring_view wtaPath,
                     std::span<const std::wstring> extraArgs);

        /// Whether wta is currently spawned. Becomes false after a
        /// crash is observed by the wait callback, or after the last
        /// pane releases.
        bool IsRunning() const noexcept;

        /// Whether the master died *unexpectedly* (crash/OOM/external
        /// kill) while agent panes were still live, and has not yet
        /// been recovered via `/restart`. While this latch is set,
        /// `AcquirePane` refuses to silently respawn the master — so a
        /// new tab / pane toggle does NOT bring up a lone fresh master
        /// that the orphaned helpers can't see (split-brain). Instead
        /// every agent pane stays uniformly in the "connection lost —
        /// run /restart" state until the user explicitly recovers the
        /// whole stack. Cleared by `Restart()` (the `/restart` path) or
        /// once the last orphaned pane releases. See
        /// `doc/specs/Multi-window-agent-pane.md`.
        bool IsDegraded() const noexcept;

        /// Native handle of the running master process, valid only
        /// while `IsRunning()` returns true. Exposed for diagnostic
        /// purposes (logging, telemetry). The helper architecture no
        /// longer needs cross-process HANDLE marshaling — helpers
        /// connect to the master via the named pipe instead.
        /// Returns INVALID_HANDLE_VALUE when the master is not
        /// running.
        HANDLE ProcessHandle() const noexcept;

        /// Native PID of the running wta process. Returned for
        /// diagnostic logging only; routing in the shared-wta
        /// architecture is by tab StableId, not by PID.
        DWORD ProcessId() const noexcept;

        /// Path to the Windows named pipe that wta-master is
        /// listening on. Generated once at first acquire (per-process
        /// unique GUID) and reused for the master's lifetime; each
        /// per-pane wta-helper connects to this pipe to talk ACP
        /// JSON-RPC to the master. Empty before the first
        /// `AcquirePane`. Format: `\\.\pipe\wta-master-<GUID>`.
        std::wstring_view MasterPipeName() const noexcept;

    private:
        SharedWta() = default;
        ~SharedWta();

        // All `*Locked` helpers assume the caller already holds `_mtx`.
        bool _SpawnLocked(const std::wstring_view wtaPath,
                          std::span<const std::wstring> extraArgs);
        void _CleanupLocked();

        // Wait-callback bridge — `RegisterWaitForSingleObject` requires
        // a free function. The `context` PVOID carries the PID this
        // wait was registered for (not a `this` pointer — the singleton
        // is reached via `Instance()`), so the callback can detect a
        // stale registration after `_CleanupLocked` + `_SpawnLocked`
        // replaced the master out from under it. Without that check,
        // a delayed callback for the OLD master would null out the
        // *new* master's `_process` / `_waitHandle` and silently break
        // crash detection.
        static void CALLBACK _OnProcessExitedThunk(PVOID context, BOOLEAN timedOut);
        void _OnProcessExited(DWORD observedPid);

        mutable std::mutex _mtx;
        wil::unique_handle _process;
        wil::unique_handle _job;
        HANDLE _waitHandle{ nullptr };
        DWORD _pid{ 0 };
        size_t _refCount{ 0 };
        // Generated lazily on first AcquirePane; reused across
        // master respawns within the same Terminal process so any
        // helpers spawned with stale cmdline can still find the
        // currently-live master.
        std::wstring _masterPipeName;
        // Cached cmdline inputs from the most recent successful
        // `_SpawnLocked`. Replayed verbatim by `Restart()` so the
        // refreshed master inherits the per-process settings that
        // were in effect when this Terminal process first booted an
        // agent pane. Empty when no successful spawn has happened.
        std::wstring _cachedWtaPath;
        std::vector<std::wstring> _cachedExtraArgs;
        // Wall-clock-ish stamp of the most recent successful spawn.
        // Used by the no-arg `Restart()` to dedup the fan-out from
        // `_dispatchRestartAgentStackToPage`: every open window's
        // `OnRestartAgentStackRequested` calls `Restart()` on its own
        // UI thread, and without dedup they sequentially kill each
        // other's freshly-spawned masters. A short time window is
        // enough because the fan-out runs in tight succession on
        // adjacent UI-thread ticks.
        std::optional<std::chrono::steady_clock::time_point> _lastRespawn;
        // "Degraded" latch: set when the master dies UNEXPECTEDLY
        // (crash/OOM/external kill, observed by the wait callback) while
        // panes still hold refs. While set, `AcquirePane` refuses to
        // lazily respawn the master, so the dead state stays consistent
        // across every agent pane (all show "connection lost — /restart")
        // instead of a new pane silently getting a lone fresh master the
        // orphaned helpers can never reconnect to. Cleared by `Restart()`
        // (the `/restart` recovery) and when the last pane releases (so a
        // subsequent cold open spawns normally). Distinct from
        // `!_process.is_valid()`, which is also true on a clean cold start
        // or after the last release — those MUST still spawn.
        bool _degraded{ false };
    };
}
