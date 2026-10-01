// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#include "pch.h"
#include "TabStrip.h"
#include "AgentIconUtils.h"
#include "TabStripAutomationPeer.h"

#include "TabStrip.g.cpp"
#include "TabStripSelectionChangedEventArgs.g.cpp"
#include "TabStripCloseRequestedEventArgs.g.cpp"
#include "TabStripDragStartingEventArgs.g.cpp"
#include "TabStripDroppedOutsideEventArgs.g.cpp"
#include "TabStripPaneItem.g.cpp"
#include "TabStripDisplayItem.g.cpp"
#include "TabStripPaneEventArgs.g.cpp"

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;

static constexpr double SearchPanelExpandedHeight = 40.0;
static constexpr auto SearchPanelAnimationDuration = std::chrono::milliseconds{ 200 };

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
}

namespace winrt::TerminalApp::implementation
{
    TabStripPaneItem::TabStripPaneItem(MUX::Controls::TabViewItem tab,
                                       uint32_t contentId,
                                       hstring iconPath,
                                       hstring title,
                                       bool isActive) :
        _tab{ std::move(tab) },
        _contentId{ contentId }
    {
        SyncIcon(iconPath);
        Title(std::move(title));
        AutomationName(Title());
        IsActive(isActive);
        ActiveIndicatorVisibility(isActive ? Visibility::Visible : Visibility::Collapsed);
    }

    void TabStripPaneItem::SyncIcon(const hstring& iconPath)
    {
        if (_iconPath == iconPath && Icon())
        {
            return;
        }

        if (iconPath.empty())
        {
            WUX::Controls::FontIcon fallback;
            fallback.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });
            fallback.FontSize(12);
            fallback.Glyph(L"\xE756");
            Icon(fallback);
        }
        else
        {
            Icon(::Microsoft::Terminal::UI::AgentIcons::ElementForIconPath(iconPath));
        }
        Icon().Width(16);
        Icon().Height(16);
        _iconPath = iconPath;
    }

    TabStripDisplayItem::TabStripDisplayItem(MUX::Controls::TabViewItem tab,
                                             IInspectable header) :
        _tab{ std::move(tab) },
        _paneItems{ single_threaded_observable_vector<TerminalApp::TabStripPaneItem>() }
    {
        Header(std::move(header));
    }

    void TabStripDisplayItem::SyncTabPresentation(bool railCollapsed)
    {
        if (const auto header = Header().try_as<TerminalApp::TabHeaderControl>())
        {
            Title(header.Title());
            IsPinned(header.TabStatus().IsPinned());
            header.IsMetadataVisible(!railCollapsed && !IsGroup() && !header.MetadataText().empty());
        }
        else if (const auto headerText = Header().try_as<hstring>())
        {
            Title(*headerText);
        }
        ContextFlyout(_tab.ContextFlyout());
        ToolTipText(WUX::Automation::AutomationProperties::GetHelpText(_tab));
        AcceleratorKey(WUX::Automation::AutomationProperties::GetAcceleratorKey(_tab));
        HeaderVisibility(railCollapsed ? Visibility::Collapsed : Visibility::Visible);
        CloseVisibility(!railCollapsed && _tab.IsClosable() ? Visibility::Visible : Visibility::Collapsed);
        UpdatePresentation(railCollapsed);
    }

    void TabStripDisplayItem::SyncIcon(hstring const& iconPath)
    {
        if (_iconPath && *_iconPath == iconPath)
        {
            return;
        }

        if (iconPath.empty())
        {
            WUX::Controls::FontIcon fallback;
            fallback.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons" });
            fallback.Glyph(L"\xE756");
            Icon(fallback);
        }
        else
        {
            Icon(::Microsoft::Terminal::UI::AgentIcons::ElementForIconPath(iconPath));
        }
        _iconPath = iconPath;
    }

    void TabStripDisplayItem::UpdatePresentation(bool railCollapsed)
    {
        const auto isGroup = IsGroup();
        GroupVisibility(isGroup && !railCollapsed ? Visibility::Visible : Visibility::Collapsed);
        ChildrenVisibility(isGroup && !railCollapsed && IsExpanded() ? Visibility::Visible : Visibility::Collapsed);
        IconVisibility(isGroup && !railCollapsed ? Visibility::Collapsed : Visibility::Visible);
        PinnedIconVisibility(railCollapsed && IsPinned() ? Visibility::Visible : Visibility::Collapsed);
        HeaderMinHeight(railCollapsed ? 32.0 : 40.0);
        LeadingContentMargin(railCollapsed ? WUX::Thickness{} : WUX::Thickness{ 6, 0, 10, 0 });
        ChevronGlyph(IsExpanded() ? L"\xE70D" : L"\xE76C");
    }

    static WUX::FrameworkElement _findNamedElement(WUX::DependencyObject const& root, std::wstring_view name)
    {
        const auto childCount = WUX::Media::VisualTreeHelper::GetChildrenCount(root);
        for (int32_t index = 0; index < childCount; ++index)
        {
            const auto child = WUX::Media::VisualTreeHelper::GetChild(root, index);
            if (const auto element = child.try_as<WUX::FrameworkElement>();
                element && element.Name() == name)
            {
                return element;
            }

            if (const auto element = _findNamedElement(child, name))
            {
                return element;
            }
        }

        return nullptr;
    }

    static WUX::Controls::Button _findCloseButton(WUX::DependencyObject const& root)
    {
        if (const auto element = _findNamedElement(root, L"CloseButton"))
        {
            return element.try_as<WUX::Controls::Button>();
        }

        return nullptr;
    }

    static bool _originatesFromHeaderControl(IInspectable const& source, WUX::DependencyObject const& root)
    {
        auto current = source.try_as<WUX::DependencyObject>();
        while (current && winrt::get_abi(current) != winrt::get_abi(root))
        {
            if (current.try_as<WUX::Controls::Button>() ||
                current.try_as<WUX::Controls::TextBox>())
            {
                return true;
            }
            current = WUX::Media::VisualTreeHelper::GetParent(current);
        }
        return false;
    }

    static bool _applyVerticalTabChrome(MUX::Controls::TabViewItem const& item)
    {
        item.ApplyTemplate();
        bool applied = false;

        if (const auto layoutRoot = _findNamedElement(item, L"LayoutRoot").try_as<WUX::Controls::Grid>())
        {
            applied = true;
            const auto transparentBrush = WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() };
            for (const auto keyName : {
                     L"TabViewItemHeaderBackground",
                     L"TabViewItemHeaderBackgroundPointerOver",
                     L"TabViewItemHeaderBackgroundPressed",
                     L"TabViewItemHeaderBackgroundSelected",
                     L"TabViewItemHeaderBackgroundDisabled" })
            {
                const auto key = box_value(keyName);
                layoutRoot.Resources().Remove(key);
                layoutRoot.Resources().Insert(key, transparentBrush);
            }

            auto selectionBackground = _findNamedElement(layoutRoot, L"VerticalSelectionBackground").try_as<WUX::Controls::Border>();
            if (!selectionBackground)
            {
                const auto lightTheme = item.ActualTheme() == WUX::ElementTheme::Light;
                selectionBackground = WUX::Controls::Border{};
                selectionBackground.Name(L"VerticalSelectionBackground");
                selectionBackground.Background(WUX::Media::SolidColorBrush{
                    Windows::UI::ColorHelper::FromArgb(lightTheme ? 0x09 : 0x0F,
                                                       lightTheme ? 0x00 : 0xFF,
                                                       lightTheme ? 0x00 : 0xFF,
                                                       lightTheme ? 0x00 : 0xFF) });
                selectionBackground.CornerRadius(WUX::CornerRadius{ 6.0, 6.0, 6.0, 6.0 });
                selectionBackground.IsHitTestVisible(false);
                WUX::Controls::Grid::SetColumnSpan(selectionBackground, 3);
                layoutRoot.Children().InsertAt(0, selectionBackground);
            }
            selectionBackground.Opacity(item.IsSelected() ? 1.0 : 0.0);
        }

        if (const auto tabContainer = _findNamedElement(item, L"TabContainer").try_as<WUX::Controls::Border>())
        {
            tabContainer.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
            tabContainer.CornerRadius(WUX::CornerRadius{ 6.0, 6.0, 6.0, 6.0 });
        }

        for (const auto name : {
                 L"SelectedBackgroundPath",
                 L"RightRadiusRenderArc",
                 L"LeftRadiusRenderArc",
                 L"TabSeparator",
                 L"BottomBorderLine" })
        {
            if (const auto element = _findNamedElement(item, name))
            {
                element.Opacity(0.0);
            }
        }

        return applied;
    }

    static void _restoreTabChrome(MUX::Controls::TabViewItem const& item)
    {
        item.ApplyTemplate();

        if (const auto layoutRoot = _findNamedElement(item, L"LayoutRoot").try_as<WUX::Controls::Grid>())
        {
            for (const auto keyName : {
                     L"TabViewItemHeaderBackground",
                     L"TabViewItemHeaderBackgroundPointerOver",
                     L"TabViewItemHeaderBackgroundPressed",
                     L"TabViewItemHeaderBackgroundSelected",
                     L"TabViewItemHeaderBackgroundDisabled" })
            {
                layoutRoot.Resources().Remove(box_value(keyName));
            }

            if (const auto selectionBackground = _findNamedElement(layoutRoot, L"VerticalSelectionBackground"))
            {
                uint32_t index{};
                if (layoutRoot.Children().IndexOf(selectionBackground.try_as<WUX::UIElement>(), index))
                {
                    layoutRoot.Children().RemoveAt(index);
                }
            }
        }

        if (const auto tabContainer = _findNamedElement(item, L"TabContainer").try_as<WUX::Controls::Border>())
        {
            tabContainer.ClearValue(WUX::Controls::Border::BackgroundProperty());
            tabContainer.ClearValue(WUX::Controls::Border::CornerRadiusProperty());
        }

        for (const auto name : {
                 L"SelectedBackgroundPath",
                 L"RightRadiusRenderArc",
                 L"LeftRadiusRenderArc",
                 L"TabSeparator",
                 L"BottomBorderLine" })
        {
            if (const auto element = _findNamedElement(item, name))
            {
                element.ClearValue(WUX::UIElement::OpacityProperty());
            }
        }
    }

    TabStrip::TabStrip()
    {
        _tabItems = single_threaded_observable_vector<IInspectable>();
        _displayItems = single_threaded_observable_vector<TerminalApp::TabStripDisplayItem>();
        _historyItems = single_threaded_observable_vector<TerminalApp::TabStripHistoryItem>();

        InitializeComponent();
        ::Microsoft::Terminal::UI::AgentIcons::Resources() = Resources();

        ItemsList().ItemsSource(_displayItems);
        // ListView consumes Enter even when its focused row is already selected.
        ItemsList().AddHandler(WUX::UIElement::KeyDownEvent(),
                               winrt::box_value(WUX::Input::KeyEventHandler{ get_weak(), &TabStrip::_onListKeyDown }),
                               true);
        _vectorChangedRevoker = _tabItems.VectorChanged(auto_revoke, { get_weak(), &TabStrip::_onItemsVectorChanged });
        _highContrast = _accessibilitySettings.HighContrast();
        _highContrastChangedRevoker = _accessibilitySettings.HighContrastChanged(auto_revoke, [weakThis{ get_weak() }, dispatcher{ Dispatcher() }](auto&&, auto&&) {
            try
            {
                dispatcher.RunAsync(Windows::UI::Core::CoreDispatcherPriority::Normal, [weakThis]() {
                    if (const auto self = weakThis.get())
                    {
                        self->_setHighContrastMode(self->_accessibilitySettings.HighContrast());
                    }
                });
            }
            CATCH_LOG();
        });
        Loaded([weakThis{ get_weak() }](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_searchAnimationEnabled = true;
                self->_updateSearchVisualState();
            }
        });
        Unloaded([weakThis{ get_weak() }](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_searchAnimationEnabled = false;
                self->_setSearchPanelExpanded(false, false);
            }
        });
        _richTabGitAvailable = _isGitInstalled();
        _applyRailState();
        _updateRichTabMetadataSelectionState();
    }

    IInspectable TabStrip::SelectedItem()
    {
        return _tabFromItem(ItemsList().SelectedItem());
    }
    void TabStrip::SelectedItem(IInspectable const& value)
    {
        if (!value)
        {
            ItemsList().SelectedIndex(-1);
            return;
        }

        uint32_t index{};
        if (_tabItems.IndexOf(value, index))
        {
            ItemsList().SelectedIndex(gsl::narrow_cast<int32_t>(index));
        }
    }
    int32_t TabStrip::SelectedIndex()
    {
        return ItemsList().SelectedIndex();
    }
    void TabStrip::SelectedIndex(int32_t value)
    {
        ItemsList().SelectedIndex(value);
    }
    DependencyObject TabStrip::ContainerFromIndex(int32_t index)
    {
        return ItemsList().ContainerFromIndex(index);
    }

    void TabStrip::SetTabItemVisibility(IInspectable const& item, bool visible)
    {
        if (const auto display = DisplayItemForTab(item))
        {
            SetTabItemVisibility(display, visible);
        }
    }

    void TabStrip::SetTabItemVisibility(TerminalApp::TabStripDisplayItem const& display, bool visible)
    {
        if (const auto tab = display ? display.Tab() : nullptr)
        {
            if (const auto previous = _tabItemVisibility.find(winrt::get_abi(tab));
                previous != _tabItemVisibility.end() && previous->second.Item.get() == tab && previous->second.Visible == visible)
            {
                return;
            }
            _tabItemVisibility.insert_or_assign(winrt::get_abi(tab), TabItemVisibilityState{ winrt::make_weak(tab), visible });
            if (const auto container = ItemsList().ContainerFromItem(display).try_as<ListViewItem>())
            {
                _applyTabItemVisibility(tab, container);
            }
        }
    }

    void TabStrip::SetFilterStatus(uint32_t visibleTabCount, bool selectedTabVisible)
    {
        FilterStatusText().Text(visibleTabCount == 1 ?
                                    RS_(L"VerticalTabsFilterStatusSingle") :
                                    winrt::hstring{ RS_fmt(L"VerticalTabsFilterStatusPlural", visibleTabCount) });
        HiddenCurrentTabIndicator().Visibility(selectedTabVisible ? Visibility::Collapsed : Visibility::Visible);
        FilterStatusBar().Visibility(_filterMode != TerminalApp::TabStripFilterMode::AllTabs ?
                                         Visibility::Visible :
                                         Visibility::Collapsed);
    }

    void TabStrip::SetTabPresentation(IInspectable const& item,
                                      hstring const& title,
                                      hstring const& iconPath)
    {
        if (const auto display = DisplayItemForTab(item))
        {
            SetTabPresentation(display, title, iconPath);
        }
    }

    void TabStrip::SetTabPresentation(TerminalApp::TabStripDisplayItem const& display,
                                      hstring const& title,
                                      hstring const& iconPath)
    {
        display.Title(title);
        if (const auto header = display.Header().try_as<TerminalApp::TabHeaderControl>())
        {
            header.Title(title);
        }
        winrt::get_self<TabStripDisplayItem>(display)->SyncIcon(iconPath);
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
        _refreshDisplayItemVisuals(display);
    }

    void TabStrip::SetPaneItems(IInspectable const& item,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup)
    {
        if (const auto display = DisplayItemForTab(item))
        {
            SetPaneItems(display, panes, isGroup);
        }
    }

    void TabStrip::SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup)
    {
        const auto current = display.PaneItems();
        const auto count = panes ? panes.Size() : 0;
        for (uint32_t index = 0; index < count; ++index)
        {
            const auto pane = panes.GetAt(index);
            auto match = index;
            while (match < current.Size() && current.GetAt(match).ContentId() != pane.ContentId())
            {
                ++match;
            }
            if (match == current.Size())
            {
                current.InsertAt(index, pane);
            }
            else
            {
                // Keep the row and its bindings alive during focus/title updates.
                const auto existing = current.GetAt(match);
                winrt::get_self<TabStripPaneItem>(existing)->SyncIcon(winrt::get_self<TabStripPaneItem>(pane)->IconPath());
                existing.Title(pane.Title());
                existing.IsActive(pane.IsActive());
                existing.ActiveIndicatorVisibility(pane.ActiveIndicatorVisibility());
                existing.MetadataText(pane.MetadataText());
                existing.MetadataVisibility(pane.MetadataVisibility());
                existing.AutomationName(pane.AutomationName());
                if (match != index)
                {
                    current.RemoveAt(match);
                    current.InsertAt(index, existing);
                }
            }
        }
        while (current.Size() > count)
        {
            current.RemoveAtEnd();
        }
        for (const auto& pane : current)
        {
            pane.HighlightQuery(display.SearchText());
        }
        display.IsGroup(isGroup);
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
    }

    IInspectable TabStrip::HeaderForTab(IInspectable const& item) const
    {
        if (const auto tab = item.try_as<MUX::Controls::TabViewItem>())
        {
            if (const auto display = _displayItemForTab(tab))
            {
                return display.Header();
            }
        }
        return nullptr;
    }

    TerminalApp::TabStripDisplayItem TabStrip::DisplayItemForTab(IInspectable const& item) const
    {
        if (const auto tab = item.try_as<MUX::Controls::TabViewItem>())
        {
            return _displayItemForTab(tab);
        }
        return nullptr;
    }

    TerminalApp::TabStripDisplayItem TabStrip::DisplayItemAt(uint32_t index) const
    {
        return _displayItemAt(index);
    }

    void TabStrip::SyncTabPresentation(TerminalApp::TabStripDisplayItem const& display)
    {
        if (display)
        {
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
            _refreshDisplayItemVisuals(display);
        }
    }

    void TabStrip::SetTabSearchText(IInspectable const& item, hstring const& searchText)
    {
        if (const auto tab = item.try_as<MUX::Controls::TabViewItem>())
        {
            if (const auto display = _displayItemForTab(tab))
            {
                display.SearchText(searchText);
                if (const auto header = display.Header().try_as<TerminalApp::TabHeaderControl>())
                {
                    header.SearchText(searchText);
                }
                for (const auto& pane : display.PaneItems())
                {
                    pane.HighlightQuery(searchText);
                }
            }
        }
    }

    static std::optional<Windows::UI::Color> _tabSelectionColor(MUX::Controls::TabViewItem const& tab)
    {
        if (const auto brush = tab ? tab.Background().try_as<WUX::Media::SolidColorBrush>() : nullptr;
            brush && brush.Color().A != 0 && brush.Opacity() > 0)
        {
            return brush.Color();
        }
        return std::nullopt;
    }

    static Windows::UI::Color _tabSelectionForeground(const Windows::UI::Color color) noexcept
    {
        const auto luminance = (0.2126 * color.R + 0.7152 * color.G + 0.0722 * color.B) / 255.0;
        return luminance >= 0.6 ? Windows::UI::Colors::Black() : Windows::UI::Colors::White();
    }

    static void _applyHistoryRowForeground(FrameworkElement const& root, TerminalApp::TabStripHistoryItem const& item)
    {
        if (!root)
        {
            return;
        }
        const auto isCurrent = item && item.IsCurrent();
        const auto foreground = isCurrent ? item.CurrentForeground() : nullptr;
        const auto palette = root.FindName(L"HistorySelectionPalette").try_as<Control>();
        for (const auto name : { L"HistoryTitleText", L"HistorySubtitleText", L"HistoryStatusText" })
        {
            if (const auto control = root.FindName(name).try_as<Control>())
            {
                control.ClearValue(Control::ForegroundProperty());
                if (foreground)
                {
                    control.Foreground(foreground);
                }
                else if (isCurrent && palette)
                {
                    WUX::Data::Binding binding;
                    binding.Source(palette);
                    binding.Path(PropertyPath{ L"Foreground" });
                    binding.Mode(WUX::Data::BindingMode::OneWay);
                    control.SetBinding(Control::ForegroundProperty(), binding);
                }
            }
        }
    }

    static void _applyHistoryRowAutomation(DependencyObject const& container, TerminalApp::TabStripHistoryItem const& item)
    {
        WUX::Automation::AutomationProperties::SetItemStatus(
            container, item && item.IsCurrent() ? RS_(L"VerticalTabsHistoryCurrentSession") : winrt::hstring{});
    }

    void TabStrip::_setHighContrastMode(bool enabled)
    {
        _highContrast = enabled;
        for (const auto& display : _displayItems)
        {
            _refreshDisplayItemVisuals(display);
        }
    }

    void TabStrip::_updateDisplayItemVisuals(FrameworkElement const& root,
                                             TerminalApp::TabStripDisplayItem const& display)
    {
        if (!root)
        {
            return;
        }

        // The rest of the row is driven by observable properties and x:Bind.
        // Resolve the template root once; FindName searches within that template.
        const auto grid = root.Name() == L"TabHeaderGrid" ?
                              root.try_as<Grid>() :
                              _findNamedElement(root, L"TabHeaderGrid").try_as<Grid>();
        if (grid)
        {
            const auto selectionBackground = grid.FindName(L"TabColorSelectionBackground").try_as<WUX::Controls::Border>();
            if (!display)
            {
                if (selectionBackground)
                {
                    selectionBackground.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
                }
                return;
            }
            const auto toggle = grid.FindName(L"TabGroupToggleButton").try_as<WUX::Controls::Control>();
            if (toggle)
            {
                const auto label = display.IsExpanded() ? RS_(L"VerticalTabsCollapseGroup") : RS_(L"VerticalTabsExpandGroup");
                if (WUX::Automation::AutomationProperties::GetName(toggle) != label)
                {
                    WUX::Automation::AutomationProperties::SetName(toggle, label);
                    ToolTipService::SetToolTip(toggle, box_value(label));
                }
            }
            const auto tabColor = _tabSelectionColor(display.Tab());
            const auto selected = display.SelectionVisibility() == Visibility::Visible;
            if (selectionBackground)
            {
                const auto showColor = tabColor && (selected || !_highContrast);
                WUX::Media::SolidColorBrush brush{ showColor ? *tabColor : Windows::UI::Colors::Transparent() };
                if (showColor && !selected)
                {
                    brush.Opacity(display.Tab().Background().as<WUX::Media::SolidColorBrush>().Opacity());
                }
                selectionBackground.Background(brush);
            }
            grid.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
            const auto header = display.Header().try_as<WUX::Controls::Control>();
            const auto close = grid.FindName(L"TabCloseButton").try_as<WUX::Controls::Control>();
            if (tabColor && selected)
            {
                const auto foreground = WUX::Media::SolidColorBrush{ _tabSelectionForeground(*tabColor) };
                for (const auto& control : { header, toggle, close })
                {
                    if (control)
                    {
                        control.Foreground(foreground);
                    }
                }
            }
            else
            {
                for (const auto& control : { header, toggle, close })
                {
                    if (control)
                    {
                        control.ClearValue(WUX::Controls::Control::ForegroundProperty());
                    }
                }
            }
        }
    }

    void TabStrip::_refreshDisplayItemVisuals(TerminalApp::TabStripDisplayItem const& display)
    {
        if (!display)
        {
            return;
        }

        uint32_t index{};
        if (_displayItems.IndexOf(display, index))
        {
            if (const auto container = ItemsList().ContainerFromIndex(index).try_as<FrameworkElement>())
            {
                _updateDisplayItemVisuals(container, display);
            }
        }
    }

    void TabStrip::OnTabHeaderLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto display = root.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
            {
                _updateDisplayItemVisuals(root, display);
                if (const auto close = root.FindName(L"TabCloseButton").try_as<FrameworkElement>())
                {
                    WUX::Automation::AutomationProperties::SetName(close, RS_(L"TabClose"));
                    ToolTipService::SetToolTip(close, box_value(RS_(L"TabCloseToolTip")));
                }
            }
        }
    }

    void TabStrip::OnPaneRowLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto pane = root.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                if (const auto title = _findNamedElement(root, L"PaneTitleText").try_as<TerminalApp::HighlightedTextControl>())
                {
                    title.Text(pane.Title());
                }
                if (const auto indicator = _findNamedElement(root, L"PaneActiveIndicator"))
                {
                    indicator.Visibility(pane.ActiveIndicatorVisibility());
                }
                if (const auto close = _findNamedElement(root, L"PaneCloseButton"))
                {
                    const auto label = RS_(L"PaneClose");
                    WUX::Automation::AutomationProperties::SetName(close, label);
                    ToolTipService::SetToolTip(close, box_value(label));
                }
            }
        }
    }

    void TabStrip::Orientation(TerminalApp::TabStripOrientation value)
    {
        _orientation = value;
        // Prototype: the ItemsStackPanel is hardcoded Vertical in XAML. C is
        // where the layout actually flips based on this property. The setter
        // stores the value so the drop-index math can read it, but has no
        // visual effect yet.
    }

    bool TabStrip::CanReorderTabs()
    {
        return ItemsList().AllowDrop();
    }
    void TabStrip::CanReorderTabs(bool value)
    {
        const auto enabled = value && !_isRailCollapsed;
        ItemsList().CanReorderItems(enabled);
        ItemsList().ReorderMode(enabled ? ListViewReorderMode::Enabled : ListViewReorderMode::Disabled);
        ItemsList().AllowDrop(enabled);
    }
    bool TabStrip::CanDragTabs()
    {
        return ItemsList().CanDragItems();
    }
    void TabStrip::CanDragTabs(bool value)
    {
        ItemsList().CanDragItems(value);
    }
    bool TabStrip::TabsVisible()
    {
        return _tabsVisible;
    }
    void TabStrip::TabsVisible(bool value)
    {
        if (_tabsVisible != value)
        {
            _tabsVisible = value;
            const auto historyVisible = _historyActive && !_isRailCollapsed;
            ItemsList().Visibility(value && !historyVisible ? Visibility::Visible : Visibility::Collapsed);
        }
    }
    void TabStrip::IsRailCollapsed(bool value)
    {
        if (_isRailCollapsed != value)
        {
            _isRailCollapsed = value;
            _applyRailState();
        }
    }

    void TabStrip::PrepareTabItem(MUX::Controls::TabViewItem const& item)
    {
        if (const auto display = _displayItemForTab(item))
        {
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
        }
    }

    void TabStrip::RefreshTabColor(MUX::Controls::TabViewItem const& item)
    {
        _refreshDisplayItemVisuals(_displayItemForTab(item));
    }

    void TabStrip::FilterMode(TerminalApp::TabStripFilterMode value)
    {
        const auto changed = _filterMode != value;
        _filterMode = value;
        FilterStatusBar().Visibility(value != TerminalApp::TabStripFilterMode::AllTabs ?
                                         Visibility::Visible :
                                         Visibility::Collapsed);
        if (changed)
        {
            FilterChanged.raise(*this, nullptr);
        }
    }

    void TabStrip::SearchActive(bool value)
    {
        if (_searchActive != value)
        {
            _searchActive = value;
            _updateSearchVisualState();
        }
    }

    void TabStrip::SearchQuery(winrt::hstring const& value)
    {
        if (_searchQuery != value)
        {
            _searchQuery = value;
            _syncingSearchState = true;
            SearchTextBox().Text(value);
            _syncingSearchState = false;
            _updateSearchVisualState();
        }
    }

    bool TabStrip::FocusTabSearch()
    {
        if (_isRailCollapsed || _historyActive || !SearchTabsButton().IsEnabled())
        {
            return false;
        }

        if (!_searchActive)
        {
            SearchActivationRequested.raise(*this, nullptr);
            SearchTabsButton().IsChecked(true);
            OnSearchToggleClick(nullptr, nullptr);
        }
        return SearchTextBox().Focus(WUX::FocusState::Programmatic);
    }

    WUX::Style TabStrip::_historyStatusTextStyle(winrt::hstring const& status)
    {
        const auto styleKey = status == L"Working"   ? L"HistoryActiveTextStyle" :
                              status == L"Attention" ? L"HistoryAttentionTextStyle" :
                              status == L"Error"     ? L"HistoryErrorTextStyle" :
                                                       L"HistorySubtitleTextStyle";
        return Resources().Lookup(box_value(styleKey)).as<WUX::Style>();
    }

    void TabStrip::CommitHistorySnapshot(std::vector<TerminalApp::TabStripHistoryItem> items, const bool ready)
    {
        _historySnapshot = std::move(items);
        // WTA supplies newest-activity-first rows; preserve that order within each group.
        std::stable_partition(_historySnapshot.begin(), _historySnapshot.end(), [](const auto& item) {
            const auto status = item.Status();
            return status != L"Ended" && status != L"Historical";
        });
        _historySearchTerms.clear();
        _historySearchTerms.reserve(_historySnapshot.size());
        for (const auto& item : _historySnapshot)
        {
            item.IsCurrent(false);
            item.CurrentBackground(nullptr);
            item.CurrentForeground(nullptr);
            item.StatusTextStyle(_historyStatusTextStyle(item.Status()));
            auto iconKey = box_value(L"AgentIcon." + item.AgentId());
            if (!Resources().HasKey(iconKey))
            {
                iconKey = box_value(L"AgentIcon.generic");
            }
            item.IconTemplate(Resources().Lookup(iconKey).as<DataTemplate>());
            _historySearchTerms.emplace_back(_buildHistorySearchTerms(item));
        }
        _applyHistoryProjection(true);
        if (ready && _historyActive && _agentFilterTelemetryPending)
        {
            _agentFilterTelemetryPending = false;
            TraceLoggingWrite(
                g_hTerminalAppProvider,
                "SidebarAgentFilterApplied",
                TraceLoggingDescription("User entered the sidebar agent view and its session rows loaded"),
                TraceLoggingUInt32(_historyItems.Size(), "row_count"),
                TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
                TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
        }
    }

    void TabStrip::SetCurrentHistoryItem(TerminalApp::TabStripHistoryItem const& current,
                                         MUX::Controls::TabViewItem const& tab)
    {
        WUX::Media::Brush background{ nullptr };
        WUX::Media::Brush foreground{ nullptr };
        if (const auto color = _tabSelectionColor(tab))
        {
            background = WUX::Media::SolidColorBrush{ *color };
            foreground = WUX::Media::SolidColorBrush{ _tabSelectionForeground(*color) };
        }
        for (uint32_t index = 0; index < _historyItems.Size(); ++index)
        {
            const auto item = _historyItems.GetAt(index);
            const auto isCurrent = item == current;
            item.IsCurrent(isCurrent);
            item.CurrentBackground(isCurrent ? background : nullptr);
            item.CurrentForeground(isCurrent ? foreground : nullptr);
            if (const auto container = HistoryList().ContainerFromIndex(index).try_as<ListViewItem>())
            {
                _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), item);
                _applyHistoryRowAutomation(container, item);
            }
        }
    }

    bool TabStrip::ApplyHistoryStatusDelta(winrt::hstring const& sessionId,
                                           winrt::hstring const& paneSessionId,
                                           winrt::hstring const& status,
                                           winrt::hstring const& statusText)
    {
        bool updated = false;
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            auto& item = _historySnapshot[index];
            if (item.SessionId() != sessionId)
            {
                continue;
            }

            item.PaneSessionId(paneSessionId);
            item.Status(status);
            item.StatusText(statusText);
            item.StatusTextStyle(_historyStatusTextStyle(status));
            const auto isLive = status == L"Idle" ||
                                status == L"Working" ||
                                status == L"Attention" ||
                                status == L"Error";
            item.IsLive(isLive);
            item.IsHistorical(status == L"Ended" || status == L"Historical");

            _historySearchTerms[index] = _buildHistorySearchTerms(item);
            updated = true;
        }

        if (updated)
        {
            _applyHistoryProjection();
        }
        return updated;
    }

    void TabStrip::ClearHistorySnapshot()
    {
        _historySnapshot.clear();
        _historySearchTerms.clear();
        _historyItems.Clear();
        _updateHistoryVisualState();
    }

    void TabStrip::ClearHistorySearch()
    {
        if (_historySearchQuery.empty() && HistorySearchTextBox().Text().empty())
        {
            return;
        }

        _historySearchQuery.clear();
        _syncingHistorySearchState = true;
        HistorySearchTextBox().Text(L"");
        _syncingHistorySearchState = false;
        _applyHistoryProjection();
    }

    void TabStrip::HistoryActive(bool value)
    {
        if (!value)
        {
            _agentFilterTelemetryPending = false;
        }
        FilterMode(TerminalApp::TabStripFilterMode::AllTabs);

        if (_historyActive != value)
        {
            _historyActive = value;
            _historyActivating = false;
            ClearHistorySearch();
            _updateSearchVisualState();
            _updateHistoryVisualState();
        }
    }

    void TabStrip::HistoryLoading(bool value)
    {
        if (_historyLoading != value)
        {
            _historyLoading = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::HistoryActivating(bool value)
    {
        if (_historyActivating != value)
        {
            _historyActivating = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::HistoryError(winrt::hstring const& value)
    {
        if (_historyError != value)
        {
            _historyError = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::HistoryRefreshError(winrt::hstring const& value)
    {
        if (_historyRefreshError != value)
        {
            _historyRefreshError = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::ProjectionControlsEnabled(bool value)
    {
        _projectionControlsEnabled = value;
        SearchTabsButton().IsEnabled(value);
        FilterTabsButton().IsEnabled(value && !_isRailCollapsed);
        TabHistoryButton().IsEnabled(value && !_isRailCollapsed);
    }

    void TabStrip::MoveTabItem(uint32_t from, uint32_t to)
    {
        if (from == to ||
            from >= _tabItems.Size() ||
            to >= _tabItems.Size())
        {
            return;
        }

        if (_displayItems.Size() != _tabItems.Size())
        {
            _syncDisplayItems();
        }
        if (from >= _displayItems.Size() || to >= _displayItems.Size())
        {
            return;
        }

        const auto item = _tabItems.GetAt(from);
        const auto display = _displayItems.GetAt(from);
        _syncingNativeReorder = true;
        auto endSync = wil::scope_exit([&]() noexcept {
            _syncingNativeReorder = false;
        });

        _tabItems.RemoveAt(from);
        _tabItems.InsertAt(to, item);
        _displayItems.RemoveAt(from);
        _displayItems.InsertAt(to, display);
    }

    void TabStrip::RestoreTabOrder(const std::vector<IInspectable>& items)
    {
        _syncingNativeReorder = true;
        auto endSync = wil::scope_exit([&]() noexcept {
            _syncingNativeReorder = false;
        });
        _tabItems.ReplaceAll(items);
        _syncDisplayItems();
    }

    void TabStrip::SetTabPinned(const MUX::Controls::TabViewItem& item, bool pinned)
    {
        if (const auto display = _displayItemForTab(item))
        {
            display.IsPinned(pinned);
            winrt::get_self<TabStripDisplayItem>(display)->UpdatePresentation(_isRailCollapsed);
        }
    }

    void TabStrip::BeginHeaderTransfer()
    {
        _pendingHeaderTransfers.clear();
        _deferringHeaderRestore = true;
    }

    void TabStrip::CompleteHeaderTransfer()
    {
        _deferringHeaderRestore = false;
        for (const auto& [_, pending] : _pendingHeaderTransfers)
        {
            if (const auto tab = pending.Item.get(); tab && pending.Header && !tab.Header())
            {
                tab.Header(pending.Header);
            }
        }
        _pendingHeaderTransfers.clear();
    }

    void TabStrip::RichTabRepositoryVisible(bool value)
    {
        if (value && (!_richTabGitAvailable || (!_richTabRepositoryVisible && _richTabMetadataSelectionCount() >= 2)))
        {
            RichTabRepositoryVisibleItem().IsChecked(false);
            return;
        }
        _richTabRepositoryVisible = value;
        RichTabRepositoryVisibleItem().IsChecked(value);
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::RichTabBranchVisible(bool value)
    {
        if (value && (!_richTabGitAvailable || (!_richTabBranchVisible && _richTabMetadataSelectionCount() >= 2)))
        {
            RichTabBranchVisibleItem().IsChecked(false);
            return;
        }
        _richTabBranchVisible = value;
        RichTabBranchVisibleItem().IsChecked(value);
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::RichTabAgentStatusVisible(bool value)
    {
        if (value && !_richTabAgentStatusVisible && _richTabMetadataSelectionCount() >= 2)
        {
            RichTabAgentStatusVisibleItem().IsChecked(false);
            return;
        }
        _richTabAgentStatusVisible = value;
        RichTabAgentStatusVisibleItem().IsChecked(value);
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::RichTabWorkingDirectoryVisible(bool value)
    {
        if (value && !_richTabWorkingDirectoryVisible && _richTabMetadataSelectionCount() >= 2)
        {
            RichTabWorkingDirectoryVisibleItem().IsChecked(false);
            return;
        }
        _richTabWorkingDirectoryVisible = value;
        RichTabWorkingDirectoryVisibleItem().IsChecked(value);
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::RichTabChangesVisible(bool value)
    {
        if (value && (!_richTabGitAvailable || (!_richTabChangesVisible && _richTabMetadataSelectionCount() >= 2)))
        {
            RichTabChangesVisibleItem().IsChecked(false);
            return;
        }
        _richTabChangesVisible = value;
        RichTabChangesVisibleItem().IsChecked(value);
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::RichTabMetadataControlsVisible(const bool value)
    {
        _richTabMetadataControlsVisible = value;
        const auto visibility = value ? Visibility::Visible : Visibility::Collapsed;
        FilterTabsButton().Visibility(value && !_isRailCollapsed ? Visibility::Visible : Visibility::Collapsed);
        RichTabMetadataSectionItem().Visibility(visibility);
        RichTabAgentStatusVisibleItem().Visibility(visibility);
        RichTabWorkingDirectoryVisibleItem().Visibility(visibility);
        RichTabRepositoryVisibleItem().Visibility(visibility);
        RichTabBranchVisibleItem().Visibility(visibility);
        RichTabChangesVisibleItem().Visibility(visibility);

        if (!value)
        {
            FilterTabsButton().Flyout().Hide();
            RichTabAgentStatusVisible(false);
            RichTabWorkingDirectoryVisible(false);
            RichTabRepositoryVisible(false);
            RichTabBranchVisible(false);
            RichTabChangesVisible(false);
        }
    }

    bool TabStrip::_isGitInstalled() noexcept
    {
        const auto isApplication = [](const wchar_t* candidate) {
            const std::filesystem::path path{ candidate };
            // Get-Command -CommandType Application excludes PowerShell scripts,
            // even when PATHEXT includes .PS1. Other extensions need not be in PATHEXT.
            std::error_code ec;
            return _wcsicmp(path.extension().c_str(), L".ps1") != 0 &&
                   std::filesystem::is_regular_file(path, ec);
        };
        wchar_t buffer[MAX_PATH];
        const auto envLength = GetEnvironmentVariableW(L"INTELLIGENT_TERMINAL_GIT_BINARY", nullptr, 0);
        if (envLength > 0)
        {
            std::wstring binary(envLength, L'\0');
            if (GetEnvironmentVariableW(L"INTELLIGENT_TERMINAL_GIT_BINARY", binary.data(), envLength) > 0)
            {
                if (isApplication(binary.c_str()))
                {
                    return true;
                }
                const auto pathLength = GetEnvironmentVariableW(L"PATH", nullptr, 0);
                if (pathLength > 0)
                {
                    std::wstring path(pathLength, L'\0');
                    if (GetEnvironmentVariableW(L"PATH", path.data(), pathLength) > 0)
                    {
                        if (const auto length = SearchPathW(path.c_str(), binary.c_str(), nullptr, MAX_PATH, buffer, nullptr);
                            length > 0 && length < MAX_PATH && isApplication(buffer))
                        {
                            return true;
                        }

                        const auto pathextLength = GetEnvironmentVariableW(L"PATHEXT", nullptr, 0);
                        std::wstring pathext = (pathextLength > 0) ? std::wstring(pathextLength, L'\0') : L".COM;.EXE;.BAT;.CMD";
                        if (pathextLength > 0)
                        {
                            GetEnvironmentVariableW(L"PATHEXT", pathext.data(), pathextLength);
                        }

                        for (const auto& ext : til::split_iterator{ std::wstring_view{ pathext }, L';' })
                        {
                            if (!ext.empty())
                            {
                                std::wstring extStr{ ext };
                                if (const auto length = SearchPathW(path.c_str(), binary.c_str(), extStr.c_str(), MAX_PATH, buffer, nullptr);
                                    length > 0 && length < MAX_PATH && isApplication(buffer))
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
                // Override was specified but could not be resolved as an executable:
                // provider.ps1 gives the override strict precedence and will fail,
                // so do not fall back to git.exe or SearchPathW without PATH.
                return false;
            }
        }

        const auto pathLength = GetEnvironmentVariableW(L"PATH", nullptr, 0);
        if (pathLength > 0)
        {
            std::wstring path(pathLength, L'\0');
            if (GetEnvironmentVariableW(L"PATH", path.data(), pathLength) > 0)
            {
                if (SearchPathW(path.c_str(), L"git", L".exe", MAX_PATH, buffer, nullptr) > 0)
                {
                    return true;
                }
            }
        }
        return false;
    }

    void TabStrip::RichTabGitAvailable(bool value)
    {
        if (_richTabGitAvailable != value)
        {
            _richTabGitAvailable = value;
            bool changedVisibility = false;
            if (!value)
            {
                if (_richTabRepositoryVisible)
                {
                    RichTabRepositoryVisible(false);
                    changedVisibility = true;
                }
                if (_richTabBranchVisible)
                {
                    RichTabBranchVisible(false);
                    changedVisibility = true;
                }
                if (_richTabChangesVisible)
                {
                    RichTabChangesVisible(false);
                    changedVisibility = true;
                }
            }
            _updateRichTabMetadataSelectionState();
            if (changedVisibility)
            {
                VisibleFieldsChanged.raise(*this, nullptr);
            }
        }
    }

    uint32_t TabStrip::_richTabMetadataSelectionCount() const noexcept
    {
        return static_cast<uint32_t>(_richTabAgentStatusVisible) +
               static_cast<uint32_t>(_richTabWorkingDirectoryVisible) +
               static_cast<uint32_t>(_richTabRepositoryVisible) +
               static_cast<uint32_t>(_richTabBranchVisible) +
               static_cast<uint32_t>(_richTabChangesVisible);
    }

    void TabStrip::_updateRichTabMetadataSelectionState()
    {
        const auto canSelectAnother = _richTabMetadataSelectionCount() < 2;

        RichTabAgentStatusVisibleItem().IsEnabled(_richTabAgentStatusVisible || canSelectAnother);
        RichTabWorkingDirectoryVisibleItem().IsEnabled(_richTabWorkingDirectoryVisible || canSelectAnother);
        RichTabRepositoryVisibleItem().IsEnabled(_richTabGitAvailable && (_richTabRepositoryVisible || canSelectAnother));
        RichTabBranchVisibleItem().IsEnabled(_richTabGitAvailable && (_richTabBranchVisible || canSelectAnother));
        RichTabChangesVisibleItem().IsEnabled(_richTabGitAvailable && (_richTabChangesVisible || canSelectAnother));
    }

    UIElement TabStrip::TopChromeContent()
    {
        return TopChromeContentPresenter().Content().try_as<UIElement>();
    }
    void TabStrip::TopChromeContent(UIElement const& value)
    {
        TopChromeContentPresenter().Content(value);
        TopChrome().Visibility(value ? Visibility::Visible : Visibility::Collapsed);
    }

    void TabStrip::OnRailToggleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        RailCollapseRequested.raise(*this, nullptr);
    }

    void TabStrip::OnCompactNewTabClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        CompactNewTabRequested.raise(*this, nullptr);
    }

    void TabStrip::OnCompactNewTabMenuClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        CompactNewTabMenuRequested.raise(*this, CompactNewTabMenuButton());
    }

    void TabStrip::OnHistoryClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        OpenHistory();
    }

    void TabStrip::OpenHistory()
    {
        if (_isRailCollapsed || !_projectionControlsEnabled)
        {
            return;
        }
        if (!_historyActive)
        {
            _agentFilterTelemetryPending = true;
        }
        HistoryActive(true);
        HistoryRequested.raise(*this, nullptr);
        HistorySearchTextBox().Focus(WUX::FocusState::Programmatic);
    }

    void TabStrip::OnHistoryCloseClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        HistoryClosed.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabRepositoryVisibleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        _keepRichTabMetadataFlyoutOpen = true;
        RichTabRepositoryVisible(RichTabRepositoryVisibleItem().IsChecked());
        VisibleFieldsChanged.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabBranchVisibleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        _keepRichTabMetadataFlyoutOpen = true;
        RichTabBranchVisible(RichTabBranchVisibleItem().IsChecked());
        VisibleFieldsChanged.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabAgentStatusVisibleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        _keepRichTabMetadataFlyoutOpen = true;
        RichTabAgentStatusVisible(RichTabAgentStatusVisibleItem().IsChecked());
        VisibleFieldsChanged.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabWorkingDirectoryVisibleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        _keepRichTabMetadataFlyoutOpen = true;
        RichTabWorkingDirectoryVisible(RichTabWorkingDirectoryVisibleItem().IsChecked());
        VisibleFieldsChanged.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabChangesVisibleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        _keepRichTabMetadataFlyoutOpen = true;
        RichTabChangesVisible(RichTabChangesVisibleItem().IsChecked());
        VisibleFieldsChanged.raise(*this, nullptr);
    }

    void TabStrip::OnRichTabMetadataFlyoutClosing(
        IInspectable const&,
        WUX::Controls::Primitives::FlyoutBaseClosingEventArgs const& e)
    {
        if (_keepRichTabMetadataFlyoutOpen)
        {
            _keepRichTabMetadataFlyoutOpen = false;
            e.Cancel(true);
        }
    }

    void TabStrip::OnShowAllTabsClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        if (_historyActive)
        {
            HistoryClosed.raise(*this, nullptr);
        }
        else
        {
            FilterMode(TerminalApp::TabStripFilterMode::AllTabs);
        }
    }

    void TabStrip::OnSearchToggleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        if (_isRailCollapsed)
        {
            RailCollapseRequested.raise(*this, nullptr);
            if (_isRailCollapsed)
            {
                SearchTabsButton().IsChecked(false);
                return;
            }
            SearchTabsButton().IsChecked(true);
        }

        _searchActive = SearchTabsButton().IsChecked().GetBoolean();
        if (!_searchActive)
        {
            _searchQuery.clear();
            _syncingSearchState = true;
            SearchTextBox().Text(L"");
            _syncingSearchState = false;
        }
        _updateSearchVisualState();
        SearchChanged.raise(*this, nullptr);

        if (_searchActive)
        {
            SearchTextBox().Focus(WUX::FocusState::Programmatic);
        }
    }

    void TabStrip::OnSearchPointerPressed(IInspectable const&, WUX::Input::PointerRoutedEventArgs const&)
    {
        if (!_searchActive)
        {
            SearchActivationRequested.raise(*this, nullptr);
        }
    }

    void TabStrip::OnSearchTextChanged(IInspectable const&, TextChangedEventArgs const&)
    {
        if (_syncingSearchState)
        {
            return;
        }

        _searchQuery = SearchTextBox().Text();
        _updateSearchVisualState();
        SearchChanged.raise(*this, nullptr);
    }

    void TabStrip::OnSearchBoxKeyDown(IInspectable const&, WUX::Input::KeyRoutedEventArgs const& e)
    {
        if (e.OriginalKey() == Windows::System::VirtualKey::Escape)
        {
            _searchActive = false;
            _searchQuery.clear();
            _syncingSearchState = true;
            SearchTextBox().Text(L"");
            _syncingSearchState = false;
            _updateSearchVisualState();
            SearchChanged.raise(*this, nullptr);
            e.Handled(true);
        }
    }

    void TabStrip::OnHistorySearchTextChanged(IInspectable const&, TextChangedEventArgs const&)
    {
        if (_syncingHistorySearchState)
        {
            return;
        }

        _historySearchQuery = HistorySearchTextBox().Text();
        _applyHistoryProjection();
    }

    void TabStrip::OnHistorySearchBoxKeyDown(IInspectable const&, WUX::Input::KeyRoutedEventArgs const& e)
    {
        if (e.OriginalKey() != Windows::System::VirtualKey::Escape)
        {
            return;
        }

        if (!_historySearchQuery.empty())
        {
            ClearHistorySearch();
            HistorySearchTextBox().Focus(WUX::FocusState::Programmatic);
        }
        else
        {
            HistoryClosed.raise(*this, nullptr);
        }
        e.Handled(true);
    }

    void TabStrip::OnHistoryItemClick(IInspectable const&, ItemClickEventArgs const& e)
    {
        if (_historyActivating || _historyLoading)
        {
            return;
        }
        if (const auto item = e.ClickedItem().try_as<TerminalApp::TabStripHistoryItem>())
        {
            HistoryActivationRequested.raise(
                *this,
                winrt::make<TabStripHistoryActivationEventArgs>(item));
        }
    }

    void TabStrip::OnHistoryRowLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        const auto root = sender.as<FrameworkElement>();
        const auto item = root.DataContext().try_as<TerminalApp::TabStripHistoryItem>();
        _applyHistoryRowForeground(root, item);
    }

    void TabStrip::OnHistoryContainerContentChanging(ListViewBase const&, ContainerContentChangingEventArgs const& e)
    {
        if (const auto container = e.ItemContainer())
        {
            const auto item = e.InRecycleQueue() ? nullptr : e.Item().try_as<TerminalApp::TabStripHistoryItem>();
            _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(),
                                       item);
            _applyHistoryRowAutomation(container, item);
        }
    }

    void TabStrip::OnContainerContentChanging(ListViewBase const&,
                                               ContainerContentChangingEventArgs const& e)
    {
        const auto container = e.ItemContainer().try_as<ListViewItem>();
        if (!container)
        {
            return;
        }

        if (e.InRecycleQueue())
        {
            _updateDisplayItemVisuals(container.ContentTemplateRoot().try_as<FrameworkElement>(), nullptr);
            container.Visibility(Visibility::Visible);
            return;
        }

        if (const auto item = _tabFromItem(e.Item()))
        {
            _applyTabItemVisibility(item, container);
        }
        else
        {
            container.Visibility(Visibility::Visible);
        }
        _updateDisplayItemVisuals(container.ContentTemplateRoot().try_as<FrameworkElement>(),
                                  e.Item().try_as<TerminalApp::TabStripDisplayItem>());
    }

    void TabStrip::_applyRailState()
    {
        const auto expandedVisibility = _isRailCollapsed ? Visibility::Collapsed : Visibility::Visible;
        const auto collapsedVisibility = _isRailCollapsed ? Visibility::Visible : Visibility::Collapsed;

        MinWidth(_isRailCollapsed ? 40.0 : 180.0);
        CompactNewTabToolbar().Visibility(collapsedVisibility);
        VerticalTabsHeader().Visibility(expandedVisibility);
        SearchTabsButton().IsHitTestVisible(true);
        SearchTabsButton().IsEnabled(_projectionControlsEnabled);
        FilterTabsButton().Visibility(_richTabMetadataControlsVisible ? expandedVisibility : Visibility::Collapsed);
        FilterTabsButton().IsHitTestVisible(!_isRailCollapsed);
        FilterTabsButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
        TabHistoryButton().Visibility(expandedVisibility);
        TabHistoryButton().IsHitTestVisible(!_isRailCollapsed);
        TabHistoryButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
        FilterStatusBar().IsHitTestVisible(!_isRailCollapsed);
        ItemsList().AllowDrop(ItemsList().CanDragItems() && !_isRailCollapsed);
        TabsToolbar().Padding(_isRailCollapsed ? WUX::Thickness{} : WUX::Thickness{ 12, 0, 8, 0 });
        WUX::Controls::Grid::SetColumn(SearchTabsButton(), _isRailCollapsed ? 0 : 1);
        WUX::Controls::Grid::SetColumnSpan(SearchTabsButton(), _isRailCollapsed ? 4 : 1);
        SearchTabsButton().Width(40.0);
        SearchTabsButton().Height(40.0);

        if (_isRailCollapsed)
        {
            if (const auto flyout = FilterTabsButton().Flyout())
            {
                flyout.Hide();
            }
        }

        for (uint32_t index = 0; index < _tabItems.Size(); ++index)
        {
            if (const auto item = _tabItems.GetAt(index).try_as<MUX::Controls::TabViewItem>())
            {
                _applyTabItemVisibility(item);
            }
            if (const auto display = _displayItemAt(index))
            {
                winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
            }
        }

        _updateSearchVisualState();
        _updateHistoryVisualState();
    }

    void TabStrip::_applyTabItemRailState(MUX::Controls::TabViewItem const& item)
    {
        if (const auto header = item.Header().try_as<UIElement>())
        {
            header.Visibility(Visibility::Visible);
        }

        item.Height(32.0);
        item.CornerRadius(WUX::CornerRadius{ 6.0, 6.0, 6.0, 6.0 });
        item.VerticalContentAlignment(WUX::VerticalAlignment::Center);
        _applyVerticalTabChrome(item);

        if (_isRailCollapsed)
        {
            item.Width(40.0);
            item.MinWidth(40.0);
            item.MaxWidth(40.0);
        }
        else
        {
            item.Width(std::numeric_limits<double>::quiet_NaN());
            item.MinWidth(0.0);
            item.MaxWidth(std::numeric_limits<double>::infinity());
        }

        _refreshCloseButton(item);
        if (const auto display = _displayItemForTab(item))
        {
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
        }
    }

    void TabStrip::_restoreTabItemRailState(MUX::Controls::TabViewItem const& item)
    {
        item.ClearValue(WUX::FrameworkElement::HeightProperty());
        item.ClearValue(WUX::Controls::Control::CornerRadiusProperty());
        item.ClearValue(WUX::Controls::Control::VerticalContentAlignmentProperty());
        _restoreTabChrome(item);
    }

    void TabStrip::_applyTabItemVisibility(MUX::Controls::TabViewItem const& item)
    {
        uint32_t index{};
        if (_tabItems.IndexOf(item, index))
        {
            if (const auto container = ItemsList().ContainerFromIndex(index).try_as<ListViewItem>())
            {
                _applyTabItemVisibility(item, container);
            }
        }
    }

    void TabStrip::_applyTabItemVisibility(MUX::Controls::TabViewItem const& item,
                                            ListViewItem const& container)
    {
        bool visible = true;
        if (const auto desired = _tabItemVisibility.find(winrt::get_abi(item));
            desired != _tabItemVisibility.end())
        {
            if (const auto storedItem = desired->second.Item.get();
                storedItem && winrt::get_abi(storedItem) == winrt::get_abi(item))
            {
                visible = desired->second.Visible;
            }
        }
        container.Visibility(visible ? Visibility::Visible : Visibility::Collapsed);
    }

    void TabStrip::_pruneTabItemVisibility()
    {
        for (auto it = _tabItemVisibility.begin(); it != _tabItemVisibility.end();)
        {
            const auto item = it->second.Item.get();
            uint32_t index{};
            if (!item || !_tabItems.IndexOf(item, index))
            {
                it = _tabItemVisibility.erase(it);
            }
            else
            {
                ++it;
            }
        }
    }

    void TabStrip::_setSearchPanelExpanded(const bool expanded, const bool animate)
    {
        if (_searchPanelExpanded == expanded && !_searchPanelStoryboard)
        {
            return;
        }

        _searchPanelExpanded = expanded;
        const auto generation = ++_searchAnimationGeneration;
        const auto panel = SearchPanel();
        const auto startHeight = panel.ActualHeight();
        const auto startOpacity = panel.Opacity();

        if (_searchPanelStoryboard)
        {
            _searchPanelStoryboard.Stop();
            _searchPanelStoryboard = nullptr;
        }

        panel.Visibility(Visibility::Visible);
        panel.IsHitTestVisible(expanded);

        if (!animate)
        {
            panel.Height(expanded ? SearchPanelExpandedHeight : 0.0);
            panel.Opacity(expanded ? 1.0 : 0.0);
            panel.Visibility(expanded ? Visibility::Visible : Visibility::Collapsed);
            return;
        }

        namespace Animation = WUX::Media::Animation;
        const auto duration = DurationHelper::FromTimeSpan(TimeSpan{ SearchPanelAnimationDuration });

        Animation::DoubleAnimation heightAnimation;
        heightAnimation.Duration(duration);
        heightAnimation.From(startHeight);
        heightAnimation.To(expanded ? SearchPanelExpandedHeight : 0.0);
        auto heightEasing = Animation::QuadraticEase{};
        heightEasing.EasingMode(Animation::EasingMode::EaseOut);
        heightAnimation.EasingFunction(heightEasing);
        heightAnimation.EnableDependentAnimation(true);

        Animation::DoubleAnimation opacityAnimation;
        opacityAnimation.Duration(duration);
        opacityAnimation.From(startOpacity);
        opacityAnimation.To(expanded ? 1.0 : 0.0);
        auto opacityEasing = Animation::QuadraticEase{};
        opacityEasing.EasingMode(Animation::EasingMode::EaseOut);
        opacityAnimation.EasingFunction(opacityEasing);
        opacityAnimation.EnableDependentAnimation(true);

        Animation::Storyboard storyboard;
        storyboard.Duration(duration);
        storyboard.FillBehavior(Animation::FillBehavior::Stop);
        storyboard.Children().Append(heightAnimation);
        storyboard.Children().Append(opacityAnimation);
        storyboard.SetTarget(heightAnimation, panel);
        storyboard.SetTargetProperty(heightAnimation, L"Height");
        storyboard.SetTarget(opacityAnimation, panel);
        storyboard.SetTargetProperty(opacityAnimation, L"Opacity");

        heightAnimation.Completed([weakThis{ get_weak() }, generation, expanded](auto&&, auto&&) {
            if (const auto self = weakThis.get();
                self && self->_searchAnimationGeneration == generation)
            {
                const auto panel = self->SearchPanel();
                panel.Height(expanded ? SearchPanelExpandedHeight : 0.0);
                panel.Opacity(expanded ? 1.0 : 0.0);
                panel.Visibility(expanded ? Visibility::Visible : Visibility::Collapsed);
                self->_searchPanelStoryboard = nullptr;
            }
        });

        _searchPanelStoryboard = storyboard;
        storyboard.Begin();
    }

    void TabStrip::_updateSearchVisualState()
    {
        _syncingSearchState = true;
        SearchTabsButton().IsChecked(_searchActive);
        _syncingSearchState = false;

        const auto expanded = _searchActive && !_isRailCollapsed && !_historyActive;
        _setSearchPanelExpanded(expanded, _searchAnimationEnabled && !_isRailCollapsed && !_historyActive);
    }

    std::vector<winrt::hstring> TabStrip::_buildHistorySearchTerms(TerminalApp::TabStripHistoryItem const& item)
    {
        std::vector<winrt::hstring> terms;
        const auto append = [&terms](const winrt::hstring& value) {
            if (!value.empty())
            {
                terms.emplace_back(value);
            }
        };

        append(item.Title());
        append(item.Subtitle() + item.StatusText());
        append(item.AgentId());
        append(item.ProviderDisplayName());
        append(item.AgentSource());
        append(item.WslDistro());
        append(item.Status());
        if (item.IsLive())
        {
            terms.emplace_back(L"live");
        }
        else if (item.IsHistorical())
        {
            terms.emplace_back(L"history");
        }
        return terms;
    }

    bool TabStrip::_matchesHistorySearch(const size_t index) const
    {
        if (_historySearchQuery.empty())
        {
            return true;
        }

        const std::wstring_view query{ _historySearchQuery.c_str(), _historySearchQuery.size() };
        for (const auto& value : _historySearchTerms.at(index))
        {
            const std::wstring_view candidate{ value.c_str(), value.size() };
            if (query.size() > candidate.size())
            {
                continue;
            }

            for (size_t offset = 0; offset + query.size() <= candidate.size(); ++offset)
            {
                if (til::compare_ordinal_insensitive(candidate.substr(offset, query.size()), query) == 0)
                {
                    return true;
                }
            }
        }
        return false;
    }

    static bool _sameHistoryItem(TerminalApp::TabStripHistoryItem const& left,
                                 TerminalApp::TabStripHistoryItem const& right)
    {
        return left == right ||
               (left.SessionId() == right.SessionId() &&
                left.AgentId() == right.AgentId() &&
                left.AgentSource() == right.AgentSource() &&
                left.WslDistro() == right.WslDistro() &&
                left.SessionUniverse() == right.SessionUniverse() &&
                left.Title() == right.Title() &&
                left.Subtitle() == right.Subtitle() &&
                left.Status() == right.Status() &&
                left.StatusText() == right.StatusText() &&
                left.SearchQuery() == right.SearchQuery() &&
                left.ProviderDisplayName() == right.ProviderDisplayName() &&
                left.Cwd() == right.Cwd() &&
                left.PaneSessionId() == right.PaneSessionId() &&
                left.IsLive() == right.IsLive() &&
                left.IsAgentPane() == right.IsAgentPane() &&
                left.IsHistorical() == right.IsHistorical() &&
                left.StatusTextStyle() == right.StatusTextStyle() &&
                left.IconTemplate() == right.IconTemplate());
    }

    void TabStrip::_applyHistoryProjection(const bool preserveScroll)
    {
        std::vector<TerminalApp::TabStripHistoryItem> visibleItems;
        visibleItems.reserve(_historySnapshot.size());
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            _historySnapshot[index].SearchQuery(_historySearchQuery);
            if (_matchesHistorySearch(index))
            {
                visibleItems.emplace_back(_historySnapshot[index]);
            }
        }
        if (preserveScroll)
        {
            // A collection reset discards ListView's viewport. Update slots instead;
            // KeepScrollOffset preserves the user's position without a deferred scroll.
            for (uint32_t index = 0; index < visibleItems.size(); ++index)
            {
                if (index >= _historyItems.Size())
                {
                    _historyItems.Append(visibleItems[index]);
                }
                else if (!_sameHistoryItem(_historyItems.GetAt(index), visibleItems[index]))
                {
                    _historyItems.SetAt(index, visibleItems[index]);
                }
            }
            while (_historyItems.Size() > visibleItems.size())
            {
                _historyItems.RemoveAtEnd();
            }
        }
        else
        {
            _historyItems.ReplaceAll(visibleItems);
        }
        _updateHistoryVisualState();
        HistoryProjectionChanged.raise();
    }

    void TabStrip::_updateHistoryVisualState()
    {
        const auto visible = _historyActive && !_isRailCollapsed;
        HistoryPanel().Visibility(visible ? Visibility::Visible : Visibility::Collapsed);
        TabsToolbar().Visibility(visible ? Visibility::Collapsed : Visibility::Visible);
        ItemsList().Visibility(_tabsVisible && !visible ? Visibility::Visible : Visibility::Collapsed);
        HistoryLoadingIndicator().IsActive(visible && _historyLoading);
        HistoryLoadingIndicator().Visibility(visible && _historyLoading ? Visibility::Visible : Visibility::Collapsed);
        HistoryList().IsItemClickEnabled(!_historyActivating && !_historyLoading);
        const auto showList = visible && !_historyLoading && _historyItems.Size() > 0;
        HistoryList().Visibility(showList ?
                                     Visibility::Visible :
                                     Visibility::Collapsed);
        Grid::SetRow(HistoryMessage(), showList ? 1 : 0);
        if (!visible || _historyLoading)
        {
            HistoryMessage().Visibility(Visibility::Collapsed);
        }
        else if (const auto error = HistoryError(); !error.empty())
        {
            HistoryMessage().Text(error);
            HistoryMessage().Visibility(Visibility::Visible);
        }
        else if (_historyItems.Size() == 0)
        {
            HistoryMessage().Text(_historySnapshot.empty() ?
                                      RS_(L"VerticalTabsHistoryEmpty") :
                                      RS_(L"VerticalTabsHistoryNoMatches"));
            HistoryMessage().Visibility(Visibility::Visible);
        }
        else
        {
            HistoryMessage().Visibility(Visibility::Collapsed);
        }
    }

    void TabStrip::_onItemsVectorChanged(IObservableVector<IInspectable> const& sender,
                                          IVectorChangedEventArgs const& args)
    {
        if (_syncingNativeReorder)
        {
            TabItemsChanged.raise(*this, args);
            return;
        }

        if (args.CollectionChange() != CollectionChange::ItemInserted)
        {
            _pruneTabItemVisibility();
        }

        if (_draggingItem)
        {
            _dragCollectionChanged = true;
            _syncDisplayItems();
            _removeStaleCloseRequestedSubscriptions(sender);
            TabItemsChanged.raise(*this, args);
            return;
        }

        // Sync per-item CloseRequested subscriptions on add/remove.
        // (Reset covers bulk clear; individual changes cover the common paths.)
        switch (args.CollectionChange())
        {
        case CollectionChange::ItemInserted:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _displayItems.InsertAt(args.Index(), _makeDisplayItem(item));
                _applyTabItemVisibility(item);
            }
            break;
        case CollectionChange::ItemRemoved:
            if (args.Index() < _displayItems.Size())
            {
                _restoreDisplayItemHeader(_displayItems.GetAt(args.Index()));
                _displayItems.RemoveAt(args.Index());
            }
            break;
        case CollectionChange::ItemChanged:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                if (args.Index() < _displayItems.Size())
                {
                    _restoreDisplayItemHeader(_displayItems.GetAt(args.Index()));
                }
                _displayItems.SetAt(args.Index(), _makeDisplayItem(item));
                _applyTabItemVisibility(item);
            }
            break;
        case CollectionChange::Reset:
            _syncDisplayItems();
            _clearCloseRequestedSubscriptions();
            _tabItemVisibility.clear();
            for (uint32_t i = 0; i < sender.Size(); ++i)
            {
                if (const auto item = sender.GetAt(i).try_as<MUX::Controls::TabViewItem>())
                {
                    _applyTabItemVisibility(item);
                }
            }
            break;
        }

        _removeStaleCloseRequestedSubscriptions(sender);
        TabItemsChanged.raise(*this, args);
    }

    TerminalApp::TabStripDisplayItem TabStrip::_displayItemAt(uint32_t index) const
    {
        return index < _displayItems.Size() ? _displayItems.GetAt(index) : nullptr;
    }

    TerminalApp::TabStripDisplayItem TabStrip::_displayItemForTab(MUX::Controls::TabViewItem const& tab) const
    {
        uint32_t index{};
        return tab && _tabItems.IndexOf(tab, index) ? _displayItemAt(index) : nullptr;
    }

    TerminalApp::TabStripDisplayItem TabStrip::_makeDisplayItem(MUX::Controls::TabViewItem const& tab)
    {
        auto header = tab.Header();
        tab.Header(nullptr);
        auto display = winrt::make<TabStripDisplayItem>(tab, std::move(header));
        auto title = WUX::Automation::AutomationProperties::GetName(tab);
        if (title.empty())
        {
            title = RS_(L"VerticalTabsFallbackTabTitle");
        }
        display.Title(title);
        winrt::get_self<TabStripDisplayItem>(display)->SyncIcon({});
        const auto key = winrt::get_abi(tab);
        if (const auto found = _groupExpansion.find(key);
            found != _groupExpansion.end() && found->second.Item.get())
        {
            display.IsExpanded(found->second.Expanded);
        }
        else
        {
            _groupExpansion.insert_or_assign(key, GroupExpansionState{ winrt::make_weak(tab), true });
        }
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed);
        return display;
    }

    void TabStrip::_restoreDisplayItemHeader(TerminalApp::TabStripDisplayItem const& display)
    {
        if (display)
        {
            const auto tab = display.Tab();
            const auto header = display.Header();
            if (tab && header && !tab.Header())
            {
                auto current = header.try_as<DependencyObject>();
                while (current)
                {
                    const auto parent = WUX::Media::VisualTreeHelper::GetParent(current);
                    if (const auto presenter = parent.try_as<ContentPresenter>())
                    {
                        if (winrt::get_abi(presenter.Content()) == winrt::get_abi(header))
                        {
                            presenter.Content(nullptr);
                            break;
                        }
                    }
                    current = parent;
                }
                display.Header(nullptr);
                if (const auto headerControl = header.try_as<TerminalApp::TabHeaderControl>())
                {
                    headerControl.IsMetadataVisible(!headerControl.MetadataText().empty());
                }
                if (_deferringHeaderRestore)
                {
                    _pendingHeaderTransfers.insert_or_assign(
                        winrt::get_abi(tab),
                        PendingHeaderTransfer{ winrt::make_weak(tab), header });
                }
                else
                {
                    tab.Header(header);
                }
            }
        }
    }

    MUX::Controls::TabViewItem TabStrip::_tabFromItem(IInspectable const& item)
    {
        if (const auto tab = item.try_as<MUX::Controls::TabViewItem>())
        {
            return tab;
        }
        if (const auto display = item.try_as<TerminalApp::TabStripDisplayItem>())
        {
            return display.Tab();
        }
        return nullptr;
    }

    void TabStrip::_syncDisplayItems()
    {
        const auto previousSelection = SelectedItem();
        std::unordered_map<void*, TerminalApp::TabStripDisplayItem> existing;
        for (const auto& display : _displayItems)
        {
            if (const auto tab = display.Tab())
            {
                existing.emplace(winrt::get_abi(tab), display);
            }
        }

        std::vector<TerminalApp::TabStripDisplayItem> next;
        next.reserve(_tabItems.Size());
        for (const auto& value : _tabItems)
        {
            if (const auto tab = value.try_as<MUX::Controls::TabViewItem>())
            {
                if (const auto found = existing.find(winrt::get_abi(tab)); found != existing.end())
                {
                    next.emplace_back(found->second);
                }
                else
                {
                    next.emplace_back(_makeDisplayItem(tab));
                }
            }
        }
        for (const auto& [_, display] : existing)
        {
            uint32_t index{};
            if (!_tabItems.IndexOf(display.Tab(), index))
            {
                _restoreDisplayItemHeader(display);
            }
        }
        _displayItems.ReplaceAll(next);
        SelectedItem(previousSelection);
    }

    void TabStrip::_hookCloseRequested(MUX::Controls::TabViewItem const& item)
    {
        const auto key = winrt::get_abi(item);
        if (_closeRequestedSubscriptions.contains(key))
        {
            return;
        }

        const auto weakThis = get_weak();
        const auto weakItem = winrt::make_weak(item);
        const auto loadedToken = item.Loaded([weakThis, weakItem](auto&&, auto&&) {
            const auto self = weakThis.get();
            const auto tab = weakItem.get();
            if (self && tab)
            {
                self->_applyTabItemRailState(tab);
                self->_applyTabItemVisibility(tab);
            }
        });
        const auto layoutUpdatedToken = item.LayoutUpdated([weakThis, weakItem](auto&&, auto&&) {
            const auto self = weakThis.get();
            const auto tab = weakItem.get();
            if (self && tab && _applyVerticalTabChrome(tab))
            {
                self->_refreshCloseButton(tab);
                if (const auto found = self->_closeRequestedSubscriptions.find(winrt::get_abi(tab));
                    found != self->_closeRequestedSubscriptions.end())
                {
                    tab.LayoutUpdated(found->second.LayoutUpdatedToken);
                    found->second.LayoutUpdatedToken = {};
                }
            }
        });
        _closeRequestedSubscriptions.emplace(key, CloseRequestedSubscription{ weakItem, loadedToken, layoutUpdatedToken });
        _refreshCloseButton(item);
    }

    void TabStrip::_refreshCloseButton(MUX::Controls::TabViewItem const& item)
    {
        const auto key = winrt::get_abi(item);
        if (const auto found = _closeRequestedSubscriptions.find(key);
            found != _closeRequestedSubscriptions.end())
        {
            item.ApplyTemplate();
            if (const auto button = _findCloseButton(item))
            {
                button.IsHitTestVisible(!_isRailCollapsed);

                if (const auto currentButton = found->second.CloseButton.get();
                    currentButton && currentButton == button)
                {
                    return;
                }

                if (const auto currentButton = found->second.CloseButton.get())
                {
                    currentButton.Click(found->second.ClickToken);
                }

                const auto weakThis = get_weak();
                const auto weakItem = winrt::make_weak(item);
                found->second.ClickToken = button.Click([weakThis, weakItem](auto&&, auto&&) {
                    const auto self = weakThis.get();
                    const auto tab = weakItem.get();
                    if (self && tab)
                    {
                        auto args = winrt::make_self<TabStripCloseRequestedEventArgs>(tab);
                        self->TabCloseRequested.raise(*self, *args);
                    }
                });
                found->second.CloseButton = winrt::make_weak(button);
            }
        }
    }

    void TabStrip::_removeStaleCloseRequestedSubscriptions(IObservableVector<IInspectable> const& items)
    {
        for (auto it = _closeRequestedSubscriptions.begin(); it != _closeRequestedSubscriptions.end();)
        {
            bool stillPresent = false;
            for (uint32_t index = 0; index < items.Size(); ++index)
            {
                if (winrt::get_abi(items.GetAt(index)) == it->first)
                {
                    stillPresent = true;
                    break;
                }
            }

            if (stillPresent)
            {
                ++it;
            }
            else
            {
                if (const auto item = it->second.Item.get())
                {
                    if (it->second.LayoutUpdatedToken.value)
                    {
                        item.LayoutUpdated(it->second.LayoutUpdatedToken);
                    }
                    _restoreTabItemRailState(item);
                    item.Loaded(it->second.LoadedToken);
                }
                if (const auto button = it->second.CloseButton.get())
                {
                    button.Click(it->second.ClickToken);
                }
                const auto key = it->first;
                it = _closeRequestedSubscriptions.erase(it);
                _tabItemVisibility.erase(key);
            }
        }
    }

    void TabStrip::_clearCloseRequestedSubscriptions()
    {
        for (const auto& [_, subscription] : _closeRequestedSubscriptions)
        {
            if (const auto item = subscription.Item.get())
            {
                if (subscription.LayoutUpdatedToken.value)
                {
                    item.LayoutUpdated(subscription.LayoutUpdatedToken);
                }
                _restoreTabItemRailState(item);
                item.Loaded(subscription.LoadedToken);
            }
            if (const auto button = subscription.CloseButton.get())
            {
                button.Click(subscription.ClickToken);
            }
        }
        _closeRequestedSubscriptions.clear();
    }

    WUX::Automation::Peers::AutomationPeer TabStrip::OnCreateAutomationPeer()
    {
        return winrt::make<TabStripAutomationPeer>(*this);
    }

    void TabStrip::_onListKeyDown(IInspectable const&, WUX::Input::KeyRoutedEventArgs const& e)
    {
        if (e.OriginalKey() != Windows::System::VirtualKey::Enter)
        {
            return;
        }

        const auto coreWindow = winrt::Windows::UI::Core::CoreWindow::GetForCurrentThread();
        if (!coreWindow)
        {
            return;
        }
        constexpr auto down = winrt::Windows::UI::Core::CoreVirtualKeyStates::Down;
        if (WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Control), down) ||
            WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Menu), down) ||
            WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Shift), down))
        {
            return;
        }

        const auto selectedItem = ItemsList().SelectedItem();
        const auto root = XamlRoot();
        if (!selectedItem || !root)
        {
            return;
        }

        const auto selectedContainer = ItemsList().ContainerFromItem(selectedItem).try_as<ListViewItem>();
        const auto focused = WUX::Input::FocusManager::GetFocusedElement(root).try_as<ListViewItem>();
        if (selectedContainer && focused && selectedContainer == focused)
        {
            if (const auto tab = _tabFromItem(selectedItem))
            {
                TabFocusRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(tab));
                e.Handled(true);
            }
        }
    }

    void TabStrip::OnListSelectionChanged(IInspectable const& /*sender*/,
                                           SelectionChangedEventArgs const& e)
    {
        // Keep TabViewItem selection synchronized for close-button and
        // accessibility behavior. ListView renders the interaction states.
        for (const auto& removed : e.RemovedItems())
        {
            if (auto item = _tabFromItem(removed))
            {
                item.IsSelected(false);
            }
            if (const auto display = removed.try_as<TerminalApp::TabStripDisplayItem>())
            {
                display.SelectionVisibility(Visibility::Collapsed);
                _refreshDisplayItemVisuals(display);
            }
        }
        for (const auto& added : e.AddedItems())
        {
            if (auto item = _tabFromItem(added))
            {
                item.IsSelected(true);
            }
            if (const auto display = added.try_as<TerminalApp::TabStripDisplayItem>())
            {
                display.SelectionVisibility(Visibility::Visible);
                _refreshDisplayItemVisuals(display);
            }
        }

        IInspectable added = e.AddedItems().Size() > 0 ? _tabFromItem(e.AddedItems().GetAt(0)) : nullptr;
        IInspectable removed = e.RemovedItems().Size() > 0 ? _tabFromItem(e.RemovedItems().GetAt(0)) : nullptr;
        auto args = winrt::make_self<TabStripSelectionChangedEventArgs>(std::move(added), std::move(removed));
        SelectionChanged.raise(*this, *args);
    }

    void TabStrip::OnDragItemsStarting(IInspectable const& /*sender*/,
                                        DragItemsStartingEventArgs const& e)
    {
        // ListView packs the dragged items into e.Items(); for SelectionMode=Single
        // there's at most one. Wrap it in a TabView-shaped args object so
        // TerminalPage's tearoff-setup code can look identical to the horizontal path.
        // The vertical list contains descriptors, so map the surfaced item back
        // to the canonical TabViewItem before entering the shared drag pipeline.
        IInspectable surfacedItem = e.Items().Size() > 0 ? e.Items().GetAt(0) : nullptr;
        auto tab = _tabFromItem(surfacedItem);
        // Stash the resolved TabViewItem (not the surfaced Border) so
        // OnDragItemsCompleted's tearoff path also gets the right identity.
        _draggingItem = tab ? IInspectable{ tab } : surfacedItem;
        _dragCollectionChanged = false;
        uint32_t draggingIndex{};
        _draggingIndex = tab && _tabItems.IndexOf(tab, draggingIndex) ?
                             std::optional<uint32_t>{ draggingIndex } :
                             std::nullopt;

        auto args = winrt::make_self<TabStripDragStartingEventArgs>(tab, _draggingItem, e.Data());
        TabDragStarting.raise(*this, *args);

        if (args->Cancel())
        {
            e.Cancel(true);
            _draggingItem = nullptr;
            _draggingIndex.reset();
            _dragCollectionChanged = false;
        }
    }

    void TabStrip::OnGroupToggleClick(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto element = sender.try_as<FrameworkElement>())
        {
            if (const auto display = element.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
            {
                _toggleGroup(display);
            }
        }
    }

    void TabStrip::_toggleGroup(TerminalApp::TabStripDisplayItem const& display)
    {
        display.IsExpanded(!display.IsExpanded());
        _groupExpansion.insert_or_assign(
            winrt::get_abi(display.Tab()),
            GroupExpansionState{ winrt::make_weak(display.Tab()), display.IsExpanded() });
        winrt::get_self<TabStripDisplayItem>(display)->UpdatePresentation(_isRailCollapsed);
        _refreshDisplayItemVisuals(display);
        GroupExpansionChanged.raise(*this, display.Tab());
    }

    void TabStrip::OnPanePointerPressed(IInspectable const& sender,
                                         WUX::Input::PointerRoutedEventArgs const& e)
    {
        const auto element = sender.try_as<FrameworkElement>();
        const auto pane = element ? element.DataContext().try_as<TerminalApp::TabStripPaneItem>() : nullptr;
        if (!pane)
        {
            return;
        }

        const auto properties = e.GetCurrentPoint(element).Properties();
        if (properties.IsMiddleButtonPressed())
        {
            e.Handled(true);
            PaneCloseRequested.raise(*this, winrt::make<TabStripPaneEventArgs>(pane.Tab(), pane.ContentId()));
        }
    }

    void TabStrip::OnPanePointerEntered(IInspectable const& sender,
                                        WUX::Input::PointerRoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto background = root.FindName(L"PaneHoverBackground").try_as<UIElement>())
            {
                background.Opacity(1.0);
            }
        }
    }

    void TabStrip::OnPanePointerExited(IInspectable const& sender,
                                       WUX::Input::PointerRoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto background = root.FindName(L"PaneHoverBackground").try_as<UIElement>())
            {
                background.Opacity(0.0);
            }
        }
    }

    void TabStrip::OnPaneActivateClick(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto element = sender.try_as<FrameworkElement>())
        {
            if (const auto pane = element.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                PaneActivationRequested.raise(*this, winrt::make<TabStripPaneEventArgs>(pane.Tab(), pane.ContentId()));
            }
        }
    }

    void TabStrip::OnPaneDoubleTapped(IInspectable const&,
                                       WUX::Input::DoubleTappedRoutedEventArgs const& e)
    {
        e.Handled(true);
    }

    void TabStrip::OnPaneRightTapped(IInspectable const&,
                                      WUX::Input::RightTappedRoutedEventArgs const& e)
    {
        e.Handled(true);
    }

    void TabStrip::OnPaneCloseClick(IInspectable const& sender, RoutedEventArgs const& e)
    {
        static_cast<void>(e);
        if (const auto element = sender.try_as<FrameworkElement>())
        {
            if (const auto pane = element.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                PaneCloseRequested.raise(*this, winrt::make<TabStripPaneEventArgs>(pane.Tab(), pane.ContentId()));
            }
        }
    }

    void TabStrip::OnTabHeaderPointerPressed(IInspectable const& sender,
                                              WUX::Input::PointerRoutedEventArgs const& e)
    {
        const auto element = sender.try_as<FrameworkElement>();
        const auto display = element ? element.DataContext().try_as<TerminalApp::TabStripDisplayItem>() : nullptr;
        if (!display)
        {
            return;
        }

        const auto properties = e.GetCurrentPoint(element).Properties();
        if (properties.IsMiddleButtonPressed())
        {
            _pressedHeaderTab = {};
            _pressedHeaderWasSelected = false;
            e.Handled(true);
            TabCloseRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(display.Tab()));
        }
        else if (properties.IsLeftButtonPressed() && !_originatesFromHeaderControl(e.OriginalSource(), element))
        {
            _pressedHeaderTab = winrt::make_weak(display.Tab());
            const auto selectedTab = _tabFromItem(ItemsList().SelectedItem());
            _pressedHeaderWasSelected = selectedTab &&
                                        winrt::get_abi(selectedTab) == winrt::get_abi(display.Tab());
        }
        else
        {
            _pressedHeaderTab = {};
            _pressedHeaderWasSelected = false;
        }
    }

    void TabStrip::OnTabHeaderTapped(IInspectable const& sender,
                                      WUX::Input::TappedRoutedEventArgs const& e)
    {
        const auto element = sender.try_as<FrameworkElement>();
        const auto display = element ? element.DataContext().try_as<TerminalApp::TabStripDisplayItem>() : nullptr;
        const auto pressedTab = _pressedHeaderTab.get();
        const auto shouldToggle = display &&
                                  pressedTab &&
                                  _pressedHeaderWasSelected &&
                                  display.GroupVisibility() == Visibility::Visible &&
                                  winrt::get_abi(pressedTab) == winrt::get_abi(display.Tab()) &&
                                  !_originatesFromHeaderControl(e.OriginalSource(), element);
        const auto shouldFocus = display &&
                                 pressedTab &&
                                 _pressedHeaderWasSelected &&
                                 winrt::get_abi(pressedTab) == winrt::get_abi(display.Tab()) &&
                                 !_originatesFromHeaderControl(e.OriginalSource(), element);

        _pressedHeaderTab = {};
        _pressedHeaderWasSelected = false;

        if (shouldToggle)
        {
            _toggleGroup(display);
            e.Handled(true);
        }
        if (shouldFocus)
        {
            TabFocusRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(display.Tab()));
        }
    }

    void TabStrip::OnTabHeaderDoubleTapped(IInspectable const& sender,
                                            WUX::Input::DoubleTappedRoutedEventArgs const& e)
    {
        const auto element = sender.try_as<FrameworkElement>();
        if (const auto display = element ? element.DataContext().try_as<TerminalApp::TabStripDisplayItem>() : nullptr;
            display && !_originatesFromHeaderControl(e.OriginalSource(), element))
        {
            e.Handled(true);
            TabRenameRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(display.Tab()));
        }
    }

    void TabStrip::OnTabCloseClick(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto element = sender.try_as<FrameworkElement>())
        {
            if (const auto display = element.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
            {
                TabCloseRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(display.Tab()));
            }
        }
    }

    void TabStrip::OnDragItemsCompleted(ListViewBase const& /*sender*/,
                                         DragItemsCompletedEventArgs const& e)
    {
        if (e.DropResult() == Windows::ApplicationModel::DataTransfer::DataPackageOperation::Move &&
            _draggingItem &&
            !_dragCollectionChanged)
        {
            const auto draggedTab = _draggingItem.try_as<MUX::Controls::TabViewItem>();
            uint32_t sourceIndex{};
            std::optional<uint32_t> destinationIndex;
            for (uint32_t i = 0; i < _displayItems.Size(); ++i)
            {
                const auto display = _displayItems.GetAt(i);
                if (draggedTab && winrt::get_abi(display.Tab()) == winrt::get_abi(draggedTab))
                {
                    destinationIndex = i;
                    break;
                }
            }

            if (draggedTab &&
                _tabItems.IndexOf(draggedTab, sourceIndex) &&
                destinationIndex.has_value() &&
                *destinationIndex != sourceIndex)
            {
                _syncingNativeReorder = true;
                auto endSync = wil::scope_exit([&]() noexcept {
                    _syncingNativeReorder = false;
                });
                const auto item = _tabItems.GetAt(sourceIndex);
                _tabItems.RemoveAt(sourceIndex);
                _tabItems.InsertAt(*destinationIndex, item);
            }
        }
        else if (_dragCollectionChanged)
        {
            _syncDisplayItems();
        }

        TabDragCompleted.raise(*this, nullptr);

        // Tearoff signal: dropped where nobody accepted it. Mirrors MUX
        // TabView.TabDroppedOutside — TerminalPage will create a new window.
        if (e.DropResult() == Windows::ApplicationModel::DataTransfer::DataPackageOperation::None && _draggingItem)
        {
            auto tab = _draggingItem.try_as<MUX::Controls::TabViewItem>();
            auto args = winrt::make_self<TabStripDroppedOutsideEventArgs>(tab, _draggingItem);
            TabDroppedOutside.raise(*this, *args);
        }
        _draggingItem = nullptr;
        _draggingIndex.reset();
        _dragCollectionChanged = false;
    }

    void TabStrip::OnListDragOver(IInspectable const& /*sender*/,
                                   WUX::DragEventArgs const& e)
    {
        TabStripDragOver.raise(*this, e);
    }

    void TabStrip::OnListDrop(IInspectable const& /*sender*/,
                               WUX::DragEventArgs const& e)
    {
        TabStripDrop.raise(*this, e);
    }

    int32_t TabStrip::_computeDropIndex(winrt::Windows::Foundation::Point const& stripRelativePos)
    {
        // Axis-parameterized per the B→C migration rules. The math is identical
        // to what TerminalPage::_onTabStripDrop does today for horizontal, just
        // switched to the Y axis when Orientation is Vertical.
        const bool vertical = _orientation == TerminalApp::TabStripOrientation::Vertical;
        const auto count = _tabItems.Size();

        for (uint32_t i = 0; i < count; ++i)
        {
            auto container = ItemsList().ContainerFromIndex(i).try_as<ListViewItem>();
            if (!container)
            {
                continue;
            }
            auto transform = container.TransformToVisual(ItemsList());
            auto containerOrigin = transform.TransformPoint({ 0, 0 });
            const auto axisPos = vertical ? stripRelativePos.Y - containerOrigin.Y
                                          : stripRelativePos.X - containerOrigin.X;
            const auto axisDim = vertical ? container.ActualHeight() : container.ActualWidth();
            if (axisPos < axisDim / 2)
            {
                return gsl::narrow_cast<int32_t>(i);
            }
        }
        return -1;
    }
}
