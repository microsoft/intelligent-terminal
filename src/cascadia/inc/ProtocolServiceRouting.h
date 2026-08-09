// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <utility>

namespace Microsoft::Terminal::Protocol::details
{
    template<typename TVisibleWindowService, typename TProcessWideService>
    decltype(auto) RouteShellSessionRequest(
        const bool hasVisibleWindow,
        TVisibleWindowService&& visibleWindowService,
        TProcessWideService&& processWideService)
    {
        if (hasVisibleWindow)
        {
            return std::forward<TVisibleWindowService>(visibleWindowService)();
        }
        return std::forward<TProcessWideService>(processWideService)();
    }
}
