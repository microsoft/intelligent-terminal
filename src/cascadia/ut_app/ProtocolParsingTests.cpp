// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalProtocol/ProtocolParsing.h"
#include "../inc/DiagnosticsMetadata.h"
#include "../inc/DiagnosticProcessHandle.h"
#include "../TerminalApp/BugReportDiagnostics.h"
#include <fstream>
#include <thread>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Protocol::Parsing;

namespace TerminalAppUnitTests
{
    class ProtocolParsingTests
    {
        TEST_CLASS(ProtocolParsingTests);

        TEST_METHOD(DefaultPasteRequestUsesDirectRoute);
        TEST_METHOD(AgentAvailabilityUsesDirectRoute);
        TEST_METHOD(AgentSessionsRetiredUsesDirectRoute);
        TEST_METHOD(RestartRequestIdentityIsStampedOnce);
        TEST_METHOD(BoundedCommandPreservesUtf8Characters);
        TEST_METHOD(BoundedBufferTailAppliesLineAndCharacterLimits);
        TEST_METHOD(BoundedBufferTailPreservesBlankLines);
        TEST_METHOD(CapabilitySupportDistinguishesUnsupportedFromMalformed);
        TEST_METHOD(DiagnosticIdentityIsOptionalAndAllowlisted);
        TEST_METHOD(DiagnosticSettingsExcludePrivateValues);
        TEST_METHOD(DiagnosticErrorsAreBoundedAndCounted);
        TEST_METHOD(DiagnosticReportFilesAreIsolatedAndCleaned);
        TEST_METHOD(DiagnosticArchiveKeepsMetadataSeparateFromLogs);
        TEST_METHOD(DiagnosticProcessHandleSurvivesExitAndOwnerReset);
        TEST_METHOD(DiagnosticProcessHandleAcquisitionIsSynchronized);
    };

    void ProtocolParsingTests::DiagnosticProcessHandleSurvivesExitAndOwnerReset()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        wil::unique_handle reportHandle;
        Json::Value expected;
        {
            D::DiagnosticProcessHandle pin;
            VERIFY_IS_FALSE(static_cast<bool>(pin.Duplicate()));
            wchar_t system[MAX_PATH]{};
            VERIFY_IS_TRUE(GetSystemDirectoryW(system, ARRAYSIZE(system)) != 0);
            const auto executable = std::filesystem::path{ system } / L"cmd.exe";
            auto command = L"\"" + executable.wstring() + L"\" /d /c exit 0";
            STARTUPINFOW startup{ sizeof(startup) };
            wil::unique_process_information child;
            VERIFY_IS_TRUE(CreateProcessW(executable.c_str(), command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW, nullptr, nullptr, &startup, &child));
            pin.Capture(child.hProcess);
            expected = D::ProcessIdentity(child.hProcess);
            VERIFY_ARE_EQUAL("available", expected["status"].asString());
            VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(child.hProcess, 10000));
            child.reset();
            // A recycled original HANDLE must not affect the separate pin.
            wil::unique_event unrelated{ CreateEventW(nullptr, TRUE, FALSE, nullptr) };
            reportHandle = pin.Duplicate();
            VERIFY_IS_TRUE(static_cast<bool>(reportHandle));
            pin.Capture(nullptr);
            VERIFY_IS_FALSE(static_cast<bool>(pin.Duplicate()));
        }
        const auto actual = D::ProcessIdentity(reportHandle.get());
        VERIFY_ARE_EQUAL(expected["instance_id"].asString(), actual["instance_id"].asString());
        VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_OBJECT_0), WaitForSingleObject(reportHandle.get(), 0));
    }

    void ProtocolParsingTests::DiagnosticProcessHandleAcquisitionIsSynchronized()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        D::DiagnosticProcessHandle pin;
        const auto expected = D::ProcessIdentity(GetCurrentProcess())["instance_id"].asString();
        pin.Capture(GetCurrentProcess());
        std::thread replace{ [&] {
            for (size_t i = 0; i < 1000; ++i)
            {
                pin.Capture(GetCurrentProcess());
            }
        } };
        const auto join = wil::scope_exit([&] { replace.join(); });
        for (size_t i = 0; i < 1000; ++i)
        {
            const auto process = pin.Duplicate();
            VERIFY_IS_TRUE(static_cast<bool>(process));
            VERIFY_ARE_EQUAL(expected, D::ProcessIdentity(process.get())["instance_id"].asString());
            VERIFY_ARE_EQUAL(static_cast<DWORD>(WAIT_TIMEOUT), WaitForSingleObject(process.get(), 0));
        }
    }

    void ProtocolParsingTests::DiagnosticIdentityIsOptionalAndAllowlisted()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        VERIFY_ARE_EQUAL("unavailable", D::ServerIdentity({})["status"].asString());
        for (const auto* payload : { "true", "[]", R"({"pid":"PRIVATE","start_time_filetime":"123"})", R"({"pid":1,"start_time_filetime":"18446744073709551616"})", R"({"pid":0,"start_time_filetime":"123"})" })
        {
            Json::Value input;
            VERIFY_IS_TRUE(ParseJson(payload, input));
            VERIFY_ARE_EQUAL("invalid", D::ServerIdentity(input)["status"].asString());
        }
        Json::Value input;
        VERIFY_IS_TRUE(ParseJson(R"({"pid":123,"start_time_filetime":"134000000000000000","instance_id":"PRIVATE","package_full_name":"PRIVATE\\PATH","product_version":"PRIVATE","command":"PRIVATE","environment":"PRIVATE","com_clsid":"PRIVATE","package_version":"1.2.3.4"})", input));
        const auto safe = D::ServerIdentity(input);
        VERIFY_ARE_EQUAL("available", safe["status"].asString());
        VERIFY_ARE_EQUAL("123-134000000000000000", safe["instance_id"].asString());
        VERIFY_ARE_EQUAL("1.2.3.4", safe["package_version"].asString());
        VERIFY_IS_TRUE(safe.toStyledString().find("PRIVATE") == std::string::npos);
        VERIFY_IS_TRUE(D::IsPackageIdentity("Microsoft.WindowsTerminal_1.2.3.4_x64__8wekyb3d8bbwe"));
        VERIFY_IS_FALSE(D::IsPackageIdentity("PRIVATE_TOKEN"));
        VERIFY_IS_FALSE(D::IsGuid("PRIVATE_GUID"));
        VERIFY_IS_TRUE(D::IsGuid("{12345678-1234-1234-1234-123456789abc}"));
    }

    void ProtocolParsingTests::DiagnosticSettingsExcludePrivateValues()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        VERIFY_ARE_EQUAL("custom", D::Provider(L"custom:PRIVATE_TOKEN"));
        VERIFY_ARE_EQUAL("copilot", D::Provider(L"copilot"));
        for (const auto* value : { L"PRIVATE_TOKEN", L"C:\\Users\\PRIVATE\\agent.exe", L"https://PRIVATE", L"agent[secret=PRIVATE]=trace" })
        {
            VERIFY_ARE_EQUAL("unavailable", D::Provider(value));
            VERIFY_ARE_EQUAL("unavailable", D::Enum(value, { L"auto", L"ask" }));
        }
        VERIFY_ARE_EQUAL("auto", D::Enum(L"auto", { L"auto", L"ask" }));
    }

    void ProtocolParsingTests::DiagnosticErrorsAreBoundedAndCounted()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        Json::Value report;
        for (unsigned i = 0; i < 100; ++i)
            D::Error(report, "binary_hash", "unavailable");
        VERIFY_ARE_EQUAL(Json::UInt64{ 100 }, report["collection_error_count"].asUInt64());
        VERIFY_ARE_EQUAL(D::ErrorLimit, report["collection_errors"].size());
        VERIFY_ARE_EQUAL(2u, report["collection_errors"][0].size());
    }

    void ProtocolParsingTests::DiagnosticReportFilesAreIsolatedAndCleaned()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        std::filesystem::path firstPath;
        std::filesystem::path secondPath;
        {
            D::ReportFile first{ std::filesystem::current_path() };
            D::ReportFile second{ std::filesystem::current_path() };
            firstPath = first.Directory();
            secondPath = second.Directory();
            VERIFY_IS_TRUE(firstPath != secondPath);
            Json::Value report;
            report["schema_version"] = 1;
            report["effective_provider"] = D::Provider(L"custom:PRIVATE_TOKEN");
            D::Error(report, "ui_snapshot", "unavailable");
            first.Write(report);
            second.Write(report);
            std::ifstream file{ firstPath / L"diagnostics.json", std::ios::binary };
            const std::string text{ std::istreambuf_iterator<char>{ file }, std::istreambuf_iterator<char>{} };
            Json::Value parsed;
            VERIFY_IS_TRUE(ParseJson(text, parsed));
            VERIFY_ARE_EQUAL(1, parsed["schema_version"].asInt());
            VERIFY_ARE_EQUAL(Json::UInt64{ 1 }, parsed["collection_error_count"].asUInt64());
            VERIFY_IS_TRUE(text.find("PRIVATE") == std::string::npos);
            VERIFY_IS_TRUE(std::filesystem::exists(secondPath / L"diagnostics.json"));
        }
        VERIFY_IS_FALSE(std::filesystem::exists(firstPath));
        VERIFY_IS_FALSE(std::filesystem::exists(secondPath));
    }

    void ProtocolParsingTests::DiagnosticArchiveKeepsMetadataSeparateFromLogs()
    {
        namespace D = IntelligentTerminal::Diagnostics;
        D::ReportFile report{ std::filesystem::current_path() };
        const auto logs = report.Directory() / L"logs";
        const auto zip = report.Directory() / L"report.zip";
        const auto metadataOnly = report.Directory() / L"metadata.zip";
        const auto cleanup = wil::scope_exit([&] {
            std::error_code error;
            std::filesystem::remove(logs / L"sample.log", error);
            std::filesystem::remove(logs, error);
            std::filesystem::remove(zip, error);
            std::filesystem::remove(metadataOnly, error);
        });
        VERIFY_IS_TRUE(std::filesystem::create_directory(logs));
        {
            std::ofstream log{ logs / L"sample.log" };
            log << "sample log\n";
        }
        Json::Value metadata;
        metadata["schema_version"] = 1;
        report.Write(metadata);
        wchar_t system[MAX_PATH]{};
        VERIFY_IS_TRUE(GetSystemDirectoryW(system, ARRAYSIZE(system)) != 0);
        const auto tar = std::filesystem::path{ system } / L"tar.exe";
        VERIFY_ARE_EQUAL(D::ArchiveResult::Success, D::ArchiveReportFile(tar, zip, logs, report));
        const auto archiveBytes = [](const auto& path) {
            std::ifstream file{ path, std::ios::binary };
            return std::string{ std::istreambuf_iterator<char>{ file }, std::istreambuf_iterator<char>{} };
        };
        const auto bytes = archiveBytes(zip);
        VERIFY_IS_TRUE(bytes.starts_with("PK"));
        VERIFY_IS_TRUE(bytes.find("diagnostics.json") != std::string::npos);
        VERIFY_IS_TRUE(bytes.find("logs/sample.log") != std::string::npos);
        VERIFY_IS_TRUE(bytes.find("logs/diagnostics.json") == std::string::npos);
        VERIFY_ARE_EQUAL(D::ArchiveResult::Success, D::ArchiveReportFile(tar, metadataOnly, {}, report));
        const auto partial = archiveBytes(metadataOnly);
        VERIFY_IS_TRUE(partial.find("diagnostics.json") != std::string::npos);
        VERIFY_IS_TRUE(partial.find("sample.log") == std::string::npos);
    }

    void ProtocolParsingTests::CapabilitySupportDistinguishesUnsupportedFromMalformed()
    {
        for (const auto* payload : { R"(["get_pane_context"])", R"(["other","get_pane_context"])" })
        {
            Json::Value capabilities;
            VERIFY_IS_TRUE(ParseJson(payload, capabilities));
            VERIFY_ARE_EQUAL(CapabilitySupport::Supported, ClassifyCapability(capabilities, "get_pane_context"));
        }
        for (const auto* payload : { "[]", R"(["other"])" })
        {
            Json::Value capabilities;
            VERIFY_IS_TRUE(ParseJson(payload, capabilities));
            VERIFY_ARE_EQUAL(CapabilitySupport::Unsupported, ClassifyCapability(capabilities, "get_pane_context"));
        }
        for (const auto* payload : { "null", "{}", "true", "1", R"("get_pane_context")", "[null]", R"(["get_pane_context",{}])", R"([false,"get_pane_context"])" })
        {
            Json::Value capabilities;
            VERIFY_IS_TRUE(ParseJson(payload, capabilities));
            VERIFY_ARE_EQUAL(CapabilitySupport::Invalid, ClassifyCapability(capabilities, "get_pane_context"));
        }
    }

    void ProtocolParsingTests::DefaultPasteRequestUsesDirectRoute()
    {
        Json::Value event;
        const auto route = ClassifySendEvent(
            R"({"type":"event","method":"request_default_paste","params":{"window_id":"1","tab_id":"tab-a","pane_id":"pane-a"}})",
            event);

        VERIFY_ARE_EQUAL(SendEventRoute::DefaultPaste, route);
        VERIFY_ARE_EQUAL("request_default_paste", event["method"].asString());
    }

    void ProtocolParsingTests::AgentAvailabilityUsesDirectRoute()
    {
        Json::Value event;
        const auto route = ClassifySendEvent(
            R"({"type":"event","method":"agent_availability_changed","params":{"agent_id":"copilot","tab_id":"tab-a"}})",
            event);

        VERIFY_ARE_EQUAL(SendEventRoute::AgentAvailability, route);
        VERIFY_ARE_EQUAL("copilot", event["params"]["agent_id"].asString());
    }

    void ProtocolParsingTests::AgentSessionsRetiredUsesDirectRoute()
    {
        Json::Value event;
        const auto route = ClassifySendEvent(
            R"({"type":"event","method":"agent_sessions_retired","params":{"operation_id":"123-1","success":true,"reason":"restart_agent_stack","failed_tabs":[]}})",
            event);

        VERIFY_ARE_EQUAL(SendEventRoute::AgentSessionsRetired, route);
        VERIFY_ARE_EQUAL("123-1", event["params"]["operation_id"].asString());
    }

    void ProtocolParsingTests::RestartRequestIdentityIsStampedOnce()
    {
        Json::Value event;
        VERIFY_IS_TRUE(ParseJson(
            R"({"type":"event","method":"restart_agent_stack","params":{}})",
            event));

        EnsureRequestId(event, "request-1");
        EnsureRequestId(event, "request-2");

        VERIFY_ARE_EQUAL("request-1", event["params"]["request_id"].asString());
    }

    void ProtocolParsingTests::BoundedCommandPreservesUtf8Characters()
    {
        const auto result = BuildBoundedCommand("a\xF0\x9F\x8D\xA6"
                                                "bc",
                                                1,
                                                3);
        VERIFY_ARE_EQUAL(std::string{ "a\xF0\x9F\x8D\xA6"
                                     "b" },
                         result.content);
        VERIFY_ARE_EQUAL(1, result.lineCount);
        VERIFY_IS_TRUE(result.truncated);

        const auto lines = BuildBoundedCommand("command\r\n"
                                               "first\r\n"
                                               "second\r\n",
                                               2,
                                               100);
        VERIFY_ARE_EQUAL("command\n"
                         "first",
                         lines.content);
        VERIFY_ARE_EQUAL(2, lines.lineCount);
        VERIFY_IS_TRUE(lines.truncated);

        const auto newlineLookahead = BuildBoundedCommand("command\n", 2, 7);
        VERIFY_ARE_EQUAL("command", newlineLookahead.content);
        VERIFY_IS_TRUE(newlineLookahead.truncated);

        const auto blankLineLookahead = BuildBoundedCommand("command\n\n", 2, 100);
        VERIFY_ARE_EQUAL("command\n", blankLineLookahead.content);
        VERIFY_ARE_EQUAL(2, blankLineLookahead.lineCount);
        VERIFY_IS_TRUE(blankLineLookahead.truncated);

        const auto leadingBlankLines = BuildBoundedCommand("\n\n"
                                                           "command\n",
                                                           10,
                                                           100);
        VERIFY_ARE_EQUAL("\n\n"
                         "command\n",
                         leadingBlankLines.content);
        VERIFY_ARE_EQUAL(4, leadingBlankLines.lineCount);
        VERIFY_IS_FALSE(leadingBlankLines.truncated);
    }

    void ProtocolParsingTests::BoundedBufferTailAppliesLineAndCharacterLimits()
    {
        const auto byLines = BuildBoundedBufferTail("first\r\n"
                                                  "second\r\n"
                                                  "third\r\n",
                                                  2,
                                                  100);
        VERIFY_ARE_EQUAL("second\n"
                         "third",
                         byLines.content);
        VERIFY_ARE_EQUAL(2, byLines.lineCount);
        VERIFY_IS_TRUE(byLines.truncated);

        const auto byCharacters = BuildBoundedBufferTail("one\r\n"
                                                       "two\r\n"
                                                       "three\r\n",
                                                       3,
                                                       6);
        VERIFY_ARE_EQUAL("\n"
                         "three",
                         byCharacters.content);
        VERIFY_ARE_EQUAL(2, byCharacters.lineCount);
        VERIFY_IS_TRUE(byCharacters.truncated);

        const auto exact = BuildBoundedBufferTail("one\r\n"
                                                 "two\r\n",
                                                 2,
                                                 7);
        VERIFY_ARE_EQUAL("one\n"
                         "two",
                         exact.content);
        VERIFY_ARE_EQUAL(2, exact.lineCount);
        VERIFY_IS_FALSE(exact.truncated);
    }

    void ProtocolParsingTests::BoundedBufferTailPreservesBlankLines()
    {
        const auto leading = BuildBoundedBufferTail("\r\n\r\n"
                                                   "error\r\n",
                                                   3,
                                                   100);
        VERIFY_ARE_EQUAL("\n\n"
                         "error",
                         leading.content);
        VERIFY_ARE_EQUAL(3, leading.lineCount);
        VERIFY_IS_FALSE(leading.truncated);

        const auto selectedTail = BuildBoundedBufferTail("older\r\n\r\n\r\n"
                                                        "error\r\n",
                                                        3,
                                                        100);
        VERIFY_ARE_EQUAL("\n\n"
                         "error",
                         selectedTail.content);
        VERIFY_ARE_EQUAL(3, selectedTail.lineCount);
        VERIFY_IS_TRUE(selectedTail.truncated);

        const auto interior = BuildBoundedBufferTail("first\r\n\r\n"
                                                    "error\r\n",
                                                    3,
                                                    100);
        VERIFY_ARE_EQUAL("first\n\n"
                         "error",
                         interior.content);
        VERIFY_ARE_EQUAL(3, interior.lineCount);
        VERIFY_IS_FALSE(interior.truncated);

        const auto trailing = BuildBoundedBufferTail("error\r\n\r\n", 3, 100);
        VERIFY_ARE_EQUAL("error\n", trailing.content);
        VERIFY_ARE_EQUAL(2, trailing.lineCount);
        VERIFY_IS_FALSE(trailing.truncated);

        const auto terminated = BuildBoundedBufferTail("error\r\n", 3, 100);
        VERIFY_ARE_EQUAL("error", terminated.content);
        VERIFY_ARE_EQUAL(1, terminated.lineCount);
        VERIFY_IS_FALSE(terminated.truncated);
    }
}
