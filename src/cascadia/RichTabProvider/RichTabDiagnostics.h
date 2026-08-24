// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <cstdint>
#include <functional>
#include <optional>
#include <string>

namespace Microsoft::Terminal::RichTab::Provider
{
    enum class RichTabDiagnosticLevel
    {
        Debug,
        Info,
        Warning,
        Error,
    };

    enum class RichTabDiagnosticEvent
    {
        SinkState,
        AttachmentState,
        ContextState,
        CatalogState,
        RefreshPlan,
        RequestState,
        InstanceState,
        PublishResult,
        CompositionState,
        PageHandoff,
        HeaderMetadataState,
    };

    enum class RichTabDiagnosticState
    {
        None,
        Started,
        Degraded,
        Recovered,
        Rotated,
        EventsDropped,
        Attached,
        Detached,
        Skipped,
        GraceStarted,
        GraceExpired,
        Changed,
        Eligible,
        Effective,
        Disabled,
        Shadowed,
        Rejected,
        Planned,
        Dispatched,
        Coalesced,
        Superseded,
        Invalidated,
        Completed,
        Failed,
        Running,
        Exited,
        Backoff,
        Restarted,
        Stopped,
        Accepted,
        Empty,
        Nonempty,
        Applied,
        StaleDiscarded,
        CachedOnly,
        NoMatchingTab,
    };

    enum class RichTabDiagnosticReason
    {
        None,
        FeatureDisabled,
        ControlMissing,
        NotConnected,
        SessionMissing,
        AttachmentMissing,
        ContextChanged,
        CatalogChanged,
        PaneConnected,
        CommandFinished,
        TabActivated,
        ManualRefresh,
        CatalogLoadFailed,
        ConsentRequired,
        PreferenceDisabled,
        SourceNotAllowed,
        IntegrityFailed,
        ActivationUnsupported,
        ProviderFilterMiss,
        NoCallbacks,
        NoEffectiveProvider,
        SerializationFailed,
        InvalidRequest,
        ResolveFailed,
        LaunchFailed,
        InputWriteFailed,
        WaitFailed,
        TimedOut,
        OutputLimitExceeded,
        ExitCodeUnavailable,
        ExitNonzero,
        ExitedWithoutPublish,
        SnapshotInvalid,
        PublishRoutingUnavailable,
        LeaseCreationFailed,
        LeaseUnknown,
        LeaseExpired,
        LeaseStale,
        GenerationStale,
        InstanceMissing,
        ContextStale,
        ControlFrameFailed,
        ControlWriteFailed,
        ProcessExited,
        DetachedRetentionExpired,
        OlderUpdate,
        DispatcherUnavailable,
        PageUnavailable,
        CallbackFailed,
        Rename,
        EmptyMetadata,
        QueueFull,
        LogDirectoryUnavailable,
        LogOpenFailed,
        LogWriteFailed,
        LogRotated,
    };

    struct RichTabDiagnosticEventData
    {
        RichTabDiagnosticLevel level{ RichTabDiagnosticLevel::Info };
        RichTabDiagnosticEvent event{ RichTabDiagnosticEvent::SinkState };
        RichTabDiagnosticState state{ RichTabDiagnosticState::None };
        RichTabDiagnosticReason reason{ RichTabDiagnosticReason::None };

        std::string sessionId;
        std::string providerId;
        std::string requestId;
        std::string supersededByRequestId;
        std::string tabId;

        std::optional<uint64_t> attachmentId;
        std::optional<uint64_t> sessionIncarnation;
        std::optional<uint64_t> contextRevision;
        std::optional<uint64_t> catalogRevision;
        std::optional<uint64_t> updateSequence;
        std::optional<uint64_t> generation;
        std::optional<uint64_t> instanceGeneration;
        std::optional<uint64_t> retryAfterMilliseconds;
        std::optional<uint64_t> suppressedCount;

        std::optional<uint32_t> win32Error;
        std::optional<uint32_t> exitCode;

        std::optional<size_t> catalogCount;
        std::optional<size_t> effectiveCount;
        std::optional<size_t> filterMatchedCount;
        std::optional<size_t> activationSupportedCount;
        std::optional<size_t> eligibleCount;
        std::optional<size_t> startedCount;
        std::optional<size_t> persistentCount;
        std::optional<size_t> coalescedCount;
        std::optional<size_t> skippedCount;
        std::optional<size_t> snapshotCount;
        std::optional<size_t> fieldCount;
        std::optional<size_t> targetCount;

        std::optional<bool> cwdPresent;
        std::optional<bool> cwdAuthoritative;
        std::optional<bool> cwdChanged;
        std::optional<bool> authorityChanged;
        std::optional<bool> shellTypePresent;
        std::optional<bool> shellChanged;
        std::optional<bool> persistent;
        std::optional<bool> presentationPresent;
        std::optional<bool> requestedVisible;
        std::optional<bool> effectiveVisible;
        std::optional<bool> textPresent;
        std::optional<bool> automationNamePresent;
        std::optional<bool> overrideActive;
    };

    using RichTabDiagnosticObserver = std::function<void(const RichTabDiagnosticEventData&)>;
    using RichTabDiagnosticHealthReporter = void (*)(RichTabDiagnosticReason reason, uint32_t win32Error) noexcept;

    bool StartRichTabDiagnostics(RichTabDiagnosticHealthReporter healthReporter = nullptr) noexcept;
    void EmitRichTabDiagnostic(RichTabDiagnosticEventData event) noexcept;
    void FlushRichTabDiagnostics() noexcept;
    bool ShutdownRichTabDiagnostics(uint32_t timeoutMilliseconds) noexcept;
    std::string SerializeRichTabDiagnosticForTests(const RichTabDiagnosticEventData& event);

    class ScopedRichTabDiagnosticObserver
    {
    public:
        explicit ScopedRichTabDiagnosticObserver(RichTabDiagnosticObserver observer);
        ~ScopedRichTabDiagnosticObserver();

        ScopedRichTabDiagnosticObserver(const ScopedRichTabDiagnosticObserver&) = delete;
        ScopedRichTabDiagnosticObserver& operator=(const ScopedRichTabDiagnosticObserver&) = delete;

    private:
        uint64_t _generation{ 0 };
    };
}
