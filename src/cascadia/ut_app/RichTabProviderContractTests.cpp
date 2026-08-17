// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/ProviderContracts.h"
#include "../RichTabProvider/CommandRunner.h"
#include "../RichTabProvider/PersistentProviderSupervisor.h"

#include <condition_variable>
#include <fstream>
#include <iterator>
#include <mutex>
#include <thread>

using namespace Microsoft::Terminal::RichTab::Provider;
using namespace WEX::TestExecution;
using namespace WEX::Logging;
using namespace WEX::Common;

namespace TerminalAppUnitTests
{
    namespace
    {
        class FakePersistentProviderProcess final : public IPersistentProviderProcess
        {
        public:
            bool Send(std::string frame) override
            {
                std::lock_guard lock{ _mutex };
                if (!_running)
                {
                    return false;
                }
                _frames.emplace_back(std::move(frame));
                _condition.notify_all();
                return true;
            }

            bool IsRunning() const noexcept override
            {
                std::lock_guard lock{ _mutex };
                return _running;
            }

            void Stop(std::string stopFrame, std::chrono::milliseconds) noexcept override
            {
                std::lock_guard lock{ _mutex };
                if (!stopFrame.empty())
                {
                    _frames.emplace_back(std::move(stopFrame));
                }
                _running = false;
                _stopped = true;
                _condition.notify_all();
            }

            std::string StandardError() const override
            {
                return {};
            }

            bool WaitForFrameCount(const size_t count)
            {
                std::unique_lock lock{ _mutex };
                return _condition.wait_for(
                    lock,
                    std::chrono::seconds{ 5 },
                    [&]() { return _frames.size() >= count; });
            }

            bool WaitForStop()
            {
                std::unique_lock lock{ _mutex };
                return _condition.wait_for(
                    lock,
                    std::chrono::seconds{ 5 },
                    [&]() { return _stopped; });
            }

            std::vector<std::string> Frames() const
            {
                std::lock_guard lock{ _mutex };
                return _frames;
            }

        private:
            mutable std::mutex _mutex;
            std::condition_variable _condition;
            std::vector<std::string> _frames;
            bool _running{ true };
            bool _stopped{ false };
        };

        class FakePersistentProviderProcessFactory final : public IPersistentProviderProcessFactory
        {
        public:
            PersistentProviderLaunchResult Launch(
                const Manifest&,
                const CommandRunner::Environment&) override
            {
                auto process = std::make_shared<FakePersistentProviderProcess>();
                {
                    std::lock_guard lock{ _mutex };
                    _processes.emplace_back(process);
                }
                _condition.notify_all();
                return { std::move(process), 0 };
            }

            std::shared_ptr<FakePersistentProviderProcess> WaitForProcess(const size_t index)
            {
                std::unique_lock lock{ _mutex };
                if (!_condition.wait_for(
                        lock,
                        std::chrono::seconds{ 5 },
                        [&]() { return _processes.size() > index; }))
                {
                    return {};
                }
                return _processes[index];
            }

            size_t LaunchCount() const
            {
                std::lock_guard lock{ _mutex };
                return _processes.size();
            }

        private:
            mutable std::mutex _mutex;
            std::condition_variable _condition;
            std::vector<std::shared_ptr<FakePersistentProviderProcess>> _processes;
        };
    }

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
        TEST_METHOD(ParsesPersistentManifest);
        TEST_METHOD(RejectsInvalidPersistentHosting);
        TEST_METHOD(SerializesPersistentControlFrames);
        TEST_METHOD(SupervisesPersistentRefreshAndRestart);
        TEST_METHOD(StopsPersistentProcessWhenFrameIsRejected);
        TEST_METHOD(RunsRepeatedFramesInOnePersistentProcess);
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

    void RichTabProviderContractTests::ParsesPersistentManifest()
    {
        auto persistent = std::string{ ValidManifest };
        persistent.replace(
            persistent.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        persistent.replace(
            persistent.find(R"("minVersion": 1, "maxVersion": 1)"),
            std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(),
            R"("minVersion": 2, "maxVersion": 2)");
        persistent.insert(
            persistent.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");

        const auto manifest = ParseManifest(persistent, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());
        VERIFY_ARE_EQUAL(2u, manifest.value->schemaVersion);
        VERIFY_ARE_EQUAL(HostingKind::Persistent, manifest.value->hosting.kind);
        VERIFY_ARE_EQUAL(1u, manifest.value->hosting.controlProtocolVersion);
    }

    void RichTabProviderContractTests::RejectsInvalidPersistentHosting()
    {
        auto missingHosting = std::string{ ValidManifest };
        missingHosting.replace(
            missingHosting.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        VERIFY_IS_FALSE(ParseManifest(missingHosting, LR"(C:\provider)").value.has_value());

        auto v1WithHosting = std::string{ ValidManifest };
        v1WithHosting.insert(
            v1WithHosting.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        VERIFY_IS_FALSE(ParseManifest(v1WithHosting, LR"(C:\provider)").value.has_value());

        auto wrongProtocol = missingHosting;
        wrongProtocol.insert(
            wrongProtocol.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        VERIFY_IS_FALSE(ParseManifest(wrongProtocol, LR"(C:\provider)").value.has_value());
    }

    void RichTabProviderContractTests::SerializesPersistentControlFrames()
    {
        auto persistent = std::string{ ValidManifest };
        persistent.replace(
            persistent.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        persistent.replace(
            persistent.find(R"("minVersion": 1, "maxVersion": 1)"),
            std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(),
            R"("minVersion": 2, "maxVersion": 2)");
        persistent.insert(
            persistent.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        const auto manifest = ParseManifest(persistent, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        Request request;
        request.protocolVersion = 2;
        request.requestId = "persistent-1";
        request.providerId = manifest.value->id;
        request.processEpoch = 7;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        const std::vector grants{
            PublishGrant{ request.requestId, "lease-one", 90000 },
            PublishGrant{ request.requestId, "lease-two", 90000 },
        };
        const auto start = SerializeControlFrame(
            ControlMessageKind::Start,
            request,
            grants,
            *manifest.value);
        VERIFY_IS_TRUE(start.value.has_value());
        VERIFY_IS_TRUE(start.value->starts_with("Content-Length: "));
        VERIFY_IS_TRUE(start.value->find("\r\n\r\n") != std::string::npos);
        VERIFY_IS_TRUE(start.value->find(R"("kind":"start")") != std::string::npos);
        VERIFY_IS_TRUE(start.value->find(R"("lease":"lease-two")") != std::string::npos);

        const auto stop = SerializeControlFrame(
            ControlMessageKind::Stop,
            std::nullopt,
            {},
            *manifest.value);
        VERIFY_IS_TRUE(stop.value.has_value());
        VERIFY_IS_TRUE(stop.value->find(R"("kind":"stop")") != std::string::npos);

        VERIFY_IS_FALSE(SerializeControlFrame(
                            ControlMessageKind::Lease,
                            std::nullopt,
                            {},
                            *manifest.value)
                            .value.has_value());
    }

    void RichTabProviderContractTests::SupervisesPersistentRefreshAndRestart()
    {
        auto persistent = std::string{ ValidManifest };
        persistent.replace(
            persistent.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        persistent.replace(
            persistent.find(R"("minVersion": 1, "maxVersion": 1)"),
            std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(),
            R"("minVersion": 2, "maxVersion": 2)");
        persistent.insert(
            persistent.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        const auto manifest = ParseManifest(persistent, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        const auto factory = std::make_shared<FakePersistentProviderProcessFactory>();
        PersistentProviderSupervisor supervisor{ factory };
        const PersistentProviderKey key{ manifest.value->id, "session" };
        std::mutex callbackMutex;
        std::vector<std::pair<uint64_t, bool>> callbacks;

        supervisor.StartOrRefresh(
            key,
            *manifest.value,
            {},
            [&](const uint64_t generation, const bool started) {
                std::lock_guard lock{ callbackMutex };
                callbacks.emplace_back(generation, started);
                return std::optional<std::string>{ "first" };
            },
            1,
            false,
            true);
        const auto first = factory->WaitForProcess(0);
        VERIFY_IS_NOT_NULL(first.get());
        VERIFY_IS_TRUE(first->WaitForFrameCount(1));

        supervisor.StartOrRefresh(
            key,
            *manifest.value,
            {},
            [&](const uint64_t generation, const bool started) {
                std::lock_guard lock{ callbackMutex };
                callbacks.emplace_back(generation, started);
                return std::optional<std::string>{ "second" };
            },
            2,
            false,
            false);
        VERIFY_IS_TRUE(first->WaitForFrameCount(2));
        VERIFY_ARE_EQUAL(static_cast<size_t>(1), factory->LaunchCount());

        {
            std::lock_guard lock{ callbackMutex };
            VERIFY_ARE_EQUAL(static_cast<size_t>(2), callbacks.size());
            VERIFY_IS_TRUE(callbacks[0].second);
            VERIFY_IS_FALSE(callbacks[1].second);
            VERIFY_ARE_EQUAL(callbacks[0].first, callbacks[1].first);
        }

        supervisor.StartOrRefresh(
            key,
            *manifest.value,
            {},
            [&](const uint64_t generation, const bool started) {
                std::lock_guard lock{ callbackMutex };
                callbacks.emplace_back(generation, started);
                return std::optional<std::string>{ "restart" };
            },
            3,
            true,
            true);
        const auto second = factory->WaitForProcess(1);
        VERIFY_IS_NOT_NULL(second.get());
        VERIFY_IS_TRUE(first->WaitForStop());
        VERIFY_IS_TRUE(second->WaitForFrameCount(1));

        {
            std::lock_guard lock{ callbackMutex };
            VERIFY_ARE_EQUAL(static_cast<size_t>(3), callbacks.size());
            VERIFY_IS_TRUE(callbacks[2].second);
            VERIFY_ARE_NOT_EQUAL(callbacks[1].first, callbacks[2].first);
        }

        supervisor.Stop(key);
        VERIFY_IS_TRUE(second->WaitForStop());
        const auto frames = second->Frames();
        VERIFY_IS_TRUE(frames.back().find(R"("kind":"stop")") != std::string::npos);
        VERIFY_ARE_EQUAL(
            std::chrono::milliseconds{ 1000 }.count(),
            PersistentProviderSupervisor::BackoffForFailure(1).count());
        VERIFY_ARE_EQUAL(
            std::chrono::milliseconds{ 30000 }.count(),
            PersistentProviderSupervisor::BackoffForFailure(20).count());
    }

    void RichTabProviderContractTests::StopsPersistentProcessWhenFrameIsRejected()
    {
        auto persistent = std::string{ ValidManifest };
        persistent.replace(
            persistent.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        persistent.replace(
            persistent.find(R"("minVersion": 1, "maxVersion": 1)"),
            std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(),
            R"("minVersion": 2, "maxVersion": 2)");
        persistent.insert(
            persistent.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        const auto manifest = ParseManifest(persistent, LR"(C:\provider)");
        VERIFY_IS_TRUE(manifest.value.has_value());

        const auto factory = std::make_shared<FakePersistentProviderProcessFactory>();
        PersistentProviderSupervisor supervisor{ factory };
        supervisor.StartOrRefresh(
            PersistentProviderKey{ manifest.value->id, "session" },
            *manifest.value,
            {},
            [](const uint64_t, const bool) {
                return std::optional<std::string>{};
            },
            1,
            false,
            true);

        const auto process = factory->WaitForProcess(0);
        VERIFY_IS_NOT_NULL(process.get());
        VERIFY_IS_TRUE(process->WaitForStop());
        VERIFY_IS_TRUE(process->Frames().empty());
    }

    void RichTabProviderContractTests::RunsRepeatedFramesInOnePersistentProcess()
    {
        if (!CommandRunner::ResolvePowerShell())
        {
            Log::Comment(L"PowerShell 7 is unavailable; skipping persistent process integration coverage.");
            return;
        }

        const auto root = std::filesystem::temp_directory_path() /
                          (L"rich-tab-persistent-test-" + std::to_wstring(GetCurrentProcessId()) + L"-" + std::to_wstring(GetTickCount64()));
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
        const auto output = root / L"frames.txt";
        {
            std::ofstream script{ root / L"provider.ps1", std::ios::binary };
            script << "$content = [Console]::In.ReadToEnd()\n"
                      "[IO.File]::WriteAllText($args[0], \"$PID`n$content\")\n";
        }

        auto persistent = std::string{ ValidManifest };
        persistent.replace(
            persistent.find(R"("schemaVersion": 1)"),
            std::string_view{ R"("schemaVersion": 1)" }.size(),
            R"("schemaVersion": 2)");
        persistent.replace(
            persistent.find(R"("minVersion": 1, "maxVersion": 1)"),
            std::string_view{ R"("minVersion": 1, "maxVersion": 1)" }.size(),
            R"("minVersion": 2, "maxVersion": 2)");
        persistent.insert(
            persistent.find(R"("activationEvents")"),
            R"("hosting": { "kind": "persistent", "controlProtocolVersion": 1 },)");
        auto manifest = ParseManifest(persistent, root);
        VERIFY_IS_TRUE(manifest.value.has_value());
        manifest.value->runtime.arguments = { output.native() };

        PersistentProviderSupervisor supervisor;
        const PersistentProviderKey key{ manifest.value->id, "real-process-session" };
        supervisor.StartOrRefresh(
            key,
            *manifest.value,
            {},
            [](const uint64_t, const bool) {
                return std::optional<std::string>{ "frame-one\n" };
            },
            1,
            false,
            true);
        supervisor.StartOrRefresh(
            key,
            *manifest.value,
            {},
            [](const uint64_t, const bool) {
                return std::optional<std::string>{ "frame-two\n" };
            },
            2,
            false,
            false);
        supervisor.Stop(key);

        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds{ 5 };
        while (!std::filesystem::exists(output) &&
               std::chrono::steady_clock::now() < deadline)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds{ 50 });
        }
        VERIFY_IS_TRUE(std::filesystem::exists(output));
        std::ifstream stream{ output, std::ios::binary };
        const std::string contents{
            std::istreambuf_iterator<char>{ stream },
            std::istreambuf_iterator<char>{}
        };
        VERIFY_IS_TRUE(contents.find("frame-one\n") != std::string::npos);
        VERIFY_IS_TRUE(contents.find("frame-two\n") != std::string::npos);
        VERIFY_IS_TRUE(contents.find(R"("kind":"stop")") != std::string::npos);
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
