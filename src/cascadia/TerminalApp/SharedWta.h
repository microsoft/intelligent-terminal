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
// Lifecycle model: reference-counted across independent process leases and
// agent panes. The first owner spawns the master; the last release terminates
// it via the Job Object. Master crashes are detected via
// RegisterWaitForSingleObject; state clears so the next acquire
// respawns cleanly, reusing the same pipe name so previously-spawned
// helpers can reconnect.

#include <atomic>
#include <chrono>
#include <deque>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include <wil/resource.h>
#include <winrt/Windows.Foundation.h>

namespace winrt::TerminalApp::implementation
{
    namespace details
    {
        class LiveObjectGenerationTracker
        {
        public:
            uint64_t Get(const winrt::Windows::Foundation::IInspectable& object);

        private:
            struct Entry
            {
                winrt::weak_ref<winrt::Windows::Foundation::IInspectable> object;
                uint64_t generation;
            };

            std::mutex _mutex;
            uint64_t _nextGeneration{ 0 };
            std::vector<Entry> _entries;
        };

        struct RetirementRegistration
        {
            std::string operationId;
            bool shouldPublish{ false };
            bool alreadyCompleted{ false };
        };

        class RetirementCoordinator
        {
        public:
            static constexpr size_t CompletedHistoryLimit{ 64 };

            std::string CreateRequestId();
            RetirementRegistration Register(bool scopeAll, std::string_view reason, std::string_view requestId = {});
            bool Complete(std::string_view operationId, bool expireAfterContinuations = false);
            void ReleaseContinuation(std::string_view operationId);
            void Expire(std::string_view operationId);
            bool ClaimAction(std::string_view operationId, std::string_view action);

        private:
            struct Operation
            {
                bool completed{ false };
                bool expireAfterContinuations{ false };
                bool recordedInHistory{ false };
                size_t continuationCount{ 0 };
                std::optional<std::string> requestId;
                std::unordered_set<std::string> claimedActions;
            };

            std::string _CreateIdLocked(std::string_view kind);
            void _EraseLocked(const std::string& operationId);
            void _FinalizeCompletedLocked(std::unordered_map<std::string, Operation>::iterator operation);
            void _PruneCompletedLocked();

            std::mutex _mutex;
            uint64_t _nextOperationId{ 0 };
            std::unordered_map<std::string, Operation> _operations;
            std::unordered_map<std::string, std::string> _allOperationsByRequest;
            std::deque<std::string> _completedOperations;
        };

        class TabRetirementTracker
        {
        public:
            bool BeginRecreation(std::string_view tabId);
            bool RequestClose(std::string_view tabId);
            bool Complete(std::string_view tabId);

        private:
            std::unordered_map<std::string, bool> _closeRequested;
        };

        class RestartSuppressionTracker
        {
        public:
            void Mark(std::string_view tabId);
            void Clear(std::string_view tabId);
            bool Consume(std::string_view tabId);

        private:
            std::unordered_map<std::string, std::chrono::steady_clock::time_point> _marks;
        };

        class CoalescedRequest
        {
        public:
            void Queue(std::string requestId);
            std::optional<std::string> Take();
            void Clear();
            bool Pending() const noexcept;

        private:
            std::optional<std::string> _requestId;
        };

        constexpr bool IsValidEnvironmentOverride(const std::wstring_view name, const std::wstring_view value) noexcept
        {
            return !name.empty() &&
                   name.find(L'=') == std::wstring_view::npos &&
                   name.find(L'\0') == std::wstring_view::npos &&
                   value.find(L'\0') == std::wstring_view::npos;
        }

        // A present empty string means CreateProcess should inherit the parent
        // environment; nullopt means the requested block could not be built.
        std::optional<std::wstring> BuildEnvironmentBlock(
            std::span<const std::pair<std::wstring, std::wstring>> overrides) noexcept;

        struct SuspendedProcessOperations
        {
            DWORD(WINAPI* resumeThread)(HANDLE){ ::ResumeThread };
            BOOL(WINAPI* unregisterWait)(HANDLE, HANDLE){ ::UnregisterWaitEx };
            BOOL(WINAPI* terminateProcess)(HANDLE, UINT){ ::TerminateProcess };
        };

        bool ResumeSuspendedProcess(
            HANDLE thread,
            HANDLE process,
            HANDLE& waitHandle,
            const SuspendedProcessOperations& operations = {}) noexcept;

        struct ProcessRetirementOperations
        {
            DWORD(WINAPI* waitForSingleObject)(HANDLE, DWORD){ ::WaitForSingleObject };
            BOOL(WINAPI* terminateProcess)(HANDLE, UINT){ ::TerminateProcess };
        };

        bool EnsureProcessExitedBeforeRestart(
            HANDLE process,
            DWORD pid,
            DWORD exitTimeoutMs,
            DWORD forcedExitTimeoutMs,
            const ProcessRetirementOperations& operations = {});

        class ProcessWaitGenerationTracker
        {
        public:
            using Generation = uintptr_t;

            Generation Register(DWORD pid) noexcept;
            void Retire() noexcept;
            std::optional<DWORD> Claim(Generation generation) noexcept;
            Generation Current() const noexcept;

        private:
            Generation _nextGeneration{ 0 };
            Generation _currentGeneration{ 0 };
            DWORD _pid{ 0 };
        };

        class UnexpectedExitRecoveryPolicy
        {
        public:
            using Generation = ProcessWaitGenerationTracker::Generation;

            void Arm(Generation generation) noexcept;
            void Retire() noexcept;
            bool ShouldRespawn(
                Generation generation,
                size_t refCount,
                bool spawnSuppressed,
                bool hasCachedArgs) noexcept;

        private:
            Generation _armedGeneration{ 0 };
        };

        class MasterOwnershipTracker
        {
        public:
            bool AcquirePane() noexcept;
            bool AcquireLease() noexcept;
            bool ReleasePane() noexcept;
            bool ReleaseLease() noexcept;
            bool HasOwners() const noexcept;
            bool IsLeaseOnly() const noexcept;
            size_t PaneCount() const noexcept;
            size_t LeaseCount() const noexcept;

        private:
            size_t _paneCount{ 0 };
            size_t _leaseCount{ 0 };
        };

        struct MasterLaunchConfiguration
        {
            std::wstring wtaPath;
            std::vector<std::wstring> extraArgs;
            std::vector<std::pair<std::wstring, std::wstring>> environment;

            bool operator==(const MasterLaunchConfiguration&) const = default;
        };

        class MasterConfigurationTracker
        {
        public:
            void UpdateDesired(
                std::wstring_view wtaPath,
                std::span<const std::wstring> extraArgs,
                std::span<const std::pair<std::wstring, std::wstring>> environment);
            void MarkRunning(
                std::wstring_view wtaPath,
                std::span<const std::wstring> extraArgs,
                std::span<const std::pair<std::wstring, std::wstring>> environment);
            void ClearRunning() noexcept;
            bool HasDesired() const noexcept;
            bool RunningMatchesDesired() const noexcept;
            const MasterLaunchConfiguration& Desired() const noexcept;

        private:
            static MasterLaunchConfiguration _Capture(
                std::wstring_view wtaPath,
                std::span<const std::wstring> extraArgs,
                std::span<const std::pair<std::wstring, std::wstring>> environment);

            MasterLaunchConfiguration _desired;
            std::optional<MasterLaunchConfiguration> _running;
        };

        class SharedWtaPaneReferenceToken
        {
        public:
            std::shared_ptr<SharedWtaPaneReferenceToken> Transfer() noexcept
            {
                if (!_ownsRelease.exchange(false, std::memory_order_acq_rel))
                {
                    return {};
                }
                return std::shared_ptr<SharedWtaPaneReferenceToken>{
                    new SharedWtaPaneReferenceToken{}
                };
            }

            bool ClaimRelease() noexcept
            {
                return _ownsRelease.exchange(false, std::memory_order_acq_rel);
            }

            bool OwnsRelease() const noexcept
            {
                return _ownsRelease.load(std::memory_order_acquire);
            }

        private:
            std::atomic_bool _ownsRelease{ true };
        };
    }

    class SharedWta
    {
    public:
        class MasterLease
        {
        public:
            MasterLease() noexcept = default;
            ~MasterLease();
            MasterLease(MasterLease&& other) noexcept;
            MasterLease& operator=(MasterLease&& other) noexcept;
            MasterLease(const MasterLease&) = delete;
            MasterLease& operator=(const MasterLease&) = delete;

            explicit operator bool() const noexcept;
            void Reset() noexcept;

        private:
            friend class SharedWta;
            explicit MasterLease(SharedWta* owner) noexcept;

            SharedWta* _owner{ nullptr };
        };

        /// Access the process-singleton instance. The first call lazily
        /// constructs the object; subsequent calls return the same
        /// instance. Thread-safe via magic-statics.
        static SharedWta& Instance();

        SharedWta(const SharedWta&) = delete;
        SharedWta& operator=(const SharedWta&) = delete;

        /// Acquire a pane reference to the shared wta process. Spawns wta
        /// when no master is running; subsequent acquires only add ownership.
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
        /// A pane acquire does not replace an already-running master.
        /// Master-lease acquisition and settings reconciliation refresh the
        /// trusted configuration used by lease-only replacement and crash
        /// recovery. Runtime settings updates also flow over the existing
        /// event channels (e.g. `autofix_enabled_changed`).
        ///
        /// Every successful `AcquirePane` MUST be paired with exactly
        /// one `ReleasePane` when the caller's agent pane closes.
        /// When both pane and process-lease counts reach zero the Job Object is
        /// closed, terminating wta and every descendant it spawned.
        bool AcquirePane(const std::wstring_view wtaPath,
                         std::span<const std::wstring> extraArgs = {},
                         std::span<const std::pair<std::wstring, std::wstring>> environment = {});

        /// Acquire process-scoped ownership of the trusted default master
        /// configuration without materializing an agent pane. The returned
        /// move-only lease releases automatically. Pane and lease ownership are
        /// independent; the master is retired only after both reach zero.
        MasterLease AcquireMasterLease(
            const std::wstring_view wtaPath,
            std::span<const std::wstring> extraArgs = {},
            std::span<const std::pair<std::wstring, std::wstring>> environment = {});

        /// Release a previously acquired reference. Calling without a
        /// matching `AcquirePane` is a no-op (safe to call from
        /// teardown paths that aren't sure whether they acquired).
        void ReleasePane();

        /// Release a previously acquired reference after the bounded ACP
        /// session-close window. Agent-pane Closed events can fire before
        /// the owning tab publishes tab_closed, so an immediate final
        /// release could terminate wta-master before session/close runs.
        static winrt::fire_and_forget ReleasePaneAfterSessionClose();

        /// Force-restart the wta-master process, bypassing the
        /// pane/process-lease ownership counts. Used by the
        /// `/restart` slash command and launch-configuration changes after
        /// their sessions retire. Existing panes and helpers stay alive; the
        /// replacement master listens on the same `_masterPipeName` so they
        /// can reconnect without rebuilding their physical terminal stack.
        ///
        /// Replays the newest trusted `wtaPath` + `extraArgs` configuration,
        /// including settings changes deferred while panes were live.
        ///
        /// Settings changes (acpAgent / acpModel / etc.) need to spawn
        /// the master with a *different* cmdline. For that case, call
        /// the overload that takes a fresh `wtaPath` + `extraArgs` —
        /// it replaces the cached spawn args before respawning, so
        /// the new master inherits the new per-process settings and
        /// any subsequent crash-recovery respawn uses the same.
        ///
        /// No-op if the master isn't running, or if there were no
        /// cached spawn args (no ownership acquire has succeeded this
        /// process) and no fresh args were supplied. Returns true on
        /// successful respawn or no-op.
        bool Restart();
        bool Restart(const std::wstring_view wtaPath,
                     std::span<const std::wstring> extraArgs,
                     std::span<const std::pair<std::wstring, std::wstring>> environment = {});

        /// Refresh the trusted default configuration only when the process is
        /// owned exclusively by process leases. Active pane helpers continue
        /// through their existing retirement/rebind path.
        bool RestartIfLeaseOnly(
            const std::wstring_view wtaPath,
            std::span<const std::wstring> extraArgs,
            std::span<const std::pair<std::wstring, std::wstring>> environment = {});

        std::string CreateRetirementRequestId();
        details::RetirementRegistration RegisterRetirement(
            bool scopeAll,
            std::string_view reason,
            std::string_view requestId = {});
        bool CompleteRetirement(std::string_view operationId, bool expireAfterContinuations = false);
        void ReleaseRetirementContinuation(std::string_view operationId);
        void ExpireRetirement(std::string_view operationId);
        bool ClaimRetirementAction(std::string_view operationId, std::string_view action);
        uint64_t GetSettingsGeneration(const winrt::Windows::Foundation::IInspectable& settings);

        /// Whether wta is currently spawned. Becomes false after a
        /// crash is observed by the wait callback, or after the last
        /// owner releases.
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
        /// ownership acquire. Format: `\\.\pipe\wta-master-<GUID>`.
        std::wstring_view MasterPipeName() const noexcept;

    private:
        SharedWta() = default;
        ~SharedWta();

        // All `*Locked` helpers assume the caller already holds `_mtx`.
        bool _SpawnLocked(const std::wstring_view wtaPath,
                          std::span<const std::wstring> extraArgs,
                          std::span<const std::pair<std::wstring, std::wstring>> environment,
                          bool armUnexpectedExitRecovery = true);
        bool _RestartLocked(const std::wstring_view wtaPath,
                            std::span<const std::wstring> extraArgs,
                            std::span<const std::pair<std::wstring, std::wstring>> environment);
        wil::unique_handle _CleanupLocked();
        void _ReleaseMasterLease() noexcept;

        // Wait-callback bridge — `RegisterWaitForSingleObject` requires
        // a free function. The `context` PVOID carries a monotonically
        // increasing registration generation (not a `this` pointer), so
        // delayed callbacks cannot match a replacement process even if
        // Windows reused the old PID.
        static void CALLBACK _OnProcessExitedThunk(PVOID context, BOOLEAN timedOut);
        void _OnProcessExited(details::ProcessWaitGenerationTracker::Generation generation);

        mutable std::mutex _mtx;
        wil::unique_handle _process;
        wil::unique_handle _job;
        HANDLE _waitHandle{ nullptr };
        details::ProcessWaitGenerationTracker _waitGeneration;
        details::UnexpectedExitRecoveryPolicy _unexpectedExitRecovery;
        DWORD _pid{ 0 };
        details::MasterOwnershipTracker _ownership;
        // Generated lazily on first ownership acquire; reused across
        // master respawns within the same Terminal process so any
        // helpers spawned with stale cmdline can still find the
        // currently-live master.
        std::wstring _masterPipeName;
        // The desired launch configuration is refreshed even while live panes
        // defer replacement. The running snapshot remains separate so a later
        // retirement still detects the pending restart, while crash recovery
        // immediately uses the newest trusted inputs.
        details::MasterConfigurationTracker _configuration;
        // A restart could not confirm that the retired master exited. Never
        // risk another process claiming the stable pipe in this Terminal
        // process; recovery requires restarting Terminal.
        bool _spawnSuppressed{ false };
        details::RetirementCoordinator _retirementCoordinator;
        details::LiveObjectGenerationTracker _settingsGenerations;
    };
}
