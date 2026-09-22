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

    void ProviderBroker::ReloadProviders()
    {
        auto builtIns = BuiltInProviderCatalog::Load(BuiltInProviderCatalog::PackageRoot());
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
            _visibleFields.insert_or_assign(provider->manifest.id, std::move(selectedFields));

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
                    provider.snapshot.reset();
                }
                ++session.updateSequence;
            }
            session.callbacks.emplace(attachment, std::move(callback));
            _attachmentSessions.emplace(attachment, session.context.sessionId);
            existing = _UpdateFor(session.context.sessionId, session);
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
        std::lock_guard lock{ _mutex };
        const auto attached = _attachmentSessions.find(attachment);
        if (attached == _attachmentSessions.end())
        {
            return;
        }
        if (const auto session = _sessions.find(attached->second); session != _sessions.end())
        {
            session->second.callbacks.erase(attachment);
            if (session->second.callbacks.empty())
            {
                for (auto& [_, provider] : session->second.providers)
                {
                    provider.generation = _nextGeneration++;
                    provider.pending.reset();
                }
                session->second.detachedAt = std::chrono::steady_clock::now();
            }
        }
        _attachmentSessions.erase(attached);
        _PruneDetachedSessionsLocked();
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
            for (auto& [_, provider] : session.providers)
            {
                provider.snapshot.reset();
            }
            ++session.updateSequence;
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
                provider.snapshot.reset();
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
        const bool initial)
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
                auto activation = reason;
                if (!_Handles(provider.manifest, activation))
                {
                    if (!initial || !_Handles(provider.manifest, ActivationEvent::ManualRefresh))
                    {
                        continue;
                    }
                    activation = ActivationEvent::ManualRefresh;
                }

                auto& providerState = session.providers[provider.manifest.id];
                const auto generation = _nextGeneration++;
                providerState.generation = generation;
                Request request;
                request.requestId =
                    std::to_string(_processEpoch) + "-" + std::to_string(_nextRequest++);
                request.providerId = provider.manifest.id;
                request.processEpoch = _processEpoch;
                request.sessionId = session.context.sessionId;
                request.reason = activation;
                request.workingDirectory = session.context.workingDirectory;
                request.workingDirectoryAuthoritative = session.context.workingDirectoryAuthoritative;
                request.contextRevision = session.contextRevision;
                request.shellType = session.context.shellType;
                request.firstPartyFields = session.context.firstPartyFields;
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
        std::vector<std::string> diagnostics;
        const auto serialized = SerializeRequest(request, provider.manifest);
        std::optional<Snapshot> snapshot;
        if (!serialized)
        {
            diagnostics = serialized.errors;
        }
        else
        {
            const auto command = _runner.Run(provider.manifest, *serialized.value, ProviderTimeout);
            if (command.status != CommandResult::Status::Completed || command.exitCode != 0)
            {
                diagnostics.emplace_back(
                    "Provider '" + provider.manifest.id + "' failed with status " +
                    std::to_string(static_cast<int>(command.status)) +
                    " and exit code " + std::to_string(command.exitCode));
                if (!command.standardError.empty())
                {
                    diagnostics.emplace_back(command.standardError);
                }
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
                }
                else
                {
                    diagnostics = parsed.errors;
                }
            }
        }

        std::vector<Callback> callbacks;
        BrokerUpdate update;
        std::optional<PendingRequest> next;
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

            if (state->second.generation == generation &&
                session->second.contextRevision == request.contextRevision)
            {
                if (snapshot)
                {
                    state->second.snapshot = std::move(snapshot);
                }
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
            ComposePresentation(_providers, snapshots, _visibleFields),
            std::move(diagnostics)
        };
    }

    std::optional<Presentation> ProviderBroker::ComposePresentation(
        const std::vector<Registration>& providers,
        const std::unordered_map<std::string, Snapshot>& snapshots,
        const VisibleFieldMap& visibleFields)
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
                    _Append(result.text, _ValueText(value->second), L" \u00b7 ");
                }
            }
            if (snapshot->second.tooltip)
            {
                _Append(
                    result.tooltip,
                    _ToWide(*snapshot->second.tooltip),
                    L"\n");
            }
            if (snapshot->second.accessibilityText)
            {
                _Append(
                    result.accessibilityText,
                    _ToWide(*snapshot->second.accessibilityText),
                    L", ");
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
