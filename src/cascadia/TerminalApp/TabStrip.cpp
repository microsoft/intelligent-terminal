// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// PROTOTYPE — see investigation-vertical-tabs.md. Not shipped.

#include "pch.h"
#include "TabStrip.h"
#include "TerminalPage.h"
#include "AgentIconUtils.h"
#include "TabStripAutomationPeer.h"
#include "..\RichTabProvider\ProviderBroker.h"
#include "TabHeaderControl.h"
#include "Utils.h"

#include <array>
#include <cmath>
#include <set>
#include <unordered_set>

#include "TabStrip.g.cpp"
#include "SidebarFiltersViewModel.g.cpp"
#include "TabStripSelectionChangedEventArgs.g.cpp"
#include "TabStripCloseRequestedEventArgs.g.cpp"
#include "TabStripDragStartingEventArgs.g.cpp"
#include "TabStripDroppedOutsideEventArgs.g.cpp"
#include "TabStripPaneItem.g.cpp"
#include "TabStripDisplayItem.g.cpp"
#include "TabStripPaneEventArgs.g.cpp"
#include "TabStripItemTemplateSelector.g.cpp"
#include "TabStripHistoryHeader.g.cpp"
#include "TabStripHistoryHeaderAutomationPeer.g.cpp"

using namespace winrt;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;

static constexpr double SearchPanelExpandedHeight = 40.0;
static constexpr auto SearchPanelAnimationDuration = std::chrono::milliseconds{ 200 };
// Keep the pane dismiss implementation intact so it can be restored when split-pane chrome exposes the same affordance.
static constexpr bool PaneDismissActionsEnabled = false;

namespace winrt
{
    namespace MUX = Microsoft::UI::Xaml;
    namespace WUX = Windows::UI::Xaml;
}

namespace winrt::TerminalApp::implementation
{
    void SidebarFiltersViewModel::ShowAgentsOnly(const bool value)
    {
        if (_showAgentsOnly != value)
        {
            _showAgentsOnly = value;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"ShowAgentsOnly" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"AgentsOnlyEffective" });
        }
    }

    void SidebarFiltersViewModel::ShowRecentAgentSessions(const bool value)
    {
        if (_showRecentAgentSessions != value)
        {
            _showRecentAgentSessions = value;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"ShowRecentAgentSessions" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"RecentAgentSessionsVisible" });
        }
    }

    void SidebarFiltersViewModel::SearchQuery(winrt::hstring const& value)
    {
        if (_searchQuery != value)
        {
            _searchQuery = value;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"SearchQuery" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"HasSearchQuery" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"AgentsOnlyEffective" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"RecentAgentSessionsVisible" });
        }
    }

    bool TabStripHistoryItem::RefreshAge(const uint64_t nowMs)
    {
        const auto lastActivityAtMs = LastActivityAtMs();
        if (!lastActivityAtMs)
        {
            return false;
        }

        const auto elapsedMs = nowMs > *lastActivityAtMs ? nowMs - *lastActivityAtMs : 0;
        const auto key = std::pair{ *lastActivityAtMs, elapsedMs / 60'000 };
        if (_ageKey == key)
        {
            return false;
        }
        _ageKey = key;

        const auto text = TerminalPage::_SidebarHistoryAgeText(lastActivityAtMs, nowMs);
        const auto changed = Subtitle() != text;
        Subtitle(text);
        return changed;
    }

    void TabStripHistoryItem::UpdateStatusText(winrt::hstring const& text)
    {
        const auto status = Status();
        const auto liveIt = status == L"Idle" || status == L"Working" ||
                            status == L"Attention" || status == L"Error";
        const auto hint = liveIt && OtherWindow() && !BackgroundTab() && !_locallyHeadless ?
                              winrt::hstring{ RS_fmt(L"VerticalTabsHistoryOtherWindowStatusFormat", L"") } :
                              winrt::hstring{};
        StatusText(text + hint);
        StatusLabelText(text);
        WindowHintText(hint);
    }

    bool TabStripHistoryItem::UpdateHeadlessState(const bool headless)
    {
        if (_locallyHeadless != headless)
        {
            _locallyHeadless = headless;
            UpdateStatusText(TerminalPage::_SidebarHistoryStatusText(winrt::to_string(StatusForPresentation())));
            return true;
        }
        return false;
    }

    bool TabStripHistoryItem::SetUnreadCompletion(const bool unread)
    {
        const auto previous = _hasUnreadCompletion;
        _hasUnreadCompletion = unread && Status() == L"Idle";
        if (previous || _hasUnreadCompletion)
        {
            UpdateStatusText(TerminalPage::_SidebarHistoryStatusText(winrt::to_string(StatusForPresentation())));
        }
        return previous != _hasUnreadCompletion;
    }

    DataTemplate TabStripItemTemplateSelector::SelectTemplateCore(IInspectable const& item)
    {
        return item.try_as<TerminalApp::TabStripDisplayItem>() ? LiveTemplate :
               item.try_as<TerminalApp::TabStripHistoryItem>() ? HistoryTemplate : HeaderTemplate;
    }

    DataTemplate TabStripItemTemplateSelector::SelectTemplateCore(IInspectable const& item, DependencyObject const&)
    {
        return SelectTemplateCore(item);
    }

    TabStripHistoryHeader::TabStripHistoryHeader()
    {
        const auto changed = [weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                using namespace WUX::Automation;
                if (const auto peer = Peers::FrameworkElementAutomationPeer::FromElement(*self))
                {
                    const auto expanded = self->IsChecked().Value();
                    peer.RaisePropertyChangedEvent(ExpandCollapsePatternIdentifiers::ExpandCollapseStateProperty(),
                                                   box_value(expanded ? ExpandCollapseState::Collapsed : ExpandCollapseState::Expanded),
                                                   box_value(expanded ? ExpandCollapseState::Expanded : ExpandCollapseState::Collapsed));
                }
            }
        };
        Checked(changed);
        Unchecked(changed);
    }

    WUX::Automation::Peers::AutomationPeer TabStripHistoryHeader::OnCreateAutomationPeer()
    {
        return winrt::make<TabStripHistoryHeaderAutomationPeer>(*this);
    }

    TabStripHistoryHeaderAutomationPeer::TabStripHistoryHeaderAutomationPeer(TerminalApp::TabStripHistoryHeader const& owner) :
        TabStripHistoryHeaderAutomationPeerT<TabStripHistoryHeaderAutomationPeer>(owner)
    {
    }

    IInspectable TabStripHistoryHeaderAutomationPeer::GetPatternCore(WUX::Automation::Peers::PatternInterface const& pattern)
    {
        if (pattern == WUX::Automation::Peers::PatternInterface::ExpandCollapse)
        {
            return *this;
        }
        return m_inner.as<WUX::Automation::Peers::IAutomationPeerOverrides>().GetPatternCore(pattern);
    }

    WUX::Automation::Peers::AutomationControlType TabStripHistoryHeaderAutomationPeer::GetAutomationControlTypeCore() const
    {
        return WUX::Automation::Peers::AutomationControlType::Button;
    }

    hstring TabStripHistoryHeaderAutomationPeer::GetClassNameCore() const
    {
        return L"ToggleButton";
    }

    WUX::Automation::ExpandCollapseState TabStripHistoryHeaderAutomationPeer::ExpandCollapseState() const
    {
        return Owner().as<TerminalApp::TabStripHistoryHeader>().IsChecked().Value() ?
                   WUX::Automation::ExpandCollapseState::Expanded : WUX::Automation::ExpandCollapseState::Collapsed;
    }

    void TabStripHistoryHeaderAutomationPeer::Expand()
    {
        Owner().as<TerminalApp::TabStripHistoryHeader>().IsChecked(true);
    }

    void TabStripHistoryHeaderAutomationPeer::Collapse()
    {
        Owner().as<TerminalApp::TabStripHistoryHeader>().IsChecked(false);
    }

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
        if (_iconPath == iconPath && IconSource())
        {
            return;
        }

        _iconPath = iconPath;
        if (iconPath.empty())
        {
            MUX::Controls::FontIconSource fallback;
            fallback.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons, Segoe MDL2 Assets" });
            fallback.FontSize(12);
            fallback.Glyph(L"\xE756");
            IconSource(fallback);
        }
        else
        {
            IconSource(::Microsoft::Terminal::UI::AgentIcons::SourceForIconPath(iconPath, false));
        }
    }

    WUX::Controls::IconElement TabStripPaneItem::Icon() const
    {
        return CreateIcon(IconSource());
    }

    WUX::Controls::IconElement TabStripPaneItem::CreateIcon(IInspectable const& source) const
    {
        auto element = ::Microsoft::Terminal::UI::AgentIcons::ElementForIconSource(source.as<MUX::Controls::IconSource>(), _iconPath);
        element.Width(16);
        element.Height(16);
        return element;
    }

    void TabStripPaneItem::IconSource(IInspectable const& value)
    {
        const auto source = value.try_as<MUX::Controls::IconSource>();
        THROW_HR_IF(E_INVALIDARG, !source);
        if (_iconSource != source)
        {
            _iconSource = source;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"IconSource" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"Icon" });
        }
    }

    void TabStripPaneItem::Icon(WUX::Controls::IconElement const& value)
    {
        IconSource(::Microsoft::Terminal::UI::AgentIcons::SourceForIconElement(value));
    }

    void TabStripPaneItem::UpdateCloseVisibility(bool railCollapsed)
    {
        CloseVisibility(PaneDismissActionsEnabled && !railCollapsed && (ActiveIndicatorVisibility() == Visibility::Visible || _pointerOver || _keyboardFocused) ?
                            Visibility::Visible :
                            Visibility::Collapsed);
    }

    void TabStripPaneItem::SetPointerOver(bool pointerOver, bool railCollapsed)
    {
        _pointerOver = pointerOver;
        UpdateCloseVisibility(railCollapsed);
    }

    void TabStripPaneItem::SetKeyboardFocused(bool focused, bool railCollapsed)
    {
        _keyboardFocused = focused;
        UpdateCloseVisibility(railCollapsed);
    }

    TabStripDisplayItem::TabStripDisplayItem(MUX::Controls::TabViewItem tab,
                                             IInspectable header) :
        _tab{ std::move(tab) },
        _paneItems{ single_threaded_observable_vector<TerminalApp::TabStripPaneItem>() }
    {
        Header(header);
    }

    void TabStripDisplayItem::Header(IInspectable const& value)
    {
        TerminalApp::TabHeaderPresentation presentation{ nullptr };
        if (const auto control = value.try_as<TerminalApp::TabHeaderControl>())
        {
            presentation = control.Presentation();
        }
        else if (const auto data = value.try_as<TerminalApp::TabHeaderPresentation>())
        {
            presentation = data;
        }
        else if (!value || value.try_as<hstring>())
        {
            presentation = winrt::make<TabHeaderPresentation>();
            if (const auto text = value.try_as<hstring>())
            {
                presentation.Title(*text);
            }
        }
        else
        {
            THROW_HR(E_INVALIDARG);
        }
        if (_header != presentation)
        {
            _header = presentation;
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"Header" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"Presentation" });
        }
    }

    WUX::Controls::IconElement TabStripDisplayItem::Icon() const
    {
        return CreateIcon(IconSource());
    }

    WUX::Controls::IconElement TabStripDisplayItem::CreateIcon(IInspectable const& source) const
    {
        auto element = ::Microsoft::Terminal::UI::AgentIcons::ElementForIconSource(source.as<MUX::Controls::IconSource>(), _iconPath.value_or(winrt::hstring{}));
        element.Width(16);
        element.Height(16);
        return element;
    }

    void TabStripDisplayItem::IconSource(IInspectable const& value)
    {
        _setIconSource(value, std::nullopt);
    }

    void TabStripDisplayItem::SyncIcon(hstring const& iconPath, MUX::Controls::IconSource const& source)
    {
        _setIconSource(source, iconPath);
    }

    void TabStripDisplayItem::_setIconSource(IInspectable const& value, std::optional<winrt::hstring> iconPath)
    {
        const auto source = value.try_as<MUX::Controls::IconSource>();
        THROW_HR_IF(E_INVALIDARG, !source);
        if (_iconSource != source || _iconPath != iconPath)
        {
            _iconSource = source;
            _iconPath = std::move(iconPath);
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"IconSource" });
            PropertyChanged.raise(*this, WUX::Data::PropertyChangedEventArgs{ L"Icon" });
        }
    }

    void TabStripDisplayItem::Icon(WUX::Controls::IconElement const& value)
    {
        IconSource(::Microsoft::Terminal::UI::AgentIcons::SourceForIconElement(value));
    }

    void TabStripDisplayItem::SyncTabPresentation(bool railCollapsed, bool verticalPresentation)
    {
        const auto presentation = Presentation();
        Title(presentation.Title());
        const auto status = presentation.TabStatus();
        IsPinned(status && status.IsPinned());
        IsMetadataVisible(!railCollapsed && !IsGroup() && !presentation.MetadataText().empty());
        ContextFlyout(_tab.ContextFlyout());
        if (!verticalPresentation)
        {
            _sidebarHints.reset();
        }
        _publishHints(verticalPresentation);
        HeaderVisibility(railCollapsed ? Visibility::Collapsed : Visibility::Visible);
        UpdateCloseVisibility(railCollapsed);
        UpdatePresentation(railCollapsed, verticalPresentation);
    }

    void TabStripDisplayItem::UpdateCloseVisibility(bool railCollapsed)
    {
        CloseVisibility(!railCollapsed && _tab.IsClosable() &&
                                (SelectionVisibility() == Visibility::Visible || _pointerOver || _keyboardFocused) ?
                            Visibility::Visible :
                            Visibility::Collapsed);
    }

    void TabStripDisplayItem::SetPointerOver(bool pointerOver, bool railCollapsed)
    {
        _pointerOver = pointerOver;
        UpdateCloseVisibility(railCollapsed);
    }

    void TabStripDisplayItem::SetKeyboardFocused(bool focused, bool railCollapsed)
    {
        _keyboardFocused = focused;
        UpdateCloseVisibility(railCollapsed);
    }

    void TabStripDisplayItem::SetSidebarHints(std::optional<std::pair<hstring, hstring>> hints, bool verticalPresentation)
    {
        _sidebarHints = verticalPresentation ? std::move(hints) : std::nullopt;
        _publishHints(verticalPresentation);
    }

    void TabStripDisplayItem::_publishHints(bool verticalPresentation)
    {
        AcceleratorKey(verticalPresentation && _sidebarHints ? _sidebarHints->first :
                                                              WUX::Automation::AutomationProperties::GetAcceleratorKey(_tab));
        ToolTipText(verticalPresentation && _sidebarHints ? _sidebarHints->second :
                                                           WUX::Automation::AutomationProperties::GetHelpText(_tab));
    }

    void TabStripDisplayItem::SyncIcon(hstring const& iconPath)
    {
        if (_iconPath && *_iconPath == iconPath)
        {
            return;
        }

        if (iconPath.empty())
        {
            MUX::Controls::FontIconSource fallback;
            fallback.FontFamily(WUX::Media::FontFamily{ L"Segoe Fluent Icons" });
            fallback.Glyph(L"\xE756");
            SyncIcon(iconPath, fallback);
        }
        else
        {
            SyncIcon(iconPath, ::Microsoft::Terminal::UI::AgentIcons::SourceForIconPath(iconPath, false));
        }
    }

    void TabStripDisplayItem::UpdatePresentation(bool railCollapsed, bool verticalPresentation)
    {
        const auto isGroup = IsGroup();
        const auto showPaneRows = verticalPresentation && isGroup && !railCollapsed && IsExpanded();
        const auto status = Presentation().TabStatus();
        const auto showBell = status && status.BellIndicator();
        const auto showHeaderProgressRing = status && !showBell && status.IsProgressRingActive() &&
                                            (!showPaneRows || !HeaderProgressProjectedToPaneRows());

        ShowProgressRing(showHeaderProgressRing);
        ShowStatusIcon(showBell || showHeaderProgressRing);

        GroupVisibility(isGroup && !railCollapsed ? Visibility::Visible : Visibility::Collapsed);
        ChildrenVisibility(showPaneRows ? Visibility::Visible : Visibility::Collapsed);
        const auto bitmap = IconSource().try_as<MUX::Controls::BitmapIconSource>();
        IconVisibility(ShowStatusIcon() || (isGroup && !railCollapsed) || (bitmap && !bitmap.UriSource()) ? Visibility::Collapsed : Visibility::Visible);
        PinnedIconVisibility(railCollapsed && IsPinned() ? Visibility::Visible : Visibility::Collapsed);
        HeaderMinHeight(railCollapsed ? 32.0 : 40.0);
        LeadingContentMargin(railCollapsed ? WUX::Thickness{} : WUX::Thickness{ 6, 0, 10, 0 });
        LeadingContentColumnSpan(railCollapsed ? 3 : 1);
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

    static WUX::Media::Brush _paneProgressBrush(const WUX::ResourceDictionary& resources,
                                                const WUX::ElementTheme requestedTheme,
                                                const uint64_t progressState,
                                                const bool highContrastActive)
    {
        std::wstring_view key;
        switch (progressState)
        {
        case 1:
        case 3:
            key = L"PaneProgressAccentBrush";
            break;
        case 2:
            key = L"PaneProgressCriticalBrush";
            break;
        case 4:
            key = L"PaneProgressCautionBrush";
            break;
        default:
            return nullptr;
        }

        const auto keyValue = winrt::box_value(key);
        if (highContrastActive)
        {
            const auto highContrastKey = winrt::box_value(L"HighContrast");
            for (const auto& dictionary : resources.MergedDictionaries())
            {
                if (dictionary.Source())
                {
                    continue;
                }

                const auto themeDictionaries = dictionary.ThemeDictionaries();
                if (!themeDictionaries.HasKey(highContrastKey))
                {
                    continue;
                }

                const auto highContrastDictionary = themeDictionaries.Lookup(highContrastKey).as<WUX::ResourceDictionary>();
                if (highContrastDictionary.HasKey(keyValue))
                {
                    return highContrastDictionary.Lookup(keyValue).try_as<WUX::Media::Brush>();
                }
            }

            return resources.Lookup(keyValue).try_as<WUX::Media::Brush>();
        }

        return ThemeLookup(resources, requestedTheme, keyValue).try_as<WUX::Media::Brush>();
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
        _viewportItems = single_threaded_observable_vector<IInspectable>();

        InitializeComponent();
        _sidebarFiltersChanged = _sidebarFilters.PropertyChanged(auto_revoke, { get_weak(), &TabStrip::_onSidebarFiltersChanged });

        // ItemsControl handles a UIElement item as its own content; it must not
        // also be the data item for the section's DataTemplate.
        _historySectionItem = box_value(RS_(L"VerticalTabsSessionHistoryHeader/Text"));
        const auto selector = winrt::make_self<TabStripItemTemplateSelector>();
        selector->LiveTemplate = ItemsList().ItemTemplate();
        selector->HistoryTemplate = Resources().Lookup(box_value(L"HistoryRowTemplate")).as<DataTemplate>();
        selector->HeaderTemplate = Resources().Lookup(box_value(L"HistoryHeaderTemplate")).as<DataTemplate>();
        ItemsList().ItemTemplate(nullptr);
        ItemsList().ItemTemplateSelector(*selector);
        ItemsList().ItemsSource(_displayItems);
        // ListView consumes Enter even when its focused row is already selected.
        ItemsList().AddHandler(WUX::UIElement::KeyDownEvent(),
                               winrt::box_value(WUX::Input::KeyEventHandler{ get_weak(), &TabStrip::_onListKeyDown }),
                               true);
        ItemsList().PreviewKeyDown({ get_weak(), &TabStrip::_onHistoryPreviewKeyDown });
        ItemsList().AddHandler(
            WUX::UIElement::PointerPressedEvent(),
            winrt::box_value(WUX::Input::PointerEventHandler{ [weak = get_weak()](const IInspectable&, const WUX::Input::PointerRoutedEventArgs& e) {
                if (const auto self = weak.get())
                {
                    const auto list = self->ItemsList();
                    if (!e.GetCurrentPoint(list).Properties().IsLeftButtonPressed())
                    {
                        return;
                    }
                    auto source = e.OriginalSource().try_as<DependencyObject>();
                    for (; source && source != list; source = WUX::Media::VisualTreeHelper::GetParent(source))
                    {
                        if (source.try_as<ListViewItem>() || source.try_as<Primitives::ScrollBar>())
                        {
                            return;
                        }
                    }
                    if (source == list)
                    {
                        // Blank clicks can focus an island-level ScrollViewer outside
                        // our key event route. Keep focus in the Sidebar instead.
                        const auto item = list.SelectedItem();
                        const auto selected = item ? list.ContainerFromItem(item).try_as<ListViewItem>() : nullptr;
                        e.Handled((selected && selected.Focus(WUX::FocusState::Pointer)) ||
                                  self->SearchTabsButton().Focus(WUX::FocusState::Pointer));
                    }
                }
            } }),
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
                self->_updateHistoryAgeTimer();
            }
        });
        ActualThemeChanged([weakThis{ get_weak() }](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_refreshRealizedPaneRowVisuals(self->_highContrast);
                for (uint32_t index = 0; index < self->_displayItems.Size(); ++index)
                {
                    const auto display = self->_displayItems.GetAt(index);
                    if (const auto container = self->ItemsList().ContainerFromItem(display).try_as<FrameworkElement>())
                    {
                        self->_updateDisplayItemVisuals(container, display);
                    }
                }
                for (uint32_t index = 0; index < self->ItemsList().Items().Size(); ++index)
                {
                    if (!self->ItemsList().Items().GetAt(index).try_as<TerminalApp::TabStripDisplayItem>())
                    {
                        if (const auto container = self->ItemsList().ContainerFromIndex(index))
                        {
                            if (const auto presenter = _findNamedElement(container, L"SidebarItemPresenter").try_as<Primitives::ListViewItemPresenter>())
                            {
                                presenter.PointerOverBackground(self->SidebarDefaultHoverBrush().Background());
                            }
                        }
                    }
                }
            }
        });
        Unloaded([weakThis{ get_weak() }](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_searchAnimationEnabled = false;
                self->_setSearchPanelExpanded(false, false);
                if (self->_historyAgeTimer)
                {
                    self->_historyAgeTimer.Stop();
                }
            }
        });
        if constexpr (Feature_RichTabProviders::IsEnabled())
        {
            _richTabGitAvailable = ::Microsoft::Terminal::RichTab::Provider::ProviderBroker::Instance().GitAvailable();
        }
        _applyRailState();
        _updateRichTabMetadataSelectionState();
    }

    void TabStrip::_onSidebarFiltersChanged(IInspectable const&, WUX::Data::PropertyChangedEventArgs const& args)
    {
        const auto queryChanged = args.PropertyName() == L"SearchQuery";
        const auto recentSessionsChanged = args.PropertyName() == L"ShowRecentAgentSessions";
        const auto filterChanged = args.PropertyName() == L"ShowAgentsOnly" || recentSessionsChanged;
        if (!queryChanged && !filterChanged)
        {
            return;
        }
        if (args.PropertyName() == L"ShowAgentsOnly")
        {
            AgentsOnlyIndicator().Visibility(_sidebarFilters.ShowAgentsOnly() ? Visibility::Visible : Visibility::Collapsed);
        }
        if (recentSessionsChanged)
        {
            _agentFilterTelemetryPending = _sidebarFilters.ShowRecentAgentSessions();
        }
        if (queryChanged)
        {
            _syncingSearchState = true;
            SearchTextBox().Text(SearchQuery());
            _syncingSearchState = false;
            _updateSearchVisualState();
        }
        _applyHistoryProjection(true);
        if (queryChanged)
        {
            SearchChanged.raise(*this, nullptr);
        }
        else
        {
            FilterChanged.raise(*this, nullptr);
        }
        _updateHistoryAgeTimer();
        TryCompleteAgentFilterTelemetry();
    }

    IInspectable TabStrip::SelectedItem()
    {
        return _tabFromItem(ItemsList().SelectedItem());
    }
    void TabStrip::SelectedItem(IInspectable const& value)
    {
        if (!value)
        {
            ItemsList().SelectedItem(nullptr);
            return;
        }

        uint32_t index{};
        if (_tabItems.IndexOf(value, index))
        {
            ItemsList().SelectedItem(_displayItems.GetAt(index));
        }
    }
    int32_t TabStrip::SelectedIndex()
    {
        const auto index = ItemsList().SelectedIndex();
        return index < static_cast<int32_t>(_tabItems.Size()) ? index : -1;
    }
    void TabStrip::SelectedIndex(int32_t value)
    {
        ItemsList().SelectedIndex(value < static_cast<int32_t>(_tabItems.Size()) ? value : -1);
    }
    DependencyObject TabStrip::ContainerFromIndex(int32_t index)
    {
        return index >= 0 && index < static_cast<int32_t>(_tabItems.Size()) ? ItemsList().ContainerFromIndex(index) : nullptr;
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
                                      hstring const& iconPath,
                                      const bool useNativeIconSource)
    {
        display.Title(title);
        display.Presentation().Title(title);
        if (useNativeIconSource)
        {
            // A real Tab's retained identity can intentionally be null. Never
            // promote its empty native progress-slot source into Sidebar identity.
            const auto retainedSource = display.Presentation().IconSource().try_as<MUX::Controls::IconSource>();
            auto source = retainedSource ? retainedSource : display.Tab().IconSource();
            if (!retainedSource)
            {
                if (const auto placeholder = source.try_as<MUX::Controls::FontIconSource>();
                    placeholder && placeholder.Glyph().empty())
                {
                    source = nullptr;
                }
            }
            if (source)
            {
                winrt::get_self<TabStripDisplayItem>(display)->SyncIcon(iconPath, source);
            }
            else if (const auto empty = display.IconSource().try_as<MUX::Controls::BitmapIconSource>();
                     !empty || empty.UriSource())
            {
                display.IconSource(MUX::Controls::BitmapIconSource{});
            }
        }
        else
        {
            winrt::get_self<TabStripDisplayItem>(display)->SyncIcon(iconPath);
        }
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
        _refreshDisplayItemVisuals(display);
    }

    static void _updatePaneSelection(TerminalApp::TabStripDisplayItem const& display, const bool isSelected, const bool railCollapsed)
    {
        for (const auto& pane : display.PaneItems())
        {
            pane.ActiveIndicatorVisibility(isSelected && pane.IsActive() ? Visibility::Visible : Visibility::Collapsed);
            winrt::get_self<TabStripPaneItem>(pane)->UpdateCloseVisibility(railCollapsed);
        }
    }

    void TabStrip::SetPaneItems(IInspectable const& item,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup)
    {
        SetPaneItems(item, panes, isGroup, false);
    }

    void TabStrip::SetPaneItems(IInspectable const& item,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup,
                                bool headerProgressProjectedToPaneRows)
    {
        if (const auto display = DisplayItemForTab(item))
        {
            SetPaneItems(display, panes, isGroup, headerProgressProjectedToPaneRows);
        }
    }

    void TabStrip::SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup)
    {
        SetPaneItems(display, panes, isGroup, false);
    }

    void TabStrip::SetPaneItems(TerminalApp::TabStripDisplayItem const& display,
                                IVector<TerminalApp::TabStripPaneItem> const& panes,
                                bool isGroup,
                                bool headerProgressProjectedToPaneRows)
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
                existing.MetadataText(pane.MetadataText());
                existing.MetadataHighlightColor(pane.MetadataHighlightColor());
                existing.MetadataHighlightRanges(pane.MetadataHighlightRanges());
                existing.MetadataVisibility(pane.MetadataVisibility());
                existing.AutomationName(pane.AutomationName());
                existing.ProgressState(pane.ProgressState());
                existing.IsProgressRingActive(pane.IsProgressRingActive());
                existing.IsProgressRingIndeterminate(pane.IsProgressRingIndeterminate());
                existing.ProgressValue(pane.ProgressValue());
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
        _updatePaneSelection(display, SelectedItem() == display.Tab(), _isRailCollapsed);
        display.IsGroup(isGroup);
        winrt::get_self<TabStripDisplayItem>(display)->HeaderProgressProjectedToPaneRows(headerProgressProjectedToPaneRows);
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
        _refreshDisplayItemVisuals(display);
    }

    IInspectable TabStrip::HeaderForTab(IInspectable const& item, const bool realize)
    {
        if (const auto tab = item.try_as<MUX::Controls::TabViewItem>())
        {
            if (const auto display = _displayItemForTab(tab))
            {
                if (const auto desired = _tabItemVisibility.find(winrt::get_abi(tab));
                    desired != _tabItemVisibility.end() && desired->second.Item.get() == tab && !desired->second.Visible)
                {
                    return nullptr;
                }
                if (realize)
                {
                    if (_isRailCollapsed)
                    {
                        RailCollapseRequested.raise(*this, nullptr);
                    }
                    ItemsList().ScrollIntoView(display);
                    ItemsList().UpdateLayout();
                }
                if (_isRailCollapsed || ItemsList().Visibility() != Visibility::Visible ||
                    Visibility() != WUX::Visibility::Visible)
                {
                    return nullptr;
                }
                if (const auto container = ItemsList().ContainerFromItem(display).try_as<ListViewItem>())
                {
                    if (const auto root = container.ContentTemplateRoot().try_as<FrameworkElement>();
                        root && container.Visibility() == WUX::Visibility::Visible)
                    {
                        const auto header = _findNamedElement(root, L"TabHeaderPresenter").try_as<TerminalApp::TabHeaderControl>();
                        for (auto current = header.try_as<DependencyObject>(); current; current = WUX::Media::VisualTreeHelper::GetParent(current))
                        {
                            if (const auto element = current.try_as<UIElement>();
                                element && element.Visibility() != WUX::Visibility::Visible)
                            {
                                return nullptr;
                            }
                        }
                        return header;
                    }
                }
            }
        }
        return nullptr;
    }

    void TabStrip::BringTabHeaderIntoView(IInspectable const& item)
    {
        if (_isRailCollapsed || !_isVerticalPresentation)
        {
            return;
        }
        auto header = HeaderForTab(item).try_as<FrameworkElement>();
        if (!header)
        {
            header = HeaderForTab(item, true).try_as<FrameworkElement>();
        }
        if (header)
        {
            // The parent grid is the full header, without the group's pane rows.
            if (const auto row = header.Parent().try_as<FrameworkElement>())
            {
                const auto list = ItemsList();
                const auto bounds = row.TransformToVisual(list).TransformBounds(
                    Rect{ 0, 0, static_cast<float>(row.ActualWidth()), static_cast<float>(row.ActualHeight()) });
                if (bounds.Y < -0.5f || bounds.Y + bounds.Height > list.ActualHeight() + 0.5)
                {
                    row.StartBringIntoView();
                }
            }
        }
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
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
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
                display.Presentation().SearchText(searchText);
                for (const auto& pane : display.PaneItems())
                {
                    pane.HighlightQuery(searchText);
                }
            }
        }
    }

    static WUX::Media::SolidColorBrush _tabSelectionBrush(MUX::Controls::TabViewItem const& tab, const bool selected)
    {
        if (!tab)
        {
            return nullptr;
        }
        if (selected)
        {
            const auto themes = tab.Resources().ThemeDictionaries();
            const auto themeKey = box_value(tab.ActualTheme() == ElementTheme::Light ? L"Light" : L"Dark");
            if (themes.HasKey(themeKey))
            {
                const auto resources = themes.Lookup(themeKey).try_as<ResourceDictionary>();
                const auto colorKey = box_value(L"TabViewItemHeaderBackgroundSelected");
                if (resources && resources.HasKey(colorKey))
                {
                    if (const auto brush = resources.Lookup(colorKey).try_as<WUX::Media::SolidColorBrush>())
                    {
                        return brush;
                    }
                }
            }
        }
        if (const auto brush = tab.Background().try_as<WUX::Media::SolidColorBrush>();
            brush && brush.Color().A != 0 && brush.Opacity() > 0)
        {
            return WUX::Media::SolidColorBrush{ brush.Color() };
        }
        return nullptr;
    }

    static WUX::Media::SolidColorBrush _tabHoverBrush(MUX::Controls::TabViewItem const& tab)
    {
        if (tab)
        {
            const auto themes = tab.Resources().ThemeDictionaries();
            const auto themeKey = box_value(tab.ActualTheme() == ElementTheme::Light ? L"Light" : L"Dark");
            if (themes.HasKey(themeKey))
            {
                const auto resources = themes.Lookup(themeKey).try_as<ResourceDictionary>();
                const auto colorKey = box_value(L"TabViewItemHeaderBackgroundPointerOver");
                if (resources && resources.HasKey(colorKey))
                {
                    const auto brush = resources.Lookup(colorKey).try_as<WUX::Media::SolidColorBrush>();
                    if (brush && brush.Color().A != 0 && brush.Opacity() > 0)
                    {
                        return brush;
                    }
                }
            }
        }
        return nullptr;
    }

    static Windows::UI::Color _compositeBrushColor(WUX::Media::SolidColorBrush const& brush, const Windows::UI::Color backdrop)
    {
        const auto alpha = static_cast<uint8_t>(std::lround(brush.Color().A * brush.Opacity()));
        return til::color{ brush.Color() }.with_alpha(alpha).layer_over(til::color{ backdrop });
    }

    static Windows::UI::Color _sidebarBackdropColor(WUX::Media::Brush const& background, const ElementTheme theme)
    {
        const auto fallback = theme == ElementTheme::Light ? Windows::UI::Colors::White() : Windows::UI::Colors::Black();
        if (const auto solid = background.try_as<WUX::Media::SolidColorBrush>())
        {
            return _compositeBrushColor(solid, fallback);
        }
        if (const auto acrylic = background.try_as<WUX::Media::AcrylicBrush>())
        {
            // Acrylic has no single rendered color; its fallback gives a stable contrast baseline.
            const auto color = acrylic.FallbackColor();
            const auto alpha = static_cast<uint8_t>(std::lround(color.A * acrylic.Opacity()));
            return til::color{ color }.with_alpha(alpha).layer_over(til::color{ fallback });
        }
        return fallback;
    }

    static Windows::UI::Color _tabSelectionForeground(WUX::Media::SolidColorBrush const& brush, const Windows::UI::Color backdrop)
    {
        const auto rendered = til::color{ _compositeBrushColor(brush, backdrop) };
        const auto linear = [](const uint8_t value) {
            const auto channel = value / 255.0;
            return channel <= 0.04045 ? channel / 12.92 : std::pow((channel + 0.055) / 1.055, 2.4);
        };
        const auto luminance = 0.2126 * linear(rendered.r) + 0.7152 * linear(rendered.g) + 0.0722 * linear(rendered.b);
        return (luminance + 0.05) / 0.05 >= 1.05 / (luminance + 0.05) ?
                   Windows::UI::Colors::Black() :
                   Windows::UI::Colors::White();
    }

    static void _bindMonochromeIconForeground(ContentPresenter const& presenter, Control const& foregroundOwner)
    {
        const auto icon = presenter ? presenter.Content().try_as<IconElement>() : nullptr;
        const auto bitmap = icon ? icon.try_as<BitmapIcon>() : nullptr;
        if (icon && foregroundOwner && !icon.try_as<MUX::Controls::ImageIcon>() &&
            (!bitmap || bitmap.ShowAsMonochrome()))
        {
            WUX::Data::Binding binding;
            binding.Source(foregroundOwner);
            binding.Path(WUX::PropertyPath{ L"Foreground" });
            binding.Mode(WUX::Data::BindingMode::OneWay);
            icon.SetBinding(IconElement::ForegroundProperty(), binding);
        }
    }

    static void _applyHistoryRowForeground(FrameworkElement const& root, TerminalApp::TabStripHistoryItem const& item)
    {
        if (!root)
        {
            return;
        }
        const auto isCurrent = item && item.IsCurrent();
        const auto external = item && item.Status() == L"InUse";
        const auto externalTip = external ? RS_(L"VerticalTabsHistoryInUseToolTip") : winrt::hstring{};
        if (const auto text = root.FindName(L"HistoryInUseTooltipText").try_as<TextBlock>())
        {
            text.Text(externalTip);
            text.Visibility(external ? Visibility::Visible : Visibility::Collapsed);
        }
        const auto foreground = isCurrent ? item.CurrentForeground() : nullptr;
        const auto palette = root.FindName(L"HistorySelectionPalette").try_as<Control>();
        for (const auto name : { L"HistoryTitleText", L"HistoryCwdText", L"HistorySubtitleText", L"HistoryStatusText", L"HistoryWindowHintText" })
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
        if (item)
        {
            const auto provider = item.ProviderDisplayName();
            WUX::Automation::AutomationProperties::SetName(
                container, provider.empty() ? item.Title() : item.Title() + L" \u00b7 " + provider);
            WUX::Automation::AutomationProperties::SetHelpText(
                container, item.Status() == L"InUse" ? RS_(L"VerticalTabsHistoryInUseToolTip") : winrt::hstring{});
        }
        else
        {
            container.ClearValue(WUX::Automation::AutomationProperties::NameProperty());
            container.ClearValue(WUX::Automation::AutomationProperties::HelpTextProperty());
        }
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
                if (const auto hoverBackground = grid.FindName(L"TabHoverBackground").try_as<WUX::Controls::Border>())
                {
                    hoverBackground.Opacity(0.0);
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
            const auto isGroup = display.IsGroup();
            const auto hoverBrush = !isGroup && !_highContrast ? _tabHoverBrush(display.Tab()) : nullptr;
            const auto hovered = hoverBrush && winrt::get_self<TabStripDisplayItem>(display)->IsPointerOver();
            bool selectedPaneVisible = false;
            if (isGroup && display.ChildrenVisibility() == Visibility::Visible)
            {
                for (const auto& pane : display.PaneItems())
                {
                    if (pane.ActiveIndicatorVisibility() == Visibility::Visible)
                    {
                        selectedPaneVisible = true;
                        break;
                    }
                }
            }
            const auto groupIndicatorVisible = isGroup && display.ChildrenVisibility() == Visibility::Visible;
            const auto selected = display.SelectionVisibility() == Visibility::Visible && !selectedPaneVisible;
            const auto pointerOver = winrt::get_self<TabStripDisplayItem>(display)->IsPointerOver();
            if (const auto hoverBackground = grid.FindName(L"TabHoverBackground").try_as<WUX::Controls::Border>())
            {
                hoverBackground.Opacity(!_highContrast && !selected && pointerOver && !hoverBrush ? 1.0 : 0.0);
            }
            if (const auto colorHover = grid.FindName(L"TabColorHoverBackground").try_as<WUX::Controls::Border>())
            {
                colorHover.Background(hoverBrush);
                colorHover.Opacity(!_highContrast && !selected && pointerOver && hoverBrush ? 1.0 : 0.0);
            }
            if (const auto container = ItemsList().ContainerFromItem(display);
                container)
            {
                if (const auto presenter = _findNamedElement(container, L"SidebarItemPresenter").try_as<Primitives::ListViewItemPresenter>())
                {
                    if (Windows::Foundation::Metadata::ApiInformation::IsPropertyPresent(
                            L"Windows.UI.Xaml.Controls.Primitives.ListViewItemPresenter", L"SelectionIndicatorVisualEnabled"))
                    {
                        presenter.SelectionIndicatorVisualEnabled(!groupIndicatorVisible);
                    }
                    const auto marker = box_value(L"SidebarThemedHover");
                    if (!_highContrast)
                    {
                        presenter.PointerOverBackground(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
                        if (!presenter.Resources().HasKey(marker))
                        {
                            presenter.Resources().Insert(marker, box_value(true));
                        }
                    }
                    else
                    {
                        presenter.PointerOverBackground(grid.FindName(L"TabHoverBackground").as<WUX::Controls::Border>().Background());
                        if (presenter.Resources().HasKey(marker))
                        {
                            presenter.Resources().Remove(marker);
                        }
                    }
                }
            }
            const auto tabColor = isGroup && !selected ? nullptr : _tabSelectionBrush(display.Tab(), selected);
            if (const auto nativeSelection = grid.FindName(L"TabSelectionBackground").try_as<WUX::Controls::Border>())
            {
                nativeSelection.Visibility(selected && !tabColor ? Visibility::Visible : Visibility::Collapsed);
            }
            if (selectionBackground)
            {
                const auto showColor = tabColor && (selected || !_highContrast) && !(!selected && hovered);
                WUX::Media::SolidColorBrush brush{ showColor ? tabColor.Color() : Windows::UI::Colors::Transparent() };
                if (showColor && !selected)
                {
                    brush.Opacity(display.Tab().Background().as<WUX::Media::SolidColorBrush>().Opacity());
                }
                else if (showColor)
                {
                    brush.Opacity(tabColor.Opacity());
                }
                selectionBackground.Background(brush);
            }
            grid.Background(WUX::Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
            const auto header = grid.FindName(L"TabHeaderPresenter").try_as<WUX::Controls::Control>();
            const auto close = grid.FindName(L"TabCloseButton").try_as<WUX::Controls::Control>();
            const auto iconPresenter = grid.FindName(L"TabIconPresenter").try_as<ContentPresenter>();
            const auto inactiveBrush = !selected && !_highContrast && selectionBackground ?
                                           selectionBackground.Background().try_as<WUX::Media::SolidColorBrush>() :
                                           nullptr;
            const auto foregroundBrush = selected ? tabColor :
                                            hovered ? hoverBrush :
                                            inactiveBrush && inactiveBrush.Color().A > 0 && inactiveBrush.Opacity() > 0 ? inactiveBrush : nullptr;
            if (foregroundBrush)
            {
                auto backdrop = _sidebarBackdropColor(Background(), grid.ActualTheme());
                if (foregroundBrush == inactiveBrush && pointerOver && !hoverBrush)
                {
                    if (const auto defaultHover = grid.FindName(L"TabHoverBackground").as<WUX::Controls::Border>().Background().try_as<WUX::Media::SolidColorBrush>())
                    {
                        backdrop = _compositeBrushColor(defaultHover, backdrop);
                    }
                }
                const auto foreground = WUX::Media::SolidColorBrush{ _tabSelectionForeground(foregroundBrush, backdrop) };
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
            _bindMonochromeIconForeground(iconPresenter, header);
        }
    }

    void TabStrip::_updatePaneRowVisuals(FrameworkElement const& root,
                                         TerminalApp::TabStripPaneItem const& pane)
    {
        _updatePaneRowVisuals(root, pane, _highContrast);
    }

    void TabStrip::_updatePaneRowVisuals(FrameworkElement const& root,
                                         TerminalApp::TabStripPaneItem const& pane,
                                         const bool highContrastActive)
    {
        if (!root || !pane)
        {
            return;
        }

        const auto button = root.FindName(L"PaneActivateButton").try_as<WUX::Controls::Control>();
        const auto active = pane.ActiveIndicatorVisibility() == Visibility::Visible;
        const auto color = active ? _tabSelectionBrush(pane.Tab(), true) : nullptr;
        const auto hoverBrush = !active && !highContrastActive ? _tabHoverBrush(pane.Tab()) : nullptr;
        const auto hovered = hoverBrush && winrt::get_self<TabStripPaneItem>(pane)->IsPointerOver();
        if (const auto background = root.FindName(L"PaneColorSelectionBackground").try_as<WUX::Controls::Border>())
        {
            WUX::Media::SolidColorBrush brush{ color ? color.Color() : Windows::UI::Colors::Transparent() };
            if (color)
            {
                brush.Opacity(color.Opacity());
            }
            background.Background(brush);
        }
        if (const auto nativeSelection = root.FindName(L"PaneActiveBackground").try_as<WUX::Controls::Border>())
        {
            nativeSelection.Visibility(active && !color ? Visibility::Visible : Visibility::Collapsed);
        }
        const auto foregroundBrush = color ? color : hovered ? hoverBrush : nullptr;
        const auto foreground = foregroundBrush ?
                                    WUX::Media::SolidColorBrush{ _tabSelectionForeground(foregroundBrush, _sidebarBackdropColor(Background(), root.ActualTheme())) } :
                                    nullptr;
        for (const auto name : { L"PaneActivateButton", L"PaneCloseButton", L"PaneTitleText", L"PaneMetadataText" })
        {
            if (const auto control = root.FindName(name).try_as<WUX::Controls::Control>())
            {
                if (foreground)
                {
                    control.Foreground(foreground);
                }
                else
                {
                    control.ClearValue(WUX::Controls::Control::ForegroundProperty());
                }
            }
        }
        const auto presenter = root.FindName(L"PaneIconPresenter").try_as<ContentPresenter>();
        _bindMonochromeIconForeground(presenter, button);
        if (const auto hover = root.FindName(L"PaneHoverBackground").try_as<UIElement>())
        {
            const auto showHover = !active && winrt::get_self<TabStripPaneItem>(pane)->IsPointerOver();
            hover.Opacity(showHover && !hoverBrush ? 1.0 : 0.0);
            if (const auto colorHover = root.FindName(L"PaneColorHoverBackground").try_as<WUX::Controls::Border>())
            {
                colorHover.Background(hoverBrush);
                colorHover.Opacity(showHover && hoverBrush ? 1.0 : 0.0);
            }
        }

        const auto updateRing = [&](WUX::Controls::Control const& ring) {
            if (!ring)
            {
                return;
            }

            if (const auto brush = _paneProgressBrush(Resources(), root.ActualTheme(), pane.ProgressState(), highContrastActive))
            {
                ring.Foreground(brush);
            }
            else
            {
                ring.ClearValue(WUX::Controls::Control::ForegroundProperty());
            }

            WUX::Automation::AutomationProperties::SetName(ring, pane.AutomationName());
        };
        updateRing(_findNamedElement(root, L"PaneProgressRing").try_as<WUX::Controls::Control>());
        updateRing(_findNamedElement(root, L"PaneIndeterminateProgressRing").try_as<WUX::Controls::Control>());
    }

    void TabStrip::_refreshRealizedPaneRowVisuals(const bool highContrastActive)
    {
        for (uint32_t index = 0; index < _displayItems.Size(); ++index)
        {
            _refreshPaneRowVisuals(_displayItems.GetAt(index), highContrastActive);
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
                _refreshPaneRowVisuals(display);
            }
        }
    }

    void TabStrip::_refreshPaneRowVisuals(TerminalApp::TabStripDisplayItem const& display)
    {
        _refreshPaneRowVisuals(display, _highContrast);
    }

    void TabStrip::_refreshPaneRowVisuals(TerminalApp::TabStripDisplayItem const& display,
                                          const bool highContrastActive)
    {
        if (!display)
        {
            return;
        }

        uint32_t displayIndex{};
        if (!_displayItems.IndexOf(display, displayIndex))
        {
            return;
        }

        const auto container = ItemsList().ContainerFromIndex(displayIndex).try_as<ListViewItem>();
        const auto root = container ? container.ContentTemplateRoot().try_as<FrameworkElement>() : nullptr;
        const auto paneList = root ? _findNamedElement(root, L"TabPaneItems").try_as<ItemsControl>() : nullptr;
        if (!paneList)
        {
            return;
        }

        const auto panes = display.PaneItems();
        for (uint32_t index = 0; index < panes.Size(); ++index)
        {
            const auto paneContainer = paneList.ContainerFromIndex(index).try_as<ContentPresenter>();
            if (!paneContainer || WUX::Media::VisualTreeHelper::GetChildrenCount(paneContainer) == 0)
            {
                continue;
            }

            if (const auto paneRoot = WUX::Media::VisualTreeHelper::GetChild(paneContainer, 0).try_as<FrameworkElement>())
            {
                _updatePaneRowVisuals(paneRoot, panes.GetAt(index), highContrastActive);
            }
        }
    }

    void TabStrip::OnTabHeaderLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            _updateSidebarRowToolTipPlacement(root);
            if (const auto display = root.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
            {
                winrt::get_self<TabStripDisplayItem>(display)->SetPointerOver(false, _isRailCollapsed);
                _updateDisplayItemVisuals(root, display);
                if (const auto close = root.FindName(L"TabCloseButton").try_as<FrameworkElement>())
                {
                    WUX::Automation::AutomationProperties::SetName(close, RS_(L"TabClose"));
                    ToolTipService::SetToolTip(close, box_value(RS_(L"TabCloseToolTip")));
                }
            }
        }
    }

    void TabStrip::OnTabHeaderUnloaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto display = root.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
            {
                winrt::get_self<TabStripDisplayItem>(display)->SetPointerOver(false, _isRailCollapsed);
            }
        }
    }

    void TabStrip::OnSidebarRowSizeChanged(IInspectable const& sender, SizeChangedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            _updateSidebarRowToolTipPlacement(root);
        }
    }

    void TabStrip::OnSidebarRowPointerEntered(IInspectable const& sender, Input::PointerRoutedEventArgs const&)
    {
        const auto row = sender.as<FrameworkElement>();
        _updateSidebarRowToolTipPlacement(row);
        if (const auto display = row.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
        {
            winrt::get_self<TabStripDisplayItem>(display)->SetPointerOver(true, _isRailCollapsed);
            _updateDisplayItemVisuals(row, display);
        }
    }

    void TabStrip::OnSidebarRowPointerExited(IInspectable const& sender, Input::PointerRoutedEventArgs const& e)
    {
        const auto row = sender.as<FrameworkElement>();
        const auto position = e.GetCurrentPoint(row).Position();
        if (position.X >= 0 && position.X < row.ActualWidth() &&
            position.Y >= 0 && position.Y < row.ActualHeight())
        {
            return;
        }
        if (const auto display = row.DataContext().try_as<TerminalApp::TabStripDisplayItem>())
        {
            winrt::get_self<TabStripDisplayItem>(display)->SetPointerOver(false, _isRailCollapsed);
            _updateDisplayItemVisuals(row, display);
        }
    }

    void TabStrip::OnSidebarRowGotFocus(IInspectable const& sender, RoutedEventArgs const&)
    {
        _updateSidebarRowToolTipPlacement(sender.as<FrameworkElement>());
    }

    void TabStrip::OnSidebarListFocusChanged(IInspectable const&, RoutedEventArgs const&)
    {
        TerminalApp::TabStripDisplayItem focused{ nullptr };
        if (const auto root = XamlRoot())
        {
            auto current = WUX::Input::FocusManager::GetFocusedElement(root).try_as<DependencyObject>();
            for (; current; current = WUX::Media::VisualTreeHelper::GetParent(current))
            {
                if (const auto container = current.try_as<ListViewItem>())
                {
                    const auto display = container.Content().try_as<TerminalApp::TabStripDisplayItem>();
                    if (display && ItemsList().ContainerFromItem(display) == container)
                    {
                        focused = display;
                    }
                    break;
                }
            }
        }
        if (const auto previous = _focusedCloseDisplay.get(); previous != focused)
        {
            if (previous)
            {
                winrt::get_self<TabStripDisplayItem>(previous)->SetKeyboardFocused(false, _isRailCollapsed);
            }
            _focusedCloseDisplay = focused ? winrt::make_weak(focused) : winrt::weak_ref<TerminalApp::TabStripDisplayItem>{};
            if (focused)
            {
                winrt::get_self<TabStripDisplayItem>(focused)->SetKeyboardFocused(true, _isRailCollapsed);
            }
        }
    }

    void TabStrip::_updateSidebarRowToolTipPlacement(FrameworkElement const& row)
    {
        const auto xamlRoot = row.XamlRoot();
        if (!xamlRoot)
        {
            return;
        }
        const auto target = xamlRoot.Content();
        if (const auto tooltip = ToolTipService::GetToolTip(row).try_as<ToolTip>())
        {
            tooltip.Placement(row.FlowDirection() == FlowDirection::RightToLeft ? Primitives::PlacementMode::Left : Primitives::PlacementMode::Right);
            ToolTipService::SetPlacementTarget(row, target);
            tooltip.PlacementRect(row.TransformToVisual(target).TransformBounds(
                Rect{ 0, 0, static_cast<float>(row.ActualWidth()), static_cast<float>(row.ActualHeight()) }));
        }
    }

    void TabStrip::OnRowHeaderLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        const auto header = sender.as<TerminalApp::TabHeaderControl>();
        const auto impl = winrt::get_self<TabHeaderControl>(header);
        if (std::exchange(impl->SidebarEventsAttached, true))
        {
            return;
        }
        header.TitleChangeRequested([weakThis = get_weak(), weakHeader = winrt::make_weak(header)](const auto& title) {
            const auto self = weakThis.get();
            const auto view = weakHeader.get();
            const auto display = view ? view.DataContext().try_as<TerminalApp::TabStripDisplayItem>() : nullptr;
            if (self && display)
            {
                self->HeaderTitleChangeRequested.raise(display.Tab(), title);
            }
        });
        header.RenameEnded({ get_weak(), &TabStrip::OnHeaderRenameEnded });
    }

    void TabStrip::OnHeaderRenameEnded(IInspectable const& sender, IInspectable const&)
    {
        const auto view = sender.try_as<FrameworkElement>();
        if (const auto display = view ? view.DataContext().try_as<TerminalApp::TabStripDisplayItem>() : nullptr)
        {
            TabFocusRequested.raise(*this, winrt::make<TabStripCloseRequestedEventArgs>(display.Tab()));
        }
    }

    void TabStrip::OnPaneRowLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto pane = root.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                winrt::get_self<TabStripPaneItem>(pane)->SetPointerOver(false, _isRailCollapsed);
                if (const auto title = _findNamedElement(root, L"PaneTitleText").try_as<TerminalApp::HighlightedTextControl>())
                {
                    title.Text(pane.Title());
                }
                if (const auto close = _findNamedElement(root, L"PaneCloseButton"))
                {
                    const auto label = RS_(L"PaneClose");
                    WUX::Automation::AutomationProperties::SetName(close, label);
                    ToolTipService::SetToolTip(close, box_value(label));
                }

                _updatePaneRowVisuals(root, pane);
            }
        }
    }

    void TabStrip::OnPaneRowUnloaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (const auto pane = root.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                const auto impl = winrt::get_self<TabStripPaneItem>(pane);
                impl->SetPointerOver(false, _isRailCollapsed);
                impl->SetKeyboardFocused(false, _isRailCollapsed);
            }
        }
    }

    void TabStrip::OnPaneRowFocusChanged(IInspectable const& sender, RoutedEventArgs const&)
    {
        const auto row = sender.as<FrameworkElement>();
        if (const auto pane = row.DataContext().try_as<TerminalApp::TabStripPaneItem>())
        {
            bool focused = false;
            if (const auto root = row.XamlRoot())
            {
                for (auto current = WUX::Input::FocusManager::GetFocusedElement(root).try_as<DependencyObject>();
                     current;
                     current = WUX::Media::VisualTreeHelper::GetParent(current))
                {
                    if (current == row)
                    {
                        focused = true;
                        break;
                    }
                }
            }
            winrt::get_self<TabStripPaneItem>(pane)->SetKeyboardFocused(focused, _isRailCollapsed);
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
            const auto historyVisible = _sidebarFilters.RecentAgentSessionsVisible() && !_isRailCollapsed;
            ItemsList().Visibility(value || historyVisible ? Visibility::Visible : Visibility::Collapsed);
        }
    }
    void TabStrip::IsRailCollapsed(bool value)
    {
        if (_isRailCollapsed != value)
        {
            _isRailCollapsed = value;
            _applyRailState();
            _updateHistoryAgeTimer();
        }
    }

    void TabStrip::PrepareTabItem(MUX::Controls::TabViewItem const& item)
    {
        if (const auto display = _displayItemForTab(item))
        {
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
        }
    }

    void TabStrip::RefreshTabColor(MUX::Controls::TabViewItem const& item)
    {
        _refreshDisplayItemVisuals(_displayItemForTab(item));
    }

    void TabStrip::FilterMode(TerminalApp::TabStripFilterMode value)
    {
        _sidebarFilters.ShowAgentsOnly(value == TerminalApp::TabStripFilterMode::AgentsOnly);
    }

    void TabStrip::SearchActive(bool value)
    {
        if (_searchActive != value)
        {
            _searchActive = value;
            _updateSearchVisualState();
            if (!value && !SearchQuery().empty())
            {
                SearchQuery(L"");
            }
            else
            {
                SearchChanged.raise(*this, nullptr);
            }
        }
    }

    void TabStrip::SearchQuery(winrt::hstring const& value)
    {
        _sidebarFilters.SearchQuery(value);
    }

    bool TabStrip::FocusTabSearch()
    {
        if (_isRailCollapsed || !SearchTabsButton().IsEnabled())
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
        const auto color = TabHeaderControl::AgentStatusHighlightColor(winrt::to_string(status));
        const auto styleKey = color == TerminalApp::TextHighlightColor::Success  ? L"HistoryActiveTextStyle" :
                              color == TerminalApp::TextHighlightColor::Caution  ? L"HistoryAttentionTextStyle" :
                              color == TerminalApp::TextHighlightColor::Critical ? L"HistoryErrorTextStyle" :
                                                                                   L"HistorySubtitleTextStyle";
        return Resources().Lookup(box_value(styleKey)).as<WUX::Style>();
    }

    bool TabStrip::_updateHistoryCompletionPresentation(const TerminalApp::TabStripHistoryItem& item)
    {
        const auto nativeItem = winrt::get_self<TabStripHistoryItem>(item);
        const auto changed = nativeItem->SetUnreadCompletion(_agentCompletionResolver && _agentCompletionResolver(item));
        const auto style = _historyStatusTextStyle(nativeItem->StatusForPresentation());
        const auto styleChanged = item.StatusTextStyle() != style;
        item.StatusTextStyle(style);
        return changed || styleChanged;
    }

    void TabStrip::SetAgentCompletionResolver(std::function<bool(const TerminalApp::TabStripHistoryItem&)> resolver)
    {
        _agentCompletionResolver = std::move(resolver);
        RefreshHistoryAgentCompletions();
    }

    void TabStrip::RefreshHistoryAgentCompletions()
    {
        bool changed = false;
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            if (_updateHistoryCompletionPresentation(_historySnapshot[index]))
            {
                _historySearchTerms[index] = _buildHistorySearchTerms(_historySnapshot[index]);
                changed = true;
            }
        }
        for (const auto& item : _historyItems)
        {
            changed = _updateHistoryCompletionPresentation(item) || changed;
            if (changed)
            {
                if (const auto container = ItemsList().ContainerFromItem(item).try_as<ListViewItem>())
                {
                    _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), item);
                    _applyHistoryRowAutomation(container, item);
                }
            }
        }
        if (changed)
        {
            _applyHistoryProjection(true);
        }
    }

    void TabStrip::CommitHistorySnapshot(std::vector<TerminalApp::TabStripHistoryItem> items, const bool ready)
    {
        _historySnapshotReady = ready;
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
            if (item.StatusLabelText().empty())
            {
                winrt::get_self<TabStripHistoryItem>(item)->UpdateStatusText(item.StatusText());
            }
            const auto wasCurrent = item.IsCurrent();
            item.IsCurrent(false);
            item.CurrentBackground(nullptr);
            item.CurrentForeground(nullptr);
            _updateHistoryCompletionPresentation(item);
            if (wasCurrent)
            {
                if (const auto container = ItemsList().ContainerFromItem(item).try_as<ListViewItem>())
                {
                    _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), item);
                    _applyHistoryRowAutomation(container, item);
                }
            }
            auto iconKey = box_value(L"AgentIcon." + item.AgentId());
            if (!Resources().HasKey(iconKey))
            {
                iconKey = box_value(L"AgentIcon.generic");
            }
            item.IconTemplate(Resources().Lookup(iconKey).as<DataTemplate>());
            _historySearchTerms.emplace_back(_buildHistorySearchTerms(item));
        }
        _applyHistoryProjection(true);
        TryCompleteAgentFilterTelemetry();
    }

    void TabStrip::TryCompleteAgentFilterTelemetry()
    {
        if (_historySnapshotReady && !_historyLoadPending &&
            _historyError.empty() && _historyRefreshError.empty() &&
            _sidebarFilters.RecentAgentSessionsVisible() && _agentFilterTelemetryPending)
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

    void TabStrip::_updateHistoryAgeTimer()
    {
        if (!_sidebarFilters.RecentAgentSessionsVisible() || !_isVerticalPresentation || _isRailCollapsed || !IsLoaded())
        {
            if (_historyAgeTimer)
            {
                _historyAgeTimer.Stop();
            }
            return;
        }
        if (!_historyAgeTimer)
        {
            _historyAgeTimer = DispatcherTimer{};
            _historyAgeTimer.Interval(std::chrono::seconds{ 1 });
            _historyAgeTimer.Tick([weakThis{ get_weak() }](auto&&, auto&&) {
                if (const auto self = weakThis.get())
                {
                    const auto nowMs = static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                                                                 std::chrono::system_clock::now().time_since_epoch())
                                                                 .count());
                    self->_refreshHistoryAges(nowMs);
                }
            });
        }
        const auto nowMs = static_cast<uint64_t>(std::chrono::duration_cast<std::chrono::milliseconds>(
                                                     std::chrono::system_clock::now().time_since_epoch())
                                                     .count());
        _refreshHistoryAges(nowMs);
        _historyAgeTimer.Start();
    }

    void TabStrip::_refreshHistoryAges(const uint64_t nowMs)
    {
        if (!_sidebarFilters.RecentAgentSessionsVisible() || !_isVerticalPresentation || _isRailCollapsed)
        {
            return;
        }
        bool changed = false;
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            const auto& item = _historySnapshot[index];
            if (winrt::get_self<TabStripHistoryItem>(item)->RefreshAge(nowMs))
            {
                _historySearchTerms[index] = _buildHistorySearchTerms(item);
                changed = true;
            }
        }
        // Equal snapshots can retain distinct visible objects.
        for (const auto& item : _historyItems)
        {
            winrt::get_self<TabStripHistoryItem>(item)->RefreshAge(nowMs);
        }
        if (changed && _sidebarFilters.HasSearchQuery())
        {
            _applyHistoryProjection(true);
        }
    }

    void TabStrip::SetRepresentedHistorySessions(std::vector<RepresentedHistorySession> sessions)
    {
        if (_representedHistorySessions != sessions)
        {
            _representedHistorySessions = std::move(sessions);
            if (_sidebarFilters.RecentAgentSessionsVisible())
            {
                _applyHistoryProjection(true);
            }
        }
    }

    void TabStrip::SetCurrentHistoryItem(TerminalApp::TabStripHistoryItem const& current,
                                         MUX::Controls::TabViewItem const& tab)
    {
        WUX::Media::Brush background{ nullptr };
        WUX::Media::Brush foreground{ nullptr };
        if (const auto color = current ? _tabSelectionBrush(tab, true) : nullptr)
        {
            background = WUX::Media::SolidColorBrush{ color.Color() };
            background.as<WUX::Media::SolidColorBrush>().Opacity(color.Opacity());
            foreground = WUX::Media::SolidColorBrush{ _tabSelectionForeground(color, _sidebarBackdropColor(Background(), ActualTheme())) };
        }
        const auto sameBrush = [](const WUX::Media::Brush& left, const WUX::Media::Brush& right) {
            if (left == right)
            {
                return true;
            }
            const auto leftSolid = left.try_as<WUX::Media::SolidColorBrush>();
            const auto rightSolid = right.try_as<WUX::Media::SolidColorBrush>();
            return leftSolid && rightSolid &&
                   leftSolid.Color() == rightSolid.Color() &&
                   leftSolid.Opacity() == rightSolid.Opacity();
        };
        for (uint32_t index = 0; index < _historyItems.Size(); ++index)
        {
            const auto item = _historyItems.GetAt(index);
            const auto isCurrent = item == current;
            const auto itemBackground = isCurrent ? background : nullptr;
            const auto itemForeground = isCurrent ? foreground : nullptr;
            // Status/projection changes and container realization update unrelated row visuals.
            if (item.IsCurrent() == isCurrent &&
                sameBrush(item.CurrentBackground(), itemBackground) &&
                sameBrush(item.CurrentForeground(), itemForeground))
            {
                continue;
            }
            item.IsCurrent(isCurrent);
            item.CurrentBackground(itemBackground);
            item.CurrentForeground(itemForeground);
            if (const auto container = ItemsList().ContainerFromItem(item).try_as<ListViewItem>())
            {
                _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), item);
                _applyHistoryRowAutomation(container, item);
            }
        }
    }

    void TabStrip::UpdateHistoryKeepRunning(const std::function<bool(const winrt::hstring&)>& isPaneKeepRunning,
                                            const std::function<bool(const winrt::hstring&)>& isPaneHeadless)
    {
        bool changed = false;
        const auto update = [&](const TerminalApp::TabStripHistoryItem& item) {
            item.IsKeepRunning(!item.PaneSessionId().empty() && isPaneKeepRunning(item.PaneSessionId()));
            changed |= winrt::get_self<TabStripHistoryItem>(item)->UpdateHeadlessState(
                !item.PaneSessionId().empty() && isPaneHeadless(item.PaneSessionId()));
        };
        for (const auto& item : _historySnapshot)
        {
            update(item);
        }
        // Equal snapshots can retain distinct visible row objects.
        for (const auto& item : _historyItems)
        {
            update(item);
        }
        if (changed)
        {
            for (size_t index = 0; index < _historySnapshot.size(); ++index)
            {
                _historySearchTerms[index] = _buildHistorySearchTerms(_historySnapshot[index]);
            }
            if (_sidebarFilters.HasSearchQuery())
            {
                _applyHistoryProjection(true);
            }
        }
    }

    void TabStrip::UpdateHistoryPaneOwnership(const std::function<std::optional<bool>(const winrt::hstring&)>& otherWindow)
    {
        bool changed = false;
        const auto update = [&](const TerminalApp::TabStripHistoryItem& item) {
            const auto status = item.Status();
            if (status != L"Idle" && status != L"Working" && status != L"Attention" && status != L"Error")
            {
                return;
            }
            if (const auto ownership = otherWindow(item.PaneSessionId()))
            {
                const auto native = winrt::get_self<TabStripHistoryItem>(item);
                native->BackgroundTab(false);
                native->OtherWindow(*ownership);
                native->UpdateStatusText(item.StatusLabelText());
                changed = true;
            }
        };
        for (const auto& item : _historySnapshot)
        {
            update(item);
        }
        for (const auto& item : _historyItems)
        {
            update(item);
        }
        if (changed)
        {
            for (size_t index = 0; index < _historySnapshot.size(); ++index)
            {
                _historySearchTerms[index] = _buildHistorySearchTerms(_historySnapshot[index]);
            }
            if (_sidebarFilters.HasSearchQuery())
            {
                _applyHistoryProjection(true);
            }
        }
    }

    bool TabStrip::ApplyHistoryStatusDelta(winrt::hstring const& sessionId,
                                           winrt::hstring const& paneSessionId,
                                           winrt::hstring const& status,
                                           winrt::hstring const& statusText)
    {
        bool updated = false;
        const auto applyStatus = [&](TerminalApp::TabStripHistoryItem const& item) {
            const auto nativeItem = winrt::get_self<TabStripHistoryItem>(item);
            const auto liveIt = status == L"Idle" || status == L"Working" ||
                                status == L"Attention" || status == L"Error";
            if (!liveIt || item.PaneSessionId() != paneSessionId)
            {
                nativeItem->OtherWindow(false);
                nativeItem->BackgroundTab(false);
            }
            item.PaneSessionId(paneSessionId);
            item.Status(status);
            nativeItem->UpdateStatusText(statusText);
            item.StatusTextStyle(_historyStatusTextStyle(status));
            item.IsLive(status == L"Idle" ||
                        status == L"Working" ||
                        status == L"Attention" ||
                        status == L"Error" ||
                        status == L"InUse");
            item.IsHistorical(status == L"Ended" || status == L"Historical");
            _updateHistoryCompletionPresentation(item);
        };
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            auto& item = _historySnapshot[index];
            if (item.SessionId() != sessionId)
            {
                continue;
            }

            applyStatus(item);
            _historySearchTerms[index] = _buildHistorySearchTerms(item);
            updated = true;
        }

        if (updated)
        {
            // Equal snapshot refreshes retain the visible row's older object.
            // Update that object too rather than replacing its focused container.
            for (const auto& item : _historyItems)
            {
                if (item.SessionId() == sessionId)
                {
                    applyStatus(item);
                    if (const auto container = ItemsList().ContainerFromItem(item).try_as<ListViewItem>())
                    {
                        _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), item);
                        _applyHistoryRowAutomation(container, item);
                    }
                }
            }
            _applyHistoryProjection(true);
        }
        return updated;
    }

    void TabStrip::ClearHistorySnapshot()
    {
        InvalidateHistorySnapshotReadiness();
        _historySnapshot.clear();
        _historySearchTerms.clear();
        _historyItems.Clear();
        _updateHistoryVisualState();
    }

    void TabStrip::HistoryActive(bool value)
    {
        _sidebarFilters.ShowRecentAgentSessions(value);
    }

    void TabStrip::HistoryLoading(bool value)
    {
        if (value)
        {
            InvalidateHistorySnapshotReadiness();
        }
        if (_historyLoadPending != value)
        {
            _historyLoadPending = value;
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
        if (!value.empty())
        {
            InvalidateHistorySnapshotReadiness();
        }
        if (_historyError != value)
        {
            _historyError = value;
            _updateHistoryVisualState();
        }
    }

    void TabStrip::HistoryRefreshError(winrt::hstring const& value)
    {
        if (!value.empty())
        {
            InvalidateHistorySnapshotReadiness();
        }
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
        _syncViewportItems();
    }

    void TabStrip::SetVerticalPresentation(const bool vertical)
    {
        if (_isVerticalPresentation != vertical)
        {
            _isVerticalPresentation = vertical;
            for (const auto& display : _displayItems)
            {
                winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
            }
            _updateHistoryAgeTimer();
        }
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
            winrt::get_self<TabStripDisplayItem>(display)->UpdatePresentation(_isRailCollapsed, _isVerticalPresentation);
        }
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
        HistoryActive(true);
        if (_searchActive)
        {
            SearchTextBox().Focus(WUX::FocusState::Programmatic);
        }
    }

    void TabStrip::OnHistoryCloseClick(IInspectable const&, WUX::RoutedEventArgs const&)
    {
        HistoryActive(false);
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

    void TabStrip::OnDisplayOptionsOpened(IInspectable const&, IInspectable const&)
    {
        _displayOptionsKeyDownRevoker.revoke();
        // The flyout's input route does not pass through the sidebar's visual tree.
        for (auto parent = WUX::Media::VisualTreeHelper::GetParent(AgentsOnlyFilterMenuItem());
             parent;
             parent = WUX::Media::VisualTreeHelper::GetParent(parent))
        {
            if (const auto presenter = parent.try_as<WUX::Controls::MenuFlyoutPresenter>())
            {
                _displayOptionsKeyDownRevoker = presenter.PreviewKeyDown(auto_revoke, [weakThis{ get_weak() }](auto&&, const auto& e) {
                    if (const auto self = weakThis.get())
                    {
                        self->DisplayOptionsKeyDown.raise(*self, e);
                    }
                });
                return;
            }
        }
        LOG_HR_MSG(E_UNEXPECTED, "Sidebar Display options presenter was not found.");
    }

    void TabStrip::OnDisplayOptionsClosed(IInspectable const&, IInspectable const&)
    {
        _displayOptionsKeyDownRevoker.revoke();
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

        SearchActive(SearchTabsButton().IsChecked().GetBoolean());

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

        SearchQuery(SearchTextBox().Text());
    }

    void TabStrip::OnSearchBoxKeyDown(IInspectable const&, WUX::Input::KeyRoutedEventArgs const& e)
    {
        if (e.OriginalKey() == Windows::System::VirtualKey::Escape)
        {
            SearchActive(false);
            e.Handled(true);
        }
    }

    Grid TabStrip::HistorySection()
    {
        const auto container = ItemsList().ContainerFromItem(_historySectionItem).try_as<ListViewItem>();
        const auto root = container ? container.ContentTemplateRoot().try_as<Grid>() : nullptr;
        return root && root == _historySection.get() ? root : nullptr;
    }

    void TabStrip::OnHistorySectionLoaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (const auto root = sender.try_as<Grid>())
        {
            const auto container = ItemsList().ContainerFromItem(_historySectionItem).try_as<ListViewItem>();
            if (container && container.ContentTemplateRoot() == root)
            {
                _historySection = winrt::make_weak(root);
                const auto previousSync = std::exchange(_syncingHistorySection, true);
                const auto endSync = wil::scope_exit([&]() noexcept { _syncingHistorySection = previousSync; });
                _updateHistoryVisualState();
            }
        }
    }

    void TabStrip::OnHistorySectionUnloaded(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (_historySection.get() == sender)
        {
            _historySection = {};
        }
    }

    void TabStrip::OnHistoryExpandedChanged(IInspectable const& sender, RoutedEventArgs const&)
    {
        if (_syncingHistorySection || !_historySection.get())
        {
            return;
        }
        if (const auto button = sender.try_as<TerminalApp::TabStripHistoryHeader>())
        {
            if (button == HistoryHeaderButton())
            {
                _historyExpanded = button.IsChecked().Value();
                _updateHistoryVisualState();
            }
        }
    }

    void TabStrip::OnHistoryItemClick(IInspectable const&, ItemClickEventArgs const& e)
    {
        if (_historyActivating || HistoryLoading())
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
        _updateSidebarRowToolTipPlacement(root);
        const auto item = root.DataContext().try_as<TerminalApp::TabStripHistoryItem>();
        if (const auto icon = root.FindName(L"HistoryKeepRunningIcon").try_as<FontIcon>())
        {
            const auto help = RS_(L"KeepTabRunningToolTip");
            WUX::Automation::AutomationProperties::SetName(icon, RS_(L"KeepTabRunningText"));
            WUX::Automation::AutomationProperties::SetHelpText(icon, help);
        }
        if (const auto text = root.FindName(L"HistoryKeepRunningTooltipText").try_as<TextBlock>())
        {
            text.Text(RS_(L"KeepTabRunningToolTip"));
        }
        _applyHistoryRowForeground(root, item);
    }

    void TabStrip::OnContainerContentChanging(ListViewBase const&,
                                               ContainerContentChangingEventArgs const& e)
    {
        const auto container = e.ItemContainer().try_as<ListViewItem>();
        if (!container)
        {
            return;
        }

        const auto display = e.InRecycleQueue() ? nullptr : e.Item().try_as<TerminalApp::TabStripDisplayItem>();
        const auto history = e.InRecycleQueue() ? nullptr : e.Item().try_as<TerminalApp::TabStripHistoryItem>();
        const auto header = !e.InRecycleQueue() && e.Item() == _historySectionItem;
        if (!display)
        {
            if (const auto presenter = _findNamedElement(container, L"SidebarItemPresenter").try_as<Primitives::ListViewItemPresenter>();
                presenter)
            {
                if (Windows::Foundation::Metadata::ApiInformation::IsPropertyPresent(
                        L"Windows.UI.Xaml.Controls.Primitives.ListViewItemPresenter", L"SelectionIndicatorVisualEnabled"))
                {
                    presenter.SelectionIndicatorVisualEnabled(true);
                }
                if (presenter.Resources().HasKey(box_value(L"SidebarThemedHover")))
                {
                    presenter.PointerOverBackground(SidebarDefaultHoverBrush().Background());
                    presenter.Resources().Remove(box_value(L"SidebarThemedHover"));
                }
            }
        }
        container.IsTabStop(!header);
        // History keeps the previous row geometry; it isn't indented under the header.
        container.Margin(history || header ? Thickness{ 0 } : Thickness{ 6, 4, 6, 4 });
        _applyHistoryRowAutomation(container, history);
        _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), history);
        if (display)
        {
            // Keep scrolling placeholders at the same minimum height as their tab headers.
            container.MinHeight(display.HeaderMinHeight());
            // XAML's dynamic binding subscriber is not agile and attempts to marshal
            // PropertyChangedEventHandler during realization. Keep these UI-thread
            // automation updates on our own native delegates instead.
            const auto presentation = display.Presentation();
            WUX::Automation::AutomationProperties::SetName(container, presentation.AutomationName());
            WUX::Automation::AutomationProperties::SetAcceleratorKey(container, display.AcceleratorKey());
            WUX::Automation::AutomationProperties::SetHelpText(container, display.ToolTipText());
            for (auto it = _rowAutomationSubscriptions.begin(); it != _rowAutomationSubscriptions.end();)
            {
                it = it->second.Container.get() ? std::next(it) : _rowAutomationSubscriptions.erase(it);
            }
            const auto key = winrt::get_abi(container);
            _rowAutomationSubscriptions.erase(key);
            const auto weakContainer = winrt::make_weak(container);
            const auto weakDisplay = winrt::make_weak(display);
            auto presentationChanged = presentation.PropertyChanged(auto_revoke, [weakContainer, weakDisplay](auto&&, auto&&) {
                if (const auto row = weakContainer.get())
                {
                    if (const auto item = weakDisplay.get();
                        item && row.Content() == item)
                    {
                        WUX::Automation::AutomationProperties::SetName(row, item.Presentation().AutomationName());
                    }
                }
            });
            auto displayChanged = display.PropertyChanged(auto_revoke, [weakContainer, weakDisplay](auto&&, auto&&) {
                if (const auto row = weakContainer.get())
                {
                    if (const auto item = weakDisplay.get();
                        item && row.Content() == item)
                    {
                        row.MinHeight(item.HeaderMinHeight());
                        WUX::Automation::AutomationProperties::SetAcceleratorKey(row, item.AcceleratorKey());
                        WUX::Automation::AutomationProperties::SetHelpText(row, item.ToolTipText());
                    }
                }
            });
            _rowAutomationSubscriptions.emplace(key, RowAutomationSubscriptions{
                weakContainer, std::move(presentationChanged), std::move(displayChanged) });
        }
        else
        {
            _rowAutomationSubscriptions.erase(winrt::get_abi(container));
            container.ClearValue(FrameworkElement::MinHeightProperty());
            container.ClearValue(WUX::Automation::AutomationProperties::AcceleratorKeyProperty());
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
                                  display);
    }

    void TabStrip::_applyRailState()
    {
        const auto expandedVisibility = _isRailCollapsed ? Visibility::Collapsed : Visibility::Visible;
        const auto collapsedVisibility = _isRailCollapsed ? Visibility::Visible : Visibility::Collapsed;

        MinWidth(_isRailCollapsed ? 40.0 : 180.0);
        CompactNewTabToolbar().Visibility(collapsedVisibility);
        VerticalTabsHeader().Visibility(expandedVisibility);
        VerticalTabsHeader().Visibility(expandedVisibility);
        SearchTabsButton().IsHitTestVisible(true);
        SearchTabsButton().IsEnabled(_projectionControlsEnabled);
        FilterTabsButton().Visibility(_richTabMetadataControlsVisible ? expandedVisibility : Visibility::Collapsed);
        FilterTabsButton().IsHitTestVisible(!_isRailCollapsed);
        FilterTabsButton().IsEnabled(_projectionControlsEnabled && !_isRailCollapsed);
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
                winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
                for (const auto& pane : display.PaneItems())
                {
                    winrt::get_self<TabStripPaneItem>(pane)->UpdateCloseVisibility(_isRailCollapsed);
                }
                _refreshDisplayItemVisuals(display);
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
            winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
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
        const auto searchLabel = RS_(L"VerticalTabsAgentsSearch/Text");
        SearchTextBox().PlaceholderText(searchLabel);
        WUX::Automation::AutomationProperties::SetName(
            SearchTextBox(), searchLabel);
        WUX::Automation::AutomationProperties::SetName(
            SearchTabsButton(), searchLabel);
        ToolTipService::SetToolTip(
            SearchTabsButton(), box_value(searchLabel));
        WUX::Automation::AutomationProperties::SetHelpText(
            SearchTabsButton(), searchLabel);

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
        append(item.Cwd());
        append(item.Subtitle() + item.StatusText());
        append(item.Subtitle() + L" \u00b7 " + item.StatusText());
        auto providerLabel = item.ProviderDisplayName();
        if (item.AgentSource() == L"wsl" && !item.WslDistro().empty())
        {
            providerLabel = providerLabel + L" \u00b7 " + item.WslDistro();
        }
        append(providerLabel + L" \u00b7 " + item.Subtitle() + L" \u00b7 " + item.StatusText());
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
        const auto searchQuery = SearchQuery();
        if (searchQuery.empty())
        {
            return true;
        }

        const std::wstring_view query{ searchQuery.c_str(), searchQuery.size() };
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
                                 TerminalApp::TabStripHistoryItem const& right,
                                 const bool includeStatus = true)
    {
        return left == right ||
               (left.SessionId() == right.SessionId() &&
                left.AgentId() == right.AgentId() &&
                left.AgentSource() == right.AgentSource() &&
                left.WslDistro() == right.WslDistro() &&
                left.SessionUniverse() == right.SessionUniverse() &&
                left.Title() == right.Title() &&
                left.Subtitle() == right.Subtitle() &&
                left.SearchQuery() == right.SearchQuery() &&
                left.ProviderDisplayName() == right.ProviderDisplayName() &&
                left.Cwd() == right.Cwd() &&
                left.IsAgentPane() == right.IsAgentPane() &&
                left.IconTemplate() == right.IconTemplate() &&
                (!includeStatus ||
                 (left.Status() == right.Status() &&
                  winrt::get_self<TabStripHistoryItem>(left)->HasUnreadCompletion() == winrt::get_self<TabStripHistoryItem>(right)->HasUnreadCompletion() &&
                  left.StatusText() == right.StatusText() &&
                  left.PaneSessionId() == right.PaneSessionId() &&
                  left.IsLive() == right.IsLive() &&
                  left.IsHistorical() == right.IsHistorical() &&
                  winrt::get_self<TabStripHistoryItem>(left)->BackgroundTab() == winrt::get_self<TabStripHistoryItem>(right)->BackgroundTab() &&
                  winrt::get_self<TabStripHistoryItem>(left)->OtherWindow() == winrt::get_self<TabStripHistoryItem>(right)->OtherWindow() &&
                  left.StatusTextStyle() == right.StatusTextStyle())));
    }

    void TabStrip::_applyHistoryProjection(const bool preserveScroll)
    {
        const auto sameHistoryKey = [](const auto& left, const auto& right) {
            return left.SessionId() == right.SessionId() &&
                   left.AgentId() == right.AgentId() &&
                   left.AgentSource() == right.AgentSource() &&
                   left.WslDistro() == right.WslDistro() &&
                   left.SessionUniverse() == right.SessionUniverse();
        };
        std::vector<TerminalApp::TabStripHistoryItem> representedKeys;
        if (_sidebarFilters.RecentAgentSessionsVisible())
        {
            for (const auto& session : _representedHistorySessions)
            {
                if (!session.inferredAgentSource.empty())
                {
                    for (const auto& item : _historySnapshot)
                    {
                        if (item.SessionId() == session.sessionId && item.AgentId() == session.agentId &&
                            item.AgentSource() == session.inferredAgentSource &&
                            item.WslDistro() == session.inferredWslDistro &&
                            item.SessionUniverse() == session.inferredSessionUniverse &&
                            item.PaneSessionId().empty())
                        {
                            representedKeys.emplace_back(item);
                        }
                    }
                    continue;
                }
                TerminalApp::TabStripHistoryItem candidate{ nullptr };
                TerminalApp::TabStripHistoryItem paneCandidate{ nullptr };
                bool ambiguous = false;
                bool paneAmbiguous = false;
                for (const auto& item : _historySnapshot)
                {
                    if (item.SessionId() != session.sessionId ||
                        (!session.agentId.empty() && item.AgentId() != session.agentId))
                    {
                        continue;
                    }
                    if (candidate && !sameHistoryKey(candidate, item))
                    {
                        ambiguous = true;
                    }
                    else if (!candidate)
                    {
                        candidate = item;
                    }
                    const auto rowPaneId = item.PaneSessionId();
                    if (!session.paneSessionId.empty() &&
                        til::compare_ordinal_insensitive(
                            std::wstring_view{ rowPaneId.c_str(), rowPaneId.size() },
                            std::wstring_view{ session.paneSessionId.c_str(), session.paneSessionId.size() }) == 0)
                    {
                        if (paneCandidate && !sameHistoryKey(paneCandidate, item))
                        {
                            paneAmbiguous = true;
                        }
                        else if (!paneCandidate)
                        {
                            paneCandidate = item;
                        }
                    }
                }
                // Rebound sessions may still carry an old pane ID in the snapshot.
                // With colliding identities, only a pane-qualified match is safe.
                if (!paneAmbiguous && paneCandidate)
                {
                    representedKeys.emplace_back(paneCandidate);
                }
                else if (!ambiguous && candidate)
                {
                    representedKeys.emplace_back(candidate);
                }
            }
        }

        std::vector<TerminalApp::TabStripHistoryItem> visibleItems;
        visibleItems.reserve(_historySnapshot.size());
        for (size_t index = 0; index < _historySnapshot.size(); ++index)
        {
            _historySnapshot[index].SearchQuery(SearchQuery());
            const auto& item = _historySnapshot[index];
            if (std::ranges::any_of(representedKeys, [&](const auto& represented) {
                    return sameHistoryKey(represented, item);
                }))
            {
                continue;
            }
            if (_matchesHistorySearch(index))
            {
                visibleItems.emplace_back(_historySnapshot[index]);
            }
        }
        if (preserveScroll)
        {
            const auto historyKey = [](const auto& item) {
                return std::array{
                    item.SessionId(),
                    item.AgentId(),
                    item.AgentSource(),
                    item.WslDistro(),
                    item.SessionUniverse()
                };
            };
            std::set<std::array<winrt::hstring, 5>> visibleKeys;
            for (const auto& item : visibleItems)
            {
                visibleKeys.emplace(historyKey(item));
            }
            // Preserve surviving rows when membership changes, and update status in place.
            std::set<std::array<winrt::hstring, 5>> retainedKeys;
            for (uint32_t index = _historyItems.Size(); index > 0; --index)
            {
                const auto key = historyKey(_historyItems.GetAt(index - 1));
                if (!visibleKeys.contains(key))
                {
                    _historyItems.RemoveAt(index - 1);
                }
                else
                {
                    retainedKeys.emplace(key);
                }
            }
            for (uint32_t index = 0; index < visibleItems.size(); ++index)
            {
                if (index < _historyItems.Size() && !sameHistoryKey(_historyItems.GetAt(index), visibleItems[index]))
                {
                    auto existing = _historyItems.Size();
                    if (retainedKeys.contains(historyKey(visibleItems[index])))
                    {
                        existing = index + 1;
                        while (existing < _historyItems.Size() && !sameHistoryKey(_historyItems.GetAt(existing), visibleItems[index]))
                        {
                            ++existing;
                        }
                    }
                    if (existing < _historyItems.Size())
                    {
                        const auto moved = _historyItems.GetAt(existing);
                        _historyItems.RemoveAt(existing);
                        _historyItems.InsertAt(index, moved);
                    }
                    else
                    {
                        _historyItems.InsertAt(index, visibleItems[index]);
                    }
                }
                if (index >= _historyItems.Size())
                {
                    _historyItems.Append(visibleItems[index]);
                }
                else
                {
                    const auto current = _historyItems.GetAt(index);
                    const auto incoming = visibleItems[index];
                    current.Subtitle(incoming.Subtitle());
                    if (_sameHistoryItem(current, incoming, false))
                    {
                        const auto nativeCurrent = winrt::get_self<TabStripHistoryItem>(current);
                        const auto nativeIncoming = winrt::get_self<TabStripHistoryItem>(incoming);
                        if (!_sameHistoryItem(current, incoming))
                        {
                            const auto paneChanged = current.PaneSessionId() != incoming.PaneSessionId();
                            current.PaneSessionId(incoming.PaneSessionId());
                            current.Status(incoming.Status());
                            current.IsLive(incoming.IsLive());
                            current.IsHistorical(incoming.IsHistorical());
                            nativeCurrent->BackgroundTab(nativeIncoming->BackgroundTab());
                            nativeCurrent->OtherWindow(nativeIncoming->OtherWindow());
                            nativeCurrent->SetUnreadCompletion(nativeIncoming->HasUnreadCompletion());
                            if (paneChanged)
                            {
                                nativeCurrent->UpdateHeadlessState(false);
                            }
                            nativeCurrent->UpdateStatusText(incoming.StatusLabelText());
                            current.StatusTextStyle(incoming.StatusTextStyle());
                            if (const auto container = ItemsList().ContainerFromItem(current).try_as<ListViewItem>())
                            {
                                _applyHistoryRowForeground(container.ContentTemplateRoot().try_as<FrameworkElement>(), current);
                                _applyHistoryRowAutomation(container, current);
                            }
                        }
                        nativeCurrent->LastActivityAtMs(nativeIncoming->LastActivityAtMs());
                    }
                    else
                    {
                        _historyItems.SetAt(index, incoming);
                    }
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
        const auto visible = _sidebarFilters.RecentAgentSessionsVisible() && !_isRailCollapsed;
        const auto expanded = _historyExpanded || _sidebarFilters.HasSearchQuery();
        const auto loading = HistoryLoading();
        const auto section = HistorySection();
        if (section)
        {
            section.Visibility(visible ? Visibility::Visible : Visibility::Collapsed);
        }
        TabsToolbar().Visibility(Visibility::Visible);
        VerticalTabsHeader().Text(RS_(L"VerticalTabsHeader/Text"));
        const auto detailsLabel = RS_(L"VerticalTabsFilterButton/[using:Windows.UI.Xaml.Controls]ToolTipService/ToolTip");
        ToolTipService::SetToolTip(FilterTabsButton(), box_value(detailsLabel));
        WUX::Automation::AutomationProperties::SetName(FilterTabsButton(), detailsLabel);
        WUX::Automation::AutomationProperties::SetHelpText(FilterTabsButton(), detailsLabel);
        ItemsList().Visibility(visible || _tabsVisible ? Visibility::Visible : Visibility::Collapsed);
        ItemsList().IsItemClickEnabled(visible && !_historyActivating && !loading);
        if (section)
        {
            const auto previousSync = std::exchange(_syncingHistorySection, true);
            const auto endSync = wil::scope_exit([&]() noexcept { _syncingHistorySection = previousSync; });
            HistoryHeaderButton().IsChecked(expanded);
            HistoryHeaderButton().IsEnabled(!_sidebarFilters.HasSearchQuery());
            HistoryChevron().Glyph(expanded ? L"\xE70D" : L"\xE76C");
            HistoryStatusPanel().Visibility(expanded ? Visibility::Visible : Visibility::Collapsed);
            HistoryLoadingIndicator().IsActive(visible && expanded && loading);
            HistoryLoadingIndicator().Visibility(visible && loading ? Visibility::Visible : Visibility::Collapsed);
            WUX::Automation::AutomationProperties::SetName(HistoryHeaderButton(), HistoryHeader().Text());
            const auto historyAction = expanded ?
                                           RS_(L"VerticalTabsCollapseRecentSessions") : RS_(L"VerticalTabsExpandRecentSessions");
            ToolTipService::SetToolTip(HistoryHeaderButton(), box_value(historyAction));
            WUX::Automation::AutomationProperties::SetHelpText(HistoryHeaderButton(), historyAction);
            if (!visible || loading)
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
                const auto searchQuery = SearchQuery();
                const std::wstring_view query{ searchQuery.c_str(), searchQuery.size() };
                const auto hasSearchQuery = query.find_first_not_of(L" \t\r\n") != std::wstring_view::npos;
                HistoryMessage().Text(_historySnapshot.empty() || !hasSearchQuery ?
                                          RS_(L"VerticalTabsHistoryEmpty") :
                                          RS_(L"VerticalTabsHistoryNoMatches"));
                HistoryMessage().Visibility(Visibility::Visible);
            }
            else
            {
                HistoryMessage().Visibility(Visibility::Collapsed);
            }
        }
        if (!_syncingHistorySection)
        {
            _syncViewportItems();
        }
    }

    void TabStrip::_syncViewportItems()
    {
        const auto combined = _sidebarFilters.RecentAgentSessionsVisible() && !_isRailCollapsed;
        const auto selection = SelectedItem();
        const auto previousSync = std::exchange(_syncingViewportSelection, true);
        const auto endSync = wil::scope_exit([&]() noexcept { _syncingViewportSelection = previousSync; });
        if (combined)
        {
            uint32_t headerIndex{};
            if (!_viewportItems.IndexOf(_historySectionItem, headerIndex))
            {
                _viewportItems.Clear();
                for (const auto& display : _displayItems)
                {
                    _viewportItems.Append(display);
                }
                headerIndex = _viewportItems.Size();
                _viewportItems.Append(_historySectionItem);
            }
            else
            {
                // Native reorder can drop a live row below Recent Sessions.
                // Keep the section element/container alive while normalizing that move.
                for (uint32_t index = _viewportItems.Size(); index > headerIndex + 1; --index)
                {
                    if (_viewportItems.GetAt(index - 1).try_as<TerminalApp::TabStripDisplayItem>())
                    {
                        _viewportItems.RemoveAt(index - 1);
                    }
                }
                for (uint32_t index = headerIndex; index > 0; --index)
                {
                    const auto display = _viewportItems.GetAt(index - 1).try_as<TerminalApp::TabStripDisplayItem>();
                    uint32_t canonicalIndex{};
                    if (!display || !_displayItems.IndexOf(display, canonicalIndex))
                    {
                        _viewportItems.RemoveAt(index - 1);
                        --headerIndex;
                    }
                }
                for (uint32_t index = 0; index < _displayItems.Size(); ++index)
                {
                    const auto desired = _displayItems.GetAt(index);
                    if (index < headerIndex && _viewportItems.GetAt(index) == desired)
                    {
                        continue;
                    }
                    uint32_t existingIndex{};
                    if (_viewportItems.IndexOf(desired, existingIndex) && existingIndex < headerIndex)
                    {
                        _viewportItems.RemoveAt(existingIndex);
                        --headerIndex;
                    }
                    _viewportItems.InsertAt(index, desired);
                    ++headerIndex;
                }
            }
            const auto historyCount = (_historyExpanded || _sidebarFilters.HasSearchQuery()) ? _historyItems.Size() : 0;
            // Query canonical IUnknown identity, not the projected interface pointer.
            std::unordered_set<::IUnknown*> retainedHistoryItems;
            retainedHistoryItems.reserve(historyCount);
            for (uint32_t index = 0; index < historyCount; ++index)
            {
                retainedHistoryItems.emplace(_historyItems.GetAt(index).as<::IUnknown>().get());
            }
            std::unordered_set<::IUnknown*> viewportHistoryItems;
            viewportHistoryItems.reserve(historyCount);
            for (uint32_t index = _viewportItems.Size(); index > headerIndex + 1; --index)
            {
                const auto item = _viewportItems.GetAt(index - 1).try_as<TerminalApp::TabStripHistoryItem>();
                const auto identity = item ? item.as<::IUnknown>().get() : nullptr;
                if (!historyCount || !item || !retainedHistoryItems.contains(identity))
                {
                    _viewportItems.RemoveAt(index - 1);
                }
                else
                {
                    viewportHistoryItems.emplace(identity);
                }
            }
            for (uint32_t index = 0; index < historyCount; ++index)
            {
                const auto slot = headerIndex + 1 + index;
                const auto item = _historyItems.GetAt(index);
                if (slot >= _viewportItems.Size())
                {
                    _viewportItems.Append(item);
                }
                else if (_viewportItems.GetAt(slot) != item)
                {
                    uint32_t existing{};
                    if (viewportHistoryItems.contains(item.as<::IUnknown>().get()) && _viewportItems.IndexOf(item, existing))
                    {
                        _viewportItems.RemoveAt(existing);
                    }
                    _viewportItems.InsertAt(slot, item);
                }
                viewportHistoryItems.emplace(item.as<::IUnknown>().get());
            }
            while (_viewportItems.Size() > headerIndex + 1 + historyCount)
            {
                _viewportItems.RemoveAtEnd();
            }
            if (ItemsList().ItemsSource() != _viewportItems)
            {
                ItemsList().ItemsSource(_viewportItems);
            }
        }
        else if (ItemsList().ItemsSource() != _displayItems)
        {
            ItemsList().ItemsSource(_displayItems);
            _viewportItems.Clear();
        }
        SelectedItem(selection);
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
            _syncViewportItems();
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
                _syncViewportItems();
                _applyTabItemVisibility(item);
            }
            break;
        case CollectionChange::ItemRemoved:
            if (args.Index() < _displayItems.Size())
            {
                _displayItems.RemoveAt(args.Index());
                _syncViewportItems();
            }
            break;
        case CollectionChange::ItemChanged:
            if (const auto item = sender.GetAt(args.Index()).try_as<MUX::Controls::TabViewItem>())
            {
                _displayItems.SetAt(args.Index(), _makeDisplayItem(item));
                _syncViewportItems();
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
        auto display = winrt::make<TabStripDisplayItem>(tab, tab.Header());
        auto title = WUX::Automation::AutomationProperties::GetName(tab);
        if (title.empty())
        {
            title = RS_(L"VerticalTabsFallbackTabTitle");
        }
        display.Title(title);
        if (!tab.Header())
        {
            display.Presentation().Title(title);
        }
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
        winrt::get_self<TabStripDisplayItem>(display)->SyncTabPresentation(_isRailCollapsed, _isVerticalPresentation);
        return display;
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
        _displayItems.ReplaceAll(next);
        _syncViewportItems();
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

    void TabStrip::_onHistoryPreviewKeyDown(IInspectable const&, WUX::Input::KeyRoutedEventArgs const& e)
    {
        if (e.Handled() || e.OriginalKey() != Windows::System::VirtualKey::Enter ||
            _historyActivating || HistoryLoading())
        {
            return;
        }

        const auto coreWindow = Windows::UI::Core::CoreWindow::GetForCurrentThread();
        const auto root = XamlRoot();
        if (!coreWindow || !root)
        {
            return;
        }
        constexpr auto down = Windows::UI::Core::CoreVirtualKeyStates::Down;
        if (WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Control), down) ||
            WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Menu), down) ||
            WI_IsFlagSet(coreWindow.GetKeyState(Windows::System::VirtualKey::Shift), down))
        {
            return;
        }

        auto focused = WUX::Input::FocusManager::GetFocusedElement(root).try_as<DependencyObject>();
        while (focused)
        {
            if (focused.try_as<WUX::Controls::Primitives::ButtonBase>())
            {
                return;
            }
            if (const auto container = focused.try_as<ListViewItem>())
            {
                const auto item = container.Content().try_as<TerminalApp::TabStripHistoryItem>();
                if (item && ItemsList().ContainerFromItem(item) == container)
                {
                    e.Handled(true);
                    HistoryActivationRequested.raise(*this, winrt::make<TabStripHistoryActivationEventArgs>(item));
                }
                return;
            }
            focused = WUX::Media::VisualTreeHelper::GetParent(focused);
        }
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
        if (_syncingViewportSelection)
        {
            return;
        }
        if (e.AddedItems().Size() && !_tabFromItem(e.AddedItems().GetAt(0)))
        {
            // Focusing/resuming history must not change the canonical tab selection.
            _syncingViewportSelection = true;
            const auto endSync = wil::scope_exit([&]() noexcept { _syncingViewportSelection = false; });
            ItemsList().SelectedItem(e.RemovedItems().Size() ? e.RemovedItems().GetAt(0) : nullptr);
            return;
        }
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
                winrt::get_self<TabStripDisplayItem>(display)->UpdateCloseVisibility(_isRailCollapsed);
                _updatePaneSelection(display, false, _isRailCollapsed);
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
                winrt::get_self<TabStripDisplayItem>(display)->UpdateCloseVisibility(_isRailCollapsed);
                _updatePaneSelection(display, true, _isRailCollapsed);
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
        if (!tab)
        {
            e.Cancel(true);
            return;
        }
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
        winrt::get_self<TabStripDisplayItem>(display)->UpdatePresentation(_isRailCollapsed, _isVerticalPresentation);
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
            const auto pane = root.DataContext().try_as<TerminalApp::TabStripPaneItem>();
            if (pane)
            {
                winrt::get_self<TabStripPaneItem>(pane)->SetPointerOver(true, _isRailCollapsed);
                _updatePaneRowVisuals(root, pane);
            }
            else if (const auto background = root.FindName(L"PaneHoverBackground").try_as<UIElement>())
            {
                background.Opacity(1.0);
            }
        }
    }

    void TabStrip::OnPanePointerExited(IInspectable const& sender,
                                       WUX::Input::PointerRoutedEventArgs const& e)
    {
        if (const auto root = sender.try_as<FrameworkElement>())
        {
            if (e)
            {
                const auto position = e.GetCurrentPoint(root).Position();
                if (position.X >= 0 && position.X < root.ActualWidth() &&
                    position.Y >= 0 && position.Y < root.ActualHeight())
                {
                    return;
                }
            }
            if (const auto pane = root.DataContext().try_as<TerminalApp::TabStripPaneItem>())
            {
                winrt::get_self<TabStripPaneItem>(pane)->SetPointerOver(false, _isRailCollapsed);
                _updatePaneRowVisuals(root, pane);
            }
            else if (const auto background = root.FindName(L"PaneHoverBackground").try_as<UIElement>())
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
            uint32_t tabIndex{};
            for (const auto& value : ItemsList().Items())
            {
                const auto display = value.try_as<TerminalApp::TabStripDisplayItem>();
                if (!display)
                {
                    continue;
                }
                if (draggedTab && winrt::get_abi(display.Tab()) == winrt::get_abi(draggedTab))
                {
                    destinationIndex = tabIndex;
                    break;
                }
                ++tabIndex;
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
                if (_sidebarFilters.RecentAgentSessionsVisible() && !_isRailCollapsed)
                {
                    const auto display = _displayItems.GetAt(sourceIndex);
                    _displayItems.RemoveAt(sourceIndex);
                    _displayItems.InsertAt(*destinationIndex, display);
                }
            }
            _syncViewportItems();
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
