// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RtlHelperTests.cpp
//
// Smoke tests for the RTL detection helper at
// src/cascadia/inc/RtlHelper.h. The helper is a thin wrapper around
// `GetLocaleInfoEx` — the OS owns the authoritative classifier — so
// these tests deliberately do NOT maintain a parallel list of "what
// counts as RTL". For the broad-coverage test we ask the OS itself
// (via `EnumSystemLocalesEx`) which locales it knows about and assert
// the helper agrees with the OS on each. No hardcoded language list
// anywhere in this file.

#include "precomp.h"

#include <winnls.h>

#include <vector>

#include "../inc/RtlHelper.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::RtlHelper;

namespace TerminalAppUnitTests
{
    class RtlHelperTests
    {
        TEST_CLASS(RtlHelperTests);

        TEST_METHOD(EmptyStringIsLtr);
        TEST_METHOD(EnUsIsLtr);
        TEST_METHOD(MalformedTagsAreLtr);
        TEST_METHOD(MatchesOsClassificationForEveryInstalledLocale);
        TEST_METHOD(PseudoMirroredIsRtl);
        TEST_METHOD(PseudoLtrPseudoLocalesAreLtr);
        TEST_METHOD(MatchingIsCaseInsensitive);
    };

    // Ask the OS via a *different* code path than the helper uses,
    // so a flag / buffer-sizing bug in `IsRtlLocale` can't mask itself
    // when the test compares expected vs. actual. The helper reads
    // the value as a binary `DWORD` via `LOCALE_RETURN_NUMBER`; here
    // we deliberately omit that flag so the OS returns the value as a
    // decimal string ("0" .. "3"), which we parse. Independent path,
    // same underlying classifier.
    static bool OsSaysRtl(std::wstring_view tag)
    {
        const std::wstring nullTerminated{ tag };
        wchar_t buf[8]{};
        const int chars = ::GetLocaleInfoEx(
            nullTerminated.c_str(),
            LOCALE_IREADINGLAYOUT,
            buf,
            static_cast<int>(std::size(buf)));
        return chars > 1 && buf[0] == L'1';
    }

    // Callback for `EnumSystemLocalesEx`. Each locale name is pushed
    // into the `std::vector<std::wstring>*` whose pointer is passed
    // through `LPARAM`.
    static BOOL CALLBACK CollectLocaleCallback(LPWSTR localeName, DWORD /*flags*/, LPARAM lParam)
    {
        auto* vec = reinterpret_cast<std::vector<std::wstring>*>(lParam);
        vec->emplace_back(localeName);
        return TRUE;
    }

    // Collect every BCP-47 tag the OS knows about. We don't carry our
    // own list — `EnumSystemLocalesEx` is the list, this is the loop.
    static std::vector<std::wstring> EnumerateInstalledLocales()
    {
        std::vector<std::wstring> locales;
        const BOOL ok = ::EnumSystemLocalesEx(
            CollectLocaleCallback,
            LOCALE_WINDOWS,
            reinterpret_cast<LPARAM>(&locales),
            nullptr);
        VERIFY_IS_TRUE(ok != FALSE, L"EnumSystemLocalesEx failed");
        return locales;
    }

    void RtlHelperTests::EmptyStringIsLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L""));
    }

    void RtlHelperTests::EnUsIsLtr()
    {
        // Smoke-test anchor: en-US is the universal LTR baseline.
        // Catches a regression where the helper inverts its result or
        // returns true on success unconditionally, without relying on
        // OS enumeration to find any LTR locale to compare against.
        VERIFY_IS_FALSE(OsSaysRtl(L"en-US"));
        VERIFY_IS_FALSE(IsRtlLocale(L"en-US"));
    }

    void RtlHelperTests::MalformedTagsAreLtr()
    {
        // `GetLocaleInfoEx` returns 0 for these; the helper must treat
        // failure as LTR (the safe default).
        VERIFY_IS_FALSE(IsRtlLocale(L"-"));
        VERIFY_IS_FALSE(IsRtlLocale(L"-ar"));
        VERIFY_IS_FALSE(IsRtlLocale(L"not a tag"));
        VERIFY_IS_FALSE(IsRtlLocale(L"!!!"));
    }

    void RtlHelperTests::MatchesOsClassificationForEveryInstalledLocale()
    {
        // Enumerate every locale the OS knows about (no hardcoded
        // list) and assert the helper agrees with the OS on each. If
        // the OS ever ships a new RTL locale we automatically cover
        // it without touching this test.
        const auto locales = EnumerateInstalledLocales();
        VERIFY_IS_FALSE(locales.empty(), L"EnumSystemLocalesEx returned no locales; expected at least one");
        size_t mismatches = 0;
        for (const auto& tag : locales)
        {
            const bool expected = OsSaysRtl(tag);
            const bool actual = IsRtlLocale(tag);
            if (expected != actual)
            {
                ++mismatches;
                Log::Comment(NoThrowString().Format(
                    L"Disagreement on '%s': helper=%d, os=%d",
                    tag.c_str(),
                    actual ? 1 : 0,
                    expected ? 1 : 0));
            }
        }
        VERIFY_ARE_EQUAL(size_t{ 0 }, mismatches, L"Helper must agree with OS on every installed locale");
    }

    void RtlHelperTests::PseudoMirroredIsRtl()
    {
        // `qps-plocm` is the Microsoft pseudo-mirrored pseudo-locale —
        // it is the canonical way to validate FlowDirection plumbing.
        // The OS classifies it as RTL; we just verify we don't lose
        // that on the way through.
        VERIFY_IS_TRUE(OsSaysRtl(L"qps-plocm"));
        VERIFY_IS_TRUE(IsRtlLocale(L"qps-plocm"));
    }

    void RtlHelperTests::PseudoLtrPseudoLocalesAreLtr()
    {
        // `qps-ploc` / `qps-ploca` accent + pad strings but do not
        // mirror.
        VERIFY_IS_FALSE(OsSaysRtl(L"qps-ploc"));
        VERIFY_IS_FALSE(IsRtlLocale(L"qps-ploc"));
        VERIFY_IS_FALSE(OsSaysRtl(L"qps-ploca"));
        VERIFY_IS_FALSE(IsRtlLocale(L"qps-ploca"));
    }

    void RtlHelperTests::MatchingIsCaseInsensitive()
    {
        // BCP-47 tags are case-insensitive by spec; the OS normalizes
        // them. Confirm we pass that through using one RTL and one
        // LTR pseudo-locale (avoids hardcoding any real language).
        VERIFY_ARE_EQUAL(IsRtlLocale(L"qps-plocm"), IsRtlLocale(L"QPS-PLOCM"));
        VERIFY_ARE_EQUAL(IsRtlLocale(L"qps-ploc"), IsRtlLocale(L"QPS-PLOC"));
    }
}
