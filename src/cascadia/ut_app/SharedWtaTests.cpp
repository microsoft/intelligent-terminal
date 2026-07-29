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

        TEST_METHOD(EmptyEnvironmentOverridesInheritParent);
        TEST_METHOD(ValidEnvironmentOverridesCloneAndReplace);
        TEST_METHOD(MixedInvalidEnvironmentOverridesFail);
        TEST_METHOD(AcceptsValidEnvironmentOverride);
        TEST_METHOD(RejectsEmptyEnvironmentName);
        TEST_METHOD(RejectsEqualsInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentName);
        TEST_METHOD(RejectsEmbeddedNullInEnvironmentValue);
    };

    void SharedWtaTests::EmptyEnvironmentOverridesInheritParent()
    {
        const auto block = details::BuildEnvironmentBlock({});

        VERIFY_IS_TRUE(block.has_value());
        VERIFY_IS_TRUE(block->empty());
    }

    void SharedWtaTests::ValidEnvironmentOverridesCloneAndReplace()
    {
        const std::array overrides{
            std::pair{ std::wstring{ L"PATH" }, std::wstring{ L"WTA_SHARED_WTA_TEST_OVERRIDE=debug" } },
        };

        const auto block = details::BuildEnvironmentBlock(overrides);

        VERIFY_IS_TRUE(block.has_value());
        VERIFY_IS_FALSE(block->empty());
        VERIFY_IS_GREATER_THAN_OR_EQUAL(block->size(), size_t{ 2 });
        VERIFY_ARE_EQUAL(L'\0', (*block)[block->size() - 1]);
        VERIFY_ARE_EQUAL(L'\0', (*block)[block->size() - 2]);

        size_t pathEntries = 0;
        bool foundInheritedEntry = false;
        for (const wchar_t* current = block->data(); *current;)
        {
            const std::wstring_view entry{ current };
            const auto separator = entry.find(L'=', entry.starts_with(L'=') ? 1 : 0);
            const auto name = separator == std::wstring_view::npos ? entry : entry.substr(0, separator);
            if (_wcsicmp(std::wstring{ name }.c_str(), L"PATH") == 0)
            {
                ++pathEntries;
                VERIFY_ARE_EQUAL(std::wstring_view{ L"PATH=WTA_SHARED_WTA_TEST_OVERRIDE=debug" }, entry);
            }
            else
            {
                foundInheritedEntry = true;
            }
            current += entry.size() + 1;
        }
        VERIFY_ARE_EQUAL(size_t{ 1 }, pathEntries);
        VERIFY_IS_TRUE(foundInheritedEntry);
    }

    void SharedWtaTests::MixedInvalidEnvironmentOverridesFail()
    {
        const std::array invalidNameOverrides{
            std::pair{ std::wstring{ L"WTA_LOG" }, std::wstring{ L"debug" } },
            std::pair{ std::wstring{}, std::wstring{ L"value" } },
        };
        VERIFY_IS_FALSE(details::BuildEnvironmentBlock(invalidNameOverrides).has_value());

        constexpr wchar_t invalidValue[]{ L't', L'r', L'a', L'c', L'e', L'\0', L'j', L'u', L'n', L'k' };
        const std::array invalidValueOverrides{
            std::pair{ std::wstring{ L"WTA_LOG" }, std::wstring{ L"debug" } },
            std::pair{ std::wstring{ L"RUST_LOG" }, std::wstring{ invalidValue, std::size(invalidValue) } },
        };
        VERIFY_IS_FALSE(details::BuildEnvironmentBlock(invalidValueOverrides).has_value());
    }

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
