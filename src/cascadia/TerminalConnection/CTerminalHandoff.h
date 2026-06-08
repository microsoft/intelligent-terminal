/*++
Copyright (c) Microsoft Corporation
Licensed under the MIT license.

Module Name:
- CTerminalHandoff.h

Abstract:
- This module receives an incoming request to host a terminal UX
  for a console mode application already started and attached to a PTY.

Author(s):
- Michael Niksa (MiNiksa) 31-Aug-2020

--*/

#pragma once

#include "ITerminalHandoff.h"

#if defined(WT_BRANDING_RELEASE)
#define __CLSID_CTerminalHandoff "83D9C36B-160A-49C2-A222-A4A211A45B38"
#elif defined(WT_BRANDING_PREVIEW)
#define __CLSID_CTerminalHandoff "1FB14274-C0FC-43EB-B46B-10789AC76C1D"
#elif defined(WT_BRANDING_CANARY)
#define __CLSID_CTerminalHandoff "26B27C03-6354-4E19-9640-6E8D780A4675"
#else
#define __CLSID_CTerminalHandoff "AC7A517A-2E34-46C3-9A9F-CA20DAF2D8DF"
#endif

using NewHandoffFunction = HRESULT (*)(HANDLE* in, HANDLE* out, HANDLE signal, HANDLE reference, HANDLE server, HANDLE client, const TERMINAL_STARTUP_INFO* startupInfo);

struct __declspec(uuid(__CLSID_CTerminalHandoff))
CTerminalHandoff : public Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::RuntimeClassType::ClassicCom>, ITerminalHandoff3>
{
#pragma region ITerminalHandoff
    STDMETHODIMP EstablishPtyHandoff(HANDLE* in, HANDLE* out, HANDLE signal, HANDLE reference, HANDLE server, HANDLE client, const TERMINAL_STARTUP_INFO* startupInfo) override;

#pragma endregion

    static void s_setCallback(NewHandoffFunction callback) noexcept;
    static HRESULT s_StartListening();

private:
    static HRESULT s_StopListening();
};

// Disable warnings from the CoCreatableClass macro as the value it provides for
// automatic COM class registration is of much greater value than the nits from
// the static analysis warnings.
#pragma warning(push)

#pragma warning(disable : 26477) // Macro uses 0/NULL over nullptr.
#pragma warning(disable : 26476) // Macro uses naked union over variant.

CoCreatableClass(CTerminalHandoff);

#pragma warning(pop)
