// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RtlHelperTests.cpp
//
// Smoke tests for the RTL detection helper at
// src/cascadia/inc/RtlHelper.h. The helper is a thin wrapper around
// `GetLocaleInfoEx` — the OS owns the authoritative classifier — so
// these tests deliberately do NOT maintain a parallel list of "what
// counts as RTL". They only pin that:
//
//   * The wrapper correctly calls into the OS (i.e. our well-known
//     fork-shipping locales classify the way users expect).
//   * Garbage input is treated as LTR (the safe default) and does not
//     crash.
//
// For every locale the helper needs to classify, we ask the OS itself
// for the expected answer and assert agreement — there is no
// hardcoded "Arabic = RTL" knowledge in this file. No WinRT
// activation required.

#include "precomp.h"

#include <winnls.h>

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
        TEST_METHOD(MalformedTagsAreLtr);
        TEST_METHOD(MatchesOsClassificationForShippingLocales);
        TEST_METHOD(PseudoMirroredIsRtl);
        TEST_METHOD(PseudoLtrPseudoLocalesAreLtr);
        TEST_METHOD(MatchingIsCaseInsensitive);
    };

    // The set of locales whose layout direction we *care about* in
    // this product. For each, the OS is the source of truth — we just
    // assert our helper agrees with what the OS reports. Tests don't
    // hardcode the LTR/RTL answer.
    static constexpr std::wstring_view kLocalesToProbe[] = {
        // RTL locales the fork ships translations for.
        L"ar-SA", L"he-IL", L"fa-IR", L"ur-PK", L"ug-CN",
        // A representative sample of LTR locales from the fork's set.
        L"en-US", L"en-GB", L"de-DE", L"fr-FR", L"ja-JP",
        L"zh-CN", L"zh-TW", L"ko-KR", L"es-ES", L"hi-IN",
        L"ru-RU", L"pt-BR", L"it-IT", L"pl-PL", L"tr-TR",
    };

    // Ask the OS directly via the same Win32 API the helper wraps.
    // Tests derive the expected answer from this — no hardcoded
    // "language X is RTL".
    static bool OsSaysRtl(std::wstring_view tag)
    {
        const std::wstring nullTerminated{ tag };
        DWORD value{};
        const int chars = ::GetLocaleInfoEx(
            nullTerminated.c_str(),
            LOCALE_IREADINGLAYOUT | LOCALE_RETURN_NUMBER,
            reinterpret_cast<LPWSTR>(&value),
            static_cast<int>(sizeof(value) / sizeof(wchar_t)));
        return chars > 0 && value == 1;
    }

    void RtlHelperTests::EmptyStringIsLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L""));
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

    void RtlHelperTests::MatchesOsClassificationForShippingLocales()
    {
        // For every locale we care about, our wrapper must agree with
        // the OS. No hardcoded RTL list — the OS *is* the list.
        for (const auto tag : kLocalesToProbe)
        {
            const bool expected = OsSaysRtl(tag);
            const bool actual = IsRtlLocale(tag);
            VERIFY_ARE_EQUAL(expected, actual, NoThrowString().Format(L"locale=%s", std::wstring{ tag }.c_str()));
        }
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
        // them. Confirm we pass that through correctly so a locale
        // tag from settings.json (potentially typed with mixed case)
        // still classifies.
        VERIFY_ARE_EQUAL(IsRtlLocale(L"ar-SA"), IsRtlLocale(L"AR-sa"));
        VERIFY_ARE_EQUAL(IsRtlLocale(L"he-IL"), IsRtlLocale(L"HE-il"));
        VERIFY_ARE_EQUAL(IsRtlLocale(L"qps-plocm"), IsRtlLocale(L"QPS-PLOCM"));
    }
}
