// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TabHeaderControl.h"
#include <winrt/Windows.UI.Xaml.Markup.h>

#include "TabHeaderControl.g.cpp"
#include "TabHeaderPresentation.g.cpp"

using namespace winrt;
using namespace winrt::Microsoft::UI::Xaml;

namespace winrt::TerminalApp::implementation
{
    TabHeaderControl::TabHeaderControl()
    {
        Presentation(winrt::make<TabHeaderPresentation>());
        InitializeComponent();
        _progressRingPresenter = HeaderProgressRingPresenter();
        _compactCompletionIndicator = HeaderCompactCompletionIndicator();
        Windows::UI::Xaml::Data::Binding completionBrush;
        completionBrush.Source(HeaderCompletionDot());
        completionBrush.Path(Windows::UI::Xaml::PropertyPath{ L"Fill" });
        completionBrush.Mode(Windows::UI::Xaml::Data::BindingMode::OneWay);
        _compactCompletionIndicator.Children().GetAt(0).as<Windows::UI::Xaml::Shapes::Ellipse>().SetBinding(
            Windows::UI::Xaml::Shapes::Shape::FillProperty(), completionBrush);
        _initialized = true;
        _UpdateUnreadActivityVisibility();
        _UpdateTitleVisibility();

        Loaded([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_AttachProgressToNativeIconSlot();
            }
        });
        Unloaded([weakThis = get_weak()](auto&&, auto&&) {
            if (const auto self = weakThis.get())
            {
                self->_ClearNativeHeaderObserver();
            }
        });

        const auto keepRunningIcon = HeaderKeepRunningIcon();
        const auto keepRunningName = RS_(L"HeadlessModeOnName");
        const auto keepRunningHelp = RS_(L"KeepTabRunningToolTip");
        Windows::UI::Xaml::Automation::AutomationProperties::SetName(keepRunningIcon, keepRunningName);
        Windows::UI::Xaml::Automation::AutomationProperties::SetHelpText(keepRunningIcon, keepRunningHelp);
        Windows::UI::Xaml::Controls::ToolTipService::SetToolTip(keepRunningIcon, box_value(keepRunningHelp));

        const auto pinnedIcon = HeaderPinnedIcon();
        const auto pinnedName = RS_(L"PinnedTabName");
        Windows::UI::Xaml::Automation::AutomationProperties::SetName(pinnedIcon, pinnedName);
        Windows::UI::Xaml::Controls::ToolTipService::SetToolTip(pinnedIcon, box_value(pinnedName));

        // We'll only process the KeyUp event if we received an initial KeyDown event first.
        // Avoids issue immediately closing the tab rename when we see the enter KeyUp event that was
        // sent to the command palette to trigger the openTabRenamer action in the first place.
        HeaderRenamerTextBox().KeyDown([&](auto&&, auto&& e) {
            _receivedKeyDown = true;

            // GH#9632 - mark navigation buttons as handled.
            // This should prevent the tab view to use this key for navigation between tabs
            if (e.OriginalKey() == Windows::System::VirtualKey::Down ||
                e.OriginalKey() == Windows::System::VirtualKey::Up ||
                e.OriginalKey() == Windows::System::VirtualKey::Left ||
                e.OriginalKey() == Windows::System::VirtualKey::Right)
            {
                e.Handled(true);
            }
        });

        // NOTE: (Preview)KeyDown does not work here. If you use that, we'll
        // remove the TextBox from the UI tree, then the following KeyUp
        // will bubble to the NewTabButton, which we don't want to have
        // happen.
        HeaderRenamerTextBox().KeyUp([&](auto&&, const Windows::UI::Xaml::Input::KeyRoutedEventArgs& e) {
            if (_receivedKeyDown)
            {
                if (e.OriginalKey() == Windows::System::VirtualKey::Enter)
                {
                    // User is done making changes, close the rename box
                    _CloseRenameBox();
                }
                else if (e.OriginalKey() == Windows::System::VirtualKey::Escape)
                {
                    // User wants to discard the changes they made,
                    // set _renameCancelled to true and close the rename box
                    _renameCancelled = true;
                    _CloseRenameBox();
                }
            }
        });
    }

    TabHeaderControl::~TabHeaderControl()
    {
        try
        {
            _ClearNativeHeaderObserver();
        }
        CATCH_LOG()
    }

    void TabHeaderControl::_ClearNativeHeaderObserver()
    {
        if (const auto presenter = _nativeHeaderPresenter.get())
        {
            presenter.UnregisterPropertyChangedCallback(Windows::UI::Xaml::UIElement::VisibilityProperty(), _nativeHeaderVisibilityToken);
            SetMetadataForeground(nullptr, false);
        }
        _nativeHeaderPresenter = nullptr;
    }

    void TabHeaderControl::_AttachProgressToNativeIconSlot()
    {
        // WinUI Compact hides the unselected tab's entire header. Move this
        // header's existing bell/progress presenter into the native leading icon
        // slot instead; never share a UIElement between two visual parents.
        using namespace Windows::UI::Xaml;
        using namespace Windows::UI::Xaml::Controls;
        Microsoft::UI::Xaml::Controls::TabViewItem tab{ nullptr };
        for (DependencyObject parent = HeaderLayout(); parent; parent = Media::VisualTreeHelper::GetParent(parent))
        {
            if ((tab = parent.try_as<Microsoft::UI::Xaml::Controls::TabViewItem>()))
            {
                break;
            }
        }
        if (!tab || Media::VisualTreeHelper::GetChildrenCount(tab) == 0)
        {
            return; // Sidebar headers have their own descriptor-owned slot.
        }
        const auto root = Media::VisualTreeHelper::GetChild(tab, 0).as<FrameworkElement>();
        const auto box = root.FindName(L"IconBox").try_as<Viewbox>();
        const auto icon = root.FindName(L"IconControl").try_as<ContentControl>();
        if (!box || !icon)
        {
            return;
        }
        _ClearNativeHeaderObserver();
        if (const auto presenter = root.FindName(L"ContentPresenter").try_as<UIElement>())
        {
            _nativeHeaderPresenter = winrt::make_weak(presenter);
            // Native tabs own their selection/hover contrast palette.
            SetMetadataForeground(nullptr, true);
            _nativeHeaderVisibilityToken = presenter.RegisterPropertyChangedCallback(UIElement::VisibilityProperty(), [weakThis = get_weak()](auto&&, auto&&) {
                if (const auto self = weakThis.get())
                {
                    self->_UpdateUnreadActivityVisibility();
                }
            });
        }
        _UpdateUnreadActivityVisibility();
        const auto progress = _progressRingPresenter;
        const auto previousProgressParent = Media::VisualTreeHelper::GetParent(progress);
        if (previousProgressParent == box.Child() && Media::VisualTreeHelper::GetParent(icon) == box.Child())
        {
            return; // Already installed for this template instance.
        }
        uint32_t index{};
        if (const auto previous = previousProgressParent.try_as<Panel>();
            previous && previous.Children().IndexOf(progress, index))
        {
            previous.Children().RemoveAt(index);
        }
        // A fresh header may replace a previous header without replacing the
        // TabViewItem template. Detach the original native icon before replacing
        // the old slot, which also releases its old header's presenter.
        if (const auto previous = Media::VisualTreeHelper::GetParent(icon).try_as<Panel>();
            previous && previous.Children().IndexOf(icon, index))
        {
            previous.Children().RemoveAt(index);
        }
        box.Child(nullptr);
        Grid slot;
        slot.Children().Append(icon);
        slot.Children().Append(progress);
        box.Child(slot);
        icon.Visibility(TabStatus() && (TabStatus().BellIndicator() || TabStatus().IsProgressRingActive()) ?
                            Windows::UI::Xaml::Visibility::Collapsed :
                            Windows::UI::Xaml::Visibility::Visible);
    }

    void TabHeaderControl::Presentation(const TerminalApp::TabHeaderPresentation& value)
    {
        THROW_HR_IF(E_INVALIDARG, !value);
        if (_presentation == value)
        {
            return;
        }
        if (_presentation)
        {
            // Recycling must not commit an old row's edit to its new data owner.
            _renameCancelled = true;
            _CloseRenameBox(false);
        }
        _presentationChanged.revoke();
        _presentation = value;
        _presentationChanged = value.PropertyChanged(winrt::auto_revoke, [weakThis = get_weak()](auto&&, const auto& args) {
            if (const auto self = weakThis.get())
            {
                self->PropertyChanged.raise(*self, args);
                if (args.PropertyName() == L"SearchText")
                {
                    self->_UpdateTitleVisibility();
                }
                if (args.PropertyName() == L"HasUnreadActivity" || args.PropertyName() == L"UnreadActivityProjectedToPaneRows")
                {
                    self->_UpdateUnreadActivityVisibility();
                }
            }
        });
        for (const auto name : { L"Presentation", L"Title", L"SearchText", L"RenamerMaxWidth", L"TabStatus", L"MetadataText", L"MetadataAutomationName", L"MetadataHighlightColor", L"MetadataHighlightRanges" })
        {
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ name });
        }
        _UpdateUnreadActivityVisibility();
        _UpdateTitleVisibility();
    }

    void TabHeaderControl::PaneRowsVisibility(const Windows::UI::Xaml::Visibility value)
    {
        if (_paneRowsVisibility != value)
        {
            _paneRowsVisibility = value;
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ L"PaneRowsVisibility" });
            _UpdateUnreadActivityVisibility();
        }
    }

    void TabHeaderControl::_UpdateUnreadActivityVisibility()
    {
        if (_initialized)
        {
            const auto presenter = _nativeHeaderPresenter.get();
            const auto compact = presenter && presenter.Visibility() == Windows::UI::Xaml::Visibility::Collapsed;
            const auto unread = _presentation.HasUnreadActivity() &&
                                _paneRowsVisibility != Windows::UI::Xaml::Visibility::Visible;
            HeaderCompletionIndicator().Visibility(
                unread && !compact ?
                    Windows::UI::Xaml::Visibility::Visible :
                    Windows::UI::Xaml::Visibility::Collapsed);
            _compactCompletionIndicator.Visibility(
                unread && compact ?
                    Windows::UI::Xaml::Visibility::Visible :
                    Windows::UI::Xaml::Visibility::Collapsed);
        }
    }

    TerminalApp::TextHighlightColor TabHeaderControl::AgentStatusHighlightColor(const std::string_view status) noexcept
    {
        return status == "Working"   ? TerminalApp::TextHighlightColor::Success :
               status == "Attention" ? TerminalApp::TextHighlightColor::Caution :
               status == "Error"     ? TerminalApp::TextHighlightColor::Critical :
                                       TerminalApp::TextHighlightColor::None;
    }

    Windows::Foundation::Collections::IVector<TerminalApp::HighlightedRun> TabHeaderControl::BuildMetadataHighlightRanges(
        const ::Microsoft::Terminal::RichTab::Provider::Presentation& presentation)
    {
        if (presentation.agentStatusRanges.empty())
        {
            return nullptr;
        }
        std::vector<TerminalApp::HighlightedRun> ranges;
        for (const auto& range : presentation.agentStatusRanges)
        {
            THROW_HR_IF(E_INVALIDARG, range.length == 0 || range.start >= presentation.text.size() || range.length > presentation.text.size() - range.start);
            ranges.push_back({ range.start, range.start + range.length - 1 });
        }
        return winrt::single_threaded_vector(std::move(ranges));
    }

    // Method Description:
    // - Returns true if we're in the middle of a tab rename. This is used to
    //   mitigate GH#10112.
    // Arguments:
    // - <none>
    // Return Value:
    // - true if the renamer is open.
    bool TabHeaderControl::InRename()
    {
        return Windows::UI::Xaml::Visibility::Visible == HeaderRenamerTextBox().Visibility();
    }

    bool TabHeaderControl::IsTitleHighlighted()
    {
        return !SearchText().empty() && !InRename();
    }

    void TabHeaderControl::SetSidebarTextStyles(const Windows::UI::Xaml::Style& titleStyle,
                                                const Windows::UI::Xaml::Style& metadataStyle)
    {
        _sidebarTitleTextStyle = titleStyle;
        _sidebarMetadataTextStyle = metadataStyle;
        HeaderTextBlock().Style(titleStyle);
        if (_highlightedTitle)
        {
            _highlightedTitle.TextBlockStyle(titleStyle);
        }
        if (_metadataTextBlock)
        {
            _metadataTextBlock.TextBlockStyle(metadataStyle);
            _metadataTextBlock.Margin({});
        }
    }

    void TabHeaderControl::_UpdateTitleVisibility()
    {
        if (_initialized)
        {
            HeaderTextBlock().Visibility(InRename() || IsTitleHighlighted() ?
                                             Windows::UI::Xaml::Visibility::Collapsed :
                                             Windows::UI::Xaml::Visibility::Visible);
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ L"IsTitleHighlighted" });
            // Bound x:Load emits an unavailable projected UnloadObject call; realize static stubs explicitly.
            if (IsTitleHighlighted())
            {
                if (!_highlightedTitle)
                {
                    _highlightedTitle = FindName(L"HeaderHighlightedTextBlock").as<TerminalApp::HighlightedTextControl>();
                    if (_sidebarTitleTextStyle)
                    {
                        _highlightedTitle.TextBlockStyle(_sidebarTitleTextStyle);
                    }
                }
            }
            else if (_highlightedTitle)
            {
                Windows::UI::Xaml::Markup::XamlMarkupHelper::UnloadObject(_highlightedTitle);
                _highlightedTitle = nullptr;
            }
        }
    }

    bool TabHeaderControl::IsMetadataVisible() const noexcept
    {
        return _isMetadataVisible;
    }

    void TabHeaderControl::IsMetadataVisible(const bool value)
    {
        if (_isMetadataVisible != value)
        {
            _isMetadataVisible = value;
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ L"IsMetadataVisible" });
        }
        _UpdateMetadataVisibility();
    }

    bool TabHeaderControl::ShowProgressRing() const noexcept
    {
        return _showProgressRing;
    }

    void TabHeaderControl::ShowProgressRing(const bool value)
    {
        if (_showProgressRing != value)
        {
            _showProgressRing = value;
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ L"ShowProgressRing" });
        }
    }

    void TabHeaderControl::_UpdateMetadataVisibility()
    {
        if (_isMetadataVisible)
        {
            if (!_metadataTextBlock)
            {
                _metadataTextBlock = FindName(L"HeaderMetadataTextBlock").as<TerminalApp::HighlightedTextControl>();
                if (_sidebarMetadataTextStyle)
                {
                    _metadataTextBlock.TextBlockStyle(_sidebarMetadataTextStyle);
                    _metadataTextBlock.Margin({});
                }
                _UpdateMetadataForeground();
            }
            _metadataTextBlock.Visibility(!InRename() ?
                                              Windows::UI::Xaml::Visibility::Visible :
                                              Windows::UI::Xaml::Visibility::Collapsed);
        }
        else if (_metadataTextBlock)
        {
            Windows::UI::Xaml::Markup::XamlMarkupHelper::UnloadObject(_metadataTextBlock);
            _metadataTextBlock = nullptr;
        }
    }

    void TabHeaderControl::SetMetadataForeground(const Windows::UI::Xaml::Media::Brush& foreground, const bool followTitle)
    {
        _metadataForeground = foreground;
        _metadataFollowsTitle = followTitle;
        _UpdateMetadataForeground();
    }

    void TabHeaderControl::_UpdateMetadataForeground()
    {
        if (!_metadataTextBlock)
        {
            return;
        }
        using namespace Windows::UI::Xaml;
        if (_metadataFollowsTitle)
        {
            Data::Binding binding;
            binding.Source(HeaderTextBlock());
            binding.Path(PropertyPath{ L"Foreground" });
            binding.Mode(Data::BindingMode::OneWay);
            _metadataTextBlock.SetBinding(Controls::Control::ForegroundProperty(), binding);
        }
        else if (_metadataForeground)
        {
            _metadataTextBlock.Foreground(_metadataForeground);
        }
        else
        {
            _metadataTextBlock.ClearValue(Controls::Control::ForegroundProperty());
        }
    }

    // Method Description:
    // - Show the tab rename box for the user to rename the tab title
    // - We automatically use the previous title as the initial text of the box
    void TabHeaderControl::BeginRename()
    {
        _receivedKeyDown = false;
        _renameCancelled = false;

        HeaderRenamerTextBox().Visibility(Windows::UI::Xaml::Visibility::Visible);
        _UpdateTitleVisibility();
        _UpdateMetadataVisibility();

        HeaderRenamerTextBox().Text(Title());
        HeaderRenamerTextBox().SelectAll();
        HeaderRenamerTextBox().Focus(Windows::UI::Xaml::FocusState::Programmatic);

        TraceLoggingWrite(
            g_hTerminalAppProvider, // handle to TerminalApp tracelogging provider
            "TabRenamerOpened",
            TraceLoggingDescription("Event emitted when the tab renamer is opened"),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));
    }

    void TabHeaderControl::CancelRename()
    {
        if (InRename())
        {
            _renameCancelled = true;
            _CloseRenameBox();
        }
    }

    // Method Description:
    // - Event handler for when the rename box loses focus
    // - When the rename box loses focus, we send a request for the title change depending
    //   on whether the rename was cancelled
    void TabHeaderControl::RenameBoxLostFocusHandler(const Windows::Foundation::IInspectable& /*sender*/,
                                                     const Windows::UI::Xaml::RoutedEventArgs& /*e*/)
    {
        // If the context menu associated with the renamer text box is open we know it gained the focus.
        // In this case we ignore this event (we will regain the focus once the menu will be closed).
        const auto flyout = HeaderRenamerTextBox().ContextFlyout();
        if (flyout && flyout.IsOpen())
        {
            return;
        }

        // Log the data here, rather than in _CloseRenameBox. If we do it there,
        // it'll get fired twice, once when the key is pressed to commit/cancel,
        // and then again when the focus is lost

        TraceLoggingWrite(
            g_hTerminalAppProvider, // handle to TerminalApp tracelogging provider
            "TabRenamerClosed",
            TraceLoggingDescription("Event emitted when the tab renamer is closed"),
            TraceLoggingBoolean(_renameCancelled, "CancelledRename", "True if the user cancelled the rename, false if they committed."),
            TraceLoggingKeyword(MICROSOFT_KEYWORD_MEASURES),
            TelemetryPrivacyDataTag(PDT_ProductAndServiceUsage));

        _CloseRenameBox();
        if (!_renameCancelled)
        {
            TitleChangeRequested.raise(HeaderRenamerTextBox().Text());
        }
    }

    // Method Description:
    // - Hides the rename box and displays the title text block
    void TabHeaderControl::_CloseRenameBox(const bool notify)
    {
        if (HeaderRenamerTextBox().Visibility() == Windows::UI::Xaml::Visibility::Visible)
        {
            HeaderRenamerTextBox().Visibility(Windows::UI::Xaml::Visibility::Collapsed);
            _UpdateTitleVisibility();
            _UpdateMetadataVisibility();
            if (notify)
            {
                RenameEnded.raise(*this, nullptr);
            }
        }
    }
}
