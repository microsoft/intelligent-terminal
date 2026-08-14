// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderBroker.h"
#include "BuiltInProviderCatalog.h"

#include <windows.h>
#include <bcrypt.h>
#include <winrt/base.h>

#include <algorithm>
#include <charconv>
#include <thread>
#include <unordered_set>
#include <utility>

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
    }

    ProviderBroker& ProviderBroker::Instance()
    {
        static auto instance = new ProviderBroker{};
        return *instance;
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
        for (size_t index = 0; index < WorkerCount; ++index)
        {
            std::thread{ [this]() {
                winrt::init_apartment(winrt::apartment_type::multi_threaded);
                for (;;)
                {
                    std::function<void()> work;
                    {
                        std::unique_lock lock{ _executorMutex };
                        _executorCondition.wait(lock, [&]() { return !_executorQueue.empty(); });
                        work = std::move(_executorQueue.front());
                        _executorQueue.pop_front();
                    }
                    work();
                }
            } }.detach();
        }
        ReloadProviders();
    }

    void ProviderBroker::_Enqueue(std::function<void()> work)
    {
        {
            std::lock_guard lock{ _executorMutex };
            _executorQueue.emplace_back(std::move(work));
        }
        _executorCondition.notify_one();
    }

    uint64_t ProviderBroker::ProcessEpoch() const noexcept
    {
        return _processEpoch;
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

        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        std::vector<std::string> sessionsToRefresh;
        std::unordered_set<std::string> refreshProviders;
        {
            std::lock_guard lock{ _mutex };
            const auto previous = _providers;
            _catalog = std::move(merged.descriptors);
            _availableProviders = std::move(merged.available);
            _providers = _EffectiveProvidersLocked();
            _UpdateCatalogEffectiveStateLocked();
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
                        provider.generation = _nextGeneration++;
                        provider.pending.reset();
                        provider.snapshot.reset();
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
            _registryStamp = _RegistryStamp();
        }

        for (const auto& [callback, update] : notifications)
        {
            callback(update);
        }
        for (const auto& sessionId : sessionsToRefresh)
        {
            _Refresh(sessionId, ActivationEvent::ManualRefresh, true, refreshProviders);
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
            auto watcher = AppExtensionProviderCatalog::WatchForChanges([this]() {
                _ScheduleAppExtensionDiscovery();
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

    void ProviderBroker::ApplyPreferences(std::vector<ProviderPreference> preferences)
    {
        preferences = _NormalizePreferences(std::move(preferences));
        std::vector<std::pair<Callback, BrokerUpdate>> notifications;
        std::vector<std::string> sessionsToRefresh;
        std::unordered_set<std::string> refreshProviders;
        {
            std::lock_guard lock{ _mutex };
            if (_preferences == preferences)
            {
                return;
            }

            const auto previous = _providers;
            _preferences = std::move(preferences);
            _providers = _EffectiveProvidersLocked();
            _UpdateCatalogEffectiveStateLocked();
            refreshProviders = _ProvidersNeedingRefresh(previous, _providers);

            for (auto& [sessionId, session] : _sessions)
            {
                for (auto& [id, provider] : session.providers)
                {
                    if (_FindProvider(previous, id) && !_FindProvider(_providers, id))
                    {
                        provider.generation = _nextGeneration++;
                        provider.pending.reset();
                        provider.snapshot.reset();
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
        }

        for (const auto& [callback, update] : notifications)
        {
            callback(update);
        }
        for (const auto& sessionId : sessionsToRefresh)
        {
            _Refresh(sessionId, ActivationEvent::ManualRefresh, true, refreshProviders);
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
        _ReloadProvidersIfChanged();
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
            _Refresh(sessionId, reason, false);
        }
    }

    void ProviderBroker::_Refresh(
        const std::string& sessionId,
        const ActivationEvent reason,
        const bool initial,
        const std::unordered_set<std::string>& providerIds)
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
                if (!providerIds.empty() && !providerIds.contains(provider.manifest.id))
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
                if (providerState.running)
                {
                    providerState.pending = PendingRequest{ provider, std::move(request), generation };
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
                 provider = std::move(next->provider),
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
            state.contextRevision,
            state.updateSequence,
            ComposePresentation(_providers, snapshots, _preferences),
            std::move(diagnostics)
        };
    }

    std::optional<Presentation> ProviderBroker::ComposePresentation(
        const std::vector<Registration>& providers,
        const std::unordered_map<std::string, Snapshot>& snapshots,
        const std::vector<ProviderPreference>& preferences)
    {
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

            const auto previousLength = result.text.size();
            for (const auto field : visibleFields)
            {
                if (const auto value = snapshot->second.fields.find(field->id);
                    value != snapshot->second.fields.end())
                {
                    _Append(result.text, _ValueText(value->second), L" \u00b7 ");
                }
            }
            if (result.text.size() == previousLength)
            {
                continue;
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
