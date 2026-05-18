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
        // and hides the agent logo. Driven by TerminalPage::_BroadcastAgentSetView.
        bool _isSessionsView{ false };

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
