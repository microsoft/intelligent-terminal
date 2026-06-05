// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "CAgentChannelPoc.h"

#include <wil/com.h>
#include <wil/resource.h>
#include <wil/result.h>

#include <atomic>
#include <chrono>
#include <string>
#include <thread>

using namespace Microsoft::WRL;

// The registration ID of the class object, for clean up later.
static DWORD g_agentChannelPocRegistration = 0;

STDMETHODIMP CAgentChannelPoc::Subscribe(IAgentChannelSink* sink)
try
{
    RETURN_HR_IF(E_INVALIDARG, !sink);

    // Store the sink as an *agile reference* so it can be safely resolved and
    // called from a different apartment/thread than the one Subscribe arrived
    // on. This is the correct fix for the STA->MTA callback hazard that the
    // WinRT path papered over.
    ComPtr<IAgileReference> agileSink;
    RETURN_IF_FAILED(::RoGetAgileReference(AGILEREFERENCE_DEFAULT, __uuidof(IAgentChannelSink), sink, &agileSink));

    // Fire a few events from a background MTA thread to exercise the
    // cross-process classic-COM event callback. The lambda captures the agile
    // reference by value, so it is independent of this object's lifetime.
    std::thread([agileSink]() {
        auto coUninit = wil::CoInitializeEx(COINIT_MULTITHREADED);
        for (int i = 1; i <= 5; ++i)
        {
            std::this_thread::sleep_for(std::chrono::milliseconds(500));

            ComPtr<IAgentChannelSink> resolved;
            if (FAILED(agileSink->Resolve(__uuidof(IAgentChannelSink), &resolved)) || !resolved)
            {
                break;
            }

            const auto payload = std::wstring{ L"{\"poc_event\":" } + std::to_wstring(i) + L"}";
            wil::unique_bstr json{ ::SysAllocString(payload.c_str()) };
            if (FAILED(resolved->OnEvent(json.get())))
            {
                break; // client disconnected — stop firing
            }
        }
    }).detach();

    return S_OK;
}
CATCH_RETURN()

STDMETHODIMP CAgentChannelPoc::Unsubscribe()
{
    return S_OK;
}

STDMETHODIMP CAgentChannelPoc::Ping(BSTR input, BSTR* output)
{
    RETURN_HR_IF_NULL(E_POINTER, output);
    *output = nullptr;

    std::wstring response = L"pong: ";
    if (input)
    {
        response += input;
    }

    *output = ::SysAllocString(response.c_str());
    RETURN_HR_IF_NULL(E_OUTOFMEMORY, *output);
    return S_OK;
}

HRESULT CAgentChannelPoc::s_StartListening()
try
{
    const auto classFactory = Make<SimpleClassFactory<CAgentChannelPoc>>();
    RETURN_LAST_ERROR_IF_NULL(classFactory);

    ComPtr<IUnknown> unk;
    RETURN_IF_FAILED(classFactory.As(&unk));

    RETURN_IF_FAILED(::CoRegisterClassObject(__uuidof(CAgentChannelPoc), unk.Get(), CLSCTX_LOCAL_SERVER, REGCLS_MULTIPLEUSE, &g_agentChannelPocRegistration));
    return S_OK;
}
CATCH_RETURN()

HRESULT CAgentChannelPoc::s_StopListening()
{
    if (g_agentChannelPocRegistration)
    {
        RETURN_IF_FAILED(::CoRevokeClassObject(g_agentChannelPocRegistration));
        g_agentChannelPocRegistration = 0;
    }
    return S_OK;
}
