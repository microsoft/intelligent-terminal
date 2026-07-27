// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"

#include "TabRowControl.g.h"

namespace winrt::TerminalApp::implementation
{
    struct TabRowControl : TabRowControlT<TabRowControl>
    {
        TabRowControl();

        void OnNewTabButtonClick(const Windows::Foundation::IInspectable& sender, const Microsoft::UI::Xaml::Controls::SplitButtonClickEventArgs& args);
        void OnNewTabButtonDrop(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::UI::Xaml::DragEventArgs& e);
        void OnNewTabButtonDragOver(const winrt::Windows::Foundation::IInspectable& sender, const winrt::Windows::UI::Xaml::DragEventArgs& e);

        til::property_changed_event PropertyChanged;
        WINRT_OBSERVABLE_PROPERTY(bool, ShowElevationShield, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(bool, ShowWorkspacesButton, PropertyChanged.raise, true);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, WorkspaceName, PropertyChanged.raise, L"");

    public:
        // PROTOTYPE — flipping this at Initialize hides the MUX TabView and
        // shows the local:TabStrip (see investigation-vertical-tabs.md).
        // WINRT_OBSERVABLE_PROPERTY above leaves the access modifier at
        // private, so the explicit public: is load-bearing.
        bool IsVerticalLayout() const noexcept { return _isVerticalLayout; }
        void IsVerticalLayout(bool value);

    private:
        bool _isVerticalLayout{ false };
        void _applyLayoutVisibility();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabRowControl);
}
