// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <windows.h>
#include <objbase.h>
#include <algorithm>
#include <filesystem>
#include <optional>
#include <string>
#include <utility>
#include <vector>
#include <wil/resource.h>

#include "IntelligentTerminalPaths.h"

namespace Microsoft::Terminal::WtaProcess
{
    // Optional parent observation. A provider requests this before its runtime
    // starts; other captured commands never join this job or retire resources.
    class ProcessResourceScope
    {
    public:
        ProcessResourceScope() = default;
        ProcessResourceScope(ProcessResourceScope&&) noexcept = default;
        ProcessResourceScope& operator=(ProcessResourceScope&&) noexcept = default;
        ProcessResourceScope(const ProcessResourceScope&) = delete;
        ProcessResourceScope& operator=(const ProcessResourceScope&) = delete;

        wil::unique_handle CreateJob() noexcept
        try
        {
            try
            {
                GUID guid{};
                THROW_IF_FAILED(CoCreateGuid(&guid));
                wchar_t text[39]{};
                THROW_HR_IF(E_UNEXPECTED, StringFromGUID2(guid, text, ARRAYSIZE(text)) == 0);
                for (const auto ch : std::wstring_view{ text })
                {
                    if (ch != L'{' && ch != L'}' && ch != L'-')
                    {
                        _id.push_back(static_cast<wchar_t>(ch >= L'A' && ch <= L'F' ? ch + (L'a' - L'A') : ch));
                    }
                }
                _used.reset(CreateEventW(nullptr, TRUE, FALSE, _Name(L"used").c_str()));
                THROW_LAST_ERROR_IF(!_used);
                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS), GetLastError() == ERROR_ALREADY_EXISTS);
                _ready.reset(CreateEventW(nullptr, TRUE, FALSE, _Name(L"ready").c_str()));
                THROW_LAST_ERROR_IF(!_ready);
                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS), GetLastError() == ERROR_ALREADY_EXISTS);
                _available = true;
            }
            CATCH_LOG();

            auto job = wil::unique_handle{ CreateJobObjectW(nullptr, _available ? _Name(L"job").c_str() : nullptr) };
            if (!job || (_available && GetLastError() == ERROR_ALREADY_EXISTS))
            {
                LOG_LAST_ERROR();
                _available = false;
                job.reset(CreateJobObjectW(nullptr, nullptr));
            }
            if (job)
            {
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if (!SetInformationJobObject(job.get(), JobObjectExtendedLimitInformation, &limits, sizeof(limits)))
                {
                    LOG_LAST_ERROR();
                    _available = false;
                    job.reset();
                }
            }
            return job;
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            _available = false;
            return {};
        }

        std::wstring Environment(const wchar_t* original)
        {
            auto inherited = wil::unique_environstrings_ptr{ original ? nullptr : GetEnvironmentStringsW() };
            const auto input = original ? original : inherited.get();
            THROW_LAST_ERROR_IF(!input);
            std::vector<std::wstring> entries;
            for (auto entry = input; *entry; entry += wcslen(entry) + 1)
            {
                if (_wcsnicmp(entry, L"LOCALAPPDATA=", 13) == 0)
                {
                    _local = entry + 13;
                }
                if (_wcsnicmp(entry, L"WTA_PROCESS_RESOURCE_SCOPE=", 27) != 0)
                {
                    entries.emplace_back(entry);
                }
            }
            entries.emplace_back(L"WTA_PROCESS_RESOURCE_SCOPE=" + (_available ? _id : L"unavailable"));
            std::sort(entries.begin(), entries.end(), [](const auto& left, const auto& right) {
                return CompareStringOrdinal(left.c_str(), -1, right.c_str(), -1, TRUE) == CSTR_LESS_THAN;
            });
            std::wstring result;
            for (const auto& entry : entries)
            {
                result.append(entry);
                result.push_back(L'\0');
            }
            result.push_back(L'\0');
            return result;
        }

        void Attached() noexcept
        {
            if (_available)
            {
                _attached = true;
                LOG_IF_WIN32_BOOL_FALSE(SetEvent(_ready.get()));
            }
        }

        bool Requested() const noexcept
        {
            if (!_available || !_used)
            {
                return false;
            }
            const auto state = WaitForSingleObject(_used.get(), 0);
            LOG_LAST_ERROR_IF(state == WAIT_FAILED);
            return state == WAIT_OBJECT_0;
        }

        void RequestShutdown(const HANDLE shutdown, const HANDLE process, const DWORD timeoutMs) const noexcept
        {
            if (Requested() && shutdown && process)
            {
                LOG_IF_WIN32_BOOL_FALSE(SetEvent(shutdown));
                const auto result = WaitForSingleObject(process, timeoutMs);
                LOG_HR_IF(HRESULT_FROM_WIN32(ERROR_TIMEOUT), result == WAIT_TIMEOUT);
                LOG_LAST_ERROR_IF(result == WAIT_FAILED);
            }
        }

        void AttachRequested(const HANDLE process, const HANDLE job) noexcept
        {
            if (!_attachmentAttempted && !_attached && Requested())
            {
                _attachmentAttempted = true;
                if (AssignProcessToJobObject(job, process))
                {
                    Attached();
                }
                else
                {
                    LOG_LAST_ERROR();
                }
            }
        }

        bool Retire(const HANDLE job, const DWORD timeoutMs = 5000) noexcept
        try
        {
            // A requester sets "used" before waiting for "ready". Closing that
            // gate first covers admitted requests and blocks a late first one.
            if (_ready)
            {
                THROW_IF_WIN32_BOOL_FALSE(ResetEvent(_ready.get()));
            }
            if (!_attached || !Requested())
            {
                return false;
            }
            THROW_IF_WIN32_BOOL_FALSE(TerminateJobObject(job, 1));
            const auto start = GetTickCount64();
            for (;;)
            {
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{};
                THROW_IF_WIN32_BOOL_FALSE(QueryInformationJobObject(
                    job, JobObjectBasicAccountingInformation, &accounting, sizeof(accounting), nullptr));
                if (accounting.ActiveProcesses == 0)
                {
                    break;
                }
                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_TIMEOUT), GetTickCount64() - start >= timeoutMs);
                Sleep(25);
            }
            _RecordRetirement();
            return true;
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION();
            return false;
        }

    private:
        std::wstring _Name(const wchar_t* kind) const
        {
            return std::wstring{ L"Local\\wta-resources-" } + kind + L"-" + _id;
        }

        void _RecordRetirement() const
        {
            const auto root = IntelligentTerminal::AcpRuntimeRoot(_local);
            std::vector<wil::unique_hfile> pins;
            auto current = root.root_path();
            const auto pin = [&](const std::filesystem::path& path) {
                wil::unique_hfile directory{ CreateFileW(path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT, nullptr) };
                THROW_LAST_ERROR_IF(!directory);
                BY_HANDLE_FILE_INFORMATION info{};
                THROW_IF_WIN32_BOOL_FALSE(GetFileInformationByHandle(directory.get(), &info));
                THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_INVALID_DATA),
                            !(info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) ||
                                (info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT));
                pins.emplace_back(std::move(directory));
            };
            pin(current);
            for (const auto& component : root.relative_path())
            {
                current /= component;
                pin(current);
            }
            const auto path = root / (L".retired-" + _id);
            wil::unique_hfile receipt{ CreateFileW(path.c_str(), GENERIC_WRITE, FILE_SHARE_READ, nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr) };
            THROW_LAST_ERROR_IF(!receipt);
            THROW_IF_WIN32_BOOL_FALSE(FlushFileBuffers(receipt.get()));
        }

        std::wstring _id;
        std::filesystem::path _local;
        wil::unique_handle _used;
        wil::unique_handle _ready;
        bool _available{ false };
        bool _attached{ false };
        bool _attachmentAttempted{ false };
    };
}
