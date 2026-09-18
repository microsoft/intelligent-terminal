// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "TmuxProtocol.h"
#include <optional>

namespace winrt::TerminalApp::implementation
{
    inline void SetTmuxSessionListStatus(const Windows::UI::Xaml::Controls::MenuFlyout& menu,
                                         const hstring& text,
                                         const hstring& detail = {})
    {
        using namespace Windows::UI::Xaml::Controls;
        MenuFlyoutItem item;
        item.Text(text);
        item.IsEnabled(false);
        if (!detail.empty())
        {
            ToolTipService::SetToolTip(item, box_value(detail));
        }
        menu.Items().Clear();
        menu.Items().Append(item);
    }

    template<typename Callback>
    void SetTmuxSessionList(const Windows::UI::Xaml::Controls::MenuFlyout& menu,
                            const std::vector<::Microsoft::Terminal::Tmux::SessionInfo>& sessions,
                            const std::optional<::Microsoft::Terminal::Tmux::Id> active,
                            Callback openSession)
    {
        using namespace Windows::UI::Xaml;
        menu.Items().Clear();
        for (const auto& session : sessions)
        {
            Controls::MenuFlyoutItem item;
            item.Text(to_hstring(session.name));
            Automation::AutomationProperties::SetAutomationId(item, hstring{ L"TmuxSession_" + std::to_wstring(session.id) });
            if (session.id == active)
            {
                item.Icon(Controls::SymbolIcon{ Controls::Symbol::Accept });
            }
            item.Click([openSession, id = session.id](auto&&, auto&&) { openSession(id); });
            menu.Items().Append(item);
        }
    }
}
