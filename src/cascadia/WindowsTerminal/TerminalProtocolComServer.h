// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ITerminalProtocolServer.h" // MIDL-generated from src/host/proxy/ITerminalProtocolServer.idl

// Per-brand CLSIDs — same pattern as CTerminalHandoff.
#if defined(WT_BRANDING_RELEASE)
#define __CLSID_TerminalProtocolServer "A2E4F6B8-1C3D-4E5F-A6B7-C8D9E0F1A2B3"
#elif defined(WT_BRANDING_PREVIEW)
#define __CLSID_TerminalProtocolServer "B3F5A7C9-2D4E-4F6A-B7C8-D9E0F1A2B3C4"
#elif defined(WT_BRANDING_CANARY)
#define __CLSID_TerminalProtocolServer "C4A6B8D0-3E5F-4A7B-C8D9-E0F1A2B3C4D5"
#else
#define __CLSID_TerminalProtocolServer "D5B7C9E1-4F6A-4B8C-D9E0-F1A2B3C4D5E6"
#endif

class ProtocolRequestHandler;
class WindowEmperor;

struct __declspec(uuid(__CLSID_TerminalProtocolServer))
TerminalProtocolComServer : public Microsoft::WRL::RuntimeClass<Microsoft::WRL::RuntimeClassFlags<Microsoft::WRL::RuntimeClassType::ClassicCom>, ITerminalProtocolServer>
{
    // ITerminalProtocolServer
    STDMETHODIMP HandleRequest(BSTR requestJson, BSTR* responseJson) override; // JSON fallback (retained during transition)

    // Typed methods
    STDMETHODIMP Authenticate(BSTR token, BOOL* authenticated, BSTR* protocolVersion) override;
    STDMETHODIMP GetCapabilities(BSTR* protocolVersion, BSTR* supportedMethodsJson) override;

    STDMETHODIMP GetActivePane(PROTOCOL_PANE_INFO* result) override;
    STDMETHODIMP ListWindows(UINT32* count, PROTOCOL_WINDOW_INFO** results) override;
    STDMETHODIMP ListTabs(BSTR windowIdFilter, UINT32* count, PROTOCOL_TAB_INFO** results) override;
    STDMETHODIMP ListPanes(BSTR windowIdFilter, BSTR tabIdFilter, UINT32* count, PROTOCOL_PANE_INFO** results) override;
    STDMETHODIMP ReadPaneOutput(BSTR paneId, BSTR source, INT32 maxLines, PROTOCOL_PANE_OUTPUT* result) override;
    STDMETHODIMP GetProcessStatus(BSTR paneId, PROTOCOL_PROCESS_STATUS* result) override;
    STDMETHODIMP GetSessionVariable(BSTR paneId, BSTR name, PROTOCOL_SESSION_VARIABLE* result) override;
    STDMETHODIMP GetSettings(BSTR* settingsJson) override;

    STDMETHODIMP CreateTab(BSTR windowId, BSTR profile, BSTR commandline, BSTR title,
                           BOOL suppressAppTitle, BOOL injectMcpCredentials, BOOL background,
                           PROTOCOL_TAB_CREATION_RESULT* result) override;
    STDMETHODIMP SplitPane(BSTR paneId, BSTR direction, float size, BSTR profile, BSTR commandline,
                           BOOL injectMcpCredentials, BOOL background,
                           PROTOCOL_TAB_CREATION_RESULT* result) override;
    STDMETHODIMP ClosePane(BSTR paneId) override;
    STDMETHODIMP SendInput(BSTR paneId, BSTR text) override;
    STDMETHODIMP SetSessionVariable(BSTR paneId, BSTR name, BSTR value) override;
    STDMETHODIMP SetSettings(BSTR settingsContent, BSTR* backupPath) override;

    // Static setup — must be called before s_StartListening().
    static void s_setEmperor(WindowEmperor* emperor) noexcept;
    static void s_setHandler(ProtocolRequestHandler* handler) noexcept;

    static HRESULT s_StartListening();
    static HRESULT s_StopListening();

private:
    bool _authenticated = false;

    static WindowEmperor* s_emperor;
    static ProtocolRequestHandler* s_handler;
};

#pragma warning(push)
#pragma warning(disable : 26477)
#pragma warning(disable : 26476)
CoCreatableClass(TerminalProtocolComServer);
#pragma warning(pop)
