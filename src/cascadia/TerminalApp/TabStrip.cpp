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
    TabStrip::TabStrip()
    {
        _tabItems = single_threaded_observable_vector<IInspectable>();

        InitializeComponent();

        ItemsList().ItemsSource(_tabItems);
        _vectorChangedRevoker = _tabItems.VectorChanged(auto_revoke, { get_weak(), &TabStrip::_onItemsVectorChanged });
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

    UIElement TabStrip::LeadingContent()
    {
        return LeadingContentPresenter().Content().try_as<UIElement>();
    }
    void TabStrip::LeadingContent(UIElement const& value)
    {
        LeadingContentPresenter().Content(value);
    }
    UIElement TabStrip::TrailingContent()
    {
        return TrailingContentPresenter().Content().try_as<UIElement>();
    }
    void TabStrip::TrailingContent(UIElement const& value)
    {
        TrailingContentPresenter().Content(value);
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
            }
            break;
        case CollectionChange::ItemRemoved:
            // We can't reach the removed item from `sender` here — the removal
            // side is bounded by _closeRequestedTokens's key set; if the item
            // is gone from the tree, its subscription lapses naturally. No
            // action required for the prototype.
            break;
        case CollectionChange::ItemChanged:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _hookCloseRequested(item);
            }
            break;
        case CollectionChange::Reset:
            _closeRequestedTokens.clear();
            for (uint32_t i = 0; i < sender.Size(); ++i)
            {
                if (const auto item = sender.GetAt(i).try_as<MUX::Controls::TabViewItem>())
                {
                    _hookCloseRequested(item);
                }
            }
            break;
        }

        TabItemsChanged.raise(*this, args);
    }

    void TabStrip::_hookCloseRequested(MUX::Controls::TabViewItem const& item)
    {
        // The rail renders its own X in the DataTemplate wrapper (see
        // TabStrip.xaml, OnCustomCloseClick). MUX's built-in CloseButton on
        // TabViewItem only shows when the item lives inside a TabView (via
        // TabView.CloseButtonOverlayMode), so we suppress it here — this
        // prevents both a stale hidden button and any double-X if MUX ever
        // changes its default.
        item.IsClosable(false);
        _closeRequestedTokens[winrt::get_abi(item)] = {};
    }

    void TabStrip::_unhookCloseRequested(MUX::Controls::TabViewItem const& item)
    {
        _closeRequestedTokens.erase(winrt::get_abi(item));
    }

    void TabStrip::OnCustomCloseClick(IInspectable const& sender, WUX::RoutedEventArgs const&)
    {
        if (const auto button = sender.try_as<WUX::Controls::Button>())
        {
            if (const auto tab = button.DataContext().try_as<MUX::Controls::TabViewItem>())
            {
                auto args = winrt::make_self<TabStripCloseRequestedEventArgs>(tab);
                TabCloseRequested.raise(*this, *args);
            }
        }
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
        using DPO = Windows::ApplicationModel::DataTransfer::DataPackageOperation;
        if (e.DropResult() == DPO::None && _draggingItem)
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
