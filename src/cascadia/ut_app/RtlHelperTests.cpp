// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// RtlHelperTests.cpp
//
// Pure-function tests for the RTL language detection helper at
// src/cascadia/inc/RtlHelper.h. FreOverlay uses this to decide whether
// to set FlowDirection::RightToLeft on its root grid; getting the
// detection wrong means RTL users either don't see the layout flip at
// all, or LTR users get an accidentally-mirrored FRE. These tests pin
// the language-tag → RTL classification so any breaking change surfaces
// here rather than at runtime.
//
// No winrt, no XAML, no I/O — pure wstring_view in, bool out.

#include "precomp.h"

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
        TEST_METHOD(EnglishVariantsAreLtr);
        TEST_METHOD(CommonLtrLanguagesAreLtr);
        TEST_METHOD(ArabicVariantsAreRtl);
        TEST_METHOD(HebrewVariantsAreRtl);
        TEST_METHOD(PersianIsRtl);
        TEST_METHOD(UrduIsRtl);
        TEST_METHOD(UyghurIsRtl);
        TEST_METHOD(OtherRtlScriptsAreRtl);
        TEST_METHOD(MatchingIsCaseInsensitive);
        TEST_METHOD(PseudoMirroredIsRtl);
        TEST_METHOD(PseudoLtrPseudoLocaleIsLtr);
        TEST_METHOD(SubtagPrefixOnlyMatchesPrimary);
        TEST_METHOD(BareLanguageWithoutRegionWorks);
    };

    void RtlHelperTests::EmptyStringIsLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L""));
    }

    void RtlHelperTests::MalformedTagsAreLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L"-"));
        VERIFY_IS_FALSE(IsRtlLocale(L"-ar"));
        VERIFY_IS_FALSE(IsRtlLocale(L"-bogus"));
    }

    void RtlHelperTests::EnglishVariantsAreLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L"en"));
        VERIFY_IS_FALSE(IsRtlLocale(L"en-US"));
        VERIFY_IS_FALSE(IsRtlLocale(L"en-GB"));
        VERIFY_IS_FALSE(IsRtlLocale(L"en-AU"));
    }

    void RtlHelperTests::CommonLtrLanguagesAreLtr()
    {
        VERIFY_IS_FALSE(IsRtlLocale(L"de-DE"));
        VERIFY_IS_FALSE(IsRtlLocale(L"fr-FR"));
        VERIFY_IS_FALSE(IsRtlLocale(L"ja-JP"));
        VERIFY_IS_FALSE(IsRtlLocale(L"zh-CN"));
        VERIFY_IS_FALSE(IsRtlLocale(L"zh-TW"));
        VERIFY_IS_FALSE(IsRtlLocale(L"ru-RU"));
        VERIFY_IS_FALSE(IsRtlLocale(L"ko-KR"));
        VERIFY_IS_FALSE(IsRtlLocale(L"es-ES"));
        VERIFY_IS_FALSE(IsRtlLocale(L"hi-IN"));
    }

    void RtlHelperTests::ArabicVariantsAreRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"ar"));
        VERIFY_IS_TRUE(IsRtlLocale(L"ar-SA"));
        VERIFY_IS_TRUE(IsRtlLocale(L"ar-EG"));
        VERIFY_IS_TRUE(IsRtlLocale(L"ar-AE"));
    }

    void RtlHelperTests::HebrewVariantsAreRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"he"));
        VERIFY_IS_TRUE(IsRtlLocale(L"he-IL"));
        // Legacy ISO-639-1 code for Hebrew — older Windows builds still emit this.
        VERIFY_IS_TRUE(IsRtlLocale(L"iw"));
        VERIFY_IS_TRUE(IsRtlLocale(L"iw-IL"));
    }

    void RtlHelperTests::PersianIsRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"fa"));
        VERIFY_IS_TRUE(IsRtlLocale(L"fa-IR"));
    }

    void RtlHelperTests::UrduIsRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"ur"));
        VERIFY_IS_TRUE(IsRtlLocale(L"ur-PK"));
    }

    void RtlHelperTests::UyghurIsRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"ug"));
        VERIFY_IS_TRUE(IsRtlLocale(L"ug-CN"));
    }

    void RtlHelperTests::OtherRtlScriptsAreRtl()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"ps-AF"));   // Pashto
        VERIFY_IS_TRUE(IsRtlLocale(L"sd-PK"));   // Sindhi (Perso-Arabic)
        VERIFY_IS_TRUE(IsRtlLocale(L"ckb-IQ"));  // Central Kurdish
        VERIFY_IS_TRUE(IsRtlLocale(L"yi"));      // Yiddish
        VERIFY_IS_TRUE(IsRtlLocale(L"dv-MV"));   // Divehi
    }

    void RtlHelperTests::MatchingIsCaseInsensitive()
    {
        VERIFY_IS_TRUE(IsRtlLocale(L"AR"));
        VERIFY_IS_TRUE(IsRtlLocale(L"AR-sa"));
        VERIFY_IS_TRUE(IsRtlLocale(L"Ar-Sa"));
        VERIFY_IS_TRUE(IsRtlLocale(L"HE-IL"));
        VERIFY_IS_FALSE(IsRtlLocale(L"EN-us"));
    }

    void RtlHelperTests::PseudoMirroredIsRtl()
    {
        // qps-plocm is the Microsoft pseudo-mirrored pseudo-locale —
        // shipping it as RTL is exactly what makes the pseudo-locale
        // useful for validating FlowDirection plumbing.
        VERIFY_IS_TRUE(IsRtlLocale(L"qps-plocm"));
        VERIFY_IS_TRUE(IsRtlLocale(L"QPS-PLOCM"));
        VERIFY_IS_TRUE(IsRtlLocale(L"Qps-Plocm"));
    }

    void RtlHelperTests::PseudoLtrPseudoLocaleIsLtr()
    {
        // qps-ploc and qps-ploca are the LTR pseudo-locales — they
        // accent and pad strings but do not mirror layout.
        VERIFY_IS_FALSE(IsRtlLocale(L"qps-ploc"));
        VERIFY_IS_FALSE(IsRtlLocale(L"qps-ploca"));
    }

    void RtlHelperTests::SubtagPrefixOnlyMatchesPrimary()
    {
        // A language subtag that *contains* an RTL prefix but is not
        // itself one of our known RTL tags must not match. E.g. `aru`
        // would be a (hypothetical) 3-letter tag starting with `ar`;
        // we only RTL-flip if the *full* primary subtag is `ar`.
        VERIFY_IS_FALSE(IsRtlLocale(L"aru"));
        VERIFY_IS_FALSE(IsRtlLocale(L"arn-CL"));  // Mapuche / Mapudungun
        VERIFY_IS_FALSE(IsRtlLocale(L"her"));     // Herero
        VERIFY_IS_FALSE(IsRtlLocale(L"hen"));     // not a real tag, but covers the prefix-match trap
    }

    void RtlHelperTests::BareLanguageWithoutRegionWorks()
    {
        // No `-region` suffix — must still classify correctly.
        VERIFY_IS_TRUE(IsRtlLocale(L"ar"));
        VERIFY_IS_TRUE(IsRtlLocale(L"he"));
        VERIFY_IS_FALSE(IsRtlLocale(L"en"));
        VERIFY_IS_FALSE(IsRtlLocale(L"de"));
    }
}
