// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../TerminalApp/SharedWta.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppUnitTests
{
    class SharedWtaTests
    {
        TEST_CLASS(SharedWtaTests);

        TEST_METHOD(AcceptsValidEnvironmentOverride);
        TEST_METHOD(RejectsEmptyEnvironmentName);
        TEST_METHOD(RejectsEqualsInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentValue);
    };

    void SharedWtaTests::AcceptsValidEnvironmentOverride()
    {
        VERIFY_IS_TRUE(details::IsValidEnvironmentOverride(L"WTA_LOG", L"debug=verbose"));
        VERIFY_IS_TRUE(details::IsValidEnvironmentOverride(L"WTA_LOG", L""));
    }

    void SharedWtaTests::RejectsEmptyEnvironmentName()
    {
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"", L"value"));
    }

    void SharedWtaTests::RejectsEqualsInEnvironmentName()
    {
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"WTA=LOG", L"value"));
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"=C:", L"value"));
    }

    void SharedWtaTests::RejectsEmbeddedNullInEnvironmentName()
    {
        constexpr wchar_t name[]{ L'W', L'T', L'A', L'\0', L'L', L'O', L'G' };
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(std::wstring_view{ name, std::size(name) }, L"value"));
    }

    void SharedWtaTests::RejectsEmbeddedNullInEnvironmentValue()
    {
        constexpr wchar_t value[]{ L'd', L'e', L'b', L'u', L'g', L'\0', L't', L'r', L'a', L'c', L'e' };
        VERIFY_IS_FALSE(details::IsValidEnvironmentOverride(L"WTA_LOG", std::wstring_view{ value, std::size(value) }));
    }
}
