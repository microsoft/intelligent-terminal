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

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
}

namespace winrt::TerminalApp::implementation
{
    static WUX::Controls::Button _findCloseButton(WUX::DependencyObject const& root)
    {
        const auto childCount = WUX::Media::VisualTreeHelper::GetChildrenCount(root);
        for (int32_t index = 0; index < childCount; ++index)
        {
            const auto child = WUX::Media::VisualTreeHelper::GetChild(root, index);
            if (const auto button = child.try_as<WUX::Controls::Button>();
                button && button.Name() == L"CloseButton")
            {
                return button;
            }

            if (const auto button = _findCloseButton(child))
            {
                return button;
            }
        }
        return nullptr;
    }

    TabStrip::TabStrip()
    {
        _tabItems = single_threaded_observable_vector<IInspectable>();

        InitializeComponent();

        ItemsList().ItemsSource(_tabItems);
        _vectorChangedRevoker = _tabItems.VectorChanged(auto_revoke, { get_weak(), &TabStrip::_onItemsVectorChanged });
        _applyRailState();
    }

    IInspectable TabStrip::SelectedItem()
    {
        return ItemsList().SelectedItem();
    }
    void TabStrip::SelectedItem(IInspectable const& value)
    {
        ItemsList().SelectedItem(value);
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

    void TabStrip::_applyRailState()
    {
        const auto expandedVisibility = _isRailCollapsed ? Visibility::Collapsed : Visibility::Visible;
        const auto collapsedVisibility = _isRailCollapsed ? Visibility::Visible : Visibility::Collapsed;

        MinWidth(_isRailCollapsed ? 40.0 : 180.0);
        CompactNewTabToolbar().Visibility(collapsedVisibility);
        VerticalTabsHeader().Visibility(expandedVisibility);
        TabsToolbar().Padding(_isRailCollapsed ? WUX::Thickness{} : WUX::Thickness{ 12, 0, 8, 0 });
        WUX::Controls::Grid::SetColumn(SearchTabsButton(), _isRailCollapsed ? 0 : 1);
        WUX::Controls::Grid::SetColumnSpan(SearchTabsButton(), _isRailCollapsed ? 4 : 1);
        SearchTabsButton().Width(_isRailCollapsed ? 40.0 : 32.0);
        SearchTabsButton().Height(_isRailCollapsed ? 40.0 : 32.0);
        ItemsList().Visibility(_tabsVisible ? Visibility::Visible : Visibility::Collapsed);

        for (uint32_t index = 0; index < _tabItems.Size(); ++index)
        {
            if (const auto item = _tabItems.GetAt(index).try_as<MUX::Controls::TabViewItem>())
            {
                _applyTabItemRailState(item);
            }
        }
    }

    void TabStrip::_applyTabItemRailState(MUX::Controls::TabViewItem const& item)
    {
        if (const auto header = item.Header().try_as<UIElement>())
        {
            header.Visibility(_isRailCollapsed ? Visibility::Collapsed : Visibility::Visible);
        }

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
            }
            break;
        case CollectionChange::ItemRemoved:
            break;
        case CollectionChange::ItemChanged:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _hookCloseRequested(item);
                _applyTabItemRailState(item);
            }
            break;
        case CollectionChange::Reset:
            _clearCloseRequestedSubscriptions();
            for (uint32_t i = 0; i < sender.Size(); ++i)
            {
                if (const auto item = sender.GetAt(i).try_as<MUX::Controls::TabViewItem>())
                {
                    _hookCloseRequested(item);
                    _applyTabItemRailState(item);
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

        item.IsClosable(true);

        const auto weakThis = get_weak();
        const auto weakItem = winrt::make_weak(item);
        const auto loadedToken = item.Loaded([weakThis, weakItem](auto&&, auto&&) {
            const auto self = weakThis.get();
            const auto tab = weakItem.get();
            if (self && tab)
            {
                self->_refreshCloseButton(tab);
            }
        });
        _closeRequestedSubscriptions.emplace(key, CloseRequestedSubscription{ weakItem, loadedToken });
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
                    item.Loaded(it->second.LoadedToken);
                }
                if (const auto button = it->second.CloseButton.get())
                {
                    button.Click(it->second.ClickToken);
                }
                it = _closeRequestedSubscriptions.erase(it);
            }
        }
    }

    void TabStrip::_clearCloseRequestedSubscriptions()
    {
        for (const auto& [_, subscription] : _closeRequestedSubscriptions)
        {
            if (const auto item = subscription.Item.get())
            {
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
        // Sync IsSelected on the TabViewItem so the tab visually reflects
        // selection state (the ListViewItem chrome is stripped in XAML, so
        // the TabViewItem owns the visual).
        for (const auto& removed : e.RemovedItems())
        {
            if (auto item = removed.try_as<MUX::Controls::TabViewItem>())
            {
                item.IsSelected(false);
            }
        }
        for (const auto& added : e.AddedItems())
        {
            if (auto item = added.try_as<MUX::Controls::TabViewItem>())
            {
                item.IsSelected(true);
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
