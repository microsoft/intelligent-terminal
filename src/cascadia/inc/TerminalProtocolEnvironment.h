// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <til/env.h>
#include <wil/resource.h>
#include <wil/result.h>

namespace Microsoft::Terminal::Protocol
{
    inline wil::unique_environstrings_ptr CaptureProtocolStartupEnvironment()
    {
        THROW_IF_WIN32_BOOL_FALSE(SetEnvironmentVariableW(L"WT_COM_CLSID", nullptr));
        wil::unique_environstrings_ptr snapshot{ GetEnvironmentStringsW() };
        THROW_LAST_ERROR_IF_NULL(snapshot);
        return snapshot;
    }

    inline void ApplyHostClsid(til::env& environment, const std::wstring& hostClsid)
    {
        if (hostClsid.empty())
        {
            environment.as_map().erase(L"WT_COM_CLSID");
        }
        else
        {
            environment.as_map().insert_or_assign(L"WT_COM_CLSID", hostClsid);
        }
    }
}
