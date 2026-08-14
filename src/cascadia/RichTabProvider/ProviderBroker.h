// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AppExtensionProviderCatalog.h"
#include "CommandRunner.h"
#include "ProviderRegistry.h"

#include <chrono>
#include <functional>
#include <atomic>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <optional>
#include <unordered_map>
#include <unordered_set>

namespace Microsoft::Terminal::RichTab::Provider
{
    struct ProviderPreference
    {
        std::string id;
        std::optional<bool> enabled;
        std::optional<std::vector<std::string>> fields;

        bool operator==(const ProviderPreference&) const = default;
    };

    struct ProviderDescriptor
    {
        std::string id;
        std::string displayName;
        ProviderSourceKind source{ ProviderSourceKind::LegacyManaged };
        bool consentEnabled{ false };
        bool integrityValid{ false };
        bool eligible{ false };
        bool effectiveEnabled{ false };
        bool shadowed{ false };
        std::string consentKey;
        std::vector<FieldDeclaration> fields;
    };

    struct Presentation
    {
        std::wstring text;
        std::wstring tooltip;
        std::wstring accessibilityText;

        bool operator==(const Presentation&) const = default;
    };

    struct SessionContext
    {
        std::string sessionId;
        std::filesystem::path workingDirectory;
        bool workingDirectoryAuthoritative{ false };
        std::optional<std::string> shellType;
    };

    struct BrokerUpdate
    {
        std::string sessionId;
        uint64_t contextRevision{ 0 };
        uint64_t updateSequence{ 0 };
        std::optional<Presentation> presentation;
        std::vector<std::string> diagnostics;
    };

    struct ProviderCatalogSnapshot
    {
        std::vector<Registration> available;
        std::vector<ProviderDescriptor> descriptors;
    };

    class ProviderBroker
    {
    public:
        using AttachmentId = uint64_t;
        using Callback = std::function<void(const BrokerUpdate&)>;

        static ProviderBroker& Instance();

        AttachmentId Attach(SessionContext context, Callback callback);
        void Detach(AttachmentId attachment);
        void UpdateContext(
            AttachmentId attachment,
            std::filesystem::path workingDirectory,
            bool authoritative,
            std::optional<std::string> shellType);
        void Activate(AttachmentId attachment);
        void Notify(AttachmentId attachment, ActivationEvent reason);
        void ReloadProviders();
        void ApplyPreferences(std::vector<ProviderPreference> preferences);
        std::vector<ProviderDescriptor> Catalog();
        RegistryResult<bool> SetProviderConsent(
            std::string_view id,
            std::string_view consentKey,
            bool enabled);

        uint64_t ProcessEpoch() const noexcept;

        static std::optional<Presentation> ComposePresentation(
            const std::vector<Registration>& providers,
            const std::unordered_map<std::string, Snapshot>& snapshots,
            const std::vector<ProviderPreference>& preferences = {});
        static ProviderCatalogSnapshot BuildCatalog(
            std::vector<Registration> candidates);

    private:
        struct PendingRequest
        {
            Registration provider;
            Request request;
            uint64_t generation{ 0 };
        };

        struct ProviderState
        {
            uint64_t generation{ 0 };
            uint64_t runningGeneration{ 0 };
            bool running{ false };
            std::optional<PendingRequest> pending;
            std::optional<Snapshot> snapshot;
        };

        struct SessionState
        {
            SessionContext context;
            uint64_t contextRevision{ 0 };
            uint64_t updateSequence{ 0 };
            std::optional<std::chrono::steady_clock::time_point> detachedAt;
            std::unordered_map<AttachmentId, Callback> callbacks;
            std::unordered_map<std::string, ProviderState> providers;
        };

        ProviderBroker();

        void _Refresh(
            const std::string& sessionId,
            ActivationEvent reason,
            bool initial,
            const std::unordered_set<std::string>& providerIds = {});
        void _RunProvider(
            Registration provider,
            Request request,
            uint64_t generation);
        BrokerUpdate _UpdateFor(
            const std::string& sessionId,
            const SessionState& state,
            std::vector<std::string> diagnostics = {}) const;
        void _Enqueue(std::function<void()> work);
        void _ReloadProvidersIfChanged();
        void _ReloadProvidersFromSources();
        void _ScheduleAppExtensionDiscovery();
        void _PruneDetachedSessionsLocked();
        uint64_t _RegistryStamp() const noexcept;
        std::vector<Registration> _EffectiveProvidersLocked() const;
        void _UpdateCatalogEffectiveStateLocked();

        mutable std::mutex _mutex;
        std::mutex _reloadMutex;
        std::mutex _executorMutex;
        std::condition_variable _executorCondition;
        std::deque<std::function<void()>> _executorQueue;
        ProviderRegistry _registry;
        CommandRunner _runner;
        std::vector<ProviderDescriptor> _catalog;
        std::vector<Registration> _availableProviders;
        std::vector<Registration> _providers;
        std::vector<ProviderPreference> _preferences;
        AppExtensionDiscoveryResult _appExtensionDiscovery;
        bool _appExtensionDiscoveryScheduled{ false };
        bool _appExtensionDiscoveryPending{ false };
        std::shared_ptr<void> _appExtensionWatcher;
        std::unordered_map<std::string, SessionState> _sessions;
        std::unordered_map<AttachmentId, std::string> _attachmentSessions;
        uint64_t _processEpoch{ 0 };
        uint64_t _nextAttachment{ 1 };
        uint64_t _nextRequest{ 1 };
        uint64_t _nextGeneration{ 1 };
        std::atomic<uint64_t> _registryStamp{ 0 };
    };
}
