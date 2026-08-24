// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderBroker.h"
#include "BuiltInProviderCatalog.h"
#include "RichTabDiagnostics.h"

#include <windows.h>
#include <bcrypt.h>
#include <winrt/base.h>

#include <algorithm>
#include <array>
#include <charconv>
#include <numeric>
#include <thread>
#include <unordered_set>
#include <utility>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr auto ProviderTimeout = std::chrono::seconds{ 5 };
        constexpr auto PublishLeaseLifetime = std::chrono::seconds{ 10 };
        constexpr auto PersistentPublishLeaseLifetime = std::chrono::seconds{ 90 };
        constexpr auto DetachedSessionRetention = std::chrono::seconds{ 30 };

        RichTabDiagnosticReason _DiagnosticReason(const ActivationEvent event) noexcept
        {
            switch (event)
            {
            case ActivationEvent::PaneConnected:
                return RichTabDiagnosticReason::PaneConnected;
            case ActivationEvent::WorkingDirectoryChanged:
                return RichTabDiagnosticReason::ContextChanged;
            case ActivationEvent::CommandFinished:
                return RichTabDiagnosticReason::CommandFinished;
            case ActivationEvent::TabActivated:
                return RichTabDiagnosticReason::TabActivated;
            case ActivationEvent::ManualRefresh:
                return RichTabDiagnosticReason::ManualRefresh;
            }
            return RichTabDiagnosticReason::None;
        }

        std::optional<std::wstring> _EnvironmentValue(const wchar_t* name)
        {
            const auto required = GetEnvironmentVariableW(name, nullptr, 0);
            if (required == 0)
            {
                return std::nullopt;
            }
            std::wstring value(required, L'\0');
            const auto written = GetEnvironmentVariableW(name, value.data(), required);
            if (written == 0 || written >= required)
            {
                return std::nullopt;
            }
            value.resize(written);
            return value;
        }

        bool _Handles(const Manifest& manifest, const ActivationEvent event)
        {
            return std::find(
                       manifest.activationEvents.begin(),
                       manifest.activationEvents.end(),
                       event) != manifest.activationEvents.end();
        }

        std::wstring _ToWide(const std::string_view value)
        {
            if (value.empty())
            {
                return {};
            }
            const auto required = MultiByteToWideChar(
                CP_UTF8,
                MB_ERR_INVALID_CHARS,
                value.data(),
                static_cast<int>(value.size()),
                nullptr,
                0);
            if (required <= 0)
            {
                return {};
            }
            std::wstring result(static_cast<size_t>(required), L'\0');
            if (MultiByteToWideChar(
                    CP_UTF8,
                    MB_ERR_INVALID_CHARS,
                    value.data(),
                    static_cast<int>(value.size()),
                    result.data(),
                    required) != required)
            {
                return {};
            }
            return result;
        }

        std::wstring _ValueText(const FieldValue& value)
        {
            return std::visit(
                [](const auto& current) -> std::wstring {
                    using T = std::decay_t<decltype(current)>;
                    if constexpr (std::is_same_v<T, std::string>)
                    {
                        return _ToWide(current);
                    }
                    else if constexpr (std::is_same_v<T, bool>)
                    {
                        return current ? L"true" : L"false";
                    }
                    else
                    {
                        return std::to_wstring(current);
                    }
                },
                value);
        }

        void _Append(std::wstring& target, const std::wstring_view value, const std::wstring_view separator)
        {
            if (value.empty())
            {
                return;
            }
            if (!target.empty())
            {
                target.append(separator);
            }
            target.append(value);
        }

        bool _SameProvider(const Registration& first, const Registration& second)
        {
            const auto& left = first.manifest;
            const auto& right = second.manifest;
            const auto sameFields =
                left.fields.size() == right.fields.size() &&
                std::equal(
                    left.fields.begin(),
                    left.fields.end(),
                    right.fields.begin(),
                    [](const auto& firstField, const auto& secondField) {
                        return firstField.id == secondField.id &&
                               firstField.displayName == secondField.displayName &&
                               firstField.type == secondField.type &&
                               firstField.defaultVisible == secondField.defaultVisible;
                    });
            return left.schemaVersion == right.schemaVersion &&
                   left.id == right.id &&
                   left.displayName == right.displayName &&
                   left.publisher == right.publisher &&
                   left.version == right.version &&
                   left.protocol.minimum == right.protocol.minimum &&
                   left.protocol.maximum == right.protocol.maximum &&
                   left.runtime.kind == right.runtime.kind &&
                   left.runtime.entrypoint == right.runtime.entrypoint &&
                   left.runtime.arguments == right.runtime.arguments &&
                   left.hosting == right.hosting &&
                   left.activationEvents == right.activationEvents &&
                   sameFields &&
                   left.extensionRoot == right.extensionRoot &&
                   first.kind == second.kind &&
                   first.root == second.root &&
                   first.payloadHash == second.payloadHash &&
                   first.enabled == second.enabled &&
                   first.integrityValid == second.integrityValid &&
                   first.sourceIdentity == second.sourceIdentity;
        }

        bool _SameProviders(
            const std::vector<Registration>& first,
            const std::vector<Registration>& second)
        {
            return first.size() == second.size() &&
                   std::equal(first.begin(), first.end(), second.begin(), _SameProvider);
        }

        const Registration* _FindProvider(
            const std::vector<Registration>& providers,
            const std::string_view id)
        {
            const auto found = std::find_if(providers.begin(), providers.end(), [&](const auto& provider) {
                return provider.manifest.id == id;
            });
            return found == providers.end() ? nullptr : &*found;
        }

        const ProviderPreference* _FindPreference(
            const std::vector<ProviderPreference>& preferences,
            const std::string_view id)
        {
            const auto found = std::find_if(preferences.begin(), preferences.end(), [&](const auto& preference) {
                return preference.id == id;
            });
            return found == preferences.end() ? nullptr : &*found;
        }

        std::vector<ProviderPreference> _NormalizePreferences(std::vector<ProviderPreference> preferences)
        {
            std::vector<ProviderPreference> normalized;
            std::unordered_set<std::string> providerIds;
            normalized.reserve(preferences.size());
            for (auto& preference : preferences)
            {
                if (preference.id.empty() || !providerIds.emplace(preference.id).second)
                {
                    continue;
                }
                if (preference.fields)
                {
                    std::vector<std::string> fields;
                    std::unordered_set<std::string> fieldIds;
                    fields.reserve(preference.fields->size());
                    for (auto& field : *preference.fields)
                    {
                        if (!field.empty() && fieldIds.emplace(field).second)
                        {
                            fields.emplace_back(std::move(field));
                        }
                    }
                    preference.fields = std::move(fields);
                }
                normalized.emplace_back(std::move(preference));
            }
            return normalized;
        }

        std::unordered_set<std::string> _ProvidersNeedingRefresh(
            const std::vector<Registration>& previous,
            const std::vector<Registration>& current)
        {
            std::unordered_set<std::string> result;
            for (const auto& provider : current)
            {
                const auto old = _FindProvider(previous, provider.manifest.id);
                if (!old || !_SameProvider(*old, provider))
                {
                    result.emplace(provider.manifest.id);
                }
            }
            return result;
        }

        RichTabDiagnosticEventData _CatalogDiagnosticEvent(
            const ProviderDescriptor& descriptor,
            const uint64_t catalogRevision)
        {
            auto state = RichTabDiagnosticState::Eligible;
            auto reason = RichTabDiagnosticReason::None;
            if (descriptor.shadowed)
            {
                state = RichTabDiagnosticState::Shadowed;
            }
            else if (!descriptor.integrityValid)
            {
                state = RichTabDiagnosticState::Rejected;
                reason = RichTabDiagnosticReason::IntegrityFailed;
            }
            else if (!descriptor.consentEnabled)
            {
                state = RichTabDiagnosticState::Disabled;
                reason =
                    ConsentRequirementFor(descriptor.source) == ProviderConsentRequirement::Required ?
                        RichTabDiagnosticReason::ConsentRequired :
                        RichTabDiagnosticReason::SourceNotAllowed;
            }
            else if (descriptor.effectiveEnabled)
            {
                state = RichTabDiagnosticState::Effective;
            }
            else if (descriptor.eligible)
            {
                state = RichTabDiagnosticState::Disabled;
                reason = RichTabDiagnosticReason::PreferenceDisabled;
            }
            else
            {
                state = RichTabDiagnosticState::Rejected;
                reason = RichTabDiagnosticReason::CatalogLoadFailed;
            }
            return RichTabDiagnosticEventData{
                .event = RichTabDiagnosticEvent::CatalogState,
                .state = state,
                .reason = reason,
                .providerId = descriptor.id,
                .catalogRevision = catalogRevision,
            };
        }
    }

    ProviderBroker& ProviderBroker::Instance()
    {
        static ProviderBroker instance;
        return instance;
    }

    ProviderBroker::ProviderBroker()
    {
        if (BCryptGenRandom(
                nullptr,
                reinterpret_cast<PUCHAR>(&_processEpoch),
                sizeof(_processEpoch),
                BCRYPT_USE_SYSTEM_PREFERRED_RNG) < 0 ||
            _processEpoch == 0)
        {
            _processEpoch = (GetTickCount64() << 16) ^ GetCurrentProcessId();
        }
        constexpr size_t WorkerCount{ 4 };
        _executorWorkers.reserve(WorkerCount);
        for (size_t index = 0; index < WorkerCount; ++index)
        {
            _executorWorkers.emplace_back([this]() {
                winrt::init_apartment(winrt::apartment_type::multi_threaded);
                for (;;)
                {
                    std::function<void()> work;
                    {
                        std::unique_lock lock{ _executorMutex };
                        _executorCondition.wait(lock, [&]() {
                            return _executorStopping || !_executorQueue.empty();
                        });
                        if (_executorStopping)
                        {
                            return;
                        }
                        work = std::move(_executorQueue.front());
                        _executorQueue.pop_front();
                    }
                    work();
                }
            });
        }
        _persistentSupervisor.SetEventCallback([this](const auto& event) {
            _OnPersistentProviderEvent(event);
        });
        _callbackLifetime = std::make_shared<CallbackLifetime>();
        _callbackLifetime->owner = this;
        _housekeepingThread = std::thread{ [this]() { _HousekeepingWorker(); } };
        ReloadProviders();
    }

    ProviderBroker::~ProviderBroker()
    {
        {
            std::lock_guard lock{ _callbackLifetime->mutex };
            _callbackLifetime->owner = nullptr;
        }
        {
            std::lock_guard lock{ _reloadMutex };
            _appExtensionWatcher.reset();
        }

        {
            std::lock_guard lock{ _housekeepingMutex };
            _housekeepingStopping = true;
        }
        _housekeepingCondition.notify_one();
        if (_housekeepingThread.joinable())
        {
            _housekeepingThread.join();
        }

        _persistentSupervisor.SetEventCallback({});
        _persistentSupervisor.Shutdown();

        {
            std::lock_guard lock{ _executorMutex };
            _executorStopping = true;
            _executorQueue.clear();
        }
        _executorCondition.notify_all();
        for (auto& worker : _executorWorkers)
        {
            if (worker.joinable())
            {
                worker.join();
            }
        }
    }

    void ProviderBroker::_Enqueue(std::function<void()> work)
    {
        {
            std::lock_guard lock{ _executorMutex };
            if (_executorStopping)
            {
                return;
            }
            _executorQueue.emplace_back(std::move(work));
        }
        _executorCondition.notify_one();
    }

    uint64_t ProviderBroker::ProcessEpoch() const noexcept
    {
        return _processEpoch;
    }

    std::string ProviderBroker::_CreatePublishLeaseLocked(
        const Registration& provider,
        const Request& request,
        const uint64_t generation,
        const std::chrono::steady_clock::duration lifetime,
        const uint64_t instanceGeneration,
        const bool persistent)
    {
        constexpr char hex[] = "0123456789abcdef";
        std::array<uint8_t, 32> bytes{};
        for (;;)
        {
            const auto status = BCryptGenRandom(
                nullptr,
                bytes.data(),
                static_cast<ULONG>(bytes.size()),
                BCRYPT_USE_SYSTEM_PREFERRED_RNG);
            if (status < 0)
            {
                return {};
            }
            std::string lease(bytes.size() * 2, '\0');
            for (size_t i = 0; i < bytes.size(); ++i)
            {
                lease[i * 2] = hex[bytes[i] >> 4];
                lease[i * 2 + 1] = hex[bytes[i] & 0x0f];
            }
            if (_publishLeases.emplace(
                                  lease,
                                  PublishLease{
                                      provider,
                                      request,
                                      generation,
                                      instanceGeneration,
                                      persistent,
                                      std::chrono::steady_clock::now() + lifetime })
                    .second)
            {
                return lease;
            }
        }
    }

    void ProviderBroker::_InvalidatePublishLeasesLocked(
        const std::string_view sessionId,
        const std::string_view providerId)
    {
        std::erase_if(_publishLeases, [&](const auto& item) {
            const auto& binding = item.second;
            return binding.request.sessionId == sessionId &&
                   (providerId.empty() || binding.provider.manifest.id == providerId);
        });
    }

    bool ProviderBroker::_ValidatePublishLeaseLocked(
        const PublishLease& binding,
        SessionState*& session,
        ProviderState*& providerState)
    {
        if (binding.request.processEpoch != _processEpoch ||
            binding.request.providerId != binding.provider.manifest.id ||
            binding.request.sessionId.empty() ||
            binding.request.requestId.empty() ||
            binding.persistent !=
                (binding.provider.manifest.hosting.kind == HostingKind::Persistent) ||
            (binding.persistent && binding.instanceGeneration == 0))
        {
            return false;
        }

        const auto sessionEntry = _sessions.find(binding.request.sessionId);
        if (sessionEntry == _sessions.end())
        {
            return false;
        }
        const auto stateEntry = sessionEntry->second.providers.find(binding.provider.manifest.id);
        const auto currentProvider = _FindProvider(_providers, binding.provider.manifest.id);
        if (stateEntry == sessionEntry->second.providers.end() ||
            !currentProvider ||
            !_SameProvider(*currentProvider, binding.provider) ||
            !stateEntry->second.running ||
            stateEntry->second.runningGeneration != binding.generation ||
            stateEntry->second.generation != binding.generation ||
            stateEntry->second.activeRequestId != binding.request.requestId ||
            sessionEntry->second.contextRevision != binding.request.contextRevision ||
            (binding.persistent &&
             stateEntry->second.persistentInstanceGeneration != binding.instanceGeneration))
        {
            return false;
        }

        session = &sessionEntry->second;
        providerState = &stateEntry->second;
        return true;
    }

    std::optional<std::string> ProviderBroker::_CreatePersistentControlFrameLocked(
        const Registration& provider,
        const Request& request,
        const uint64_t generation,
        const uint64_t instanceGeneration,
        const bool started)
    {
        const auto session = _sessions.find(request.sessionId);
        if (session == _sessions.end() ||
            session->second.contextRevision != request.contextRevision)
        {
            return std::nullopt;
        }
        const auto state = session->second.providers.find(provider.manifest.id);
        const auto currentProvider = _FindProvider(_providers, provider.manifest.id);
        if (state == session->second.providers.end() ||
            state->second.generation != generation ||
            state->second.activeRequestId != request.requestId ||
            !currentProvider ||
            !_SameProvider(*currentProvider, provider))
        {
            return std::nullopt;
        }

        _InvalidatePublishLeasesLocked(request.sessionId, provider.manifest.id);
        std::vector<PublishGrant> grants;
        std::vector<std::string> leases;
        grants.reserve(2);
        leases.reserve(2);
        for (size_t index = 0; index < 2; ++index)
        {
            auto lease = _CreatePublishLeaseLocked(
                provider,
                request,
                generation,
                PersistentPublishLeaseLifetime,
                instanceGeneration,
                true);
            if (lease.empty())
            {
                for (const auto& created : leases)
                {
                    _publishLeases.erase(created);
                }
                return std::nullopt;
            }
            grants.emplace_back(PublishGrant{
                request.requestId,
                lease,
                static_cast<uint64_t>(
                    std::chrono::duration_cast<std::chrono::milliseconds>(
                        PersistentPublishLeaseLifetime)
                        .count()),
            });
            leases.emplace_back(std::move(lease));
        }

        const auto frame = SerializeControlFrame(
            started ? ControlMessageKind::Start : ControlMessageKind::Refresh,
            request,
            grants,
            provider.manifest);
        if (!frame)
        {
            for (const auto& lease : leases)
            {
                _publishLeases.erase(lease);
            }
            return std::nullopt;
        }

        state->second.running = true;
        state->second.runningGeneration = generation;
        state->second.persistentInstanceGeneration = instanceGeneration;
        return frame.value;
    }

    PublishResult ProviderBroker::Publish(
        const std::string_view lease,
        const std::string_view snapshotJson)
    {
        PublishLease binding;
        std::optional<RichTabDiagnosticReason> initialRejection;
        {
            std::lock_guard lock{ _mutex };
            const auto leaseEntry = _publishLeases.find(std::string{ lease });
            if (leaseEntry == _publishLeases.end())
            {
                initialRejection = RichTabDiagnosticReason::LeaseUnknown;
            }
            else
            {
                binding = leaseEntry->second;
                _publishLeases.erase(leaseEntry);
                if (std::chrono::steady_clock::now() >= binding.expiresAt)
                {
                    initialRejection = RichTabDiagnosticReason::LeaseExpired;
                }
                else
                {
                    SessionState* session = nullptr;
                    ProviderState* providerState = nullptr;
                    if (!_ValidatePublishLeaseLocked(binding, session, providerState))
                    {
                        initialRejection = RichTabDiagnosticReason::LeaseStale;
                    }
                }
            }
        }
        if (initialRejection)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::PublishResult,
                .state = RichTabDiagnosticState::Rejected,
                .reason = *initialRejection,
                .sessionId = binding.request.sessionId,
                .providerId = binding.provider.manifest.id,
                .requestId = binding.request.requestId,
                .contextRevision = binding.request.contextRevision,
                .generation = binding.generation,
                .instanceGeneration = binding.instanceGeneration,
                .persistent = binding.persistent,
            });
            switch (*initialRejection)
            {
            case RichTabDiagnosticReason::LeaseUnknown:
                return { false, "publish lease is unknown or already consumed" };
            case RichTabDiagnosticReason::LeaseExpired:
                return { false, "publish lease has expired" };
            default:
                return { false, "publish lease is stale" };
            }
        }

        const auto parsed = ParseSnapshot(
            snapshotJson,
            binding.provider.manifest,
            binding.request.requestId);
        if (!parsed)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::PublishResult,
                .state = RichTabDiagnosticState::Rejected,
                .reason = RichTabDiagnosticReason::SnapshotInvalid,
                .sessionId = binding.request.sessionId,
                .providerId = binding.provider.manifest.id,
                .requestId = binding.request.requestId,
                .contextRevision = binding.request.contextRevision,
                .generation = binding.generation,
                .instanceGeneration = binding.instanceGeneration,
                .persistent = binding.persistent,
            });
            if (binding.persistent)
            {
                _ReplenishPersistentGrants(binding);
            }
            return { false, parsed.errors.empty() ? "snapshot is invalid" : parsed.errors.front() };
        }

        std::vector<Callback> callbacks;
        BrokerUpdate update;
        bool staleAfterParsing = false;
        bool compositionChanged = false;
        {
            std::lock_guard lock{ _mutex };
            SessionState* session = nullptr;
            ProviderState* providerState = nullptr;
            if (!_ValidatePublishLeaseLocked(binding, session, providerState))
            {
                staleAfterParsing = true;
            }
            else
            {
                UpdateFieldChangeSequences(
                    *parsed.value,
                    providerState->fieldBaseline,
                    providerState->fieldChangeSequences,
                    session->nextFieldChangeSequence);
                providerState->snapshot = *parsed.value;
                providerState->publishedGeneration = binding.generation;
                ++session->updateSequence;
                providerState->publishedUpdateSequence = session->updateSequence;
                update = _UpdateFor(binding.request.sessionId, *session);
                const auto presentationPresent = update.presentation.has_value();
                compositionChanged =
                    !session->diagnosticPresentationPresent ||
                    *session->diagnosticPresentationPresent != presentationPresent;
                session->diagnosticPresentationPresent = presentationPresent;
                callbacks.reserve(session->callbacks.size());
                for (const auto& [_, callback] : session->callbacks)
                {
                    callbacks.emplace_back(callback);
                }
            }
        }
        if (staleAfterParsing)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::PublishResult,
                .state = RichTabDiagnosticState::Rejected,
                .reason = RichTabDiagnosticReason::LeaseStale,
                .sessionId = binding.request.sessionId,
                .providerId = binding.provider.manifest.id,
                .requestId = binding.request.requestId,
                .contextRevision = binding.request.contextRevision,
                .generation = binding.generation,
                .instanceGeneration = binding.instanceGeneration,
                .persistent = binding.persistent,
            });
            return { false, "publish lease is stale" };
        }

        EmitRichTabDiagnostic({
            .level = RichTabDiagnosticLevel::Debug,
            .event = RichTabDiagnosticEvent::PublishResult,
            .state = RichTabDiagnosticState::Accepted,
            .sessionId = binding.request.sessionId,
            .providerId = binding.provider.manifest.id,
            .requestId = binding.request.requestId,
            .sessionIncarnation = update.sessionIncarnation,
            .contextRevision = binding.request.contextRevision,
            .updateSequence = update.updateSequence,
            .generation = binding.generation,
            .instanceGeneration = binding.instanceGeneration,
            .fieldCount = parsed.value->fields.size(),
            .persistent = binding.persistent,
            .presentationPresent = update.presentation.has_value(),
        });
        EmitRichTabDiagnostic({
            .level = compositionChanged ?
                         RichTabDiagnosticLevel::Info :
                         RichTabDiagnosticLevel::Debug,
            .event = RichTabDiagnosticEvent::CompositionState,
            .state = update.presentation ? RichTabDiagnosticState::Nonempty : RichTabDiagnosticState::Empty,
            .reason = update.presentation ? RichTabDiagnosticReason::None : RichTabDiagnosticReason::EmptyMetadata,
            .sessionId = binding.request.sessionId,
            .sessionIncarnation = update.sessionIncarnation,
            .contextRevision = binding.request.contextRevision,
            .updateSequence = update.updateSequence,
            .snapshotCount = size_t{ 1 },
            .fieldCount = parsed.value->fields.size(),
            .presentationPresent = update.presentation.has_value(),
        });
        if (binding.persistent)
        {
            try
            {
                _ReplenishPersistentGrants(binding);
            }
            catch (...)
            {
                EmitRichTabDiagnostic({
                    .level = RichTabDiagnosticLevel::Warning,
                    .event = RichTabDiagnosticEvent::RequestState,
                    .state = RichTabDiagnosticState::Failed,
                    .reason = RichTabDiagnosticReason::LeaseCreationFailed,
                    .sessionId = binding.request.sessionId,
                    .providerId = binding.provider.manifest.id,
                    .requestId = binding.request.requestId,
                    .sessionIncarnation = update.sessionIncarnation,
                    .contextRevision = binding.request.contextRevision,
                    .updateSequence = update.updateSequence,
                    .generation = binding.generation,
                    .instanceGeneration = binding.instanceGeneration,
                    .persistent = true,
                });
            }
        }
        _DeliverCallbacks(callbacks, update);
        return { true, "snapshot committed" };
    }

    void ProviderBroker::_ReplenishPersistentGrants(const PublishLease& binding)
    {
        std::optional<std::string> frame;
        {
            std::lock_guard lock{ _mutex };
            SessionState* session = nullptr;
            ProviderState* providerState = nullptr;
            if (!binding.persistent ||
                !_ValidatePublishLeaseLocked(binding, session, providerState))
            {
                return;
            }

            const auto now = std::chrono::steady_clock::now();
            std::erase_if(_publishLeases, [&](const auto& item) {
                const auto& current = item.second;
                return current.persistent &&
                       current.request.sessionId == binding.request.sessionId &&
                       current.provider.manifest.id == binding.provider.manifest.id &&
                       current.expiresAt <= now;
            });

            size_t outstanding = 0;
            for (const auto& [_, current] : _publishLeases)
            {
                if (current.persistent &&
                    current.request.sessionId == binding.request.sessionId &&
                    current.provider.manifest.id == binding.provider.manifest.id &&
                    current.generation == binding.generation &&
                    current.instanceGeneration == binding.instanceGeneration &&
                    current.request.requestId == binding.request.requestId)
                {
                    ++outstanding;
                }
            }
            if (outstanding >= 2)
            {
                return;
            }

            std::vector<PublishGrant> grants;
            std::vector<std::string> leases;
            for (; outstanding < 2; ++outstanding)
            {
                auto lease = _CreatePublishLeaseLocked(
                    binding.provider,
                    binding.request,
                    binding.generation,
                    PersistentPublishLeaseLifetime,
                    binding.instanceGeneration,
                    true);
                if (lease.empty())
                {
                    break;
                }
                grants.emplace_back(PublishGrant{
                    binding.request.requestId,
                    lease,
                    static_cast<uint64_t>(
                        std::chrono::duration_cast<std::chrono::milliseconds>(
                            PersistentPublishLeaseLifetime)
                            .count()),
                });
                leases.emplace_back(std::move(lease));
            }
            if (grants.empty())
            {
                return;
            }

            const auto serialized = SerializeControlFrame(
                ControlMessageKind::Lease,
                std::nullopt,
                grants,
                binding.provider.manifest);
            if (!serialized)
            {
                for (const auto& created : leases)
                {
                    _publishLeases.erase(created);
                }
                return;
            }
            frame = std::move(serialized.value);
        }

        _persistentSupervisor.SendLease(
            { binding.provider.manifest.id, binding.request.sessionId },
            binding.instanceGeneration,
            binding.generation,
            std::move(*frame));
    }

    uint64_t ProviderBroker::_RegistryStamp() const noexcept
    {
        const auto stamp = [&](const std::filesystem::path& path) {
            std::error_code error;
            const auto value = std::filesystem::last_write_time(path, error);
            return error ? uint64_t{ 0 } :
                           static_cast<uint64_t>(value.time_since_epoch().count());
        };
        const auto registrations = stamp(_registry.Root() / L"registrations");
        const auto consents = stamp(_registry.Root() / L"consents");
        return registrations ^
               (consents + 0x9e3779b97f4a7c15ull +
                (registrations << 6) +
                (registrations >> 2));
    }

    void ProviderBroker::_ReloadProvidersIfChanged()
    {
        const auto stamp = _RegistryStamp();
        if (stamp != _registryStamp)
        {
            ReloadProviders();
        }
    }

    void ProviderBroker::ReloadProviders()
    {
        _ReloadProvidersFromSources();
        _ScheduleAppExtensionDiscovery();
    }

    ProviderCatalogSnapshot ProviderBroker::BuildCatalog(
        std::vector<Registration> candidates)
    {
        ProviderCatalogSnapshot result;
        std::unordered_map<std::string, int> winningPrecedence;
        for (const auto& provider : candidates)
        {
            const auto precedence =
                ProviderSourcePrecedence(provider.sourceIdentity.kind);
            const auto [entry, inserted] =
                winningPrecedence.try_emplace(provider.manifest.id, precedence);
            if (!inserted)
            {
                entry->second = (std::max)(entry->second, precedence);
            }
        }

        std::stable_sort(candidates.begin(), candidates.end(), [](const auto& first, const auto& second) {
            return ProviderSourcePrecedence(first.sourceIdentity.kind) >
                   ProviderSourcePrecedence(second.sourceIdentity.kind);
        });
        std::unordered_set<std::string> selected;
        for (auto& provider : candidates)
        {
            const auto precedence =
                ProviderSourcePrecedence(provider.sourceIdentity.kind);
            const auto shadowed =
                precedence < winningPrecedence[provider.manifest.id] ||
                !selected.emplace(provider.manifest.id).second;
            const auto sourceAllowed =
                ConsentRequirementFor(provider.sourceIdentity.kind) !=
                ProviderConsentRequirement::SourceNotAllowed;
            const auto consentEnabled =
                provider.sourceIdentity.kind == ProviderSourceKind::BuiltIn ||
                (sourceAllowed && provider.enabled);
            const auto eligible =
                consentEnabled &&
                provider.integrityValid &&
                !shadowed;
            result.descriptors.emplace_back(ProviderDescriptor{
                provider.manifest.id,
                provider.manifest.displayName,
                provider.sourceIdentity.kind,
                consentEnabled,
                provider.integrityValid,
                eligible,
                eligible,
                shadowed,
                ProviderConsentKey(provider.sourceIdentity).value_or(""),
                provider.manifest.fields });
            if (eligible)
            {
                result.available.emplace_back(std::move(provider));
            }
        }
        std::sort(result.available.begin(), result.available.end(), [](const auto& first, const auto& second) {
            return first.manifest.id < second.manifest.id;
        });
        std::sort(result.descriptors.begin(), result.descriptors.end(), [](const auto& first, const auto& second) {
            if (first.id != second.id)
            {
                return first.id < second.id;
            }
            return ProviderSourcePrecedence(first.source) >
                   ProviderSourcePrecedence(second.source);
        });
        return result;
    }

    void ProviderBroker::_ReloadProvidersFromSources()
    {
        std::lock_guard reloadLock{ _reloadMutex };
        auto builtIns = BuiltInProviderCatalog::Load(BuiltInProviderCatalog::PackageRoot());
        auto listed = _registry.List();
        std::vector<Registration> candidates;
        if (builtIns.value)
        {
            candidates = std::move(*builtIns.value);
        }
        for (const auto& discovered : _appExtensionDiscovery.providers)
        {
            if (discovered.status != AppExtensionDiscoveryStatus::Discovered ||
                !discovered.manifest)
            {
                continue;
            }
            auto consent = _registry.AppExtensionConsentEnabled(
                discovered.identity);
            Registration registration;
            registration.manifest = *discovered.manifest;
            registration.kind = RegistrationKind::Managed;
            registration.root = discovered.publicPath;
            registration.enabled = consent.value.value_or(false);
            registration.integrityValid = true;
            registration.sourceIdentity = discovered.identity;
            candidates.emplace_back(std::move(registration));
        }
        if (listed.value)
        {
            for (auto& provider : *listed.value)
            {
                candidates.emplace_back(std::move(provider));
            }
        }
        auto merged = BuildCatalog(std::move(candidates));
        std::vector<RichTabDiagnosticEventData> catalogEvents;
        const auto catalogErrorCount =
            builtIns.errors.size() +
            listed.errors.size() +
            _appExtensionDiscovery.diagnostics.size() +
            std::accumulate(
                _appExtensionDiscovery.providers.begin(),
                _appExtensionDiscovery.providers.end(),
                size_t{ 0 },
                [](const size_t total, const auto& provider) {
                    return total + provider.diagnostics.size();
                });
        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        std::vector<std::string> sessionsToRefresh;
        std::unordered_set<std::string> refreshProviders;
        std::vector<PersistentProviderKey> persistentProvidersToStop;
        std::vector<ProviderDescriptor> diagnosticCatalog;
        uint64_t catalogRevision = 0;
        {
            std::lock_guard lock{ _mutex };
            const auto previous = _providers;
            _catalog = std::move(merged.descriptors);
            _availableProviders = std::move(merged.available);
            _providers = _EffectiveProvidersLocked();
            _UpdateCatalogEffectiveStateLocked();
            ++_catalogRevision;
            catalogRevision = _catalogRevision;
            diagnosticCatalog = _catalog;
            refreshProviders = _ProvidersNeedingRefresh(previous, _providers);
            const auto presentationChanged = !_SameProviders(previous, _providers);
            for (auto& [sessionId, session] : _sessions)
            {
                for (auto& [id, provider] : session.providers)
                {
                    const auto oldRegistration = _FindProvider(previous, id);
                    const auto newRegistration = _FindProvider(_providers, id);
                    if (!newRegistration || !oldRegistration || !_SameProvider(*oldRegistration, *newRegistration))
                    {
                        if (oldRegistration &&
                            oldRegistration->manifest.hosting.kind == HostingKind::Persistent)
                        {
                            persistentProvidersToStop.emplace_back(
                                PersistentProviderKey{ id, sessionId });
                        }
                        _InvalidatePublishLeasesLocked(sessionId, id);
                        provider.generation = _nextGeneration++;
                        provider.running = false;
                        provider.runningGeneration = 0;
                        provider.persistentInstanceGeneration = 0;
                        provider.activeRequestId.clear();
                        provider.pending.reset();
                        provider.snapshot.reset();
                        provider.fieldBaseline.reset();
                        provider.fieldChangeSequences.clear();
                    }
                }

                if (presentationChanged)
                {
                    ++session.updateSequence;
                    const auto update = _UpdateFor(sessionId, session);
                    for (const auto& [_, callback] : session.callbacks)
                    {
                        notifications.emplace_back(callback, update);
                    }
                }
                if (!session.callbacks.empty() && !refreshProviders.empty())
                {
                    sessionsToRefresh.emplace_back(sessionId);
                }
            }
            for (const auto& key : persistentProvidersToStop)
            {
                _persistentSupervisor.Stop(key);
            }
            _registryStamp = _RegistryStamp();
        }

        catalogEvents.reserve(diagnosticCatalog.size() + 1);
        for (const auto& descriptor : diagnosticCatalog)
        {
            catalogEvents.emplace_back(_CatalogDiagnosticEvent(descriptor, catalogRevision));
        }
        if (catalogErrorCount != 0)
        {
            catalogEvents.emplace_back(RichTabDiagnosticEventData{
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::CatalogState,
                .state = RichTabDiagnosticState::Rejected,
                .reason = RichTabDiagnosticReason::CatalogLoadFailed,
                .catalogRevision = catalogRevision,
                .skippedCount = catalogErrorCount,
            });
        }
        for (const auto& [callback, update] : notifications)
        {
            _DeliverCallback(callback, update);
        }
        for (auto& event : catalogEvents)
        {
            EmitRichTabDiagnostic(std::move(event));
        }
        for (const auto& sessionId : sessionsToRefresh)
        {
            _Refresh(
                sessionId,
                ActivationEvent::ManualRefresh,
                true,
                refreshProviders,
                false,
                true);
        }
    }

    void ProviderBroker::_ScheduleAppExtensionDiscovery()
    {
        {
            std::lock_guard lock{ _reloadMutex };
            if (_appExtensionDiscoveryScheduled)
            {
                _appExtensionDiscoveryPending = true;
                return;
            }
            _appExtensionDiscoveryScheduled = true;
        }
        _Enqueue([this]() {
            const std::weak_ptr weakLifetime{ _callbackLifetime };
            auto watcher = AppExtensionProviderCatalog::WatchForChanges([weakLifetime]() {
                if (const auto lifetime = weakLifetime.lock())
                {
                    std::lock_guard lock{ lifetime->mutex };
                    if (lifetime->owner)
                    {
                        lifetime->owner->_ScheduleAppExtensionDiscovery();
                    }
                }
            });
            auto discovery = AppExtensionProviderCatalog::DiscoverAsync().get();
            bool rediscover = false;
            {
                std::lock_guard lock{ _reloadMutex };
                _appExtensionDiscovery = std::move(discovery);
                if (!_appExtensionWatcher)
                {
                    _appExtensionWatcher = std::move(watcher);
                }
                _appExtensionDiscoveryScheduled = false;
                rediscover = std::exchange(
                    _appExtensionDiscoveryPending,
                    false);
            }
            _ReloadProvidersFromSources();
            if (rediscover)
            {
                _ScheduleAppExtensionDiscovery();
            }
        });
    }

    std::vector<Registration> ProviderBroker::_EffectiveProvidersLocked() const
    {
        std::vector<Registration> effective;
        effective.reserve(_availableProviders.size());
        std::unordered_set<std::string> added;
        for (const auto& preference : _preferences)
        {
            const auto provider = _FindProvider(_availableProviders, preference.id);
            if (provider && preference.enabled.value_or(true))
            {
                effective.emplace_back(*provider);
                added.emplace(preference.id);
            }
        }
        for (const auto& provider : _availableProviders)
        {
            if (!added.contains(provider.manifest.id))
            {
                const auto preference = _FindPreference(_preferences, provider.manifest.id);
                if (!preference || preference->enabled.value_or(true))
                {
                    effective.emplace_back(provider);
                }
            }
        }
        return effective;
    }

    void ProviderBroker::_UpdateCatalogEffectiveStateLocked()
    {
        for (auto& descriptor : _catalog)
        {
            const auto preference = _FindPreference(_preferences, descriptor.id);
            descriptor.effectiveEnabled =
                descriptor.eligible &&
                (!preference || preference->enabled.value_or(true));
        }
    }

    void ProviderBroker::ApplyPreferences(
        std::vector<ProviderPreference> preferences,
        const bool prioritizeRecentlyUpdatedFields)
    {
        preferences = _NormalizePreferences(std::move(preferences));
        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        std::vector<std::string> sessionsToRefresh;
        std::unordered_set<std::string> refreshProviders;
        std::vector<PersistentProviderKey> persistentProvidersToStop;
        std::vector<RichTabDiagnosticEventData> catalogEvents;
        {
            std::lock_guard lock{ _mutex };
            if (_preferences == preferences &&
                _prioritizeRecentlyUpdatedFields == prioritizeRecentlyUpdatedFields)
            {
                return;
            }

            const auto previous = _providers;
            _preferences = std::move(preferences);
            _prioritizeRecentlyUpdatedFields = prioritizeRecentlyUpdatedFields;
            _providers = _EffectiveProvidersLocked();
            _UpdateCatalogEffectiveStateLocked();
            ++_catalogRevision;
            catalogEvents.reserve(_catalog.size());
            for (const auto& descriptor : _catalog)
            {
                catalogEvents.emplace_back(_CatalogDiagnosticEvent(descriptor, _catalogRevision));
            }
            refreshProviders = _ProvidersNeedingRefresh(previous, _providers);

            for (auto& [sessionId, session] : _sessions)
            {
                for (auto& [id, provider] : session.providers)
                {
                    if (_FindProvider(previous, id) && !_FindProvider(_providers, id))
                    {
                        const auto oldRegistration = _FindProvider(previous, id);
                        if (oldRegistration &&
                            oldRegistration->manifest.hosting.kind == HostingKind::Persistent)
                        {
                            persistentProvidersToStop.emplace_back(
                                PersistentProviderKey{ id, sessionId });
                        }
                        _InvalidatePublishLeasesLocked(sessionId, id);
                        provider.generation = _nextGeneration++;
                        provider.running = false;
                        provider.runningGeneration = 0;
                        provider.persistentInstanceGeneration = 0;
                        provider.activeRequestId.clear();
                        provider.pending.reset();
                        provider.snapshot.reset();
                        provider.fieldBaseline.reset();
                        provider.fieldChangeSequences.clear();
                    }
                }

                ++session.updateSequence;
                const auto update = _UpdateFor(sessionId, session);
                for (const auto& [_, callback] : session.callbacks)
                {
                    notifications.emplace_back(callback, update);
                }
                if (!session.callbacks.empty() && !refreshProviders.empty())
                {
                    sessionsToRefresh.emplace_back(sessionId);
                }
            }
            for (const auto& key : persistentProvidersToStop)
            {
                _persistentSupervisor.Stop(key);
            }
        }

        for (const auto& [callback, update] : notifications)
        {
            _DeliverCallback(callback, update);
        }
        for (auto& event : catalogEvents)
        {
            EmitRichTabDiagnostic(std::move(event));
        }
        for (const auto& sessionId : sessionsToRefresh)
        {
            _Refresh(
                sessionId,
                ActivationEvent::ManualRefresh,
                true,
                refreshProviders,
                false,
                true);
        }
    }

    std::vector<ProviderDescriptor> ProviderBroker::Catalog()
    {
        _ReloadProvidersIfChanged();
        std::lock_guard lock{ _mutex };
        return _catalog;
    }

    RegistryResult<bool> ProviderBroker::SetProviderConsent(
        const std::string_view id,
        const std::string_view consentKey,
        const bool enabled)
    {
        {
            std::lock_guard lock{ _mutex };
            const auto descriptor = std::find_if(
                _catalog.begin(),
                _catalog.end(),
                [&](const auto& current) {
                    return current.id == id &&
                           current.consentKey == consentKey;
                });
            if (descriptor == _catalog.end() ||
                descriptor->source != ProviderSourceKind::AppExtension ||
                descriptor->shadowed ||
                !descriptor->integrityValid ||
                descriptor->consentKey.empty())
            {
                RegistryResult<bool> result;
                result.errors.emplace_back(
                    "The confirmed App Extension identity is no longer eligible");
                return result;
            }
        }

        ProviderSourceIdentity appExtensionIdentity;
        {
            std::lock_guard lock{ _reloadMutex };
            const auto found = std::find_if(
                _appExtensionDiscovery.providers.begin(),
                _appExtensionDiscovery.providers.end(),
                [&](const auto& provider) {
                    return provider.status == AppExtensionDiscoveryStatus::Discovered &&
                           provider.manifest &&
                           provider.manifest->id == id &&
                           ProviderConsentKey(provider.identity).value_or("") == consentKey;
                });
            if (found != _appExtensionDiscovery.providers.end())
            {
                appExtensionIdentity = found->identity;
            }
        }

        RegistryResult<bool> result;
        if (appExtensionIdentity.kind == ProviderSourceKind::AppExtension)
        {
            result = _registry.SetAppExtensionConsentEnabled(
                appExtensionIdentity,
                enabled);
        }
        else
        {
            result.errors.emplace_back(
                "The confirmed App Extension identity is no longer installed");
        }
        if (result)
        {
            ReloadProviders();
        }
        return result;
    }

    ProviderBroker::AttachmentId ProviderBroker::Attach(SessionContext context, Callback callback)
    {
        _ReloadProvidersIfChanged();
        if (context.sessionId.empty())
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::AttachmentState,
                .state = RichTabDiagnosticState::Skipped,
                .reason = RichTabDiagnosticReason::SessionMissing,
            });
            return 0;
        }

        AttachmentId attachment = 0;
        std::optional<BrokerUpdate> existing;
        bool contextChanged = false;
        bool sessionCreated = false;
        const auto cwdPresent = !context.workingDirectory.empty();
        const auto cwdAuthoritative = context.workingDirectoryAuthoritative;
        const auto shellTypePresent = context.shellType.has_value();
        auto initialCallback = callback;
        std::vector<std::string> prunedSessions;
        {
            std::lock_guard lock{ _mutex };
            prunedSessions = _PruneDetachedSessionsLocked();
            for (const auto& sessionId : prunedSessions)
            {
                _persistentSupervisor.StopSession(sessionId);
            }
            attachment = _nextAttachment++;
            auto [sessionEntry, inserted] = _sessions.try_emplace(context.sessionId);
            auto& session = sessionEntry->second;
            session.detachedAt.reset();
            if (inserted)
            {
                sessionCreated = true;
                session.sessionIncarnation = _nextSessionIncarnation++;
                session.context = context;
            }
            else if (
                session.context.workingDirectory != context.workingDirectory ||
                session.context.workingDirectoryAuthoritative != context.workingDirectoryAuthoritative ||
                session.context.shellType != context.shellType)
            {
                contextChanged = true;
                session.context.workingDirectory = std::move(context.workingDirectory);
                session.context.workingDirectoryAuthoritative = context.workingDirectoryAuthoritative;
                session.context.shellType = std::move(context.shellType);
                ++session.contextRevision;
                _InvalidatePublishLeasesLocked(session.context.sessionId);
                for (auto& [_, provider] : session.providers)
                {
                    provider.generation = _nextGeneration++;
                    provider.running = false;
                    provider.runningGeneration = 0;
                    provider.persistentInstanceGeneration = 0;
                    provider.activeRequestId.clear();
                    provider.pending.reset();
                    provider.snapshot.reset();
                    provider.fieldBaseline.reset();
                    provider.fieldChangeSequences.clear();
                }
                session.nextFieldChangeSequence = 0;
                ++session.updateSequence;
            }
            session.callbacks.emplace(attachment, std::move(callback));
            _attachmentSessions.emplace(attachment, session.context.sessionId);
            existing = _UpdateFor(session.context.sessionId, session);
        }

        for (const auto& prunedSession : prunedSessions)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::AttachmentState,
                .state = RichTabDiagnosticState::GraceExpired,
                .reason = RichTabDiagnosticReason::DetachedRetentionExpired,
                .sessionId = prunedSession,
            });
        }
        if (contextChanged)
        {
            _persistentSupervisor.StopSession(existing->sessionId);
        }
        if (sessionCreated || contextChanged || existing->presentation)
        {
            _DeliverCallback(initialCallback, *existing);
        }
        EmitRichTabDiagnostic({
            .event = RichTabDiagnosticEvent::AttachmentState,
            .state = RichTabDiagnosticState::Attached,
            .reason = RichTabDiagnosticReason::PaneConnected,
            .sessionId = existing->sessionId,
            .attachmentId = attachment,
            .sessionIncarnation = existing->sessionIncarnation,
            .contextRevision = existing->contextRevision,
            .updateSequence = existing->updateSequence,
            .cwdPresent = cwdPresent,
            .cwdAuthoritative = cwdAuthoritative,
            .shellTypePresent = shellTypePresent,
            .presentationPresent = existing->presentation.has_value(),
        });
        _Refresh(
            existing->sessionId,
            ActivationEvent::PaneConnected,
            true,
            {},
            contextChanged,
            true);
        return attachment;
    }

    void ProviderBroker::Detach(const AttachmentId attachment)
    {
        std::vector<std::string> prunedSessions;
        std::string detachedSessionId;
        bool enteredGrace = false;
        {
            std::lock_guard lock{ _mutex };
            const auto attached = _attachmentSessions.find(attachment);
            if (attached == _attachmentSessions.end())
            {
                return;
            }
            detachedSessionId = attached->second;
            if (const auto session = _sessions.find(attached->second); session != _sessions.end())
            {
                session->second.callbacks.erase(attachment);
                if (session->second.callbacks.empty())
                {
                    enteredGrace = true;
                    for (auto& [id, provider] : session->second.providers)
                    {
                        const auto registration = _FindProvider(_providers, id);
                        if (!registration ||
                            registration->manifest.hosting.kind != HostingKind::Persistent)
                        {
                            provider.generation = _nextGeneration++;
                            provider.activeRequestId.clear();
                            provider.pending.reset();
                        }
                    }
                    session->second.detachedAt = std::chrono::steady_clock::now();
                }
            }
            _attachmentSessions.erase(attached);
            prunedSessions = _PruneDetachedSessionsLocked();
            for (const auto& sessionId : prunedSessions)
            {
                _persistentSupervisor.StopSession(sessionId);
            }
        }
        EmitRichTabDiagnostic({
            .event = RichTabDiagnosticEvent::AttachmentState,
            .state = RichTabDiagnosticState::Detached,
            .sessionId = detachedSessionId,
            .attachmentId = attachment,
        });
        if (enteredGrace)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::AttachmentState,
                .state = RichTabDiagnosticState::GraceStarted,
                .sessionId = detachedSessionId,
            });
        }
        for (const auto& prunedSession : prunedSessions)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::AttachmentState,
                .state = RichTabDiagnosticState::GraceExpired,
                .reason = RichTabDiagnosticReason::DetachedRetentionExpired,
                .sessionId = prunedSession,
            });
        }
        _housekeepingCondition.notify_one();
    }

    void ProviderBroker::UpdateContext(
        const AttachmentId attachment,
        std::filesystem::path workingDirectory,
        const bool authoritative,
        std::optional<std::string> shellType)
    {
        _ReloadProvidersIfChanged();
        std::string sessionId;
        std::vector<Callback> callbacks;
        BrokerUpdate update;
        bool cwdChanged = false;
        bool authorityChanged = false;
        bool shellChanged = false;
        bool compositionChanged = false;
        const auto cwdPresent = !workingDirectory.empty();
        const auto shellTypePresent = shellType.has_value();
        {
            std::lock_guard lock{ _mutex };
            const auto attached = _attachmentSessions.find(attachment);
            if (attached == _attachmentSessions.end())
            {
                return;
            }
            auto& session = _sessions.at(attached->second);
            if (session.context.workingDirectory == workingDirectory &&
                session.context.workingDirectoryAuthoritative == authoritative &&
                session.context.shellType == shellType)
            {
                return;
            }
            cwdChanged = session.context.workingDirectory != workingDirectory;
            authorityChanged = session.context.workingDirectoryAuthoritative != authoritative;
            shellChanged = session.context.shellType != shellType;
            session.context.workingDirectory = std::move(workingDirectory);
            session.context.workingDirectoryAuthoritative = authoritative;
            session.context.shellType = std::move(shellType);
            ++session.contextRevision;
            _InvalidatePublishLeasesLocked(session.context.sessionId);
            for (auto& [_, provider] : session.providers)
            {
                provider.generation = _nextGeneration++;
                provider.running = false;
                provider.runningGeneration = 0;
                provider.persistentInstanceGeneration = 0;
                provider.activeRequestId.clear();
                provider.pending.reset();
                provider.snapshot.reset();
                provider.fieldBaseline.reset();
                provider.fieldChangeSequences.clear();
            }
            session.nextFieldChangeSequence = 0;
            ++session.updateSequence;
            sessionId = session.context.sessionId;
            update = _UpdateFor(sessionId, session);
            const auto presentationPresent = update.presentation.has_value();
            compositionChanged =
                !session.diagnosticPresentationPresent ||
                *session.diagnosticPresentationPresent != presentationPresent;
            session.diagnosticPresentationPresent = presentationPresent;
            callbacks.reserve(session.callbacks.size());
            for (const auto& [_, callback] : session.callbacks)
            {
                callbacks.emplace_back(callback);
            }
        }
        _persistentSupervisor.StopSession(sessionId);
        _DeliverCallbacks(callbacks, update);
        EmitRichTabDiagnostic({
            .event = RichTabDiagnosticEvent::ContextState,
            .state = RichTabDiagnosticState::Changed,
            .reason = RichTabDiagnosticReason::ContextChanged,
            .sessionId = sessionId,
            .sessionIncarnation = update.sessionIncarnation,
            .contextRevision = update.contextRevision,
            .updateSequence = update.updateSequence,
            .cwdPresent = cwdPresent,
            .cwdAuthoritative = authoritative,
            .cwdChanged = cwdChanged,
            .authorityChanged = authorityChanged,
            .shellTypePresent = shellTypePresent,
            .shellChanged = shellChanged,
            .presentationPresent = update.presentation.has_value(),
        });
        if (compositionChanged)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::CompositionState,
                .state = update.presentation ? RichTabDiagnosticState::Nonempty : RichTabDiagnosticState::Empty,
                .reason = update.presentation ? RichTabDiagnosticReason::None : RichTabDiagnosticReason::EmptyMetadata,
                .sessionId = sessionId,
                .sessionIncarnation = update.sessionIncarnation,
                .contextRevision = update.contextRevision,
                .updateSequence = update.updateSequence,
                .presentationPresent = update.presentation.has_value(),
            });
        }
        _Refresh(
            sessionId,
            ActivationEvent::WorkingDirectoryChanged,
            false,
            {},
            true,
            true);
    }

    void ProviderBroker::Activate(const AttachmentId attachment)
    {
        Notify(attachment, ActivationEvent::TabActivated);
    }

    void ProviderBroker::Notify(
        const AttachmentId attachment,
        const ActivationEvent reason)
    {
        _ReloadProvidersIfChanged();
        std::string sessionId;
        {
            std::lock_guard lock{ _mutex };
            if (const auto found = _attachmentSessions.find(attachment); found != _attachmentSessions.end())
            {
                sessionId = found->second;
            }
        }
        if (!sessionId.empty())
        {
            _Refresh(
                sessionId,
                reason,
                false,
                {},
                false,
                reason == ActivationEvent::ManualRefresh);
        }
    }

    void ProviderBroker::_Refresh(
        const std::string& sessionId,
        const ActivationEvent reason,
        const bool initial,
        const std::unordered_set<std::string>& providerIds,
        const bool forcePersistentRestart,
        const bool resetPersistentBackoff)
    {
        struct Pending
        {
            Registration provider;
            Request request;
            uint64_t generation{ 0 };
        };
        std::vector<Pending> pending;
        std::vector<Pending> persistent;
        std::vector<RichTabDiagnosticEventData> requestEvents;
        size_t eligibleCount = 0;
        size_t catalogCount = 0;
        size_t effectiveCount = 0;
        size_t filterMatchedCount = 0;
        size_t activationSupportedCount = 0;
        size_t coalescedCount = 0;
        size_t skippedCount = 0;
        bool sessionUnavailable = false;
        bool sessionMissing = false;
        uint64_t sessionIncarnation = 0;
        uint64_t catalogRevision = 0;
        {
            std::lock_guard lock{ _mutex };
            const auto found = _sessions.find(sessionId);
            catalogCount = _catalog.size();
            effectiveCount = _providers.size();
            catalogRevision = _catalogRevision;
            if (found == _sessions.end())
            {
                sessionUnavailable = true;
                sessionMissing = true;
            }
            else if (found->second.callbacks.empty())
            {
                sessionUnavailable = true;
            }
            else
            {
                auto& session = found->second;
                sessionIncarnation = session.sessionIncarnation;
                for (const auto& provider : _providers)
                {
                    if (!providerIds.empty() && !providerIds.contains(provider.manifest.id))
                    {
                        continue;
                    }
                    ++filterMatchedCount;
                    ++eligibleCount;
                    auto activation = reason;
                    if (!_Handles(provider.manifest, activation))
                    {
                        if (!initial || !_Handles(provider.manifest, ActivationEvent::ManualRefresh))
                        {
                            ++skippedCount;
                            continue;
                        }
                        activation = ActivationEvent::ManualRefresh;
                    }
                    ++activationSupportedCount;

                    auto& providerState = session.providers[provider.manifest.id];
                    const auto previousActiveRequestId = providerState.activeRequestId;
                    const auto previousRunningGeneration = providerState.runningGeneration;
                    const auto generation = _nextGeneration++;
                    providerState.generation = generation;
                    Request request;
                    request.requestId =
                        std::to_string(_processEpoch) + "-" + std::to_string(_nextRequest++);
                    request.protocolVersion = (std::min)(CurrentProtocolVersion, provider.manifest.protocol.maximum);
                    request.providerId = provider.manifest.id;
                    request.processEpoch = _processEpoch;
                    request.sessionId = session.context.sessionId;
                    request.reason = activation;
                    request.workingDirectory = session.context.workingDirectory;
                    request.workingDirectoryAuthoritative = session.context.workingDirectoryAuthoritative;
                    request.contextRevision = session.contextRevision;
                    request.shellType = session.context.shellType;
                    providerState.activeRequestId = request.requestId;
                    providerState.lastActivation = activation;
                    const auto persistentProvider = provider.manifest.hosting.kind == HostingKind::Persistent;
                    if (persistentProvider)
                    {
                        _InvalidatePublishLeasesLocked(
                            session.context.sessionId,
                            provider.manifest.id);
                        providerState.running = false;
                        providerState.runningGeneration = 0;
                        providerState.persistentInstanceGeneration = 0;
                        providerState.pending.reset();
                        requestEvents.emplace_back(RichTabDiagnosticEventData{
                            .level = RichTabDiagnosticLevel::Debug,
                            .event = RichTabDiagnosticEvent::RequestState,
                            .state = RichTabDiagnosticState::Dispatched,
                            .reason = _DiagnosticReason(activation),
                            .sessionId = request.sessionId,
                            .providerId = request.providerId,
                            .requestId = request.requestId,
                            .contextRevision = request.contextRevision,
                            .generation = generation,
                            .persistent = true,
                        });
                        persistent.push_back(Pending{ provider, std::move(request), generation });
                        continue;
                    }
                    if (providerState.running)
                    {
                        ++coalescedCount;
                        if (!providerState.pending && !previousActiveRequestId.empty())
                        {
                            requestEvents.emplace_back(RichTabDiagnosticEventData{
                                .event = RichTabDiagnosticEvent::RequestState,
                                .state = RichTabDiagnosticState::Superseded,
                                .sessionId = request.sessionId,
                                .providerId = request.providerId,
                                .requestId = previousActiveRequestId,
                                .supersededByRequestId = request.requestId,
                                .sessionIncarnation = session.sessionIncarnation,
                                .contextRevision = request.contextRevision,
                                .generation = previousRunningGeneration,
                                .persistent = false,
                            });
                        }
                        if (providerState.pending)
                        {
                            requestEvents.emplace_back(RichTabDiagnosticEventData{
                                .event = RichTabDiagnosticEvent::RequestState,
                                .state = RichTabDiagnosticState::Superseded,
                                .sessionId = providerState.pending->request.sessionId,
                                .providerId = providerState.pending->request.providerId,
                                .requestId = providerState.pending->request.requestId,
                                .supersededByRequestId = request.requestId,
                                .sessionIncarnation = session.sessionIncarnation,
                                .contextRevision = providerState.pending->request.contextRevision,
                                .generation = providerState.pending->generation,
                                .persistent = false,
                            });
                        }
                        requestEvents.emplace_back(RichTabDiagnosticEventData{
                            .level = RichTabDiagnosticLevel::Debug,
                            .event = RichTabDiagnosticEvent::RequestState,
                            .state = RichTabDiagnosticState::Coalesced,
                            .reason = _DiagnosticReason(activation),
                            .sessionId = request.sessionId,
                            .providerId = request.providerId,
                            .requestId = request.requestId,
                            .sessionIncarnation = session.sessionIncarnation,
                            .contextRevision = request.contextRevision,
                            .generation = generation,
                            .persistent = false,
                        });
                        providerState.pending = PendingRequest{ provider, std::move(request), generation };
                        continue;
                    }
                    providerState.running = true;
                    providerState.runningGeneration = generation;
                    requestEvents.emplace_back(RichTabDiagnosticEventData{
                        .level = RichTabDiagnosticLevel::Debug,
                        .event = RichTabDiagnosticEvent::RequestState,
                        .state = RichTabDiagnosticState::Dispatched,
                        .reason = _DiagnosticReason(activation),
                        .sessionId = request.sessionId,
                        .providerId = request.providerId,
                        .requestId = request.requestId,
                        .sessionIncarnation = session.sessionIncarnation,
                        .contextRevision = request.contextRevision,
                        .generation = generation,
                        .persistent = false,
                    });
                    pending.push_back(Pending{ provider, std::move(request), generation });
                }
            }
        }

        if (sessionUnavailable)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Info,
                .event = RichTabDiagnosticEvent::RefreshPlan,
                .state = RichTabDiagnosticState::Skipped,
                .reason = sessionMissing ? RichTabDiagnosticReason::SessionMissing : RichTabDiagnosticReason::NoCallbacks,
                .sessionId = sessionId,
                .catalogRevision = catalogRevision,
                .catalogCount = catalogCount,
                .effectiveCount = effectiveCount,
            });
            return;
        }
        const auto startedCount = pending.size() + persistent.size();
        auto planState = RichTabDiagnosticState::Planned;
        auto planReason = _DiagnosticReason(reason);
        if (startedCount == 0 && coalescedCount == 0)
        {
            planState = RichTabDiagnosticState::Skipped;
            planReason =
                effectiveCount == 0 ? RichTabDiagnosticReason::NoEffectiveProvider :
                filterMatchedCount == 0 ? RichTabDiagnosticReason::ProviderFilterMiss :
                activationSupportedCount == 0 ? RichTabDiagnosticReason::ActivationUnsupported :
                                                planReason;
        }
        EmitRichTabDiagnostic({
            .level = RichTabDiagnosticLevel::Info,
            .event = RichTabDiagnosticEvent::RefreshPlan,
            .state = planState,
            .reason = planReason,
            .sessionId = sessionId,
            .sessionIncarnation = sessionIncarnation,
            .catalogRevision = catalogRevision,
            .catalogCount = catalogCount,
            .effectiveCount = effectiveCount,
            .filterMatchedCount = filterMatchedCount,
            .activationSupportedCount = activationSupportedCount,
            .eligibleCount = eligibleCount,
            .startedCount = startedCount,
            .persistentCount = persistent.size(),
            .coalescedCount = coalescedCount,
            .skippedCount = skippedCount,
        });
        for (auto& event : requestEvents)
        {
            EmitRichTabDiagnostic(std::move(event));
        }
        for (auto& work : persistent)
        {
            _RunPersistentProvider(
                std::move(work.provider),
                std::move(work.request),
                work.generation,
                forcePersistentRestart,
                resetPersistentBackoff);
        }
        for (auto& work : pending)
        {
            _Enqueue(
                [this,
                 provider = std::move(work.provider),
                 request = std::move(work.request),
                 generation = work.generation]() mutable {
                    _RunProvider(std::move(provider), std::move(request), generation);
                });
        }
    }

    void ProviderBroker::_RunPersistentProvider(
        Registration provider,
        Request request,
        const uint64_t generation,
        const bool forceRestart,
        const bool resetBackoff)
    {
        const auto clsid = _EnvironmentValue(L"WT_COM_CLSID");
        const auto cli = _EnvironmentValue(L"WT_RICH_TAB_CLI");
        if (!clsid || !cli)
        {
            _OnPersistentProviderEvent(PersistentProviderEvent{
                PersistentProviderEventKind::LaunchFailed,
                { provider.manifest.id, request.sessionId },
                0,
                generation,
                std::chrono::milliseconds{ 0 },
                ERROR_ENVVAR_NOT_FOUND,
                "Rich Tab publish routing is unavailable",
            });
            return;
        }

        _persistentSupervisor.StartOrRefresh(
            { provider.manifest.id, request.sessionId },
            provider.manifest,
            {
                { L"WT_COM_CLSID", *clsid },
                { L"WT_RICH_TAB_CLI", *cli },
            },
            [this,
             provider,
             request,
             generation](const uint64_t instanceGeneration, const bool started) -> std::optional<std::string> {
                std::optional<std::string> frame;
                {
                    std::lock_guard lock{ _mutex };
                    frame = _CreatePersistentControlFrameLocked(
                        provider,
                        request,
                        generation,
                        instanceGeneration,
                        started);
                }
                EmitRichTabDiagnostic({
                    .level = frame ? RichTabDiagnosticLevel::Debug : RichTabDiagnosticLevel::Warning,
                    .event = RichTabDiagnosticEvent::RequestState,
                    .state = frame ? RichTabDiagnosticState::Dispatched : RichTabDiagnosticState::Failed,
                    .reason = frame ? RichTabDiagnosticReason::None : RichTabDiagnosticReason::ControlFrameFailed,
                    .sessionId = request.sessionId,
                    .providerId = provider.manifest.id,
                    .requestId = request.requestId,
                    .contextRevision = request.contextRevision,
                    .generation = generation,
                    .instanceGeneration = instanceGeneration,
                    .persistent = true,
                });
                return frame;
            },
            generation,
            forceRestart,
            resetBackoff);
    }

    void ProviderBroker::_RunProvider(
        Registration provider,
        Request request,
        const uint64_t generation)
    {
        std::vector<std::string> diagnostics;
        const auto serialized = SerializeRequest(request, provider.manifest);
        std::optional<Snapshot> snapshot;
        std::optional<std::string> publishLease;
        std::optional<RichTabDiagnosticReason> failureReason;
        CommandResult command;
        const auto publishProtocol = request.protocolVersion >= 2;
        if (!serialized)
        {
            diagnostics = serialized.errors;
            failureReason = RichTabDiagnosticReason::SerializationFailed;
        }
        else
        {
            CommandRunner::Environment environment;
            if (publishProtocol)
            {
                const auto clsid = _EnvironmentValue(L"WT_COM_CLSID");
                const auto cli = _EnvironmentValue(L"WT_RICH_TAB_CLI");
                if (!clsid || !cli)
                {
                    diagnostics.emplace_back("Rich Tab publish routing is unavailable");
                    failureReason = RichTabDiagnosticReason::PublishRoutingUnavailable;
                }
                else
                {
                    std::lock_guard lock{ _mutex };
                    publishLease = _CreatePublishLeaseLocked(
                        provider,
                        request,
                        generation,
                        PublishLeaseLifetime);
                    if (publishLease->empty())
                    {
                        publishLease.reset();
                        diagnostics.emplace_back("Failed to create a secure Rich Tab publish lease");
                        failureReason = RichTabDiagnosticReason::LeaseCreationFailed;
                    }
                    else
                    {
                        environment = {
                            { L"WT_COM_CLSID", *clsid },
                            { L"WT_RICH_TAB_CLI", *cli },
                            { L"WT_RICH_TAB_LEASE", std::wstring{ publishLease->begin(), publishLease->end() } },
                        };
                    }
                }
            }

            command = diagnostics.empty() ?
                          _runner.Run(provider.manifest, *serialized.value, ProviderTimeout, environment) :
                          CommandResult{};
            if (command.status != CommandResult::Status::Completed || command.exitCode != 0)
            {
                if (!failureReason)
                {
                    switch (command.status)
                    {
                    case CommandResult::Status::InvalidRequest:
                        failureReason = RichTabDiagnosticReason::InvalidRequest;
                        break;
                    case CommandResult::Status::ResolveFailed:
                        failureReason = RichTabDiagnosticReason::ResolveFailed;
                        break;
                    case CommandResult::Status::LaunchFailed:
                        failureReason = RichTabDiagnosticReason::LaunchFailed;
                        break;
                    case CommandResult::Status::InputWriteFailed:
                        failureReason = RichTabDiagnosticReason::InputWriteFailed;
                        break;
                    case CommandResult::Status::WaitFailed:
                        failureReason = RichTabDiagnosticReason::WaitFailed;
                        break;
                    case CommandResult::Status::TimedOut:
                        failureReason = RichTabDiagnosticReason::TimedOut;
                        break;
                    case CommandResult::Status::OutputLimitExceeded:
                        failureReason = RichTabDiagnosticReason::OutputLimitExceeded;
                        break;
                    case CommandResult::Status::ExitCodeUnavailable:
                        failureReason = RichTabDiagnosticReason::ExitCodeUnavailable;
                        break;
                    case CommandResult::Status::Completed:
                        failureReason = RichTabDiagnosticReason::ExitNonzero;
                        break;
                    }
                }
                diagnostics.emplace_back(
                    "Provider '" + provider.manifest.id + "' failed with status " +
                    std::to_string(static_cast<int>(command.status)) +
                    " and exit code " + std::to_string(command.exitCode));
                if (!command.standardError.empty())
                {
                    diagnostics.emplace_back(command.standardError);
                }
            }
            else if (!publishProtocol)
            {
                const auto parsed = ParseSnapshot(
                    command.standardOutput,
                    provider.manifest,
                    request.requestId);
                if (parsed)
                {
                    snapshot = *parsed.value;
                }
                else
                {
                    diagnostics = parsed.errors;
                    failureReason = RichTabDiagnosticReason::SnapshotInvalid;
                }
            }
        }

        std::vector<Callback> callbacks;
        BrokerUpdate update;
        std::optional<PendingRequest> next;
        std::optional<uint64_t> committedUpdateSequence;
        std::optional<uint64_t> completedSessionIncarnation;
        std::optional<bool> committedPresentationPresent;
        bool compositionChanged = false;
        const auto snapshotProduced = snapshot.has_value();
        {
            std::lock_guard lock{ _mutex };
            const auto session = _sessions.find(request.sessionId);
            if (session == _sessions.end())
            {
                return;
            }
            const auto state = session->second.providers.find(provider.manifest.id);
            if (state == session->second.providers.end() ||
                !state->second.running ||
                state->second.runningGeneration != generation)
            {
                return;
            }
            state->second.running = false;
            if (publishLease)
            {
                _publishLeases.erase(*publishLease);
            }

            if (state->second.generation == generation &&
                session->second.contextRevision == request.contextRevision)
            {
                if (publishProtocol && state->second.publishedGeneration == generation)
                {
                    committedUpdateSequence = state->second.publishedUpdateSequence;
                    completedSessionIncarnation = session->second.sessionIncarnation;
                    committedPresentationPresent = session->second.diagnosticPresentationPresent;
                }
                else
                {
                    if (publishProtocol && diagnostics.empty())
                    {
                        diagnostics.emplace_back("Provider exited without publishing a Snapshot");
                        failureReason = RichTabDiagnosticReason::ExitedWithoutPublish;
                    }
                    if (snapshot)
                    {
                        UpdateFieldChangeSequences(
                            *snapshot,
                            state->second.fieldBaseline,
                            state->second.fieldChangeSequences,
                            session->second.nextFieldChangeSequence);
                        state->second.snapshot = std::move(snapshot);
                    }
                    ++session->second.updateSequence;
                    update = _UpdateFor(request.sessionId, session->second, std::move(diagnostics));
                    const auto presentationPresent = update.presentation.has_value();
                    compositionChanged =
                        !session->second.diagnosticPresentationPresent ||
                        *session->second.diagnosticPresentationPresent != presentationPresent;
                    session->second.diagnosticPresentationPresent = presentationPresent;
                    callbacks.reserve(session->second.callbacks.size());
                    for (const auto& [_, callback] : session->second.callbacks)
                    {
                        callbacks.emplace_back(callback);
                    }
                }
            }

            if (state->second.pending)
            {
                next = std::move(state->second.pending);
                state->second.pending.reset();
                state->second.running = true;
                state->second.runningGeneration = next->generation;
            }
        }

        _DeliverCallbacks(callbacks, update);
        if (compositionChanged)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::CompositionState,
                .state = update.presentation ? RichTabDiagnosticState::Nonempty : RichTabDiagnosticState::Empty,
                .reason = update.presentation ? RichTabDiagnosticReason::None : RichTabDiagnosticReason::EmptyMetadata,
                .sessionId = request.sessionId,
                .sessionIncarnation = update.sessionIncarnation,
                .contextRevision = request.contextRevision,
                .updateSequence = update.updateSequence,
                .snapshotCount = snapshotProduced ? size_t{ 1 } : size_t{ 0 },
                .presentationPresent = update.presentation.has_value(),
            });
        }
        if (failureReason)
        {
            EmitRichTabDiagnostic({
                .level = *failureReason == RichTabDiagnosticReason::TimedOut ?
                             RichTabDiagnosticLevel::Error :
                             RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::RequestState,
                .state = RichTabDiagnosticState::Failed,
                .reason = *failureReason,
                .sessionId = request.sessionId,
                .providerId = provider.manifest.id,
                .requestId = request.requestId,
                .sessionIncarnation = callbacks.empty() ? completedSessionIncarnation : std::optional<uint64_t>{ update.sessionIncarnation },
                .contextRevision = request.contextRevision,
                .updateSequence = callbacks.empty() ?
                                      committedUpdateSequence :
                                      std::optional<uint64_t>{ update.updateSequence },
                .generation = generation,
                .win32Error = command.win32Error,
                .exitCode = command.exitCode,
                .persistent = false,
                .presentationPresent = callbacks.empty() ?
                                           committedPresentationPresent :
                                           std::optional<bool>{ update.presentation.has_value() },
            });
        }
        else if (!callbacks.empty() || committedUpdateSequence)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Info,
                .event = RichTabDiagnosticEvent::RequestState,
                .state = RichTabDiagnosticState::Completed,
                .sessionId = request.sessionId,
                .providerId = provider.manifest.id,
                .requestId = request.requestId,
                .sessionIncarnation = callbacks.empty() ? completedSessionIncarnation : std::optional<uint64_t>{ update.sessionIncarnation },
                .contextRevision = request.contextRevision,
                .updateSequence = callbacks.empty() ? committedUpdateSequence : std::optional<uint64_t>{ update.updateSequence },
                .generation = generation,
                .persistent = false,
                .presentationPresent = callbacks.empty() ? committedPresentationPresent : std::optional<bool>{ update.presentation.has_value() },
            });
        }
        if (next)
        {
            _Enqueue(
                [this,
                 provider = std::move(next->provider),
                 request = std::move(next->request),
                 generation = next->generation]() mutable {
                    _RunProvider(std::move(provider), std::move(request), generation);
                });
        }
    }

    void ProviderBroker::_OnPersistentProviderEvent(
        const PersistentProviderEvent& event)
    {
        if (event.kind == PersistentProviderEventKind::Stopped)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::InstanceState,
                .state = RichTabDiagnosticState::Stopped,
                .sessionId = event.key.sessionId,
                .providerId = event.key.providerId,
                .generation = event.requestGeneration,
                .instanceGeneration = event.instanceGeneration,
                .persistent = true,
            });
            return;
        }
        if (event.kind == PersistentProviderEventKind::LeaseDropped ||
            event.kind == PersistentProviderEventKind::StopFrameFailed)
        {
            auto reason = RichTabDiagnosticReason::ControlFrameFailed;
            if (event.kind == PersistentProviderEventKind::LeaseDropped)
            {
                reason =
                    event.win32Error == ERROR_REVISION_MISMATCH ? RichTabDiagnosticReason::GenerationStale :
                    event.win32Error == ERROR_NOT_FOUND ? RichTabDiagnosticReason::InstanceMissing :
                                                         RichTabDiagnosticReason::ProcessExited;
            }
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = event.kind == PersistentProviderEventKind::LeaseDropped ?
                             RichTabDiagnosticEvent::RequestState :
                             RichTabDiagnosticEvent::InstanceState,
                .state = RichTabDiagnosticState::Failed,
                .reason = reason,
                .sessionId = event.key.sessionId,
                .providerId = event.key.providerId,
                .generation = event.requestGeneration,
                .instanceGeneration = event.instanceGeneration,
                .win32Error = event.win32Error,
                .persistent = true,
            });
            return;
        }
        if (event.kind == PersistentProviderEventKind::Started)
        {
            EmitRichTabDiagnostic({
                .event = RichTabDiagnosticEvent::InstanceState,
                .state = RichTabDiagnosticState::Running,
                .sessionId = event.key.sessionId,
                .providerId = event.key.providerId,
                .generation = event.requestGeneration,
                .instanceGeneration = event.instanceGeneration,
                .persistent = true,
            });
            return;
        }
        if (event.kind == PersistentProviderEventKind::RestartRequested)
        {
            ActivationEvent reason{ ActivationEvent::ManualRefresh };
            auto restart = false;
            {
                std::lock_guard lock{ _mutex };
                const auto session = _sessions.find(event.key.sessionId);
                if (session == _sessions.end() || session->second.callbacks.empty())
                {
                    return;
                }
                const auto state = session->second.providers.find(event.key.providerId);
                const auto provider = _FindProvider(_providers, event.key.providerId);
                if (state == session->second.providers.end() ||
                    state->second.generation != event.requestGeneration ||
                    !provider ||
                    provider->manifest.hosting.kind != HostingKind::Persistent)
                {
                    return;
                }
                reason = state->second.lastActivation;
                restart = true;
            }
            if (restart)
            {
                EmitRichTabDiagnostic({
                    .event = RichTabDiagnosticEvent::InstanceState,
                    .state = RichTabDiagnosticState::Restarted,
                    .reason = RichTabDiagnosticReason::ProcessExited,
                    .sessionId = event.key.sessionId,
                    .providerId = event.key.providerId,
                    .generation = event.requestGeneration,
                    .instanceGeneration = event.instanceGeneration,
                    .persistent = true,
                });
                _Refresh(
                    event.key.sessionId,
                    reason,
                    false,
                    { event.key.providerId },
                    false,
                    false);
            }
            return;
        }

        std::vector<Callback> callbacks;
        BrokerUpdate update;
        {
            std::lock_guard lock{ _mutex };
            const auto session = _sessions.find(event.key.sessionId);
            if (session == _sessions.end())
            {
                return;
            }
            const auto state = session->second.providers.find(event.key.providerId);
            if (state == session->second.providers.end() ||
                state->second.generation != event.requestGeneration ||
                (event.instanceGeneration != 0 &&
                 state->second.persistentInstanceGeneration != 0 &&
                 state->second.persistentInstanceGeneration != event.instanceGeneration))
            {
                return;
            }

            state->second.running = false;
            state->second.runningGeneration = 0;
            state->second.persistentInstanceGeneration = 0;
            _InvalidatePublishLeasesLocked(
                event.key.sessionId,
                event.key.providerId);

            std::string diagnostic =
                "Persistent provider '" + event.key.providerId + "' failed";
            if (event.win32Error != ERROR_SUCCESS)
            {
                diagnostic += " with Win32 error " + std::to_string(event.win32Error);
            }
            std::vector<std::string> diagnostics{ std::move(diagnostic) };
            if (!event.standardError.empty())
            {
                diagnostics.emplace_back(event.standardError);
            }
            ++session->second.updateSequence;
            update = _UpdateFor(
                event.key.sessionId,
                session->second,
                std::move(diagnostics));
            callbacks.reserve(session->second.callbacks.size());
            for (const auto& [_, callback] : session->second.callbacks)
            {
                callbacks.emplace_back(callback);
            }
        }

        _DeliverCallbacks(callbacks, update);
        auto reason = RichTabDiagnosticReason::ProcessExited;
        if (event.kind == PersistentProviderEventKind::LaunchFailed)
        {
            reason = event.win32Error == ERROR_ENVVAR_NOT_FOUND ?
                         RichTabDiagnosticReason::PublishRoutingUnavailable :
                         RichTabDiagnosticReason::LaunchFailed;
        }
        else if (event.kind == PersistentProviderEventKind::ControlWriteFailed)
        {
            reason = RichTabDiagnosticReason::ControlWriteFailed;
        }
        if (event.kind == PersistentProviderEventKind::ProcessExited)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::InstanceState,
                .state = RichTabDiagnosticState::Exited,
                .reason = RichTabDiagnosticReason::ProcessExited,
                .sessionId = event.key.sessionId,
                .providerId = event.key.providerId,
                .sessionIncarnation = update.sessionIncarnation,
                .updateSequence = update.updateSequence,
                .generation = event.requestGeneration,
                .instanceGeneration = event.instanceGeneration,
                .win32Error = event.win32Error,
                .persistent = true,
            });
        }
        EmitRichTabDiagnostic({
            .level = RichTabDiagnosticLevel::Error,
            .event = RichTabDiagnosticEvent::InstanceState,
            .state = RichTabDiagnosticState::Backoff,
            .reason = reason,
            .sessionId = event.key.sessionId,
            .providerId = event.key.providerId,
            .sessionIncarnation = update.sessionIncarnation,
            .updateSequence = update.updateSequence,
            .generation = event.requestGeneration,
            .instanceGeneration = event.instanceGeneration,
            .retryAfterMilliseconds = static_cast<uint64_t>(event.retryAfter.count()),
            .win32Error = event.win32Error,
            .persistent = true,
            .presentationPresent = update.presentation.has_value(),
        });
    }

    std::vector<std::string> ProviderBroker::_PruneDetachedSessionsLocked()
    {
        std::vector<std::string> removed;
        const auto oldestRetained = std::chrono::steady_clock::now() - DetachedSessionRetention;
        for (auto current = _sessions.begin(); current != _sessions.end();)
        {
            const auto& session = current->second;
            if (session.callbacks.empty() &&
                session.detachedAt &&
                *session.detachedAt <= oldestRetained)
            {
                _InvalidatePublishLeasesLocked(current->first);
                removed.emplace_back(current->first);
                current = _sessions.erase(current);
            }
            else
            {
                ++current;
            }
        }
        return removed;
    }

    void ProviderBroker::_HousekeepingWorker()
    {
        for (;;)
        {
            {
                std::unique_lock lock{ _housekeepingMutex };
                _housekeepingCondition.wait_for(
                    lock,
                    std::chrono::seconds{ 1 },
                    [&]() { return _housekeepingStopping; });
                if (_housekeepingStopping)
                {
                    return;
                }
            }

            std::vector<std::string> removed;
            std::vector<PublishLease> grantsToRenew;
            {
                std::lock_guard lock{ _mutex };
                removed = _PruneDetachedSessionsLocked();
                for (const auto& sessionId : removed)
                {
                    _persistentSupervisor.StopSession(sessionId);
                }

                const auto now = std::chrono::steady_clock::now();
                for (auto current = _publishLeases.begin(); current != _publishLeases.end();)
                {
                    const auto& binding = current->second;
                    if (!binding.persistent || binding.expiresAt > now)
                    {
                        ++current;
                        continue;
                    }

                    SessionState* session = nullptr;
                    ProviderState* providerState = nullptr;
                    if (_ValidatePublishLeaseLocked(binding, session, providerState) &&
                        std::none_of(
                            grantsToRenew.begin(),
                            grantsToRenew.end(),
                            [&](const auto& pending) {
                                return pending.request.sessionId == binding.request.sessionId &&
                                       pending.provider.manifest.id == binding.provider.manifest.id &&
                                       pending.generation == binding.generation &&
                                       pending.instanceGeneration == binding.instanceGeneration;
                            }))
                    {
                        grantsToRenew.emplace_back(binding);
                    }
                    current = _publishLeases.erase(current);
                }
            }
            for (const auto& sessionId : removed)
            {
                EmitRichTabDiagnostic({
                    .event = RichTabDiagnosticEvent::AttachmentState,
                    .state = RichTabDiagnosticState::GraceExpired,
                    .reason = RichTabDiagnosticReason::DetachedRetentionExpired,
                    .sessionId = sessionId,
                });
            }
            for (const auto& binding : grantsToRenew)
            {
                _ReplenishPersistentGrants(binding);
            }
        }
    }

    BrokerUpdate ProviderBroker::_UpdateFor(
        const std::string& sessionId,
        const SessionState& state,
        std::vector<std::string> diagnostics) const
    {
        std::unordered_map<std::string, Snapshot> snapshots;
        FieldChangeSequences fieldChangeSequences;
        for (const auto& [id, provider] : state.providers)
        {
            if (provider.snapshot)
            {
                snapshots.emplace(id, *provider.snapshot);
            }
            if (!provider.fieldChangeSequences.empty())
            {
                fieldChangeSequences.emplace(id, provider.fieldChangeSequences);
            }
        }
        return BrokerUpdate{
            sessionId,
            state.sessionIncarnation,
            state.contextRevision,
            state.updateSequence,
            ComposePresentation(
                _providers,
                snapshots,
                _preferences,
                fieldChangeSequences,
                _prioritizeRecentlyUpdatedFields),
            std::move(diagnostics)
        };
    }

    void ProviderBroker::_DeliverCallback(
        const Callback& callback,
        const BrokerUpdate& update) noexcept
    {
        try
        {
            callback(update);
        }
        catch (...)
        {
            EmitRichTabDiagnostic({
                .level = RichTabDiagnosticLevel::Warning,
                .event = RichTabDiagnosticEvent::PageHandoff,
                .state = RichTabDiagnosticState::Failed,
                .reason = RichTabDiagnosticReason::CallbackFailed,
                .sessionId = update.sessionId,
                .sessionIncarnation = update.sessionIncarnation,
                .contextRevision = update.contextRevision,
                .updateSequence = update.updateSequence,
                .presentationPresent = update.presentation.has_value(),
            });
        }
    }

    void ProviderBroker::_DeliverCallbacks(
        const std::vector<Callback>& callbacks,
        const BrokerUpdate& update) noexcept
    {
        for (const auto& callback : callbacks)
        {
            _DeliverCallback(callback, update);
        }
    }

    std::optional<Presentation> ProviderBroker::ComposePresentation(
        const std::vector<Registration>& providers,
        const std::unordered_map<std::string, Snapshot>& snapshots,
        const std::vector<ProviderPreference>& preferences,
        const FieldChangeSequences& fieldChangeSequences,
        const bool prioritizeRecentlyUpdatedFields)
    {
        struct FieldCandidate
        {
            const std::string* providerId;
            const Snapshot* snapshot;
            const FieldValue* value;
            uint64_t changeSequence{ 0 };
        };

        std::vector<FieldCandidate> candidates;
        Presentation result;
        for (const auto& provider : providers)
        {
            const auto snapshot = snapshots.find(provider.manifest.id);
            if (snapshot == snapshots.end())
            {
                continue;
            }
            const auto preference = _FindPreference(preferences, provider.manifest.id);
            std::vector<const FieldDeclaration*> visibleFields;
            if (preference && preference->fields)
            {
                visibleFields.reserve(preference->fields->size());
                for (const auto& fieldId : *preference->fields)
                {
                    const auto field = std::find_if(
                        provider.manifest.fields.begin(),
                        provider.manifest.fields.end(),
                        [&](const auto& declaration) {
                            return declaration.id == fieldId;
                        });
                    if (field != provider.manifest.fields.end())
                    {
                        visibleFields.emplace_back(&*field);
                    }
                }
            }
            else
            {
                for (const auto& field : provider.manifest.fields)
                {
                    if (field.defaultVisible)
                    {
                        visibleFields.emplace_back(&field);
                    }
                }
            }

            for (const auto field : visibleFields)
            {
                if (const auto value = snapshot->second.fields.find(field->id);
                    value != snapshot->second.fields.end())
                {
                    uint64_t changeSequence = 0;
                    if (const auto providerChanges = fieldChangeSequences.find(provider.manifest.id);
                        providerChanges != fieldChangeSequences.end())
                    {
                        if (const auto fieldChange = providerChanges->second.find(field->id);
                            fieldChange != providerChanges->second.end())
                        {
                            changeSequence = fieldChange->second;
                        }
                    }
                    candidates.emplace_back(FieldCandidate{
                        &provider.manifest.id,
                        &snapshot->second,
                        &value->second,
                        changeSequence,
                    });
                }
            }
        }

        if (prioritizeRecentlyUpdatedFields)
        {
            std::stable_sort(candidates.begin(), candidates.end(), [](const auto& left, const auto& right) {
                return left.changeSequence > right.changeSequence;
            });
        }

        std::unordered_set<std::string> metadataAdded;
        for (const auto& candidate : candidates)
        {
            _Append(result.text, _ValueText(*candidate.value), L" \u00b7 ");
            if (!metadataAdded.emplace(*candidate.providerId).second)
            {
                continue;
            }
            if (candidate.snapshot->tooltip)
            {
                _Append(result.tooltip, _ToWide(*candidate.snapshot->tooltip), L"\n");
            }
            if (candidate.snapshot->accessibilityText)
            {
                _Append(result.accessibilityText, _ToWide(*candidate.snapshot->accessibilityText), L", ");
            }
        }
        if (result.text.empty())
        {
            return std::nullopt;
        }
        if (result.tooltip.empty())
        {
            result.tooltip = result.text;
        }
        if (result.accessibilityText.empty())
        {
            result.accessibilityText = result.text;
        }
        return result;
    }

    bool ProviderBroker::UpdateFieldChangeSequences(
        const Snapshot& snapshot,
        std::optional<std::unordered_map<std::string, FieldValue>>& baseline,
        std::unordered_map<std::string, uint64_t>& changeSequences,
        uint64_t& nextChangeSequence)
    {
        if (!baseline)
        {
            baseline = snapshot.fields;
            return false;
        }

        std::unordered_set<std::string> changedFields;
        for (const auto& [id, value] : *baseline)
        {
            const auto current = snapshot.fields.find(id);
            if (current == snapshot.fields.end() || current->second != value)
            {
                changedFields.emplace(id);
            }
        }
        for (const auto& [id, value] : snapshot.fields)
        {
            const auto previous = baseline->find(id);
            if (previous == baseline->end() || previous->second != value)
            {
                changedFields.emplace(id);
            }
        }

        *baseline = snapshot.fields;
        if (changedFields.empty())
        {
            return false;
        }

        const auto sequence = ++nextChangeSequence;
        for (const auto& id : changedFields)
        {
            changeSequences[id] = sequence;
        }
        return true;
    }
}
