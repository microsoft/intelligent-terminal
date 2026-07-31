// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "RepoAwarenessService.h"

namespace Microsoft::Terminal::RepoAwareness
{
    namespace
    {
        std::wstring _worktreeKey(const std::filesystem::path& root)
        {
            auto key = root.lexically_normal().native();
            std::transform(key.begin(), key.end(), key.begin(), [](const wchar_t character) {
                return static_cast<wchar_t>(towlower(character));
            });
            std::replace(key.begin(), key.end(), L'/', L'\\');
            while (key.size() > 3 && key.back() == L'\\')
            {
                key.pop_back();
            }
            return key;
        }

        RepoAvailability _availabilityForError(const LocalGitError error)
        {
            switch (error)
            {
            case LocalGitError::NotRepository:
                return RepoAvailability::NotRepository;
            case LocalGitError::GitUnavailable:
                return RepoAvailability::GitUnavailable;
            case LocalGitError::InvalidWorkingDirectory:
                return RepoAvailability::UnsupportedWorkingDirectory;
            default:
                return RepoAvailability::Error;
            }
        }

        template<typename T>
        RepoSummary _toSummary(const T& entry, const bool stale)
        {
            const auto& source = entry.snapshot.status;
            RepoSummary summary;
            summary.availability = RepoAvailability::Ready;
            summary.branch = source.branch;
            summary.headOid = source.headOid;
            summary.upstream = source.upstream;
            summary.ahead = source.ahead;
            summary.behind = source.behind;
            summary.modifiedCount = source.modifiedCount;
            summary.stagedCount = source.stagedCount;
            summary.untrackedCount = source.untrackedCount;
            summary.conflictedCount = source.conflictedCount;
            summary.generation = entry.generation;
            summary.detached = source.detached;
            summary.unborn = source.unborn;
            summary.stale = stale;
            summary.filesTruncated = source.filesTruncated;
            return summary;
        }
    }

    RepoAwarenessService::RepoAwarenessService() :
        RepoAwarenessService{
            [provider = []() -> std::shared_ptr<LocalGitProvider> {
                const auto executable = GitProcessRunner::FindGitExecutable();
                return executable ? std::make_shared<LocalGitProvider>(GitProcessRunner{ *executable }) : nullptr;
            }()](const std::filesystem::path& workingDirectory, const size_t maxFiles, const std::atomic_bool* cancelled) {
                if (!provider)
                {
                    LocalGitResult result;
                    result.error = LocalGitError::GitUnavailable;
                    return result;
                }
                return provider->Refresh(workingDirectory, maxFiles, cancelled);
            }
        }
    {
    }

    RepoAwarenessService::RepoAwarenessService(RefreshFunction refresh,
                                               const std::chrono::milliseconds ttl,
                                               const std::chrono::milliseconds cacheGrace) :
        _refresh{ std::move(refresh) },
        _ttl{ ttl },
        _cacheGrace{ cacheGrace },
        _worker{ [this] { _workerLoop(); } }
    {
    }

    RepoAwarenessService::~RepoAwarenessService()
    {
        _stopping.store(true, std::memory_order_relaxed);
        _wakeWorker.notify_all();
        if (_worker.joinable())
        {
            _worker.join();
        }
    }

    RepoAwarenessService& RepoAwarenessService::Instance()
    {
        static RepoAwarenessService instance;
        return instance;
    }

    void RepoAwarenessService::ObservePane(std::string sessionId,
                                           std::filesystem::path workingDirectory,
                                           const bool workingDirectoryReportedByShell,
                                           const bool commandMarksReportedByShell,
                                           const bool commandCompleted)
    {
        if (sessionId.empty())
        {
            return;
        }

        std::lock_guard lock{ _mutex };
        auto& pane = _panes[sessionId];
        const auto ready = workingDirectoryReportedByShell && commandMarksReportedByShell;
        const auto changed = pane.workingDirectory != workingDirectory || pane.ready != ready;
        if (changed)
        {
            const auto previousWorktreeKey = pane.worktreeKey;
            pane.generation = _nextPaneGeneration++;
            pane.workingDirectory = std::move(workingDirectory);
            pane.worktreeKey.reset();
            pane.lastError = LocalGitError::None;
            pane.ready = ready;
            pane.refreshPending = false;
            _markUnreferencedLocked(previousWorktreeKey);
        }

        if (pane.ready && _consumerDemandLocked() > 0 && (changed || commandCompleted))
        {
            _enqueueRefreshLocked(sessionId, pane, true, true);
        }
        _evictUnusedLocked();
    }

    void RepoAwarenessService::RemovePane(const std::string_view sessionId)
    {
        std::lock_guard lock{ _mutex };
        const auto found = _panes.find(std::string{ sessionId });
        if (found != _panes.end())
        {
            const auto worktreeKey = found->second.worktreeKey;
            _panes.erase(found);
            _markUnreferencedLocked(worktreeKey);
            _evictUnusedLocked();
        }
    }

    RepoSummary RepoAwarenessService::GetSummary(const std::string_view sessionId, const bool forceRefresh)
    {
        std::lock_guard lock{ _mutex };
        const auto key = std::string{ sessionId };
        const auto found = _panes.find(key);
        if (found == _panes.end())
        {
            return {};
        }
        return _getSummaryLocked(key, found->second, forceRefresh);
    }

    void RepoAwarenessService::AddConsumer()
    {
        std::lock_guard lock{ _mutex };
        const auto previousDemand = _consumerDemandLocked();
        ++_inProcessConsumerCount;
        _onConsumerDemandChangedLocked(previousDemand);
    }

    void RepoAwarenessService::RemoveConsumer()
    {
        std::lock_guard lock{ _mutex };
        if (_inProcessConsumerCount > 0)
        {
            const auto previousDemand = _consumerDemandLocked();
            --_inProcessConsumerCount;
            _onConsumerDemandChangedLocked(previousDemand);
        }
    }

    void RepoAwarenessService::SetProtocolConsumerCount(const size_t count)
    {
        std::lock_guard lock{ _mutex };
        const auto previousDemand = _consumerDemandLocked();
        _protocolConsumerCount = count;
        _onConsumerDemandChangedLocked(previousDemand);
    }

    size_t RepoAwarenessService::_consumerDemandLocked() const noexcept
    {
        return _inProcessConsumerCount + _protocolConsumerCount;
    }

    void RepoAwarenessService::_onConsumerDemandChangedLocked(const size_t previousDemand)
    {
        const auto demand = _consumerDemandLocked();
        if (previousDemand > 0 && demand == 0)
        {
            _dropConsumerRequestsLocked();
        }
        else if (previousDemand == 0 && demand > 0)
        {
            for (auto& [sessionId, pane] : _panes)
            {
                if (pane.ready)
                {
                    _enqueueRefreshLocked(sessionId, pane, false, true);
                }
            }
        }
    }

    uint64_t RepoAwarenessService::SubscribeSummaryChanged(SummaryChangedCallback callback)
    {
        std::lock_guard lock{ _mutex };
        const auto token = _nextSummaryChangedToken++;
        _summaryChangedCallbacks.emplace(token, std::move(callback));
        return token;
    }

    void RepoAwarenessService::UnsubscribeSummaryChanged(const uint64_t token)
    {
        std::lock_guard lock{ _mutex };
        _summaryChangedCallbacks.erase(token);
    }

    bool RepoAwarenessService::WaitForIdle(const std::chrono::milliseconds timeout)
    {
        std::unique_lock lock{ _mutex };
        return _idle.wait_for(lock, timeout, [&] {
            return _requests.empty() && !_workerBusy;
        });
    }

    void RepoAwarenessService::_enqueueRefreshLocked(const std::string& sessionId, PaneState& pane, const bool refreshCached, const bool requiresConsumer)
    {
        if (!pane.ready || pane.workingDirectory.empty())
        {
            return;
        }
        if (pane.refreshPending)
        {
            if (!requiresConsumer)
            {
                const auto queued = std::find_if(_requests.begin(), _requests.end(), [&](const auto& request) {
                    return request.sessionId == sessionId &&
                           request.paneGeneration == pane.generation;
                });
                if (queued != _requests.end())
                {
                    queued->requiresConsumer = false;
                }
            }
            return;
        }

        if (!pane.worktreeKey)
        {
            pane.worktreeKey = _findCachedWorktreeLocked(pane.workingDirectory);
        }
        if (pane.worktreeKey)
        {
            auto& cached = _cache.at(*pane.worktreeKey);
            cached.unreferencedSince.reset();
            if (!refreshCached && std::chrono::steady_clock::now() - cached.updated < _ttl)
            {
                return;
            }
        }

        pane.refreshPending = true;
        _requests.emplace_back(RefreshRequest{
            sessionId,
            pane.workingDirectory,
            pane.generation,
            std::chrono::steady_clock::now(),
            requiresConsumer,
        });
        _wakeWorker.notify_one();
    }

    void RepoAwarenessService::_dropConsumerRequestsLocked()
    {
        std::erase_if(_requests, [&](const auto& request) {
            if (!request.requiresConsumer)
            {
                return false;
            }

            if (const auto pane = _panes.find(request.sessionId);
                pane != _panes.end() && pane->second.generation == request.paneGeneration)
            {
                pane->second.refreshPending = false;
            }
            return true;
        });
        if (_requests.empty() && !_workerBusy)
        {
            _idle.notify_all();
        }
    }

    RepoSummary RepoAwarenessService::_getSummaryLocked(const std::string& sessionId, PaneState& pane, const bool forceRefresh)
    {
        if (!pane.ready)
        {
            return {};
        }

        if (pane.worktreeKey)
        {
            const auto cached = _cache.find(*pane.worktreeKey);
            if (cached != _cache.end())
            {
                const auto stale = std::chrono::steady_clock::now() - cached->second.updated >= _ttl;
                if (forceRefresh || stale)
                {
                    _enqueueRefreshLocked(sessionId, pane, true, false);
                }
                return _toSummary(cached->second, stale);
            }
        }

        if (pane.lastError != LocalGitError::None)
        {
            RepoSummary summary;
            summary.availability = _availabilityForError(pane.lastError);
            if (forceRefresh)
            {
                _enqueueRefreshLocked(sessionId, pane, true, false);
            }
            return summary;
        }

        _enqueueRefreshLocked(sessionId, pane, true, false);
        RepoSummary summary;
        summary.availability = RepoAvailability::Loading;
        return summary;
    }

    std::optional<std::wstring> RepoAwarenessService::_findCachedWorktreeLocked(const std::filesystem::path& workingDirectory) const
    {
        const auto cwd = _worktreeKey(workingDirectory);
        if (const auto mapped = _cwdToWorktree.find(cwd); mapped != _cwdToWorktree.end())
        {
            if (_cache.contains(mapped->second))
            {
                return mapped->second;
            }
        }
        return std::nullopt;
    }

    void RepoAwarenessService::_markUnreferencedLocked(const std::optional<std::wstring>& worktreeKey)
    {
        if (!worktreeKey)
        {
            return;
        }
        const auto stillReferenced = std::any_of(_panes.begin(), _panes.end(), [&](const auto& pair) {
            return pair.second.worktreeKey == worktreeKey;
        });
        if (!stillReferenced)
        {
            if (const auto found = _cache.find(*worktreeKey); found != _cache.end())
            {
                found->second.unreferencedSince = std::chrono::steady_clock::now();
            }
        }
    }

    void RepoAwarenessService::_evictUnusedLocked()
    {
        const auto now = std::chrono::steady_clock::now();
        std::erase_if(_cache, [&](const auto& pair) {
            return pair.second.unreferencedSince &&
                   now - *pair.second.unreferencedSince >= _cacheGrace;
        });

        while (_cache.size() > 64)
        {
            auto oldest = _cache.end();
            for (auto it = _cache.begin(); it != _cache.end(); ++it)
            {
                if (it->second.unreferencedSince &&
                    (oldest == _cache.end() || it->second.updated < oldest->second.updated))
                {
                    oldest = it;
                }
            }
            if (oldest == _cache.end())
            {
                break;
            }
            _cache.erase(oldest);
        }

        std::erase_if(_cwdToWorktree, [&](const auto& pair) {
            return !_cache.contains(pair.second);
        });
    }

    void RepoAwarenessService::_workerLoop()
    {
        for (;;)
        {
            RefreshRequest request;
            std::vector<SummaryChangedCallback> cachedCallbacks;
            RepoSummary cachedSummary;
            bool satisfiedFromCache = false;
            {
                std::unique_lock lock{ _mutex };
                _wakeWorker.wait(lock, [&] {
                    return _stopping.load(std::memory_order_relaxed) || !_requests.empty();
                });
                if (_stopping.load(std::memory_order_relaxed))
                {
                    break;
                }

                request = std::move(_requests.front());
                _requests.pop_front();
                _workerBusy = true;

                const auto pane = _panes.find(request.sessionId);
                if (pane == _panes.end() ||
                    pane->second.generation != request.paneGeneration ||
                    !pane->second.ready ||
                    (request.requiresConsumer && _consumerDemandLocked() == 0))
                {
                    _workerBusy = false;
                    if (_requests.empty())
                    {
                        _idle.notify_all();
                    }
                    continue;
                }

                if (const auto cachedKey = _findCachedWorktreeLocked(request.workingDirectory))
                {
                    auto& cached = _cache.at(*cachedKey);
                    if (cached.updated >= request.queuedAt)
                    {
                        const auto previousWorktreeKey = pane->second.worktreeKey;
                        pane->second.worktreeKey = cachedKey;
                        pane->second.refreshPending = false;
                        pane->second.lastError = LocalGitError::None;
                        cached.unreferencedSince.reset();
                        if (previousWorktreeKey != pane->second.worktreeKey)
                        {
                            _markUnreferencedLocked(previousWorktreeKey);
                        }
                        cachedSummary = _toSummary(cached, false);
                        cachedCallbacks.reserve(_summaryChangedCallbacks.size());
                        for (const auto& [_, callback] : _summaryChangedCallbacks)
                        {
                            cachedCallbacks.emplace_back(callback);
                        }
                        satisfiedFromCache = true;
                        _workerBusy = false;
                        if (_requests.empty())
                        {
                            _idle.notify_all();
                        }
                    }
                }
            }

            if (satisfiedFromCache)
            {
                for (const auto& callback : cachedCallbacks)
                {
                    try
                    {
                        callback(request.sessionId, cachedSummary);
                    }
                    catch (...)
                    {
                        LOG_CAUGHT_EXCEPTION();
                    }
                }
                continue;
            }

            auto result = _refresh(request.workingDirectory, 500, &_stopping);
            std::vector<SummaryChangedCallback> callbacks;
            RepoSummary summary;
            bool notify = false;
            {
                std::lock_guard lock{ _mutex };
                const auto found = _panes.find(request.sessionId);
                if (found != _panes.end() &&
                    found->second.generation == request.paneGeneration &&
                    found->second.ready)
                {
                    auto& pane = found->second;
                    pane.refreshPending = false;
                    pane.lastError = result.error;
                    if (result)
                    {
                        const auto key = _worktreeKey(result.snapshot.worktreeRoot);
                        const auto previousWorktreeKey = pane.worktreeKey;
                        _cwdToWorktree[_worktreeKey(request.workingDirectory)] = key;
                        auto& entry = _cache[key];
                        entry.snapshot = std::move(result.snapshot);
                        entry.updated = std::chrono::steady_clock::now();
                        entry.generation = _nextSnapshotGeneration++;
                        entry.unreferencedSince.reset();
                        pane.worktreeKey = key;
                        pane.lastError = LocalGitError::None;
                        if (previousWorktreeKey != pane.worktreeKey)
                        {
                            _markUnreferencedLocked(previousWorktreeKey);
                        }
                        summary = _toSummary(entry, false);
                    }
                    else
                    {
                        summary.availability = _availabilityForError(pane.lastError);
                    }
                    callbacks.reserve(_summaryChangedCallbacks.size());
                    for (const auto& [_, callback] : _summaryChangedCallbacks)
                    {
                        callbacks.emplace_back(callback);
                    }
                    notify = true;
                }

                _workerBusy = false;
                if (_requests.empty())
                {
                    _idle.notify_all();
                }
                _evictUnusedLocked();
            }

            if (notify)
            {
                for (const auto& callback : callbacks)
                {
                    try
                    {
                        callback(request.sessionId, summary);
                    }
                    catch (...)
                    {
                        LOG_CAUGHT_EXCEPTION();
                    }
                }
            }
        }
    }
}
