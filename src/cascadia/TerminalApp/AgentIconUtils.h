// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "../inc/AgentRegistry.h"

namespace Microsoft::Terminal::UI::AgentIcons
{
    inline winrt::hstring IconPathForProvider(const std::wstring_view providerId)
    {
        for (const auto& agent : ::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents)
        {
            if (::Microsoft::Terminal::Settings::Model::AgentRegistry::AgentIdEquals(agent.id, providerId))
            {
                return winrt::hstring{ L"ms-appx:///AgentIcons/" } + winrt::hstring{ agent.id } + L".svg";
            }
        }
        return L"ms-appx:///AgentIcons/generic.svg";
    }

    inline winrt::Windows::UI::Xaml::ResourceDictionary& Resources()
    {
        thread_local winrt::Windows::UI::Xaml::ResourceDictionary resources{ nullptr };
        if (!resources)
        {
            winrt::Windows::UI::Xaml::ResourceDictionary dictionary;
            dictionary.Source(winrt::Windows::Foundation::Uri{ L"ms-appx:///TerminalApp/AgentIconResources.xaml" });
            resources = dictionary;
        }
        return resources;
    }

    inline winrt::Windows::UI::Xaml::Media::Geometry TakeGeometry(const winrt::Windows::UI::Xaml::Shapes::Path& shape)
    {
        const auto geometry = shape.Data();
        // Geometry has a single owner; detach it from the temporary template.
        shape.Data(nullptr);
        return geometry;
    }

    inline winrt::hstring MaskPathForIconPath(const winrt::hstring& iconPath)
    {
        constexpr std::wstring_view prefix{ L"ms-appx:///AgentIcons/" };
        constexpr std::wstring_view suffix{ L".svg" };
        const std::wstring_view path{ iconPath };
        if (!path.starts_with(prefix) || !path.ends_with(suffix))
        {
            return {};
        }
        const auto id = path.substr(prefix.size(), path.size() - prefix.size() - suffix.size());
        if (id != L"generic" && std::ranges::none_of(::Microsoft::Terminal::Settings::Model::AgentRegistry::BuiltinAcpAgents,
                                                    [&](const auto& agent) { return agent.id == id; }))
        {
            return {};
        }
        return winrt::hstring{ L"ms-appx:///AgentIcons/Masks/" } + winrt::hstring{ id } + L".png";
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
        if (const auto mask = MaskPathForIconPath(iconPath); !mask.empty())
        {
            return winrt::Microsoft::Terminal::UI::IconPathConverter::IconSourceMUX(mask, true);
        }
        return winrt::Microsoft::Terminal::UI::IconPathConverter::IconSourceMUX(iconPath, monochrome);
    }

    inline winrt::Windows::UI::Xaml::Controls::IconElement ElementForIconPath(const winrt::hstring& iconPath)
    {
        if (const auto mask = MaskPathForIconPath(iconPath); !mask.empty())
        {
            winrt::Windows::UI::Xaml::Controls::BitmapIcon icon;
            icon.UriSource(winrt::Windows::Foundation::Uri{ mask });
            icon.ShowAsMonochrome(true);
            icon.Width(16);
            icon.Height(16);
            return icon;
        }
        return winrt::Microsoft::Terminal::UI::IconPathConverter::IconWUX(iconPath);
    }

    inline winrt::Microsoft::UI::Xaml::Controls::IconSource SourceForIconElement(const winrt::Windows::UI::Xaml::Controls::IconElement& value)
    {
        namespace MUX = winrt::Microsoft::UI::Xaml::Controls;
        namespace WUX = winrt::Windows::UI::Xaml::Controls;
        MUX::IconSource source{ nullptr };
        if (!value)
        {
            source = MUX::BitmapIconSource{};
        }
        else if (const auto element = value.try_as<WUX::IconSourceElement>())
        {
            const auto data = element.IconSource();
            if (const auto font = data.try_as<WUX::FontIconSource>())
            {
                MUX::FontIconSource converted;
                converted.Glyph(font.Glyph());
                converted.FontFamily(font.FontFamily());
                converted.FontSize(font.FontSize());
                converted.FontStyle(font.FontStyle());
                converted.FontWeight(font.FontWeight());
                converted.MirroredWhenRightToLeft(font.MirroredWhenRightToLeft());
                converted.IsTextScaleFactorEnabled(font.IsTextScaleFactorEnabled());
                source = converted;
            }
            else if (const auto bitmap = data.try_as<WUX::BitmapIconSource>())
            {
                MUX::BitmapIconSource converted;
                converted.UriSource(bitmap.UriSource());
                converted.ShowAsMonochrome(bitmap.ShowAsMonochrome());
                source = converted;
            }
            else if (const auto path = data.try_as<WUX::PathIconSource>())
            {
                MUX::PathIconSource converted;
                converted.Data(path.Data());
                source = converted;
            }
            else if (const auto symbol = data.try_as<WUX::SymbolIconSource>())
            {
                MUX::SymbolIconSource converted;
                converted.Symbol(symbol.Symbol());
                source = converted;
            }
            else
            {
                THROW_HR_IF(E_INVALIDARG, data);
            }
        }
        else if (const auto font = value.try_as<WUX::FontIcon>())
        {
            MUX::FontIconSource data;
            data.Glyph(font.Glyph());
            data.FontFamily(font.FontFamily());
            data.FontSize(font.FontSize());
            data.FontStyle(font.FontStyle());
            data.FontWeight(font.FontWeight());
            data.MirroredWhenRightToLeft(font.MirroredWhenRightToLeft());
            data.IsTextScaleFactorEnabled(font.IsTextScaleFactorEnabled());
            source = data;
        }
        else if (const auto bitmap = value.try_as<WUX::BitmapIcon>())
        {
            MUX::BitmapIconSource data;
            data.UriSource(bitmap.UriSource());
            data.ShowAsMonochrome(bitmap.ShowAsMonochrome());
            source = data;
        }
        else if (const auto path = value.try_as<WUX::PathIcon>())
        {
            MUX::PathIconSource data;
            data.Data(path.Data());
            source = data;
        }
        else if (const auto symbol = value.try_as<WUX::SymbolIcon>())
        {
            MUX::SymbolIconSource data;
            data.Symbol(symbol.Symbol());
            source = data;
        }
        else if (const auto image = value.try_as<MUX::ImageIcon>())
        {
            MUX::ImageIconSource data;
            data.ImageSource(image.Source());
            source = data;
        }
        else
        {
            THROW_HR(E_INVALIDARG);
        }
        if (!source)
        {
            source = MUX::BitmapIconSource{};
        }
        if (value)
        {
            source.Foreground(value.Foreground());
        }
        return source;
    }

    inline winrt::Windows::UI::Xaml::Media::Geometry SnapshotGeometry(const winrt::Windows::UI::Xaml::Media::Geometry& geometry)
    {
        namespace Media = winrt::Windows::UI::Xaml::Media;
        if (!geometry)
        {
            return nullptr;
        }
        Media::Geometry snapshot{ nullptr };
        if (const auto ellipse = geometry.try_as<Media::EllipseGeometry>())
        {
            Media::EllipseGeometry copy;
            copy.Center(ellipse.Center());
            copy.RadiusX(ellipse.RadiusX());
            copy.RadiusY(ellipse.RadiusY());
            snapshot = copy;
        }
        else if (const auto line = geometry.try_as<Media::LineGeometry>())
        {
            Media::LineGeometry copy;
            copy.StartPoint(line.StartPoint());
            copy.EndPoint(line.EndPoint());
            snapshot = copy;
        }
        else if (const auto rectangle = geometry.try_as<Media::RectangleGeometry>())
        {
            Media::RectangleGeometry copy;
            copy.Rect(rectangle.Rect());
            snapshot = copy;
        }
        else if (const auto group = geometry.try_as<Media::GeometryGroup>())
        {
            Media::GeometryGroup copy;
            copy.FillRule(group.FillRule());
            for (const auto& child : group.Children())
            {
                copy.Children().Append(SnapshotGeometry(child));
            }
            snapshot = copy;
        }
        else if (const auto path = geometry.try_as<Media::PathGeometry>())
        {
            Media::PathGeometry copy;
            copy.FillRule(path.FillRule());
            for (const auto& figure : path.Figures())
            {
                Media::PathFigure copiedFigure;
                copiedFigure.StartPoint(figure.StartPoint());
                copiedFigure.IsClosed(figure.IsClosed());
                copiedFigure.IsFilled(figure.IsFilled());
                for (const auto& segment : figure.Segments())
                {
                    Media::PathSegment copiedSegment{ nullptr };
                    if (const auto arc = segment.try_as<Media::ArcSegment>())
                    {
                        Media::ArcSegment value;
                        value.Point(arc.Point());
                        value.Size(arc.Size());
                        value.RotationAngle(arc.RotationAngle());
                        value.IsLargeArc(arc.IsLargeArc());
                        value.SweepDirection(arc.SweepDirection());
                        copiedSegment = value;
                    }
                    else if (const auto bezier = segment.try_as<Media::BezierSegment>())
                    {
                        Media::BezierSegment value;
                        value.Point1(bezier.Point1());
                        value.Point2(bezier.Point2());
                        value.Point3(bezier.Point3());
                        copiedSegment = value;
                    }
                    else if (const auto lineSegment = segment.try_as<Media::LineSegment>())
                    {
                        Media::LineSegment value;
                        value.Point(lineSegment.Point());
                        copiedSegment = value;
                    }
                    else if (const auto polyBezier = segment.try_as<Media::PolyBezierSegment>())
                    {
                        Media::PolyBezierSegment value;
                        for (const auto& point : polyBezier.Points())
                        {
                            value.Points().Append(point);
                        }
                        copiedSegment = value;
                    }
                    else if (const auto polyLine = segment.try_as<Media::PolyLineSegment>())
                    {
                        Media::PolyLineSegment value;
                        for (const auto& point : polyLine.Points())
                        {
                            value.Points().Append(point);
                        }
                        copiedSegment = value;
                    }
                    else if (const auto polyQuadraticBezier = segment.try_as<Media::PolyQuadraticBezierSegment>())
                    {
                        Media::PolyQuadraticBezierSegment value;
                        for (const auto& point : polyQuadraticBezier.Points())
                        {
                            value.Points().Append(point);
                        }
                        copiedSegment = value;
                    }
                    else if (const auto quadraticBezier = segment.try_as<Media::QuadraticBezierSegment>())
                    {
                        Media::QuadraticBezierSegment value;
                        value.Point1(quadraticBezier.Point1());
                        value.Point2(quadraticBezier.Point2());
                        copiedSegment = value;
                    }
                    else
                    {
                        THROW_HR_MSG(E_NOTIMPL, "Unsupported icon path segment");
                    }
                    copiedFigure.Segments().Append(copiedSegment);
                }
                copy.Figures().Append(copiedFigure);
            }
            snapshot = copy;
        }
        else
        {
            THROW_HR_MSG(E_NOTIMPL, "Unsupported icon geometry");
        }
        if (const auto transform = geometry.Transform())
        {
            // Snapshot the evaluated affine transform, not its bindings or animations.
            const auto origin = transform.TransformPoint({ 0, 0 });
            const auto x = transform.TransformPoint({ 1, 0 });
            const auto y = transform.TransformPoint({ 0, 1 });
            Media::MatrixTransform copy;
            copy.Matrix({ static_cast<double>(x.X) - origin.X, static_cast<double>(x.Y) - origin.Y,
                          static_cast<double>(y.X) - origin.X, static_cast<double>(y.Y) - origin.Y,
                          origin.X, origin.Y });
            snapshot.Transform(copy);
        }
        return snapshot;
    }

    inline winrt::Windows::UI::Xaml::Controls::IconElement ElementForIconSource(const winrt::Microsoft::UI::Xaml::Controls::IconSource& source,
                                                                            const winrt::hstring& iconPath = {})
    {
        namespace MUX = winrt::Microsoft::UI::Xaml::Controls;
        namespace WUX = winrt::Windows::UI::Xaml::Controls;
        WUX::IconElement element{ nullptr };
        if (const auto font = source.try_as<MUX::FontIconSource>())
        {
            WUX::FontIcon icon;
            icon.Glyph(font.Glyph());
            icon.FontFamily(font.FontFamily());
            icon.FontSize(font.FontSize());
            icon.FontStyle(font.FontStyle());
            icon.FontWeight(font.FontWeight());
            icon.MirroredWhenRightToLeft(font.MirroredWhenRightToLeft());
            icon.IsTextScaleFactorEnabled(font.IsTextScaleFactorEnabled());
            element = icon;
        }
        else if (const auto bitmap = source.try_as<MUX::BitmapIconSource>())
        {
            WUX::BitmapIcon icon;
            icon.UriSource(bitmap.UriSource());
            icon.ShowAsMonochrome(bitmap.ShowAsMonochrome());
            element = icon;
        }
        else if (const auto path = source.try_as<MUX::PathIconSource>())
        {
            WUX::PathIcon icon;
            // Each realized PathIcon needs its own Geometry; source adapters retain caller data.
            const auto geometry = GeometryForIconPath(iconPath);
            icon.Data(geometry ? geometry : SnapshotGeometry(path.Data()));
            icon.Width(16);
            icon.Height(16);
            element = icon;
        }
        else if (const auto symbol = source.try_as<MUX::SymbolIconSource>())
        {
            WUX::SymbolIcon icon;
            icon.Symbol(symbol.Symbol());
            element = icon;
        }
        else if (const auto image = source.try_as<MUX::ImageIconSource>())
        {
            MUX::ImageIcon icon;
            icon.Source(image.ImageSource());
            icon.Width(32);
            icon.Height(32);
            element = icon;
        }
        else
        {
            THROW_HR(E_INVALIDARG);
        }
        if (source.Foreground())
        {
            element.Foreground(source.Foreground());
        }
        return element;
    }
}
