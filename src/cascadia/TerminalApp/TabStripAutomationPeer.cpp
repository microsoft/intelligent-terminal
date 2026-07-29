// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TabStrip.h"
#include "TabStripAutomationPeer.h"

#include "TabStripAutomationPeer.g.cpp"

namespace winrt::TerminalApp::implementation
{
    TabStripAutomationPeer::TabStripAutomationPeer(TerminalApp::TabStrip const& owner) :
        TabStripAutomationPeerT<TabStripAutomationPeer>(owner)
    {
    }

    winrt::hstring TabStripAutomationPeer::GetClassNameCore() const
    {
        return L"TabStrip";
    }

    winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType TabStripAutomationPeer::GetAutomationControlTypeCore() const
    {
        return winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType::Tab;
    }

    winrt::hstring TabStripAutomationPeer::GetLocalizedControlTypeCore() const
    {
        return L"tab strip";
    }

    winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation TabStripAutomationPeer::GetOrientationCore() const
    {
        return winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation::Vertical;
    }
}
