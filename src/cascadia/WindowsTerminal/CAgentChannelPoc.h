// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Classic-COM PoC server for IAgentChannel.
//
// Mirrors CTerminalHandoff: a WRL ClassicCom RuntimeClass whose cross-process
// marshaling is handled by the MIDL proxy/stub in OpenConsoleProxy.dll. Unlike
// the WinRT/MBM IProtocolServer, activating and marshaling this interface does
// NOT consult the WinRT activation catalog (CWinRTActivationStoreCatalog) — the
// path implicated in the combase 0xc0000005 / wtcli 0x80010105 failures.
//
// PoC scope: Dev CLSID only. Productionizing would add per-branding CLSIDs the
// way CTerminalHandoff / TerminalProtocolComServer do.

#pragma once

#include "IAgentChannel.h"

#include <wrl/implements.h>
#include <wrl/module.h>

struct __declspec(uuid("4E1F8A92-6C3D-4B7A-8E5F-0A1B2C3D4E6F"))
CAgentChannelPoc : public Microsoft::WRL::RuntimeClass<
                       Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::RuntimeClassType::ClassicCom>,
                       IAgentChannel>
{
    // ── IAgentChannel ──
    STDMETHODIMP Subscribe(IAgentChannelSink* sink) override;
    STDMETHODIMP Unsubscribe() override;
    STDMETHODIMP Ping(BSTR input, BSTR* output) override;

    static HRESULT s_StartListening();
    static HRESULT s_StopListening();
};
