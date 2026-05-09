// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "FreOverlay.g.h"

namespace winrt::TerminalApp::implementation
{
    struct FreOverlay : FreOverlayT<FreOverlay>
    {
        FreOverlay();

        // x:Bind properties for localized strings
        winrt::hstring FreTitle();
        winrt::hstring Card1Title();
        winrt::hstring Card1Description();
        winrt::hstring Card2Title();
        winrt::hstring Card2Description();
        winrt::hstring Card3Title();
        winrt::hstring Card3Description();
        winrt::hstring Card4Title();
        winrt::hstring Card4Description();
        winrt::hstring DetailTitle();
        winrt::hstring DetailDescription();
        winrt::hstring DetailLink();
        winrt::hstring NextButtonText();

        // Event — sender must be the WinRT projected type.
        til::typed_event<winrt::TerminalApp::FreOverlay, winrt::Windows::Foundation::IInspectable> Completed;

        // XAML event handlers — must be public for generated code access.
        void _OnNextButtonClick(const winrt::Windows::Foundation::IInspectable& sender,
                                const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnCloseButtonClick(const winrt::Windows::Foundation::IInspectable& sender,
                                 const winrt::Windows::UI::Xaml::RoutedEventArgs& args);
        void _OnNavItemTapped(const winrt::Windows::Foundation::IInspectable& sender,
                              const winrt::Windows::UI::Xaml::Input::TappedRoutedEventArgs& args);

    private:
        int32_t _selectedIndex{ 0 };

        // Per-section detail strings (title, description).
        static constexpr int32_t NavItemCount = 4;
        std::array<winrt::hstring, NavItemCount> _detailTitles;
        std::array<winrt::hstring, NavItemCount> _detailDescs;

        void _SelectNavItem(int32_t index);
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(FreOverlay);
}
