// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace Microsoft::Terminal
{
    inline void BindAgentIconTheme(const winrt::Windows::UI::Xaml::FrameworkElement& element,
                                   const winrt::Windows::UI::Xaml::Media::Imaging::SvgImageSource& source)
    {
        const auto uri = source.UriSource().AbsoluteUri();
        winrt::hstring originalPath;
        for (const auto provider : { L"copilot", L"claude", L"codex", L"opencode" })
        {
            const auto path = winrt::hstring{ L"ms-appx:///AgentIcons/" } + provider;
            if (uri == path + L".svg" || uri == path + L"-light.svg")
            {
                originalPath = path;
                break;
            }
        }
        if (originalPath.empty())
        {
            return;
        }

        const auto update = [source, originalPath](const auto& sender, const auto&) {
            const auto theme = sender.template as<winrt::Windows::UI::Xaml::FrameworkElement>().ActualTheme();
            const auto path = originalPath + (theme == winrt::Windows::UI::Xaml::ElementTheme::Light ? L"-light.svg" : L".svg");
            if (source.UriSource().AbsoluteUri() != path)
            {
                source.UriSource(winrt::Windows::Foundation::Uri{ path });
            }
        };
        element.Loaded(update);
        element.ActualThemeChanged(update);
    }
}
