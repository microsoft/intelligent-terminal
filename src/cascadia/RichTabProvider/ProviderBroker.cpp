// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderBroker.h"
#include "BuiltInProviderCatalog.h"

#include <windows.h>
#include <bcrypt.h>

#include <algorithm>
#include <charconv>
#include <thread>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        constexpr auto ProviderTimeout = std::chrono::seconds{ 5 };
        constexpr auto DetachedSessionRetention = std::chrono::seconds{ 30 };

        bool _IsBuiltInGitStatus(const Registration& provider)
        {
            return provider.manifest.id == "com.microsoft.intelligent-terminal.git-status" &&
                   provider.origin == RegistrationOrigin::BuiltInCatalog &&
                   provider.kind == RegistrationKind::Managed &&
                   provider.enabled && provider.integrityValid &&
                   provider.root == BuiltInProviderCatalog::PackageRoot() / L"RichTabProviders" / L"GitStatus";
        }

        bool _IsDiagnosticEligible(const Registration& provider) noexcept
        {
            return provider.origin == RegistrationOrigin::BuiltInCatalog &&
                   provider.manifest.id == "com.microsoft.intelligent-terminal.git-status";
        }

        uint8_t _FieldMask(const std::vector<std::string>& fields) noexcept
        {
            uint8_t result = ProviderFieldNone;
            for (const auto& field : fields)
            {
                if (field == "agentStatus")
                {
                    result |= ProviderFieldAgentStatus;
                }
                else if (field == "workingDirectory")
                {
                    result |= ProviderFieldWorkingDirectory;
                }
                else if (field == "repository")
                {
                    result |= ProviderFieldRepository;
                }
                else if (field == "branch")
                {
                    result |= ProviderFieldBranch;
                }
                else if (field == "changes")
                {
                    result |= ProviderFieldChanges;
                }
            }
            return result;
        }

        uint8_t _FieldMask(const Snapshot& snapshot) noexcept
        {
            uint8_t result = ProviderFieldNone;
            for (const auto& [field, _] : snapshot.fields)
            {
                if (field == "agentStatus")
                {
                    result |= ProviderFieldAgentStatus;
                }
                else if (field == "workingDirectory")
                {
                    result |= ProviderFieldWorkingDirectory;
                }
                else if (field == "repository")
                {
                    result |= ProviderFieldRepository;
                }
                else if (field == "branch")
                {
                    result |= ProviderFieldBranch;
                }
                else if (field == "changes")
                {
                    result |= ProviderFieldChanges;
                }
            }
            return result;
        }

        void _ClearGitStatusCwdDependentFields(Snapshot& snapshot)
        {
            for (const auto* field : { "workingDirectory", "repository", "branch", "changes" })
            {
                snapshot.fields.erase(field);
            }
            snapshot.tooltip.reset();
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
    }

    ProviderBroker& ProviderBroker::Instance()
    {
        static ProviderBroker instance;
        return instance;
    }

    ProviderBroker::ProviderBroker() :
        ProviderBroker{ CommandRunner::ResolveGit(), true }
    {
    }

    ProviderBroker::ProviderBroker(std::optional<std::filesystem::path> gitBinary) :
        ProviderBroker{ std::move(gitBinary), false }
    {
    }

    ProviderBroker::ProviderBroker(
        std::optional<std::filesystem::path> gitBinary,
        const bool enableDiagnosticLogging) :
        _gitBinary{ std::move(gitBinary) }
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
        if (enableDiagnosticLogging)
        {
            _diagnosticLog = ProviderDiagnosticLog::Create(_processEpoch);
        }
        constexpr size_t WorkerCount{ 4 };
        _executorWorkers.reserve(WorkerCount);
        for (size_t index = 0; index < WorkerCount; ++index)
        {
            _executorWorkers.emplace_back([this]() {
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
        ReloadProviders();
    }

    ProviderBroker::~ProviderBroker()
    {
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

    bool ProviderBroker::GitAvailable() const noexcept
    {
        return _gitBinary.has_value();
    }

    void ProviderBroker::ReloadProviders()
    {
        auto builtIns = BuiltInProviderCatalog::Load(BuiltInProviderCatalog::PackageRoot());
        if (_diagnosticLog && !_catalogOutcomeLogged.exchange(true, std::memory_order_acq_rel))
        {
            ProviderDiagnosticRecord record;
            record.event = ProviderDiagnosticEvent::Catalog;
            record.timestamp = ProviderDiagnosticTimestamp();
            record.processId = GetCurrentProcessId();
            record.processEpoch = _processEpoch;
            record.catalogOutcome = builtIns.outcome;
            if (builtIns.value && !builtIns.value->empty())
            {
                const auto& provider = builtIns.value->front();
                record.catalogOrigin = provider.origin == RegistrationOrigin::BuiltInCatalog;
                record.providerVersion = ParseProviderDiagnosticVersion(provider.manifest.version);
            }
            _diagnosticLog->TryLog(record);
        }
        std::vector<Registration> providers;
        if (builtIns.value)
        {
            providers = std::move(*builtIns.value);
        }
        std::sort(providers.begin(), providers.end(), [](const auto& first, const auto& second) {
            return first.manifest.id < second.manifest.id;
        });

        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        {
            std::lock_guard lock{ _mutex };
            _providers = std::move(providers);
            for (auto& [sessionId, session] : _sessions)
            {
                std::erase_if(session.providers, [&](const auto& item) {
                    return std::none_of(_providers.begin(), _providers.end(), [&](const auto& provider) {
                        return provider.manifest.id == item.first;
                    });
                });
                for (auto& [_, provider] : session.providers)
                {
                    provider.generation = _nextGeneration++;
                    provider.pending.reset();
                    provider.snapshot.reset();
                }

                ++session.updateSequence;
                const auto update = _UpdateFor(sessionId, session);
                for (const auto& [_, callback] : session.callbacks)
                {
                    notifications.emplace_back(callback, update);
                }
            }
        }

        for (const auto& [callback, update] : notifications)
        {
            callback(update);
        }
    }

    void ProviderBroker::SetVisibleFields(std::string_view providerId, std::vector<std::string> fields)
    {
        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        std::vector<std::string> sessionIdsToRefresh;
        {
            std::lock_guard lock{ _mutex };
            const auto provider = std::find_if(_providers.begin(), _providers.end(), [&](const auto& candidate) {
                return candidate.manifest.id == providerId;
            });
            if (provider == _providers.end())
            {
                return;
            }

            std::unordered_set<std::string> declaredFields;
            declaredFields.reserve(provider->manifest.fields.size());
            for (const auto& field : provider->manifest.fields)
            {
                declaredFields.emplace(field.id);
            }

            std::unordered_set<std::string> selectedFields;
            selectedFields.reserve(fields.size());
            for (auto& field : fields)
            {
                if (declaredFields.contains(field))
                {
                    selectedFields.emplace(std::move(field));
                }
            }

            const auto current = _visibleFields.find(provider->manifest.id);
            if (current != _visibleFields.end() && current->second == selectedFields)
            {
                return;
            }

            const auto emptySelection = selectedFields.empty();
            _visibleFields.insert_or_assign(provider->manifest.id, std::move(selectedFields));

            for (auto& [sessionId, session] : _sessions)
            {
                if (!session.callbacks.empty())
                {
                    sessionIdsToRefresh.push_back(sessionId);
                }
                if (emptySelection)
                {
                    if (const auto state = session.providers.find(provider->manifest.id); state != session.providers.end())
                    {
                        state->second.generation = _nextGeneration++;
                        state->second.pending.reset();
                        state->second.snapshot.reset();
                    }
                }
                ++session.updateSequence;
                const auto update = _UpdateFor(sessionId, session);
                for (const auto& [_, callback] : session.callbacks)
                {
                    notifications.emplace_back(callback, update);
                }
            }
        }

        for (const auto& [callback, update] : notifications)
        {
            callback(update);
        }

        for (const auto& sid : sessionIdsToRefresh)
        {
            _Refresh(sid, ActivationEvent::ManualRefresh, false);
        }
    }

    void ProviderBroker::SetFieldDisplayNames(
        std::string_view providerId,
        std::unordered_map<std::string, std::string> displayNames)
    {
        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        {
            std::lock_guard lock{ _mutex };
            const auto provider = std::find_if(_providers.begin(), _providers.end(), [&](const auto& candidate) {
                return candidate.manifest.id == providerId;
            });
            if (provider == _providers.end())
            {
                return;
            }

            std::unordered_set<std::string> declaredFields;
            declaredFields.reserve(provider->manifest.fields.size());
            for (const auto& field : provider->manifest.fields)
            {
                declaredFields.emplace(field.id);
            }

            std::unordered_map<std::string, std::string> localizedDisplayNames;
            localizedDisplayNames.reserve(displayNames.size());
            for (auto& [field, displayName] : displayNames)
            {
                if (declaredFields.contains(field) && !displayName.empty())
                {
                    localizedDisplayNames.emplace(std::move(field), std::move(displayName));
                }
            }

            const auto current = _fieldDisplayNames.find(provider->manifest.id);
            if (current != _fieldDisplayNames.end() && current->second == localizedDisplayNames)
            {
                return;
            }
            _fieldDisplayNames.insert_or_assign(provider->manifest.id, std::move(localizedDisplayNames));

            for (auto& [sessionId, session] : _sessions)
            {
                ++session.updateSequence;
                const auto update = _UpdateFor(sessionId, session);
                for (const auto& [_, callback] : session.callbacks)
                {
                    notifications.emplace_back(callback, update);
                }
            }
        }

        for (const auto& [callback, update] : notifications)
        {
            callback(update);
        }
    }

    std::optional<std::vector<std::string>> ProviderBroker::VisibleFields(std::string_view providerId) const
    {
        std::lock_guard lock{ _mutex };
        const auto provider = std::find_if(_providers.begin(), _providers.end(), [&](const auto& candidate) {
            return candidate.manifest.id == providerId;
        });
        if (provider == _providers.end())
        {
            return std::nullopt;
        }

        const auto selected = _visibleFields.find(provider->manifest.id);
        std::vector<std::string> result;
        result.reserve(provider->manifest.fields.size());
        for (const auto& field : provider->manifest.fields)
        {
            if (selected != _visibleFields.end() ? selected->second.contains(field.id) : field.defaultVisible)
            {
                result.emplace_back(field.id);
            }
        }
        return result;
    }

    ProviderBroker::AttachmentId ProviderBroker::Attach(SessionContext context, Callback callback)
    {
        if (context.sessionId.empty())
        {
            return 0;
        }

        AttachmentId attachment = 0;
        std::optional<BrokerUpdate> existing;
        bool contextChanged = false;
        auto initialCallback = callback;
        std::vector<Callback> previousCallbacks;
        {
            std::lock_guard lock{ _mutex };
            _PruneDetachedSessionsLocked();
            attachment = _nextAttachment++;
            auto& session = _sessions[context.sessionId];
            session.detachedAt.reset();
            if (session.context.sessionId.empty())
            {
                session.context = context;
                session.sessionIncarnation = _nextSessionIncarnation++;
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
                for (auto& [_, provider] : session.providers)
                {
                    provider.generation = _nextGeneration++;
                    provider.pending.reset();
                }
                _InvalidateGitStatusWorkingDirectoryLocked(session);
                ++session.updateSequence;
            }
            session.callbacks.emplace(attachment, std::move(callback));
            _attachmentSessions.emplace(attachment, session.context.sessionId);
            if (_RecomputeAgentWorkingDirectoryLocked(session))
            {
                for (const auto& [id, listener] : session.callbacks)
                {
                    if (id != attachment)
                    {
                        previousCallbacks.emplace_back(listener);
                    }
                }
            }
            existing = _UpdateFor(session.context.sessionId, session);
        }

        for (const auto& listener : previousCallbacks)
        {
            listener(*existing);
        }
        if (contextChanged || existing->presentation)
        {
            initialCallback(*existing);
        }
        _Refresh(existing->sessionId, ActivationEvent::PaneConnected, true);
        return attachment;
    }

    void ProviderBroker::Detach(const AttachmentId attachment)
    {
        std::string sessionId;
        std::vector<Callback> callbacks;
        BrokerUpdate update;
        {
            std::lock_guard lock{ _mutex };
            const auto attached = _attachmentSessions.find(attachment);
            if (attached == _attachmentSessions.end())
            {
                return;
            }
            if (const auto found = _sessions.find(attached->second); found != _sessions.end())
            {
                auto& session = found->second;
                session.callbacks.erase(attachment);
                session.activeAgentWorkingDirectories.erase(attachment);
                if (session.callbacks.empty())
                {
                    session.activeAgentWorkingDirectories.clear();
                    session.agentActive = false;
                    session.agentWorkingDirectory.reset();
                    for (auto& [_, provider] : session.providers)
                    {
                        provider.generation = _nextGeneration++;
                        provider.pending.reset();
                    }
                    _InvalidateGitStatusWorkingDirectoryLocked(session);
                    ++session.updateSequence;
                    session.detachedAt = std::chrono::steady_clock::now();
                }
                else if (_RecomputeAgentWorkingDirectoryLocked(session))
                {
                    sessionId = attached->second;
                    update = _UpdateFor(sessionId, session);
                    callbacks.reserve(session.callbacks.size());
                    for (const auto& [_, callback] : session.callbacks)
                    {
                        callbacks.emplace_back(callback);
                    }
                }
            }
            _attachmentSessions.erase(attached);
            _PruneDetachedSessionsLocked();
        }
        for (const auto& callback : callbacks)
        {
            callback(update);
        }
        if (!sessionId.empty())
        {
            _Refresh(sessionId, ActivationEvent::WorkingDirectoryChanged, false, "com.microsoft.intelligent-terminal.git-status");
        }
    }

    void ProviderBroker::UpdateContext(
        const AttachmentId attachment,
        std::filesystem::path workingDirectory,
        const bool authoritative,
        std::optional<std::string> shellType)
    {
        std::string sessionId;
        std::vector<Callback> callbacks;
        BrokerUpdate update;
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
            session.context.workingDirectory = std::move(workingDirectory);
            session.context.workingDirectoryAuthoritative = authoritative;
            session.context.shellType = std::move(shellType);
            ++session.contextRevision;
            ++session.updateSequence;
            _InvalidateGitStatusWorkingDirectoryLocked(session);
            sessionId = session.context.sessionId;
            update = _UpdateFor(sessionId, session);
            callbacks.reserve(session.callbacks.size());
            for (const auto& [_, callback] : session.callbacks)
            {
                callbacks.emplace_back(callback);
            }
        }
        for (const auto& callback : callbacks)
        {
            callback(update);
        }
        _Refresh(sessionId, ActivationEvent::WorkingDirectoryChanged, false);
    }

    void ProviderBroker::UpdateFirstPartyFields(
        const AttachmentId attachment,
        std::unordered_map<std::string, std::string> fields)
    {
        std::string sessionId;
        std::vector<Callback> callbacks;
        BrokerUpdate update;
        {
            std::lock_guard lock{ _mutex };
            const auto attached = _attachmentSessions.find(attachment);
            if (attached == _attachmentSessions.end())
            {
                return;
            }
            auto& session = _sessions.at(attached->second);
            if (session.context.firstPartyFields == fields)
            {
                return;
            }

            session.context.firstPartyFields = std::move(fields);
            ++session.contextRevision;
            for (auto& [_, provider] : session.providers)
            {
                provider.generation = _nextGeneration++;
                provider.pending.reset();
            }
            ++session.updateSequence;
            sessionId = attached->second;
            update = _UpdateFor(sessionId, session);
            callbacks.reserve(session.callbacks.size());
            for (const auto& [_, callback] : session.callbacks)
            {
                callbacks.emplace_back(callback);
            }
        }

        for (const auto& callback : callbacks)
        {
            callback(update);
        }
        _Refresh(sessionId, ActivationEvent::ManualRefresh, false);
    }

    void ProviderBroker::UpdateAgentWorkingDirectory(
        const AttachmentId attachment,
        bool agentActive,
        std::optional<std::string> workingDirectory)
    {
        if (!agentActive || (workingDirectory && workingDirectory->empty()))
        {
            workingDirectory.reset();
        }
        std::string sessionId;
        std::vector<Callback> callbacks;
        BrokerUpdate update;
        {
            std::lock_guard lock{ _mutex };
            const auto attached = _attachmentSessions.find(attachment);
            if (attached == _attachmentSessions.end())
            {
                return;
            }
            auto& session = _sessions.at(attached->second);
            if (agentActive)
            {
                session.activeAgentWorkingDirectories.insert_or_assign(attachment, std::move(workingDirectory));
            }
            else
            {
                session.activeAgentWorkingDirectories.erase(attachment);
            }
            if (!_RecomputeAgentWorkingDirectoryLocked(session))
            {
                return;
            }
            sessionId = attached->second;
            update = _UpdateFor(sessionId, session);
            for (const auto& [_, callback] : session.callbacks)
            {
                callbacks.emplace_back(callback);
            }
        }
        for (const auto& callback : callbacks)
        {
            callback(update);
        }
        _Refresh(sessionId, ActivationEvent::WorkingDirectoryChanged, false, "com.microsoft.intelligent-terminal.git-status");
    }

    void ProviderBroker::_InvalidateGitStatusWorkingDirectoryLocked(SessionState& session)
    {
        for (auto& [id, provider] : session.providers)
        {
            if (std::any_of(_providers.begin(), _providers.end(), [&](const auto& registration) {
                    return registration.manifest.id == id && _IsBuiltInGitStatus(registration);
                }))
            {
                provider.generation = _nextGeneration++;
                provider.pending.reset();
                if (provider.snapshot)
                {
                    _ClearGitStatusCwdDependentFields(*provider.snapshot);
                }
            }
        }
    }

    bool ProviderBroker::_RecomputeAgentWorkingDirectoryLocked(SessionState& session)
    {
        const auto agentActive = !session.activeAgentWorkingDirectories.empty();
        std::optional<std::string> cwd;
        if (agentActive && session.callbacks.size() == 1)
        {
            cwd = session.activeAgentWorkingDirectories.begin()->second;
        }
        if (session.agentActive == agentActive && session.agentWorkingDirectory == cwd)
        {
            return false;
        }
        session.agentActive = agentActive;
        session.agentWorkingDirectory = std::move(cwd);
        ++session.updateSequence;
        _InvalidateGitStatusWorkingDirectoryLocked(session);
        return true;
    }

    void ProviderBroker::Activate(const AttachmentId attachment)
    {
        Notify(attachment, ActivationEvent::TabActivated);
    }

    void ProviderBroker::Notify(
        const AttachmentId attachment,
        const ActivationEvent reason)
    {
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
            _Refresh(sessionId, reason, false);
        }
    }

    void ProviderBroker::_Refresh(
        const std::string& sessionId,
        const ActivationEvent reason,
        const bool initial,
        const std::string_view onlyProvider)
    {
        struct Pending
        {
            Registration provider;
            Request request;
            uint64_t generation{ 0 };
        };
        std::vector<Pending> pending;
        {
            std::lock_guard lock{ _mutex };
            const auto found = _sessions.find(sessionId);
            if (found == _sessions.end() || found->second.callbacks.empty())
            {
                return;
            }
            auto& session = found->second;
            for (const auto& provider : _providers)
            {
                if (!onlyProvider.empty() && provider.manifest.id != onlyProvider)
                {
                    continue;
                }
                if (!onlyProvider.empty() && !_IsBuiltInGitStatus(provider))
                {
                    continue;
                }
                auto activation = reason;
                if (!_Handles(provider.manifest, activation))
                {
                    if (!initial || !_Handles(provider.manifest, ActivationEvent::ManualRefresh))
                    {
                        continue;
                    }
                    activation = ActivationEvent::ManualRefresh;
                }

                std::vector<std::string> visibleFieldsForProvider;
                const auto selected = _visibleFields.find(provider.manifest.id);
                if (selected != _visibleFields.end())
                {
                    visibleFieldsForProvider.assign(selected->second.begin(), selected->second.end());
                }
                else
                {
                    for (const auto& field : provider.manifest.fields)
                    {
                        if (field.defaultVisible)
                        {
                            visibleFieldsForProvider.emplace_back(field.id);
                        }
                    }
                }

                auto& providerState = session.providers[provider.manifest.id];
                if (visibleFieldsForProvider.empty())
                {
                    providerState.generation = _nextGeneration++;
                    providerState.pending.reset();
                    providerState.snapshot.reset();
                    continue;
                }
                const auto generation = _nextGeneration++;
                providerState.generation = generation;
                Request request;
                request.hostRequestId = _nextRequest++;
                request.requestId =
                    std::to_string(_processEpoch) + "-" + std::to_string(request.hostRequestId);
                request.providerId = provider.manifest.id;
                request.processEpoch = _processEpoch;
                request.sessionId = session.context.sessionId;
                request.reason = activation;
                request.workingDirectory = session.context.workingDirectory;
                request.workingDirectoryAuthoritative = session.context.workingDirectoryAuthoritative;
                request.contextRevision = session.contextRevision;
                request.shellType = session.context.shellType;
                request.firstPartyFields = session.context.firstPartyFields;
                if (_IsBuiltInGitStatus(provider))
                {
                    const auto binary = _gitBinary ? _gitBinary->u8string() : std::u8string{};
                    request.firstPartyFields.insert_or_assign("gitBinary", std::string{ binary.begin(), binary.end() });
                    if (session.agentActive)
                    {
                        request.firstPartyFields.insert_or_assign("agentCwdActive", "true");
                        if (session.agentWorkingDirectory)
                        {
                            request.firstPartyFields.insert_or_assign("agentWorkingDirectory", *session.agentWorkingDirectory);
                        }
                    }
                }
                request.visibleFields = std::move(visibleFieldsForProvider);
                if (providerState.running)
                {
                    providerState.pending = PendingRequest{ std::move(request), generation };
                    continue;
                }
                providerState.running = true;
                providerState.runningGeneration = generation;
                pending.push_back(Pending{ provider, std::move(request), generation });
            }
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

    void ProviderBroker::_RunProvider(
        Registration provider,
        Request request,
        const uint64_t generation)
    {
        const auto diagnosticsEnabled =
            _diagnosticLog && _diagnosticLog->Enabled() && _IsDiagnosticEligible(provider);
        ProviderDiagnosticRecord diagnosticRecord;
        if (diagnosticsEnabled)
        {
            diagnosticRecord.event = ProviderDiagnosticEvent::Invocation;
            diagnosticRecord.timestamp = ProviderDiagnosticTimestamp();
            diagnosticRecord.processId = GetCurrentProcessId();
            diagnosticRecord.processEpoch = _processEpoch;
            diagnosticRecord.hostRequestId = request.hostRequestId;
            diagnosticRecord.generation = generation;
            diagnosticRecord.contextRevision = request.contextRevision;
            diagnosticRecord.providerVersion = ParseProviderDiagnosticVersion(provider.manifest.version);
            diagnosticRecord.activationReason = request.reason;
            diagnosticRecord.selectedFields = _FieldMask(request.visibleFields);
            diagnosticRecord.catalogOrigin = provider.origin == RegistrationOrigin::BuiltInCatalog;
        }

        std::vector<std::string> diagnostics;
        const auto serialized = SerializeRequest(request, provider.manifest);
        std::optional<Snapshot> snapshot;
        if (!serialized)
        {
            diagnostics = serialized.errors;
        }
        else
        {
            const auto started = std::chrono::steady_clock::now();
            const auto command = _runner.Run(provider.manifest, *serialized.value, ProviderTimeout);
            const auto finished = std::chrono::steady_clock::now();
            if (diagnosticsEnabled)
            {
                diagnosticRecord.runnerAttempted = true;
                diagnosticRecord.durationMicroseconds = static_cast<uint64_t>(
                    std::chrono::duration_cast<std::chrono::microseconds>(finished - started).count());
                diagnosticRecord.runnerStatus = command.status;
                diagnosticRecord.processStarted = command.processStarted;
                diagnosticRecord.exitCode = command.exitCode;
                diagnosticRecord.exitCodeValid = command.exitCodeValid;
                diagnosticRecord.win32Error = command.win32Error;
                diagnosticRecord.win32ErrorValid = command.win32ErrorValid;
                diagnosticRecord.outputLimitObserved = command.outputLimitObserved;
                diagnosticRecord.standardOutputExceeded = command.standardOutputExceeded;
                diagnosticRecord.standardErrorExceeded = command.standardErrorExceeded;
            }
            if (command.status != CommandResult::Status::Completed || command.exitCode != 0)
            {
                diagnostics.emplace_back(
                    "Provider '" + provider.manifest.id + "' failed with status " +
                    std::to_string(static_cast<int>(command.status)) +
                    " and exit code " + std::to_string(command.exitCode));
            }
            else
            {
                const auto parsed = ParseSnapshot(
                    command.standardOutput,
                    provider.manifest,
                    request.requestId);
                if (parsed)
                {
                    snapshot = *parsed.value;
                    if (diagnosticsEnabled)
                    {
                        diagnosticRecord.parseOutcome = ProviderParseOutcome::Valid;
                        diagnosticRecord.returnedFields = _FieldMask(*snapshot);
                    }
                    if (const auto status = request.firstPartyFields.find("agentStatusId");
                        status != request.firstPartyFields.end())
                    {
                        snapshot->agentStatus = status->second;
                    }
                }
                else
                {
                    if (diagnosticsEnabled)
                    {
                        diagnosticRecord.parseOutcome = ProviderParseOutcome::Invalid;
                    }
                    diagnostics.insert(diagnostics.end(), parsed.errors.begin(), parsed.errors.end());
                }
            }
            if (!command.standardError.empty())
            {
                diagnostics.emplace_back(command.standardError);
            }
        }

        std::vector<Callback> callbacks;
        BrokerUpdate update;
        std::optional<PendingRequest> next;
        auto disposition = ProviderBrokerDisposition::NotApplicable;
        {
            std::lock_guard lock{ _mutex };
            const auto session = _sessions.find(request.sessionId);
            if (session == _sessions.end())
            {
                disposition = ProviderBrokerDisposition::SessionMissing;
            }
            else
            {
                const auto state = session->second.providers.find(provider.manifest.id);
                if (state == session->second.providers.end())
                {
                    disposition = ProviderBrokerDisposition::ProviderStateMissing;
                }
                else if (!state->second.running ||
                         state->second.runningGeneration != generation)
                {
                    disposition = ProviderBrokerDisposition::RunningGenerationMismatch;
                }
                else
                {
                    state->second.running = false;
                    if (state->second.generation != generation)
                    {
                        disposition = ProviderBrokerDisposition::GenerationMismatch;
                    }
                    else if (session->second.contextRevision != request.contextRevision)
                    {
                        disposition = ProviderBrokerDisposition::ContextRevisionMismatch;
                    }
                    else
                    {
                        disposition = ProviderBrokerDisposition::Accepted;
                        state->second.snapshot = std::move(snapshot);
                        ++session->second.updateSequence;
                        update = _UpdateFor(request.sessionId, session->second, std::move(diagnostics));
                        callbacks.reserve(session->second.callbacks.size());
                        for (const auto& [_, callback] : session->second.callbacks)
                        {
                            callbacks.emplace_back(callback);
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
            }
        }

        if (diagnosticsEnabled)
        {
            diagnosticRecord.brokerDisposition = disposition;
            _diagnosticLog->TryLog(diagnosticRecord);
        }
        for (const auto& callback : callbacks)
        {
            callback(update);
        }
        if (next)
        {
            _Enqueue(
                [this,
                 provider = std::move(provider),
                 request = std::move(next->request),
                 generation = next->generation]() mutable {
                    _RunProvider(std::move(provider), std::move(request), generation);
                });
        }
    }

    void ProviderBroker::_PruneDetachedSessionsLocked()
    {
        const auto oldestRetained = std::chrono::steady_clock::now() - DetachedSessionRetention;
        std::erase_if(_sessions, [&](const auto& item) {
            const auto& session = item.second;
            return session.callbacks.empty() &&
                   session.detachedAt &&
                   *session.detachedAt <= oldestRetained;
        });
    }

    BrokerUpdate ProviderBroker::_UpdateFor(
        const std::string& sessionId,
        const SessionState& state,
        std::vector<std::string> diagnostics) const
    {
        std::unordered_map<std::string, Snapshot> snapshots;
        for (const auto& [id, provider] : state.providers)
        {
            if (provider.snapshot)
            {
                snapshots.emplace(id, *provider.snapshot);
            }
        }
        return BrokerUpdate{
            sessionId,
            state.sessionIncarnation,
            state.contextRevision,
            state.updateSequence,
            ComposePresentation(_providers, snapshots, _visibleFields, _fieldDisplayNames),
            std::move(diagnostics)
        };
    }

    std::optional<Presentation> ProviderBroker::ComposePresentation(
        const std::vector<Registration>& providers,
        const std::unordered_map<std::string, Snapshot>& snapshots,
        const VisibleFieldMap& visibleFields,
        const FieldDisplayNameMap& fieldDisplayNames)
    {
        Presentation result;
        for (const auto& provider : providers)
        {
            const auto snapshot = snapshots.find(provider.manifest.id);
            if (snapshot == snapshots.end())
            {
                continue;
            }
            const auto selectedFields = visibleFields.find(provider.manifest.id);
            for (const auto& field : provider.manifest.fields)
            {
                const auto visible = selectedFields != visibleFields.end() ?
                                         selectedFields->second.contains(field.id) :
                                         field.defaultVisible;
                if (!visible)
                {
                    continue;
                }
                if (const auto value = snapshot->second.fields.find(field.id);
                    value != snapshot->second.fields.end())
                {
                    const auto valueText = _ValueText(value->second);
                    _Append(result.text, valueText, L"\n");
                    if (field.id == "agentStatus" && !valueText.empty())
                    {
                        result.agentStatusRanges.push_back({ result.text.size() - valueText.size(), valueText.size() });
                        result.agentStatus = snapshot->second.agentStatus;
                    }

                    auto displayName = field.displayName;
                    if (const auto providerNames = fieldDisplayNames.find(provider.manifest.id);
                        providerNames != fieldDisplayNames.end())
                    {
                        if (const auto localizedName = providerNames->second.find(field.id);
                            localizedName != providerNames->second.end())
                        {
                            displayName = localizedName->second;
                        }
                    }

                    auto accessibilityText = _ToWide(displayName);
                    accessibilityText.append(L": ");
                    accessibilityText.append(valueText);
                    _Append(result.accessibilityText, accessibilityText, L", ");
                }
            }
            if (snapshot->second.tooltip)
            {
                _Append(
                    result.tooltip,
                    _ToWide(*snapshot->second.tooltip),
                    L"\n");
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
}
