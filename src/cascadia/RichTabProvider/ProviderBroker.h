// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AppExtensionProviderCatalog.h"
#include "CommandRunner.h"
#include "PersistentProviderSupervisor.h"
#include "ProviderRegistry.h"

#include <chrono>
#include <functional>
#include <atomic>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <optional>
#include <thread>
#include <unordered_map>
#include <unordered_set>

namespace Microsoft::Terminal::RichTab::Provider
{
    using FieldChangeSequences = std::unordered_map<std::string, std::unordered_map<std::string, uint64_t>>;

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
        uint64_t sessionIncarnation{ 0 };
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

    struct PublishResult
    {
        bool succeeded{ false };
        std::string message;
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
        void ApplyPreferences(
            std::vector<ProviderPreference> preferences,
            bool prioritizeRecentlyUpdatedFields = false);
        std::vector<ProviderDescriptor> Catalog();
        RegistryResult<bool> SetProviderConsent(
            std::string_view id,
            std::string_view consentKey,
            bool enabled);

        uint64_t ProcessEpoch() const noexcept;
        PublishResult Publish(std::string_view lease, std::string_view snapshotJson);

        static std::optional<Presentation> ComposePresentation(
            const std::vector<Registration>& providers,
            const std::unordered_map<std::string, Snapshot>& snapshots,
            const std::vector<ProviderPreference>& preferences = {},
            const FieldChangeSequences& fieldChangeSequences = {},
            bool prioritizeRecentlyUpdatedFields = false);
        static bool UpdateFieldChangeSequences(
            const Snapshot& snapshot,
            std::optional<std::unordered_map<std::string, FieldValue>>& baseline,
            std::unordered_map<std::string, uint64_t>& changeSequences,
            uint64_t& nextChangeSequence);
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
            std::optional<std::unordered_map<std::string, FieldValue>> fieldBaseline;
            std::unordered_map<std::string, uint64_t> fieldChangeSequences;
            uint64_t publishedGeneration{ 0 };
            uint64_t publishedUpdateSequence{ 0 };
            uint64_t persistentInstanceGeneration{ 0 };
            std::string activeRequestId;
            ActivationEvent lastActivation{ ActivationEvent::ManualRefresh };
        };

        struct PublishLease
        {
            Registration provider;
            Request request;
            uint64_t generation{ 0 };
            uint64_t instanceGeneration{ 0 };
            bool persistent{ false };
            std::chrono::steady_clock::time_point expiresAt;
        };

        struct SessionState
        {
            SessionContext context;
            uint64_t sessionIncarnation{ 0 };
            uint64_t contextRevision{ 0 };
            uint64_t updateSequence{ 0 };
            std::optional<std::chrono::steady_clock::time_point> detachedAt;
            std::unordered_map<AttachmentId, Callback> callbacks;
            std::unordered_map<std::string, ProviderState> providers;
            uint64_t nextFieldChangeSequence{ 0 };
            std::optional<bool> diagnosticPresentationPresent;
        };

        struct CallbackLifetime
        {
            std::mutex mutex;
            ProviderBroker* owner{ nullptr };
        };

        ProviderBroker();
        ~ProviderBroker();

        void _Refresh(
            const std::string& sessionId,
            ActivationEvent reason,
            bool initial,
            const std::unordered_set<std::string>& providerIds = {},
            bool forcePersistentRestart = false,
            bool resetPersistentBackoff = false);
        void _RunProvider(
            Registration provider,
            Request request,
            uint64_t generation);
        void _RunPersistentProvider(
            Registration provider,
            Request request,
            uint64_t generation,
            bool forceRestart,
            bool resetBackoff);
        void _OnPersistentProviderEvent(const PersistentProviderEvent& event);
        static void _DeliverCallback(
            const Callback& callback,
            const BrokerUpdate& update) noexcept;
        static void _DeliverCallbacks(
            const std::vector<Callback>& callbacks,
            const BrokerUpdate& update) noexcept;
        BrokerUpdate _UpdateFor(
            const std::string& sessionId,
            const SessionState& state,
            std::vector<std::string> diagnostics = {}) const;
        void _Enqueue(std::function<void()> work);
        void _ReloadProvidersIfChanged();
        void _ReloadProvidersFromSources();
        void _ScheduleAppExtensionDiscovery();
        std::vector<std::string> _PruneDetachedSessionsLocked();
        void _HousekeepingWorker();
        uint64_t _RegistryStamp() const noexcept;
        std::vector<Registration> _EffectiveProvidersLocked() const;
        void _UpdateCatalogEffectiveStateLocked();
        std::string _CreatePublishLeaseLocked(
            const Registration& provider,
            const Request& request,
            uint64_t generation,
            std::chrono::steady_clock::duration lifetime,
            uint64_t instanceGeneration = 0,
            bool persistent = false);
        void _InvalidatePublishLeasesLocked(
            std::string_view sessionId,
            std::string_view providerId = {});
        bool _ValidatePublishLeaseLocked(
            const PublishLease& binding,
            SessionState*& session,
            ProviderState*& providerState);
        std::optional<std::string> _CreatePersistentControlFrameLocked(
            const Registration& provider,
            const Request& request,
            uint64_t generation,
            uint64_t instanceGeneration,
            bool started);
        void _ReplenishPersistentGrants(const PublishLease& binding);

        mutable std::mutex _mutex;
        std::mutex _reloadMutex;
        std::mutex _executorMutex;
        std::condition_variable _executorCondition;
        std::deque<std::function<void()>> _executorQueue;
        std::vector<std::thread> _executorWorkers;
        bool _executorStopping{ false };
        std::mutex _housekeepingMutex;
        std::condition_variable _housekeepingCondition;
        std::thread _housekeepingThread;
        bool _housekeepingStopping{ false };
        ProviderRegistry _registry;
        CommandRunner _runner;
        PersistentProviderSupervisor _persistentSupervisor;
        std::vector<ProviderDescriptor> _catalog;
        std::vector<Registration> _availableProviders;
        std::vector<Registration> _providers;
        std::vector<ProviderPreference> _preferences;
        bool _prioritizeRecentlyUpdatedFields{ false };
        AppExtensionDiscoveryResult _appExtensionDiscovery;
        bool _appExtensionDiscoveryScheduled{ false };
        bool _appExtensionDiscoveryPending{ false };
        std::shared_ptr<void> _appExtensionWatcher;
        std::shared_ptr<CallbackLifetime> _callbackLifetime;
        std::unordered_map<std::string, SessionState> _sessions;
        std::unordered_map<AttachmentId, std::string> _attachmentSessions;
        std::unordered_map<std::string, PublishLease> _publishLeases;
        uint64_t _processEpoch{ 0 };
        uint64_t _catalogRevision{ 0 };
        uint64_t _nextSessionIncarnation{ 1 };
        uint64_t _nextAttachment{ 1 };
        uint64_t _nextRequest{ 1 };
        uint64_t _nextGeneration{ 1 };
        std::atomic<uint64_t> _registryStamp{ 0 };
    };
}
