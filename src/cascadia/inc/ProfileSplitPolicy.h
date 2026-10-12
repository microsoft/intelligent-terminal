// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace Microsoft::Terminal::ProfileSplits
{
    namespace Model = winrt::Microsoft::Terminal::Settings::Model;

    struct Decision
    {
        bool duplicate{ false };
        bool unavailable{ false };
        Model::Profile target{ nullptr };
    };

    inline Decision Resolve(const Model::CascadiaSettings& settings,
                            const Model::Profile& source,
                            const Model::INewContentArgs& content)
    {
        const auto args = content.try_as<Model::NewTerminalArgs>();
        if (content && (!args || !args.Profile().empty() || args.ProfileIndex() ||
                        !args.Commandline().empty() || !args.Type().empty() || args.ContentId()))
        {
            return {};
        }
        const auto target = source ? source.DefaultSplitProfile() : winrt::hstring{};
        if (target.empty())
        {
            return { true };
        }

        Decision decision;
        GUID id{};
        if (target == L"default")
        {
            decision.target = settings.FindProfile(settings.GlobalSettings().DefaultProfile());
        }
        else if (SUCCEEDED(IIDFromString(target.c_str(), &id)))
        {
            decision.target = settings.FindProfile(winrt::guid{ id });
        }
        if (!decision.target || decision.target.Hidden() || decision.target.Orphaned())
        {
            decision.unavailable = true;
            decision.target = settings.FindProfile(settings.GlobalSettings().DefaultProfile());
        }
        return decision;
    }
}
