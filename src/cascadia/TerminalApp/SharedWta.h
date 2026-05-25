// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

// Process-scope singleton for the shared-wta architecture
// (`aiIntegration.sharedWtaProcess`). One wta.exe per Terminal
// process, spawned lazily on the first agent-pane request, contained
// in a Job Object with KILL_ON_JOB_CLOSE so it dies with Terminal.
//
// See doc/specs/Multi-window-agent-pane.md for the full design.
//
// This class only owns wta's *process lifecycle*. The wire protocol
// (`_internal.attach_pane` / `_internal.detach_pane` /
// `_internal.resize_pane` events on the existing
// IProtocolEventCallback channel) is driven by the caller, which
// holds a TerminalPage reference and uses
// `TerminalPage::ProtocolVtSequenceReceived` to emit JSON — the
// ComServer fan-out then delivers the event to wta. Keeping this
// class decoupled from any specific TerminalPage means it can be
// reused across windows.

#include <mutex>
#include <string_view>

#include <wil/resource.h>

namespace winrt::TerminalApp::implementation
{
    class SharedWta
    {
    public:
        /// Access the process-singleton instance. The first call lazily
        /// constructs the object; subsequent calls return the same
        /// instance. Thread-safe via call_once.
        static SharedWta& Instance();

        SharedWta(const SharedWta&) = delete;
        SharedWta& operator=(const SharedWta&) = delete;

        /// Spawn wta.exe --headless if not already running, contain
        /// it in a Job Object, and return true on success. Idempotent
        /// — re-entry is cheap (just a flag check under the lock).
        ///
        /// `wtaPath`: full path to wta.exe. The caller is expected to
        /// resolve this via the existing `_DetectWtaPath()` logic, or
        /// any equivalent search.
        bool EnsureRunning(const std::wstring_view wtaPath);

        /// Whether wta has been spawned. Useful for tests and for
        /// gating diagnostic output.
        bool IsRunning() const noexcept;

        /// Native handle of the running wta process, valid only while
        /// `IsRunning()` returns true. Callers use this for
        /// `DuplicateHandle` when marshaling conpty slave HANDLEs
        /// into wta's address space ahead of an `_internal.attach_pane`
        /// event. Returns INVALID_HANDLE_VALUE when wta is not
        /// running — callers must check `IsRunning()` first.
        HANDLE ProcessHandle() const noexcept;

        /// Native PID of the running wta process. Returned for
        /// diagnostic logging only; routing in the shared-wta
        /// architecture is by tab StableId, not by PID.
        DWORD ProcessId() const noexcept;

    private:
        SharedWta() = default;

        mutable std::mutex _mtx;
        wil::unique_handle _process;
        wil::unique_handle _job;
        DWORD _pid{ 0 };
    };
}
