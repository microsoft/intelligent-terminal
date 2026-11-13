// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Groups sidebar chrome and the hosted TreeView. The native tree peer owns
// selection and TreeItem children; the wrapper must not advertise a Tab.

#pragma once

#include "TabStripAutomationPeer.g.h"

namespace winrt::TerminalApp::implementation
{
    struct TabStripAutomationPeer : TabStripAutomationPeerT<TabStripAutomationPeer>
    {
        TabStripAutomationPeer(TerminalApp::TabStrip const& owner);

        winrt::hstring GetClassNameCore() const;
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType GetAutomationControlTypeCore() const;
        winrt::hstring GetNameCore() const;
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation GetOrientationCore() const;
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabStripAutomationPeer);
}
