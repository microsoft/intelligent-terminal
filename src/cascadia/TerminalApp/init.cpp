// Copyright (c) Microsoft Corporation
// Licensed under the MIT license.

#include "pch.h"
#include <WilErrorReporting.h>

// Note: Generate GUID using TlgGuid.exe tool
TRACELOGGING_DEFINE_PROVIDER(
    g_hTerminalAppProvider,
    "Microsoft.Windows.Terminal.App",
    // {24a1622f-7da7-5c77-3303-d850bd1ab2ed}
    (0x24a1622f, 0x7da7, 0x5c77, 0x33, 0x03, 0xd8, 0x50, 0xbd, 0x1a, 0xb2, 0xed),
    TraceLoggingOptionMicrosoftTelemetry());

// Fork-specific provider for AI-agent (Intelligent Terminal) telemetry.
// Keep all fork-added events here so upstream provider schemas stay clean.
// The same provider GUID is registered by WTA (Rust side) so both processes
// emit into a single ETW stream — see tools/wta/src/telemetry.rs.
TRACELOGGING_DEFINE_PROVIDER(
    g_hTerminalAgentProvider,
    "Microsoft.Windows.Terminal.Agent",
    // {c2cc7e3b-9d5f-4a2e-b8a4-1f3e5d7c9b6a}
    (0xc2cc7e3b, 0x9d5f, 0x4a2e, 0xb8, 0xa4, 0x1f, 0x3e, 0x5d, 0x7c, 0x9b, 0x6a),
    TraceLoggingOptionMicrosoftTelemetry());

BOOL WINAPI DllMain(HINSTANCE hInstDll, DWORD reason, LPVOID /*reserved*/)
{
    switch (reason)
    {
    case DLL_PROCESS_ATTACH:
        DisableThreadLibraryCalls(hInstDll);
        TraceLoggingRegister(g_hTerminalAppProvider);
        TraceLoggingRegister(g_hTerminalAgentProvider);
        Microsoft::Console::ErrorReporting::EnableFallbackFailureReporting(g_hTerminalAppProvider);
        break;
    case DLL_PROCESS_DETACH:
        if (g_hTerminalAppProvider)
        {
            TraceLoggingUnregister(g_hTerminalAppProvider);
        }
        if (g_hTerminalAgentProvider)
        {
            TraceLoggingUnregister(g_hTerminalAgentProvider);
        }
        break;
    }

    return TRUE;
}

UTILS_DEFINE_LIBRARY_RESOURCE_SCOPE(L"TerminalApp/Resources")
