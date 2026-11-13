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
        // Selection belongs to the hosted TreeViewList. This peer groups that
        // tree with search, filters, and sidebar chrome; it is not a second
        // selection container (and its children are TreeItems, not TabItems).
        return winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType::Group;
    }

    winrt::hstring TabStripAutomationPeer::GetNameCore() const
    {
        const auto name = m_inner.as<winrt::Windows::UI::Xaml::Automation::Peers::IAutomationPeerOverrides>().GetNameCore();
        return name.empty() ? RS_(L"VerticalTabsHeader/Text") : name;
    }

    winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation TabStripAutomationPeer::GetOrientationCore() const
    {
        return winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation::Vertical;
    }
}
