// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"

#include "TabHeaderControl.g.h"
#include "TabHeaderPresentation.g.h"

namespace winrt::TerminalApp::implementation
{
    struct TabHeaderPresentation : TabHeaderPresentationT<TabHeaderPresentation>
    {
        til::property_changed_event PropertyChanged;
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Title, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SearchText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(double, RenamerMaxWidth, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::TerminalApp::TerminalTabStatus, TabStatus, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, MetadataText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, MetadataAutomationName, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, AutomationName, PropertyChanged.raise);
    };

    struct TabHeaderControl : TabHeaderControlT<TabHeaderControl>
    {
        TabHeaderControl();
        void BeginRename();
        void CancelRename();

        void RenameBoxLostFocusHandler(const winrt::Windows::Foundation::IInspectable& sender,
                                       const winrt::Windows::UI::Xaml::RoutedEventArgs& e);

        bool InRename();
        bool IsMetadataVisible() const noexcept;
        void IsMetadataVisible(bool value);
        bool ShowProgressRing() const noexcept;
        void ShowProgressRing(bool value);

        til::event<TerminalApp::TitleChangeRequestedArgs> TitleChangeRequested;
        til::typed_event<> RenameEnded;
        bool SidebarEventsAttached{ false };

        til::property_changed_event PropertyChanged;
        winrt::TerminalApp::TabHeaderPresentation Presentation() const noexcept { return _presentation; }
        void Presentation(const winrt::TerminalApp::TabHeaderPresentation& value);
        winrt::hstring Title() const { return _presentation.Title(); }
        void Title(const winrt::hstring& value) { _presentation.Title(value); }
        winrt::hstring SearchText() const { return _presentation.SearchText(); }
        void SearchText(const winrt::hstring& value) { _presentation.SearchText(value); }
        double RenamerMaxWidth() const { return _presentation.RenamerMaxWidth(); }
        void RenamerMaxWidth(double value) { _presentation.RenamerMaxWidth(value); }
        winrt::hstring MetadataText() const { return _presentation.MetadataText(); }
        void MetadataText(const winrt::hstring& value) { _presentation.MetadataText(value); }
        winrt::hstring MetadataAutomationName() const { return _presentation.MetadataAutomationName(); }
        void MetadataAutomationName(const winrt::hstring& value) { _presentation.MetadataAutomationName(value); }
        winrt::TerminalApp::TerminalTabStatus TabStatus() const { return _presentation.TabStatus(); }
        void TabStatus(const winrt::TerminalApp::TerminalTabStatus& value) { _presentation.TabStatus(value); }
        WINRT_OBSERVABLE_PROPERTY(bool, ShowPinnedIcon, PropertyChanged.raise, true);

    private:
        bool _receivedKeyDown{ false };
        bool _renameCancelled{ false };
        bool _isMetadataVisible{ false };
        bool _showProgressRing{ true };
        winrt::TerminalApp::TabHeaderPresentation _presentation{ nullptr };
        winrt::Windows::UI::Xaml::Data::INotifyPropertyChanged::PropertyChanged_revoker _presentationChanged;

        void _CloseRenameBox(bool notify = true);
        void _UpdateMetadataVisibility();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabHeaderControl);
}
