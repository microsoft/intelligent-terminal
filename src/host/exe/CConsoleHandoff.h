/*++
Copyright (c) Microsoft Corporation
Licensed under the MIT license.

Module Name:
- CConsoleHandoff.h

Abstract:
- This module receives a console session handoff from the operating system to
  an out-of-band, out-of-box console host.

Author(s):
- Michael Niksa (MiNiksa) 31-Aug-2020

--*/

#pragma once

#include "IConsoleHandoff.h"

#if defined(WT_BRANDING_RELEASE)
#define __CLSID_CConsoleHandoff "9A0159CA-5632-4916-B4D5-052D9D5A6195"
#elif defined(WT_BRANDING_PREVIEW)
#define __CLSID_CConsoleHandoff "FD0E53A3-EE5F-42EE-86DD-233F5A9FE85E"
#elif defined(WT_BRANDING_CANARY)
#define __CLSID_CConsoleHandoff "04142D7E-503D-40B7-A170-212F6806F354"
#else
#define __CLSID_CConsoleHandoff "47613D30-96E3-42C1-BAED-0281BDFB56CF"
#endif

using namespace Microsoft::WRL;

struct __declspec(uuid(__CLSID_CConsoleHandoff))
CConsoleHandoff : public RuntimeClass<RuntimeClassFlags<ClassicCom>, IConsoleHandoff, IDefaultTerminalMarker>
{
#pragma region IConsoleHandoff
    STDMETHODIMP EstablishHandoff(HANDLE server,
                                  HANDLE inputEvent,
                                  PCCONSOLE_PORTABLE_ATTACH_MSG msg,
                                  HANDLE signalPipe,
                                  HANDLE inboxProcess,
                                  HANDLE* process);

#pragma endregion
};

CoCreatableClass(CConsoleHandoff);
