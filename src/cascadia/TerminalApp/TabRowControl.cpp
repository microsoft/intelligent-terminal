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
    static void _removeChild(const WUX::Controls::Panel& panel, const WUX::UIElement& child)
    {
        if (!panel || !child)
        {
            return;
        }

        uint32_t index = 0;
        if (panel.Children().IndexOf(child, index))
        {
            panel.Children().RemoveAt(index);
        }
    }

    static WUX::Media::Geometry _geometryFromPathData(const hstring& data)
    {
        return WUX::Markup::XamlBindingHelper::ConvertValue(xaml_typename<WUX::Media::Geometry>(), box_value(data)).as<WUX::Media::Geometry>();
    }

    TabRowControl::TabRowControl()
    {
        InitializeComponent();
        _ensureVerticalChrome();
        _applyLayoutVisibility();
    }

    void TabRowControl::ShowElevationShield(const bool value)
    {
        ElevationShieldIcon().Visibility(value ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
        if (_showElevationShield != value)
        {
            _showElevationShield = value;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"ShowElevationShield" });
        }
    }

    void TabRowControl::IsVerticalLayout(bool value)
    {
        if (_isVerticalLayout == value)
        {
            return;
        }
        _isVerticalLayout = value;
        if (_isVerticalLayout)
        {
            _attachChromeToVertical();
        }
        else
        {
            _attachChromeToHorizontal();
        }
        _applyLayoutVisibility();
        PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"IsVerticalLayout" });
    }

    void TabRowControl::_applyLayoutVisibility()
    {
        // PROTOTYPE — the horizontal TabView and vertical TabStrip both live
        // in the XAML tree. TerminalPage owns the live vertical strip so it
        // remains in the client XAML island while TabRow can live in the
        // non-client titlebar.
        TabView().Visibility(_isVerticalLayout ? WUX::Visibility::Collapsed : WUX::Visibility::Visible);
        TabStrip().Visibility(WUX::Visibility::Collapsed);
    }

    void TabRowControl::_ensureVerticalChrome()
    {
        if (_verticalTitleBarContent)
        {
            return;
        }

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

        const auto railToggleGeometry = _geometryFromPathData(L"F0 M18.9752 18.7268H20.1252C20.4426 18.7268 20.7002 18.4692 20.7002 18.1518C20.7002 17.8344 20.4426 17.5768 20.1252 17.5768H18.9752C18.6578 17.5768 18.4002 17.8344 18.4002 18.1518C18.4002 18.4692 18.6578 18.7268 18.9752 18.7268Z M20.1252 21.0267H18.9752C18.6578 21.0267 18.4002 20.7691 18.4002 20.4517C18.4002 20.1343 18.6578 19.8767 18.9752 19.8767H20.1252C20.4426 19.8767 20.7002 20.1343 20.7002 20.4517C20.7002 20.7691 20.4426 21.0267 20.1252 21.0267Z M20.1252 22.1768H18.9752C18.6578 22.1768 18.4002 22.4344 18.4002 22.7518C18.4002 23.0692 18.6578 23.3268 18.9752 23.3268H20.1252C20.4426 23.3268 20.7002 23.0692 20.7002 22.7518C20.7002 22.4344 20.4426 22.1768 20.1252 22.1768Z M27.0254 11.8268H18.9754C17.3872 11.8268 16.1004 13.1136 16.1004 14.7018V22.7518C16.1004 24.3399 17.3872 25.6268 18.9754 25.6268H27.0254C28.6135 25.6268 29.9004 24.3399 29.9004 22.7518V14.7018C29.9004 13.1136 28.6135 11.8268 27.0254 11.8268ZM28.7504 16.4268V22.7518C28.7504 23.704 27.9776 24.4768 27.0254 24.4768H23.0004V16.4268H28.7504ZM18.9754 24.4768C18.0232 24.4768 17.2504 23.704 17.2504 22.7518V16.4268H21.8504V24.4768H18.9754ZM28.7504 15.2768H17.2504V14.7018C17.2504 13.7496 18.0232 12.9768 18.9754 12.9768H27.0254C27.9776 12.9768 28.7504 13.7496 28.7504 14.7018V15.2768Z");
        WUX::Media::TranslateTransform railToggleOffset;
        railToggleOffset.X(-16.1004);
        railToggleOffset.Y(-11.8268);
        railToggleGeometry.Transform(railToggleOffset);

        _verticalRailToggleIcon = WUX::Controls::PathIcon{};
        _verticalRailToggleIcon.Width(14);
        _verticalRailToggleIcon.Height(14);
        _verticalRailToggleIcon.Data(railToggleGeometry);
        railToggle.Content(_verticalRailToggleIcon);
        railToggle.Click([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->RailCollapseRequested.raise(*self, nullptr);
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
        expandedChrome.Children().Append(leadingChrome);

        WUX::Controls::Grid topChromeContainer;
        topChromeContainer.HorizontalAlignment(WUX::HorizontalAlignment::Right);
        topChromeContainer.Margin(WUX::Thickness{ 0, 0, 4, 0 });
        WUX::Controls::Grid::SetColumn(topChromeContainer, 1);
        expandedChrome.Children().Append(topChromeContainer);

        // Keep a distinct SplitButton permanently parented here. Reparenting
        // the horizontal button leaves its automation peer attached to the
        // old subtree and hides the client UIA tree after a layout round trip.
        _verticalNewTabButton = MUX::Controls::SplitButton{};
        _verticalNewTabButton.Width(64);
        _verticalNewTabButton.Height(32);
        _verticalNewTabButton.Padding(WUX::Thickness{});
        _verticalNewTabButton.HorizontalAlignment(WUX::HorizontalAlignment::Stretch);
        _verticalNewTabButton.VerticalAlignment(WUX::VerticalAlignment::Center);
        _verticalNewTabButton.AllowDrop(true);
        _verticalNewTabButton.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
        _verticalNewTabButton.BorderThickness(WUX::Thickness{});
        _verticalNewTabButton.Content(box_value(L"\xE710"));
        _verticalNewTabButton.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });
        _verticalNewTabButton.FontSize(12);
        const auto dividerKey = box_value(L"SplitButtonBorderBrushDivider");
        const auto dividerBrush = WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() };
        WUX::ResourceDictionary defaultResources;
        WUX::ResourceDictionary lightResources;
        WUX::ResourceDictionary darkResources;
        WUX::ResourceDictionary highContrastResources;
        defaultResources.Insert(dividerKey, dividerBrush);
        lightResources.Insert(dividerKey, dividerBrush);
        darkResources.Insert(dividerKey, dividerBrush);
        highContrastResources.Insert(dividerKey, dividerBrush);
        const auto themeResources = _verticalNewTabButton.Resources().ThemeDictionaries();
        themeResources.Insert(box_value(L"Default"), defaultResources);
        themeResources.Insert(box_value(L"Light"), lightResources);
        themeResources.Insert(box_value(L"Dark"), darkResources);
        themeResources.Insert(box_value(L"HighContrast"), highContrastResources);
        WUX::Automation::AutomationProperties::SetAccessibilityView(_verticalNewTabButton, WUX::Automation::Peers::AccessibilityView::Control);
        const auto newTabName = WUX::Automation::AutomationProperties::GetName(NewTabButton());
        const auto newTabHelpText = WUX::Automation::AutomationProperties::GetHelpText(NewTabButton());
        WUX::Automation::AutomationProperties::SetName(_verticalNewTabButton, newTabName);
        WUX::Automation::AutomationProperties::SetHelpText(_verticalNewTabButton, newTabHelpText);
        WUX::Controls::ToolTipService::SetToolTip(_verticalNewTabButton, box_value(newTabName));
        _verticalNewTabButton.DragOver({ get_weak(), &TabRowControl::OnNewTabButtonDragOver });
        topChromeContainer.Children().Append(_verticalNewTabButton);

        titlebarGrid.Children().Append(expandedChrome);
        _verticalLeadingChrome = leadingChrome;
        _verticalNewTabHost = topChromeContainer;
        _verticalExpandedChrome = expandedChrome;
        _verticalTitleBarContent = titlebarGrid;
        SetVerticalRailState(true, false, 220);
    }

    void TabRowControl::_attachChromeToVertical()
    {
        _ensureVerticalChrome();

        const auto shield = ElevationShieldIcon();
        const auto workspaces = WorkspaceDropdown();

        _removeChild(HorizontalChromeHeader(), shield);
        _removeChild(HorizontalChromeHeader(), workspaces);

        _verticalLeadingChrome.Children().Append(shield);
        _verticalLeadingChrome.Children().Append(workspaces);
        shield.Visibility(ShowElevationShield() ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
    }

    void TabRowControl::_attachChromeToHorizontal()
    {
        if (!_verticalTitleBarContent)
        {
            return;
        }

        const auto shield = ElevationShieldIcon();
        const auto workspaces = WorkspaceDropdown();

        _removeChild(_verticalLeadingChrome, shield);
        _removeChild(_verticalLeadingChrome, workspaces);

        HorizontalChromeHeader().Children().Append(shield);
        HorizontalChromeHeader().Children().Append(workspaces);
        shield.Visibility(ShowElevationShield() ? WUX::Visibility::Visible : WUX::Visibility::Collapsed);
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
