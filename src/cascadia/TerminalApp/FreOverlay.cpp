// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "FreOverlay.h"
#include "FreOverlay.g.cpp"

#include <LibraryResources.h>

using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Media;
using namespace winrt::Windows::UI::Xaml::Controls;

namespace winrt::TerminalApp::implementation
{
    FreOverlay::FreOverlay()
    {
        InitializeComponent();

        // Pre-load per-section detail strings.
        _detailTitles = {
            RS_(L"FreOverlay_DetailTitle"),
            RS_(L"FreOverlay_Detail2Title"),
            RS_(L"FreOverlay_Detail3Title"),
            RS_(L"FreOverlay_Detail4Title"),
        };
        _detailDescs = {
            RS_(L"FreOverlay_DetailDescription"),
            RS_(L"FreOverlay_Detail2Description"),
            RS_(L"FreOverlay_Detail3Description"),
            RS_(L"FreOverlay_Detail4Description"),
        };
    }

    // ── Localized string getters (x:Bind, evaluated once) ───────────────

    winrt::hstring FreOverlay::FreTitle()       { return RS_(L"FreOverlay_Title"); }
    winrt::hstring FreOverlay::Card1Title()     { return RS_(L"FreOverlay_Card1Title"); }
    winrt::hstring FreOverlay::Card1Description() { return RS_(L"FreOverlay_Card1Description"); }
    winrt::hstring FreOverlay::Card2Title()     { return RS_(L"FreOverlay_Card2Title"); }
    winrt::hstring FreOverlay::Card2Description() { return RS_(L"FreOverlay_Card2Description"); }
    winrt::hstring FreOverlay::Card3Title()     { return RS_(L"FreOverlay_Card3Title"); }
    winrt::hstring FreOverlay::Card3Description() { return RS_(L"FreOverlay_Card3Description"); }
    winrt::hstring FreOverlay::Card4Title()     { return RS_(L"FreOverlay_Card4Title"); }
    winrt::hstring FreOverlay::Card4Description() { return RS_(L"FreOverlay_Card4Description"); }
    winrt::hstring FreOverlay::DetailTitle()    { return _detailTitles[0]; }
    winrt::hstring FreOverlay::DetailDescription() { return _detailDescs[0]; }
    winrt::hstring FreOverlay::DetailLink()     { return RS_(L"FreOverlay_DetailLink"); }
    winrt::hstring FreOverlay::NextButtonText() { return RS_(L"FreOverlay_NextButton"); }

    // ── Navigation ──────────────────────────────────────────────────────

    void FreOverlay::_OnNavItemTapped(const IInspectable& sender,
                                      const winrt::Windows::UI::Xaml::Input::TappedRoutedEventArgs& /*args*/)
    {
        if (const auto fe = sender.try_as<FrameworkElement>())
        {
            if (const auto tag = fe.Tag())
            {
                const auto idx = winrt::unbox_value<winrt::hstring>(tag);
                _SelectNavItem(std::stoi(winrt::to_string(idx)));
            }
        }
    }

    void FreOverlay::_SelectNavItem(int32_t index)
    {
        if (index < 0 || index >= NavItemCount || index == _selectedIndex)
            return;

        // Clear old selection
        const Border bgBorders[] = { NavBg0(), NavBg1(), NavBg2(), NavBg3() };
        const winrt::Windows::UI::Xaml::Shapes::Rectangle selRects[] = { NavSel0(), NavSel1(), NavSel2(), NavSel3() };
        auto transparent = SolidColorBrush{ winrt::Windows::UI::Colors::Transparent() };
        bgBorders[_selectedIndex].Background(transparent);
        bgBorders[_selectedIndex].BorderBrush(transparent);
        selRects[_selectedIndex].Visibility(Visibility::Collapsed);

        // Set new selection
        _selectedIndex = index;
        auto selectedBrush = SolidColorBrush{
            winrt::Windows::UI::ColorHelper::FromArgb(0x0A, 0xFF, 0xFF, 0xFF) };
        bgBorders[_selectedIndex].Background(selectedBrush);
        bgBorders[_selectedIndex].BorderBrush(selectedBrush);
        selRects[_selectedIndex].Visibility(Visibility::Visible);

        // Update detail text
        DetailTitleText().Text(_detailTitles[index]);
        DetailDescRun().Text(_detailDescs[index]);

        // "Learn more" link only on the first tab
        if (index == 0)
        {
            DetailLinkSpacer().Text(L" ");
            DetailLinkRun().Text(RS_(L"FreOverlay_DetailLink"));
        }
        else
        {
            DetailLinkSpacer().Text(L"");
            DetailLinkRun().Text(L"");
        }
    }

    // ── Button handlers ─────────────────────────────────────────────────

    void FreOverlay::_OnNextButtonClick(const IInspectable& /*sender*/,
                                        const RoutedEventArgs& /*args*/)
    {
        Completed.raise(*this, nullptr);
    }

    void FreOverlay::_OnCloseButtonClick(const IInspectable& /*sender*/,
                                         const RoutedEventArgs& /*args*/)
    {
        Completed.raise(*this, nullptr);
    }
}
