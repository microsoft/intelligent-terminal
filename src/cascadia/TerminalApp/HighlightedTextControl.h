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
        void SetTextAndColor(const winrt::hstring& text,
                             winrt::TerminalApp::TextHighlightColor color,
                             const winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>& ranges);

        DEPENDENCY_PROPERTY(winrt::hstring, Text);
        DEPENDENCY_PROPERTY(winrt::hstring, SearchText);
        DEPENDENCY_PROPERTY(winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>, HighlightedRuns);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Style, TextBlockStyle);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Style, HighlightedRunStyle);
        DEPENDENCY_PROPERTY(bool, UsePathEllipsis);
        DEPENDENCY_PROPERTY(winrt::TerminalApp::TextHighlightColor, ColorHighlight);
        DEPENDENCY_PROPERTY(winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>, ColorHighlightRanges);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, SuccessHighlightBrush);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CautionHighlightBrush);
        DEPENDENCY_PROPERTY(winrt::Windows::UI::Xaml::Media::Brush, CriticalHighlightBrush);
        DEPENDENCY_PROPERTY(double, TextOpacity);

    private:
        struct PathEllipsisResult
        {
            winrt::hstring text;
            std::vector<size_t> sourceToDisplay;
        };

        friend struct HighlightedTextControlPropertyRegistration;
        static void _InitializeProperties();
        static void _onPropertyChanged(const Windows::UI::Xaml::DependencyObject& o, const Windows::UI::Xaml::DependencyPropertyChangedEventArgs& e);
        PathEllipsisResult _pathEllipsisText(const winrt::hstring& text) const;
        std::vector<winrt::TerminalApp::HighlightedRun> _mapRuns(const winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>& runs) const;
        void _updateTextAndStyle();
        void _updateColorHighlights();
        winrt::Windows::UI::Xaml::Controls::TextBlock _textBlock{ nullptr };
        winrt::hstring _displayText;
        std::vector<size_t> _sourceToDisplay;
        winrt::Windows::UI::Xaml::Media::SolidColorBrush _foreground{ nullptr };
        winrt::Windows::UI::Xaml::FrameworkElement::SizeChanged_revoker _sizeChangedRevoker;
        int64_t _foregroundToken{};
        int64_t _colorToken{};
        int64_t _opacityToken{};
        bool _updatingTextAndColor{ false };
        void _observeForeground();
    };
}

namespace winrt::TerminalApp::factory_implementation
{
    BASIC_FACTORY(HighlightedTextControl);
}
