// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CommandRunner.h"
#include "ProviderRegistry.h"

#include <chrono>
#include <functional>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <optional>
#include <thread>
#include <unordered_map>
#include <unordered_set>

namespace Microsoft::Terminal::RichTab::Provider
{
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
        std::unordered_map<std::string, std::string> firstPartyFields;
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

    class ProviderBroker
    {
    public:
        using AttachmentId = uint64_t;
        using Callback = std::function<void(const BrokerUpdate&)>;
        using VisibleFieldMap = std::unordered_map<std::string, std::unordered_set<std::string>>;

        static ProviderBroker& Instance();

        AttachmentId Attach(SessionContext context, Callback callback);
        void Detach(AttachmentId attachment);
        void UpdateContext(
            AttachmentId attachment,
            std::filesystem::path workingDirectory,
            bool authoritative,
            std::optional<std::string> shellType);
        void UpdateFirstPartyFields(
            AttachmentId attachment,
            std::unordered_map<std::string, std::string> fields);
        void Activate(AttachmentId attachment);
        void Notify(AttachmentId attachment, ActivationEvent reason);
        void ReloadProviders();
        void SetVisibleFields(std::string_view providerId, std::vector<std::string> fields);
        std::optional<std::vector<std::string>> VisibleFields(std::string_view providerId) const;

        uint64_t ProcessEpoch() const noexcept;

        static std::optional<Presentation> ComposePresentation(
            const std::vector<Registration>& providers,
            const std::unordered_map<std::string, Snapshot>& snapshots,
            const VisibleFieldMap& visibleFields = {});

    private:
        struct PendingRequest
        {
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
            uint64_t sessionIncarnation{ 0 };
            uint64_t contextRevision{ 0 };
            uint64_t updateSequence{ 0 };
            std::optional<std::chrono::steady_clock::time_point> detachedAt;
            std::unordered_map<AttachmentId, Callback> callbacks;
            std::unordered_map<std::string, ProviderState> providers;
        };

        ProviderBroker();
        ~ProviderBroker();

        void _Refresh(
            const std::string& sessionId,
            ActivationEvent reason,
            bool initial);
        void _RunProvider(
            Registration provider,
            Request request,
            uint64_t generation);
        BrokerUpdate _UpdateFor(
            const std::string& sessionId,
            const SessionState& state,
            std::vector<std::string> diagnostics = {}) const;
        void _Enqueue(std::function<void()> work);
        void _PruneDetachedSessionsLocked();

        mutable std::mutex _mutex;
        std::mutex _executorMutex;
        std::condition_variable _executorCondition;
        std::deque<std::function<void()>> _executorQueue;
        std::vector<std::thread> _executorWorkers;
        bool _executorStopping{ false };
        CommandRunner _runner;
        std::vector<Registration> _providers;
        VisibleFieldMap _visibleFields;
        std::unordered_map<std::string, SessionState> _sessions;
        std::unordered_map<AttachmentId, std::string> _attachmentSessions;
        uint64_t _processEpoch{ 0 };
        uint64_t _nextSessionIncarnation{ 1 };
        uint64_t _nextAttachment{ 1 };
        uint64_t _nextRequest{ 1 };
        uint64_t _nextGeneration{ 1 };
    };
}
