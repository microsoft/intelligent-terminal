// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#include "pch.h"
#include "TabStrip.h"
#include "TabStripAutomationPeer.h"

#include "TabStrip.g.cpp"
#include "TabStripSelectionChangedEventArgs.g.cpp"
#include "TabStripCloseRequestedEventArgs.g.cpp"
#include "TabStripDragStartingEventArgs.g.cpp"
#include "TabStripDroppedOutsideEventArgs.g.cpp"

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
        _historyItems = single_threaded_observable_vector<TerminalApp::TabStripHistoryItem>();

        InitializeComponent();

        ItemsList().ItemsSource(_tabItems);
        _vectorChangedRevoker = _tabItems.VectorChanged(auto_revoke, { get_weak(), &TabStrip::_onItemsVectorChanged });
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
        _applyRailState();
        _updateHistoryVisualState();
    }

    IInspectable TabStrip::SelectedItem()
    {
        return ItemsList().SelectedItem();
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
        const auto tab = item.try_as<MUX::Controls::TabViewItem>();
        if (!tab)
        {
            return;
        }

        uint32_t index{};
        if (_tabItems.IndexOf(tab, index))
        {
            _tabItemVisibility.insert_or_assign(winrt::get_abi(tab), TabItemVisibilityState{ winrt::make_weak(tab), visible });
            _applyTabItemVisibility(tab);
            _pruneTabItemVisibility();
        }
    }

    void TabStrip::SetFilterStatus(uint32_t visibleTabCount, bool selectedTabVisible)
    {
        FilterStatusText().Text(visibleTabCount == 1 ?
                                    RS_(L"VerticalTabsFilterStatusSingle") :
                                    winrt::hstring{ RS_fmt(L"VerticalTabsFilterStatusPlural", visibleTabCount) });
        HiddenCurrentTabIndicator().Visibility(selectedTabVisible ? Visibility::Collapsed : Visibility::Visible);
        FilterStatusBar().Visibility(_filterMode == TerminalApp::TabStripFilterMode::AgentsOnly ?
                                         Visibility::Visible :
                                         Visibility::Collapsed);
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
        return ItemsList().CanReorderItems();
    }
    void TabStrip::CanReorderTabs(bool value)
    {
        ItemsList().CanReorderItems(value);
        ItemsList().ReorderMode(value ? ListViewReorderMode::Enabled : ListViewReorderMode::Disabled);
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
        _tabsVisible = value;
        _applyRailState();
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
        _applyTabItemRailState(item);
    }

    void TabStrip::FilterMode(TerminalApp::TabStripFilterMode value)
    {
        const auto changed = _filterMode != value;
        _filterMode = value;
        AllTabsFilterItem().IsChecked(value == TerminalApp::TabStripFilterMode::AllTabs);
        AgentsOnlyFilterItem().IsChecked(value == TerminalApp::TabStripFilterMode::AgentsOnly);
        FilterStatusBar().Visibility(value == TerminalApp::TabStripFilterMode::AgentsOnly ?
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

    void TabStrip::CommitHistorySnapshot(std::vector<TerminalApp::TabStripHistoryItem> items)
    {
        _historySnapshot = std::move(items);
        _historySearchTerms.clear();
        _historySearchTerms.reserve(_historySnapshot.size());
        for (const auto& item : _historySnapshot)
        {
            _historySearchTerms.emplace_back(_buildHistorySearchTerms(item));
        }
        _applyHistoryProjection();
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
        if (_historyActive != value)
        {
            _historyActive = value;
            ClearHistorySearch();
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

    void TabStrip::HistoryError(winrt::hstring const& value)
    {
        if (_historyError != value)
        {
            _historyError = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::ProjectionControlsEnabled(bool value)
    {
        _projectionControlsEnabled = value;
        SearchTabsButton().IsEnabled(value && !_isRailCollapsed);
        FilterTabsButton().IsEnabled(value && !_isRailCollapsed);
        TabHistoryButton().IsEnabled(value && !_isRailCollapsed);
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

    void TabStrip::OnAllTabsFilterClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        FilterMode(TerminalApp::TabStripFilterMode::AllTabs);
    }

    void TabStrip::OnAgentsOnlyFilterClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        FilterMode(TerminalApp::TabStripFilterMode::AgentsOnly);
    }

    void TabStrip::OnShowAllTabsClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        FilterMode(TerminalApp::TabStripFilterMode::AllTabs);
    }

    void TabStrip::OnSearchToggleClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        if (_isRailCollapsed)
        {
            SearchTabsButton().IsChecked(false);
            return;
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
        if (!_searchActive && !_isRailCollapsed)
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

    void TabStrip::OnHistoryClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        if (_isRailCollapsed || !_projectionControlsEnabled)
        {
            return;
        }
        HistoryActive(true);
        HistoryRequested.raise(*this, nullptr);
        HistorySearchTextBox().Focus(WUX::FocusState::Programmatic);
    }

    void TabStrip::OnHistoryCloseClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        HistoryClosed.raise(*this, nullptr);
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
        if (const auto item = e.ClickedItem().try_as<TerminalApp::TabStripHistoryItem>())
        {
            HistoryActivationRequested.raise(
                *this,
                winrt::make<TabStripHistoryActivationEventArgs>(item));
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
            container.Visibility(Visibility::Visible);
            return;
        }

        if (const auto item = e.Item().try_as<MUX::Controls::TabViewItem>())
        {
            _applyTabItemVisibility(item, container);
        }
        else
        {
            container.Visibility(Visibility::Visible);
        }
    }

    void TabStrip::_applyRailState()
    {
        const auto expandedVisibility = _isRailCollapsed ? Visibility::Collapsed : Visibility::Visible;
        const auto collapsedVisibility = _isRailCollapsed ? Visibility::Visible : Visibility::Collapsed;

        MinWidth(_isRailCollapsed ? 40.0 : 180.0);
        CompactNewTabToolbar().Visibility(collapsedVisibility);
        VerticalTabsHeader().Visibility(expandedVisibility);
        SearchTabsButton().IsHitTestVisible(!_isRailCollapsed);
        SearchTabsButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
        FilterTabsButton().IsHitTestVisible(!_isRailCollapsed);
        FilterTabsButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
        TabHistoryButton().IsHitTestVisible(!_isRailCollapsed);
        TabHistoryButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
        FilterStatusBar().IsHitTestVisible(!_isRailCollapsed);
        ItemsList().AllowDrop(!_isRailCollapsed);
        TabsToolbar().Padding(_isRailCollapsed ? WUX::Thickness{} : WUX::Thickness{ 12, 0, 8, 0 });
        WUX::Controls::Grid::SetColumn(SearchTabsButton(), _isRailCollapsed ? 0 : 1);
        WUX::Controls::Grid::SetColumnSpan(SearchTabsButton(), _isRailCollapsed ? 4 : 1);
        SearchTabsButton().Width(40.0);
        SearchTabsButton().Height(40.0);
        ItemsList().Visibility(_tabsVisible ? Visibility::Visible : Visibility::Collapsed);

        if (_isRailCollapsed)
        {
            if (const auto flyout = FilterTabsButton().Flyout())
            {
                flyout.Hide();
            }
            _updateHistoryVisualState();
        }

        for (uint32_t index = 0; index < _tabItems.Size(); ++index)
        {
            if (const auto item = _tabItems.GetAt(index).try_as<MUX::Controls::TabViewItem>())
            {
                _applyTabItemRailState(item);
                _applyTabItemVisibility(item);
            }
        }

        _updateSearchVisualState();
    }

    void TabStrip::_applyTabItemRailState(MUX::Controls::TabViewItem const& item)
    {
        if (const auto header = item.Header().try_as<UIElement>())
        {
            header.Visibility(_isRailCollapsed ? Visibility::Collapsed : Visibility::Visible);
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

        const auto expanded = _searchActive && !_isRailCollapsed;
        _setSearchPanelExpanded(expanded, _searchAnimationEnabled && !_isRailCollapsed);
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
        append(item.AgentId());
        append(item.ProviderDisplayName());
        append(item.AgentSource());
        append(item.WslDistro());
        append(item.Status());
        terms.emplace_back(item.IsLive() ? L"live" : L"history");
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

    void TabStrip::_applyHistoryProjection()
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
        _historyItems.ReplaceAll(visibleItems);
        _updateHistoryVisualState();
    }

    void TabStrip::_updateHistoryVisualState()
    {
        const auto visible = _historyActive && !_isRailCollapsed;
        HistoryPanel().Visibility(visible ? Visibility::Visible : Visibility::Collapsed);
        HistoryLoadingIndicator().IsActive(visible && _historyLoading);
        HistoryLoadingIndicator().Visibility(visible && _historyLoading ? Visibility::Visible : Visibility::Collapsed);
        HistoryList().Visibility(visible && !_historyLoading && _historyError.empty() && _historyItems.Size() > 0 ?
                                     Visibility::Visible :
                                     Visibility::Collapsed);
        if (!visible || _historyLoading)
        {
            HistoryMessage().Visibility(Visibility::Collapsed);
        }
        else if (!_historyError.empty())
        {
            HistoryMessage().Text(_historyError);
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
        // Sync per-item CloseRequested subscriptions on add/remove.
        // (Reset covers bulk clear; individual changes cover the common paths.)
        switch (args.CollectionChange())
        {
        case CollectionChange::ItemInserted:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _hookCloseRequested(item);
                _applyTabItemRailState(item);
                _applyTabItemVisibility(item);
            }
            break;
        case CollectionChange::ItemRemoved:
            break;
        case CollectionChange::ItemChanged:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _hookCloseRequested(item);
                _applyTabItemRailState(item);
                _applyTabItemVisibility(item);
            }
            break;
        case CollectionChange::Reset:
            _clearCloseRequestedSubscriptions();
            _tabItemVisibility.clear();
            for (uint32_t i = 0; i < sender.Size(); ++i)
            {
                if (const auto item = sender.GetAt(i).try_as<MUX::Controls::TabViewItem>())
                {
                    _hookCloseRequested(item);
                    _applyTabItemRailState(item);
                    _applyTabItemVisibility(item);
                }
            }
            break;
        }

        _removeStaleCloseRequestedSubscriptions(sender);
        TabItemsChanged.raise(*this, args);
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

    void TabStrip::OnListSelectionChanged(IInspectable const& /*sender*/,
                                           SelectionChangedEventArgs const& e)
    {
        // Keep TabViewItem selection synchronized for close-button and
        // accessibility behavior. The injected LayoutRoot background renders
        // the vertical rounded selection state.
        for (const auto& removed : e.RemovedItems())
        {
            if (auto item = removed.try_as<MUX::Controls::TabViewItem>())
            {
                item.IsSelected(false);
                _applyVerticalTabChrome(item);
            }
        }
        for (const auto& added : e.AddedItems())
        {
            if (auto item = added.try_as<MUX::Controls::TabViewItem>())
            {
                item.IsSelected(true);
                _applyVerticalTabChrome(item);
            }
        }

        IInspectable added = e.AddedItems().Size() > 0 ? e.AddedItems().GetAt(0) : nullptr;
        IInspectable removed = e.RemovedItems().Size() > 0 ? e.RemovedItems().GetAt(0) : nullptr;
        auto args = winrt::make_self<TabStripSelectionChangedEventArgs>(std::move(added), std::move(removed));
        SelectionChanged.raise(*this, *args);
    }

    void TabStrip::OnDragItemsStarting(IInspectable const& /*sender*/,
                                        DragItemsStartingEventArgs const& e)
    {
        // ListView packs the dragged items into e.Items(); for SelectionMode=Single
        // there's at most one. Wrap it in a TabView-shaped args object so
        // TerminalPage's tearoff-setup code can look identical to the horizontal path.
        // Load-bearing lookup: ListView.DragItemsStartingEventArgs.Items() surfaces
        // the *Content* of each ContentControl-based item rather than the item
        // itself. Since each TabViewItem has a unique empty Border as its Content
        // (the GH bodge from Tab::_MakeTabViewItem), what we get is a Border, not
        // the TabViewItem. Recover the actual TabViewItem by finding the one in
        // _tabItems whose Content is this Border — Content uniqueness makes that
        // lookup unambiguous.
        IInspectable surfacedItem = e.Items().Size() > 0 ? e.Items().GetAt(0) : nullptr;
        MUX::Controls::TabViewItem tab{ nullptr };
        if (surfacedItem)
        {
            const auto surfacedAbi = winrt::get_abi(surfacedItem);
            for (uint32_t i = 0; i < _tabItems.Size(); ++i)
            {
                if (const auto candidate = _tabItems.GetAt(i).try_as<MUX::Controls::TabViewItem>())
                {
                    if (winrt::get_abi(candidate.Content()) == surfacedAbi)
                    {
                        tab = candidate;
                        break;
                    }
                }
            }
        }
        // Stash the resolved TabViewItem (not the surfaced Border) so
        // OnDragItemsCompleted's tearoff path also gets the right identity.
        _draggingItem = tab ? IInspectable{ tab } : surfacedItem;

        auto args = winrt::make_self<TabStripDragStartingEventArgs>(tab, _draggingItem, e.Data());
        TabDragStarting.raise(*this, *args);

        if (args->Cancel())
        {
            e.Cancel(true);
            _draggingItem = nullptr;
        }
    }

    void TabStrip::OnDragItemsCompleted(ListViewBase const& /*sender*/,
                                         DragItemsCompletedEventArgs const& e)
    {
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
