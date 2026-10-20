// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "BuiltInProviderCatalog.h"
#include "CommandRunner.h"

#include <windows.h>

#include <array>
#include <atomic>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <string_view>
#include <type_traits>

namespace TerminalAppUnitTests
{
    class RichTabProviderTests;
}

namespace Microsoft::Terminal::RichTab::Provider
{
    enum class ProviderParseOutcome : uint8_t
    {
        NotAttempted,
        Valid,
        Invalid,
    };

    enum class ProviderBrokerDisposition : uint8_t
    {
        NotApplicable,
        SessionMissing,
        ProviderStateMissing,
        RunningGenerationMismatch,
        GenerationMismatch,
        ContextRevisionMismatch,
        Accepted,
    };

    enum class ProviderDiagnosticEvent : uint8_t
    {
        Catalog,
        Invocation,
    };

    enum ProviderFieldMask : uint8_t
    {
        ProviderFieldNone = 0,
        ProviderFieldAgentStatus = 1 << 0,
        ProviderFieldWorkingDirectory = 1 << 1,
        ProviderFieldRepository = 1 << 2,
        ProviderFieldBranch = 1 << 3,
        ProviderFieldChanges = 1 << 4,
    };

    struct ProviderDiagnosticVersion
    {
        uint16_t major{ 0 };
        uint16_t minor{ 0 };
        uint16_t patch{ 0 };
        uint16_t revision{ 0 };
        uint8_t componentCount{ 0 };
        bool valid{ false };
    };

    struct ProviderDiagnosticRecord
    {
        ProviderDiagnosticEvent event{ ProviderDiagnosticEvent::Invocation };
        uint64_t timestamp{ 0 };
        uint64_t processEpoch{ 0 };
        uint64_t hostRequestId{ 0 };
        uint64_t generation{ 0 };
        uint64_t contextRevision{ 0 };
        uint64_t durationMicroseconds{ 0 };
        uint32_t processId{ 0 };
        ProviderCatalogOutcome catalogOutcome{ ProviderCatalogOutcome::Unknown };
        ProviderDiagnosticVersion providerVersion;
        ActivationEvent activationReason{ ActivationEvent::ManualRefresh };
        CommandResult::Status runnerStatus{ CommandResult::Status::LaunchFailed };
        ProviderParseOutcome parseOutcome{ ProviderParseOutcome::NotAttempted };
        ProviderBrokerDisposition brokerDisposition{ ProviderBrokerDisposition::NotApplicable };
        uint32_t exitCode{ 0 };
        uint32_t win32Error{ 0 };
        uint8_t selectedFields{ ProviderFieldNone };
        uint8_t returnedFields{ ProviderFieldNone };
        bool catalogOrigin{ false };
        bool runnerAttempted{ false };
        bool processStarted{ false };
        bool exitCodeValid{ false };
        bool win32ErrorValid{ false };
        bool outputLimitObserved{ false };
        bool standardOutputExceeded{ false };
        bool standardErrorExceeded{ false };
    };
    static_assert(std::is_nothrow_copy_assignable_v<ProviderDiagnosticRecord>);

    ProviderDiagnosticVersion ParseProviderDiagnosticVersion(std::string_view value) noexcept;
    uint64_t ProviderDiagnosticTimestamp() noexcept;

    class ProviderDiagnosticLog
    {
    public:
        static ProviderDiagnosticLog* Create(uint64_t processEpoch) noexcept;
        static std::unique_ptr<ProviderDiagnosticLog> CreateForTests(
            uint64_t processEpoch,
            std::filesystem::path directory,
            bool dropOnContention = false) noexcept;

        ~ProviderDiagnosticLog();

        ProviderDiagnosticLog(const ProviderDiagnosticLog&) = delete;
        ProviderDiagnosticLog& operator=(const ProviderDiagnosticLog&) = delete;

        void TryLog(const ProviderDiagnosticRecord& record) noexcept;
        bool Enabled() const noexcept;
        uint64_t DroppedRecords() const noexcept;

    private:
        friend class ::TerminalAppUnitTests::RichTabProviderTests;

        static constexpr size_t QueueCapacity{ 256 };
        static constexpr uint64_t MaximumFileSize{ 1024 * 1024 };

        ProviderDiagnosticLog(
            uint64_t processEpoch,
            std::filesystem::path directory,
            bool dropOnContention) noexcept;

        bool _Start() noexcept;
        static DWORD WINAPI _ThreadEntry(void* context) noexcept;
        void _Run() noexcept;
        bool _OpenFile() noexcept;
        bool _Write(const ProviderDiagnosticRecord& record) noexcept;
        void _Disable() noexcept;
        void _CleanupOldFiles() noexcept;

        const uint64_t _processEpoch;
        const std::filesystem::path _directory;
        const bool _dropOnContention;
        SRWLOCK _queueLock = SRWLOCK_INIT;
        CONDITION_VARIABLE _queueCondition = CONDITION_VARIABLE_INIT;
        std::array<ProviderDiagnosticRecord, QueueCapacity> _queue{};
        size_t _queueHead{ 0 };
        size_t _queueTail{ 0 };
        size_t _queueSize{ 0 };
        bool _stopping{ false };
        std::atomic<bool> _enabled{ false };
        std::atomic<uint64_t> _droppedRecords{ 0 };
        HANDLE _thread{ nullptr };
        HANDLE _file{ INVALID_HANDLE_VALUE };
        uint64_t _bytesWritten{ 0 };
    };
}
