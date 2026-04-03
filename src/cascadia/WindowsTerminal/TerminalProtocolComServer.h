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
    winrt::com_array<Protocol::TabInfo> ListTabs(winrt::hstring const& windowIdFilter);
    winrt::com_array<Protocol::PaneInfo> ListPanes(winrt::hstring const& windowIdFilter,
                                                    winrt::hstring const& tabIdFilter);
    Protocol::PaneOutput ReadPaneOutput(winrt::hstring const& paneId,
                                         winrt::hstring const& source,
                                         int32_t maxLines);
    Protocol::ProcessStatus GetProcessStatus(winrt::hstring const& paneId);
    Protocol::SessionVariable GetSessionVariable(winrt::hstring const& paneId,
                                                   winrt::hstring const& name);
    winrt::hstring GetSettings();

    Protocol::TabCreationResult CreateTab(winrt::hstring const& windowId,
                                           winrt::hstring const& profile,
                                           winrt::hstring const& commandline,
                                           winrt::hstring const& title,
                                           bool suppressAppTitle,
                                           bool injectMcpCredentials,
                                           bool background);
    Protocol::TabCreationResult SplitPane(winrt::hstring const& paneId,
                                           winrt::hstring const& direction,
                                           float size,
                                           winrt::hstring const& profile,
                                           winrt::hstring const& commandline,
                                           bool injectMcpCredentials,
                                           bool background);
    void ClosePane(winrt::hstring const& paneId);
    void SendInput(winrt::hstring const& paneId, winrt::hstring const& text);
    void SetSessionVariable(winrt::hstring const& paneId,
                            winrt::hstring const& name,
                            winrt::hstring const& value);
    winrt::hstring SetSettings(winrt::hstring const& settingsContent);
    Protocol::QuickPickResult QuickPick(winrt::hstring const& title,
                                         winrt::array_view<winrt::hstring const> choices,
                                         bool allowFreeInput);

    // Static setup — must be called before s_StartListening().
    static void s_setEmperor(WindowEmperor* emperor) noexcept;

    static HRESULT s_StartListening();
    static HRESULT s_StopListening();

    // Deliver an event to all connected COM clients.
    static void s_NotifyEventToComClients(const std::string& eventJson);

private:
    bool _authenticated = false;

    // Event tracking for push-based notifications to COM clients.
    winrt::event<winrt::Windows::Foundation::TypedEventHandler<
        winrt::Windows::Foundation::IInspectable, winrt::hstring>> _eventReceived;

    // Static tracking of live COM instances for event delivery
    static std::mutex s_instancesMutex;
    static std::vector<TerminalProtocolComServer*> s_instances;
    static bool s_pageEventsRegistered;

    void _addInstance();
    void _removeInstance();
    static void _ensurePageEventsRegistered();

    static WindowEmperor* s_emperor;
};
