// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "winrt/Windows.UI.Xaml.Interop.h"
#include "HighlightedTextControl.h"

#include "HighlightedTextControl.g.cpp"

using namespace winrt;
using namespace winrt::TerminalApp;
using namespace winrt::Windows::UI::Core;
using namespace winrt::Windows::UI::Xaml;
using namespace winrt::Windows::System;
using namespace winrt::Windows::UI::Text;
using namespace winrt::Windows::Foundation;
using namespace winrt::Windows::Foundation::Collections;
using namespace winrt::Microsoft::Terminal::Settings::Model;

namespace winrt::TerminalApp::implementation
{
    // Our control exposes a "Text" property to be used with Data Binding
    // To allow this we need to register a Dependency Property Identifier to be used by the property system
    // (https://docs.microsoft.com/en-us/windows/uwp/xaml-platform/custom-dependency-properties)
    DependencyProperty HighlightedTextControl::_TextProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_SearchTextProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_HighlightedRunsProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_TextBlockStyleProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_HighlightedRunStyleProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_ColorHighlightProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_ColorHighlightRangesProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_SuccessHighlightBrushProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_CautionHighlightBrushProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_CriticalHighlightBrushProperty{ nullptr };
    DependencyProperty HighlightedTextControl::_TextOpacityProperty{ nullptr };

    HighlightedTextControlPropertyRegistration::HighlightedTextControlPropertyRegistration()
    {
        // ThemeResource expressions can be assigned during Control base construction.
        HighlightedTextControl::_InitializeProperties();
    }

    HighlightedTextControl::HighlightedTextControl() = default;

    HighlightedTextControl::~HighlightedTextControl()
    {
        try
        {
            if (_textBlock)
            {
                _textBlock.UnregisterPropertyChangedCallback(Controls::TextBlock::ForegroundProperty(), _foregroundToken);
            }
            if (_foreground)
            {
                _foreground.UnregisterPropertyChangedCallback(Media::SolidColorBrush::ColorProperty(), _colorToken);
                _foreground.UnregisterPropertyChangedCallback(Media::Brush::OpacityProperty(), _opacityToken);
            }
        }
        CATCH_LOG();
    }

    void HighlightedTextControl::_observeForeground()
    {
        if (_foreground)
        {
            _foreground.UnregisterPropertyChangedCallback(Media::SolidColorBrush::ColorProperty(), _colorToken);
            _foreground.UnregisterPropertyChangedCallback(Media::Brush::OpacityProperty(), _opacityToken);
        }
        _foreground = _textBlock.Foreground().try_as<Media::SolidColorBrush>();
        if (_foreground)
        {
            const auto changed = [weak = get_weak()](auto&&, auto&&) {
                if (const auto self = weak.get())
                {
                    self->_updateColorHighlights();
                }
            };
            _colorToken = _foreground.RegisterPropertyChangedCallback(Media::SolidColorBrush::ColorProperty(), changed);
            _opacityToken = _foreground.RegisterPropertyChangedCallback(Media::Brush::OpacityProperty(), changed);
        }
    }

    void HighlightedTextControl::_InitializeProperties()
    {
        static auto [[maybe_unused]] registered = [] {
            _TextProperty = DependencyProperty::Register(
                L"Text",
                xaml_typename<winrt::hstring>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _SearchTextProperty = DependencyProperty::Register(
                L"SearchText",
                xaml_typename<winrt::hstring>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _HighlightedRunsProperty = DependencyProperty::Register(
                L"HighlightedRuns",
                xaml_typename<winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _TextBlockStyleProperty = DependencyProperty::Register(
                L"TextBlockStyle",
                xaml_typename<winrt::Windows::UI::Xaml::Style>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata{ nullptr });

            _HighlightedRunStyleProperty = DependencyProperty::Register(
                L"HighlightedRunStyle",
                xaml_typename<winrt::Windows::UI::Xaml::Style>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _ColorHighlightProperty = DependencyProperty::Register(
                L"ColorHighlight",
                xaml_typename<winrt::TerminalApp::TextHighlightColor>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(box_value(winrt::TerminalApp::TextHighlightColor::None), HighlightedTextControl::_onPropertyChanged));

            _ColorHighlightRangesProperty = DependencyProperty::Register(
                L"ColorHighlightRanges",
                xaml_typename<winrt::Windows::Foundation::Collections::IVector<winrt::TerminalApp::HighlightedRun>>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _SuccessHighlightBrushProperty = DependencyProperty::Register(
                L"SuccessHighlightBrush",
                xaml_typename<winrt::Windows::UI::Xaml::Media::Brush>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _CautionHighlightBrushProperty = DependencyProperty::Register(
                L"CautionHighlightBrush",
                xaml_typename<winrt::Windows::UI::Xaml::Media::Brush>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _CriticalHighlightBrushProperty = DependencyProperty::Register(
                L"CriticalHighlightBrush",
                xaml_typename<winrt::Windows::UI::Xaml::Media::Brush>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(nullptr, HighlightedTextControl::_onPropertyChanged));

            _TextOpacityProperty = DependencyProperty::Register(
                L"TextOpacity",
                xaml_typename<double>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(box_value(1.0), HighlightedTextControl::_onPropertyChanged));

            return true;
        }();
    }

    void HighlightedTextControl::_onPropertyChanged(const DependencyObject& o, const DependencyPropertyChangedEventArgs& /*e*/)
    {
        const auto control = o.try_as<winrt::TerminalApp::HighlightedTextControl>();
        if (control)
        {
            winrt::get_self<HighlightedTextControl>(control)->_updateTextAndStyle();
        }
    }

    void HighlightedTextControl::OnApplyTemplate()
    {
        if (_textBlock)
        {
            _textBlock.UnregisterPropertyChangedCallback(Controls::TextBlock::ForegroundProperty(), _foregroundToken);
        }
        _textBlock = GetTemplateChild(L"TextView").try_as<Controls::TextBlock>();
        if (_textBlock)
        {
            _foregroundToken = _textBlock.RegisterPropertyChangedCallback(Controls::TextBlock::ForegroundProperty(), [weak = get_weak()](auto&&, auto&&) {
                if (const auto self = weak.get())
                {
                    self->_observeForeground();
                    self->_updateColorHighlights();
                }
            });
            _observeForeground();
        }
        _updateTextAndStyle();
    }

    static void _applyStyleToObject(const winrt::Windows::UI::Xaml::Style& style, const winrt::Windows::UI::Xaml::DependencyObject& object)
    {
        if (!style)
        {
            return;
        }

        static const auto fontWeightProperty{ winrt::Windows::UI::Xaml::Documents::TextElement::FontWeightProperty() };

        const auto setters{ style.Setters() };
        for (auto&& setterBase : setters)
        {
            const auto setter = setterBase.as<winrt::Windows::UI::Xaml::Setter>();
            const auto property = setter.Property();
            auto value = setter.Value();

            if (property == fontWeightProperty) [[unlikely]]
            {
                // BODGY - The XAML compiler emits a boxed int32, but the dependency property
                // here expects a boxed FontWeight (which also requires a u16. heh.)
                // FontWeight is one of the few properties that is broken like this, and on Run it's the
                // only one... so we can trivially check this case.
                const auto weight{ winrt::unbox_value_or<int32_t>(value, static_cast<int32_t>(400)) };
                value = winrt::box_value(winrt::Windows::UI::Text::FontWeight{ static_cast<uint16_t>(weight) });
            }

            object.SetValue(property, value);
        }
    }

    void HighlightedTextControl::_updateTextAndStyle()
    {
        const auto textBlock = GetTemplateChild(L"TextView").try_as<winrt::Windows::UI::Xaml::Controls::TextBlock>();
        if (!textBlock)
        {
            return;
        }

        const auto text = Text();
        const auto runs = HighlightedRuns();
        std::vector<TerminalApp::HighlightedRun> searchRuns;
        if ((!runs || runs.Size() == 0) && !text.empty())
        {
            const auto searchText = SearchText();
            const std::wstring_view query{ searchText.c_str(), searchText.size() };
            const std::wstring_view candidate{ text.c_str(), text.size() };
            if (!query.empty() && query.size() <= candidate.size())
            {
                for (size_t offset = 0; offset + query.size() <= candidate.size();)
                {
                    if (til::compare_ordinal_insensitive(candidate.substr(offset, query.size()), query) == 0)
                    {
                        searchRuns.emplace_back(offset, offset + query.size() - 1);
                        offset += query.size();
                    }
                    else
                    {
                        ++offset;
                    }
                }
            }
        }

        const auto inlinesCollection = textBlock.Inlines();
        inlinesCollection.Clear();

        // The code below constructs local hstring instances because hstring is required to be null-terminated
        // and slicing _does not_ guarantee null termination. Passing a sliced wstring_view directly into run.Text()
        // (which is a winrt::param::hstring--different thing!--will result in an exception when the sliced portion
        // is not null-terminated.
        if (!text.empty())
        {
            size_t lastPos = 0;
            const auto appendHighlightedRun = [&](const TerminalApp::HighlightedRun& highlightedRun) {
                const auto [start, end] = highlightedRun;
                if (start > lastPos)
                {
                    const hstring nonMatch{ til::safe_slice_abs(text, lastPos, static_cast<size_t>(start)) };
                    Documents::Run run;
                    run.Text(nonMatch);
                    inlinesCollection.Append(run);
                }

                const hstring matchSeg{ til::safe_slice_abs(text, static_cast<size_t>(start), static_cast<size_t>(end + 1)) };
                Documents::Run run;
                run.Text(matchSeg);

                if (const auto runStyle = HighlightedRunStyle())
                {
                    _applyStyleToObject(runStyle, run);
                }
                else
                {
                    run.FontWeight(FontWeights::Bold());
                }
                inlinesCollection.Append(run);

                lastPos = static_cast<size_t>(end + 1);
            };

            if (runs && runs.Size())
            {
                for (const auto& highlightedRun : runs)
                {
                    appendHighlightedRun(highlightedRun);
                }
            }
            else
            {
                for (const auto& highlightedRun : searchRuns)
                {
                    appendHighlightedRun(highlightedRun);
                }
            }

            // This will also be true if there are no runs at all
            if (lastPos < text.size())
            {
                // checking lastPos here prevents a needless deep copy of the whole text in the no-match case
                const hstring tail{ lastPos == 0 ? text : hstring{ til::safe_slice_abs(text, lastPos, SIZE_T_MAX) } };
                Documents::Run run;
                run.Text(tail);
                inlinesCollection.Append(run);
            }
        }

        _updateColorHighlights();
    }

    void HighlightedTextControl::_updateColorHighlights()
    {
        if (!_textBlock)
        {
            return;
        }
        const auto text = Text();
        const auto highlights = _textBlock.TextHighlighters();
        highlights.Clear();
        if (TextOpacity() != 1.0 && _foreground && !text.empty())
        {
            // Keep inherited/custom foreground intact; only neutral glyphs are muted.
            Media::SolidColorBrush brush{ _foreground.Color() };
            brush.Opacity(_foreground.Opacity() * TextOpacity());
            Documents::TextHighlighter neutral;
            neutral.Foreground(brush);
            neutral.Background(Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
            neutral.Ranges().Append(Documents::TextRange{ 0, gsl::narrow<int32_t>(text.size()) });
            highlights.Append(neutral);
        }
        const auto color = ColorHighlight();
        const auto colorRanges = ColorHighlightRanges();
        if (color != TerminalApp::TextHighlightColor::None && colorRanges && !text.empty())
        {
            Media::Brush brush{ nullptr };
            switch (color)
            {
            case TerminalApp::TextHighlightColor::Success:
                brush = SuccessHighlightBrush();
                break;
            case TerminalApp::TextHighlightColor::Caution:
                brush = CautionHighlightBrush();
                break;
            case TerminalApp::TextHighlightColor::Critical:
                brush = CriticalHighlightBrush();
                break;
            default:
                THROW_HR(E_INVALIDARG);
            }
            // Theme brushes can arrive separately; their changes rebuild the highlight.
            if (!brush)
            {
                return;
            }
            Documents::TextHighlighter highlighter;
            highlighter.Foreground(brush);
            highlighter.Background(Media::SolidColorBrush{ Windows::UI::Colors::Transparent() });
            const auto ranges = highlighter.Ranges();
            for (const auto& range : colorRanges)
            {
                THROW_HR_IF(E_INVALIDARG, range.End < range.Start);
                if (range.Start < text.size())
                {
                    const auto end = std::min<uint64_t>(range.End, text.size() - 1);
                    ranges.Append(Documents::TextRange{
                        gsl::narrow<int32_t>(range.Start),
                        gsl::narrow<int32_t>(end - range.Start + 1) });
                }
            }
            if (ranges.Size() != 0)
            {
                highlights.Append(highlighter);
            }
        }
    }
}
