// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <Windows.h>
#include <objbase.h>
#include <wil/resource.h>
#include <wil/result.h>

#include <string>

namespace Microsoft::Terminal::AgentSessionHooks
{
    inline std::wstring DisabledEventName(REFGUID serverId)
    {
        wchar_t id[39]{};
        THROW_HR_IF(E_UNEXPECTED, StringFromGUID2(serverId, id, ARRAYSIZE(id)) == 0);
        return std::wstring{ L"Local\\IntelligentTerminal.AgentSessionHooksDisabled." } + id;
    }

    // A live, per-server signal reaches already-running CLIs without changing
    // their inherited environment or persistent plugin configuration.
    [[nodiscard]] inline HRESULT SetEnabled(wil::unique_event& disabledEvent, REFGUID serverId, const bool enabled) noexcept
    try
    {
        if (!disabledEvent)
        {
            const auto name = DisabledEventName(serverId);
            disabledEvent.reset(CreateEventW(nullptr, TRUE, !enabled, name.c_str()));
            RETURN_LAST_ERROR_IF_NULL(disabledEvent.get());
        }
        RETURN_IF_WIN32_BOOL_FALSE(enabled ? ResetEvent(disabledEvent.get()) : SetEvent(disabledEvent.get()));
        return S_OK;
    }
    CATCH_RETURN()

    inline bool IsEnabled(REFGUID serverId)
    {
        const auto name = DisabledEventName(serverId);
        const wil::unique_handle disabledEvent{ OpenEventW(SYNCHRONIZE, FALSE, name.c_str()) };
        if (!disabledEvent)
        {
            const auto error = GetLastError();
            // Older Terminal builds have no signal. The COM server also gates
            // delivery when activation starts a new Terminal process.
            if (error == ERROR_FILE_NOT_FOUND)
            {
                return true;
            }
            LOG_WIN32(error);
            return false;
        }

        const auto result = WaitForSingleObject(disabledEvent.get(), 0);
        if (result == WAIT_FAILED)
        {
            LOG_LAST_ERROR();
        }
        return result == WAIT_TIMEOUT;
    }
}
