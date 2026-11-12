// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentProfileGenerator.h"
#include "DynamicProfileUtils.h"
#include "../inc/AgentProfileUtils.h"
#include "../inc/AgentRegistry.h"
#include <mutex>
#include <set>

using namespace winrt::Microsoft::Terminal::Settings::Model;
namespace AgentProfiles = ::Microsoft::Terminal::AgentProfiles;

namespace
{
    AgentProfileGenerator::WslSourceCache& WslCache()
    {
        static AgentProfileGenerator::WslSourceCache cache;
        return cache;
    }

}

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

void AgentProfileGenerator::RetainKnownWslAgents(WslSnapshot& sources, const WslSnapshot* previous)
{
    for (auto& source : sources)
    {
        if (source.probeFailed)
        {
            // Explicit discovery can start a stopped source. Only failed probes
            // lack authoritative installation evidence.
            source.agentIds.clear();
            if (previous)
            {
                const auto found = std::ranges::find_if(*previous, [&](const auto& candidate) {
                    return til::compare_ordinal_insensitive(candidate.distro, source.distro) == 0;
                });
                if (found != previous->end())
                {
                    source.agentIds = found->agentIds;
                }
            }
        }
    }
}

AgentProfileGenerator::WslSourceCache::Snapshot AgentProfileGenerator::WslSourceCache::Read()
{
    std::scoped_lock lock{ _mutex };
    return _sources;
}

void AgentProfileGenerator::WslSourceCache::Update(Snapshot sources)
{
    std::scoped_lock lock{ _mutex };
    RetainKnownWslAgents(sources, &_sources);
    _sources = std::move(sources);
    _discovered = true;
}

void AgentProfileGenerator::WslSourceCache::RestoreSavedProfiles(const std::vector<winrt::com_ptr<implementation::Profile>>& profiles)
{
    std::scoped_lock lock{ _mutex };
    if (_discovered)
    {
        return;
    }
    namespace Registry = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
    namespace Backend = ::Microsoft::Terminal::Settings::Model;
    for (const auto& profile : profiles)
    {
        const auto agentId = profile->AgentProfileId();
        if (profile->Source() != AgentProfiles::Source ||
            !profile->HasAgentProfileId() || !profile->HasAgentProfileSource() ||
            !Registry::SupportsNativeProfile(std::wstring_view{ agentId }))
        {
            continue;
        }
        const auto backend = Backend::AgentPaneBackend::FromSettings(agentId, profile->AgentProfileSource());
        if (!backend || backend->source != Backend::AgentPaneBackendSource::Wsl)
        {
            continue;
        }
        const auto identity = std::wstring{ AgentProfiles::Source } + L":" + backend->SelectionId();
        if (profile->Guid() != CreateDynamicProfile(identity)->Guid())
        {
            continue;
        }
        auto found = std::ranges::find(_sources, backend->wslDistro, &::Microsoft::Terminal::AgentAvailability::AgentSourceSnapshot::distro);
        if (found == _sources.end())
        {
            _sources.push_back({ backend->wslDistro, {}, false });
            found = std::prev(_sources.end());
        }
        found->agentIds.emplace(agentId);
    }
}

void AgentProfileGenerator::UpdateWslSources(WslSnapshot sources)
{
    WslCache().Update(std::move(sources));
}

void AgentProfileGenerator::RestoreSavedWslProfiles(const std::vector<winrt::com_ptr<implementation::Profile>>& profiles)
{
    WslCache().RestoreSavedProfiles(profiles);
}

void AgentProfileGenerator::GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles) const
{
    static std::mutex mutex;
    static DiscoveryCache cache;
    DiscoveryCache::Snapshot host;
    {
        std::scoped_lock lock{ mutex };
        host = cache.Get(std::chrono::steady_clock::now(), [] {
            return AgentProfiles::Discover(AgentProfiles::NativePath());
        });
    }
    GenerateProfiles(profiles, host, WslCache().Read());
}

void AgentProfileGenerator::GenerateProfiles(std::vector<winrt::com_ptr<implementation::Profile>>& profiles,
                                             const DiscoveryCache::Snapshot& host,
                                             const WslSnapshot& wsl)
{
    namespace Registry = ::Microsoft::Terminal::Settings::Model::AgentRegistry;
    auto sortedSources = wsl;
    std::ranges::sort(sortedSources, {}, &::Microsoft::Terminal::AgentAvailability::AgentSourceSnapshot::distro);
    const auto append = [&](const auto& agent, const std::wstring_view distro, const std::wstring_view executable) {
        const auto backend = distro.empty() ? std::wstring{ agent.id } :
                                              ::Microsoft::Terminal::Settings::Model::AgentPaneBackend::Wsl(distro, agent.id);
        const auto identity = std::wstring{ AgentProfiles::Source } + L":" + backend;
        auto profile = CreateDynamicProfile(identity);
        profile->Name(winrt::hstring{ std::wstring{ agent.displayName } + (distro.empty() ? L"" : L" (" + std::wstring{ distro } + L")") });
        profile->Icon(winrt::hstring{ L"ms-appx:///AgentIcons/" + std::wstring{ agent.id } + L".svg" });
        profile->AgentProfileId(winrt::hstring{ agent.id });
        profile->AgentProfileSource(winrt::hstring{ distro.empty() ? L"host" : L"wsl:" + std::wstring{ distro } });
        profile->Commandline(winrt::hstring{ distro.empty() ?
                                                 AgentProfiles::BuildCommand(executable, agent.id, {}, {}, {}) :
                                                 AgentProfiles::BuildWslCommand(distro, agent.id, {}, {}, {}) });
        if (!distro.empty())
        {
            profile->StartingDirectory(L"~");
            profile->PathTranslationStyle(winrt::Microsoft::Terminal::Control::PathTranslationStyle::WSL);
        }
        profiles.emplace_back(std::move(profile));
    };
    for (const auto& agent : Registry::BuiltinNativeProfileAgents)
    {
        if (!::Microsoft::Terminal::Settings::Model::AgentPolicy::IsAgentAllowed(agent.id))
        {
            continue;
        }
        if (const auto found = host.find(std::wstring{ agent.id }); found != host.end())
        {
            append(agent, {}, found->second.native());
        }
        std::set<std::wstring> emitted;
        for (const auto& source : sortedSources)
        {
            if (::Microsoft::Terminal::WslDistroName::IsSafe(std::wstring_view{ source.distro }) &&
                !source.distro.starts_with(L"docker-desktop") && !source.distro.starts_with(L"rancher-desktop") &&
                source.agentIds.contains(std::wstring{ agent.id }) && emitted.emplace(source.distro).second)
            {
                append(agent, source.distro, {});
            }
        }
    }
}
