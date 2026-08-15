// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Spec A §2.4: reports the vertical rail as a Tab control (not a plain
// UserControl / List) so screen readers announce it correctly. Children
// resolve to the hosted TabViewItems via the default GetChildrenCore path.

#pragma once

#include "TabStripAutomationPeer.g.h"

namespace winrt::TerminalApp::implementation
{
    struct TabStripAutomationPeer : TabStripAutomationPeerT<TabStripAutomationPeer>
    {
        TabStripAutomationPeer(TerminalApp::TabStrip const& owner);

        winrt::hstring GetClassNameCore() const;
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType GetAutomationControlTypeCore() const;
        winrt::hstring GetLocalizedControlTypeCore() const;
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationOrientation GetOrientationCore() const;
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabStripAutomationPeer);
}
