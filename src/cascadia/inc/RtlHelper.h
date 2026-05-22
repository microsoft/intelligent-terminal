// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RtlHelper.h
//
// Pure helper for right-to-left (RTL) language detection. Used by
// FreOverlay (and any other XAML surface that wants to honor the user's
// preferred UI language layout direction) to decide whether to set
// FlowDirection::RightToLeft on its root element.
//
// Design notes
// ------------
// XAML cascades FlowDirection down the visual tree and auto-mirrors
// HorizontalAlignment, so the natural fix for RTL layout is to set
// FlowDirection on the *root* of a screen and let everything else
// inherit. That keeps the change surgical: one line of layout code, no
// per-control re-shuffling, no over-engineering.
//
// What counts as "RTL" is determined by the BCP-47 language subtag,
// matched case-insensitively. The list mirrors the set of RTL locales
// we ship resources for plus a couple of well-known RTL scripts that
// could conceivably be passed through `settings.json`'s Language field.
// `qps-plocm` (Microsoft pseudo-mirrored pseudo-locale) is included so
// localization engineers can validate the wiring without a real RTL
// build.
//
// Header-only on purpose: the Settings ModelLib is a static lib wrapped
// by a DLL that only exports WinRT types, and unit tests in ut_app must
// be able to link this without a separate object.

#pragma once

#include <cwctype>
#include <string_view>

namespace Microsoft::Terminal::RtlHelper
{
    // BCP-47 language subtags whose scripts are written right-to-left.
    // Keep ASCII-lowercase; comparison is case-insensitive.
    //
    //   ar  — Arabic
    //   he  — Hebrew (also covers legacy `iw`)
    //   fa  — Persian / Farsi
    //   ur  — Urdu
    //   ug  — Uyghur
    //   ps  — Pashto
    //   sd  — Sindhi (Perso-Arabic script)
    //   ckb — Central Kurdish (Sorani)
    //   yi  — Yiddish
    //   dv  — Divehi / Maldivian
    //
    // We also recognize the Microsoft pseudo-mirrored pseudo-locale
    // `qps-plocm`, which is the canonical way to validate RTL plumbing.
    inline constexpr std::wstring_view kRtlLanguageSubtags[] = {
        L"ar",
        L"he",
        L"iw",
        L"fa",
        L"ur",
        L"ug",
        L"ps",
        L"sd",
        L"ckb",
        L"yi",
        L"dv",
    };

    // Returns true if `language` is a BCP-47 tag whose primary language
    // subtag (the bit before the first `-`) is right-to-left. Matching
    // is case-insensitive. Empty strings, malformed input, and unknown
    // languages all yield false (i.e. the safe LTR default).
    //
    // Examples:
    //   IsRtlLocale(L"ar-SA")     -> true
    //   IsRtlLocale(L"AR")        -> true
    //   IsRtlLocale(L"he-IL")     -> true
    //   IsRtlLocale(L"qps-plocm") -> true   (pseudo-mirrored)
    //   IsRtlLocale(L"qps-ploc")  -> false  (pseudo-LTR)
    //   IsRtlLocale(L"en-US")     -> false
    //   IsRtlLocale(L"")          -> false
    //   IsRtlLocale(L"-bogus")    -> false
    [[nodiscard]] inline bool IsRtlLocale(std::wstring_view language) noexcept
    {
        if (language.empty())
        {
            return false;
        }

        // Special-case the pseudo-mirrored pseudo-locale. It is not a
        // BCP-47 language subtag in its own right (`qps` is the
        // pseudo-language prefix), so we match the whole tag.
        constexpr std::wstring_view pseudoMirrored = L"qps-plocm";
        if (language.size() == pseudoMirrored.size())
        {
            bool eq = true;
            for (size_t i = 0; i < pseudoMirrored.size(); ++i)
            {
                if (std::towlower(language[i]) != pseudoMirrored[i])
                {
                    eq = false;
                    break;
                }
            }
            if (eq)
            {
                return true;
            }
        }

        // Extract the primary language subtag (chars before the first `-`).
        const auto dash = language.find(L'-');
        const auto primary = (dash == std::wstring_view::npos)
                                 ? language
                                 : language.substr(0, dash);
        if (primary.empty())
        {
            return false;
        }

        for (const auto& tag : kRtlLanguageSubtags)
        {
            if (primary.size() != tag.size())
            {
                continue;
            }
            bool match = true;
            for (size_t i = 0; i < tag.size(); ++i)
            {
                if (std::towlower(primary[i]) != tag[i])
                {
                    match = false;
                    break;
                }
            }
            if (match)
            {
                return true;
            }
        }
        return false;
    }
}
