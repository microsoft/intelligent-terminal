// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <winrt/Windows.Foundation.Collections.h>
#include <winrt/Microsoft.Terminal.Settings.Model.h>
#include "AgentProfileUtils.h"

namespace Microsoft::Terminal::NewTabMenu
{
    inline winrt::Windows::Foundation::Collections::IVector<winrt::Microsoft::Terminal::Settings::Model::NewTabMenuEntry> GroupDefaultProfiles(
        const winrt::Windows::Foundation::Collections::IVector<winrt::Microsoft::Terminal::Settings::Model::NewTabMenuEntry>& entries)
    {
        namespace Model = winrt::Microsoft::Terminal::Settings::Model;
        if (!entries || entries.Size() != 1 || !entries.GetAt(0) || entries.GetAt(0).Type() != Model::NewTabMenuEntryType::RemainingProfiles)
        {
            return entries;
        }

        const auto profiles = entries.GetAt(0).as<Model::RemainingProfilesEntry>().Profiles();
        if (!profiles)
        {
            return entries;
        }

        auto ordinaryProfiles = winrt::single_threaded_map<int32_t, Model::Profile>();
        auto agentProfiles = winrt::single_threaded_map<int32_t, Model::Profile>();
        for (const auto& [index, profile] : profiles)
        {
            if (profile.Source() == AgentProfiles::Source)
            {
                agentProfiles.Insert(index, profile);
            }
            else
            {
                ordinaryProfiles.Insert(index, profile);
            }
        }

        if (ordinaryProfiles.Size() == 0 || agentProfiles.Size() == 0)
        {
            return entries;
        }

        Model::RemainingProfilesEntry ordinaryEntry;
        ordinaryEntry.Profiles(ordinaryProfiles);
        Model::RemainingProfilesEntry agentEntry;
        agentEntry.Profiles(agentProfiles);

        // Expand only the rendered menu; keep the configured entries unchanged.
        return winrt::single_threaded_vector<Model::NewTabMenuEntry>({ ordinaryEntry, Model::SeparatorEntry{}, agentEntry });
    }
}
