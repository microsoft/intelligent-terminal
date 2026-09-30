// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"
#include "winrt/Windows.UI.ViewManagement.h"

#include "TabStrip.g.h"
#include "TabStripSelectionChangedEventArgs.g.h"
#include "TabStripCloseRequestedEventArgs.g.h"
#include "TabStripDragStartingEventArgs.g.h"
#include "TabStripDroppedOutsideEventArgs.g.h"
#include "TabStripHistoryItem.g.h"
#include "TabStripHistoryActivationEventArgs.g.h"
#include "TabStripPaneItem.g.h"
#include "TabStripDisplayItem.g.h"
#include "TabStripPaneEventArgs.g.h"

namespace TerminalAppLocalTests
{
    class TabTests;
}

namespace winrt::TerminalApp::implementation
{
    struct TabStripHistoryItem : TabStripHistoryItemT<TabStripHistoryItem>
    {
        TabStripHistoryItem() = default;
        WINRT_PROPERTY(winrt::hstring, SessionId);
        WINRT_PROPERTY(winrt::hstring, Title);
        WINRT_PROPERTY(winrt::hstring, Subtitle);
        WINRT_PROPERTY(winrt::hstring, Cwd);
        WINRT_PROPERTY(winrt::hstring, PaneSessionId);
        WINRT_PROPERTY(winrt::hstring, AgentId);
        WINRT_PROPERTY(winrt::hstring, ProviderDisplayName);
        WINRT_PROPERTY(winrt::hstring, AgentSource);
        WINRT_PROPERTY(winrt::hstring, WslDistro);
        WINRT_PROPERTY(winrt::hstring, SessionUniverse);
        WINRT_PROPERTY(winrt::hstring, Status);
        WINRT_PROPERTY(winrt::hstring, SearchQuery);
        WINRT_PROPERTY(bool, IsLive, false);
        WINRT_PROPERTY(bool, IsAgentPane, false);
        WINRT_PROPERTY(bool, IsHistorical, false);
        WINRT_PROPERTY(winrt::hstring, StatusText);
        WINRT_PROPERTY(winrt::Windows::UI::Xaml::Style, StatusTextStyle, nullptr);
        WINRT_PROPERTY(winrt::Windows::UI::Xaml::DataTemplate, IconTemplate, nullptr);
        WINRT_OBSERVABLE_PROPERTY(bool, IsCurrent, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CurrentBackground, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CurrentForeground, PropertyChanged.raise, nullptr);

    public:
        til::property_changed_event PropertyChanged;
    };

    struct TabStripHistoryActivationEventArgs : TabStripHistoryActivationEventArgsT<TabStripHistoryActivationEventArgs>
    {
        WINRT_PROPERTY(TerminalApp::TabStripHistoryItem, Item, nullptr);

    public:
        explicit TabStripHistoryActivationEventArgs(TerminalApp::TabStripHistoryItem item) :
            _Item{ std::move(item) } {}
    };

    struct TabStripPaneItem : TabStripPaneItemT<TabStripPaneItem>
    {
        TabStripPaneItem() = default;
        TabStripPaneItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                         uint32_t contentId,
                         winrt::hstring iconPath,
                         winrt::hstring title,
                         bool isActive);

        winrt::Microsoft::UI::Xaml::Controls::TabViewItem Tab() const noexcept { return _tab; }
        uint32_t ContentId() const noexcept { return _contentId; }
        const winrt::hstring& IconPath() const noexcept { return _iconPath; }
        void SyncIcon(const winrt::hstring& iconPath);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Title, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(bool, IsActive, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, ActiveIndicatorVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, MetadataText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, AutomationName, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, MetadataVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Controls::IconElement, Icon, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, HighlightQuery, PropertyChanged.raise);

    public:
        til::property_changed_event PropertyChanged;

    private:
        winrt::Microsoft::UI::Xaml::Controls::TabViewItem _tab{ nullptr };
        uint32_t _contentId{};
        winrt::hstring _iconPath;
    };

    struct TabStripDisplayItem : TabStripDisplayItemT<TabStripDisplayItem>
    {
        TabStripDisplayItem() = default;
        TabStripDisplayItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                            winrt::Windows::Foundation::IInspectable header);

        winrt::Microsoft::UI::Xaml::Controls::TabViewItem Tab() const noexcept { return _tab; }
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripPaneItem> PaneItems() const noexcept { return _paneItems; }
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::Foundation::IInspectable, Header, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Title, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SearchText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Controls::IconElement, Icon, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Controls::Primitives::FlyoutBase, ContextFlyout, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(bool, IsExpanded, PropertyChanged.raise, true);
        WINRT_OBSERVABLE_PROPERTY(bool, IsGroup, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(bool, IsPinned, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, HeaderVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Visible);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, CloseVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Visible);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, SelectionVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, GroupVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, ChildrenVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, IconVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Visible);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Visibility, PinnedIconVisibility, PropertyChanged.raise, winrt::Windows::UI::Xaml::Visibility::Collapsed);
        WINRT_OBSERVABLE_PROPERTY(double, HeaderMinHeight, PropertyChanged.raise, 40.0);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, ChevronGlyph, PropertyChanged.raise, L"\xE70D");
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Thickness, LeadingContentMargin, PropertyChanged.raise, 6, 0, 10, 0);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, ToolTipText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, AcceleratorKey, PropertyChanged.raise);

    public:
        void SyncTabPresentation(bool railCollapsed);
        void SyncIcon(winrt::hstring const& iconPath);
        void UpdatePresentation(bool railCollapsed);
        til::property_changed_event PropertyChanged;

    private:
        winrt::Microsoft::UI::Xaml::Controls::TabViewItem _tab{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripPaneItem> _paneItems{ nullptr };
        std::optional<winrt::hstring> _iconPath;
    };

    struct TabStripPaneEventArgs : TabStripPaneEventArgsT<TabStripPaneEventArgs>
    {
        WINRT_PROPERTY(winrt::Microsoft::UI::Xaml::Controls::TabViewItem, Tab, nullptr);
        WINRT_PROPERTY(uint32_t, ContentId, 0);

    public:
        TabStripPaneEventArgs(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                              uint32_t contentId) :
            _Tab{ std::move(tab) }, _ContentId{ contentId } {}
    };

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
        // control accessors are non-const.
        winrt::Windows::Foundation::IInspectable SelectedItem();
        void SelectedItem(winrt::Windows::Foundation::IInspectable const& value);
        int32_t SelectedIndex();
        void SelectedIndex(int32_t value);
        winrt::Windows::UI::Xaml::DependencyObject ContainerFromIndex(int32_t index);
        void SetTabItemVisibility(winrt::Windows::Foundation::IInspectable const& item, bool visible);
        void SetTabItemVisibility(TerminalApp::TabStripDisplayItem const& display, bool visible);
        void SetFilterStatus(uint32_t visibleTabCount, bool selectedTabVisible);
        void SetTabPresentation(winrt::Windows::Foundation::IInspectable const& item,
                                winrt::hstring const& title,
                                winrt::hstring const& iconPath);
        void SetTabPresentation(TerminalApp::TabStripDisplayItem const& display,
                                winrt::hstring const& title,
                                winrt::hstring const& iconPath);
        void SetPaneItems(winrt::Windows::Foundation::IInspectable const& item,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup);
        void SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup);
        winrt::Windows::Foundation::IInspectable HeaderForTab(winrt::Windows::Foundation::IInspectable const& item) const;
        TerminalApp::TabStripDisplayItem DisplayItemForTab(winrt::Windows::Foundation::IInspectable const& item) const;
        TerminalApp::TabStripDisplayItem DisplayItemAt(uint32_t index) const;
        void SyncTabPresentation(TerminalApp::TabStripDisplayItem const& display);
        void SetTabSearchText(winrt::Windows::Foundation::IInspectable const& item, winrt::hstring const& searchText);

        // Prototype: setter accepts Vertical only. Horizontal setter is a no-op —
        // C is where the layout actually flips.
        TerminalApp::TabStripOrientation Orientation() const noexcept { return _orientation; }
        void Orientation(TerminalApp::TabStripOrientation value);

        bool CanReorderTabs();
        void CanReorderTabs(bool value);
        bool CanDragTabs();
        void CanDragTabs(bool value);
        bool TabsVisible();
        void TabsVisible(bool value);
        bool IsRailCollapsed() const noexcept { return _isRailCollapsed; }
        void IsRailCollapsed(bool value);
        void PrepareTabItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void RefreshTabColor(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        TerminalApp::TabStripFilterMode FilterMode() const noexcept { return _filterMode; }
        void FilterMode(TerminalApp::TabStripFilterMode value);
        bool SearchActive() const noexcept { return _searchActive; }
        void SearchActive(bool value);
        winrt::hstring SearchQuery() const { return _searchQuery; }
        void SearchQuery(winrt::hstring const& value);
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripHistoryItem> HistoryItems() const { return _historyItems; }
        void CommitHistorySnapshot(std::vector<TerminalApp::TabStripHistoryItem> items, bool ready = false);
        void SetCurrentHistoryItem(TerminalApp::TabStripHistoryItem const& item,
                                   winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab);
        bool ApplyHistoryStatusDelta(winrt::hstring const& sessionId,
                                     winrt::hstring const& paneSessionId,
                                     winrt::hstring const& status,
                                     winrt::hstring const& statusText);
        bool HasHistoryItems() const noexcept { return !_historySnapshot.empty(); }
        void ClearHistorySnapshot();
        void ClearHistorySearch();
        void OpenHistory();
        bool HistoryActive() const noexcept { return _historyActive; }
        void HistoryActive(bool value);
        bool HistoryLoading() const noexcept { return _historyLoading; }
        void HistoryLoading(bool value);
        bool HistoryActivating() const noexcept { return _historyActivating; }
        void HistoryActivating(bool value);
        winrt::hstring HistoryError() const { return _historyError.empty() ? _historyRefreshError : _historyError; }
        void HistoryError(winrt::hstring const& value);
        void HistoryRefreshError(winrt::hstring const& value);
        void ProjectionControlsEnabled(bool value);
        void MoveTabItem(uint32_t from, uint32_t to);
        void RestoreTabOrder(const std::vector<winrt::Windows::Foundation::IInspectable>& items);
        void SetTabPinned(const winrt::Microsoft::UI::Xaml::Controls::TabViewItem& item, bool pinned);
        void BeginHeaderTransfer();
        void CompleteHeaderTransfer();
        bool RichTabRepositoryVisible() const noexcept { return _richTabRepositoryVisible; }
        void RichTabRepositoryVisible(bool value);
        bool RichTabBranchVisible() const noexcept { return _richTabBranchVisible; }
        void RichTabBranchVisible(bool value);
        bool RichTabAgentStatusVisible() const noexcept { return _richTabAgentStatusVisible; }
        void RichTabAgentStatusVisible(bool value);
        bool RichTabWorkingDirectoryVisible() const noexcept { return _richTabWorkingDirectoryVisible; }
        void RichTabWorkingDirectoryVisible(bool value);
        bool RichTabChangesVisible() const noexcept { return _richTabChangesVisible; }
        void RichTabChangesVisible(bool value);
        void RichTabMetadataControlsVisible(bool value);

        winrt::Windows::UI::Xaml::UIElement TopChromeContent();
        void TopChromeContent(winrt::Windows::UI::Xaml::UIElement const& value);

        // XAML-bound event handlers.
        void OnListSelectionChanged(winrt::Windows::Foundation::IInspectable const& sender,
                                     winrt::Windows::UI::Xaml::Controls::SelectionChangedEventArgs const& e);
        void OnDragItemsStarting(winrt::Windows::Foundation::IInspectable const& sender,
                                  winrt::Windows::UI::Xaml::Controls::DragItemsStartingEventArgs const& e);
        void OnDragItemsCompleted(winrt::Windows::UI::Xaml::Controls::ListViewBase const& sender,
                                   winrt::Windows::UI::Xaml::Controls::DragItemsCompletedEventArgs const& e);
        // Named to avoid colliding with IControlOverrides::OnDrop /
        // OnDragOver on the Control base class, which have different parameter types.
        void OnListDragOver(winrt::Windows::Foundation::IInspectable const& sender,
                             winrt::Windows::UI::Xaml::DragEventArgs const& e);
        void OnListDrop(winrt::Windows::Foundation::IInspectable const& sender,
                         winrt::Windows::UI::Xaml::DragEventArgs const& e);
        void OnRailToggleClick(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnCompactNewTabClick(winrt::Windows::Foundation::IInspectable const& sender,
                                  winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnCompactNewTabMenuClick(winrt::Windows::Foundation::IInspectable const& sender,
                                      winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistoryClick(winrt::Windows::Foundation::IInspectable const& sender,
                            winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistoryCloseClick(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabRepositoryVisibleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                             winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabBranchVisibleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                         winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabAgentStatusVisibleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                              winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabWorkingDirectoryVisibleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                                   winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabChangesVisibleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                          winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRichTabMetadataFlyoutClosing(
            winrt::Windows::Foundation::IInspectable const& sender,
            winrt::Windows::UI::Xaml::Controls::Primitives::FlyoutBaseClosingEventArgs const& e);
        void OnShowAllTabsClick(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnSearchToggleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnSearchPointerPressed(winrt::Windows::Foundation::IInspectable const& sender,
                                    winrt::Windows::UI::Xaml::Input::PointerRoutedEventArgs const& e);
        void OnSearchTextChanged(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::Controls::TextChangedEventArgs const& e);
        void OnSearchBoxKeyDown(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs const& e);
        void OnHistorySearchTextChanged(winrt::Windows::Foundation::IInspectable const& sender,
                                        winrt::Windows::UI::Xaml::Controls::TextChangedEventArgs const& e);
        void OnHistorySearchBoxKeyDown(winrt::Windows::Foundation::IInspectable const& sender,
                                       winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs const& e);
        void OnHistoryItemClick(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::Controls::ItemClickEventArgs const& e);
        void OnHistoryRowLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistoryContainerContentChanging(winrt::Windows::UI::Xaml::Controls::ListViewBase const& sender,
                                               winrt::Windows::UI::Xaml::Controls::ContainerContentChangingEventArgs const& e);
        void OnContainerContentChanging(winrt::Windows::UI::Xaml::Controls::ListViewBase const& sender,
                                        winrt::Windows::UI::Xaml::Controls::ContainerContentChangingEventArgs const& e);
        void OnTabHeaderLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnPaneRowLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                             winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnGroupToggleClick(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnPanePointerEntered(winrt::Windows::Foundation::IInspectable const& sender,
                                  winrt::Windows::UI::Xaml::Input::PointerRoutedEventArgs const& e);
        void OnPanePointerExited(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::Input::PointerRoutedEventArgs const& e);
        void OnPanePointerPressed(winrt::Windows::Foundation::IInspectable const& sender,
                                  winrt::Windows::UI::Xaml::Input::PointerRoutedEventArgs const& e);
        void OnPaneActivateClick(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnPaneDoubleTapped(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::Input::DoubleTappedRoutedEventArgs const& e);
        void OnPaneRightTapped(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::Input::RightTappedRoutedEventArgs const& e);
        void OnPaneCloseClick(winrt::Windows::Foundation::IInspectable const& sender,
                              winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnTabHeaderPointerPressed(winrt::Windows::Foundation::IInspectable const& sender,
                                       winrt::Windows::UI::Xaml::Input::PointerRoutedEventArgs const& e);
        void OnTabHeaderTapped(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::Input::TappedRoutedEventArgs const& e);
        void OnTabHeaderDoubleTapped(winrt::Windows::Foundation::IInspectable const& sender,
                                     winrt::Windows::UI::Xaml::Input::DoubleTappedRoutedEventArgs const& e);
        void OnTabCloseClick(winrt::Windows::Foundation::IInspectable const& sender,
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
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> RailCollapseRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> CompactNewTabRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> CompactNewTabMenuRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> FilterChanged;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> SearchActivationRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> SearchChanged;
        til::typed_event<TerminalApp::TabStrip, winrt::Microsoft::UI::Xaml::Controls::TabViewItem> GroupExpansionChanged;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> HistoryRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> HistoryClosed;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripHistoryActivationEventArgs> HistoryActivationRequested;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripPaneEventArgs> PaneActivationRequested;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripPaneEventArgs> PaneCloseRequested;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripCloseRequestedEventArgs> TabRenameRequested;
        til::typed_event<TerminalApp::TabStrip, TerminalApp::TabStripCloseRequestedEventArgs> TabFocusRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> VisibleFieldsChanged;
        til::event<winrt::delegate<>> HistoryProjectionChanged;

    private:
        TerminalApp::TabStripOrientation _orientation{ TerminalApp::TabStripOrientation::Vertical };
        bool _tabsVisible{ true };
        bool _isRailCollapsed{ false };
        bool _searchActive{ false };
        bool _syncingSearchState{ false };
        bool _searchPanelExpanded{ false };
        bool _searchAnimationEnabled{ false };
        uint64_t _searchAnimationGeneration{ 0 };
        winrt::Windows::UI::Xaml::Media::Animation::Storyboard _searchPanelStoryboard{ nullptr };
        bool _projectionControlsEnabled{ true };
        winrt::hstring _searchQuery;
        bool _historyActive{ false };
        bool _agentFilterTelemetryPending{ false };
        friend class ::TerminalAppLocalTests::TabTests;
        bool _historyLoading{ false };
        bool _historyActivating{ false };
        bool _syncingHistorySearchState{ false };
        winrt::hstring _historySearchQuery;
        winrt::hstring _historyError;
        winrt::hstring _historyRefreshError;
        TerminalApp::TabStripFilterMode _filterMode{ TerminalApp::TabStripFilterMode::AllTabs };
        bool _richTabRepositoryVisible{ false };
        bool _richTabBranchVisible{ false };
        bool _richTabAgentStatusVisible{ true };
        bool _richTabWorkingDirectoryVisible{ true };
        bool _richTabChangesVisible{ false };
        bool _richTabMetadataControlsVisible{ true };
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> _tabItems{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripDisplayItem> _displayItems{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripHistoryItem> _historyItems{ nullptr };
        std::vector<TerminalApp::TabStripHistoryItem> _historySnapshot;
        std::vector<std::vector<winrt::hstring>> _historySearchTerms;
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable>::VectorChanged_revoker _vectorChangedRevoker;
        winrt::Windows::UI::ViewManagement::AccessibilitySettings _accessibilitySettings;
        winrt::Windows::UI::ViewManagement::AccessibilitySettings::HighContrastChanged_revoker _highContrastChangedRevoker;

        struct CloseRequestedSubscription
        {
            winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> Item;
            winrt::event_token LoadedToken;
            winrt::event_token LayoutUpdatedToken;
            winrt::weak_ref<winrt::Windows::UI::Xaml::Controls::Button> CloseButton;
            winrt::event_token ClickToken;
        };
        std::unordered_map<void*, CloseRequestedSubscription> _closeRequestedSubscriptions;
        struct TabItemVisibilityState
        {
            winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> Item;
            bool Visible{ true };
        };
        std::unordered_map<void*, TabItemVisibilityState> _tabItemVisibility;
        struct GroupExpansionState
        {
            winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> Item;
            bool Expanded{ true };
        };
        std::unordered_map<void*, GroupExpansionState> _groupExpansion;
        struct PendingHeaderTransfer
        {
            winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> Item;
            winrt::Windows::Foundation::IInspectable Header;
        };
        std::unordered_map<void*, PendingHeaderTransfer> _pendingHeaderTransfers;
        bool _deferringHeaderRestore{ false };

        // The item currently being dragged. Set in OnDragItemsStarting, cleared in
        // OnDragItemsCompleted. If DropResult is None, this is the item to fire
        // TabDroppedOutside with (tearoff-to-new-window signal).
        winrt::Windows::Foundation::IInspectable _draggingItem{ nullptr };
        std::optional<uint32_t> _draggingIndex;
        bool _syncingNativeReorder{ false };
        bool _dragCollectionChanged{ false };
        bool _keepRichTabMetadataFlyoutOpen{ false };
        winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> _pressedHeaderTab;
        bool _pressedHeaderWasSelected{ false };

        void _onItemsVectorChanged(winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> const& sender,
                                     winrt::Windows::Foundation::Collections::IVectorChangedEventArgs const& args);
        TerminalApp::TabStripDisplayItem _displayItemAt(uint32_t index) const;
        TerminalApp::TabStripDisplayItem _displayItemForTab(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab) const;
        TerminalApp::TabStripDisplayItem _makeDisplayItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab);
        void _restoreDisplayItemHeader(TerminalApp::TabStripDisplayItem const& display);
        static winrt::Microsoft::UI::Xaml::Controls::TabViewItem _tabFromItem(winrt::Windows::Foundation::IInspectable const& item);
        void _syncDisplayItems();
        void _hookCloseRequested(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _refreshCloseButton(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _removeStaleCloseRequestedSubscriptions(winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> const& items);
        void _clearCloseRequestedSubscriptions();
        void _applyRailState();
        void _applyTabItemRailState(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _restoreTabItemRailState(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _applyTabItemVisibility(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item);
        void _applyTabItemVisibility(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& item,
                                     winrt::Windows::UI::Xaml::Controls::ListViewItem const& container);
        void _pruneTabItemVisibility();
        void _setSearchPanelExpanded(bool expanded, bool animate);
        void _updateSearchVisualState();
        static std::vector<winrt::hstring> _buildHistorySearchTerms(TerminalApp::TabStripHistoryItem const& item);
        winrt::Windows::UI::Xaml::Style _historyStatusTextStyle(winrt::hstring const& status);
        bool _matchesHistorySearch(size_t index) const;
        void _applyHistoryProjection(bool preserveScroll = false);
        void _updateHistoryVisualState();
        uint32_t _richTabMetadataSelectionCount() const noexcept;
        void _updateRichTabMetadataSelectionState();
        void _refreshDisplayItemVisuals(TerminalApp::TabStripDisplayItem const& display);
        static void _updateDisplayItemVisuals(winrt::Windows::UI::Xaml::FrameworkElement const& root,
                                              TerminalApp::TabStripDisplayItem const& display);
        void _toggleGroup(TerminalApp::TabStripDisplayItem const& display);

        // Axis-parameterized per B→C rules. Returns -1 to mean "append at end."
        // Non-const because it reaches into the XAML-generated ItemsList().
        int32_t _computeDropIndex(winrt::Windows::Foundation::Point const& stripRelativePos);
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabStrip);
}
