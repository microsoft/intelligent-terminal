// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

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
    enum class ProviderSourceKind
    {
        BuiltIn,
        Managed,
        Development,
    };

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
        ProviderSourceKind source{ ProviderSourceKind::Managed };
        bool consentEnabled{ false };
        bool integrityValid{ false };
        bool eligible{ false };
        bool effectiveEnabled{ false };
        bool shadowed{ false };
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

        uint64_t ProcessEpoch() const noexcept;

        static std::optional<Presentation> ComposePresentation(
            const std::vector<Registration>& providers,
            const std::unordered_map<std::string, Snapshot>& snapshots,
            const std::vector<ProviderPreference>& preferences = {});

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
        std::unordered_map<std::string, SessionState> _sessions;
        std::unordered_map<AttachmentId, std::string> _attachmentSessions;
        uint64_t _processEpoch{ 0 };
        uint64_t _nextAttachment{ 1 };
        uint64_t _nextRequest{ 1 };
        uint64_t _nextGeneration{ 1 };
        std::atomic<uint64_t> _registryStamp{ 0 };
    };
}
