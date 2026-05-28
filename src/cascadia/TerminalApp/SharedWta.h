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
#include <mutex>
#include <span>
#include <string>
#include <string_view>

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

        /// Whether wta is currently spawned. Becomes false after a
        /// crash is observed by the wait callback, or after the last
        /// pane releases.
        bool IsRunning() const noexcept;

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
        // a free function. The thunk dispatches to the instance method.
        static void CALLBACK _OnProcessExitedThunk(PVOID context, BOOLEAN timedOut);
        void _OnProcessExited();

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
    };
}
