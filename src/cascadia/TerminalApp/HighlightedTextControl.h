// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "HighlightedTextControl.g.h"

namespace winrt::TerminalApp::implementation
{
    struct HighlightedTextControlPropertyRegistration
    {
        HighlightedTextControlPropertyRegistration();
    };

    struct HighlightedTextControl : private HighlightedTextControlPropertyRegistration, HighlightedTextControlT<HighlightedTextControl>
    {
        HighlightedTextControl();
        ~HighlightedTextControl();

        void OnApplyTemplate();

        DEPENDENCY_PROPERTY(winrt::hstring, Text);
        DEPENDENCY_PROPERTY(winrt::hstring, SearchText);
        DEPENDENCY_PROPERTY(winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>, HighlightedRuns);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Style, TextBlockStyle);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Style, HighlightedRunStyle);
        DEPENDENCY_PROPERTY(winrt::TerminalApp::TextHighlightColor, ColorHighlight);
        DEPENDENCY_PROPERTY(winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>, ColorHighlightRanges);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, SuccessHighlightBrush);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CautionHighlightBrush);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CriticalHighlightBrush);
        DEPENDENCY_PROPERTY(double, TextOpacity);

    private:
        friend struct HighlightedTextControlPropertyRegistration;
        static void _InitializeProperties();
        static void _onPropertyChanged(const Windows::UI::Xaml::DependencyObject& o, const Windows::UI::Xaml::DependencyPropertyChangedEventArgs& e);
        void _updateTextAndStyle();
        void _updateColorHighlights();
        winrt::Windows::UI::Xaml::Controls::TextBlock _textBlock{ nullptr };
        winrt::Windows::UI::Xaml::Media::SolidColorBrush _foreground{ nullptr };
        int64_t _foregroundToken{};
        int64_t _colorToken{};
        int64_t _opacityToken{};
        void _observeForeground();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(HighlightedTextControl);
}
