// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "../inc/AgentRegistry.h"

namespace Microsoft::Terminal::UI::AgentIcons
{
    inline winrt::Windows::UI::Xaml::ResourceDictionary& Resources()
    {
        thread_local winrt::Windows::UI::Xaml::ResourceDictionary resources{ nullptr };
        return resources;
    }

    inline winrt::Windows::UI::Xaml::Media::Geometry TakeGeometry(const winrt::Windows::UI::Xaml::Shapes::Path& shape)
    {
        const auto geometry = shape.Data();
        // Geometry has a single owner; detach it from the temporary template.
        shape.Data(nullptr);
        return geometry;
    }

    inline winrt::Windows::UI::Xaml::Media::Geometry GeometryForIconPath(const winrt::hstring& iconPath)
    {
        constexpr std::wstring_view prefix{ L"ms-appx:///AgentIcons/" };
        constexpr std::wstring_view suffix{ L".svg" };
        const std::wstring_view path{ iconPath };
        if (!path.starts_with(prefix) || !path.ends_with(suffix))
        {
            return nullptr;
        }
        const auto id = path.substr(prefix.size(), path.size() - prefix.size() - suffix.size());
        if (std::ranges::none_of(::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents,
                                [&](const auto& agent) { return agent.id == id; }))
        {
            return nullptr;
        }
        namespace Xaml = winrt::Windows::UI::Xaml;
        const auto key = winrt::box_value(winrt::hstring{ L"AgentIcon." } + winrt::hstring{ id });
        const auto loaded = Resources().Lookup(key).as<Xaml::DataTemplate>().LoadContent();
        const auto content = loaded.as<Xaml::Controls::Viewbox>().Child();
        Xaml::Media::GeometryGroup group;
        if (const auto shape = content.try_as<Xaml::Shapes::Path>())
        {
            group.Children().Append(TakeGeometry(shape));
        }
        else
        {
            for (const auto& child : content.as<Xaml::Controls::Grid>().Children())
            {
                group.Children().Append(TakeGeometry(child.as<Xaml::Shapes::Path>()));
            }
        }
        const auto element = content.as<Xaml::FrameworkElement>();
        THROW_HR_IF_MSG(E_UNEXPECTED, !std::isfinite(element.Width()) || element.Width() <= 0 ||
                                         !std::isfinite(element.Height()) || element.Height() <= 0,
                        "Agent icon template must declare positive dimensions");
        Xaml::Media::ScaleTransform scale;
        scale.ScaleX(16.0 / element.Width());
        scale.ScaleY(16.0 / element.Height());
        group.Transform(scale);
        return group;
    }

    inline winrt::Microsoft::UI::Xaml::Controls::IconSource SourceForIconPath(const winrt::hstring& iconPath, const bool monochrome)
    {
        if (const auto geometry = GeometryForIconPath(iconPath))
        {
            winrt::Microsoft::UI::Xaml::Controls::PathIconSource source;
            source.Data(geometry);
            return source;
        }
        return winrt::Microsoft::Terminal::UI::IconPathConverter::IconSourceMUX(iconPath, monochrome);
    }

    inline winrt::Windows::UI::Xaml::Controls::IconElement ElementForIconPath(const winrt::hstring& iconPath)
    {
        if (const auto geometry = GeometryForIconPath(iconPath))
        {
            winrt::Windows::UI::Xaml::Controls::PathIcon icon;
            icon.Data(geometry);
            icon.Width(16);
            icon.Height(16);
            return icon;
        }
        return winrt::Microsoft::Terminal::UI::IconPathConverter::IconWUX(iconPath);
    }
}
