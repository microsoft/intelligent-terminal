// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include <iterator>

#include "../RichTabProvider/CommandRunner.h"
#include "../RichTabProvider/BuiltInProviderCatalog.h"
#include "../RichTabProvider/ProviderBroker.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::RichTab::Provider;

namespace
{
    std::filesystem::path _TestModuleDirectory()
    {
        HMODULE module = nullptr;
        VERIFY_WIN32_BOOL_SUCCEEDED(GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            reinterpret_cast<LPCWSTR>(&_TestModuleDirectory),
            &module));

        std::wstring path(32768, L'\0');
        const auto length = GetModuleFileNameW(module, path.data(), static_cast<DWORD>(path.size()));
        VERIFY_IS_TRUE(length > 0 && length < path.size());
        path.resize(length);
        return std::filesystem::path{ path }.parent_path();
    }

    void _WriteFile(const std::filesystem::path& path, const std::string_view contents)
    {
        std::ofstream stream{ path, std::ios::binary };
        VERIFY_IS_TRUE(stream.is_open());
        stream.write(contents.data(), static_cast<std::streamsize>(contents.size()));
        VERIFY_IS_TRUE(stream.good());
    }
}

namespace TerminalAppUnitTests
{
    class RichTabProviderTests
    {
        TEST_CLASS(RichTabProviderTests);

        TEST_METHOD(PowerShellProviderPreservesUnicodeAcrossProcessBoundary);
        TEST_METHOD(LocalizedFieldDisplayNamesOverrideManifestFallbacks);
        TEST_METHOD(ProviderDiagnosticVersionRejectsUntrustedText);
        TEST_METHOD(ProviderDiagnosticLogWritesContentMinimizedRecord);
        TEST_METHOD(ProviderDiagnosticLogCleanupSkipsUnrelatedFiles);
        TEST_METHOD(ProviderDiagnosticLogDropsOnContention);
        TEST_METHOD(CommandRunnerMarksLaunchFailureObservations);
        TEST_METHOD(BrokerClearsSuccessfulSnapshotAfterProviderFailure);
        TEST_METHOD(BrokerKeepsAgentStatusPairedWithSnapshotDuringRefresh);
        TEST_METHOD(BrokerClearsSnapshotAndSkipsExecutionWhenAllFieldsDeselected);
        TEST_METHOD(ResolveGitUsesExplicitPathAndDetectsMissingExecutable);
        TEST_METHOD(GitStatusProviderHandlesMissingGitGracefully);
        TEST_METHOD(GitStatusProviderUsesDisplayedDirectoryForGit);
        TEST_METHOD(BrokerInjectsImmutableGitAvailabilityIntoRequests);
        TEST_METHOD(GitStatusProviderKnownMissingGitDoesNotFallBack);
        TEST_METHOD(GitStatusProviderInvokesLiteralApplicationPath);
        TEST_METHOD(GitStatusProviderSkipsGitWhenNoGitFieldsVisible);
        TEST_METHOD(GitStatusProviderHandlesGitFailureGracefully);
        TEST_METHOD(GitStatusProviderSetsSafeGitConfigEnvironment);
        TEST_METHOD(AgentCwdInvalidatesOnlyTrustedGitStatusField);
        TEST_METHOD(DetachedSessionDoesNotReuseOldGitMetadata);
    };

    void RichTabProviderTests::ProviderDiagnosticVersionRejectsUntrustedText()
    {
        const auto version = ParseProviderDiagnosticVersion("1.2.3");
        VERIFY_IS_TRUE(version.valid);
        VERIFY_ARE_EQUAL(uint16_t{ 1 }, version.major);
        VERIFY_ARE_EQUAL(uint16_t{ 2 }, version.minor);
        VERIFY_ARE_EQUAL(uint16_t{ 3 }, version.patch);
        VERIFY_ARE_EQUAL(uint8_t{ 3 }, version.componentCount);

        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("").valid);
        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("1.2.3-canary").valid);
        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("1..3").valid);
        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("1.2.3.").valid);
        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("1.2.3.4.5").valid);
        VERIFY_IS_FALSE(ParseProviderDiagnosticVersion("70000").valid);
    }

    void RichTabProviderTests::ProviderDiagnosticLogWritesContentMinimizedRecord()
    {
        const auto logRoot = std::filesystem::temp_directory_path() /
                             (L"RichTabProviderLog-" + std::to_wstring(GetCurrentProcessId()) +
                              L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(logRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(logRoot, error);
        });

        {
            auto logger = ProviderDiagnosticLog::CreateForTests(42, logRoot);
            VERIFY_IS_NOT_NULL(logger.get());

            ProviderDiagnosticRecord record;
            record.event = ProviderDiagnosticEvent::Invocation;
            record.timestamp = ProviderDiagnosticTimestamp();
            record.processId = GetCurrentProcessId();
            record.processEpoch = 42;
            record.hostRequestId = 7;
            record.generation = 8;
            record.contextRevision = 9;
            record.durationMicroseconds = 10;
            record.providerVersion = ParseProviderDiagnosticVersion("1.2.3");
            record.activationReason = ActivationEvent::CommandFinished;
            record.runnerStatus = CommandResult::Status::Completed;
            record.parseOutcome = ProviderParseOutcome::Valid;
            record.brokerDisposition = ProviderBrokerDisposition::Accepted;
            record.selectedFields = ProviderFieldBranch | ProviderFieldChanges;
            record.returnedFields = ProviderFieldBranch;
            record.catalogOrigin = true;
            record.runnerAttempted = true;
            record.processStarted = true;
            record.exitCodeValid = true;
            record.outputLimitObserved = true;
            logger->TryLog(record);
            record.hostRequestId = 8;
            record.providerVersion = ParseProviderDiagnosticVersion("repository-value-canary");
            logger->TryLog(record);
            VERIFY_ARE_EQUAL(uint64_t{ 0 }, logger->DroppedRecords());
        }

        std::vector<std::filesystem::path> logs;
        for (const auto& entry : std::filesystem::directory_iterator{ logRoot })
        {
            if (entry.is_regular_file())
            {
                logs.emplace_back(entry.path());
            }
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, logs.size());
        std::ifstream stream{ logs.front(), std::ios::binary };
        VERIFY_IS_TRUE(stream.is_open());
        const std::string contents{
            std::istreambuf_iterator<char>{ stream },
            std::istreambuf_iterator<char>{}
        };
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("event":"invocation")"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("activation":"command-finished")"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("providerVersion":"1.2.3")"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("providerVersion":null)"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("selectedFields":["branch","changes"])"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, contents.find(R"("brokerDisposition":"accepted")"));
        VERIFY_ARE_EQUAL(std::string::npos, contents.find("C:\\secret\\cwd-canary"));
        VERIFY_ARE_EQUAL(std::string::npos, contents.find("repository-value-canary"));
        VERIFY_ARE_EQUAL(std::string::npos, contents.find("stderr-value-canary"));
    }

    void RichTabProviderTests::ProviderDiagnosticLogCleanupSkipsUnrelatedFiles()
    {
        const auto logRoot = std::filesystem::temp_directory_path() /
                             (L"RichTabProviderCleanup-" + std::to_wstring(GetCurrentProcessId()) +
                              L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(logRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(logRoot, error);
        });

        for (size_t index = 0; index < 128; ++index)
        {
            std::ofstream{ logRoot / (L"unrelated-" + std::to_wstring(index) + L".log"), std::ios::binary } << "unrelated";
        }
        const auto oldLog = logRoot / L"rich-tab-provider-20200101-1-0000000000000001.jsonl";
        const auto currentLog = logRoot / L"rich-tab-provider-20990101-1-0000000000000002.jsonl";
        _WriteFile(oldLog, "{}\n");
        _WriteFile(currentLog, "{}\n");
        std::error_code timeError;
        std::filesystem::last_write_time(
            oldLog,
            std::filesystem::file_time_type::clock::now() - std::chrono::hours{ 24 * 8 },
            timeError);
        VERIFY_IS_FALSE(static_cast<bool>(timeError));

        {
            auto logger = ProviderDiagnosticLog::CreateForTests(45, logRoot);
            VERIFY_IS_NOT_NULL(logger.get());
        }

        VERIFY_IS_FALSE(std::filesystem::exists(oldLog));
        VERIFY_IS_TRUE(std::filesystem::exists(currentLog));
        VERIFY_IS_TRUE(std::filesystem::exists(logRoot / L"unrelated-0.log"));
        VERIFY_IS_TRUE(std::filesystem::exists(logRoot / L"unrelated-127.log"));
    }

    void RichTabProviderTests::ProviderDiagnosticLogDropsOnContention()
    {
        const auto logRoot = std::filesystem::temp_directory_path() /
                             (L"RichTabProviderContention-" + std::to_wstring(GetCurrentProcessId()) +
                              L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(logRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(logRoot, error);
        });
        auto logger = ProviderDiagnosticLog::CreateForTests(46, logRoot, true);
        VERIFY_IS_NOT_NULL(logger.get());

        wil::unique_handle started{ CreateEventW(nullptr, TRUE, FALSE, nullptr) };
        wil::unique_handle completed{ CreateEventW(nullptr, TRUE, FALSE, nullptr) };
        VERIFY_IS_NOT_NULL(started.get());
        VERIFY_IS_NOT_NULL(completed.get());

        AcquireSRWLockExclusive(&logger->_queueLock);
        auto releaseQueueLock = wil::scope_exit([&]() noexcept {
            ReleaseSRWLockExclusive(&logger->_queueLock);
        });
        std::thread producer{ [&]() {
            SetEvent(started.get());
            logger->TryLog({});
            SetEvent(completed.get());
        } };
        const auto startedResult = WaitForSingleObject(started.get(), 1000);
        const auto completedResult = startedResult == WAIT_OBJECT_0 ?
                                         WaitForSingleObject(completed.get(), 1000) :
                                         WAIT_TIMEOUT;
        ReleaseSRWLockExclusive(&logger->_queueLock);
        releaseQueueLock.release();
        producer.join();

        VERIFY_ARE_EQUAL(DWORD{ WAIT_OBJECT_0 }, startedResult);
        VERIFY_ARE_EQUAL(DWORD{ WAIT_OBJECT_0 }, completedResult);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, logger->DroppedRecords());
    }

    void RichTabProviderTests::CommandRunnerMarksLaunchFailureObservations()
    {
        const auto providerRoot = std::filesystem::temp_directory_path() /
                                  (L"RichTabMissingEntrypoint-" + std::to_wstring(GetCurrentProcessId()) +
                                   L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(providerRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(providerRoot, error);
        });

        Manifest manifest;
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"missing-provider.ps1";
        manifest.extensionRoot = providerRoot;

        const auto result = CommandRunner{}.Run(manifest, "{}", std::chrono::seconds{ 1 });
        VERIFY_IS_TRUE(result.status == CommandResult::Status::LaunchFailed);
        VERIFY_IS_FALSE(result.processStarted);
        VERIFY_IS_FALSE(result.exitCodeValid);
        VERIFY_IS_TRUE(result.win32ErrorValid);
        VERIFY_IS_FALSE(result.outputLimitObserved);
    }

    void RichTabProviderTests::AgentCwdInvalidatesOnlyTrustedGitStatusField()
    {
        ProviderBroker broker{ std::nullopt };
        Registration trusted;
        trusted.manifest.id = "com.microsoft.intelligent-terminal.git-status";
        trusted.manifest.activationEvents = { ActivationEvent::WorkingDirectoryChanged, ActivationEvent::ManualRefresh };
        trusted.manifest.fields = { FieldDeclaration{ "workingDirectory", "Directory" },
                                    FieldDeclaration{ "branch", "Branch" } };
        trusted.kind = RegistrationKind::Managed;
        trusted.origin = RegistrationOrigin::BuiltInCatalog;
        trusted.root = BuiltInProviderCatalog::PackageRoot() / L"RichTabProviders" / L"GitStatus";
        trusted.enabled = true;
        trusted.integrityValid = true;
        Registration other = trusted;
        other.manifest.id = "com.example.other";
        other.manifest.activationEvents = { ActivationEvent::ManualRefresh };
        other.kind = RegistrationKind::Development;
        broker._providers = { trusted, other };

        auto& session = broker._sessions["agent-cwd-test"];
        session.context.sessionId = "agent-cwd-test";
        session.context.workingDirectory = L"C:\\shell";
        session.context.workingDirectoryAuthoritative = true;
        session.sessionIncarnation = 1;
        std::vector<BrokerUpdate> updates;
        session.callbacks.emplace(1, [&](const auto& update) { updates.emplace_back(update); });
        broker._attachmentSessions.emplace(1, session.context.sessionId);
        auto& git = session.providers[trusted.manifest.id];
        git.snapshot.emplace();
        git.snapshot->fields.emplace("workingDirectory", std::string{ "C:\\shell" });
        git.snapshot->fields.emplace("repository", std::string{ "old-repo" });
        git.snapshot->fields.emplace("branch", std::string{ "main" });
        git.snapshot->fields.emplace("changes", std::string{ "2" });
        git.snapshot->tooltip = "Old repository";
        git.running = true;
        git.runningGeneration = 1;
        auto& thirdParty = session.providers[other.manifest.id];
        thirdParty.snapshot.emplace();
        thirdParty.snapshot->fields.emplace("workingDirectory", std::string{ "third-party" });
        thirdParty.running = true;
        thirdParty.runningGeneration = 2;

        broker.UpdateAgentWorkingDirectory(1, true, std::string{ "D:\\agent" });

        VERIFY_ARE_EQUAL(size_t{ 1 }, updates.size());
        VERIFY_IS_FALSE(git.snapshot->fields.contains("workingDirectory"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("repository"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("branch"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("changes"));
        VERIFY_IS_FALSE(git.snapshot->tooltip.has_value());
        VERIFY_IS_TRUE(thirdParty.snapshot->fields.contains("workingDirectory"));
        VERIFY_IS_TRUE(git.pending.has_value());
        VERIFY_IS_FALSE(thirdParty.pending.has_value());
        VERIFY_ARE_EQUAL(std::string{ "D:\\agent" }, git.pending->request.firstPartyFields.at("agentWorkingDirectory"));
        VERIFY_ARE_EQUAL(std::string{ "true" }, git.pending->request.firstPartyFields.at("agentCwdActive"));
        VERIFY_ARE_EQUAL(std::wstring{ L"third-party" }, updates.back().presentation->text);

        broker.Notify(1, ActivationEvent::ManualRefresh);
        VERIFY_IS_TRUE(thirdParty.pending.has_value());
        VERIFY_IS_FALSE(thirdParty.pending->request.firstPartyFields.contains("agentCwdActive"));
        VERIFY_IS_FALSE(thirdParty.pending->request.firstPartyFields.contains("agentWorkingDirectory"));

        git.snapshot->fields.emplace("workingDirectory", std::string{ "D:\\agent" });
        git.snapshot->fields.emplace("repository", std::string{ "old-repo" });
        git.snapshot->fields.emplace("branch", std::string{ "main" });
        git.snapshot->fields.emplace("changes", std::string{ "2" });
        git.snapshot->tooltip = "Old repository";
        broker._nextAttachment = 2;
        std::vector<BrokerUpdate> duplicateUpdates;
        const auto second = broker.Attach(session.context, [&](const auto& update) {
            duplicateUpdates.emplace_back(update);
        });
        VERIFY_ARE_EQUAL(ProviderBroker::AttachmentId{ 2 }, second);
        VERIFY_IS_FALSE(git.snapshot->fields.contains("workingDirectory"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("repository"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("branch"));
        VERIFY_IS_FALSE(git.snapshot->fields.contains("changes"));
        VERIFY_IS_FALSE(git.snapshot->tooltip.has_value());
        VERIFY_IS_FALSE(duplicateUpdates.empty());
        VERIFY_ARE_EQUAL(std::wstring{ L"third-party" }, duplicateUpdates.back().presentation->text);
        VERIFY_ARE_EQUAL(std::string{ "true" }, git.pending->request.firstPartyFields.at("agentCwdActive"));
        VERIFY_IS_FALSE(git.pending->request.firstPartyFields.contains("agentWorkingDirectory"));

        broker.UpdateAgentWorkingDirectory(second, false, std::nullopt);
        VERIFY_IS_TRUE(session.agentActive);
        VERIFY_IS_FALSE(session.agentWorkingDirectory.has_value());
        broker.UpdateAgentWorkingDirectory(1, false, std::nullopt);
        VERIFY_IS_FALSE(session.agentActive);
        VERIFY_IS_FALSE(session.agentWorkingDirectory.has_value());
        VERIFY_IS_FALSE(git.pending->request.firstPartyFields.contains("agentCwdActive"));

        broker.UpdateAgentWorkingDirectory(second, true, std::string{ "E:\\agent" });
        VERIFY_IS_TRUE(session.agentActive);
        VERIFY_IS_FALSE(session.agentWorkingDirectory.has_value());
        broker.Detach(1);
        VERIFY_IS_TRUE(session.agentActive);
        VERIFY_ARE_EQUAL(std::string{ "E:\\agent" }, *session.agentWorkingDirectory);
        broker.Detach(second);
        VERIFY_IS_FALSE(session.agentActive);
        VERIFY_IS_FALSE(session.agentWorkingDirectory.has_value());
    }

    void RichTabProviderTests::DetachedSessionDoesNotReuseOldGitMetadata()
    {
        ProviderBroker broker{ std::nullopt };
        Registration gitProvider;
        gitProvider.manifest.id = "com.microsoft.intelligent-terminal.git-status";
        gitProvider.manifest.fields = { FieldDeclaration{ "agentStatus", "Status" },
                                        FieldDeclaration{ "workingDirectory", "Directory" },
                                        FieldDeclaration{ "repository", "Repository" },
                                        FieldDeclaration{ "branch", "Branch" },
                                        FieldDeclaration{ "changes", "Changes" } };
        gitProvider.kind = RegistrationKind::Managed;
        gitProvider.origin = RegistrationOrigin::BuiltInCatalog;
        gitProvider.root = BuiltInProviderCatalog::PackageRoot() / L"RichTabProviders" / L"GitStatus";
        gitProvider.enabled = true;
        gitProvider.integrityValid = true;
        broker._providers = { gitProvider };

        auto& session = broker._sessions["detached-git"];
        session.context.sessionId = "detached-git";
        session.context.workingDirectory = L"C:\\shell";
        session.sessionIncarnation = 1;
        session.callbacks.emplace(1, [](const auto&) {});
        broker._attachmentSessions.emplace(1, session.context.sessionId);
        auto& snapshot = session.providers[gitProvider.manifest.id].snapshot.emplace();
        snapshot.fields.emplace("agentStatus", std::string{ "Idle" });
        snapshot.fields.emplace("workingDirectory", std::string{ "D:\\agent" });
        snapshot.fields.emplace("repository", std::string{ "old-repo" });
        snapshot.fields.emplace("branch", std::string{ "old-branch" });
        snapshot.fields.emplace("changes", std::string{ "2" });
        snapshot.tooltip = "Old repository";

        broker.Detach(1);
        VERIFY_ARE_EQUAL(size_t{ 1 }, snapshot.fields.size());
        VERIFY_IS_TRUE(snapshot.fields.contains("agentStatus"));
        VERIFY_IS_FALSE(snapshot.tooltip.has_value());

        broker._nextAttachment = 2;
        std::vector<BrokerUpdate> updates;
        const auto second = broker.Attach(session.context, [&](const auto& update) {
            updates.emplace_back(update);
        });
        VERIFY_ARE_EQUAL(ProviderBroker::AttachmentId{ 2 }, second);
        VERIFY_ARE_EQUAL(size_t{ 1 }, updates.size());
        VERIFY_IS_TRUE(updates.back().presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"Idle" }, updates.back().presentation->text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Idle" }, updates.back().presentation->tooltip);
    }

    void RichTabProviderTests::ResolveGitUsesExplicitPathAndDetectsMissingExecutable()
    {
        BEGIN_TEST_METHOD_PROPERTIES()
            TEST_METHOD_PROPERTY(L"IsolationLevel", L"Method")
        END_TEST_METHOD_PROPERTIES()

        const auto fixtureRoot = std::filesystem::temp_directory_path() /
                                 (L"RichTabResolveGit-" + std::to_wstring(GetCurrentProcessId()) +
                                  L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(fixtureRoot.is_absolute());
        VERIFY_IS_TRUE(std::filesystem::create_directory(fixtureRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(fixtureRoot, error);
        });
        const auto gitPath = fixtureRoot / L"git.exe";
        _WriteFile(gitPath, "fixture");

        SetLastError(ERROR_SUCCESS);
        const auto priorLength = GetEnvironmentVariableW(L"PATH", nullptr, 0);
        const auto priorError = GetLastError();
        const auto priorMissing = priorLength == 0 && priorError == ERROR_ENVVAR_NOT_FOUND;
        VERIFY_IS_TRUE(priorLength > 0 || priorMissing || priorError == ERROR_SUCCESS);
        std::wstring priorPath(priorLength, L'\0');
        if (priorLength > 0)
        {
            VERIFY_ARE_EQUAL(priorLength - 1, GetEnvironmentVariableW(L"PATH", priorPath.data(), priorLength));
            priorPath.resize(priorLength - 1);
        }
        const auto restorePath = wil::scope_exit([&]() {
            VERIFY_WIN32_BOOL_SUCCEEDED(SetEnvironmentVariableW(L"PATH", priorMissing ? nullptr : priorPath.c_str()));
        });
        VERIFY_WIN32_BOOL_SUCCEEDED(SetEnvironmentVariableW(L"PATH", fixtureRoot.c_str()));

        const auto resolved = CommandRunner::ResolveGit();
        VERIFY_IS_TRUE(resolved.has_value());
        VERIFY_ARE_EQUAL(gitPath.native(), resolved->native());
        VERIFY_IS_TRUE(std::filesystem::remove(gitPath));
        VERIFY_IS_FALSE(CommandRunner::ResolveGit().has_value());
    }

    void RichTabProviderTests::PowerShellProviderPreservesUnicodeAcrossProcessBoundary()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabUnicode-" + std::to_wstring(GetCurrentProcessId()) + L"-\u7EC8\u7AEF";
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/\xE4\xB8\xBB\xE5\x88\x86\xE6\x94\xAF\n");
        _WriteFile(repositoryRoot / L"\u6587\u4EF6.txt", "content\n");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = providerRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "unicode-provider";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace(
            "agentStatus",
            "\xE6\xAD\xA3\xE5\x9C\xA8\xE5\xB7\xA5\xE4\xBD\x9C");
        request.firstPartyFields.emplace("branchLabel", "\xE5\x88\x86\xE6\x94\xAF");
        request.firstPartyFields.emplace("changesLabel", "\xE6\x9B\xB4\xE6\x94\xB9");

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);
        VERIFY_IS_TRUE(command.processStarted);
        VERIFY_IS_TRUE(command.exitCodeValid);
        VERIFY_IS_FALSE(command.win32ErrorValid);
        VERIFY_IS_TRUE(command.outputLimitObserved);
        VERIFY_IS_FALSE(command.standardOutputExceeded);
        VERIFY_IS_FALSE(command.standardErrorExceeded);

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "\xE6\xAD\xA3\xE5\x9C\xA8\xE5\xB7\xA5\xE4\xBD\x9C" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            std::string{
                "RichTabUnicode-" + std::to_string(GetCurrentProcessId()) +
                "-\xE7\xBB\x88\xE7\xAB\xAF" },
            std::get<std::string>(parsed.value->fields.at("repository")));
        VERIFY_ARE_EQUAL(
            std::string{ "\xE4\xB8\xBB\xE5\x88\x86\xE6\x94\xAF" },
            std::get<std::string>(parsed.value->fields.at("branch")));
        VERIFY_IS_TRUE(parsed.value->tooltip.has_value());
        VERIFY_ARE_NOT_EQUAL(
            std::string::npos,
            parsed.value->tooltip->find("\xE5\x88\x86\xE6\x94\xAF: \xE4\xB8\xBB\xE5\x88\x86\xE6\x94\xAF"));
        VERIFY_ARE_EQUAL(std::string::npos, parsed.value->tooltip->find("branch:"));

        Registration registration;
        registration.manifest = manifest;
        ProviderBroker::FieldDisplayNameMap localizedDisplayNames;
        localizedDisplayNames[manifest.id] = {
            { "agentStatus", "\xE4\xBB\xA3\xE7\x90\x86\xE7\x8A\xB6\xE6\x80\x81" },
            { "workingDirectory", "\xE5\xB7\xA5\xE4\xBD\x9C\xE7\x9B\xAE\xE5\xBD\x95" },
            { "repository", "\xE4\xBB\x93\xE5\xBA\x93" },
            { "branch", "\xE5\x88\x86\xE6\x94\xAF" },
            { "changes", "\xE6\x9B\xB4\xE6\x94\xB9" },
        };
        const auto presentation = ProviderBroker::ComposePresentation(
            { std::move(registration) },
            { { manifest.id, *parsed.value } },
            {},
            localizedDisplayNames);
        VERIFY_IS_TRUE(presentation.has_value());
        VERIFY_ARE_NOT_EQUAL(
            std::wstring::npos,
            presentation->accessibilityText.find(L"\u4EE3\u7406\u72B6\u6001: \u6B63\u5728\u5DE5\u4F5C"));
        VERIFY_ARE_NOT_EQUAL(
            std::wstring::npos,
            presentation->accessibilityText.find(L"\u5DE5\u4F5C\u76EE\u5F55: "));
        VERIFY_ARE_EQUAL(std::wstring::npos, presentation->accessibilityText.find(L"Agent status"));
        VERIFY_ARE_EQUAL(std::wstring::npos, presentation->accessibilityText.find(L"Current working directory"));
    }

    void RichTabProviderTests::LocalizedFieldDisplayNamesOverrideManifestFallbacks()
    {
        Registration provider;
        provider.manifest.id = "git";
        provider.manifest.fields = {
            { "branch", "Git branch", FieldType::String, true },
            { "changes", "Git changes", FieldType::String, true },
        };

        Snapshot snapshot;
        snapshot.fields.emplace("branch", std::string{ "\xE4\xB8\xBB\xE5\x88\x86\xE6\x94\xAF" });
        snapshot.fields.emplace("changes", std::string{ "~12 +200 -35" });

        ProviderBroker::FieldDisplayNameMap localizedDisplayNames;
        localizedDisplayNames["git"] = {
            { "branch", "\xE5\x88\x86\xE6\x94\xAF" },
            { "changes", "\xE6\x9B\xB4\xE6\x94\xB9" },
        };

        const auto presentation = ProviderBroker::ComposePresentation(
            { provider },
            { { "git", std::move(snapshot) } },
            {},
            localizedDisplayNames);

        VERIFY_IS_TRUE(presentation.has_value());
        VERIFY_ARE_EQUAL(
            std::wstring{ L"\u5206\u652F: \u4E3B\u5206\u652F, \u66F4\u6539: ~12 +200 -35" },
            presentation->accessibilityText);
    }

    void RichTabProviderTests::BrokerClearsSuccessfulSnapshotAfterProviderFailure()
    {
        const auto providerRoot = std::filesystem::temp_directory_path() /
                                  (L"RichTabBrokerFailure-" + std::to_wstring(GetCurrentProcessId()) +
                                   L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(providerRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(providerRoot, error);
        });
        const auto script = providerRoot / L"provider.ps1";
        _WriteFile(
            script,
            "$request = [Console]::In.ReadToEnd() | ConvertFrom-Json\n"
            "[Console]::Error.WriteLine('broker-provider-warning')\n"
            "@{ protocolVersion = 1; requestId = $request.requestId; result = @{\n"
            "    fields = @{ branch = 'main' }; tooltip = 'Current branch: main'\n"
            "} } | ConvertTo-Json -Depth 4 -Compress\n");

        Registration provider;
        provider.manifest.id = "test.broker-failure";
        provider.manifest.runtime.kind = RuntimeKind::PowerShellV1;
        provider.manifest.runtime.entrypoint = L"provider.ps1";
        provider.manifest.extensionRoot = providerRoot;
        provider.manifest.activationEvents = { ActivationEvent::ManualRefresh };
        provider.manifest.fields = { { "branch", "Git branch", FieldType::String, true } };
        provider.enabled = true;
        provider.integrityValid = true;

        std::vector<BrokerUpdate> updates;
        ProviderBroker broker{ CommandRunner::ResolveGit() };
        broker._providers = { provider };
        auto& session = broker._sessions["broker-failure-session"];
        session.context.sessionId = "broker-failure-session";
        session.context.workingDirectory = providerRoot;
        session.context.workingDirectoryAuthoritative = true;
        session.context.shellType = "powershell";
        session.sessionIncarnation = 1;
        session.contextRevision = 7;
        session.callbacks.emplace(1, [&](const BrokerUpdate& update) {
            updates.emplace_back(update);
        });
        auto& state = session.providers[provider.manifest.id];
        state.generation = 1;
        state.runningGeneration = 1;
        state.running = true;

        Request request;
        request.requestId = "broker-success";
        request.providerId = provider.manifest.id;
        request.processEpoch = broker.ProcessEpoch();
        request.sessionId = session.context.sessionId;
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = session.context.workingDirectory;
        request.workingDirectoryAuthoritative = session.context.workingDirectoryAuthoritative;
        request.shellType = session.context.shellType;
        request.contextRevision = session.contextRevision;

        // Run synchronously with matching generations; no singleton or queued refreshes.
        broker._RunProvider(provider, request, 1);
        VERIFY_ARE_EQUAL(size_t{ 1 }, updates.size());
        VERIFY_IS_TRUE(updates[0].presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"main" }, updates[0].presentation->text);
        VERIFY_ARE_EQUAL(std::wstring{ L"Current branch: main" }, updates[0].presentation->tooltip);
        VERIFY_ARE_EQUAL(std::wstring{ L"Git branch: main" }, updates[0].presentation->accessibilityText);
        VERIFY_ARE_EQUAL(size_t{ 1 }, updates[0].diagnostics.size());
        VERIFY_ARE_NOT_EQUAL(
            std::string::npos,
            updates[0].diagnostics[0].find("broker-provider-warning"));
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, updates[0].updateSequence);
        VERIFY_IS_TRUE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.running);

        _WriteFile(
            script,
            "$null = [Console]::In.ReadToEnd()\n"
            "[Console]::Error.WriteLine('broker-provider-failure')\n"
            "exit 23\n");
        request.requestId = "broker-failure";
        state.generation = 2;
        state.runningGeneration = 2;
        state.running = true;
        broker._RunProvider(provider, request, 2);

        VERIFY_ARE_EQUAL(size_t{ 2 }, updates.size());
        VERIFY_ARE_EQUAL(request.sessionId, updates[1].sessionId);
        VERIFY_ARE_EQUAL(updates[0].sessionIncarnation, updates[1].sessionIncarnation);
        VERIFY_ARE_EQUAL(uint64_t{ 7 }, updates[0].contextRevision);
        VERIFY_ARE_EQUAL(updates[0].contextRevision, updates[1].contextRevision);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, updates[1].updateSequence);
        VERIFY_IS_FALSE(updates[1].presentation.has_value());
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.running);
        VERIFY_IS_FALSE(state.pending.has_value());
        VERIFY_ARE_EQUAL(size_t{ 2 }, updates[1].diagnostics.size());
        VERIFY_ARE_EQUAL(
            "Provider '" + provider.manifest.id + "' failed with status " +
                std::to_string(static_cast<int>(CommandResult::Status::Completed)) + " and exit code 23",
            updates[1].diagnostics[0]);
        VERIFY_ARE_NOT_EQUAL(
            std::string::npos,
            updates[1].diagnostics[1].find("broker-provider-failure"));
    }

    void RichTabProviderTests::BrokerKeepsAgentStatusPairedWithSnapshotDuringRefresh()
    {
        const auto providerRoot = _TestModuleDirectory() /
                                  (L"RichTabStatusRevision-" + std::to_wstring(GetCurrentProcessId()) +
                                   L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(providerRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(providerRoot, error);
        });
        const auto script = providerRoot / L"provider.ps1";
        _WriteFile(
            script,
            "$request = [Console]::In.ReadToEnd() | ConvertFrom-Json\n"
            "@{ protocolVersion = 1; requestId = $request.requestId; result = @{\n"
            "    fields = @{ agentStatus = $request.params.firstPartyFields.agentStatus }\n"
            "} } | ConvertTo-Json -Depth 4 -Compress\n");

        Registration provider;
        provider.manifest.id = "test.status-revision";
        provider.manifest.runtime.kind = RuntimeKind::PowerShellV1;
        provider.manifest.runtime.entrypoint = L"provider.ps1";
        provider.manifest.extensionRoot = providerRoot;
        provider.manifest.activationEvents = { ActivationEvent::ManualRefresh };
        provider.manifest.fields = { { "agentStatus", "Agent status", FieldType::String, true } };
        provider.enabled = true;
        provider.integrityValid = true;

        std::vector<BrokerUpdate> updates;
        ProviderBroker broker{ CommandRunner::ResolveGit() };
        broker._providers = { provider };
        auto& session = broker._sessions["status-revision-session"];
        session.context.sessionId = "status-revision-session";
        session.context.workingDirectory = providerRoot;
        session.context.firstPartyFields = { { "agentStatus", "Active" }, { "agentStatusId", "Working" } };
        session.sessionIncarnation = 1;
        session.callbacks.emplace(1, [&](const BrokerUpdate& update) {
            updates.emplace_back(update);
        });
        broker._attachmentSessions.emplace(1, session.context.sessionId);
        auto& state = session.providers[provider.manifest.id];
        state.generation = broker._nextGeneration++;
        state.runningGeneration = state.generation;
        state.running = true;

        Request request;
        request.requestId = "initial-status";
        request.providerId = provider.manifest.id;
        request.processEpoch = broker.ProcessEpoch();
        request.sessionId = session.context.sessionId;
        request.workingDirectory = providerRoot;
        request.firstPartyFields = session.context.firstPartyFields;
        broker._RunProvider(provider, request, state.generation);
        const auto verifyPair = [&](const std::wstring& text, const std::string& status) {
            VERIFY_IS_TRUE(updates.back().presentation.has_value());
            const auto& presentation = *updates.back().presentation;
            VERIFY_ARE_EQUAL(text, presentation.text);
            VERIFY_ARE_EQUAL(status, presentation.agentStatus);
            VERIFY_ARE_EQUAL(size_t{ 1 }, presentation.agentStatusRanges.size());
            VERIFY_ARE_EQUAL(size_t{ 0 }, presentation.agentStatusRanges[0].start);
            VERIFY_ARE_EQUAL(text.size(), presentation.agentStatusRanges[0].length);
        };
        verifyPair(L"Active", "Working");

        // Hold an older invocation open so the first-party change queues its refresh.
        state.running = true;
        const auto oldGeneration = state.runningGeneration;
        broker.UpdateFirstPartyFields(1, { { "agentStatus", "Failed" }, { "agentStatusId", "Error" } });
        VERIFY_ARE_EQUAL(size_t{ 2 }, updates.size());
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, updates.back().contextRevision);
        verifyPair(L"Active", "Working");
        VERIFY_IS_TRUE(state.pending.has_value());
        auto next = std::move(*state.pending);
        state.pending.reset();
        broker._RunProvider(provider, request, oldGeneration);
        VERIFY_ARE_EQUAL(size_t{ 2 }, updates.size());
        verifyPair(L"Active", "Working");

        state.running = true;
        state.runningGeneration = next.generation;
        broker._RunProvider(provider, next.request, next.generation);
        VERIFY_ARE_EQUAL(size_t{ 3 }, updates.size());
        verifyPair(L"Failed", "Error");

        state.running = true;
        broker.UpdateFirstPartyFields(1, { { "agentStatus", "Waiting" }, { "agentStatusId", "Attention" } });
        VERIFY_ARE_EQUAL(size_t{ 4 }, updates.size());
        verifyPair(L"Failed", "Error");
        VERIFY_IS_TRUE(state.pending.has_value());
        next = std::move(*state.pending);
        state.pending.reset();
        state.runningGeneration = next.generation;
        _WriteFile(script, "$null = [Console]::In.ReadToEnd()\nexit 23\n");
        broker._RunProvider(provider, next.request, next.generation);
        VERIFY_ARE_EQUAL(size_t{ 5 }, updates.size());
        VERIFY_IS_FALSE(updates.back().presentation.has_value());
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(updates.back().diagnostics.empty());
    }

    void RichTabProviderTests::BrokerClearsSnapshotAndSkipsExecutionWhenAllFieldsDeselected()
    {
        const auto providerRoot = std::filesystem::temp_directory_path() /
                                  (L"RichTabBrokerEmptySelection-" + std::to_wstring(GetCurrentProcessId()) +
                                   L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(providerRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(providerRoot, error);
        });
        const auto sentinel = providerRoot / L"invocations.txt";
        _WriteFile(
            providerRoot / L"provider.ps1",
            "$request = [Console]::In.ReadToEnd() | ConvertFrom-Json\n"
            "[IO.File]::AppendAllText((Join-Path $PSScriptRoot 'invocations.txt'), \"invoked`n\")\n"
            "@{ protocolVersion = 1; requestId = $request.requestId; result = @{\n"
            "    fields = @{ branch = 'main' }; tooltip = 'Current branch: main'\n"
            "} } | ConvertTo-Json -Depth 4 -Compress\n");
        const auto invocationContents = [&]() {
            std::ifstream stream{ sentinel, std::ios::binary };
            VERIFY_IS_TRUE(stream.is_open());
            return std::string{ std::istreambuf_iterator<char>{ stream }, std::istreambuf_iterator<char>{} };
        };

        Registration provider;
        provider.manifest.id = "test.broker-empty-selection";
        provider.manifest.runtime.kind = RuntimeKind::PowerShellV1;
        provider.manifest.runtime.entrypoint = L"provider.ps1";
        provider.manifest.extensionRoot = providerRoot;
        provider.manifest.activationEvents = { ActivationEvent::ManualRefresh };
        provider.manifest.fields = { { "branch", "Git branch", FieldType::String, true } };
        provider.enabled = true;
        provider.integrityValid = true;

        std::vector<BrokerUpdate> updates;
        ProviderBroker broker{ CommandRunner::ResolveGit() };
        // Stop idle workers before scheduling anything; drain actual broker work synchronously.
        {
            std::lock_guard lock{ broker._executorMutex };
            broker._executorStopping = true;
        }
        broker._executorCondition.notify_all();
        for (auto& worker : broker._executorWorkers)
        {
            worker.join();
        }
        broker._executorStopping = false;
        const auto takeWork = [&]() {
            VERIFY_ARE_EQUAL(size_t{ 1 }, broker._executorQueue.size());
            auto work = std::move(broker._executorQueue.front());
            broker._executorQueue.pop_front();
            return work;
        };

        broker._providers = { provider };
        broker._visibleFields[provider.manifest.id] = { "branch" };
        auto& session = broker._sessions["broker-empty-selection-session"];
        session.context.sessionId = "broker-empty-selection-session";
        session.context.workingDirectory = providerRoot;
        session.context.workingDirectoryAuthoritative = true;
        session.context.shellType = "powershell";
        session.sessionIncarnation = 1;
        session.contextRevision = 7;
        session.callbacks.emplace(1, [&](const BrokerUpdate& update) {
            updates.emplace_back(update);
        });
        broker._Refresh(session.context.sessionId, ActivationEvent::ManualRefresh, false);
        takeWork()();
        auto& state = session.providers.at(provider.manifest.id);
        VERIFY_ARE_EQUAL(size_t{ 1 }, updates.size());
        VERIFY_IS_TRUE(updates.back().presentation.has_value());
        VERIFY_ARE_EQUAL(std::wstring{ L"main" }, updates.back().presentation->text);
        VERIFY_IS_TRUE(updates.back().diagnostics.empty());
        VERIFY_IS_TRUE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.running);
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" }, invocationContents());

        const auto successfulGeneration = state.generation;
        broker.SetVisibleFields(provider.manifest.id, {});
        VERIFY_ARE_EQUAL(size_t{ 2 }, updates.size());
        VERIFY_IS_FALSE(updates.back().presentation.has_value());
        VERIFY_ARE_EQUAL(uint64_t{ 7 }, updates.back().contextRevision);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, updates.back().sessionIncarnation);
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, updates.back().updateSequence);
        VERIFY_IS_TRUE(updates.back().diagnostics.empty());
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.running);
        VERIFY_IS_FALSE(state.pending.has_value());
        VERIFY_IS_TRUE(state.generation > successfulGeneration);
        VERIFY_IS_TRUE(broker._executorQueue.empty());
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" }, invocationContents());

        broker.SetVisibleFields(provider.manifest.id, { "branch" });
        takeWork()();
        VERIFY_IS_TRUE(state.snapshot.has_value());
        VERIFY_IS_TRUE(updates.back().presentation.has_value());
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" "invoked\n" }, invocationContents());

        broker._Refresh(session.context.sessionId, ActivationEvent::ManualRefresh, false);
        auto staleWork = takeWork();
        const auto runningGeneration = state.runningGeneration;
        broker._Refresh(session.context.sessionId, ActivationEvent::ManualRefresh, false);
        VERIFY_IS_TRUE(state.running);
        VERIFY_IS_TRUE(state.pending.has_value());
        broker.SetVisibleFields(provider.manifest.id, {});
        VERIFY_ARE_EQUAL(size_t{ 5 }, updates.size());
        VERIFY_IS_FALSE(updates.back().presentation.has_value());
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.pending.has_value());
        VERIFY_IS_TRUE(state.running);
        VERIFY_IS_TRUE(state.generation > runningGeneration);
        VERIFY_ARE_EQUAL(runningGeneration, state.runningGeneration);
        VERIFY_IS_TRUE(broker._executorQueue.empty());
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" "invoked\n" }, invocationContents());

        // Only the previously dispatched invocation completes; it must not restore the snapshot.
        const auto updateCount = updates.size();
        const auto updateSequence = session.updateSequence;
        staleWork();
        VERIFY_IS_FALSE(state.running);
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.pending.has_value());
        VERIFY_ARE_EQUAL(updateCount, updates.size());
        VERIFY_ARE_EQUAL(updateSequence, session.updateSequence);
        VERIFY_ARE_EQUAL(uint64_t{ 7 }, session.contextRevision);
        VERIFY_IS_TRUE(broker._executorQueue.empty());
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" "invoked\n" "invoked\n" }, invocationContents());

        broker._Refresh(session.context.sessionId, ActivationEvent::ManualRefresh, false);
        VERIFY_IS_FALSE(state.running);
        VERIFY_IS_FALSE(state.snapshot.has_value());
        VERIFY_IS_FALSE(state.pending.has_value());
        VERIFY_IS_TRUE(broker._executorQueue.empty());
        VERIFY_ARE_EQUAL(std::string{ "invoked\n" "invoked\n" "invoked\n" }, invocationContents());
    }

    void RichTabProviderTests::BrokerInjectsImmutableGitAvailabilityIntoRequests()
    {
        const auto providerRoot = std::filesystem::temp_directory_path() /
                                  (L"RichTabBrokerGit-" + std::to_wstring(GetCurrentProcessId()) +
                                   L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(providerRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(providerRoot, error);
        });
        const auto gitBinary = providerRoot / L"\u7EC8\u7AEF-git.cmd";
        VERIFY_IS_TRUE(gitBinary.is_absolute());
        _WriteFile(gitBinary, "@echo off\r\nexit /b 0\r\n");
        _WriteFile(
            providerRoot / L"provider.ps1",
            "[Console]::InputEncoding = [Text.UTF8Encoding]::new($false)\n"
            "[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n"
            "$request = [Console]::In.ReadToEnd() | ConvertFrom-Json\n"
            "if (-not $request.params.firstPartyFields.PSObject.Properties['gitBinary']) { throw 'Missing gitBinary' }\n"
            "$binary = [string]$request.params.firstPartyFields.gitBinary\n"
            "$branch = if ($binary.Length) { $binary } else { '[missing]' }\n"
            "@{ protocolVersion = 1; requestId = $request.requestId; result = @{\n"
            "    fields = @{ branch = $branch }\n"
            "} } | ConvertTo-Json -Depth 4 -Compress\n");

        Registration provider;
        provider.manifest.id = "com.microsoft.intelligent-terminal.git-status";
        provider.kind = RegistrationKind::Managed;
        provider.origin = RegistrationOrigin::BuiltInCatalog;
        provider.root = BuiltInProviderCatalog::PackageRoot() / L"RichTabProviders" / L"GitStatus";
        provider.manifest.runtime.kind = RuntimeKind::PowerShellV1;
        provider.manifest.runtime.entrypoint = L"provider.ps1";
        provider.manifest.extensionRoot = providerRoot;
        provider.manifest.activationEvents = { ActivationEvent::ManualRefresh };
        provider.manifest.fields = { { "branch", "Git branch", FieldType::String, true } };
        provider.enabled = true;
        provider.integrityValid = true;
        provider.manifest.version = "1.0.0";

        const auto blockedLogDirectory = providerRoot / L"blocked-log-directory";
        _WriteFile(blockedLogDirectory, "not a directory");
        auto failedLogger = ProviderDiagnosticLog::CreateForTests(43, blockedLogDirectory);
        VERIFY_IS_NOT_NULL(failedLogger.get());
        const auto successfulLogDirectory = providerRoot / L"provider-logs";
        auto successfulLogger = ProviderDiagnosticLog::CreateForTests(44, successfulLogDirectory);
        VERIFY_IS_NOT_NULL(successfulLogger.get());

        size_t binaryIndex = 0;
        for (const auto& binary : { std::optional<std::filesystem::path>{}, std::optional<std::filesystem::path>{ gitBinary } })
        {
            std::vector<BrokerUpdate> updates;
            ProviderBroker broker{ binary };
            broker._diagnosticLog = binaryIndex++ == 0 ? failedLogger.get() : successfulLogger.get();
            VERIFY_ARE_EQUAL(binary.has_value(), broker.GitAvailable());
            const auto utf8 = binary ? binary->u8string() : std::u8string{};
            const std::string expected{ utf8.begin(), utf8.end() };
            broker._providers = { provider };
            auto& session = broker._sessions["broker-git-session"];
            session.context.sessionId = "broker-git-session";
            session.context.workingDirectory = providerRoot;
            session.context.workingDirectoryAuthoritative = true;
            session.sessionIncarnation = 1;
            session.contextRevision = 1;
            session.callbacks.emplace(1, [&](const BrokerUpdate& update) {
                updates.emplace_back(update);
            });
            auto& state = session.providers[provider.manifest.id];
            for (const auto spoofed : { "caller-override", "changed-override" })
            {
                session.context.firstPartyFields = { { "gitBinary", spoofed }, { "agentStatus", "idle" } };
                // Capture the real refresh request as pending work without racing executor threads.
                state.running = true;
                broker._Refresh(session.context.sessionId, ActivationEvent::ManualRefresh, false);
                VERIFY_IS_TRUE(state.pending.has_value());
                auto pending = std::move(*state.pending);
                state.pending.reset();
                VERIFY_ARE_EQUAL(expected, pending.request.firstPartyFields.at("gitBinary"));
                VERIFY_ARE_EQUAL(std::string{ "idle" }, pending.request.firstPartyFields.at("agentStatus"));
                state.runningGeneration = pending.generation;
                broker._RunProvider(provider, std::move(pending.request), pending.generation);
                VERIFY_IS_FALSE(state.running);
                VERIFY_IS_TRUE(broker._executorQueue.empty());
                VERIFY_IS_TRUE(state.snapshot.has_value());
                VERIFY_ARE_EQUAL(expected.empty() ? std::string{ "[missing]" } : expected,
                                 std::get<std::string>(state.snapshot->fields.at("branch")));
                VERIFY_IS_TRUE(updates.back().diagnostics.empty());
                if (binary && std::filesystem::exists(*binary))
                {
                    VERIFY_IS_TRUE(std::filesystem::remove(*binary));
                }
                VERIFY_ARE_EQUAL(binary.has_value(), broker.GitAvailable());
            }
            VERIFY_ARE_EQUAL(size_t{ 2 }, updates.size());
        }

        VERIFY_ARE_EQUAL(uint64_t{ 0 }, successfulLogger->DroppedRecords());
        successfulLogger.reset();
        std::vector<std::filesystem::path> logFiles;
        for (const auto& entry : std::filesystem::directory_iterator{ successfulLogDirectory })
        {
            if (entry.is_regular_file())
            {
                logFiles.emplace_back(entry.path());
            }
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, logFiles.size());
        std::ifstream logStream{ logFiles.front(), std::ios::binary };
        const std::string logContents{
            std::istreambuf_iterator<char>{ logStream },
            std::istreambuf_iterator<char>{}
        };
        VERIFY_ARE_NOT_EQUAL(std::string::npos, logContents.find(R"("brokerDisposition":"accepted")"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, logContents.find(R"("returnedFields":["branch"])"));
        const auto providerRootUtf8 = providerRoot.u8string();
        VERIFY_ARE_EQUAL(
            std::string::npos,
            logContents.find(std::string{ providerRootUtf8.begin(), providerRootUtf8.end() }));
        VERIFY_ARE_EQUAL(std::string::npos, logContents.find("caller-override"));
        VERIFY_ARE_EQUAL(std::string::npos, logContents.find("changed-override"));
    }

    void RichTabProviderTests::GitStatusProviderHandlesMissingGitGracefully()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabMissingGit-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/main\n");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = providerRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "missing-git-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "idle");
        request.firstPartyFields.emplace("gitBinary", (repositoryRoot / L"nonexistent-git.exe").string());

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "idle" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            repositoryRoot.string(),
            std::get<std::string>(parsed.value->fields.at("workingDirectory")));
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("branch") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("changes") == parsed.value->fields.end());
    }

    void RichTabProviderTests::GitStatusProviderUsesDisplayedDirectoryForGit()
    {
        const auto fixture = std::filesystem::temp_directory_path() /
                             (L"RichTabAgentGit-" + std::to_wstring(GetCurrentProcessId()) +
                              L"-" + std::to_wstring(GetTickCount64()));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(fixture, error);
        });
        const auto shellRoot = fixture / L"shell";
        const auto agentRoot = fixture / L"agent";
        for (const auto& [root, branch] : { std::pair{ shellRoot, "shell-branch" },
                                             std::pair{ agentRoot, "agent-branch" } })
        {
            const auto gitDirectory = root / L".git";
            std::filesystem::create_directories(gitDirectory / L"objects");
            std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
            std::filesystem::create_directories(gitDirectory / L"logs");
            _WriteFile(gitDirectory / L"config", "[core]\n\trepositoryformatversion = 0\n\tbare = false\n");
            _WriteFile(gitDirectory / L"HEAD", "ref: refs/heads/" + std::string{ branch } + "\n");
        }
        const auto gitScript = fixture / L"git-probe.ps1";
        const auto invocations = fixture / L"git-invocations.txt";
        _WriteFile(gitScript, R"(
$gitArg = $args | Where-Object { $_ -like '--git-dir=*' } | Select-Object -First 1
if (-not $gitArg) { throw 'Missing git directory' }
$gitDirectory = $gitArg.Substring('--git-dir='.Length)
if ($args -contains 'status') {
    [IO.File]::AppendAllText((Join-Path $PSScriptRoot 'git-invocations.txt'), "$gitDirectory`n")
    $branch = (Get-Content -LiteralPath (Join-Path $gitDirectory 'HEAD') -Raw).Trim().Split('/')[-1]
    "# branch.head $branch"
    if ($branch -eq 'agent-branch') { '? agent.txt'; '? second.txt' }
    else { '? shell.txt' }
    $global:LASTEXITCODE = 0
}
elseif ($args -contains 'config' -or $args -contains 'rev-parse') {
    $global:LASTEXITCODE = 1
}
else {
    $global:LASTEXITCODE = 0
}
)");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, true },
            { "branch", "Git branch", FieldType::String, true },
            { "changes", "Git changes", FieldType::String, true },
        };

        Request request;
        request.requestId = "agent-git-directory";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = shellRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("gitBinary", gitScript.string());
        request.firstPartyFields.emplace("agentCwdActive", "true");
        request.firstPartyFields.emplace("agentWorkingDirectory", agentRoot.string());

        const auto inspect = [&]() {
            const auto serialized = SerializeRequest(request, manifest);
            VERIFY_IS_TRUE(static_cast<bool>(serialized));
            const auto command = CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
            VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
            VERIFY_ARE_EQUAL(0u, command.exitCode);
            const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
            VERIFY_IS_TRUE(static_cast<bool>(parsed));
            return *parsed.value;
        };
        const auto field = [](const Snapshot& snapshot, const std::string& name) {
            return std::get<std::string>(snapshot.fields.at(name));
        };
        const auto agent = inspect();
        VERIFY_ARE_EQUAL(agentRoot.string(), field(agent, "workingDirectory"));
        VERIFY_ARE_EQUAL(std::string{ "agent" }, field(agent, "repository"));
        VERIFY_ARE_EQUAL(std::string{ "agent-branch" }, field(agent, "branch"));
        VERIFY_ARE_EQUAL(std::string{ "~2 +0 -0" }, field(agent, "changes"));
        VERIFY_IS_TRUE(agent.tooltip.has_value());

        request.workingDirectoryAuthoritative = false;
        const auto agentWithoutShellCwd = inspect();
        VERIFY_ARE_EQUAL(agentRoot.string(), field(agentWithoutShellCwd, "workingDirectory"));
        VERIFY_ARE_EQUAL(std::string{ "agent-branch" }, field(agentWithoutShellCwd, "branch"));
        request.workingDirectoryAuthoritative = true;
        const auto firstGitRun = std::filesystem::file_size(invocations);

        request.firstPartyFields.erase("agentWorkingDirectory");
        const auto unknown = inspect();
        VERIFY_IS_TRUE(unknown.fields.empty());
        VERIFY_ARE_EQUAL(firstGitRun, std::filesystem::file_size(invocations));

        request.firstPartyFields.emplace("agentWorkingDirectory", "\\\\server\\share");
        const auto remote = inspect();
        VERIFY_ARE_EQUAL(std::string{ "\\\\server\\share" }, field(remote, "workingDirectory"));
        VERIFY_IS_TRUE(remote.fields.find("repository") == remote.fields.end());
        VERIFY_ARE_EQUAL(firstGitRun, std::filesystem::file_size(invocations));

        request.firstPartyFields.erase("agentCwdActive");
        const auto shell = inspect();
        VERIFY_ARE_EQUAL(shellRoot.string(), field(shell, "workingDirectory"));
        VERIFY_ARE_EQUAL(std::string{ "shell" }, field(shell, "repository"));
        VERIFY_ARE_EQUAL(std::string{ "shell-branch" }, field(shell, "branch"));
        VERIFY_ARE_EQUAL(std::string{ "~1 +0 -0" }, field(shell, "changes"));
    }

    void RichTabProviderTests::GitStatusProviderKnownMissingGitDoesNotFallBack()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabKnownMissingGit-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/main\n");

        wchar_t systemDirectory[MAX_PATH]{};
        const auto length = GetSystemDirectoryW(systemDirectory, MAX_PATH);
        VERIFY_IS_TRUE(length > 0 && length < MAX_PATH);
        std::filesystem::copy_file(std::filesystem::path{ systemDirectory } / L"cmd.exe", repositoryRoot / L"git.exe");
        _WriteFile(repositoryRoot / L"recording-git.ps1",
                   "$global:LASTEXITCODE = 0\n"
                   "if ($args -contains 'status') { '# branch.head fallback-branch' }\n");
        std::filesystem::copy_file(providerRoot / L"provider.ps1", repositoryRoot / L"original-provider.ps1");
        _WriteFile(repositoryRoot / L"provider.ps1", R"(
$env:PATH = "$PSScriptRoot;$env:PATH"
if (-not (Microsoft.PowerShell.Core\Get-Command git.exe -CommandType Application -ErrorAction Stop)) {
    throw 'Expected Git application on child PATH'
}
function Get-Command {
    param($Name, $CommandType, $ErrorAction)
    if ($Name -eq 'git.exe') {
        [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'fallback.txt'), 'discovered')
        [pscustomobject]@{ Source = (Join-Path $PSScriptRoot 'recording-git.ps1') }
    }
    else {
        Microsoft.PowerShell.Core\Get-Command $Name -CommandType $CommandType -ErrorAction $ErrorAction
    }
}
. (Join-Path $PSScriptRoot 'original-provider.ps1')
)");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = repositoryRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "known-missing-git-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "idle");
        request.firstPartyFields.emplace("gitBinary", "");

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "idle" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            repositoryRoot.string(),
            std::get<std::string>(parsed.value->fields.at("workingDirectory")));
        VERIFY_IS_FALSE(std::filesystem::exists(repositoryRoot / L"fallback.txt"));
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("branch") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("changes") == parsed.value->fields.end());

        // An absent parameter retains standalone discovery; this also proves the probe is live.
        request.firstPartyFields.erase("gitBinary");
        const auto legacyRequest = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(legacyRequest));
        const auto legacy = CommandRunner{}.Run(manifest, *legacyRequest.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(legacy.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, legacy.exitCode);
        VERIFY_IS_TRUE(std::filesystem::exists(repositoryRoot / L"fallback.txt"));
        const auto legacySnapshot = ParseSnapshot(legacy.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(legacySnapshot));
        VERIFY_ARE_EQUAL(std::string{ "fallback-branch" }, std::get<std::string>(legacySnapshot.value->fields.at("branch")));
    }

    void RichTabProviderTests::GitStatusProviderInvokesLiteralApplicationPath()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryRoot = std::filesystem::temp_directory_path() /
                                    (L"RichTabLiteralGit-" + std::to_wstring(GetCurrentProcessId()) +
                                     L"-" + std::to_wstring(GetTickCount64()));
        VERIFY_IS_TRUE(std::filesystem::create_directory(repositoryRoot));
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });
        std::filesystem::create_directory(repositoryRoot / L".git");
        _WriteFile(repositoryRoot / L".git" / L"HEAD", "ref: refs/heads/literal-branch\n");
        const auto literalGit = repositoryRoot / L"git[1].cmd";
        const auto alternateGit = repositoryRoot / L"git1.cmd";
        const auto invocationsFile = repositoryRoot / L"invocations.txt";
        const std::string fakeGit =
            "@echo off\r\necho invoked \"%~f0\" %* >> \"" + invocationsFile.string() +
            "\"\r\n:next\r\nif \"%~1\"==\"\" exit /b 0\r\n"
            "if \"%~1\"==\"status\" echo # branch.head literal-branch\r\n"
            "shift\r\ngoto next\r\n";
        _WriteFile(literalGit, fakeGit);
        _WriteFile(alternateGit, fakeGit);
        // Windows PowerShell passes the filter regexp's pipe through cmd.exe.
        _WriteFile(repositoryRoot / L"process)$.cmd", "@echo off\r\nexit /b 0\r\n");
        std::filesystem::copy_file(providerRoot / L"provider.ps1", repositoryRoot / L"original-provider.ps1");
        _WriteFile(repositoryRoot / L"provider.ps1", R"(
$env:PATH = "$PSScriptRoot;$env:PATH"
function Get-Command {
    [IO.File]::WriteAllText((Join-Path $PSScriptRoot 'discovery.txt'), 'unexpected discovery')
    Microsoft.PowerShell.Core\Get-Command @args
}
. (Join-Path $PSScriptRoot 'original-provider.ps1')
)");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = repositoryRoot;
        manifest.activationEvents = { ActivationEvent::ManualRefresh };
        manifest.fields = {
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "literal-git-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.visibleFields = { "repository", "branch" };
        request.firstPartyFields.emplace("gitBinary", literalGit.string());

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));
        const auto command = CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);
        VERIFY_IS_TRUE(command.standardError.empty());
        VERIFY_IS_FALSE(std::filesystem::exists(repositoryRoot / L"discovery.txt"));
        std::ifstream invocations{ invocationsFile };
        VERIFY_IS_TRUE(invocations.is_open());
        const std::string recorded{ std::istreambuf_iterator<char>{ invocations }, std::istreambuf_iterator<char>{} };
        VERIFY_ARE_NOT_EQUAL(std::string::npos, recorded.find("status"));
        VERIFY_ARE_NOT_EQUAL(std::string::npos, recorded.find(literalGit.string()));
        VERIFY_ARE_EQUAL(std::string::npos, recorded.find(alternateGit.string()));
        invocations.close();
        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(repositoryRoot.filename().string(), std::get<std::string>(parsed.value->fields.at("repository")));
        VERIFY_ARE_EQUAL(std::string{ "literal-branch" }, std::get<std::string>(parsed.value->fields.at("branch")));

        VERIFY_IS_TRUE(std::filesystem::remove(literalGit));
        VERIFY_IS_TRUE(std::filesystem::remove(invocationsFile));
        request.requestId = "missing-literal-git-test";
        const auto missingRequest = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(missingRequest));
        const auto missing = CommandRunner{}.Run(manifest, *missingRequest.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(missing.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, missing.exitCode);
        VERIFY_IS_TRUE(missing.standardError.empty());
        VERIFY_IS_FALSE(std::filesystem::exists(invocationsFile));
        VERIFY_IS_FALSE(std::filesystem::exists(repositoryRoot / L"discovery.txt"));
        const auto missingSnapshot = ParseSnapshot(missing.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(missingSnapshot));
        VERIFY_ARE_EQUAL(repositoryRoot.string(), std::get<std::string>(missingSnapshot.value->fields.at("workingDirectory")));
        VERIFY_IS_TRUE(missingSnapshot.value->fields.find("repository") == missingSnapshot.value->fields.end());
        VERIFY_IS_TRUE(missingSnapshot.value->fields.find("branch") == missingSnapshot.value->fields.end());
        VERIFY_IS_TRUE(missingSnapshot.value->fields.find("changes") == missingSnapshot.value->fields.end());
    }

    void RichTabProviderTests::GitStatusProviderSkipsGitWhenNoGitFieldsVisible()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabNoGitFields-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        const auto sentinelFile = repositoryRoot / L"sentinel.txt";
        const auto recordingGitScript = repositoryRoot / L"recording-git.cmd";
        std::filesystem::create_directories(repositoryRoot);
        _WriteFile(recordingGitScript, "@echo off\r\necho invoked > \"" + sentinelFile.string() + "\"\r\nexit /b 1\r\n");

        const auto probeFile = repositoryRoot / L"probes.txt";
        std::filesystem::copy_file(providerRoot / L"provider.ps1", repositoryRoot / L"original-provider.ps1");
        _WriteFile(repositoryRoot / L"provider.ps1", R"(
$probeFile = Join-Path $PSScriptRoot 'probes.txt'
function Get-PSDrive {
    [IO.File]::AppendAllText($probeFile, "Get-PSDrive`n")
    Microsoft.PowerShell.Management\Get-PSDrive @args
}
function Test-Path {
    [IO.File]::AppendAllText($probeFile, "Test-Path`n")
    Microsoft.PowerShell.Management\Test-Path @args
}
function Get-Item {
    [IO.File]::AppendAllText($probeFile, "Get-Item`n")
    Microsoft.PowerShell.Management\Get-Item @args
}
function Get-ChildItem {
    [IO.File]::AppendAllText($probeFile, "Get-ChildItem`n")
    Microsoft.PowerShell.Management\Get-ChildItem @args
}
function Get-Command {
    [IO.File]::AppendAllText($probeFile, "Get-Command $($args[0])`n")
    Microsoft.PowerShell.Core\Get-Command @args
}
. (Join-Path $PSScriptRoot 'original-provider.ps1')
)");

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/main\n");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = providerRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "no-git-fields-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "working");
        request.visibleFields = { "workingDirectory", "agentStatus" };
        request.firstPartyFields.emplace("gitBinary", recordingGitScript.string());

        manifest.extensionRoot = repositoryRoot;
        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);

        VERIFY_IS_FALSE(std::filesystem::exists(probeFile));
        VERIFY_IS_FALSE(std::filesystem::exists(sentinelFile));

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "working" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            repositoryRoot.string(),
            std::get<std::string>(parsed.value->fields.at("workingDirectory")));
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("branch") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("changes") == parsed.value->fields.end());
        VERIFY_ARE_EQUAL(size_t{ 2 }, parsed.value->fields.size());
        VERIFY_IS_FALSE(parsed.value->tooltip.has_value());

        // Positive controls prove the wrappers observe discovery, including legacy empty visibility.
        for (const auto& visibleFields : std::vector<std::vector<std::string>>{ { "branch" }, {} })
        {
            request.visibleFields = visibleFields;
            const auto controlRequest = SerializeRequest(request, manifest);
            VERIFY_IS_TRUE(static_cast<bool>(controlRequest));
            const auto control =
                CommandRunner{}.Run(manifest, *controlRequest.value, std::chrono::seconds{ 10 });
            VERIFY_IS_TRUE(control.status == CommandResult::Status::Completed);
            VERIFY_ARE_EQUAL(0u, control.exitCode);
            VERIFY_IS_TRUE(static_cast<bool>(ParseSnapshot(control.standardOutput, manifest, request.requestId)));
            VERIFY_IS_TRUE(std::filesystem::exists(sentinelFile));

            std::ifstream probes{ probeFile };
            VERIFY_IS_TRUE(probes.is_open());
            const std::string recorded{ std::istreambuf_iterator<char>{ probes }, std::istreambuf_iterator<char>{} };
            for (const auto probe : { "Get-PSDrive\n", "Test-Path\n", "Get-Item\n", "Get-ChildItem\n" })
            {
                VERIFY_ARE_NOT_EQUAL(std::string::npos, recorded.find(probe));
            }
            VERIFY_ARE_EQUAL(std::string::npos, recorded.find("Get-Command "));
            probes.close();
            VERIFY_IS_TRUE(std::filesystem::remove(probeFile));
            VERIFY_IS_TRUE(std::filesystem::remove(sentinelFile));
        }
    }

    void RichTabProviderTests::GitStatusProviderHandlesGitFailureGracefully()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabGitFail-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        // Create a failing script to simulate git failure (exit code 1)
        const auto failingGitScript = repositoryRoot / L"failing-git.cmd";
        std::filesystem::create_directories(repositoryRoot);
        _WriteFile(failingGitScript, "@echo off\r\nexit /b 1\r\n");

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/main\n");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = providerRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "git-fail-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "busy");
        request.firstPartyFields.emplace("gitBinary", failingGitScript.string());
        request.visibleFields = { "repository", "branch", "changes", "workingDirectory" };

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);
        VERIFY_ARE_NOT_EQUAL(
            std::string::npos,
            command.standardError.find("Git status inspection failed:"));

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "busy" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            repositoryRoot.string(),
            std::get<std::string>(parsed.value->fields.at("workingDirectory")));
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
    }

    void RichTabProviderTests::GitStatusProviderSetsSafeGitConfigEnvironment()
    {
        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabGitEnv-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        // Create a fake git script that records GIT_CONFIG_GLOBAL and GIT_CONFIG_SYSTEM
        const auto recordFile = repositoryRoot / L"git-env.txt";
        const auto recordingGitScript = repositoryRoot / L"recording-git.cmd";
        std::filesystem::create_directories(repositoryRoot);
        _WriteFile(
            recordingGitScript,
            "@echo off\r\n"
            "echo GLOBAL=%GIT_CONFIG_GLOBAL%> \"" + recordFile.string() + "\"\r\n"
            "echo SYSTEM=%GIT_CONFIG_SYSTEM%>> \"" + recordFile.string() + "\"\r\n"
            "exit /b 1\r\n");

        const auto gitDirectory = repositoryRoot / L".git";
        std::filesystem::create_directories(gitDirectory / L"objects");
        std::filesystem::create_directories(gitDirectory / L"refs" / L"heads");
        std::filesystem::create_directories(gitDirectory / L"logs");
        _WriteFile(
            gitDirectory / L"config",
            "[core]\n\t"
            "repository"
            "format"
            "version = 0\n\tbare = false\n");
        _WriteFile(
            gitDirectory / L"HEAD",
            "ref: refs/heads/main\n");

        Manifest manifest;
        manifest.id = "com.microsoft.intelligent-terminal.git-status";
        manifest.runtime.kind = RuntimeKind::PowerShellV1;
        manifest.runtime.entrypoint = L"provider.ps1";
        manifest.extensionRoot = providerRoot;
        manifest.activationEvents.emplace_back(ActivationEvent::ManualRefresh);
        manifest.fields = {
            { "agentStatus", "Agent status", FieldType::String, true },
            { "workingDirectory", "Current working directory", FieldType::String, true },
            { "repository", "Git repo", FieldType::String, false },
            { "branch", "Git branch", FieldType::String, false },
            { "changes", "Git changes", FieldType::String, false },
        };

        Request request;
        request.requestId = "git-env-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "ready");
        request.firstPartyFields.emplace("gitBinary", recordingGitScript.string());
        request.visibleFields = { "repository", "branch", "changes", "workingDirectory" };

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);

        // Verify that the environment variables were set to /dev/null
        VERIFY_IS_TRUE(std::filesystem::exists(recordFile));
        std::ifstream stream{ recordFile };
        std::string line;
        bool globalVerified = false;
        bool systemVerified = false;
        while (std::getline(stream, line))
        {
            if (line.find("GLOBAL=/dev/null") != std::string::npos)
            {
                globalVerified = true;
            }
            if (line.find("SYSTEM=/dev/null") != std::string::npos)
            {
                systemVerified = true;
            }
        }
        VERIFY_IS_TRUE(globalVerified);
        VERIFY_IS_TRUE(systemVerified);
    }
}
