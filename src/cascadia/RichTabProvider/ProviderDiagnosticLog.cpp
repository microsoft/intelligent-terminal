// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "ProviderDiagnosticLog.h"

#include <IntelligentTerminalPaths.h>
#include <wil/resource.h>

#include <charconv>
#include <cstdio>
#include <limits>
#include <new>
#include <string>

namespace Microsoft::Terminal::RichTab::Provider
{
    namespace
    {
        const char* _CatalogOutcome(const ProviderCatalogOutcome value) noexcept
        {
            switch (value)
            {
            case ProviderCatalogOutcome::InvalidPackageRoot:
                return "invalid-package-root";
            case ProviderCatalogOutcome::CatalogAbsent:
                return "catalog-absent";
            case ProviderCatalogOutcome::CatalogUnreadable:
                return "catalog-unreadable";
            case ProviderCatalogOutcome::ManifestUnreadable:
                return "manifest-unreadable";
            case ProviderCatalogOutcome::ManifestInvalid:
                return "manifest-invalid";
            case ProviderCatalogOutcome::ProviderIdMismatch:
                return "provider-id-mismatch";
            case ProviderCatalogOutcome::ProviderRegistered:
                return "provider-registered";
            default:
                return "unknown";
            }
        }

        const char* _ActivationReason(const ActivationEvent value) noexcept
        {
            switch (value)
            {
            case ActivationEvent::PaneConnected:
                return "pane-connected";
            case ActivationEvent::WorkingDirectoryChanged:
                return "working-directory-changed";
            case ActivationEvent::CommandFinished:
                return "command-finished";
            case ActivationEvent::TabActivated:
                return "tab-activated";
            case ActivationEvent::ManualRefresh:
            default:
                return "manual-refresh";
            }
        }

        const char* _RunnerStatus(const CommandResult::Status value) noexcept
        {
            switch (value)
            {
            case CommandResult::Status::Completed:
                return "completed";
            case CommandResult::Status::TimedOut:
                return "timed-out";
            case CommandResult::Status::OutputLimitExceeded:
                return "output-limit-exceeded";
            case CommandResult::Status::LaunchFailed:
            default:
                return "launch-failed";
            }
        }

        const char* _ParseOutcome(const ProviderParseOutcome value) noexcept
        {
            switch (value)
            {
            case ProviderParseOutcome::Valid:
                return "valid";
            case ProviderParseOutcome::Invalid:
                return "invalid";
            case ProviderParseOutcome::NotAttempted:
            default:
                return "not-attempted";
            }
        }

        const char* _BrokerDisposition(const ProviderBrokerDisposition value) noexcept
        {
            switch (value)
            {
            case ProviderBrokerDisposition::SessionMissing:
                return "session-missing";
            case ProviderBrokerDisposition::ProviderStateMissing:
                return "provider-state-missing";
            case ProviderBrokerDisposition::RunningGenerationMismatch:
                return "running-generation-mismatch";
            case ProviderBrokerDisposition::GenerationMismatch:
                return "generation-mismatch";
            case ProviderBrokerDisposition::ContextRevisionMismatch:
                return "context-revision-mismatch";
            case ProviderBrokerDisposition::Accepted:
                return "accepted";
            case ProviderBrokerDisposition::NotApplicable:
            default:
                return "not-applicable";
            }
        }

        const char* _Boolean(const bool value) noexcept
        {
            return value ? "true" : "false";
        }

        std::string _FieldArray(const uint8_t fields)
        {
            std::string result{ "[" };
            const auto append = [&](const uint8_t mask, const std::string_view name) {
                if ((fields & mask) == 0)
                {
                    return;
                }
                if (result.size() > 1)
                {
                    result.push_back(',');
                }
                result.push_back('"');
                result.append(name);
                result.push_back('"');
            };
            append(ProviderFieldAgentStatus, "agentStatus");
            append(ProviderFieldWorkingDirectory, "workingDirectory");
            append(ProviderFieldRepository, "repository");
            append(ProviderFieldBranch, "branch");
            append(ProviderFieldChanges, "changes");
            result.push_back(']');
            return result;
        }

        std::string _Version(const ProviderDiagnosticVersion& version)
        {
            if (!version.valid)
            {
                return "null";
            }
            std::array<char, 32> buffer{};
            int count = -1;
            switch (version.componentCount)
            {
            case 1:
                count = _snprintf_s(buffer.data(), buffer.size(), _TRUNCATE, "\"%u\"", version.major);
                break;
            case 2:
                count = _snprintf_s(buffer.data(), buffer.size(), _TRUNCATE, "\"%u.%u\"", version.major, version.minor);
                break;
            case 3:
                count = _snprintf_s(buffer.data(), buffer.size(), _TRUNCATE, "\"%u.%u.%u\"", version.major, version.minor, version.patch);
                break;
            case 4:
                count = _snprintf_s(buffer.data(), buffer.size(), _TRUNCATE, "\"%u.%u.%u.%u\"", version.major, version.minor, version.patch, version.revision);
                break;
            default:
                return "null";
            }
            return count > 0 ? std::string{ buffer.data(), static_cast<size_t>(count) } : "null";
        }

        std::string _Timestamp(const uint64_t timestamp)
        {
            ULARGE_INTEGER value{};
            value.QuadPart = timestamp;
            FILETIME fileTime{};
            fileTime.dwLowDateTime = value.LowPart;
            fileTime.dwHighDateTime = value.HighPart;
            SYSTEMTIME systemTime{};
            if (!FileTimeToSystemTime(&fileTime, &systemTime))
            {
                return {};
            }
            std::array<char, 32> buffer{};
            const auto count = _snprintf_s(
                buffer.data(),
                buffer.size(),
                _TRUNCATE,
                "%04u-%02u-%02uT%02u:%02u:%02u.%03uZ",
                systemTime.wYear,
                systemTime.wMonth,
                systemTime.wDay,
                systemTime.wHour,
                systemTime.wMinute,
                systemTime.wSecond,
                systemTime.wMilliseconds);
            return count > 0 ? std::string{ buffer.data(), static_cast<size_t>(count) } : std::string{};
        }
    }

    ProviderDiagnosticVersion ParseProviderDiagnosticVersion(const std::string_view value) noexcept
    {
        ProviderDiagnosticVersion result;
        if (value.empty() || value.back() == '.')
        {
            return result;
        }

        std::array<uint16_t, 4> components{};
        size_t component = 0;
        size_t start = 0;
        while (start < value.size() && component < components.size())
        {
            const auto end = value.find('.', start);
            const auto part = value.substr(start, end == std::string_view::npos ? value.size() - start : end - start);
            if (part.empty())
            {
                return {};
            }
            uint32_t parsed = 0;
            const auto converted = std::from_chars(part.data(), part.data() + part.size(), parsed);
            if (converted.ec != std::errc{} ||
                converted.ptr != part.data() + part.size() ||
                parsed > (std::numeric_limits<uint16_t>::max)())
            {
                return {};
            }
            components[component++] = static_cast<uint16_t>(parsed);
            if (end == std::string_view::npos)
            {
                start = value.size();
                break;
            }
            start = end + 1;
        }
        if (start < value.size() || component == 0)
        {
            return {};
        }

        result.major = components[0];
        result.minor = components[1];
        result.patch = components[2];
        result.revision = components[3];
        result.componentCount = static_cast<uint8_t>(component);
        result.valid = true;
        return result;
    }

    uint64_t ProviderDiagnosticTimestamp() noexcept
    {
        FILETIME time{};
        GetSystemTimeAsFileTime(&time);
        ULARGE_INTEGER value{};
        value.LowPart = time.dwLowDateTime;
        value.HighPart = time.dwHighDateTime;
        return value.QuadPart;
    }

    ProviderDiagnosticLog* ProviderDiagnosticLog::Create(const uint64_t processEpoch) noexcept
    {
        try
        {
            auto logger = std::unique_ptr<ProviderDiagnosticLog>{
                new (std::nothrow) ProviderDiagnosticLog{ processEpoch, ::IntelligentTerminal::LogDirVersioned(), true }
            };
            if (!logger)
            {
                return nullptr;
            }

            HMODULE module = nullptr;
            if (!GetModuleHandleExW(
                    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
                    reinterpret_cast<LPCWSTR>(&ProviderDiagnosticTimestamp),
                    &module))
            {
                return nullptr;
            }
            if (!logger->_Start())
            {
                FreeLibrary(module);
                return nullptr;
            }
            // Production diagnostics are process-lifetime and intentionally do
            // not add shutdown flushing or thread joins to the application path.
            // The retained module reference keeps the worker's code loaded until
            // process termination and can be rolled back if startup fails.
            return logger.release();
        }
        catch (...)
        {
            return nullptr;
        }
    }

    std::unique_ptr<ProviderDiagnosticLog> ProviderDiagnosticLog::CreateForTests(
        const uint64_t processEpoch,
        std::filesystem::path directory,
        const bool dropOnContention) noexcept
    {
        try
        {
            auto logger = std::unique_ptr<ProviderDiagnosticLog>{
                new (std::nothrow) ProviderDiagnosticLog{ processEpoch, std::move(directory), dropOnContention }
            };
            if (!logger || !logger->_Start())
            {
                return nullptr;
            }
            return logger;
        }
        catch (...)
        {
            return nullptr;
        }
    }

    ProviderDiagnosticLog::ProviderDiagnosticLog(
        const uint64_t processEpoch,
        std::filesystem::path directory,
        const bool dropOnContention) noexcept :
        _processEpoch{ processEpoch },
        _directory{ std::move(directory) },
        _dropOnContention{ dropOnContention }
    {
    }

    ProviderDiagnosticLog::~ProviderDiagnosticLog()
    {
        if (_thread)
        {
            AcquireSRWLockExclusive(&_queueLock);
            _stopping = true;
            ReleaseSRWLockExclusive(&_queueLock);
            WakeAllConditionVariable(&_queueCondition);
            WaitForSingleObject(_thread, INFINITE);
            CloseHandle(_thread);
        }
        if (_file != INVALID_HANDLE_VALUE)
        {
            CloseHandle(_file);
        }
    }

    bool ProviderDiagnosticLog::_Start() noexcept
    {
        if (_directory.empty())
        {
            return false;
        }
        _thread = CreateThread(nullptr, 0, &_ThreadEntry, this, 0, nullptr);
        if (!_thread)
        {
            return false;
        }
        SetThreadPriority(_thread, THREAD_PRIORITY_BELOW_NORMAL);
        _enabled.store(true, std::memory_order_release);
        return true;
    }

    void ProviderDiagnosticLog::TryLog(const ProviderDiagnosticRecord& record) noexcept
    {
        if (!_enabled.load(std::memory_order_acquire))
        {
            return;
        }
        if (_dropOnContention)
        {
            if (!TryAcquireSRWLockExclusive(&_queueLock))
            {
                _droppedRecords.fetch_add(1, std::memory_order_relaxed);
                return;
            }
        }
        else
        {
            AcquireSRWLockExclusive(&_queueLock);
        }

        if (_stopping || _queueSize == QueueCapacity)
        {
            ReleaseSRWLockExclusive(&_queueLock);
            _droppedRecords.fetch_add(1, std::memory_order_relaxed);
            return;
        }

        _queue[_queueTail] = record;
        _queueTail = (_queueTail + 1) % QueueCapacity;
        ++_queueSize;
        ReleaseSRWLockExclusive(&_queueLock);
        WakeConditionVariable(&_queueCondition);
    }

    uint64_t ProviderDiagnosticLog::DroppedRecords() const noexcept
    {
        return _droppedRecords.load(std::memory_order_relaxed);
    }

    bool ProviderDiagnosticLog::Enabled() const noexcept
    {
        return _enabled.load(std::memory_order_acquire);
    }

    DWORD WINAPI ProviderDiagnosticLog::_ThreadEntry(void* context) noexcept
    {
        static_cast<ProviderDiagnosticLog*>(context)->_Run();
        return 0;
    }

    void ProviderDiagnosticLog::_Run() noexcept
    {
        try
        {
            _CleanupOldFiles();
            for (;;)
            {
                ProviderDiagnosticRecord record;
                AcquireSRWLockExclusive(&_queueLock);
                while (_queueSize == 0 && !_stopping)
                {
                    SleepConditionVariableSRW(&_queueCondition, &_queueLock, INFINITE, 0);
                }
                if (_queueSize == 0 && _stopping)
                {
                    ReleaseSRWLockExclusive(&_queueLock);
                    return;
                }
                record = _queue[_queueHead];
                _queueHead = (_queueHead + 1) % QueueCapacity;
                --_queueSize;
                ReleaseSRWLockExclusive(&_queueLock);

                if (!_Write(record))
                {
                    _Disable();
                    return;
                }
            }
        }
        catch (...)
        {
            _Disable();
        }
    }

    bool ProviderDiagnosticLog::_OpenFile() noexcept
    {
        try
        {
            std::error_code error;
            std::filesystem::create_directories(_directory, error);
            if (error)
            {
                return false;
            }

            SYSTEMTIME now{};
            GetSystemTime(&now);
            std::array<wchar_t, 160> name{};
            const auto count = _snwprintf_s(
                name.data(),
                name.size(),
                _TRUNCATE,
                L"rich-tab-provider-%04u%02u%02u-%lu-%016llx.jsonl",
                now.wYear,
                now.wMonth,
                now.wDay,
                GetCurrentProcessId(),
                _processEpoch);
            if (count <= 0)
            {
                return false;
            }

            const auto path = _directory / name.data();
            _file = CreateFileW(
                path.c_str(),
                FILE_APPEND_DATA,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                nullptr,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                nullptr);
            return _file != INVALID_HANDLE_VALUE;
        }
        catch (...)
        {
            return false;
        }
    }

    bool ProviderDiagnosticLog::_Write(const ProviderDiagnosticRecord& record) noexcept
    {
        if (_file == INVALID_HANDLE_VALUE && !_OpenFile())
        {
            return false;
        }

        try
        {
            const auto selectedFields = _FieldArray(record.selectedFields);
            const auto returnedFields = _FieldArray(record.returnedFields);
            const auto version = _Version(record.providerVersion);
            const auto timestamp = _Timestamp(record.timestamp);
            if (timestamp.empty())
            {
                return false;
            }
            std::array<char, 1024> line{};
            const int count = record.event == ProviderDiagnosticEvent::Catalog ?
                                  _snprintf_s(
                                      line.data(),
                                      line.size(),
                                      _TRUNCATE,
                                      "{\"schema\":1,\"event\":\"catalog\",\"timestamp\":\"%s\",\"pid\":%lu,\"processEpoch\":%llu,\"provider\":\"git-status\",\"catalogOrigin\":%s,\"providerVersion\":%s,\"outcome\":\"%s\"}\n",
                                      timestamp.c_str(),
                                      record.processId,
                                      record.processEpoch,
                                      _Boolean(record.catalogOrigin),
                                      version.c_str(),
                                      _CatalogOutcome(record.catalogOutcome)) :
                                  _snprintf_s(
                                      line.data(),
                                      line.size(),
                                      _TRUNCATE,
                                      "{\"schema\":1,\"event\":\"invocation\",\"timestamp\":\"%s\",\"pid\":%lu,\"processEpoch\":%llu,\"request\":%llu,\"provider\":\"git-status\",\"catalogOrigin\":%s,\"providerVersion\":%s,\"activation\":\"%s\",\"generation\":%llu,\"contextRevision\":%llu,\"selectedFields\":%s,\"returnedFields\":%s,\"runnerAttempted\":%s,\"processStarted\":%s,\"runnerStatus\":\"%s\",\"durationUs\":%llu,\"exitCodeValid\":%s,\"exitCode\":%lu,\"win32ErrorValid\":%s,\"win32Error\":%lu,\"outputLimitObserved\":%s,\"stdoutExceeded\":%s,\"stderrExceeded\":%s,\"parse\":\"%s\",\"brokerDisposition\":\"%s\"}\n",
                                      timestamp.c_str(),
                                      record.processId,
                                      record.processEpoch,
                                      record.hostRequestId,
                                      _Boolean(record.catalogOrigin),
                                      version.c_str(),
                                      _ActivationReason(record.activationReason),
                                      record.generation,
                                      record.contextRevision,
                                      selectedFields.c_str(),
                                      returnedFields.c_str(),
                                      _Boolean(record.runnerAttempted),
                                      _Boolean(record.processStarted),
                                      _RunnerStatus(record.runnerStatus),
                                      record.durationMicroseconds,
                                      _Boolean(record.exitCodeValid),
                                      record.exitCode,
                                      _Boolean(record.win32ErrorValid),
                                      record.win32Error,
                                      _Boolean(record.outputLimitObserved),
                                      _Boolean(record.standardOutputExceeded),
                                      _Boolean(record.standardErrorExceeded),
                                      _ParseOutcome(record.parseOutcome),
                                      _BrokerDisposition(record.brokerDisposition));
            if (count <= 0)
            {
                return false;
            }
            const auto bytes = static_cast<uint64_t>(count);
            if (_bytesWritten + bytes > MaximumFileSize)
            {
                return false;
            }
            DWORD written = 0;
            if (!WriteFile(_file, line.data(), static_cast<DWORD>(count), &written, nullptr) ||
                written != static_cast<DWORD>(count))
            {
                return false;
            }
            _bytesWritten += bytes;
            return true;
        }
        catch (...)
        {
            return false;
        }
    }

    void ProviderDiagnosticLog::_Disable() noexcept
    {
        _enabled.store(false, std::memory_order_release);
        AcquireSRWLockExclusive(&_queueLock);
        _queueSize = 0;
        _queueHead = 0;
        _queueTail = 0;
        _stopping = true;
        ReleaseSRWLockExclusive(&_queueLock);
        WakeAllConditionVariable(&_queueCondition);
    }

    void ProviderDiagnosticLog::_CleanupOldFiles() noexcept
    {
        try
        {
            const auto pattern = _directory / L"rich-tab-provider-*.jsonl";
            WIN32_FIND_DATAW data{};
            const wil::unique_hfind search{ FindFirstFileW(pattern.c_str(), &data) };
            if (!search)
            {
                return;
            }
            size_t removed = 0;
            const auto cutoff = ProviderDiagnosticTimestamp() - 7ull * 24 * 60 * 60 * 10'000'000;
            do
            {
                if ((data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) != 0)
                {
                    continue;
                }
                ULARGE_INTEGER writeTime{};
                writeTime.LowPart = data.ftLastWriteTime.dwLowDateTime;
                writeTime.HighPart = data.ftLastWriteTime.dwHighDateTime;
                if (writeTime.QuadPart >= cutoff)
                {
                    continue;
                }

                const auto path = _directory / data.cFileName;
                if (DeleteFileW(path.c_str()) && ++removed >= 64)
                {
                    break;
                }
            }
            while (FindNextFileW(search.get(), &data));
        }
        catch (...)
        {
        }
    }
}
