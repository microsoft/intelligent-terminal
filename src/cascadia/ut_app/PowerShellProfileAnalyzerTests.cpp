// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <array>
#include <optional>

#include "../TerminalApp/PowerShellProfileAnalyzer.h"
#include "../inc/PowerShellShellIntegration.h"

using namespace Microsoft::Terminal::ShellIntegration;
using namespace Microsoft::Terminal::ShellIntegration::Health;
using namespace Microsoft::Terminal::ShellIntegration::Powershell::ProfileAnalyzer;
using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class PowerShellProfileAnalyzerTests
    {
        TEST_CLASS(PowerShellProfileAnalyzerTests);

        TEST_METHOD(RecognizesCanonicalManagedBlock);
        TEST_METHOD(RecognizesCanonicalPwshBlockWhenAvailable);
        TEST_METHOD(RecognizesCanonicalBlockFromAnotherVersion);
        TEST_METHOD(RecognizesRootStatementAfterBlock);
        TEST_METHOD(IgnoresMarkerTextInString);
        TEST_METHOD(RejectsDuplicateMarkers);
        TEST_METHOD(RejectsBlockNestedInFunction);
        TEST_METHOD(RejectsBlockNestedInScriptBlock);
        TEST_METHOD(WindowsPowerShellAcceptsNoBomAnsi);
        TEST_METHOD(PreservesUtf16ByteOffsets);
        TEST_METHOD(RejectsRelativeHostPath);
    };

    namespace
    {
        std::wstring SystemPowerShell()
        {
            std::array<wchar_t, MAX_PATH> systemDirectory{};
            const auto length = GetSystemDirectoryW(systemDirectory.data(), gsl::narrow_cast<UINT>(systemDirectory.size()));
            VERIFY_IS_TRUE(length != 0 && length < systemDirectory.size());
            return std::wstring{ systemDirectory.data(), length } + L"\\WindowsPowerShell\\v1.0\\powershell.exe";
        }

        std::optional<std::wstring> PowerShell7()
        {
            std::array<wchar_t, MAX_PATH> programFiles{};
            const auto length = GetEnvironmentVariableW(
                L"ProgramFiles",
                programFiles.data(),
                gsl::narrow_cast<DWORD>(programFiles.size()));
            if (length == 0 || length >= programFiles.size())
            {
                return std::nullopt;
            }
            std::wstring path{ programFiles.data(), length };
            path += L"\\PowerShell\\7\\pwsh.exe";
            const auto attributes = GetFileAttributesW(path.c_str());
            if (attributes == INVALID_FILE_ATTRIBUTES || (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0)
            {
                return std::nullopt;
            }
            return path;
        }

        void VerifyStatus(const AnalysisResult& actual, const Status status, const Reason reason)
        {
            VERIFY_ARE_EQUAL(status, actual.status);
            VERIFY_ARE_EQUAL(reason, actual.reason);
        }

        std::string Utf16LeWithBom(const std::string_view text)
        {
            const auto utf16 = til::u8u16(text);
            std::string bytes{ "\xff\xfe", 2 };
            bytes.append(reinterpret_cast<const char*>(utf16.data()), utf16.size() * sizeof(wchar_t));
            return bytes;
        }

        std::string Ansi(const std::wstring_view text)
        {
            const auto size = WideCharToMultiByte(
                CP_ACP,
                0,
                text.data(),
                gsl::narrow_cast<int>(text.size()),
                nullptr,
                0,
                nullptr,
                nullptr);
            VERIFY_IS_TRUE(size > 0);
            std::string bytes(static_cast<size_t>(size), '\0');
            VERIFY_ARE_EQUAL(
                size,
                WideCharToMultiByte(
                    CP_ACP,
                    0,
                    text.data(),
                    gsl::narrow_cast<int>(text.size()),
                    bytes.data(),
                    size,
                    nullptr,
                    nullptr));
            return bytes;
        }
    }

    void PowerShellProfileAnalyzerTests::RecognizesCanonicalManagedBlock()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");
        const std::string profile = "# user comment\n" + block + "\n# another comment\n";

        const auto actual = Analyze(SystemPowerShell(), profile);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 15 }, actual.blockStart);
        VERIFY_ARE_EQUAL(actual.blockStart + block.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::RecognizesCanonicalPwshBlockWhenAvailable()
    {
        const auto host = PowerShell7();
        if (!host)
        {
            return;
        }
        const auto block = Powershell::BuildBlock(L"PowerShell", "\n");

        const auto actual = Analyze(*host, block);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 0 }, actual.blockStart);
        VERIFY_ARE_EQUAL(block.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::RecognizesCanonicalBlockFromAnotherVersion()
    {
        auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");
        const auto currentFileName = til::u16u8(Powershell::ScriptFileName());
        const auto offset = block.find(currentFileName);
        VERIFY_ARE_NOT_EQUAL(std::string::npos, offset);
        block.replace(offset, currentFileName.size(), "shell-integration_v8.ps1");

        const auto actual = Analyze(SystemPowerShell(), block + "\nGet-Date\n");

        VerifyStatus(actual, Status::BlockNotLast, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 0 }, actual.blockStart);
        VERIFY_ARE_EQUAL(block.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::RecognizesRootStatementAfterBlock()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\r\n");

        const auto actual = Analyze(SystemPowerShell(), block + "\r\n$env:TERM = 'xterm-256color'\r\n");

        VerifyStatus(actual, Status::BlockNotLast, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 0 }, actual.blockStart);
        VERIFY_ARE_EQUAL(block.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::IgnoresMarkerTextInString()
    {
        const auto actual = Analyze(
            SystemPowerShell(),
            "$marker = '# >>> intelligent-terminal shell-integration >>>'\n");

        VerifyStatus(actual, Status::NotInstalled, Reason::MissingBlock);
    }

    void PowerShellProfileAnalyzerTests::RejectsDuplicateMarkers()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");

        const auto actual = Analyze(SystemPowerShell(), block + "\n" + block);

        VerifyStatus(actual, Status::Indeterminate, Reason::MalformedBlock);
    }

    void PowerShellProfileAnalyzerTests::RejectsBlockNestedInFunction()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");
        const std::string profile = "function Nested {\n" + block + "\n}\n";

        const auto actual = Analyze(SystemPowerShell(), profile);

        VerifyStatus(actual, Status::Indeterminate, Reason::MalformedBlock);
    }

    void PowerShellProfileAnalyzerTests::RejectsBlockNestedInScriptBlock()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");
        const std::string profile = "& {\n" + block + "\n}\n";

        const auto actual = Analyze(SystemPowerShell(), profile);

        VerifyStatus(actual, Status::Indeterminate, Reason::MalformedBlock);
    }

    void PowerShellProfileAnalyzerTests::WindowsPowerShellAcceptsNoBomAnsi()
    {
        const auto prefix = Ansi(L"# caf\u00e9\r\n");
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\r\n");

        const auto actual = Analyze(SystemPowerShell(), prefix + block);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(prefix.size(), actual.blockStart);
        VERIFY_ARE_EQUAL(prefix.size() + block.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::PreservesUtf16ByteOffsets()
    {
        const auto block = Powershell::BuildBlock(L"WindowsPowerShell", "\n");
        const auto bytes = Utf16LeWithBom(block);

        const auto actual = Analyze(SystemPowerShell(), bytes);

        VerifyStatus(actual, Status::Healthy, Reason::None);
        VERIFY_ARE_EQUAL(size_t{ 2 }, actual.blockStart);
        VERIFY_ARE_EQUAL(bytes.size(), actual.blockEnd);
    }

    void PowerShellProfileAnalyzerTests::RejectsRelativeHostPath()
    {
        const auto actual = Analyze(L"powershell.exe", {});

        VerifyStatus(actual, Status::Indeterminate, Reason::UnsupportedHost);
    }
}
