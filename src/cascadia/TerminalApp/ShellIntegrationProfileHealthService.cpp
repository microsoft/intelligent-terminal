// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "ShellIntegrationProfileHealthService.h"

#include <limits>

#include "../inc/ShellIntegrationCommon.h"

using namespace Microsoft::Terminal::ShellIntegration;
using namespace Microsoft::Terminal::ShellIntegration::Health;

namespace
{
    constexpr uint64_t MaximumProfileSize{ 1024 * 1024 };
    constexpr auto RetryCooldown{ std::chrono::seconds{ 30 } };

    uint64_t _HashBytes(const std::string_view bytes) noexcept
    {
        uint64_t hash{ 14695981039346656037ull };
        for (const auto byte : bytes)
        {
            hash ^= static_cast<unsigned char>(byte);
            hash *= 1099511628211ull;
        }
        return hash;
    }

    uint64_t _FileTimeToUInt64(const FILETIME& value) noexcept
    {
        ULARGE_INTEGER integer{};
        integer.LowPart = value.dwLowDateTime;
        integer.HighPart = value.dwHighDateTime;
        return integer.QuadPart;
    }

    struct Snapshot
    {
        ProfileFingerprint fingerprint;
        std::string contents;
        Reason failure{ Reason::None };
    };

    struct MetadataSnapshot
    {
        ProfileFingerprint fingerprint;
        bool exists{ false };
        Reason failure{ Reason::None };
    };

    void _PopulateMetadata(
        ProfileFingerprint& fingerprint,
        const BY_HANDLE_FILE_INFORMATION& information,
        const uint64_t mutationEpoch) noexcept
    {
        ULARGE_INTEGER size{};
        size.LowPart = information.nFileSizeLow;
        size.HighPart = information.nFileSizeHigh;
        fingerprint.size = size.QuadPart;
        fingerprint.lastWriteTime = _FileTimeToUInt64(information.ftLastWriteTime);
        fingerprint.volumeSerialNumber = information.dwVolumeSerialNumber;
        fingerprint.fileIndex =
            (static_cast<uint64_t>(information.nFileIndexHigh) << 32) |
            information.nFileIndexLow;
        fingerprint.mutationEpoch = mutationEpoch;
        fingerprint.exists = true;
    }

    MetadataSnapshot _ReadMetadata(const TargetKey& target)
    {
        MetadataSnapshot metadata;
        const std::filesystem::path path{ target.profilePath };
        const auto mutationEpoch = details::ProfileMutationEpoch(path);
        const auto epochBefore = mutationEpoch->load(std::memory_order_acquire);
        if ((epochBefore & 1u) != 0)
        {
            metadata.failure = Reason::ChangedDuringAnalysis;
            return metadata;
        }

        wil::unique_hfile file{ CreateFileW(
            path.c_str(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            nullptr) };
        if (!file)
        {
            const auto error = GetLastError();
            if (error != ERROR_FILE_NOT_FOUND && error != ERROR_PATH_NOT_FOUND)
            {
                metadata.failure = Reason::ReadFailed;
                return metadata;
            }

            const auto epochAfter = mutationEpoch->load(std::memory_order_acquire);
            if (epochBefore != epochAfter || (epochAfter & 1u) != 0)
            {
                metadata.failure = Reason::ChangedDuringAnalysis;
                return metadata;
            }
            metadata.fingerprint.mutationEpoch = epochAfter;
            return metadata;
        }

        BY_HANDLE_FILE_INFORMATION information{};
        if (!GetFileInformationByHandle(file.get(), &information))
        {
            metadata.failure = Reason::ReadFailed;
            return metadata;
        }

        const auto epochAfter = mutationEpoch->load(std::memory_order_acquire);
        if (epochBefore != epochAfter || (epochAfter & 1u) != 0)
        {
            metadata.failure = Reason::ChangedDuringAnalysis;
            return metadata;
        }

        metadata.exists = true;
        _PopulateMetadata(metadata.fingerprint, information, epochAfter);
        return metadata;
    }

    bool _MetadataMatches(
        const MetadataSnapshot& metadata,
        const Result& cached) noexcept
    {
        if (metadata.failure != Reason::None)
        {
            return false;
        }
        if (!metadata.exists)
        {
            return !cached.fingerprint.exists &&
                   cached.fingerprint.mutationEpoch == metadata.fingerprint.mutationEpoch;
        }

        return cached.fingerprint.exists &&
               cached.fingerprint.size == metadata.fingerprint.size &&
               cached.fingerprint.lastWriteTime == metadata.fingerprint.lastWriteTime &&
               cached.fingerprint.volumeSerialNumber == metadata.fingerprint.volumeSerialNumber &&
               cached.fingerprint.fileIndex == metadata.fingerprint.fileIndex &&
               cached.fingerprint.mutationEpoch == metadata.fingerprint.mutationEpoch;
    }

    bool _MetadataMatches(
        const MetadataSnapshot& metadata,
        const ProfileFingerprint& fingerprint) noexcept
    {
        return metadata.failure == Reason::None &&
               metadata.exists &&
               fingerprint.size == metadata.fingerprint.size &&
               fingerprint.lastWriteTime == metadata.fingerprint.lastWriteTime &&
               fingerprint.volumeSerialNumber == metadata.fingerprint.volumeSerialNumber &&
               fingerprint.fileIndex == metadata.fingerprint.fileIndex &&
               fingerprint.mutationEpoch == metadata.fingerprint.mutationEpoch;
    }

    Snapshot _ReadSnapshot(const TargetKey& target)
    {
        Snapshot snapshot;
        const std::filesystem::path path{ target.profilePath };
        const auto mutationEpoch = details::ProfileMutationEpoch(path);
        const auto epochBefore = mutationEpoch->load(std::memory_order_acquire);
        if ((epochBefore & 1u) != 0)
        {
            snapshot.failure = Reason::ChangedDuringAnalysis;
            return snapshot;
        }
        snapshot.fingerprint.mutationEpoch = epochBefore;

        wil::unique_hfile file{ CreateFileW(
            path.c_str(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            nullptr,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN,
            nullptr) };
        if (!file)
        {
            const auto error = GetLastError();
            snapshot.failure = error == ERROR_FILE_NOT_FOUND || error == ERROR_PATH_NOT_FOUND ?
                                   Reason::MissingBlock :
                                   Reason::ReadFailed;
            return snapshot;
        }

        BY_HANDLE_FILE_INFORMATION before{};
        if (!GetFileInformationByHandle(file.get(), &before))
        {
            snapshot.failure = Reason::ReadFailed;
            return snapshot;
        }

        ULARGE_INTEGER size{};
        size.LowPart = before.nFileSizeLow;
        size.HighPart = before.nFileSizeHigh;
        _PopulateMetadata(snapshot.fingerprint, before, epochBefore);
        if (size.QuadPart > MaximumProfileSize ||
            size.QuadPart > static_cast<uint64_t>(std::numeric_limits<DWORD>::max()))
        {
            snapshot.failure = Reason::FileTooLarge;
            return snapshot;
        }

        snapshot.contents.resize(static_cast<size_t>(size.QuadPart));
        size_t offset = 0;
        while (offset < snapshot.contents.size())
        {
            DWORD read = 0;
            const auto remaining = snapshot.contents.size() - offset;
            const auto request = static_cast<DWORD>((std::min)(
                remaining,
                static_cast<size_t>(64 * 1024)));
            if (!ReadFile(
                    file.get(),
                    snapshot.contents.data() + offset,
                    request,
                    &read,
                    nullptr) ||
                read == 0)
            {
                snapshot.failure = Reason::ReadFailed;
                return snapshot;
            }
            offset += read;
        }

        BY_HANDLE_FILE_INFORMATION after{};
        if (!GetFileInformationByHandle(file.get(), &after))
        {
            snapshot.failure = Reason::ReadFailed;
            return snapshot;
        }

        const auto epochAfter = mutationEpoch->load(std::memory_order_acquire);
        if (epochBefore != epochAfter ||
            (epochAfter & 1u) != 0 ||
            before.dwVolumeSerialNumber != after.dwVolumeSerialNumber ||
            before.nFileIndexHigh != after.nFileIndexHigh ||
            before.nFileIndexLow != after.nFileIndexLow ||
            before.nFileSizeHigh != after.nFileSizeHigh ||
            before.nFileSizeLow != after.nFileSizeLow ||
            _FileTimeToUInt64(before.ftLastWriteTime) != _FileTimeToUInt64(after.ftLastWriteTime))
        {
            snapshot.failure = Reason::ChangedDuringAnalysis;
            snapshot.contents.clear();
            return snapshot;
        }

        _PopulateMetadata(snapshot.fingerprint, after, epochAfter);
        snapshot.fingerprint.contentHash = _HashBytes(snapshot.contents);
        return snapshot;
    }
}

namespace winrt::TerminalApp::implementation
{
    ShellIntegrationProfileHealthService& ShellIntegrationProfileHealthService::Instance()
    {
        static ShellIntegrationProfileHealthService instance;
        return instance;
    }

    size_t ShellIntegrationProfileHealthService::TargetKeyHash::operator()(const TargetKey& key) const noexcept
    {
        size_t hash = std::hash<uint8_t>{}(static_cast<uint8_t>(key.shell));
        const auto combine = [&](const auto& value) {
            hash ^= std::hash<std::decay_t<decltype(value)>>{}(value) +
                    0x9e3779b9 + (hash << 6) + (hash >> 2);
        };
        combine(static_cast<uint8_t>(key.syntax));
        combine(key.profilePath);
        combine(key.hostPath);
        combine(key.shellIdentity);
        return hash;
    }

    uint64_t ShellIntegrationProfileHealthService::SetEnabled(const bool enabled) noexcept
    {
        std::lock_guard lock{ _mutex };
        if (_enabled != enabled)
        {
            _enabled = enabled;
            ++_generation;
            _entries.clear();
        }
        return _generation;
    }

    uint64_t ShellIntegrationProfileHealthService::Generation() const noexcept
    {
        std::lock_guard lock{ _mutex };
        return _generation;
    }

    bool ShellIntegrationProfileHealthService::Enabled() const noexcept
    {
        std::lock_guard lock{ _mutex };
        return _enabled;
    }

    void ShellIntegrationProfileHealthService::Request(
        TargetKey target,
        const uint64_t generation,
        Analyzer analyzer,
        Callback callback,
        const bool force)
    {
        std::optional<Result> cachedCandidate;
        bool start = false;
        bool invalidatedCache = false;
        uint64_t runToken = 0;
        {
            std::lock_guard lock{ _mutex };
            if (!_enabled || generation != _generation)
            {
                return;
            }

            auto& entry = _entries[target];
            if (entry.result)
            {
                const auto epoch = ::Microsoft::Terminal::ShellIntegration::details::ProfileMutationEpoch(target.profilePath)
                                       ->load(std::memory_order_acquire);
                if ((epoch & 1u) != 0 || epoch != entry.result->fingerprint.mutationEpoch)
                {
                    entry.result.reset();
                    entry.lastAttempt = {};
                    if ((epoch & 1u) != 0)
                    {
                        return;
                    }
                    invalidatedCache = true;
                }
            }
            if (force && entry.inFlight)
            {
                entry.rerunAnalyzer = std::move(analyzer);
                entry.rerunCallbacks.emplace_back(PendingCallback{ generation, std::move(callback) });
                return;
            }
            if (force)
            {
                entry.result.reset();
            }
            if (entry.result)
            {
                cachedCandidate = entry.result;
                entry.callbacks.emplace_back(PendingCallback{ generation, std::move(callback) });
                if (!entry.inFlight)
                {
                    entry.inFlight = true;
                    entry.lastAttempt = std::chrono::steady_clock::now();
                    entry.activeRunToken = ++_nextRunToken;
                    runToken = entry.activeRunToken;
                    start = true;
                }
            }
            else if (!force &&
                     !invalidatedCache &&
                     !entry.inFlight &&
                     entry.lastAttempt != std::chrono::steady_clock::time_point{} &&
                     std::chrono::steady_clock::now() - entry.lastAttempt < RetryCooldown)
            {
                return;
            }
            else
            {
                entry.callbacks.emplace_back(PendingCallback{ generation, std::move(callback) });
                if (!entry.inFlight)
                {
                    entry.inFlight = true;
                    entry.lastAttempt = std::chrono::steady_clock::now();
                    entry.activeRunToken = ++_nextRunToken;
                    runToken = entry.activeRunToken;
                    start = true;
                }
            }
        }

        if (start)
        {
            _Run(
                std::move(target),
                generation,
                runToken,
                std::move(analyzer),
                std::move(cachedCandidate));
        }
    }

    safe_void_coroutine ShellIntegrationProfileHealthService::_Run(
        TargetKey target,
        const uint64_t generation,
        const uint64_t runToken,
        Analyzer analyzer,
        std::optional<Result> cachedCandidate)
    {
        co_await winrt::resume_background();

        Result result;
        if (cachedCandidate && _MetadataMatches(_ReadMetadata(target), *cachedCandidate))
        {
            result = std::move(*cachedCandidate);
        }
        else
        {
            result.target = target;
            const auto snapshot = _ReadSnapshot(target);
            result.fingerprint = snapshot.fingerprint;
            if (snapshot.failure != Reason::None)
            {
                result.analysis = snapshot.failure == Reason::MissingBlock ?
                                      AnalysisResult{ Status::NotInstalled, Reason::MissingBlock } :
                                      AnalysisResult{ Status::Indeterminate, snapshot.failure };
            }
            else
            {
                result.analysis = analyzer(target, snapshot.contents);
                if (!_MetadataMatches(_ReadMetadata(target), snapshot.fingerprint))
                {
                    result.analysis = { Status::Indeterminate, Reason::ChangedDuringAnalysis };
                }
            }
        }

        std::vector<Callback> callbacks;
        std::optional<Analyzer> rerunAnalyzer;
        uint64_t rerunToken = 0;
        {
            std::lock_guard lock{ _mutex };
            const auto found = _entries.find(target);
            if (found == _entries.end() || found->second.activeRunToken != runToken)
            {
                co_return;
            }

            auto& entry = found->second;
            if (!_enabled || generation != _generation)
            {
                co_return;
            }

            if (!result.analysis.IsRetryable())
            {
                entry.result = result;
            }
            else
            {
                entry.result.reset();
                if (result.analysis.reason == Reason::ChangedDuringAnalysis)
                {
                    entry.lastAttempt = {};
                }
            }
            for (auto& pending : entry.callbacks)
            {
                if (pending.generation == generation && pending.callback)
                {
                    callbacks.emplace_back(std::move(pending.callback));
                }
            }
            entry.callbacks.clear();

            if (entry.rerunAnalyzer)
            {
                rerunAnalyzer = std::move(entry.rerunAnalyzer);
                entry.callbacks = std::move(entry.rerunCallbacks);
                entry.result.reset();
                entry.lastAttempt = std::chrono::steady_clock::now();
                entry.activeRunToken = ++_nextRunToken;
                rerunToken = entry.activeRunToken;
            }
            else
            {
                entry.inFlight = false;
            }
        }

        for (const auto& callback : callbacks)
        {
            callback(result);
        }

        if (rerunAnalyzer)
        {
            _Run(
                std::move(target),
                generation,
                rerunToken,
                std::move(*rerunAnalyzer),
                std::nullopt);
        }
    }
}
