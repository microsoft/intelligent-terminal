// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TabHeaderControl.h"

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
            }
        });
        for (const auto name : { L"Presentation", L"Title", L"SearchText", L"RenamerMaxWidth", L"TabStatus", L"MetadataText", L"MetadataAutomationName" })
        {
            PropertyChanged.raise(*this, Windows::UI::Xaml::Data::PropertyChangedEventArgs{ name });
        }
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
        HeaderMetadataTextBlock().Visibility(_isMetadataVisible && !InRename() ?
                                                 Windows::UI::Xaml::Visibility::Visible :
                                                 Windows::UI::Xaml::Visibility::Collapsed);
    }

    // Method Description:
    // - Show the tab rename box for the user to rename the tab title
    // - We automatically use the previous title as the initial text of the box
    void TabHeaderControl::BeginRename()
    {
        _receivedKeyDown = false;
        _renameCancelled = false;

        HeaderTextBlock().Visibility(Windows::UI::Xaml::Visibility::Collapsed);
        HeaderRenamerTextBox().Visibility(Windows::UI::Xaml::Visibility::Visible);
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
            HeaderTextBlock().Visibility(Windows::UI::Xaml::Visibility::Visible);
            _UpdateMetadataVisibility();
            if (notify)
            {
                RenameEnded.raise(*this, nullptr);
            }
        }
    }
}
