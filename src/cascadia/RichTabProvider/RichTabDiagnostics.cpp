// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "RichTabDiagnostics.h"

#include <windows.h>
#include <bcrypt.h>
#include <json/json.h>
#include <wil/resource.h>

#include <atomic>
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <deque>
#include <filesystem>
#include <iomanip>
#include <memory>
#include <mutex>
#include <sstream>
#include <thread>
#include <unordered_map>
#include <utility>
#include <vector>

#include "IntelligentTerminalPaths.h"

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr size_t MaximumQueuedEvents = 1024;
        constexpr size_t MaximumQueuedBytes = 1024 * 1024;
        constexpr size_t MaximumEventBytes = 4 * 1024;
        constexpr uint64_t MaximumFileBytes = 4ull * 1024ull * 1024ull;
        constexpr uint64_t MaximumClosedRunBytes = 32ull * 1024ull * 1024ull;
        constexpr auto MaximumClosedRunAge = std::chrono::hours{ 24 * 7 };
        constexpr auto FailureWindow = std::chrono::seconds{ 60 };
        constexpr size_t MaximumFailuresPerWindow = 3;
        thread_local size_t ObserverCallbackDepth = 0;

        std::string_view _Level(const RichTabDiagnosticLevel value) noexcept
        {
            switch (value)
            {
            case RichTabDiagnosticLevel::Debug:
                return "debug";
            case RichTabDiagnosticLevel::Info:
                return "info";
            case RichTabDiagnosticLevel::Warning:
                return "warn";
            case RichTabDiagnosticLevel::Error:
                return "error";
            }
            return "unknown";
        }

        std::string_view _Event(const RichTabDiagnosticEvent value) noexcept
        {
            switch (value)
            {
            case RichTabDiagnosticEvent::SinkState:
                return "sink_state";
            case RichTabDiagnosticEvent::AttachmentState:
                return "attachment_state";
            case RichTabDiagnosticEvent::ContextState:
                return "context_state";
            case RichTabDiagnosticEvent::CatalogState:
                return "catalog_state";
            case RichTabDiagnosticEvent::RefreshPlan:
                return "refresh_plan";
            case RichTabDiagnosticEvent::RequestState:
                return "request_state";
            case RichTabDiagnosticEvent::InstanceState:
                return "instance_state";
            case RichTabDiagnosticEvent::PublishResult:
                return "publish_result";
            case RichTabDiagnosticEvent::CompositionState:
                return "composition_state";
            case RichTabDiagnosticEvent::PageHandoff:
                return "page_handoff";
            case RichTabDiagnosticEvent::HeaderMetadataState:
                return "header_metadata_state";
            }
            return "unknown";
        }

        std::string_view _State(const RichTabDiagnosticState value) noexcept
        {
            switch (value)
            {
            case RichTabDiagnosticState::None:
                return {};
#define STATE(name, text) \
    case RichTabDiagnosticState::name: \
        return text
                STATE(Started, "started");
                STATE(Degraded, "degraded");
                STATE(Recovered, "recovered");
                STATE(Rotated, "rotated");
                STATE(EventsDropped, "events_dropped");
                STATE(Attached, "attached");
                STATE(Detached, "detached");
                STATE(Skipped, "skipped");
                STATE(GraceStarted, "grace_started");
                STATE(GraceExpired, "grace_expired");
                STATE(Changed, "changed");
                STATE(Eligible, "eligible");
                STATE(Effective, "effective");
                STATE(Disabled, "disabled");
                STATE(Shadowed, "shadowed");
                STATE(Rejected, "rejected");
                STATE(Planned, "planned");
                STATE(Dispatched, "dispatched");
                STATE(Coalesced, "coalesced");
                STATE(Superseded, "superseded");
                STATE(Invalidated, "invalidated");
                STATE(Completed, "completed");
                STATE(Failed, "failed");
                STATE(Running, "running");
                STATE(Exited, "exited");
                STATE(Backoff, "backoff");
                STATE(Restarted, "restarted");
                STATE(Stopped, "stopped");
                STATE(Accepted, "accepted");
                STATE(Empty, "empty");
                STATE(Nonempty, "nonempty");
                STATE(Applied, "applied");
                STATE(StaleDiscarded, "stale_discarded");
                STATE(CachedOnly, "cached_only");
                STATE(NoMatchingTab, "no_matching_tab");
#undef STATE
            }
            return "unknown";
        }

        std::string_view _Reason(const RichTabDiagnosticReason value) noexcept
        {
            switch (value)
            {
            case RichTabDiagnosticReason::None:
                return {};
#define REASON(name, text) \
    case RichTabDiagnosticReason::name: \
        return text
                REASON(FeatureDisabled, "feature_disabled");
                REASON(ControlMissing, "control_missing");
                REASON(NotConnected, "not_connected");
                REASON(SessionMissing, "session_missing");
                REASON(AttachmentMissing, "attachment_missing");
                REASON(ContextChanged, "context_changed");
                REASON(CatalogChanged, "catalog_changed");
                REASON(PaneConnected, "pane_connected");
                REASON(CommandFinished, "command_finished");
                REASON(TabActivated, "tab_activated");
                REASON(ManualRefresh, "manual_refresh");
                REASON(CatalogLoadFailed, "catalog_load_failed");
                REASON(ConsentRequired, "consent_required");
                REASON(PreferenceDisabled, "preference_disabled");
                REASON(SourceNotAllowed, "source_not_allowed");
                REASON(IntegrityFailed, "integrity_failed");
                REASON(ActivationUnsupported, "activation_unsupported");
                REASON(ProviderFilterMiss, "provider_filter_miss");
                REASON(NoCallbacks, "no_callbacks");
                REASON(NoEffectiveProvider, "no_effective_provider");
                REASON(SerializationFailed, "serialization_failed");
                REASON(InvalidRequest, "invalid_request");
                REASON(ResolveFailed, "resolve_failed");
                REASON(LaunchFailed, "launch_failed");
                REASON(InputWriteFailed, "input_write_failed");
                REASON(WaitFailed, "wait_failed");
                REASON(TimedOut, "timed_out");
                REASON(OutputLimitExceeded, "output_limit_exceeded");
                REASON(ExitCodeUnavailable, "exit_code_unavailable");
                REASON(ExitNonzero, "exit_nonzero");
                REASON(ExitedWithoutPublish, "exited_without_publish");
                REASON(SnapshotInvalid, "snapshot_invalid");
                REASON(PublishRoutingUnavailable, "publish_routing_unavailable");
                REASON(LeaseCreationFailed, "lease_creation_failed");
                REASON(LeaseUnknown, "lease_unknown");
                REASON(LeaseExpired, "lease_expired");
                REASON(LeaseStale, "lease_stale");
                REASON(GenerationStale, "generation_stale");
                REASON(InstanceMissing, "instance_missing");
                REASON(ContextStale, "context_stale");
                REASON(ControlFrameFailed, "control_frame_failed");
                REASON(ControlWriteFailed, "control_write_failed");
                REASON(ProcessExited, "process_exited");
                REASON(DetachedRetentionExpired, "detached_retention_expired");
                REASON(OlderUpdate, "older_update");
                REASON(DispatcherUnavailable, "dispatcher_unavailable");
                REASON(PageUnavailable, "page_unavailable");
                REASON(CallbackFailed, "callback_failed");
                REASON(Rename, "rename");
                REASON(EmptyMetadata, "empty_metadata");
                REASON(QueueFull, "queue_full");
                REASON(LogDirectoryUnavailable, "log_directory_unavailable");
                REASON(LogOpenFailed, "log_open_failed");
                REASON(LogWriteFailed, "log_write_failed");
                REASON(LogRotated, "log_rotated");
#undef REASON
            }
            return "unknown";
        }

        bool _DebugEnabled() noexcept
        {
#ifdef _DEBUG
            bool enabled = true;
#else
            bool enabled = false;
#endif
            wchar_t value[32]{};
            if (const auto length = GetEnvironmentVariableW(L"WTA_LOG", value, ARRAYSIZE(value)); length > 0 && length < ARRAYSIZE(value))
            {
                enabled = _wcsicmp(value, L"debug") == 0 || _wcsicmp(value, L"trace") == 0;
            }
            return enabled;
        }

        uint64_t _RandomEpoch() noexcept
        {
            uint64_t epoch = 0;
            if (BCryptGenRandom(nullptr, reinterpret_cast<PUCHAR>(&epoch), sizeof(epoch), BCRYPT_USE_SYSTEM_PREFERRED_RNG) < 0 || epoch == 0)
            {
                epoch = (GetTickCount64() << 16) ^ GetCurrentProcessId();
            }
            return epoch;
        }

        bool _HasReparseComponent(const std::filesystem::path& path)
        {
            auto current = path.root_path();
            for (const auto& component : path.relative_path())
            {
                current /= component;
                const auto attributes = GetFileAttributesW(current.c_str());
                if (attributes == INVALID_FILE_ATTRIBUTES ||
                    (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
                {
                    return true;
                }
            }
            return false;
        }

        void _PruneClosedRuns(const std::filesystem::path& directory) noexcept
        {
            struct Candidate
            {
                std::filesystem::path path;
                std::filesystem::file_time_type modified;
                uint64_t size{ 0 };
            };

            try
            {
                std::vector<Candidate> candidates;
                uint64_t totalSize = 0;
                std::error_code error;
                for (const auto& entry : std::filesystem::directory_iterator{ directory, error })
                {
                    if (error || !entry.is_regular_file(error))
                    {
                        continue;
                    }
                    const auto name = entry.path().filename().wstring();
                    if (!name.starts_with(L"rich-tabs-") ||
                        (entry.path().extension() != L".jsonl" && entry.path().extension() != L".1"))
                    {
                        continue;
                    }
                    const auto size = entry.file_size(error);
                    const auto modified = entry.last_write_time(error);
                    if (error)
                    {
                        error.clear();
                        continue;
                    }
                    candidates.emplace_back(Candidate{ entry.path(), modified, size });
                    totalSize += size;
                }
                std::sort(candidates.begin(), candidates.end(), [](const auto& left, const auto& right) {
                    return left.modified < right.modified;
                });
                const auto oldest = std::filesystem::file_time_type::clock::now() - MaximumClosedRunAge;
                for (const auto& candidate : candidates)
                {
                    if (candidate.modified > oldest && totalSize <= MaximumClosedRunBytes)
                    {
                        continue;
                    }
                    auto activePath = candidate.path;
                    if (activePath.extension() == L".1")
                    {
                        activePath.replace_extension();
                    }
                    wil::unique_hfile activeProbe{ CreateFileW(
                        activePath.c_str(),
                        DELETE,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        nullptr,
                        OPEN_EXISTING,
                        FILE_FLAG_OPEN_REPARSE_POINT,
                        nullptr) };
                    const auto runIsClosed =
                        activeProbe ||
                        GetLastError() == ERROR_FILE_NOT_FOUND;
                    if (runIsClosed && DeleteFileW(candidate.path.c_str()))
                    {
                        totalSize -= candidate.size;
                    }
                }
            }
            catch (...)
            {
            }
        }

        std::string _Timestamp()
        {
            SYSTEMTIME time{};
            GetSystemTime(&time);
            char result[32]{};
            _snprintf_s(
                result,
                _TRUNCATE,
                "%04u-%02u-%02uT%02u:%02u:%02u.%03uZ",
                time.wYear,
                time.wMonth,
                time.wDay,
                time.wHour,
                time.wMinute,
                time.wSecond,
                time.wMilliseconds);
            return result;
        }

        struct FailureKey
        {
            RichTabDiagnosticEvent event;
            RichTabDiagnosticReason reason;
            std::string sessionId;
            std::string providerId;

            bool operator==(const FailureKey&) const = default;
        };

        struct FailureKeyHash
        {
            size_t operator()(const FailureKey& value) const noexcept
            {
                auto result = static_cast<size_t>(value.event) * 131u + static_cast<size_t>(value.reason);
                result ^= std::hash<std::string>{}(value.sessionId) << 1;
                result ^= std::hash<std::string>{}(value.providerId) << 2;
                return result;
            }
        };

        struct FailureWindowState
        {
            std::chrono::steady_clock::time_point started;
            size_t emitted{ 0 };
            uint64_t suppressed{ 0 };
        };

        class DiagnosticsState
        {
        public:
            DiagnosticsState() :
                _epoch{ _RandomEpoch() },
                _debugEnabled{ _DebugEnabled() }
            {
            }

            bool Start(const RichTabDiagnosticHealthReporter healthReporter) noexcept
            {
                try
                {
                    std::lock_guard lock{ _queueMutex };
                    if (_started)
                    {
                        return true;
                    }
                    if (_shutdownRequested)
                    {
                        return false;
                    }

                    _healthReporter = healthReporter;
                    _worker = std::thread{ [this]() noexcept { _Run(); } };
                    _started = true;
                    _accepting = true;
                    return true;
                }
                catch (...)
                {
                    _ReportHealth(RichTabDiagnosticReason::LogOpenFailed, ERROR_NOT_ENOUGH_MEMORY);
                    return false;
                }
            }

            void Emit(RichTabDiagnosticEventData event) noexcept
            {
                try
                {
                    std::shared_ptr<RichTabDiagnosticObserver> observer;
                    {
                        std::lock_guard lock{ _observerMutex };
                        observer = _observer;
                        if (observer)
                        {
                            ++_observerCallbacksInFlight;
                        }
                    }
                    if (observer)
                    {
                        ++ObserverCallbackDepth;
                        try
                        {
                            (*observer)(event);
                        }
                        catch (...)
                        {
                        }
                        --ObserverCallbackDepth;
                        {
                            std::lock_guard lock{ _observerMutex };
                            --_observerCallbacksInFlight;
                        }
                        _observerCondition.notify_all();
                    }

                    if (event.level == RichTabDiagnosticLevel::Debug && !_debugEnabled)
                    {
                        return;
                    }
                    if (!_Allow(event))
                    {
                        return;
                    }
                    {
                        std::lock_guard lock{ _queueMutex };
                        if (!_accepting)
                        {
                            return;
                        }
                        if (_queue.size() >= MaximumQueuedEvents)
                        {
                            ++_dropped;
                            return;
                        }
                        const auto eventBytes = _EstimatedBytes(event);
                        if (_queuedBytes + eventBytes > MaximumQueuedBytes)
                        {
                            ++_dropped;
                            return;
                        }
                        _queuedBytes += eventBytes;
                        _queue.emplace_back(std::move(event));
                    }
                    _queueCondition.notify_one();
                }
                catch (...)
                {
                }
            }

            void Flush() noexcept
            {
                try
                {
                    std::unique_lock lock{ _queueMutex };
                    _flushCondition.wait_for(lock, std::chrono::seconds{ 2 }, [&]() {
                        return !_started || (_queue.empty() && !_writing);
                    });
                }
                catch (...)
                {
                }
            }

            bool Shutdown(const uint32_t timeoutMilliseconds) noexcept
            {
                try
                {
                    {
                        std::lock_guard lock{ _queueMutex };
                        if (!_started)
                        {
                            return true;
                        }
                        _accepting = false;
                    }

                    std::vector<RichTabDiagnosticEventData> suppressionSummaries;
                    {
                        std::lock_guard lock{ _rateMutex };
                        suppressionSummaries.reserve(_failureWindows.size());
                        for (auto& [key, window] : _failureWindows)
                        {
                            if (window.suppressed == 0)
                            {
                                continue;
                            }
                            RichTabDiagnosticEventData event;
                            event.level = RichTabDiagnosticLevel::Warning;
                            event.event = key.event;
                            event.state = RichTabDiagnosticState::EventsDropped;
                            event.reason = key.reason;
                            event.sessionId = key.sessionId;
                            event.providerId = key.providerId;
                            event.suppressedCount = window.suppressed;
                            suppressionSummaries.emplace_back(std::move(event));
                            window.suppressed = 0;
                        }
                    }

                    {
                        std::lock_guard lock{ _queueMutex };
                        const auto queueForShutdown = [&](RichTabDiagnosticEventData event) {
                            const auto eventBytes = _EstimatedBytes(event);
                            if (_queue.size() >= MaximumQueuedEvents ||
                                _queuedBytes + eventBytes > MaximumQueuedBytes)
                            {
                                ++_dropped;
                                return;
                            }
                            _queuedBytes += eventBytes;
                            _queue.emplace_back(std::move(event));
                        };
                        for (auto& summary : suppressionSummaries)
                        {
                            queueForShutdown(std::move(summary));
                        }
                        RichTabDiagnosticEventData event;
                        event.event = RichTabDiagnosticEvent::SinkState;
                        event.state = RichTabDiagnosticState::Stopped;
                        queueForShutdown(std::move(event));
                        _shutdownRequested = true;
                    }
                    _queueCondition.notify_one();

                    {
                        std::unique_lock lock{ _queueMutex };
                        if (!_workerStoppedCondition.wait_for(
                                lock,
                                std::chrono::milliseconds{ timeoutMilliseconds },
                                [&]() { return _workerStopped; }))
                        {
                            return false;
                        }
                    }

                    if (_worker.joinable())
                    {
                        _worker.join();
                    }
                    {
                        std::lock_guard lock{ _queueMutex };
                        _started = false;
                    }
                    return true;
                }
                catch (...)
                {
                    return false;
                }
            }

            uint64_t SetObserver(RichTabDiagnosticObserver observer)
            {
                std::lock_guard lock{ _observerMutex };
                if (_observer)
                {
                    throw std::logic_error{ "Only one Rich Tab diagnostic observer may be active" };
                }
                _observerGeneration++;
                _observer = std::make_shared<RichTabDiagnosticObserver>(std::move(observer));
                return _observerGeneration;
            }

            std::string SerializeForTests(const RichTabDiagnosticEventData& event)
            {
                return _Serialize(event);
            }

            void ClearObserver(const uint64_t generation) noexcept
            {
                std::unique_lock lock{ _observerMutex };
                if (_observerGeneration == generation)
                {
                    _observer.reset();
                    _observerCondition.wait(lock, [&]() {
                        return _observerCallbacksInFlight <= ObserverCallbackDepth;
                    });
                }
            }

        private:
            bool _Allow(RichTabDiagnosticEventData& event)
            {
                if (event.level != RichTabDiagnosticLevel::Warning && event.level != RichTabDiagnosticLevel::Error)
                {
                    return true;
                }

                const auto now = std::chrono::steady_clock::now();
                const FailureKey key{ event.event, event.reason, event.sessionId, event.providerId };
                std::lock_guard lock{ _rateMutex };
                auto& window = _failureWindows[key];
                if (window.started == std::chrono::steady_clock::time_point{} || now - window.started >= FailureWindow)
                {
                    event.suppressedCount = window.suppressed;
                    window = FailureWindowState{ now, 1, 0 };
                    return true;
                }
                if (window.emitted < MaximumFailuresPerWindow)
                {
                    ++window.emitted;
                    return true;
                }
                ++window.suppressed;
                return false;
            }

            std::string _Alias(
                std::unordered_map<std::string, std::string>& aliases,
                const std::string& value,
                const char prefix)
            {
                if (value.empty())
                {
                    return {};
                }
                if (const auto found = aliases.find(value); found != aliases.end())
                {
                    return found->second;
                }
                auto alias = std::string{ prefix } + std::to_string(aliases.size() + 1);
                aliases.emplace(value, alias);
                return alias;
            }

            static size_t _EstimatedBytes(const RichTabDiagnosticEventData& event) noexcept
            {
                return sizeof(event) +
                       event.sessionId.size() +
                       event.providerId.size() +
                       event.requestId.size() +
                       event.supersededByRequestId.size() +
                       event.tabId.size();
            }

            std::string _Serialize(const RichTabDiagnosticEventData& event)
            {
                Json::Value root{ Json::objectValue };
                root["schema"] = 1;
                root["ts"] = _Timestamp();
                root["seq"] = Json::UInt64{ _nextSequence.fetch_add(1, std::memory_order_relaxed) };
                root["level"] = std::string{ _Level(event.level) };
                root["event"] = std::string{ _Event(event.event) };
                root["pid"] = Json::UInt{ GetCurrentProcessId() };
                {
                    char epoch[17]{};
                    _snprintf_s(epoch, _TRUNCATE, "%016llx", _epoch);
                    root["epoch"] = epoch;
                }
                if (const auto state = _State(event.state); !state.empty())
                {
                    root["state"] = std::string{ state };
                }
                if (const auto reason = _Reason(event.reason); !reason.empty())
                {
                    root["reason"] = std::string{ reason };
                }
                {
                    std::lock_guard lock{ _aliasMutex };
                    if (const auto alias = _Alias(_sessionAliases, event.sessionId, 's'); !alias.empty())
                    {
                        root["session"] = alias;
                    }
                    if (const auto alias = _Alias(_providerAliases, event.providerId, 'p'); !alias.empty())
                    {
                        root["provider"] = alias;
                    }
                    if (const auto alias = _Alias(_tabAliases, event.tabId, 't'); !alias.empty())
                    {
                        root["tab"] = alias;
                    }
                }
                if (!event.requestId.empty() && event.requestId.size() <= 64)
                    root["request"] = event.requestId;
                if (!event.supersededByRequestId.empty() && event.supersededByRequestId.size() <= 64)
                    root["superseded_by_request"] = event.supersededByRequestId;

#define OPTIONAL_NUMBER(member, name) \
    if (event.member) \
        root[name] = Json::UInt64{ *event.member }
                OPTIONAL_NUMBER(attachmentId, "attachment");
                OPTIONAL_NUMBER(sessionIncarnation, "session_incarnation");
                OPTIONAL_NUMBER(contextRevision, "context_revision");
                OPTIONAL_NUMBER(catalogRevision, "catalog_revision");
                OPTIONAL_NUMBER(updateSequence, "update_sequence");
                OPTIONAL_NUMBER(generation, "generation");
                OPTIONAL_NUMBER(instanceGeneration, "instance_generation");
                OPTIONAL_NUMBER(retryAfterMilliseconds, "retry_after_ms");
                OPTIONAL_NUMBER(suppressedCount, "suppressed_count");
                OPTIONAL_NUMBER(win32Error, "win32_error");
                OPTIONAL_NUMBER(exitCode, "exit_code");
                OPTIONAL_NUMBER(catalogCount, "catalog_count");
                OPTIONAL_NUMBER(effectiveCount, "effective_count");
                OPTIONAL_NUMBER(filterMatchedCount, "filter_matched_count");
                OPTIONAL_NUMBER(activationSupportedCount, "activation_supported_count");
                OPTIONAL_NUMBER(eligibleCount, "eligible_count");
                OPTIONAL_NUMBER(startedCount, "started_count");
                OPTIONAL_NUMBER(persistentCount, "persistent_count");
                OPTIONAL_NUMBER(coalescedCount, "coalesced_count");
                OPTIONAL_NUMBER(skippedCount, "skipped_count");
                OPTIONAL_NUMBER(snapshotCount, "snapshot_count");
                OPTIONAL_NUMBER(fieldCount, "field_count");
                OPTIONAL_NUMBER(targetCount, "target_count");
#undef OPTIONAL_NUMBER
#define OPTIONAL_BOOL(member, name) \
    if (event.member) \
        root[name] = *event.member
                OPTIONAL_BOOL(cwdPresent, "cwd_present");
                OPTIONAL_BOOL(cwdAuthoritative, "cwd_authoritative");
                OPTIONAL_BOOL(cwdChanged, "cwd_changed");
                OPTIONAL_BOOL(authorityChanged, "authority_changed");
                OPTIONAL_BOOL(shellTypePresent, "shell_type_present");
                OPTIONAL_BOOL(shellChanged, "shell_changed");
                OPTIONAL_BOOL(persistent, "persistent");
                OPTIONAL_BOOL(presentationPresent, "presentation_present");
                OPTIONAL_BOOL(requestedVisible, "requested_visible");
                OPTIONAL_BOOL(effectiveVisible, "effective_visible");
                OPTIONAL_BOOL(textPresent, "text_present");
                OPTIONAL_BOOL(automationNamePresent, "automation_name_present");
                OPTIONAL_BOOL(overrideActive, "override_active");
#undef OPTIONAL_BOOL

                Json::StreamWriterBuilder builder;
                builder["indentation"] = "";
                builder["emitUTF8"] = true;
                auto line = Json::writeString(builder, root);
                line.push_back('\n');
                return line;
            }

            bool _Open()
            {
                std::error_code error;
                const auto directory = IntelligentTerminal::LogDirVersioned();
                if (directory.empty())
                {
                    _ReportHealth(RichTabDiagnosticReason::LogDirectoryUnavailable, ERROR_PATH_NOT_FOUND);
                    return false;
                }
                std::filesystem::create_directories(directory, error);
                if (error)
                {
                    _ReportHealth(RichTabDiagnosticReason::LogDirectoryUnavailable, static_cast<uint32_t>(error.value()));
                    return false;
                }
                if (_HasReparseComponent(directory))
                {
                    _ReportHealth(RichTabDiagnosticReason::LogDirectoryUnavailable, ERROR_REPARSE_TAG_INVALID);
                    return false;
                }
                _PruneClosedRuns(directory);

                wchar_t name[96]{};
                _snwprintf_s(
                    name,
                    _TRUNCATE,
                    L"rich-tabs-%lu-%016llx.jsonl",
                    GetCurrentProcessId(),
                    _epoch);
                _path = directory / name;
                _backupPath = _path;
                _backupPath += L".1";
                const auto disposition = _ownsPath ? OPEN_EXISTING : CREATE_NEW;
                _file.reset(CreateFileW(
                    _path.c_str(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ,
                    nullptr,
                    disposition,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    nullptr));
                if (_file)
                {
                    _ownsPath = true;
                    LARGE_INTEGER size{};
                    _written = GetFileSizeEx(_file.get(), &size) ?
                                   static_cast<uint64_t>(size.QuadPart) :
                                   0;
                    LARGE_INTEGER end{};
                    end.QuadPart = static_cast<LONGLONG>(_written);
                    if (!SetFilePointerEx(_file.get(), end, nullptr, FILE_BEGIN))
                    {
                        const auto seekError = GetLastError();
                        _file.reset();
                        _ReportHealth(RichTabDiagnosticReason::LogOpenFailed, seekError);
                    }
                }
                else
                {
                    _ReportHealth(RichTabDiagnosticReason::LogOpenFailed, GetLastError());
                }
                return static_cast<bool>(_file);
            }

            bool _Rotate()
            {
                _file.reset();
                DeleteFileW(_backupPath.c_str());
                if (!MoveFileExW(_path.c_str(), _backupPath.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
                {
                    _file.reset(CreateFileW(
                        _path.c_str(),
                        GENERIC_WRITE,
                        FILE_SHARE_READ,
                        nullptr,
                        OPEN_EXISTING,
                        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                        nullptr));
                    LARGE_INTEGER size{};
                    if (_file && GetFileSizeEx(_file.get(), &size))
                    {
                        _written = static_cast<uint64_t>(size.QuadPart);
                        LARGE_INTEGER end{};
                        end.QuadPart = static_cast<LONGLONG>(_written);
                        SetFilePointerEx(_file.get(), end, nullptr, FILE_BEGIN);
                    }
                    _ReportHealth(RichTabDiagnosticReason::LogWriteFailed, GetLastError());
                    return false;
                }
                _file.reset(CreateFileW(
                    _path.c_str(),
                    GENERIC_WRITE,
                    FILE_SHARE_READ,
                    nullptr,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                    nullptr));
                _written = 0;
                _ownsPath = static_cast<bool>(_file);
                _rotationCompleted = true;
                if (!_file)
                {
                    _ReportHealth(RichTabDiagnosticReason::LogOpenFailed, GetLastError());
                }
                return static_cast<bool>(_file);
            }

            bool _TruncateToLastCompleteRecord() noexcept
            {
                if (!_file)
                {
                    return false;
                }
                LARGE_INTEGER offset{};
                offset.QuadPart = static_cast<LONGLONG>(_written);
                if (SetFilePointerEx(_file.get(), offset, nullptr, FILE_BEGIN))
                {
                    return SetEndOfFile(_file.get()) != FALSE;
                }
                return false;
            }

            bool _Write(const std::string& line)
            {
                if (!_file && !_Open())
                {
                    return false;
                }
                if (_written + line.size() > MaximumFileBytes && !_Rotate())
                {
                    return false;
                }
                DWORD written = 0;
                const auto writeSucceeded = WriteFile(
                    _file.get(),
                    line.data(),
                    static_cast<DWORD>(line.size()),
                    &written,
                    nullptr);
                if (!writeSucceeded || written != line.size())
                {
                    const auto error = writeSucceeded ? ERROR_WRITE_FAULT : GetLastError();
                    const auto truncated = _TruncateToLastCompleteRecord();
                    _file.reset();
                    if (!truncated)
                    {
                        _Rotate();
                        _file.reset();
                    }
                    _ReportHealth(RichTabDiagnosticReason::LogWriteFailed, error);
                    return false;
                }
                _written += written;
                return true;
            }

            void _Run() noexcept
            {
                try
                {
                    for (;;)
                    {
                        RichTabDiagnosticEventData queuedEvent;
                        uint64_t dropped = 0;
                        {
                            std::unique_lock lock{ _queueMutex };
                            _queueCondition.wait(lock, [&]() { return _shutdownRequested || !_queue.empty(); });
                            if (_queue.empty() && _shutdownRequested)
                            {
                                break;
                            }
                            _queuedBytes -= _EstimatedBytes(_queue.front());
                            queuedEvent = std::move(_queue.front());
                            _queue.pop_front();
                            _writing = true;
                            dropped = _dropped.exchange(0, std::memory_order_relaxed);
                        }

                        try
                        {
                            if (!_sinkStarted)
                            {
                                RichTabDiagnosticEventData event;
                                event.event = RichTabDiagnosticEvent::SinkState;
                                event.state = RichTabDiagnosticState::Started;
                                _sinkStarted = _Write(_Serialize(event));
                                _sinkDegraded = !_sinkStarted;
                            }
                            if (dropped != 0)
                            {
                                RichTabDiagnosticEventData event;
                                event.level = RichTabDiagnosticLevel::Warning;
                                event.event = RichTabDiagnosticEvent::SinkState;
                                event.state = RichTabDiagnosticState::EventsDropped;
                                event.reason = RichTabDiagnosticReason::QueueFull;
                                event.suppressedCount = dropped;
                                _Write(_Serialize(event));
                            }
                            const auto line = _Serialize(queuedEvent);
                            const auto writeSucceeded = _Write(line);
                            if (!writeSucceeded)
                            {
                                _sinkDegraded = true;
                            }
                            else if (_sinkDegraded)
                            {
                                _sinkDegraded = false;
                                RichTabDiagnosticEventData event;
                                event.event = RichTabDiagnosticEvent::SinkState;
                                event.state = RichTabDiagnosticState::Recovered;
                                _Write(_Serialize(event));
                            }
                            if (std::exchange(_rotationCompleted, false))
                            {
                                RichTabDiagnosticEventData event;
                                event.event = RichTabDiagnosticEvent::SinkState;
                                event.state = RichTabDiagnosticState::Rotated;
                                event.reason = RichTabDiagnosticReason::LogRotated;
                                _Write(_Serialize(event));
                            }
                        }
                        catch (...)
                        {
                            _ReportHealth(RichTabDiagnosticReason::SerializationFailed, ERROR_UNHANDLED_EXCEPTION);
                        }
                        {
                            std::lock_guard lock{ _queueMutex };
                            _writing = false;
                            if (_queue.empty())
                            {
                                _flushCondition.notify_all();
                            }
                        }
                    }

                    if (_file && !FlushFileBuffers(_file.get()))
                    {
                        _ReportHealth(RichTabDiagnosticReason::LogWriteFailed, GetLastError());
                    }
                    _file.reset();
                }
                catch (...)
                {
                    _ReportHealth(RichTabDiagnosticReason::LogWriteFailed, ERROR_UNHANDLED_EXCEPTION);
                }

                {
                    std::lock_guard lock{ _queueMutex };
                    _accepting = false;
                    _writing = false;
                    _workerStopped = true;
                }
                _flushCondition.notify_all();
                _workerStoppedCondition.notify_all();
            }

            void _ReportHealth(const RichTabDiagnosticReason reason, const uint32_t error) noexcept
            {
                if (!_healthFailureReported.exchange(true, std::memory_order_relaxed))
                {
                    OutputDebugStringW(L"Rich Tabs diagnostic sink is unavailable.\n");
                    if (_healthReporter)
                    {
                        _healthReporter(reason, error);
                    }
                }
            }

            const uint64_t _epoch;
            const bool _debugEnabled;
            std::atomic<uint64_t> _nextSequence{ 1 };

            std::mutex _observerMutex;
            std::condition_variable _observerCondition;
            std::shared_ptr<RichTabDiagnosticObserver> _observer;
            uint64_t _observerGeneration{ 0 };
            size_t _observerCallbacksInFlight{ 0 };

            std::mutex _queueMutex;
            std::condition_variable _queueCondition;
            std::condition_variable _flushCondition;
            std::condition_variable _workerStoppedCondition;
            std::deque<RichTabDiagnosticEventData> _queue;
            size_t _queuedBytes{ 0 };
            bool _started{ false };
            bool _accepting{ false };
            bool _shutdownRequested{ false };
            bool _workerStopped{ false };
            bool _writing{ false };
            std::atomic<uint64_t> _dropped{ 0 };

            std::mutex _aliasMutex;
            std::unordered_map<std::string, std::string> _sessionAliases;
            std::unordered_map<std::string, std::string> _providerAliases;
            std::unordered_map<std::string, std::string> _tabAliases;

            std::mutex _rateMutex;
            std::unordered_map<FailureKey, FailureWindowState, FailureKeyHash> _failureWindows;

            wil::unique_hfile _file;
            std::filesystem::path _path;
            std::filesystem::path _backupPath;
            uint64_t _written{ 0 };
            bool _ownsPath{ false };
            bool _sinkStarted{ false };
            bool _sinkDegraded{ false };
            bool _rotationCompleted{ false };
            RichTabDiagnosticHealthReporter _healthReporter{ nullptr };
            std::atomic<bool> _healthFailureReported{ false };
            std::thread _worker;
        };

        DiagnosticsState* _Diagnostics() noexcept
        {
            static auto state = []() noexcept -> DiagnosticsState* {
                try
                {
                    return new DiagnosticsState();
                }
                catch (...)
                {
                    return nullptr;
                }
            }();
            return state;
        }
    }

    bool StartRichTabDiagnostics(const RichTabDiagnosticHealthReporter healthReporter) noexcept
    {
        if (const auto state = _Diagnostics())
        {
            return state->Start(healthReporter);
        }
        return false;
    }

    void EmitRichTabDiagnostic(RichTabDiagnosticEventData event) noexcept
    {
        if (const auto state = _Diagnostics())
        {
            state->Emit(std::move(event));
        }
    }

    void FlushRichTabDiagnostics() noexcept
    {
        if (const auto state = _Diagnostics())
        {
            state->Flush();
        }
    }

    bool ShutdownRichTabDiagnostics(const uint32_t timeoutMilliseconds) noexcept
    {
        if (const auto state = _Diagnostics())
        {
            return state->Shutdown(timeoutMilliseconds);
        }
        return true;
    }

    std::string SerializeRichTabDiagnosticForTests(const RichTabDiagnosticEventData& event)
    {
        if (const auto state = _Diagnostics())
        {
            return state->SerializeForTests(event);
        }
        throw std::bad_alloc{};
    }

    ScopedRichTabDiagnosticObserver::ScopedRichTabDiagnosticObserver(RichTabDiagnosticObserver observer) :
        _generation{ [&]() {
            if (const auto state = _Diagnostics())
            {
                return state->SetObserver(std::move(observer));
            }
            throw std::bad_alloc{};
        }() }
    {
    }

    ScopedRichTabDiagnosticObserver::~ScopedRichTabDiagnosticObserver()
    {
        if (const auto state = _Diagnostics())
        {
            state->ClearObserver(_generation);
        }
    }
}
