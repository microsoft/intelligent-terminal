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

        std::optional<Providers> ObserveLoad(const bool initialLoad, const bool succeeded, Providers applied)
        {
            if (!initialLoad && !succeeded)
            {
                return std::nullopt;
            }

            // Own raw values: the settings editor can mutate the applied object
            // before the next accepted reload, including custom-to-custom changes.
            const auto previous = initialLoad ? std::nullopt : _providers;
            _providers = std::move(applied);
            return previous;
        }

    private:
        std::optional<Providers> _providers;
    };
}
