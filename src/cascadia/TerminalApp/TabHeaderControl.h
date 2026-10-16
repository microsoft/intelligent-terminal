// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "winrt/Microsoft.UI.Xaml.Controls.h"

#include "TabHeaderControl.g.h"
#include "TabHeaderPresentation.g.h"
#include "../RichTabProvider/ProviderBroker.h"

namespace winrt::TerminalApp::implementation
{
    struct TabHeaderPresentation : TabHeaderPresentationT<TabHeaderPresentation>
    {
        til::property_changed_event PropertyChanged;
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, Title, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, SearchText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(double, RenamerMaxWidth, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::TerminalApp::TerminalTabStatus, TabStatus, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::Foundation::IInspectable, IconSource, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, MetadataText, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, MetadataAutomationName, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::hstring, AutomationName, PropertyChanged.raise);
        WINRT_OBSERVABLE_PROPERTY(winrt::TerminalApp::TextHighlightColor, MetadataHighlightColor, PropertyChanged.raise, winrt::TerminalApp::TextHighlightColor::None);
        WINRT_OBSERVABLE_PROPERTY(winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>, MetadataHighlightRanges, PropertyChanged.raise, nullptr);
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
        winrt::Windows::UI::Xaml::Controls::Grid ProgressRingPresenter() const noexcept { return _progressRingPresenter; }
        static winrt::TerminalApp::TextHighlightColor AgentStatusHighlightColor(std::string_view status) noexcept;
        static winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun> BuildMetadataHighlightRanges(const ::Microsoft::Terminal::RichTab::Provider::Presentation& presentation);

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
        winrt::TerminalApp::TextHighlightColor MetadataHighlightColor() const { return _presentation.MetadataHighlightColor(); }
        void MetadataHighlightColor(winrt::TerminalApp::TextHighlightColor value) { _presentation.MetadataHighlightColor(value); }
        winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun> MetadataHighlightRanges() const { return _presentation.MetadataHighlightRanges(); }
        void MetadataHighlightRanges(const winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>& value) { _presentation.MetadataHighlightRanges(value); }
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
        // This view remains owned here when its visual parent is the native slot.
        winrt::Windows::UI::Xaml::Controls::Grid _progressRingPresenter{ nullptr };

        void _CloseRenameBox(bool notify = true);
        void _UpdateMetadataVisibility();
        void _AttachProgressToNativeIconSlot();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(TabHeaderControl);
}
