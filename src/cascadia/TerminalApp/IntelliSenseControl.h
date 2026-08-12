// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "IntelliSenseControl.g.h"

namespace winrt::TerminalApp::implementation
{
    struct IntelliSenseControl : IntelliSenseControlT<IntelliSenseControl>
    {
        IntelliSenseControl();

        Windows::Foundation::Collections::IObservableVector<Microsoft::Terminal::Settings::Model::CompletionItem> Items() const noexcept;

        void Open(const Windows::Foundation::Collections::IVector<Microsoft::Terminal::Settings::Model::CompletionItem>& items,
                  Windows::Foundation::Point anchor,
                  Windows::Foundation::Size space,
                  float characterHeight);
        void Close();
        bool HandleKey(uint32_t vkey);

        bool OnDirectKeyEvent(uint32_t vkey, uint8_t scanCode, bool down);

        til::typed_event<winrt::TerminalApp::IntelliSenseControl, Microsoft::Terminal::Settings::Model::CompletionItem> CompletionRequested;

    private:
        friend struct IntelliSenseControlT<IntelliSenseControl>;

        void _previewKeyDownHandler(const Windows::Foundation::IInspectable& sender,
                                    const Windows::UI::Xaml::Input::KeyRoutedEventArgs& args);
        void _itemClicked(const Windows::Foundation::IInspectable& sender,
                          const Windows::UI::Xaml::Controls::ItemClickEventArgs& args);
        void _lostFocusHandler(const Windows::Foundation::IInspectable& sender,
                               const Windows::UI::Xaml::RoutedEventArgs& args);
        void _sizeChangedHandler(const Windows::Foundation::IInspectable& sender,
                                 const Windows::UI::Xaml::SizeChangedEventArgs& args);
        void _reposition(float contentHeight);
        bool _acceptSelected();
        void _selectRelative(int32_t delta);

        Windows::Foundation::Collections::IObservableVector<Microsoft::Terminal::Settings::Model::CompletionItem> _items;
        Windows::Foundation::Point _anchor{};
        Windows::Foundation::Size _availableSpace{};
        float _characterHeight{ 0 };
        float _estimatedHeight{ 0 };
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(IntelliSenseControl);
}
