// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include <windows.h>
#include <winrt/base.h>
#include <wil/result.h>

#include "../../RichTabProvider/ProviderBroker.h"

extern "C" __declspec(dllexport) HRESULT RichTabPublishSnapshot(
    BSTR lease,
    BSTR snapshotJson,
    BOOL* succeeded,
    BSTR* message) noexcept
try
{
    RETURN_HR_IF_NULL(E_POINTER, lease);
    RETURN_HR_IF_NULL(E_POINTER, snapshotJson);
    RETURN_HR_IF_NULL(E_POINTER, succeeded);
    RETURN_HR_IF_NULL(E_POINTER, message);
    *succeeded = FALSE;
    *message = nullptr;

    const auto result = Microsoft::Terminal::RichTab::Provider::ProviderBroker::Instance().Publish(
        winrt::to_string(winrt::hstring{ lease }),
        winrt::to_string(winrt::hstring{ snapshotJson }));
    *succeeded = result.succeeded;
    *message = SysAllocString(winrt::to_hstring(result.message).c_str());
    RETURN_IF_NULL_ALLOC(*message);
    return S_OK;
}
CATCH_RETURN()
