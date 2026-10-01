// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../RichTabProvider/CommandRunner.h"
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
        TEST_METHOD(GitStatusProviderHandlesMissingGitGracefully);
        TEST_METHOD(GitStatusProviderHonorsGitBinaryEnvironmentVariable);
        TEST_METHOD(GitStatusProviderSkipsGitWhenNoGitFieldsVisible);
        TEST_METHOD(GitStatusProviderHandlesGitFailureGracefully);
    };

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

    void RichTabProviderTests::GitStatusProviderHandlesMissingGitGracefully()
    {
        constexpr auto gitBinaryEnv = L"INTELLIGENT_TERMINAL_GIT_BINARY";
        SetLastError(ERROR_SUCCESS);
        const auto priorLength = GetEnvironmentVariableW(gitBinaryEnv, nullptr, 0);
        const auto priorMissing = priorLength == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND;
        std::wstring priorValue;
        if (!priorMissing && priorLength > 0)
        {
            priorValue.resize(priorLength);
            GetEnvironmentVariableW(gitBinaryEnv, priorValue.data(), priorLength);
            priorValue.resize(wcslen(priorValue.c_str()));
        }
        SetEnvironmentVariableW(gitBinaryEnv, nullptr);
        const auto envCleanup = wil::scope_exit([=]() {
            SetEnvironmentVariableW(gitBinaryEnv, priorMissing ? nullptr : priorValue.c_str());
        });

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
        request.firstPartyFields.emplace("gitBinary", "nonexistent-git-binary.exe");

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

    void RichTabProviderTests::GitStatusProviderHonorsGitBinaryEnvironmentVariable()
    {
        constexpr auto gitBinaryEnv = L"INTELLIGENT_TERMINAL_GIT_BINARY";
        SetLastError(ERROR_SUCCESS);
        const auto priorLength = GetEnvironmentVariableW(gitBinaryEnv, nullptr, 0);
        const auto priorMissing = priorLength == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND;
        std::wstring priorValue;
        if (!priorMissing && priorLength > 0)
        {
            priorValue.resize(priorLength);
            GetEnvironmentVariableW(gitBinaryEnv, priorValue.data(), priorLength);
            priorValue.resize(wcslen(priorValue.c_str()));
        }

        const auto providerRoot = _TestModuleDirectory() / L"RichTabProviders" / L"GitStatus";
        const auto repositoryName =
            L"RichTabEnvGit-" + std::to_wstring(GetCurrentProcessId());
        const auto repositoryRoot = std::filesystem::temp_directory_path() / repositoryName;
        std::filesystem::remove_all(repositoryRoot);
        const auto cleanup = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(repositoryRoot, error);
        });

        // Set environment variable to a nonexistent binary to prove the environment block passes it to provider.ps1
        SetEnvironmentVariableW(gitBinaryEnv, L"nonexistent-env-git.exe");
        const auto envCleanup = wil::scope_exit([=]() {
            SetEnvironmentVariableW(gitBinaryEnv, priorMissing ? nullptr : priorValue.c_str());
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
        request.requestId = "env-git-test";
        request.providerId = manifest.id;
        request.processEpoch = 1;
        request.sessionId = "session";
        request.reason = ActivationEvent::ManualRefresh;
        request.workingDirectory = repositoryRoot;
        request.workingDirectoryAuthoritative = true;
        request.firstPartyFields.emplace("agentStatus", "idle");
        // No firstPartyFields gitBinary set, so it relies on the environment variable override

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
        // Because INTELLIGENT_TERMINAL_GIT_BINARY was passed through the sanitized environment block,
        // provider.ps1 used nonexistent-env-git.exe and returned empty git fields without failing.
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("branch") == parsed.value->fields.end());
        VERIFY_IS_TRUE(parsed.value->fields.find("changes") == parsed.value->fields.end());
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
        request.firstPartyFields.emplace("gitBinary", "broken-git-binary.exe");

        const auto serialized = SerializeRequest(request, manifest);
        VERIFY_IS_TRUE(static_cast<bool>(serialized));

        const auto command =
            CommandRunner{}.Run(manifest, *serialized.value, std::chrono::seconds{ 10 });
        VERIFY_IS_TRUE(command.status == CommandResult::Status::Completed);
        VERIFY_ARE_EQUAL(0u, command.exitCode);

        const auto parsed = ParseSnapshot(command.standardOutput, manifest, request.requestId);
        VERIFY_IS_TRUE(static_cast<bool>(parsed));
        VERIFY_ARE_EQUAL(
            std::string{ "working" },
            std::get<std::string>(parsed.value->fields.at("agentStatus")));
        VERIFY_ARE_EQUAL(
            repositoryRoot.string(),
            std::get<std::string>(parsed.value->fields.at("workingDirectory")));
        VERIFY_IS_TRUE(parsed.value->fields.find("repository") == parsed.value->fields.end());
    }

    void RichTabProviderTests::GitStatusProviderHandlesGitFailureGracefully()
    {
        constexpr auto gitBinaryEnv = L"INTELLIGENT_TERMINAL_GIT_BINARY";
        SetLastError(ERROR_SUCCESS);
        const auto priorLength = GetEnvironmentVariableW(gitBinaryEnv, nullptr, 0);
        const auto priorMissing = priorLength == 0 && GetLastError() == ERROR_ENVVAR_NOT_FOUND;
        std::wstring priorValue;
        if (!priorMissing && priorLength > 0)
        {
            priorValue.resize(priorLength);
            GetEnvironmentVariableW(gitBinaryEnv, priorValue.data(), priorLength);
            priorValue.resize(wcslen(priorValue.c_str()));
        }
        SetEnvironmentVariableW(gitBinaryEnv, nullptr);
        const auto envCleanup = wil::scope_exit([=]() {
            SetEnvironmentVariableW(gitBinaryEnv, priorMissing ? nullptr : priorValue.c_str());
        });

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
}
