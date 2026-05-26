// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RtlHelper.h
//
// Thin wrapper around the Windows locale database for right-to-left
// (RTL) language detection. The OS already ships an authoritative
// classifier for every BCP-47 tag it knows about via
// `GetLocaleInfoEx` with the reading-layout locale-info field, so we
// delegate to it instead of maintaining our own list. Three benefits:
//
//   1. New or less common RTL scripts get correct treatment without
//      code changes.
//   2. Tests don't have to hardcode "what counts as RTL" — there is
//      no list to drift out of sync.
//   3. The FRE and the `wta` TUI agree on classification because both
//      stacks call the same Win32 API.
//
// Small pure helper shared between FRE wiring (`FreOverlay`) and the
// unit tests in `ut_app`. Header-only so neither has to take a
// dependency on a separate translation unit. No WinRT activation
// required — we go straight to the Win32 layer.

#pragma once

#include <string>
#include <string_view>

#include <winnls.h>

namespace Microsoft::Terminal::RtlHelper
{
    // Returns true if `language` is a BCP-47 language tag whose
    // preferred reading layout is right-to-left, as reported by
    // `GetLocaleInfoEx` (locale-info field that surfaces the
    // reading-layout direction).
    //
    // The API returns:
    //   0 = left-to-right
    //   1 = right-to-left
    //   2 = top-to-bottom, columns left-to-right (legacy CJK)
    //   3 = top-to-bottom, columns right-to-left (legacy CJK)
    //
    // We treat value 1 as RTL. Empty strings, malformed tags, and
    // unknown languages all yield false (the safe LTR default —
    // `GetLocaleInfoEx` returns 0 for an unknown tag and we map that
    // to "not RTL").
    //
    // Pseudo-locales: `qps-plocm` is the pseudo-mirrored pseudo-locale
    // Microsoft ships for RTL validation. The OS classifies it as RTL,
    // so we get that for free without a special case.
    [[nodiscard]] inline bool IsRtlLocale(std::wstring_view language) noexcept
    {
        if (language.empty())
        {
            return false;
        }

        // `GetLocaleInfoEx` requires a null-terminated wide string.
        const std::wstring nullTerminated{ language };

        DWORD value{};
        const int chars = ::GetLocaleInfoEx(
            nullTerminated.c_str(),
            LOCALE_IREADINGLAYOUT | LOCALE_RETURN_NUMBER,
            reinterpret_cast<LPWSTR>(&value),
            static_cast<int>(sizeof(value) / sizeof(wchar_t)));

        return chars > 0 && value == 1;
    }
}
