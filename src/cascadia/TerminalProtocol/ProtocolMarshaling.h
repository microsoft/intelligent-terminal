// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <wil/com.h>
#include "../inc/TerminalProtocolProxyRegistration.h"

namespace Microsoft::Terminal::Protocol
{
    // A thread-confined scope for callers that need the elevation gate or an
    // explicit sibling path. Destroy before CoUninitialize. Production callers
    // use the process-wide registration instead; do not combine both owners.
    class ScopedMarshaling
    {
    public:
        ~ScopedMarshaling()
        {
            LOG_IF_FAILED(_registration.Unregister());
        }

        [[nodiscard]] HRESULT InitializeForElevatedProcess() noexcept
        try
        {
            if (_initialized || !details::IsProcessElevated())
            {
                return S_OK;
            }
            return InitializeFromExecutableDirectory();
        }
        CATCH_RETURN()

        // Bypass package/elevation policy for an explicit executable-sibling
        // registration, including native probes that must not request UAC.
        [[nodiscard]] HRESULT InitializeFromExecutableDirectory() noexcept
        try
        {
            if (_initialized)
            {
                return S_OK;
            }
            const auto module = details::LoadAndVerifyProxyDll(details::GetExecutableLocalProxyPath());
            RETURN_IF_FAILED(_registration.Register(module.get()));
            _initialized = true;
            return S_OK;
        }
        CATCH_RETURN()

    private:
        details::ProxyRegistration _registration;
        bool _initialized = false;
    };
}
