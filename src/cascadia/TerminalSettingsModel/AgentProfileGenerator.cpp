// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentProfileGenerator.h"
#include "DynamicProfileUtils.h"
#include "../inc/AgentProfileUtils.h"
#include "../inc/AgentRegistry.h"
#include "../inc/WtaProcess.h"
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
    const auto launcher = AgentProfiles::ResolveLauncher();
    if (launcher.empty())
    {
        return;
    }

    // Settings reloads and the Extensions page share a short-lived successful
    // snapshot. A transient probe failure must not erase the last known set.
    static std::mutex mutex;
    static std::optional<Json::Value> snapshot;
    static auto lastProbe = std::chrono::steady_clock::time_point{};
    std::scoped_lock lock{ mutex };
    const auto now = std::chrono::steady_clock::now();
    if (!snapshot || now - lastProbe >= std::chrono::seconds{ 30 })
    {
        const auto result = ::Microsoft::Terminal::WtaProcess::RunWtaCapture(launcher, L"probe-profile-agents", 2'000, nullptr, false);
        Json::Value parsed;
        Json::CharReaderBuilder builder;
        const std::unique_ptr<Json::CharReader> reader{ builder.newCharReader() };
        std::string errors;
        const bool valid = result.completed && result.exitCode == 0 &&
                           reader->parse(result.output.data(), result.output.data() + result.output.size(), &parsed, &errors) &&
                           parsed.isObject() && parsed["agents"].isArray() &&
                           std::ranges::all_of(parsed["agents"], [](const auto& agent) {
                               return agent.isObject() && agent["id"].isString();
                           });
        if (valid)
        {
            snapshot = std::move(parsed);
            lastProbe = now;
        }
        else
        {
            LOG_HR_MSG(E_FAIL, "Native agent profile discovery failed; retaining the last successful snapshot");
            THROW_HR_IF(E_FAIL, !snapshot);
        }
    }

    for (const auto& agent : ::Microsoft::Terminal::Settings::Model::AgentRegistry::FilteredDelegateAgents())
    {
        const auto id = winrt::to_string(agent.id);
        if (!std::ranges::any_of((*snapshot)["agents"], [&](const auto& found) { return found["id"].asString() == id; }))
        {
            continue;
        }
        const auto identity = std::wstring{ AgentProfiles::Source } + L":" + std::wstring{ agent.id };
        auto profile = CreateDynamicProfile(identity);
        profile->Name(winrt::hstring{ agent.displayName });
        profile->Icon(winrt::hstring{ L"ms-appx:///AgentIcons/" + std::wstring{ agent.id } + L".png" });
        profile->AgentProfileId(winrt::hstring{ agent.id });
        profile->Commandline(winrt::hstring{ AgentProfiles::BuildCommand(launcher, agent.id, {}, {}, {}) });
        profiles.emplace_back(std::move(profile));
    }
}
