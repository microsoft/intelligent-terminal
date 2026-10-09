// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentProfileGenerator.h"
#include "DynamicProfileUtils.h"
#include "../inc/AgentProfileUtils.h"
#include "../inc/AgentRegistry.h"
#include <mutex>

using namespace winrt::Microsoft::Terminal::Settings::Model;
namespace AgentProfiles = ::Microsoft::Terminal::AgentProfiles;

std::wstring_view AgentProfileGenerator::GetNamespace() const noexcept
{
    return AgentProfiles::Source;
}

std::wstring_view AgentProfileGenerator::GetDisplayName() const noexcept
{
    return RS_(L"AgentProfileGeneratorDisplayName");
}

std::wstring_view AgentProfileGenerator::GetIcon() const noexcept
{
    return L"\uE99A";
}

void AgentProfileGenerator::GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles) const
{
    static std::mutex mutex;
    static std::optional<std::map<std::wstring, std::filesystem::path>> snapshot;
    static auto lastProbe = std::chrono::steady_clock::time_point{};
    std::scoped_lock lock{ mutex };
    const auto now = std::chrono::steady_clock::now();
    if (!snapshot || now - lastProbe >= std::chrono::seconds{ 30 })
    {
        try
        {
            snapshot = AgentProfiles::Discover(AgentProfiles::NativePath());
            lastProbe = now;
        }
        catch (...)
        {
            LOG_CAUGHT_EXCEPTION_MSG("Native agent profile discovery failed; retaining the last successful snapshot");
            if (!snapshot)
            {
                throw;
            }
        }
    }

    for (const auto& agent : ::Microsoft::Terminal::Settings::Model::AgentRegistry::FilteredDelegateAgents())
    {
        const auto found = snapshot->find(std::wstring{ agent.id });
        if (found == snapshot->end())
        {
            continue;
        }
        const auto identity = std::wstring{ AgentProfiles::Source } + L":" + std::wstring{ agent.id };
        auto profile = CreateDynamicProfile(identity);
        profile->Name(winrt::hstring{ agent.displayName });
        profile->Icon(winrt::hstring{ L"ms-appx:///AgentIcons/" + std::wstring{ agent.id } + L".svg" });
        profile->AgentProfileId(winrt::hstring{ agent.id });
        profile->Commandline(winrt::hstring{ AgentProfiles::BuildCommand(found->second.native(), agent.id, {}, {}, {}) });
        profiles.emplace_back(std::move(profile));
    }
}
