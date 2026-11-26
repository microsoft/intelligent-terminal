// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <windows.h>
#include <array>
#include <string>
#include <string_view>
#include <vector>

namespace Microsoft::Terminal::Settings::Model::AcpAgentCatalog
{
    // Rebuilding an already-open Agents page (reload/reset) is not page entry.
    // Keep this side-effect boundary shared with the countable lifecycle tests.
    template<typename Refresh>
    void RefreshOnAgentsPageEntry(const bool wasAgentsPage, const Refresh& refresh)
    {
        if (!wasAgentsPage)
        {
            refresh();
        }
    }

    // Bundled metadata, not a remote feed or a provider installer.
    // Executables/arguments: pinned registry manifests and exact-version npm
    // package.json bin metadata; see doc/specs/acp-registry-agents.md.
    inline constexpr std::wstring_view Revision{ L"2a27ff18b5816f47d7ec538e22943ff194e05131" };

#if defined(_M_ARM64)
    inline constexpr std::wstring_view PoolsideArchiveExecutable{ L"pool-windows-arm64" };
#else
    inline constexpr std::wstring_view PoolsideArchiveExecutable{ L"pool-windows-amd64" };
#endif

    struct Agent
    {
        std::wstring_view id;
        std::wstring_view name;
        std::wstring_view executable;
        std::wstring_view arguments;
        std::wstring_view dependency{};
        std::wstring_view alternateExecutable{};
    };

    // Existing built-ins keep their existing discovery/auth/launch behavior.
    // Entries needing distribution-specific environment setup stay manual.
    // Never use npx, uvx, node or python as evidence that an agent is installed.
    inline constexpr std::array Agents{
        Agent{ L"amp-acp", L"Amp", L"amp-acp", L"", L"amp" },
        Agent{ L"autohand", L"Autohand Code", L"autohand", L"--acp" },
        Agent{ L"bob", L"Bob", L"bob", L"acp" },
        Agent{ L"cline", L"Cline", L"cline", L"--acp" },
        Agent{ L"codebuddy-code", L"Codebuddy Code", L"codebuddy-code", L"--acp" },
        Agent{ L"cortex-code", L"Cortex Code", L"cortex", L"acp serve" },
        Agent{ L"corust-agent", L"Corust Agent", L"corust-agent-acp", L"" },
        Agent{ L"crow-cli", L"crow-cli", L"crow-cli", L"acp" },
        Agent{ L"cursor", L"Cursor", L"cursor-agent", L"acp" },
        Agent{ L"devin", L"Devin", L"devin", L"acp" },
        Agent{ L"dimcode", L"DimCode", L"dimcode", L"acp" },
        Agent{ L"dirac", L"Dirac", L"dirac", L"--acp" },
        Agent{ L"github-copilot", L"GitHub Copilot Language Server", L"copilot-language-server", L"--acp" },
        Agent{ L"glm-acp-agent", L"GLM Agent", L"glm-acp-agent", L"" },
        Agent{ L"goose", L"goose", L"goose", L"acp" },
        Agent{ L"grok-build", L"Grok Build", L"grok", L"agent stdio" },
        Agent{ L"harn", L"Harn", L"harn", L"serve acp" },
        Agent{ L"junie", L"Junie", L"junie", L"--acp=true" },
        Agent{ L"kilo", L"Kilo", L"kilo", L"acp" },
        Agent{ L"kimchi", L"Kimchi", L"kimchi", L"--mode acp" },
        // Both Kimi distributions expose the same ACP invocation: one choice,
        // without merging or claiming anything about their native histories.
        Agent{ L"kimi-code", L"Kimi Code", L"kimi", L"acp" },
        Agent{ L"minimax-code", L"MiniMax Code", L"code-acp", L"acp" },
        Agent{ L"mistral-vibe", L"Mistral Vibe", L"vibe-acp", L"" },
        Agent{ L"nova", L"Nova", L"nova", L"acp" },
        Agent{ L"pi-acp", L"pi ACP", L"pi-acp", L"", L"pi" },
        Agent{ L"poolside", L"Poolside", L"pool", L"acp", L"", PoolsideArchiveExecutable },
        Agent{ L"qoder", L"Qoder CLI", L"qodercli", L"--acp" },
        Agent{ L"qwen-code", L"Qwen Code", L"qwen", L"--acp --experimental-skills" },
        // The exact-version README requires --acp; the registry omits it.
        Agent{ L"sigit", L"siGit Code", L"sigit", L"--acp" },
        Agent{ L"stakpak", L"Stakpak", L"stakpak", L"acp" },
    };

    inline constexpr std::wstring_view IdPrefix{ L"custom:acp-registry:" };

    // Namespace ownership does not depend on this build's metadata revision.
    // A newer/removed ID remains a catalog selection, never a legacy command.
    inline bool IsCatalogId(const std::wstring_view id) noexcept
    {
        return id.starts_with(IdPrefix);
    }

    inline std::wstring Id(const Agent& agent)
    {
        // A separate namespace avoids claiming legacy executable-derived IDs
        // such as custom:acp-registry-bob for handwritten wrapper commands.
        return std::wstring{ IdPrefix } + std::wstring{ agent.id };
    }

    inline const Agent* Find(const std::wstring_view id) noexcept
    {
        if (IsCatalogId(id))
        {
            for (const auto& agent : Agents)
            {
                if (agent.id == id.substr(IdPrefix.size()))
                {
                    return &agent;
                }
            }
        }
        return nullptr;
    }

    inline std::wstring Executable(const std::wstring_view name, const wchar_t* searchPath = nullptr)
    {
        // Search configured PATH only, not SearchPathW's implicit application /
        // current-directory search. A workspace file is not an installed agent.
        std::wstring environmentPath;
        if (!searchPath)
        {
            const auto required = GetEnvironmentVariableW(L"PATH", nullptr, 0);
            if (!required)
            {
                return {};
            }
            environmentPath.resize(required);
            const auto length = GetEnvironmentVariableW(L"PATH", environmentPath.data(), required);
            if (!length || length >= required)
            {
                return {};
            }
            environmentPath.resize(length);
            searchPath = environmentPath.c_str();
        }
        // Empty PATH components also make SearchPathW search the workspace.
        // Match native executable discovery: ignore them and strip path quotes.
        std::wstring paths;
        const std::wstring_view configuredPath{ searchPath };
        for (size_t start = 0; start < configuredPath.size();)
        {
            const auto end = configuredPath.find(L';', start);
            auto directory = configuredPath.substr(start, end == std::wstring_view::npos ? end : end - start);
            if (directory.size() >= 2 && directory.front() == L'"' && directory.back() == L'"')
            {
                directory = directory.substr(1, directory.size() - 2);
            }
            if (!directory.empty())
            {
                if (!paths.empty())
                {
                    paths.push_back(L';');
                }
                paths.append(directory);
            }
            if (end == std::wstring_view::npos)
            {
                break;
            }
            start = end + 1;
        }
        if (paths.empty())
        {
            return {};
        }
        // Never run --version, a package manager, a provider, or a hook probe.
        for (const auto extension : { L".exe", L".cmd" })
        {
            const auto leaf = std::wstring{ name } + extension;
            std::wstring path(32768, L'\0');
            const auto length = SearchPathW(paths.c_str(), leaf.c_str(), nullptr, static_cast<DWORD>(path.size()), path.data(), nullptr);
            if (length && length < path.size())
            {
                path.resize(length);
                const auto attributes = GetFileAttributesW(path.c_str());
                if (attributes != INVALID_FILE_ATTRIBUTES && !(attributes & FILE_ATTRIBUTE_DIRECTORY))
                {
                    return path;
                }
            }
        }
        return {};
    }

    inline std::wstring Command(const Agent& agent, const wchar_t* searchPath = nullptr)
    {
        auto executable = Executable(agent.executable, searchPath);
        if (executable.empty() && !agent.alternateExecutable.empty())
        {
            executable = Executable(agent.alternateExecutable, searchPath);
        }
        if (executable.empty() ||
            (!agent.dependency.empty() && Executable(agent.dependency, searchPath).empty()))
        {
            return {};
        }
        auto command = L"\"" + executable + L"\"";
        if (!agent.arguments.empty())
        {
            command += L" " + std::wstring{ agent.arguments };
        }
        return command;
    }

    struct InstalledAgent
    {
        const Agent* metadata;
        std::wstring command;
    };

    inline std::vector<InstalledAgent> Discover(const wchar_t* searchPath = nullptr)
    {
        std::vector<InstalledAgent> installed;
        for (const auto& agent : Agents)
        {
            if (auto command = Command(agent, searchPath); !command.empty())
            {
                installed.push_back({ &agent, std::move(command) });
            }
        }
        return installed;
    }

    inline std::wstring IconPath(const Agent& agent, const bool mask = false)
    {
        const auto icon = agent.id == L"github-copilot" ? L"copilot" : agent.id;
        return std::wstring{ L"ms-appx:///AgentIcons/" } +
               (agent.id == L"github-copilot" ? L"" : L"Registry/") +
               (mask ? L"Masks/" : L"") + std::wstring{ icon } + (mask ? L".png" : L".svg");
    }
}
