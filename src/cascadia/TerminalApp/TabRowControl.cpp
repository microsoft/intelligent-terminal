// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TabRowControl.h"

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

    // Spec A §5.1: move the three chrome elements out of TabView's header /
    // footer slots. Shield + workspaces go into a horizontal StackPanel that
    // TerminalPage hands to the titlebar (same bar as min/max/close); the
    // new-tab split button anchors right-aligned inside TabStrip's trailing
    // slot at the bottom of the rail. XAML forbids a UIElement having two
    // logical parents at once, so we clear both host panels first.
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

        WUX::Controls::StackPanel titlebarPanel;
        titlebarPanel.Orientation(WUX::Controls::Orientation::Horizontal);
        titlebarPanel.VerticalAlignment(WUX::VerticalAlignment::Center);
        titlebarPanel.Children().Append(shield);
        titlebarPanel.Children().Append(workspaces);
        _verticalTitleBarContent = titlebarPanel;

        // Put the right-alignment on an outer Grid instead of the SplitButton
        // itself — setting HorizontalAlignment=Right on the SplitButton
        // fights its own template layout and collapses the dropdown chevron.
        // A right-aligned outer Grid with a Stretch SplitButton lets MUX
        // render both primary and secondary parts at their intrinsic size.
        newTab.HorizontalAlignment(WUX::HorizontalAlignment::Stretch);
        newTab.Height(31);
        newTab.MinWidth(64);
        newTab.Margin(WUX::Thickness{ 0, 4, 0, 4 });
        WUX::Controls::Grid trailingContainer;
        trailingContainer.HorizontalAlignment(WUX::HorizontalAlignment::Right);
        trailingContainer.Margin(WUX::Thickness{ 0, 0, 8, 0 });
        trailingContainer.Children().Append(newTab);
        TabStrip().TrailingContent(trailingContainer);
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
