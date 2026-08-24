// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "CommandRunner.h"

#include <chrono>
#include <condition_variable>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>

namespace Microsoft::Terminal::RichTab::Provider
{
    struct PersistentProviderKey
    {
        std::string providerId;
        std::string sessionId;

        bool operator==(const PersistentProviderKey&) const = default;
    };

    class IPersistentProviderProcess
    {
    public:
        virtual ~IPersistentProviderProcess() = default;

        virtual bool Send(std::string frame) = 0;
        virtual bool IsRunning() const noexcept = 0;
        virtual void Stop(std::string stopFrame, std::chrono::milliseconds timeout) noexcept = 0;
        virtual std::string StandardError() const = 0;
    };

    struct PersistentProviderLaunchResult
    {
        std::shared_ptr<IPersistentProviderProcess> process;
        uint32_t win32Error{ 0 };
    };

    class IPersistentProviderProcessFactory
    {
    public:
        virtual ~IPersistentProviderProcessFactory() = default;

        virtual PersistentProviderLaunchResult Launch(
            const Manifest& manifest,
            const CommandRunner::Environment& environment) = 0;
    };

    enum class PersistentProviderEventKind
    {
        Started,
        LaunchFailed,
        ProcessExited,
        ControlWriteFailed,
        LeaseDropped,
        StopFrameFailed,
        Stopped,
        RestartRequested,
    };

    struct PersistentProviderEvent
    {
        PersistentProviderEventKind kind{ PersistentProviderEventKind::LaunchFailed };
        PersistentProviderKey key;
        uint64_t instanceGeneration{ 0 };
        uint64_t requestGeneration{ 0 };
        std::chrono::milliseconds retryAfter{ 0 };
        uint32_t win32Error{ 0 };
        std::string standardError;
    };

    class PersistentProviderSupervisor
    {
    public:
        using FrameFactory = std::function<std::optional<std::string>(
            uint64_t instanceGeneration,
            bool started)>;
        using EventCallback = std::function<void(const PersistentProviderEvent&)>;

        explicit PersistentProviderSupervisor(
            std::shared_ptr<IPersistentProviderProcessFactory> processFactory = {});
        ~PersistentProviderSupervisor();

        PersistentProviderSupervisor(const PersistentProviderSupervisor&) = delete;
        PersistentProviderSupervisor& operator=(const PersistentProviderSupervisor&) = delete;

        void SetEventCallback(EventCallback callback);
        void StartOrRefresh(
            PersistentProviderKey key,
            Manifest manifest,
            CommandRunner::Environment environment,
            FrameFactory frameFactory,
            uint64_t requestGeneration,
            bool forceRestart,
            bool resetBackoff);
        void SendLease(
            PersistentProviderKey key,
            uint64_t instanceGeneration,
            uint64_t requestGeneration,
            std::string frame);
        void Stop(PersistentProviderKey key);
        void StopProvider(std::string providerId);
        void StopSession(std::string sessionId);
        void Shutdown() noexcept;

        static std::chrono::milliseconds BackoffForFailure(size_t consecutiveFailures) noexcept;

    private:
        struct KeyHash
        {
            size_t operator()(const PersistentProviderKey& key) const noexcept;
        };

        struct Instance
        {
            Manifest manifest;
            CommandRunner::Environment environment;
            std::shared_ptr<IPersistentProviderProcess> process;
            uint64_t instanceGeneration{ 0 };
            uint64_t requestGeneration{ 0 };
            bool controlStarted{ false };
            size_t consecutiveFailures{ 0 };
            std::chrono::steady_clock::time_point startedAt{};
            std::optional<std::chrono::steady_clock::time_point> restartAt;
        };

        void _Queue(std::function<void()> action);
        void _Worker();
        void _StartOrRefresh(
            const PersistentProviderKey& key,
            Manifest manifest,
            CommandRunner::Environment environment,
            FrameFactory frameFactory,
            uint64_t requestGeneration,
            bool forceRestart,
            bool resetBackoff);
        void _SendLease(
            const PersistentProviderKey& key,
            uint64_t instanceGeneration,
            uint64_t requestGeneration,
            std::string frame);
        void _StopMatching(const std::function<bool(const PersistentProviderKey&)>& predicate);
        void _StopInstance(
            const PersistentProviderKey& key,
            Instance& instance) noexcept;
        void _ScheduleRestart(
            const PersistentProviderKey& key,
            Instance& instance,
            PersistentProviderEventKind eventKind,
            uint32_t win32Error,
            std::string standardError);
        void _CheckProcesses();
        void _Emit(PersistentProviderEvent event);

        std::shared_ptr<IPersistentProviderProcessFactory> _processFactory;
        std::mutex _queueMutex;
        std::condition_variable _queueCondition;
        std::deque<std::function<void()>> _actions;
        bool _stopping{ false };
        std::thread _worker;

        std::mutex _callbackMutex;
        EventCallback _eventCallback;

        std::unordered_map<PersistentProviderKey, Instance, KeyHash> _instances;
        uint64_t _nextInstanceGeneration{ 1 };
    };
}
