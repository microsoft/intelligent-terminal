// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "../TerminalApp/HistoryCompletionProvider.h"
#include "../TerminalApp/PathCompletionProvider.h"
#include "../TerminalApp/PromptInputModel.h"

#include <WexTestClass.h>

using namespace WEX::TestExecution;
using namespace winrt::TerminalApp::implementation;

namespace TerminalAppLocalTests
{
    class PromptPathProviderTests
    {
        TEST_CLASS(PromptPathProviderTests);

        TEST_METHOD(TracksEditableInput);
        TEST_METHOD(RanksHistoryCompletions);
        TEST_METHOD(CompletesFilesystemTokens);
        TEST_METHOD(CompletesQuotedFilesystemTokens);
    };

    void PromptPathProviderTests::TracksEditableInput()
    {
        PromptInputModel model;
        model.ApplyCharacter(L'a');
        model.ApplyCharacter(L'c');
        model.ApplyKey(VK_LEFT, 0, true);
        model.ApplyCharacter(L'b');

        auto snapshot = model.Snapshot();
        VERIFY_ARE_EQUAL(std::wstring{ L"abc" }, snapshot.text);
        VERIFY_ARE_EQUAL(size_t{ 2 }, snapshot.cursor);
        VERIFY_IS_TRUE(snapshot.trusted);

        model.ApplyCompletion(0, 3, L"abcd");
        snapshot = model.Snapshot();
        VERIFY_ARE_EQUAL(std::wstring{ L"abcd" }, snapshot.text);
        VERIFY_ARE_EQUAL(size_t{ 4 }, snapshot.cursor);

        model.ApplyKey(VK_UP, 0, true);
        VERIFY_IS_FALSE(model.Snapshot().trusted);
        model.ApplyKey(VK_RETURN, 0, true);
        VERIFY_IS_TRUE(model.Snapshot().trusted);
        VERIFY_IS_TRUE(model.Snapshot().text.empty());

        model.ApplyCharacter(L'x');
        model.ApplyKey(L'A', LEFT_CTRL_PRESSED, true);
        VERIFY_IS_FALSE(model.Snapshot().trusted);
        model.ApplyKey(L'C', LEFT_CTRL_PRESSED, true);
        VERIFY_IS_TRUE(model.Snapshot().trusted);
        VERIFY_IS_TRUE(model.Snapshot().text.empty());
    }

    void PromptPathProviderTests::RanksHistoryCompletions()
    {
        const std::vector<std::wstring> history{
            L"git checkout main",
            L"git status",
            L"git checkout main",
            L"git checkout dev",
            L"docker ps",
        };
        const PromptInputSnapshot input{
            .text = L"git c",
            .cursor = 5,
            .version = 1,
            .trusted = true,
        };

        HistoryCompletionProvider provider;
        const auto results = provider.Query(input, history);

        VERIFY_ARE_EQUAL(size_t{ 2 }, results.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"git checkout main" }, results[0].completionText);
        VERIFY_ARE_EQUAL(std::wstring{ L"git checkout dev" }, results[1].completionText);
        VERIFY_ARE_EQUAL(0u, results[0].replacementIndex);
        VERIFY_ARE_EQUAL(5u, results[0].replacementLength);
        VERIFY_ARE_EQUAL(1, results[0].resultType);
    }

    void PromptPathProviderTests::CompletesFilesystemTokens()
    {
        const auto root = std::filesystem::temp_directory_path() / L"IntelligentTerminalPathProviderTest";
        std::error_code ec;
        std::filesystem::remove_all(root, ec);
        std::filesystem::create_directories(root / L"Alpine", ec);
        VERIFY_IS_FALSE(static_cast<bool>(ec));
        {
            std::ofstream file{ root / L"Alpha.txt" };
            file << "test";
        }

        PathCompletionProvider provider;
        const PromptInputSnapshot input{
            .text = L"type Al",
            .cursor = 7,
            .version = 1,
            .trusted = true,
        };
        const auto results = provider.Query(input, root.wstring());

        VERIFY_ARE_EQUAL(size_t{ 2 }, results.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Alpha.txt" }, results[0].completionText);
        VERIFY_ARE_EQUAL(5u, results[0].replacementIndex);
        VERIFY_ARE_EQUAL(2u, results[0].replacementLength);
        VERIFY_ARE_EQUAL(std::wstring{ L"Alpine\\" }, results[1].completionText);

        std::filesystem::remove_all(root, ec);
    }

    void PromptPathProviderTests::CompletesQuotedFilesystemTokens()
    {
        const auto root = std::filesystem::temp_directory_path() / L"IntelligentTerminalQuotedPathProviderTest";
        std::error_code ec;
        std::filesystem::remove_all(root, ec);
        std::filesystem::create_directories(root / L"Space Dir" / L"Alpine", ec);
        VERIFY_IS_FALSE(static_cast<bool>(ec));
        {
            std::ofstream file{ root / L"Space Dir" / L"Alpha.txt" };
            file << "test";
        }
        {
            std::ofstream file{ root / L"Beta.txt" };
            file << "test";
        }

        PathCompletionProvider provider;
        const PromptInputSnapshot input{
            .text = L"type \"Space Dir\\Al",
            .cursor = 18,
            .version = 1,
            .trusted = true,
        };
        const auto results = provider.Query(input, root.wstring());

        VERIFY_ARE_EQUAL(size_t{ 2 }, results.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"\"Space Dir\\Alpha.txt\"" }, results[0].completionText);
        VERIFY_ARE_EQUAL(5u, results[0].replacementIndex);
        VERIFY_ARE_EQUAL(13u, results[0].replacementLength);
        VERIFY_ARE_EQUAL(std::wstring{ L"\"Space Dir\\Alpine\\" }, results[1].completionText);

        const PromptInputSnapshot afterClosedQuote{
            .text = L"type \"Space Dir\\\" Be",
            .cursor = 20,
            .version = 2,
            .trusted = true,
        };
        const auto afterClosedQuoteResults = provider.Query(afterClosedQuote, root.wstring());
        VERIFY_ARE_EQUAL(size_t{ 1 }, afterClosedQuoteResults.size());
        VERIFY_ARE_EQUAL(std::wstring{ L"Beta.txt" }, afterClosedQuoteResults[0].completionText);
        VERIFY_ARE_EQUAL(18u, afterClosedQuoteResults[0].replacementIndex);
        VERIFY_ARE_EQUAL(2u, afterClosedQuoteResults[0].replacementLength);

        std::filesystem::remove_all(root, ec);
    }
}
