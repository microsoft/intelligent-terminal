// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <optional>
#include <utility>
#include <winrt/base.h>

namespace TerminalApp
{
    class AgentProviderTelemetryBaseline
    {
    public:
        struct Providers
        {
            winrt::hstring primary;
            winrt::hstring delegate;
        };

        std::optional<Providers> ObserveAppliedSettings(Providers applied)
        {
            // Own raw values: the settings editor can mutate the applied object
            // before the next accepted reload, including custom-to-custom changes.
            return std::exchange(_providers, std::move(applied));
        }

    private:
        std::optional<Providers> _providers;
    };
}
