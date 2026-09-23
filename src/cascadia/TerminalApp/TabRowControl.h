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
        WINRT_OBSERVABLE_PROPERTY(bool, ShowWorkspacesButton, PropertyChanged.raise, true);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, WorkspaceName, PropertyChanged.raise, L"");

    public:
        bool ShowElevationShield() const noexcept { return _showElevationShield; }
        void ShowElevationShield(bool value);

        // PROTOTYPE — flipping this at Initialize hides the MUX TabView and
        // shows the local:TabStrip (see investigation-vertical-tabs.md).
        // WINRT_OBSERVABLE_PROPERTY above leaves the access modifier at
        // private, so the explicit public: is load-bearing.
        bool IsVerticalLayout() const noexcept { return _isVerticalLayout; }
        void IsVerticalLayout(bool value);

        // In vertical mode, this complete chrome row is hosted in the window
        // titlebar when available and falls back to the top of the rail.
        winrt::Windows::UI::Xaml::UIElement VerticalTitleBarContent() const noexcept { return _verticalTitleBarContent; }
        winrt::Microsoft::UI::Xaml::Controls::SplitButton VerticalNewTabButton() const noexcept { return _verticalNewTabButton; }
        void SetVerticalRailState(bool visible, bool collapsed, double width);
        til::typed_event<TerminalApp::TabRowControl, winrt::Windows::Foundation::IInspectable> RailCollapseRequested;

    private:
        bool _showElevationShield{ false };
        bool _isVerticalLayout{ false };
        winrt::Windows::UI::Xaml::UIElement _verticalTitleBarContent{ nullptr };
        winrt::Windows::UI::Xaml::UIElement _verticalExpandedChrome{ nullptr };
        winrt::Windows::UI::Xaml::Controls::StackPanel _verticalLeadingChrome{ nullptr };
        winrt::Windows::UI::Xaml::Controls::Grid _verticalNewTabHost{ nullptr };
        winrt::Windows::UI::Xaml::Controls::PathIcon _verticalRailToggleIcon{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::SplitButton _verticalNewTabButton{ nullptr };
        void _applyLayoutVisibility();
        void _ensureVerticalChrome();
        void _attachChromeToVertical();
        void _attachChromeToHorizontal();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabRowControl);
}
