// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <mutex>
#include <vector>

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

namespace Protocol = winrt::Microsoft::Terminal::Protocol;

class WindowEmperor;

// Class factory for CoRegisterClassObject — creates instances of T
// via winrt::make.  Metadata Based Marshaling (MBM) handles cross-process
// marshaling automatically; no proxy/stub DLL is needed.
template<typename T>
struct Factory : winrt::implements<Factory<T>, IClassFactory, winrt::no_module_lock>
{
    HRESULT __stdcall CreateInstance(IUnknown* outer, GUID const& iid, void** result) noexcept final
    {
        *result = nullptr;
        if (outer)
            return CLASS_E_NOAGGREGATION;
        try
        {
            return winrt::make<T>().as(iid, result);
        }
        catch (...)
        {
            return winrt::to_hresult();
        }
    }

    HRESULT __stdcall LockServer(BOOL) noexcept final
    {
        return S_OK;
    }
};

struct __declspec(uuid(__CLSID_TerminalProtocolServer))
TerminalProtocolComServer : winrt::implements<TerminalProtocolComServer, Protocol::IProtocolServer>
{
    ~TerminalProtocolComServer();

    // ── IProtocolServer ──
    Protocol::AuthResult Authenticate(winrt::hstring const& token);
    winrt::hstring GetCapabilities();
    Protocol::PaneInfo GetActivePane();
    winrt::com_array<Protocol::WindowInfo> ListWindows();
    winrt::com_array<Protocol::TabInfo> ListTabs(uint64_t windowIdFilter);
    winrt::com_array<Protocol::PaneInfo> ListPanes(uint64_t windowIdFilter,
                                                    uint32_t tabIdFilter);
    Protocol::PaneOutput ReadPaneOutput(winrt::guid sessionId,
                                         winrt::hstring const& source,
                                         int32_t maxLines);
    Protocol::ProcessStatus GetProcessStatus(winrt::guid sessionId);
    Protocol::SessionVariable GetSessionVariable(winrt::guid sessionId,
                                                   winrt::hstring const& name);
    winrt::hstring GetSettings();

    Protocol::TabCreationResult CreateTab(uint64_t windowId,
                                           winrt::hstring const& profile,
                                           winrt::hstring const& commandline,
                                           winrt::hstring const& title,
                                           winrt::hstring const& startingDirectory,
                                           bool suppressAppTitle,
                                           bool background);
    Protocol::TabCreationResult SplitPane(winrt::guid sessionId,
                                           winrt::hstring const& direction,
                                           float size,
                                           winrt::hstring const& profile,
                                           winrt::hstring const& commandline,
                                           bool background);
    void ClosePane(winrt::guid sessionId);
    // SendInput intentionally removed from COM. Keystroke injection is now
    // confined to per-wta secure pipes (TerminalProtocolPipeServer).
    void FocusPane(winrt::guid sessionId);
    void SetSessionVariable(winrt::guid sessionId,
                            winrt::hstring const& name,
                            winrt::hstring const& value);

    // Events — push-based via callback
    void Subscribe(Protocol::IProtocolEventCallback const& callback);
    void Unsubscribe();

    // Client-originated event publishing (agent → WT → listeners)
    void SendEvent(winrt::hstring const& eventJson);

    // Static setup — must be called before s_StartListening().
    static void s_setEmperor(WindowEmperor* emperor) noexcept;

    static HRESULT s_StartListening();
    static HRESULT s_StopListening();

    // Deliver an event to all connected COM clients.
    static void s_NotifyEventToComClients(const std::string& eventJson);

private:
    bool _authenticated = false;

    // Per-instance event callback (set via Subscribe, cleared via Unsubscribe).
    std::mutex _callbackMutex;
    Protocol::IProtocolEventCallback _callback{ nullptr };

    // Static tracking of live COM instances for event delivery
    static std::mutex s_instancesMutex;
    static std::vector<TerminalProtocolComServer*> s_instances;

    bool _instanceRegistered{ false };

    void _addInstance();
    void _removeInstance();
    static void _ensurePageEventsRegistered();

    // Dispatch an {method:"autofix_state"} payload to every window's
    // TerminalPage on its UI thread.
    static void _dispatchAutofixStateToPage(const winrt::hstring& eventJson);

    // Same shape as _dispatchAutofixStateToPage, for {method:"agent_status"}.
    static void _dispatchAgentStatusToPage(const winrt::hstring& eventJson);

    // Same shape, for {method:"close_agent_pane"} emitted by the wta TUI when
    // the user presses Ctrl+C twice. TerminalPage tears down the agent pane.
    static void _dispatchCloseAgentPaneToPage(const winrt::hstring& eventJson);

    // Same shape, for {method:"view_changed"} emitted by the wta TUI when its
    // internal view flips (Esc out of Agents, `/sessions` slash command).
    // TerminalPage mirrors the new view onto its agent bar + bottom bar state.
    static void _dispatchViewChangedToPage(const winrt::hstring& eventJson);

    // Same shape, for {method:"resume_in_new_agent_tab"} emitted by the wta
    // TUI on Shift+Enter in the session view. TerminalPage creates a new
    // tab, reconciles the shared agent pane onto it, then publishes a
    // `load_session` event back to the wta TUI for the new tab's StableId.
    static void _dispatchResumeInNewAgentTabToPage(const winrt::hstring& eventJson);

    static WindowEmperor* s_emperor;
};
