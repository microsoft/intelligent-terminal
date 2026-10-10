// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"
#include "winrt/Windows.UI.ViewManagement.h"

#include "TabStrip.g.h"
#include "SidebarFiltersViewModel.g.h"
#include "TabStripSelectionChangedEventArgs.g.h"
#include "TabStripCloseRequestedEventArgs.g.h"
#include "TabStripDragStartingEventArgs.g.h"
#include "TabStripDroppedOutsideEventArgs.g.h"
#include "TabStripHistoryItem.g.h"
#include "TabStripHistoryActivationEventArgs.g.h"
#include "TabStripPaneItem.g.h"
#include "TabStripDisplayItem.g.h"
#include "TabStripPaneEventArgs.g.h"
#include "TabStripItemTemplateSelector.g.h"
#include "TabStripHistoryHeader.g.h"
#include "TabStripHistoryHeaderAutomationPeer.g.h"

namespace TerminalAppLocalTests
{
    class TabTests;
}

namespace winrt::TerminalApp::implementation
{
    struct SidebarFiltersViewModel : SidebarFiltersViewModelT<SidebarFiltersViewModel>
    {
        SidebarFiltersViewModel() = default;
        bool ShowAgentsOnly() const noexcept { return _showAgentsOnly; }
        void ShowAgentsOnly(bool value);
        bool ShowRecentAgentSessions() const noexcept { return _showRecentAgentSessions; }
        void ShowRecentAgentSessions(bool value);
        winrt::hstring SearchQuery() const { return _searchQuery; }
        void SearchQuery(winrt::hstring const& value);
        bool HasSearchQuery() const noexcept { return !_searchQuery.empty(); }
        bool AgentsOnlyEffective() const noexcept { return _showAgentsOnly && !HasSearchQuery(); }
        bool RecentAgentSessionsVisible() const noexcept { return _showRecentAgentSessions || HasSearchQuery(); }
        til::property_changed_event PropertyChanged;

    private:
        bool _showAgentsOnly{};
        bool _showRecentAgentSessions{};
        winrt::hstring _searchQuery;
    };

    struct TabStripItemTemplateSelector : TabStripItemTemplateSelectorT<TabStripItemTemplateSelector>
    {
        winrt::Windows::UI::Xaml::DataTemplate LiveTemplate{ nullptr };
        winrt::Windows::UI::Xaml::DataTemplate HistoryTemplate{ nullptr };
        winrt::Windows::UI::Xaml::DataTemplate HeaderTemplate{ nullptr };
        winrt::Windows::UI::Xaml::DataTemplate SelectTemplateCore(winrt::Windows::Foundation::IInspectable const& item);
        winrt::Windows::UI::Xaml::DataTemplate SelectTemplateCore(winrt::Windows::Foundation::IInspectable const& item,
                                                               winrt::Windows::UI::Xaml::DependencyObject const&);
    };

    struct TabStripHistoryHeader : TabStripHistoryHeaderT<TabStripHistoryHeader>
    {
        TabStripHistoryHeader();
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationPeer OnCreateAutomationPeer();
    };
    struct TabStripHistoryHeaderAutomationPeer : TabStripHistoryHeaderAutomationPeerT<TabStripHistoryHeaderAutomationPeer>
    {
        explicit TabStripHistoryHeaderAutomationPeer(TerminalApp::TabStripHistoryHeader const& owner);
        winrt::Windows::Foundation::IInspectable GetPatternCore(winrt::Windows::UI::Xaml::Automation::Peers::PatternInterface const& pattern);
        winrt::Windows::UI::Xaml::Automation::Peers::AutomationControlType GetAutomationControlTypeCore() const;
        winrt::hstring GetClassNameCore() const;
        winrt::Windows::UI::Xaml::Automation::ExpandCollapseState ExpandCollapseState() const;
        void Expand();
        void Collapse();
    };

    struct TabStripHistoryItem : TabStripHistoryItemT<TabStripHistoryItem>
    {
        TabStripHistoryItem() = default;
        WINRT_PROPERTY(winrt::hstring, SessionId);
        WINRT_PROPERTY(winrt::hstring, Title);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Subtitle, PropertyChanged.raise);
        WINRT_PROPERTY(std::optional<uint64_t>, LastActivityAtMs);
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
        WINRT_OBSERVABLE_PROPERTY(bool, IsHistorical, PropertyChanged.raise, false);
        WINRT_PROPERTY(bool, BackgroundTab, false);
        WINRT_PROPERTY(bool, OtherWindow, false);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, StatusText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Style, StatusTextStyle, PropertyChanged.raise, nullptr);
        WINRT_PROPERTY(winrt::Windows::UI::Xaml::DataTemplate, IconTemplate, nullptr);
        WINRT_OBSERVABLE_PROPERTY(bool, IsCurrent, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CurrentBackground, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CurrentForeground, PropertyChanged.raise, nullptr);
        WINRT_OBSERVABLE_PROPERTY(bool, IsKeepRunning, PropertyChanged.raise, false);

    public:
        bool RefreshAge(uint64_t nowMs);
        void UpdateStatusText(winrt::hstring const& text);
        til::property_changed_event PropertyChanged;

    private:
        std::optional<std::pair<uint64_t, uint64_t>> _ageKey;
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

    public:
        winrt::Windows::UI::Xaml::Controls::IconElement Icon() const;
        void Icon(winrt::Windows::UI::Xaml::Controls::IconElement const& value);
        winrt::Windows::UI::Xaml::Controls::IconElement CreateIcon(winrt::Windows::Foundation::IInspectable const& source) const;
        winrt::Microsoft::UI::Xaml::Controls::IconSource IconSource() const noexcept { return _iconSource; }
        void IconSource(winrt::Windows::Foundation::IInspectable const& value);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, HighlightQuery, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(uint64_t, ProgressState, PropertyChanged.raise, 0);
        WINRT_OBSERVABLE_PROPERTY(bool, IsProgressRingActive, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(bool, IsProgressRingIndeterminate, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(uint32_t, ProgressValue, PropertyChanged.raise, 0);

    public:
        til::property_changed_event PropertyChanged;

    private:
        winrt::Microsoft::UI::Xaml::Controls::TabViewItem _tab{ nullptr };
        uint32_t _contentId{};
        winrt::hstring _iconPath;
        winrt::Microsoft::UI::Xaml::Controls::IconSource _iconSource{ nullptr };
    };

    struct TabStripDisplayItem : TabStripDisplayItemT<TabStripDisplayItem>
    {
        TabStripDisplayItem() = default;
        TabStripDisplayItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem tab,
                            winrt::Windows::Foundation::IInspectable header);

        winrt::Microsoft::UI::Xaml::Controls::TabViewItem Tab() const noexcept { return _tab; }
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripPaneItem> PaneItems() const noexcept { return _paneItems; }
        winrt::Windows::Foundation::IInspectable Header() const { return _header; }
        void Header(winrt::Windows::Foundation::IInspectable const& value);
        TerminalApp::TabHeaderPresentation Presentation() const { return Header().as<TerminalApp::TabHeaderPresentation>(); }
        WINRT_OBSERVABLE_PROPERTY(bool, IsMetadataVisible, PropertyChanged.raise, false);
        WINRT_OBSERVABLE_PROPERTY(bool, ShowProgressRing, PropertyChanged.raise, true);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Title, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SearchText, PropertyChanged.raise);

    public:
        winrt::Windows::UI::Xaml::Controls::IconElement Icon() const;
        void Icon(winrt::Windows::UI::Xaml::Controls::IconElement const& value);
        winrt::Windows::UI::Xaml::Controls::IconElement CreateIcon(winrt::Windows::Foundation::IInspectable const& source) const;
        winrt::Microsoft::UI::Xaml::Controls::IconSource IconSource() const noexcept { return _iconSource; }
        void IconSource(winrt::Windows::Foundation::IInspectable const& value);
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
        void SyncTabPresentation(bool railCollapsed, bool verticalPresentation);
        void SyncIcon(winrt::hstring const& iconPath);
        void SyncIcon(winrt::hstring const& iconPath, winrt::Microsoft::UI::Xaml::Controls::IconSource const& source);
        void UpdatePresentation(bool railCollapsed, bool verticalPresentation);
        bool HeaderProgressProjectedToPaneRows() const noexcept { return _headerProgressProjectedToPaneRows; }
        void HeaderProgressProjectedToPaneRows(bool value) noexcept { _headerProgressProjectedToPaneRows = value; }
        til::property_changed_event PropertyChanged;

    private:
        winrt::Microsoft::UI::Xaml::Controls::TabViewItem _tab{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripPaneItem> _paneItems{ nullptr };
        std::optional<winrt::hstring> _iconPath;
        TerminalApp::TabHeaderPresentation _header{ nullptr };
        winrt::Microsoft::UI::Xaml::Controls::IconSource _iconSource{ nullptr };
        bool _headerProgressProjectedToPaneRows{ false };
        void _setIconSource(winrt::Windows::Foundation::IInspectable const& value, std::optional<winrt::hstring> iconPath);
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
                                winrt::hstring const& iconPath,
                                bool useNativeIconSource = false);
        void SetPaneItems(winrt::Windows::Foundation::IInspectable const& item,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup);
        void SetPaneItems(winrt::Windows::Foundation::IInspectable const& item,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup,
                          bool headerProgressProjectedToPaneRows);
        void SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup);
        void SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                          winrt::Windows::Foundation::Collections::IVector<TerminalApp::TabStripPaneItem> const& panes,
                          bool isGroup,
                          bool headerProgressProjectedToPaneRows);
        winrt::Windows::Foundation::IInspectable HeaderForTab(winrt::Windows::Foundation::IInspectable const& item, bool realize = false);
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
        TerminalApp::TabStripFilterMode FilterMode() const noexcept { return _sidebarFilters.ShowAgentsOnly() ? TerminalApp::TabStripFilterMode::AgentsOnly : TerminalApp::TabStripFilterMode::AllTabs; }
        void FilterMode(TerminalApp::TabStripFilterMode value);
        bool SearchActive() const noexcept { return _searchActive; }
        void SearchActive(bool value);
        winrt::hstring SearchQuery() const { return _sidebarFilters.SearchQuery(); }
        void SearchQuery(winrt::hstring const& value);
        bool FocusTabSearch();
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripHistoryItem> HistoryItems() const { return _historyItems; }
        void CommitHistorySnapshot(std::vector<TerminalApp::TabStripHistoryItem> items, bool ready = false);
        void InvalidateHistorySnapshotReadiness() noexcept { _historySnapshotReady = false; }
        void TryCompleteAgentFilterTelemetry();
        struct RepresentedHistorySession
        {
            winrt::hstring sessionId;
            winrt::hstring agentId;
            winrt::hstring paneSessionId;
            bool operator==(const RepresentedHistorySession&) const = default;
        };
        void SetRepresentedHistorySessions(std::vector<RepresentedHistorySession> sessions);
        void SetCurrentHistoryItem(TerminalApp::TabStripHistoryItem const& item,
                                   winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab);
        void UpdateHistoryKeepRunning(const std::function<bool(const winrt::hstring&)>& isPaneKeepRunning);
        bool ApplyHistoryStatusDelta(winrt::hstring const& sessionId,
                                     winrt::hstring const& paneSessionId,
                                     winrt::hstring const& status,
                                     winrt::hstring const& statusText);
        bool HasHistoryItems() const noexcept { return !_historySnapshot.empty(); }
        void ClearHistorySnapshot();
        void OpenHistory();
        bool HistoryActive() const noexcept { return _sidebarFilters.ShowRecentAgentSessions(); }
        TerminalApp::SidebarFiltersViewModel SidebarFilters() const noexcept { return _sidebarFilters; }
        winrt::Windows::UI::Xaml::Controls::Grid HistorySection();
        TerminalApp::TabStripHistoryHeader HistoryHeaderButton() { return _historyElement(L"HistoryHeaderButton").try_as<TerminalApp::TabStripHistoryHeader>(); }
        winrt::Windows::UI::Xaml::Controls::TextBlock HistoryHeader() { return _historyElement(L"HistoryHeader").try_as<winrt::Windows::UI::Xaml::Controls::TextBlock>(); }
        winrt::Windows::UI::Xaml::Controls::FontIcon HistoryChevron() { return _historyElement(L"HistoryChevron").try_as<winrt::Windows::UI::Xaml::Controls::FontIcon>(); }
        winrt::Windows::UI::Xaml::Controls::Grid HistoryStatusPanel() { return _historyElement(L"HistoryStatusPanel").try_as<winrt::Windows::UI::Xaml::Controls::Grid>(); }
        winrt::Windows::UI::Xaml::Controls::ProgressRing HistoryLoadingIndicator() { return _historyElement(L"HistoryLoadingIndicator").try_as<winrt::Windows::UI::Xaml::Controls::ProgressRing>(); }
        winrt::Windows::UI::Xaml::Controls::TextBlock HistoryMessage() { return _historyElement(L"HistoryMessage").try_as<winrt::Windows::UI::Xaml::Controls::TextBlock>(); }
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
        void SetVerticalPresentation(bool vertical);
        void RestoreTabOrder(const std::vector<winrt::Windows::Foundation::IInspectable>& items);
        void SetTabPinned(const winrt::Microsoft::UI::Xaml::Controls::TabViewItem& item, bool pinned);
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
        bool RichTabGitAvailable() const noexcept { return _richTabGitAvailable; }
        void RichTabGitAvailable(bool value);
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
        void OnHistoryExpandedChanged(winrt::Windows::Foundation::IInspectable const& sender,
                                      winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistorySectionLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                                    winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistorySectionUnloaded(winrt::Windows::Foundation::IInspectable const& sender,
                                      winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistoryItemClick(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::Controls::ItemClickEventArgs const& e);
        void OnHistoryOwnershipClick(winrt::Windows::Foundation::IInspectable const& sender,
                                     winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHistoryRowLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                                winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnContainerContentChanging(winrt::Windows::UI::Xaml::Controls::ListViewBase const& sender,
                                        winrt::Windows::UI::Xaml::Controls::ContainerContentChangingEventArgs const& e);
        void OnTabHeaderLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnRowHeaderLoaded(winrt::Windows::Foundation::IInspectable const& sender,
                               winrt::Windows::UI::Xaml::RoutedEventArgs const& e);
        void OnHeaderRenameEnded(winrt::Windows::Foundation::IInspectable const& sender,
                                 winrt::Windows::Foundation::IInspectable const& args);
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
        til::typed_event<winrt::Microsoft::UI::Xaml::Controls::TabViewItem, winrt::hstring> HeaderTitleChangeRequested;
        til::typed_event<TerminalApp::TabStrip, winrt::Windows::Foundation::IInspectable> VisibleFieldsChanged;
        til::event<winrt::delegate<>> HistoryProjectionChanged;

    private:
        TerminalApp::TabStripOrientation _orientation{ TerminalApp::TabStripOrientation::Vertical };
        bool _tabsVisible{ true };
        bool _isRailCollapsed{ false };
        bool _isVerticalPresentation{ true };
        bool _searchActive{ false };
        bool _syncingSearchState{ false };
        bool _searchPanelExpanded{ false };
        bool _searchAnimationEnabled{ false };
        uint64_t _searchAnimationGeneration{ 0 };
        winrt::Windows::UI::Xaml::Media::Animation::Storyboard _searchPanelStoryboard{ nullptr };
        bool _projectionControlsEnabled{ true };
        TerminalApp::SidebarFiltersViewModel _sidebarFilters{ winrt::make<SidebarFiltersViewModel>() };
        winrt::Windows::UI::Xaml::Data::INotifyPropertyChanged::PropertyChanged_revoker _sidebarFiltersChanged;
        void _onSidebarFiltersChanged(winrt::Windows::Foundation::IInspectable const&, winrt::Windows::UI::Xaml::Data::PropertyChangedEventArgs const&);
        winrt::Windows::UI::Xaml::DispatcherTimer _historyAgeTimer{ nullptr };
        void _updateHistoryAgeTimer();
        void _refreshHistoryAges(uint64_t nowMs);
        std::vector<RepresentedHistorySession> _representedHistorySessions;
        bool _agentFilterTelemetryPending{ false };
        bool _historySnapshotReady{ false };
        friend class ::TerminalAppLocalTests::TabTests;
        bool _historyLoading{ false };
        bool _historyActivating{ false };
        bool _syncingHistorySearchState{ false };
        winrt::hstring _historySearchQuery;
        bool _syncingViewportSelection{ false };
        winrt::hstring _historyError;
        winrt::hstring _historyRefreshError;
        bool _richTabRepositoryVisible{ false };
        bool _richTabBranchVisible{ false };
        bool _richTabAgentStatusVisible{ true };
        bool _richTabWorkingDirectoryVisible{ true };
        bool _richTabChangesVisible{ false };
        bool _richTabGitAvailable{ true };
        bool _richTabMetadataControlsVisible{ true };
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> _tabItems{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripDisplayItem> _displayItems{ nullptr };
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> _viewportItems{ nullptr };
        winrt::Windows::Foundation::IInspectable _historySectionItem{ nullptr };
        winrt::weak_ref<winrt::Windows::UI::Xaml::Controls::Grid> _historySection;
        bool _historyExpanded{ true };
        bool _syncingHistorySection{ false };
        winrt::Windows::UI::Xaml::FrameworkElement _historyElement(const wchar_t* name)
        {
            const auto root = HistorySection();
            return root ? root.FindName(name).try_as<winrt::Windows::UI::Xaml::FrameworkElement>() : nullptr;
        }
        winrt::Windows::Foundation::Collections::IObservableVector<TerminalApp::TabStripHistoryItem> _historyItems{ nullptr };
        std::vector<TerminalApp::TabStripHistoryItem> _historySnapshot;
        std::vector<std::vector<winrt::hstring>> _historySearchTerms;
        winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable>::VectorChanged_revoker _vectorChangedRevoker;
        winrt::Windows::UI::ViewManagement::AccessibilitySettings _accessibilitySettings;
        winrt::Windows::UI::ViewManagement::AccessibilitySettings::HighContrastChanged_revoker _highContrastChangedRevoker{};
        bool _highContrast{ false };

        struct CloseRequestedSubscription
        {
            winrt::weak_ref<winrt::Microsoft::UI::Xaml::Controls::TabViewItem> Item;
            winrt::event_token LoadedToken;
            winrt::event_token LayoutUpdatedToken;
            winrt::weak_ref<winrt::Windows::UI::Xaml::Controls::Button> CloseButton;
            winrt::event_token ClickToken;
        };
        std::unordered_map<void*, CloseRequestedSubscription> _closeRequestedSubscriptions;
        struct RowAutomationSubscriptions
        {
            winrt::weak_ref<winrt::Windows::UI::Xaml::Controls::ListViewItem> Container;
            winrt::Windows::UI::Xaml::Data::INotifyPropertyChanged::PropertyChanged_revoker PresentationChanged;
            winrt::Windows::UI::Xaml::Data::INotifyPropertyChanged::PropertyChanged_revoker DisplayChanged;
        };
        std::unordered_map<void*, RowAutomationSubscriptions> _rowAutomationSubscriptions;
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

        void _onListKeyDown(winrt::Windows::Foundation::IInspectable const& sender,
                            winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs const& e);
        void _onHistoryPreviewKeyDown(winrt::Windows::Foundation::IInspectable const& sender,
                                      winrt::Windows::UI::Xaml::Input::KeyRoutedEventArgs const& e);
        void _onItemsVectorChanged(winrt::Windows::Foundation::Collections::IObservableVector<winrt::Windows::Foundation::IInspectable> const& sender,
                                     winrt::Windows::Foundation::Collections::IVectorChangedEventArgs const& args);
        TerminalApp::TabStripDisplayItem _displayItemAt(uint32_t index) const;
        TerminalApp::TabStripDisplayItem _displayItemForTab(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab) const;
        TerminalApp::TabStripDisplayItem _makeDisplayItem(winrt::Microsoft::UI::Xaml::Controls::TabViewItem const& tab);
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
        void _syncViewportItems();
        uint32_t _richTabMetadataSelectionCount() const noexcept;
        void _updateRichTabMetadataSelectionState();
        void _setHighContrastMode(bool enabled);
        void _refreshDisplayItemVisuals(TerminalApp::TabStripDisplayItem const& display);
        void _refreshRealizedPaneRowVisuals(bool highContrastActive);
        void _refreshPaneRowVisuals(TerminalApp::TabStripDisplayItem const& display);
        void _refreshPaneRowVisuals(TerminalApp::TabStripDisplayItem const& display,
                                    bool highContrastActive);
        void _updateDisplayItemVisuals(winrt::Windows::UI::Xaml::FrameworkElement const& root,
                                       TerminalApp::TabStripDisplayItem const& display);
        void _updatePaneRowVisuals(winrt::Windows::UI::Xaml::FrameworkElement const& root,
                                   TerminalApp::TabStripPaneItem const& pane);
        void _updatePaneRowVisuals(winrt::Windows::UI::Xaml::FrameworkElement const& root,
                                   TerminalApp::TabStripPaneItem const& pane,
                                   bool highContrastActive);
        void _toggleGroup(TerminalApp::TabStripDisplayItem const& display);

        // Axis-parameterized per B→C rules. Returns -1 to mean "append at end."
        // Non-const because it reaches into the XAML-generated ItemsList().
        int32_t _computeDropIndex(winrt::Windows::Foundation::Point const& stripRelativePos);
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(SidebarFiltersViewModel);
    BASIC_FACTORY(TabStripItemTemplateSelector);
    BASIC_FACTORY(TabStripHistoryHeader);
    BASIC_FACTORY(TabStripHistoryHeaderAutomationPeer);
    BASIC_FACTORY(TabStrip);
}
