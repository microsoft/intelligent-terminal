// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TabRowControl.h"
#include "TabStrip.h"

#include "TabRowControl.g.cpp"

using namespace winrt::Windows::ApplicationModel::DataTransfer;

using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;
using namespace winrt::Windows::UI::Text;

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
}

namespace winrt::TerminalApp::implementation
{
    TabRowControl::TabRowControl()
    {
        InitializeComponent();
        _applyLayoutVisibility();
    }

    void TabRowControl::IsVerticalLayout(bool value)
    {
        if (_isVerticalLayout == value)
        {
            return;
        }
        _isVerticalLayout = value;
        _applyLayoutVisibility();
        PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"IsVerticalLayout" });
    }

    void TabRowControl::_applyLayoutVisibility()
    {
        // PROTOTYPE — the horizontal TabView and vertical TabStrip both live
        // in the XAML tree; only one is visible at a time. Full layout
        // reshaping (moving the strip to a left column) is Spec A.
        TabView().Visibility(_isVerticalLayout ? WUX::Visibility::Collapsed : WUX::Visibility::Visible);
        TabStrip().Visibility(_isVerticalLayout ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
        if (_isVerticalLayout)
        {
            _reparentChromeToVertical();
        }
    }

    // Move the three chrome elements out of TabView's header/footer slots and
    // build the complete vertical chrome row that can be hosted by either the
    // window titlebar or the in-page fallback.
    // XAML forbids a UIElement having two logical parents at once, so we clear
    // both host panels first.
    void TabRowControl::_reparentChromeToVertical()
    {
        if (_chromeReparentedToVertical)
        {
            return;
        }
        _chromeReparentedToVertical = true;

        auto shield = ElevationShieldIcon();
        auto workspaces = WorkspaceDropdown();
        auto newTab = NewTabButton();

        if (const auto headerPanel = TabView().TabStripHeader().try_as<WUX::Controls::StackPanel>())
        {
            headerPanel.Children().Clear();
        }
        TabView().TabStripHeader(nullptr);

        if (const auto footerGrid = TabView().TabStripFooter().try_as<WUX::Controls::Grid>())
        {
            footerGrid.Children().Clear();
        }
        TabView().TabStripFooter(nullptr);

        WUX::Controls::Grid titlebarGrid;
        titlebarGrid.Height(40);
        titlebarGrid.MinWidth(40);
        titlebarGrid.HorizontalAlignment(WUX::HorizontalAlignment::Left);

        WUX::Controls::ColumnDefinition toggleColumn;
        toggleColumn.Width(WUX::GridLengthHelper::FromValueAndType(40, WUX::GridUnitType::Pixel));
        titlebarGrid.ColumnDefinitions().Append(toggleColumn);
        titlebarGrid.ColumnDefinitions().Append(WUX::Controls::ColumnDefinition{});
        WUX::Controls::ColumnDefinition newTabColumn;
        newTabColumn.Width(WUX::GridLengthHelper::Auto());
        titlebarGrid.ColumnDefinitions().Append(newTabColumn);

        WUX::Controls::Button railToggle;
        railToggle.Width(40);
        railToggle.Height(40);
        railToggle.Padding(WUX::Thickness{});
        railToggle.HorizontalAlignment(WUX::HorizontalAlignment::Left);
        railToggle.VerticalAlignment(WUX::VerticalAlignment::Center);
        railToggle.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
        railToggle.BorderThickness(WUX::Thickness{});

        _verticalRailToggleIcon = WUX::Controls::FontIcon{};
        _verticalRailToggleIcon.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });
        _verticalRailToggleIcon.FontSize(12);
        railToggle.Content(_verticalRailToggleIcon);
        railToggle.Click([weakStrip = winrt::make_weak(TabStrip())](auto&&, auto&&) {
            if (const auto strip = weakStrip.get())
            {
                winrt::get_self<implementation::TabStrip>(strip)->OnRailToggleClick(nullptr, nullptr);
            }
        });
        titlebarGrid.Children().Append(railToggle);

        WUX::Controls::Grid expandedChrome;
        WUX::Controls::Grid::SetColumn(expandedChrome, 1);
        WUX::Controls::Grid::SetColumnSpan(expandedChrome, 2);
        expandedChrome.ColumnDefinitions().Append(WUX::Controls::ColumnDefinition{});
        WUX::Controls::ColumnDefinition expandedNewTabColumn;
        expandedNewTabColumn.Width(WUX::GridLengthHelper::Auto());
        expandedChrome.ColumnDefinitions().Append(expandedNewTabColumn);

        WUX::Controls::StackPanel leadingChrome;
        leadingChrome.Orientation(WUX::Controls::Orientation::Horizontal);
        leadingChrome.VerticalAlignment(WUX::VerticalAlignment::Center);
        leadingChrome.Children().Append(shield);
        leadingChrome.Children().Append(workspaces);
        expandedChrome.Children().Append(leadingChrome);

        // Keep the right-alignment on an outer Grid. Setting it directly on
        // SplitButton fights its template and can collapse the chevron.
        newTab.HorizontalAlignment(WUX::HorizontalAlignment::Stretch);
        newTab.Height(32);
        newTab.MinWidth(64);
        newTab.Margin(WUX::Thickness{});
        WUX::Controls::Grid topChromeContainer;
        topChromeContainer.HorizontalAlignment(WUX::HorizontalAlignment::Right);
        topChromeContainer.Margin(WUX::Thickness{ 0, 0, 4, 0 });
        topChromeContainer.Children().Append(newTab);
        WUX::Controls::Grid::SetColumn(topChromeContainer, 1);
        expandedChrome.Children().Append(topChromeContainer);

        titlebarGrid.Children().Append(expandedChrome);
        _verticalExpandedChrome = expandedChrome;
        _verticalTitleBarContent = titlebarGrid;
        SetVerticalRailState(true, false, 220);
    }

    void TabRowControl::SetVerticalRailState(const bool visible, const bool collapsed, const double width)
    {
        const auto titlebarGrid = _verticalTitleBarContent.try_as<WUX::FrameworkElement>();
        if (!titlebarGrid)
        {
            return;
        }

        titlebarGrid.Width(width);
        titlebarGrid.Visibility(visible ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
        if (_verticalExpandedChrome)
        {
            _verticalExpandedChrome.Visibility(!collapsed && visible ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
        }

        if (_verticalRailToggleIcon)
        {
            _verticalRailToggleIcon.Glyph(collapsed ? L"\xE8A0" : L"\xE89F");
            const auto label = collapsed ? RS_(L"VerticalTabsExpandPane") : RS_(L"VerticalTabsCollapsePane");
            if (const auto button = _verticalRailToggleIcon.Parent().try_as<WUX::Controls::Button>())
            {
                WUX::Automation::AutomationProperties::SetName(button, label);
                WUX::Controls::ToolTipService::SetToolTip(button, box_value(label));
            }
        }
    }

    // Method Description:
    // - Bound in the Xaml editor to the [+] button.
    // Arguments:
    // <unused>
    void TabRowControl::OnNewTabButtonClick(const IInspectable&, const Controls::SplitButtonClickEventArgs&)
    {
    }

    // Method Description:
    // - Bound in Drag&Drop of the Xaml editor to the [+] button.
    // Arguments:
    // <unused>
    void TabRowControl::OnNewTabButtonDrop(const IInspectable&, const winrt::Windows::UI::Xaml::DragEventArgs&)
    {
    }

    // Method Description:
    // - Bound in Drag-over of the Xaml editor to the [+] button.
    // Allows drop of 'StorageItems' which will be used as StartingDirectory
    // Arguments:
    //  - <unused>
    //  - e: DragEventArgs which hold the items
    void TabRowControl::OnNewTabButtonDragOver(const IInspectable&, const winrt::Windows::UI::Xaml::DragEventArgs& e)
    {
        // We can only handle drag/dropping StorageItems (files).
        // If the format on the clipboard is anything else, returning
        // early here will prevent the drag/drop from doing anything.
        if (!e.DataView().Contains(StandardDataFormats::StorageItems()))
        {
            return;
        }

        // Make sure to set the AcceptedOperation, so that we can later receive the path in the Drop event
        e.AcceptedOperation(DataPackageOperation::Copy);

        const auto modifiers = static_cast<uint32_t>(e.Modifiers());
        if (WI_IsFlagSet(modifiers, static_cast<uint32_t>(DragDrop::DragDropModifiers::Alt)))
        {
            e.DragUIOverride().Caption(RS_(L"DropPathTabSplit/Text"));
        }
        else if (WI_IsFlagSet(modifiers, static_cast<uint32_t>(DragDrop::DragDropModifiers::Shift)))
        {
            e.DragUIOverride().Caption(RS_(L"DropPathTabNewWindow/Text"));
        }
        else
        {
            e.DragUIOverride().Caption(RS_(L"DropPathTabRun/Text"));
        }

        // Sets if the caption is visible
        e.DragUIOverride().IsCaptionVisible(true);
        // Sets if the dragged content is visible
        e.DragUIOverride().IsContentVisible(false);
        // Sets if the glyph is visible
        e.DragUIOverride().IsGlyphVisible(false);
    }
}
