// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/ProviderContracts.h"
#include "../RichTabProvider/CommandRunner.h"

#include <fstream>

using namespace Microsoft::Terminal::RichTab::Provider;
using namespace WEX::TestExecution;
using namespace WEX::Logging;
using namespace WEX::Common;

namespace TerminalAppUnitTests
{
    class RichTabProviderContractTests
    {
        TEST_CLASS(RichTabProviderContractTests);

        TEST_METHOD(ParsesValidPowerShellManifest);
        TEST_METHOD(RejectsUnknownAndDuplicateManifestMembers);
        TEST_METHOD(RejectsUnsafeEntrypoint);
        TEST_METHOD(RejectsWrongJsonShapesAndInvalidUtf8);
        TEST_METHOD(ParsesCompleteSnapshot);
        TEST_METHOD(RejectsUndeclaredOrMismatchedFields);
        TEST_METHOD(SerializesEpochScopedRequest);
        TEST_METHOD(NegotiatesV2PublishContract);
        TEST_METHOD(QuotesWindowsArguments);
        TEST_METHOD(TimesOutProviderThatDoesNotReadStdin);

        static constexpr std::string_view ValidManifest{
            R"({
                "schemaVersion": 1,
                "id": "sample.pr-tracker",
                "displayName": "PR Tracker",
                "publisher": "Sample",
                "version": "1.0.0",
                "protocol": { "minVersion": 1, "maxVersion": 1 },
                "runtime": {
                    "type": "powerShellV1",
                    "entrypoint": "provider.ps1",
                    "arguments": []
                },
                "activationEvents": ["onWorkingDirectoryChanged", "onManualRefresh"],
                "fields": [
                    { "id": "pull-request", "displayName": "Pull Request", "type": "string", "defaultVisible": true },
                    { "id": "failed-checks", "displayName": "Failed Checks", "type": "integer", "defaultVisible": true }
                ]
            })"
        };
    };

    void RichTabProviderContractTests::ParsesValidPowerShellManifest()
    {
        const auto parsed = ParseManifest(ValidManifest, LR"(C:\provider)");
        VERIFY_IS_TRUE(parsed.value.has_value());
        VERIFY_IS_TRUE(parsed.errors.empty());
        VERIFY_ARE_EQUAL(std::string{ "sample.pr-tracker" }, parsed.value->id);
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), parsed.value->fields.size());
        VERIFY_ARE_EQUAL(RuntimeKind::PowerShellV1, parsed.value->runtime.kind);
    }

    void RichTabProviderContractTests::RejectsUnknownAndDuplicateManifestMembers()
    {
        auto duplicate = std::string{ ValidManifest };
        const auto position = duplicate.find(R"("schemaVersion": 1)");
        duplicate.replace(position, std::string_view{ R"("schemaVersion": 1)" }.size(), R"("schemaVersion": 1, "schemaVersion": 1)");
        VERIFY_IS_FALSE(ParseManifest(duplicate, LR"(C:\provider)").value.has_value());

        auto unknown = std::string{ ValidManifest };
        unknown.insert(unknown.find(R"("schemaVersion")"), R"("unexpected": true,)");
        VERIFY_IS_FALSE(ParseManifest(unknown, LR"(C:\provider)").value.has_value());
    }

    void RichTabProviderContractTests::RejectsUnsafeEntrypoint()
    {
        auto manifest = std::string{ ValidManifest };
        const auto position = manifest.find("provider.ps1");
        manifest.replace(position, std::string_view{ "provider.ps1" }.size(), "..\\\\provider.ps1");
        VERIFY_IS_FALSE(ParseManifest(manifest, LR"(C:\provider)").value.has_value());
    }

    void RichTabProviderContractTests::RejectsWrongJsonShapesAndInvalidUtf8()
    {
        VERIFY_IS_FALSE(ParseManifest("[]", LR"(C:\provider)").value.has_value());

        auto wrongRuntime = std::string{ ValidManifest };
        const auto runtimeStart = wrongRuntime.find(R"("runtime": {)");
        const auto runtimeEnd = wrongRuntime.find(R"(},)", runtimeStart);
        wrongRuntime.replace(runtimeStart, runtimeEnd - runtimeStart + 2, R"("runtime": [],)");
        VERIFY_IS_FALSE(ParseManifest(wrongRuntime, LR"(C:\provider)").value.has_value());

        auto invalidUtf8 = std::string{ ValidManifest };
        const auto displayName = invalidUtf8.find("PR Tracker");
        invalidUtf8.replace(displayName, std::string_view{ "PR Tracker" }.size(), std::string{ "\xC3\x28", 2 });
        VERIFY_IS_FALSE(ParseManifest(invalidUtf8, LR"(C:\provider)").value.has_value());
    }

    void RichTabProviderContractTests::ParsesCompleteSnapshot()
    {
        const auto manifest = ParseManifest(ValidManifest, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        const auto snapshot = ParseSnapshot(
            R"({
                "protocolVersion": 1,
                "requestId": "request-1",
                "result": {
                    "fields": {
                        "pull-request": "PR #541",
                        "failed-checks": 1
                    },
                    "tooltip": "One failed check",
                    "accessibilityText": "Pull request 541, one failed check"
                }
            })",
            *manifest.value,
            "request-1");
        VERIFY_IS_TRUE(snapshot.value.has_value());
        VERIFY_ARE_EQUAL(static_cast<size_t>(2), snapshot.value->fields.size());
    }

    void RichTabProviderContractTests::RejectsUndeclaredOrMismatchedFields()
    {
        const auto manifest = ParseManifest(ValidManifest, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        const auto snapshot = ParseSnapshot(
            R"({
                "protocolVersion": 1,
                "requestId": "request-2",
                "result": {
                    "fields": {
                        "pull-request": 541,
                        "unknown": true
                    }
                }
            })",
            *manifest.value,
            "request-2");
        VERIFY_IS_FALSE(snapshot.value.has_value());
    }

    void RichTabProviderContractTests::SerializesEpochScopedRequest()
    {
        const auto manifest = ParseManifest(ValidManifest, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        Request request;
        request.requestId = "request-3";
        request.providerId = manifest.value->id;
        request.processEpoch = 7;
        request.sessionId = "session-id";
        request.reason = ActivationEvent::WorkingDirectoryChanged;
        request.workingDirectory = LR"(C:\repo)";
        request.workingDirectoryAuthoritative = true;
        request.contextRevision = 2;
        request.shellType = "pwsh";
        const auto serialized = SerializeRequest(request, *manifest.value);
        VERIFY_IS_TRUE(serialized.value.has_value());
        VERIFY_IS_TRUE(serialized.value->find(R"("processEpoch":7)") != std::string::npos);
        VERIFY_IS_TRUE(serialized.value->find(R"("sessionId":"session-id")") != std::string::npos);
        VERIFY_IS_TRUE(serialized.value->find(R"("type":"pwsh")") != std::string::npos);
    }

    void RichTabProviderContractTests::NegotiatesV2PublishContract()
    {
        auto v2Manifest = std::string{ ValidManifest };
        const auto range = v2Manifest.find(R"("minVersion": 1, "maxVersion": 1)");
        v2Manifest.replace(range, std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(), R"("minVersion": 2, "maxVersion": 2)");
        const auto manifest = ParseManifest(v2Manifest, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        Request request;
        request.protocolVersion = 2;
        request.requestId = "request-v2";
        request.providerId = manifest.value->id;
        request.processEpoch = 7;
        request.sessionId = "session-v2";
        request.reason = ActivationEvent::ManualRefresh;
        const auto serialized = SerializeRequest(request, *manifest.value);
        VERIFY_IS_TRUE(serialized.value.has_value());
        VERIFY_IS_TRUE(serialized.value->find(R"("protocolVersion":2)") != std::string::npos);

        const auto snapshot = ParseSnapshot(
            R"({"protocolVersion":2,"requestId":"request-v2","result":{"fields":{}}})",
            *manifest.value,
            request.requestId);
        VERIFY_IS_TRUE(snapshot.value.has_value());
    }

    void RichTabProviderContractTests::QuotesWindowsArguments()
    {
        std::wstring commandLine;
        CommandRunner::QuoteArgument(LR"(C:\path with spaces\provider.ps1)", commandLine);
        VERIFY_ARE_EQUAL(std::wstring{ LR"("C:\path with spaces\provider.ps1")" }, commandLine);

        commandLine.clear();
        CommandRunner::QuoteArgument(LR"(value\"quoted")", commandLine);
        VERIFY_ARE_EQUAL(std::wstring{ LR"("value\\\"quoted\"")" }, commandLine);
    }

    void RichTabProviderContractTests::TimesOutProviderThatDoesNotReadStdin()
    {
        if (!CommandRunner::ResolvePowerShell())
        {
            Log::Comment(L"PowerShell 7 is unavailable; skipping command runner timeout coverage.");
            return;
        }

        const auto root = std::filesystem::temp_directory_path() /
                          (L"rich-tab-provider-test-" + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(GetTickCount64()));
        struct Cleanup
        {
            std::filesystem::path root;
            ~Cleanup()
            {
                std::error_code error;
                std::filesystem::remove_all(root, error);
            }
        } cleanup{ root };

        std::filesystem::create_directories(root);
        {
            std::ofstream script{ root / L"provider.ps1", std::ios::binary };
            script << "Start-Sleep -Seconds 30\n";
        }

        const auto parsed = ParseManifest(ValidManifest, root);
        VERIFY_IS_TRUE(parsed.value.has_value());
        const auto started = std::chrono::steady_clock::now();
        const auto result = CommandRunner{}.Run(
            *parsed.value,
            std::string(CommandRunner::MaximumRequestSize - 1, 'x'),
            std::chrono::milliseconds{ 250 });
        const auto elapsed = std::chrono::steady_clock::now() - started;

        VERIFY_ARE_EQUAL(CommandResult::Status::TimedOut, result.status);
        VERIFY_IS_TRUE(elapsed < std::chrono::seconds{ 5 });
    }
}
