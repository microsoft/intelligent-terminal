// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <type_traits>
#include <winrt/base.h>

namespace Microsoft::Terminal::details
{
    // Keep construction type-dependent so discarded branches are not checked
    // in the non-template property accessors.
    template<typename T>
    T EmptyDependencyPropertyValue()
    {
        if constexpr (std::is_base_of_v<winrt::Windows::Foundation::IInspectable, T>)
        {
            return { nullptr };
        }
        else
        {
            return {};
        }
    }
}

// This macro defines a dependency property for a WinRT class.
// Use this in your class' header file after declaring it in the idl.
// Remember to register your dependency property in the respective cpp file.
#ifndef DEPENDENCY_PROPERTY
#define DEPENDENCY_PROPERTY(type, name)                                              \
public:                                                                              \
    static winrt::Windows::UI::Xaml::DependencyProperty name##Property()             \
    {                                                                                \
        return _##name##Property;                                                    \
    }                                                                                \
    type name() const                                                                \
    {                                                                                \
        auto&& temp{ GetValue(_##name##Property) };                                  \
        if (temp)                                                                    \
        {                                                                            \
            return winrt::unbox_value<type>(temp);                                   \
        }                                                                            \
        return ::Microsoft::Terminal::details::EmptyDependencyPropertyValue<type>(); \
    }                                                                                \
    void name(const type& value)                                                     \
    {                                                                                \
        SetValue(_##name##Property, winrt::box_value(value));                        \
    }                                                                                \
                                                                                     \
private:                                                                             \
    static winrt::Windows::UI::Xaml::DependencyProperty _##name##Property;
#endif

#ifndef ATTACHED_DEPENDENCY_PROPERTY
#define ATTACHED_DEPENDENCY_PROPERTY(type, name)                                                       \
public:                                                                                                \
    static winrt::Windows::UI::Xaml::DependencyProperty name##Property()                               \
    {                                                                                                  \
        return _##name##Property;                                                                      \
    }                                                                                                  \
    static type Get##name(winrt::Windows::UI::Xaml::DependencyObject const& target)                    \
    {                                                                                                  \
        auto&& temp{ target.GetValue(_##name##Property) };                                             \
        if (temp)                                                                                      \
        {                                                                                              \
            return winrt::unbox_value<type>(temp);                                                     \
        }                                                                                              \
        return ::Microsoft::Terminal::details::EmptyDependencyPropertyValue<type>();                   \
    }                                                                                                  \
    static void Set##name(winrt::Windows::UI::Xaml::DependencyObject const& target, const type& value) \
    {                                                                                                  \
        target.SetValue(_##name##Property, winrt::box_value(value));                                   \
    }                                                                                                  \
                                                                                                       \
private:                                                                                               \
    static winrt::Windows::UI::Xaml::DependencyProperty _##name##Property;
#endif
