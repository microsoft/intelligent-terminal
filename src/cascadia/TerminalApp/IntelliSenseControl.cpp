// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "IntelliSenseControl.h"
#include "IntelliSenseControl.g.cpp"

using namespace winrt;
using namespace winrt::Microsoft::Terminal::Settings::Model;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Windows::System;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::UI::Xaml::Controls;
using namespace winrt::Windows::UI::Xaml::Media;

namespace winrt::TerminalApp::implementation
{
    IntelliSenseControl::IntelliSenseControl() :
        _items{ single_threaded_observable_vector<CompletionItem>() }
    {
        InitializeComponent();
    }

    IObservableVector<CompletionItem> IntelliSenseControl::Items() const noexcept
    {
        return _items;
    }

    void IntelliSenseControl::Open(const IVector<CompletionItem>& items,
                                   Point anchor,
                                   Size space,
                                   float characterHeight)
    {
        _items.Clear();
        for (const auto& item : items)
        {
            _items.Append(item);
        }

        if (_items.Size() == 0)
        {
            Close();
            return;
        }

        _anchor = anchor;
        _availableSpace = space;
        _characterHeight = characterHeight;
        constexpr float ItemHeight = 36.0f;
        constexpr float BorderPadding = 8.0f;
        constexpr float MaximumHeight = 320.0f;
        const auto availableHeight = std::max(0.0f, space.Height);
        MaxHeight(std::min(MaximumHeight, availableHeight));
        _estimatedHeight = std::min(static_cast<float>(MaxHeight()), BorderPadding + ItemHeight * _items.Size());

        Visibility(Visibility::Visible);
        UpdateLayout();
        _reposition(std::max(_estimatedHeight, static_cast<float>(ActualHeight())));

        _itemsView().SelectedIndex(-1);
    }

    void IntelliSenseControl::Close()
    {
        Visibility(Visibility::Collapsed);
        _items.Clear();
        _estimatedHeight = 0;
    }

    void IntelliSenseControl::_reposition(const float contentHeight)
    {
        const auto width = std::max(300.0f, static_cast<float>(ActualWidth()));
        const auto x = std::clamp(_anchor.X - 24.0f, 0.0f, std::max(0.0f, _availableSpace.Width - width));
        const auto openBelow = _anchor.Y + _characterHeight + contentHeight <= _availableSpace.Height;
        const auto y = openBelow ?
                           _anchor.Y + _characterHeight :
                           std::max(0.0f, _anchor.Y - contentHeight);
        Margin(ThicknessHelper::FromLengths(x, y, 0, 0));
    }

    void IntelliSenseControl::_sizeChangedHandler(const IInspectable& /*sender*/, const SizeChangedEventArgs& args)
    {
        if (Visibility() == Visibility::Visible)
        {
            _reposition(std::max(_estimatedHeight, args.NewSize().Height));
        }
    }

    bool IntelliSenseControl::OnDirectKeyEvent(uint32_t /*vkey*/, uint8_t /*scanCode*/, bool /*down*/)
    {
        return false;
    }

    void IntelliSenseControl::_selectRelative(int32_t delta)
    {
        const auto count = static_cast<int32_t>(_items.Size());
        if (count == 0)
        {
            return;
        }

        const auto selected = _itemsView().SelectedIndex();
        const auto next = selected < 0 ?
                              (delta > 0 ? 0 : count - 1) :
                              (selected + delta + count) % count;
        _itemsView().SelectedIndex(next);
        _itemsView().ScrollIntoView(_itemsView().SelectedItem());
    }

    bool IntelliSenseControl::_acceptSelected()
    {
        if (const auto item = _itemsView().SelectedItem().try_as<CompletionItem>())
        {
            Close();
            CompletionRequested.raise(*this, item);
            return true;
        }
        return false;
    }

    bool IntelliSenseControl::HandleKey(const uint32_t vkey)
    {
        if (Visibility() != Visibility::Visible)
        {
            return false;
        }

        switch (vkey)
        {
        case VK_UP:
            _selectRelative(-1);
            return true;
        case VK_DOWN:
            _selectRelative(1);
            return true;
        case VK_RETURN:
        case VK_TAB:
            if (_acceptSelected())
            {
                return true;
            }
            Close();
            return false;
        case VK_ESCAPE:
            Close();
            return true;
        case VK_RIGHT:
        default:
            Close();
            return false;
        }
    }

    void IntelliSenseControl::_previewKeyDownHandler(const IInspectable& /*sender*/, const Windows::UI::Xaml::Input::KeyRoutedEventArgs& args)
    {
        args.Handled(HandleKey(static_cast<uint32_t>(args.OriginalKey())));
    }

    void IntelliSenseControl::_itemClicked(const IInspectable& /*sender*/, const ItemClickEventArgs& args)
    {
        if (const auto item = args.ClickedItem().try_as<CompletionItem>())
        {
            Close();
            CompletionRequested.raise(*this, item);
        }
    }

    void IntelliSenseControl::_lostFocusHandler(const IInspectable& /*sender*/, const RoutedEventArgs& /*args*/)
    {
        const auto root = XamlRoot();
        if (!root)
        {
            return;
        }

        auto focused = Windows::UI::Xaml::Input::FocusManager::GetFocusedElement(root).try_as<DependencyObject>();
        while (focused)
        {
            if (focused == *this)
            {
                return;
            }
            focused = VisualTreeHelper::GetParent(focused);
        }
        Close();
    }
}
