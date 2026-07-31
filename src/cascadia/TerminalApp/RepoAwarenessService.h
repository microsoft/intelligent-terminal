// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "LocalGitProvider.h"

#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <thread>
#include <unordered_map>

namespace Microsoft::Terminal::RepoAwareness
{
    enum class RepoAvailability
    {
        ShellIntegrationRequired,
        Loading,
        Ready,
        NotRepository,
        GitUnavailable,
        UnsupportedWorkingDirectory,
        Error,
    };

    struct RepoSummary
    {
        RepoAvailability availability = RepoAvailability::ShellIntegrationRequired;
        std::optional<std::string> branch;
        std::string headOid;
        std::optional<std::string> upstream;
        uint64_t ahead = 0;
        uint64_t behind = 0;
        uint64_t modifiedCount = 0;
        uint64_t stagedCount = 0;
        uint64_t untrackedCount = 0;
        uint64_t conflictedCount = 0;
        uint64_t generation = 0;
        bool detached = false;
        bool unborn = false;
        bool stale = false;
        bool filesTruncated = false;
    };

    class RepoAwarenessService
    {
    public:
        using RefreshFunction = std::function<LocalGitResult(const std::filesystem::path&, size_t, const std::atomic_bool*)>;
        using SummaryChangedCallback = std::function<void(const std::string&, const RepoSummary&)>;

        RepoAwarenessService();
        explicit RepoAwarenessService(RefreshFunction refresh,
                                      std::chrono::milliseconds ttl = std::chrono::seconds{ 5 },
                                      std::chrono::milliseconds cacheGrace = std::chrono::seconds{ 30 });
        ~RepoAwarenessService();

        RepoAwarenessService(const RepoAwarenessService&) = delete;
        RepoAwarenessService& operator=(const RepoAwarenessService&) = delete;

        static RepoAwarenessService& Instance();

        void ObservePane(std::string sessionId,
                         std::filesystem::path workingDirectory,
                         bool workingDirectoryReportedByShell,
                         bool commandMarksReportedByShell,
                         bool commandCompleted);
        void RemovePane(std::string_view sessionId);

        [[nodiscard]] RepoSummary GetSummary(std::string_view sessionId, bool forceRefresh = false);
        void AddConsumer();
        void RemoveConsumer();
        void SetConsumerCount(size_t count);
        [[nodiscard]] uint64_t SubscribeSummaryChanged(SummaryChangedCallback callback);
        void UnsubscribeSummaryChanged(uint64_t token);

        bool WaitForIdle(std::chrono::milliseconds timeout);

    private:
        struct PaneState
        {
            std::filesystem::path workingDirectory;
            std::optional<std::wstring> worktreeKey;
            LocalGitError lastError = LocalGitError::None;
            uint64_t generation = 0;
            bool ready = false;
            bool refreshPending = false;
        };

        struct CacheEntry
        {
            LocalRepoSnapshot snapshot;
            std::chrono::steady_clock::time_point updated;
            std::optional<std::chrono::steady_clock::time_point> unreferencedSince;
            uint64_t generation = 0;
        };

        struct RefreshRequest
        {
            std::string sessionId;
            std::filesystem::path workingDirectory;
            uint64_t paneGeneration = 0;
            std::chrono::steady_clock::time_point queuedAt;
            bool requiresConsumer = true;
        };

        void _enqueueRefreshLocked(const std::string& sessionId, PaneState& pane, bool refreshCached, bool requiresConsumer);
        void _dropConsumerRequestsLocked();
        RepoSummary _getSummaryLocked(const std::string& sessionId, PaneState& pane, bool forceRefresh);
        std::optional<std::wstring> _findCachedWorktreeLocked(const std::filesystem::path& workingDirectory) const;
        void _markUnreferencedLocked(const std::optional<std::wstring>& worktreeKey);
        void _evictUnusedLocked();
        void _workerLoop();

        RefreshFunction _refresh;
        const std::chrono::milliseconds _ttl;
        const std::chrono::milliseconds _cacheGrace;
        std::mutex _mutex;
        std::condition_variable _wakeWorker;
        std::condition_variable _idle;
        std::unordered_map<std::string, PaneState> _panes;
        std::unordered_map<std::wstring, CacheEntry> _cache;
        std::unordered_map<std::wstring, std::wstring> _cwdToWorktree;
        std::deque<RefreshRequest> _requests;
        std::unordered_map<uint64_t, SummaryChangedCallback> _summaryChangedCallbacks;
        std::atomic_bool _stopping{ false };
        size_t _consumerCount = 0;
        uint64_t _nextSummaryChangedToken = 1;
        uint64_t _nextSnapshotGeneration = 1;
        bool _workerBusy = false;
        std::thread _worker;
    };
}
