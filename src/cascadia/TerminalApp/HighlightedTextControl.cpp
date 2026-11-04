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
    DependencyProperty HighlightedTextControl::_UsePathEllipsisProperty{ nullptr };
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

            _UsePathEllipsisProperty = DependencyProperty::Register(
                L"UsePathEllipsis",
                xaml_typename<bool>(),
                xaml_typename<winrt::TerminalApp::HighlightedTextControl>(),
                PropertyMetadata(box_value(false), HighlightedTextControl::_onPropertyChanged));

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

    void HighlightedTextControl::SetTextAndColor(
        const winrt::hstring& text,
        const TerminalApp::TextHighlightColor color,
        const IVector<TerminalApp::HighlightedRun>& ranges)
    {
        if (Text() == text && ColorHighlight() == color && ColorHighlightRanges() == ranges)
        {
            return;
        }
        {
            _updatingTextAndColor = true;
            const auto reset = wil::scope_exit([this]() { _updatingTextAndColor = false; });
            Text(text);
            ColorHighlightRanges(ranges);
            ColorHighlight(color);
        }
        _updateTextAndStyle();
    }

    void HighlightedTextControl::OnApplyTemplate()
    {
        _sizeChangedRevoker.revoke();
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
            _sizeChangedRevoker = SizeChanged(auto_revoke, [weak = get_weak()](auto&&, const SizeChangedEventArgs& args) {
                if (const auto self = weak.get())
                {
                    if (self->UsePathEllipsis() && args.NewSize().Width != args.PreviousSize().Width)
                    {
                        self->_updateTextAndStyle();
                    }
                }
            });
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

    static bool _isPathSeparator(const wchar_t value) noexcept
    {
        return value == L'\\' || value == L'/';
    }

    static std::vector<TerminalApp::HighlightedRun> _searchRuns(const std::wstring_view text, const std::wstring_view query)
    {
        std::vector<TerminalApp::HighlightedRun> runs;
        if (!query.empty() && query.size() <= text.size())
        {
            for (size_t offset = 0; offset + query.size() <= text.size();)
            {
                if (til::compare_ordinal_insensitive(text.substr(offset, query.size()), query) == 0)
                {
                    runs.emplace_back(offset, offset + query.size() - 1);
                    offset += query.size();
                }
                else
                {
                    ++offset;
                }
            }
        }
        return runs;
    }

    static std::vector<size_t> _identityOffsets(const size_t size)
    {
        std::vector<size_t> offsets(size + 1);
        for (size_t index = 0; index < offsets.size(); ++index)
        {
            offsets[index] = index;
        }
        return offsets;
    }

    HighlightedTextControl::PathEllipsisResult HighlightedTextControl::_pathEllipsisText(const winrt::hstring& text) const
    {
        PathEllipsisResult result{ text, _identityOffsets(text.size()) };
        const auto width = ActualWidth();
        if (text.empty() || !_textBlock || width <= 0)
        {
            return result;
        }

        Controls::TextBlock probe;
        probe.FontFamily(_textBlock.FontFamily());
        probe.FontSize(_textBlock.FontSize());
        probe.FontStretch(_textBlock.FontStretch());
        probe.FontStyle(_textBlock.FontStyle());
        probe.FontWeight(_textBlock.FontWeight());
        probe.CharacterSpacing(_textBlock.CharacterSpacing());
        probe.FlowDirection(_textBlock.FlowDirection());
        probe.IsTextScaleFactorEnabled(_textBlock.IsTextScaleFactorEnabled());
        probe.Language(_textBlock.Language());
        probe.TextWrapping(TextWrapping::NoWrap);
        const auto unconstrained = std::numeric_limits<float>::infinity();
        const auto fits = [&](const winrt::hstring& value) {
            const auto inlines = probe.Inlines();
            inlines.Clear();
            const auto explicitRuns = HighlightedRuns();
            const auto searchText = SearchText();
            const auto runs = explicitRuns && explicitRuns.Size() && !value.empty() ?
                                  std::vector<TerminalApp::HighlightedRun>{ TerminalApp::HighlightedRun{ 0, value.size() - 1 } } :
                                  _searchRuns(std::wstring_view{ value }, std::wstring_view{ searchText });
            size_t lastPos = 0;
            const auto appendRun = [&](const size_t start, const size_t end, const bool highlighted) {
                if (end <= start)
                {
                    return;
                }
                Documents::Run run;
                run.Text(hstring{ til::safe_slice_abs(value, start, end) });
                if (highlighted)
                {
                    if (const auto runStyle = HighlightedRunStyle())
                    {
                        _applyStyleToObject(runStyle, run);
                    }
                    else
                    {
                        run.FontWeight(FontWeights::Bold());
                    }
                }
                inlines.Append(run);
            };
            for (const auto& run : runs)
            {
                appendRun(lastPos, run.Start, false);
                appendRun(run.Start, run.End + 1, true);
                lastPos = run.End + 1;
            }
            appendRun(lastPos, value.size(), false);
            probe.Measure({ unconstrained, unconstrained });
            return probe.DesiredSize().Width <= width - 0.5;
        };

        const auto compactLine = [&](const std::wstring_view path) {
            PathEllipsisResult lineResult{ hstring{ path }, _identityOffsets(path.size()) };
            if (fits(lineResult.text))
            {
                return lineResult;
            }

            wchar_t separator = L'\\';
            size_t contentStart = 0;
            bool windowsSeparators = true;
            if (path.size() >= 3 && path[1] == L':' && _isPathSeparator(path[2]))
            {
                separator = path[2];
                contentStart = 3;
            }
            else if (path.size() >= 2 && _isPathSeparator(path[0]) && _isPathSeparator(path[1]))
            {
                separator = path[0];
                const auto serverEnd = path.find_first_of(L"\\/", 2);
                const auto shareEnd = serverEnd == std::wstring_view::npos ?
                                          std::wstring_view::npos :
                                          path.find_first_of(L"\\/", serverEnd + 1);
                if (shareEnd != std::wstring_view::npos)
                {
                    contentStart = shareEnd + 1;
                }
            }
            else if (!path.empty() && path[0] == L'\\')
            {
                contentStart = 1;
            }
            else if (!path.empty() && path[0] == L'/')
            {
                separator = L'/';
                contentStart = 1;
                windowsSeparators = false;
            }
            else if (path.size() >= 2 && path[0] == L'~' && path[1] == L'\\')
            {
                contentStart = 2;
            }
            else if (path.size() >= 2 && path[0] == L'~' && path[1] == L'/')
            {
                separator = L'/';
                contentStart = 2;
                windowsSeparators = false;
            }
            if (contentStart == 0)
            {
                return lineResult;
            }

            const auto isSeparator = [windowsSeparators](const wchar_t value) noexcept {
                return value == L'/' || (windowsSeparators && value == L'\\');
            };
            std::vector<size_t> componentStarts;
            for (auto position = contentStart; position < path.size();)
            {
                while (position < path.size() && isSeparator(path[position]))
                {
                    ++position;
                }
                if (position < path.size())
                {
                    componentStarts.emplace_back(position);
                    while (position < path.size() && !isSeparator(path[position]))
                    {
                        ++position;
                    }
                }
            }
            if (componentStarts.size() < 2)
            {
                return lineResult;
            }

            const auto anchor = path.substr(0, contentStart);
            for (size_t firstComponent = 1; firstComponent < componentStarts.size(); ++firstComponent)
            {
                const auto suffixStart = componentStarts[firstComponent];
                std::wstring candidate{ anchor };
                candidate.push_back(L'\u2026');
                candidate.push_back(separator);
                candidate.append(path.substr(suffixStart));
                const hstring candidateText{ candidate };
                const auto isLastCandidate = firstComponent + 1 == componentStarts.size();
                if (fits(candidateText) || isLastCandidate)
                {
                    lineResult.text = candidateText;
                    const auto ellipsisStart = contentStart;
                    std::fill(lineResult.sourceToDisplay.begin() + contentStart,
                              lineResult.sourceToDisplay.begin() + suffixStart,
                              ellipsisStart);
                    const auto suffixDisplayStart = contentStart + 2;
                    for (auto sourceOffset = suffixStart; sourceOffset <= path.size(); ++sourceOffset)
                    {
                        lineResult.sourceToDisplay[sourceOffset] = suffixDisplayStart + sourceOffset - suffixStart;
                    }
                    return lineResult;
                }
            }
            return lineResult;
        };

        std::wstring displayText;
        displayText.reserve(text.size());
        const std::wstring_view source{ text };
        for (size_t lineStart = 0; lineStart < source.size();)
        {
            const auto lineEnd = source.find_first_of(L"\r\n", lineStart);
            const auto contentEnd = lineEnd == std::wstring_view::npos ? source.size() : lineEnd;
            const auto line = compactLine(source.substr(lineStart, contentEnd - lineStart));
            const auto displayStart = displayText.size();
            displayText.append(line.text);
            for (size_t offset = 0; offset <= contentEnd - lineStart; ++offset)
            {
                result.sourceToDisplay[lineStart + offset] = displayStart + line.sourceToDisplay[offset];
            }
            if (lineEnd == std::wstring_view::npos)
            {
                break;
            }

            auto newlineEnd = lineEnd + 1;
            if (source[lineEnd] == L'\r' && newlineEnd < source.size() && source[newlineEnd] == L'\n')
            {
                ++newlineEnd;
            }
            for (auto offset = lineEnd; offset < newlineEnd; ++offset)
            {
                result.sourceToDisplay[offset] = displayText.size();
                displayText.push_back(source[offset]);
                result.sourceToDisplay[offset + 1] = displayText.size();
            }
            lineStart = newlineEnd;
        }
        result.text = hstring{ displayText };
        return result;
    }

    std::vector<TerminalApp::HighlightedRun> HighlightedTextControl::_mapRuns(const IVector<TerminalApp::HighlightedRun>& runs) const
    {
        std::vector<TerminalApp::HighlightedRun> mapped;
        if (!runs)
        {
            return mapped;
        }
        for (const auto& run : runs)
        {
            THROW_HR_IF(E_INVALIDARG, run.End < run.Start);
            if (run.Start + 1 < _sourceToDisplay.size())
            {
                const auto end = std::min<size_t>(run.End + 1, _sourceToDisplay.size() - 1);
                const auto start = _sourceToDisplay[run.Start];
                const auto endExclusive = _sourceToDisplay[end];
                if (endExclusive > start)
                {
                    mapped.emplace_back(start, endExclusive - 1);
                }
            }
        }
        return mapped;
    }

    void HighlightedTextControl::_updateTextAndStyle()
    {
        if (_updatingTextAndColor)
        {
            return;
        }
        const auto textBlock = GetTemplateChild(L"TextView").try_as<winrt::Windows::UI::Xaml::Controls::TextBlock>();
        if (!textBlock)
        {
            return;
        }

        const auto sourceText = Text();
        auto pathResult = UsePathEllipsis() ? _pathEllipsisText(sourceText) : PathEllipsisResult{ sourceText, _identityOffsets(sourceText.size()) };
        _displayText = std::move(pathResult.text);
        _sourceToDisplay = std::move(pathResult.sourceToDisplay);
        const auto& text = _displayText;
        const auto runs = HighlightedRuns();
        const auto mappedRuns = _mapRuns(runs);
        std::vector<TerminalApp::HighlightedRun> searchRuns;
        if ((!runs || runs.Size() == 0) && !text.empty())
        {
            const auto searchText = SearchText();
            searchRuns = _searchRuns(std::wstring_view{ text }, std::wstring_view{ searchText });
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

            if (!mappedRuns.empty())
            {
                for (const auto& highlightedRun : mappedRuns)
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
        if (_updatingTextAndColor || !_textBlock)
        {
            return;
        }
        const auto& text = _displayText;
        const auto highlights = _textBlock.TextHighlighters();
        highlights.Clear();
        if (TextOpacity() != 1.0 && _foreground && !text.empty())
        {
            // Keep inherited/custom foreground intact; only neutral glyphs are muted.
            Media::SolidColorBrush brush;
            Data::Binding colorBinding;
            colorBinding.Source(_textBlock);
            colorBinding.Path(PropertyPath{ L"Foreground.Color" });
            colorBinding.Mode(Data::BindingMode::OneWay);
            Data::BindingOperations::SetBinding(brush, Media::SolidColorBrush::ColorProperty(), colorBinding);
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
            for (const auto& range : _mapRuns(colorRanges))
            {
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
