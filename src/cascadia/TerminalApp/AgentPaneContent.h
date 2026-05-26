// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "AgentPaneContent.g.h"
#include "TerminalPaneContent.h"
#include "BasicPaneEvents.h"

namespace winrt::TerminalApp::implementation
{
    struct AgentPaneContent : AgentPaneContentT<AgentPaneContent>, BasicPaneEvents
    {
    public:
        AgentPaneContent(const winrt::TerminalApp::TerminalPaneContent& inner);

        winrt::TerminalApp::TerminalPaneContent GetTerminalContent();
        winrt::Microsoft::Terminal::Control::TermControl GetTermControl();

        void UpdateAgentStatus(const winrt::hstring& name,
                               const winrt::hstring& version,
                               const winrt::hstring& model,
                               const winrt::hstring& state);

        void SetSessionsView(bool active);

        // Whether the agent pane is currently displaying its sessions view
        // (vs the chat view). Reflects the last `agent_state_changed` snapshot
        // from wta for this pane. Read by the window-level bottom bar to
        // decide the "sessions toggle" semantics — when sessions view is
        // active, the next press closes the pane; otherwise it switches
        // into sessions view.
        bool IsSessionsView() const noexcept { return _isSessionsView; }

        // --- Per-pane autofix / diagnostics state ---
        // Driven by inbound `autofix_state_changed` events for this pane's
        // owning tab. The window-level bottom bar reads these accessors
        // when refreshing for the active tab.
        enum class AutofixState
        {
            Idle,
            Detected,
            Pending,
            Armed,
            Suggested,
        };
        // Update the diagnostics state from an inbound autofix_state event
        // (single-writer for this pane's state). `pane_id` and other fields
        // come from the JSON payload; we only stash strings that the bar
        // surface needs to render. After updating, fires `StateChanged` so
        // the page can refresh the window-level bottom bar if this is the
        // active tab.
        void ApplyAutofixState(AutofixState state,
                               const winrt::hstring& paneId,
                               const winrt::hstring& summary,
                               const winrt::hstring& fixPreview,
                               const winrt::hstring& hotkeyHint,
                               const winrt::hstring& suggestionTitle);
        // Update the cached pane-position. Fires StateChanged so the
        // bottom bar can refresh its toggle-icon orientation.
        void SetAgentPanePosition(const winrt::hstring& position);
        // --- Cross-window drag rename plumbing ---
        // When a tab is dragged into this window, the source side has stashed
        // the originating tab's StableId keyed by ContentId. The target
        // window's `_MakeTerminalPane` consumes that entry, wraps the
        // ContentId-reattached pane back into an AgentPaneContent, and stores
        // the old StableId here. After the new Tab is constructed (with its
        // own fresh StableId), the page walks the agent leaves and emits a
        // `tab_renamed` event so the wta-helper can rekey its `--owner-tab-id`.
        // Internal-only (not on IDL) — only TerminalPage calls these.
        void SetPendingRenameFromTabId(const winrt::hstring& value) noexcept { _pendingRenameFromTabId = value; }
        winrt::hstring TakePendingRenameFromTabId() noexcept
        {
            const auto v = _pendingRenameFromTabId;
            _pendingRenameFromTabId = {};
            return v;
        }

        // Accessors for state that the window-level bottom bar projects.
        AutofixState GetAutofixState() const noexcept { return _autofixState; }
        winrt::hstring GetLastErrorPaneId() const noexcept { return _lastErrorPaneId; }
        winrt::hstring GetFixPreview() const noexcept { return _fixPreview; }
        winrt::hstring GetHotkeyHint() const noexcept { return _hotkeyHint; }
        winrt::hstring GetSuggestionTitle() const noexcept { return _suggestionTitle; }
        winrt::hstring GetDetectedSummary() const noexcept { return _detectedSummary; }
        winrt::hstring GetAgentPanePosition() const noexcept { return _agentPanePosition; }

        // Fired whenever cached bottom-bar-relevant state changes (autofix
        // state, sessions view, agent pane position). The outer page
        // subscribes to refresh the window-level bottom bar when the
        // firing pane belongs to the active tab.
        til::typed_event<winrt::TerminalApp::AgentPaneContent, IInspectable> StateChanged;

#pragma region IPaneContent
        winrt::Windows::UI::Xaml::FrameworkElement GetRoot();

        void UpdateSettings(const winrt::Microsoft::Terminal::Settings::Model::CascadiaSettings& settings);

        winrt::Windows::Foundation::Size MinimumSize();
        void Focus(winrt::Windows::UI::Xaml::FocusState reason = winrt::Windows::UI::Xaml::FocusState::Programmatic);
        void Close();
        winrt::Microsoft::Terminal::Settings::Model::INewContentArgs GetNewTerminalArgs(BuildStartupKind kind) const;

        winrt::hstring Title();
        uint64_t TaskbarState();
        uint64_t TaskbarProgress();
        bool ReadOnly();
        winrt::hstring Icon() const;
        Windows::Foundation::IReference<winrt::Windows::UI::Color> TabColor() const noexcept;
        winrt::Windows::UI::Xaml::Media::Brush BackgroundBrush();
#pragma endregion

#pragma region ISnappable
        float SnapDownToGrid(const TerminalApp::PaneSnapDirection direction, const float sizeToSnap);
        Windows::Foundation::Size GridUnitSize();
#pragma endregion

    private:
        winrt::TerminalApp::TerminalPaneContent _inner{ nullptr };

        // Latest agent status (raw, in case we need to recompute the displayed label).
        winrt::hstring _agentName{};
        winrt::hstring _agentVersion{};
        winrt::hstring _agentModel{};
        winrt::hstring _agentState{};

        // When true, the bar replaces "<agent> <version>" with "Agent sessions"
        // and hides the agent logo. Driven by TerminalPage::OnAgentStateChanged
        // (the single writer for view-derived UI state).
        bool _isSessionsView{ false };

        // --- Diagnostics / autofix state (projected by the window bottom bar) ---
        AutofixState _autofixState{ AutofixState::Idle };
        winrt::hstring _lastErrorPaneId{};
        winrt::hstring _fixPreview{};
        winrt::hstring _hotkeyHint{};
        winrt::hstring _suggestionTitle{};
        winrt::hstring _detectedSummary{};
        // Current AgentPanePosition for icon orientation. Set by
        // TerminalPage on creation + on settings change.
        winrt::hstring _agentPanePosition{ L"bottom" };

        // Source-tab StableId stashed during a cross-window agent-pane drag
        // (target side, between `_MakeTerminalPane` ContentId-reattach and
        // post-Tab-construction in `_InitializeTab`). Empty when no rename
        // is pending. See SetPendingRenameFromTabId / TakePendingRenameFromTabId.
        winrt::hstring _pendingRenameFromTabId{};

        // Inner content event tokens — forwarded to our own BasicPaneEvents.
        winrt::event_token _innerCloseRequested{};
        winrt::event_token _innerConnectionStateChanged{};
        winrt::event_token _innerBellRequested{};
        winrt::event_token _innerTitleChanged{};
        winrt::event_token _innerTabColorChanged{};
        winrt::event_token _innerTaskbarProgressChanged{};
        winrt::event_token _innerReadOnlyChanged{};
        winrt::event_token _innerFocusRequested{};

        void _wireInnerEvents();
        void _unwireInnerEvents();

        void _refreshLabel();
        void _refreshLogo();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(AgentPaneContent);
}
