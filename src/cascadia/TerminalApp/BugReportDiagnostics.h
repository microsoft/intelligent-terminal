// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "../inc/DiagnosticsIdentity.h"
#include <fstream>

namespace IntelligentTerminal::Diagnostics
{
    struct ProcessSnapshot
    {
        Json::Value owner;
        wil::unique_handle process;
    };

    struct Report
    {
        Json::Value value;
        std::vector<ProcessSnapshot> processes;
    };

    // Takes ownership of pinned process handles; never touches UI objects.
    void CollectBackground(Report& report);

    class ReportFile
    {
    public:
        explicit ReportFile(const std::filesystem::path& root)
        {
            GUID guid{};
            THROW_IF_FAILED(CoCreateGuid(&guid));
            wchar_t text[40]{};
            THROW_HR_IF(E_FAIL, !StringFromGUID2(guid, text, ARRAYSIZE(text)));
            _id = text;
            _directory = root / (L"bug-report-" + _id);
            THROW_HR_IF(HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS), !std::filesystem::create_directory(_directory));
        }
        ~ReportFile()
        {
            // Never recursively delete a shared tree or another report.
            std::error_code error;
            std::filesystem::remove(_directory / L"diagnostics.json", error);
            std::filesystem::remove(_directory, error);
        }
        ReportFile(const ReportFile&) = delete;
        ReportFile& operator=(const ReportFile&) = delete;

        void Write(const Json::Value& report) const
        {
            std::ofstream file{ _directory / L"diagnostics.json", std::ios::binary | std::ios::trunc };
            Json::StreamWriterBuilder writer;
            writer["indentation"] = "  ";
            file << Json::writeString(writer, report) << '\n';
            file.close();
            THROW_HR_IF(E_FAIL, !file);
        }
        const std::filesystem::path& Directory() const noexcept { return _directory; }
        const std::wstring& Id() const noexcept { return _id; }

    private:
        std::filesystem::path _directory;
        std::wstring _id;
    };

    enum class ArchiveResult
    {
        Success,
        StartFailed,
        Failed,
        TimedOut
    };

    inline ArchiveResult ArchiveReportFile(const std::filesystem::path& tar,
                                           const std::filesystem::path& zip,
                                           const std::filesystem::path& logs,
                                           const ReportFile& report)
    {
        auto command = L"\"" + tar.wstring() + L"\" -a -c -f \"" + zip.wstring() + L"\"";
        if (!logs.empty())
            command += L" -C \"" + logs.parent_path().wstring() + L"\" logs";
        command += L" -C \"" + report.Directory().wstring() + L"\" diagnostics.json";
        STARTUPINFOW startup{};
        startup.cb = sizeof(startup);
        startup.dwFlags = STARTF_USESHOWWINDOW;
        startup.wShowWindow = SW_HIDE;
        PROCESS_INFORMATION info{};
        if (!CreateProcessW(tar.c_str(), command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW, nullptr, nullptr, &startup, &info))
            return ArchiveResult::StartFailed;
        wil::unique_handle process{ info.hProcess };
        wil::unique_handle thread{ info.hThread };
        if (WaitForSingleObject(process.get(), 60000) != WAIT_OBJECT_0)
        {
            TerminateProcess(process.get(), 1);
            WaitForSingleObject(process.get(), 5000);
            return ArchiveResult::TimedOut;
        }
        DWORD exitCode{};
        return GetExitCodeProcess(process.get(), &exitCode) && exitCode == 0 ? ArchiveResult::Success : ArchiveResult::Failed;
    }
}
