// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"

#include "TabStrip.g.h"
#include "TabStripSelectionChangedEventArgs.g.h"
#include "TabStripCloseRequestedEventArgs.g.h"
#include "TabStripDragStartingEventArgs.g.h"
#include "TabStripDroppedOutsideEventArgs.g.h"

namespace winrt::TerminalApp::implementation
{
    struct TabStripSelectionChangedEventArgs : TabStripSelectionChangedEventArgsT<TabStripSelectionChangedEventArgs>
    {
        WINRT_PROPERTY(winrt::Windows::Foundation::IInspectable, AddedItem, nullptr);
        WINRT_PROPERTY(winrt::Windows::Foundation::IInspectable, RemovedItem, nullptr);

    public:
        TabStripSelectionChangedEventArgs(winrt::Windows::Foundation::IInspectable added,
                                           winrt::Windows::Foundation::IInspectable removed) :
            _AddedItem{ std::move(added) }, _RemovedItem{ std::move(removed) } {}
    };

    struct TabStripCloseRequestedEventArgs : TabStripCloseRequestedEventArgsT<TabStripCloseRequestedEventArgs>
    {
        WINRT_PROPERTY(winrt::Microsoft::UI::Xaml::Controls::TabViewItem, Tab, nullptr);

    public:
        TabStripCloseRequestedEventArgs(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab) :
            _Tab{ std::move(tab) } {}
    };

    struct TabStripDragStartingEventArgs : TabStripDragStartingEventArgsT<TabStripDragStartingEventArgs>
    {
        WINRT_PROPERTY(winrt::Microsoft::UI::Xaml::Controls::TabViewItem, Tab, nullptr);
        WINRT_PROPERTY(winrt::Windows::Foundation::IInspectable, Item, nullptr);
        WINRT_PROPERTY(winrt::Windows::ApplicationModel::DataTransfer::DataPackage, Data, nullptr);
        WINRT_PROPERTY(bool, Cancel, false);

    public:
        TabStripDragStartingEventArgs(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                                       winrt::Windows::Foundation::IInspectable item,
                                       winrt::Windows::ApplicationModel::DataTransfer::DataPackage data) :
            _Tab{ std::move(tab) }, _Item{ std::move(item) }, _Data{ std::move(data) } {}
    };

    struct TabStripDroppedOutsideEventArgs : TabStripDroppedOutsideEventArgsT<TabStripDroppedOutsideEventArgs>
    {
        WINRT_PROPERTY(winrt::Microsoft::UI::Xaml::Controls::TabViewItem, Tab, nullptr);
        WINRT_PROPERTY(winrt::Windows::Foundation::IInspectable, Item, nullptr);

    public:
        TabStripDroppedOutsideEventArgs(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                                         winrt::Windows::Foundation::IInspectable item) :
            _Tab{ std::move(tab) }, _Item{ std::move(item) } {}
    };

    struct TabStrip : TabStripT<TabStrip>
    {
        TabStrip();

        // Getter-only in the IDL — returns the single, stable observable collection
        // that call sites mutate directly via InsertAt/RemoveAt.
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> TabItems() const { return _tabItems; }

        // Proxies to the internal ListView. Non-const because the XAML-generated
        // ItemsList()/LeadingContentPresenter()/TrailingContentPresenter()
        // accessors are non-const.
        winrt::Windows::Foundation::IInspectable SelectedItem();
        void SelectedItem(winrt::Windows::Foundation::IInspectable const& value);
        int32_t SelectedIndex();
        void SelectedIndex(int32_t value);
        winrt::Windows::UI::Xaml::DependencyObject ContainerFromIndex(int32_t index);

        // Prototype: setter accepts Vertical only. Horizontal setter is a no-op —
        // C is where the layout actually flips.
        TerminalApp::TabStripOrientation Orientation() const noexcept { return _orientation; }
        void Orientation(TerminalApp::TabStripOrientation value);

        bool CanReorderTabs();
        void CanReorderTabs(bool value);
        bool CanDragTabs();
        void CanDragTabs(bool value);

        winrt::Windows::UI::Xaml::UIElement LeadingContent();
        void LeadingContent(winrt::Windows::UI::Xaml::UIElement const& value);
        winrt::Windows::UI::Xaml::UIElement TrailingContent();
        void TrailingContent(winrt::Windows::UI::Xaml::UIElement const& value);

        // XAML-bound event handlers.
        void OnListSelectionChanged(winrt::Windows::Foundation::IInspectable const& sender,
                                     winrt::Windows::UI::Xaml::Controls::SelectionChangedEventArgs const& e);
        void OnDragItemsStarting(winrt::Windows::Foundation::IInspectable const& sender,
                                  winrt::Windows::UI::Xaml::Controls::DragItemsStartingEventArgs const& e);
        void OnDragItemsCompleted(winrt::Windows::UI::Xaml::Controls::ListViewBase const& sender,
                                   winrt::Windows::UI::Xaml::Controls::DragItemsCompletedEventArgs const& e);
        // Named to avoid colliding with IControlOverrides::OnDrop /
        // OnDragOver on the Control base class, which have different arities.
        void OnListDragOver(winrt::Windows::Foundation::IInspectable const& sender,
                             winrt::Windows::UI::Xaml::DragEventArgs const& e);
        void OnListDrop(winrt::Windows::Foundation::IInspectable const& sender,
                         winrt::Windows::UI::Xaml::DragEventArgs const& e);
        void OnCustomCloseClick(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::RoutedEventArgs const& e);

        // Spec A §2.4: reports the rail as an AutomationControlType::Tab
        // container so screen readers (Narrator / third-party AT) treat it
        // like the horizontal MUX TabView rather than a generic UserControl.
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationPeer OnCreateAutomationPeer();

        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripSelectionChangedEventArgs> SelectionChanged;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripCloseRequestedEventArgs> TabCloseRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::Collections::IVectorChangedEventArgs> TabItemsChanged;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripDragStartingEventArgs> TabDragStarting;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> TabDragCompleted;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::UI::Xaml::DragEventArgs> TabStripDragOver;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::UI::Xaml::DragEventArgs> TabStripDrop;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripDroppedOutsideEventArgs> TabDroppedOutside;

    private:
        TerminalApp::TabStripOrientation _orientation{ TerminalApp::TabStripOrientation::Vertical };
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> _tabItems{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable>::VectorChanged_revoker _vectorChangedRevoker;

        // Per-TabViewItem CloseRequested subscription tokens, keyed by pointer identity.
        std::unordered_map<void*, winrt::event_token> _closeRequestedTokens;

        // The item currently being dragged. Set in OnDragItemsStarting, cleared in
        // OnDragItemsCompleted. If DropResult is None, this is the item to fire
        // TabDroppedOutside with (tearoff-to-new-window signal).
        winrt::Windows::Foundation::IInspectable _draggingItem{ nullptr };

        void _onItemsVectorChanged(winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> const& sender,
                                     winrt::Windows::Foundation::Collections::IVectorChangedEventArgs const& args);
        void _hookCloseRequested(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _unhookCloseRequested(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);

        // Axis-parameterized per B→C rules. Returns -1 to mean "append at end."
        // Non-const because it reaches into the XAML-generated ItemsList().
        int32_t _computeDropIndex(winrt::Windows::Foundation::Point const& stripRelativePos);
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabStrip);
}
