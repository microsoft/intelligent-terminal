// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include <WexTestClass.h>
#include <wininet.h>
#include <shlwapi.h>

#include "../renderer/inc/DummyRenderer.hpp"
#include "../cascadia/TerminalCore/Terminal.hpp"
#include "MockTermSettings.h"
#include "consoletaeftemplates.hpp"
#include "../../inc/TestUtils.h"

using namespace winrt::Microsoft::Terminal::Core;
using namespace Microsoft::Terminal::Core;

using namespace WEX::Common;
using namespace WEX::Logging;
using namespace WEX::TestExecution;

namespace TerminalCoreUnitTests
{
    class TerminalBufferTests;
};
using namespace TerminalCoreUnitTests;

class TerminalCoreUnitTests::TerminalBufferTests final
{
    // !!! DANGER: Many tests in this class expect the Terminal buffer
    // to be 80x32. If you change these, you'll probably inadvertently break a
    // bunch of tests !!!
    static const til::CoordType TerminalViewWidth = 80;
    static const til::CoordType TerminalViewHeight = 32;
    static const til::CoordType TerminalHistoryLength = 100;

    TEST_CLASS(TerminalBufferTests);

    TEST_METHOD(TestSimpleBufferWriting);

    TEST_METHOD(TestWrappingCharByChar);
    TEST_METHOD(TestWrappingALongString);

    TEST_METHOD(DontSnapToOutputTest);

    TEST_METHOD(TestResetClearTabStops);

    TEST_METHOD(TestAddTabStop);

    TEST_METHOD(TestClearTabStop);

    TEST_METHOD(TestGetForwardTab);

    TEST_METHOD(TestGetReverseTab);

    TEST_METHOD(TestURLPatternDetection);
    TEST_METHOD(TestHoverPathResolution);
    TEST_METHOD(TestHoverPathBoundsAndCache);

    TEST_METHOD_SETUP(MethodSetup)
    {
        // STEP 1: Set up the Terminal
        term = std::make_unique<Terminal>(Terminal::TestDummyMarker{});
        emptyRenderer = std::make_unique<DummyRenderer>(term.get());
        term->Create({ TerminalViewWidth, TerminalViewHeight }, TerminalHistoryLength, *emptyRenderer);
        return true;
    }

    TEST_METHOD_CLEANUP(MethodCleanup)
    {
        emptyRenderer = nullptr;
        term = nullptr;
        return true;
    }

private:
    void _SetTabStops(std::list<til::CoordType> columns, bool replace);
    std::list<til::CoordType> _GetTabStops();

    std::unique_ptr<DummyRenderer> emptyRenderer;
    std::unique_ptr<Terminal> term;
};

void TerminalBufferTests::TestSimpleBufferWriting()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();

    VERIFY_ARE_EQUAL(0, initialView.Top());
    VERIFY_ARE_EQUAL(32, initialView.BottomExclusive());

    termSm.ProcessString(L"Hello World");

    const auto secondView = term->GetViewport();

    VERIFY_ARE_EQUAL(0, secondView.Top());
    VERIFY_ARE_EQUAL(32, secondView.BottomExclusive());

    TestUtils::VerifyExpectedString(termTb, L"Hello World", { 0, 0 });
}

void TerminalBufferTests::TestWrappingCharByChar()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();
    auto& cursor = termTb.GetCursor();

    const auto charsToWrite = gsl::narrow_cast<til::CoordType>(TestUtils::Test100CharsString.size());

    VERIFY_ARE_EQUAL(0, initialView.Top());
    VERIFY_ARE_EQUAL(32, initialView.BottomExclusive());

    for (auto i = 0; i < charsToWrite; i++)
    {
        // This is a handy way of just printing the printable characters that
        // _aren't_ the space character.
        const auto wch = static_cast<wchar_t>(33 + (i % 94));
        termSm.ProcessCharacter(wch);
    }

    const auto secondView = term->GetViewport();

    VERIFY_ARE_EQUAL(0, secondView.Top());
    VERIFY_ARE_EQUAL(32, secondView.BottomExclusive());

    // Verify the cursor wrapped to the second line
    VERIFY_ARE_EQUAL(charsToWrite % initialView.Width(), cursor.GetPosition().x);
    VERIFY_ARE_EQUAL(1, cursor.GetPosition().y);

    // Verify that we marked the 0th row as _wrapped_
    const auto& row0 = termTb.GetRowByOffset(0);
    VERIFY_IS_TRUE(row0.WasWrapForced());

    const auto& row1 = termTb.GetRowByOffset(1);
    VERIFY_IS_FALSE(row1.WasWrapForced());

    TestUtils::VerifyExpectedString(termTb, TestUtils::Test100CharsString, { 0, 0 });
}

void TerminalBufferTests::TestWrappingALongString()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();
    auto& cursor = termTb.GetCursor();

    const auto charsToWrite = gsl::narrow_cast<til::CoordType>(TestUtils::Test100CharsString.size());
    VERIFY_ARE_EQUAL(100, charsToWrite);

    VERIFY_ARE_EQUAL(0, initialView.Top());
    VERIFY_ARE_EQUAL(32, initialView.BottomExclusive());

    termSm.ProcessString(TestUtils::Test100CharsString);

    const auto secondView = term->GetViewport();

    VERIFY_ARE_EQUAL(0, secondView.Top());
    VERIFY_ARE_EQUAL(32, secondView.BottomExclusive());

    // Verify the cursor wrapped to the second line
    VERIFY_ARE_EQUAL(charsToWrite % initialView.Width(), cursor.GetPosition().x);
    VERIFY_ARE_EQUAL(1, cursor.GetPosition().y);

    // Verify that we marked the 0th row as _wrapped_
    const auto& row0 = termTb.GetRowByOffset(0);
    VERIFY_IS_TRUE(row0.WasWrapForced());

    const auto& row1 = termTb.GetRowByOffset(1);
    VERIFY_IS_FALSE(row1.WasWrapForced());

    TestUtils::VerifyExpectedString(termTb, TestUtils::Test100CharsString, { 0, 0 });
}

void TerminalBufferTests::DontSnapToOutputTest()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();

    VERIFY_ARE_EQUAL(0, initialView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight, initialView.BottomExclusive());
    VERIFY_ARE_EQUAL(0, term->_scrollOffset);

    // -1 so that we don't print the last \n
    for (auto i = 0; i < TerminalViewHeight + 8 - 1; i++)
    {
        termSm.ProcessString(L"x\n");
    }

    const auto secondView = term->GetViewport();

    VERIFY_ARE_EQUAL(8, secondView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight + 8, secondView.BottomExclusive());
    VERIFY_ARE_EQUAL(0, term->_scrollOffset);

    Log::Comment(L"Scroll up one line");
    term->_scrollOffset = 1;

    const auto thirdView = term->GetViewport();
    VERIFY_ARE_EQUAL(7, thirdView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight + 7, thirdView.BottomExclusive());
    VERIFY_ARE_EQUAL(1, term->_scrollOffset);

    Log::Comment(L"Print a few lines, to see that the viewport stays where it was");
    for (auto i = 0; i < 8; i++)
    {
        termSm.ProcessString(L"x\n");
    }

    const auto fourthView = term->GetViewport();
    VERIFY_ARE_EQUAL(7, fourthView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight + 7, fourthView.BottomExclusive());
    VERIFY_ARE_EQUAL(1 + 8, term->_scrollOffset);

    Log::Comment(L"Print enough lines to get the buffer just about ready to "
                 L"circle (on the next newline)");
    auto viewBottom = term->_mutableViewport.BottomInclusive();
    do
    {
        termSm.ProcessString(L"x\n");
        viewBottom = term->_mutableViewport.BottomInclusive();
    } while (viewBottom < termTb.GetSize().BottomInclusive());

    const auto fifthView = term->GetViewport();
    VERIFY_ARE_EQUAL(7, fifthView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight + 7, fifthView.BottomExclusive());
    VERIFY_ARE_EQUAL(TerminalHistoryLength - 7, term->_scrollOffset);

    Log::Comment(L"Print 3 more lines, and see that we stick to where the old "
                 L"rows now are in the buffer (after circling)");
    for (auto i = 0; i < 3; i++)
    {
        termSm.ProcessString(L"x\n");
        Log::Comment(NoThrowString().Format(
            L"_scrollOffset: %d", term->_scrollOffset));
    }
    const auto sixthView = term->GetViewport();
    VERIFY_ARE_EQUAL(4, sixthView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight + 4, sixthView.BottomExclusive());
    VERIFY_ARE_EQUAL(TerminalHistoryLength - 4, term->_scrollOffset);

    Log::Comment(L"Print 8 more lines, and see that we're now just stuck at the"
                 L"top of the buffer");
    for (auto i = 0; i < 8; i++)
    {
        termSm.ProcessString(L"x\n");
        Log::Comment(NoThrowString().Format(
            L"_scrollOffset: %d", term->_scrollOffset));
    }
    const auto seventhView = term->GetViewport();
    VERIFY_ARE_EQUAL(0, seventhView.Top());
    VERIFY_ARE_EQUAL(TerminalViewHeight, seventhView.BottomExclusive());
    VERIFY_ARE_EQUAL(TerminalHistoryLength, term->_scrollOffset);
}

void TerminalBufferTests::_SetTabStops(std::list<til::CoordType> columns, bool replace)
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    auto& cursor = termTb.GetCursor();

    const auto clearTabStops = L"\033[3g";
    const auto addTabStop = L"\033H";

    if (replace)
    {
        termSm.ProcessString(clearTabStops);
    }

    for (auto column : columns)
    {
        cursor.SetXPosition(column);
        termSm.ProcessString(addTabStop);
    }
}

std::list<til::CoordType> TerminalBufferTests::_GetTabStops()
{
    std::list<til::CoordType> columns;
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();
    const auto lastColumn = initialView.RightInclusive();
    auto& cursor = termTb.GetCursor();

    cursor.SetPosition({ 0, 0 });
    for (;;)
    {
        termSm.ProcessCharacter(L'\t');
        auto column = cursor.GetPosition().x;
        if (column >= lastColumn)
        {
            break;
        }
        columns.push_back(column);
    }

    return columns;
}

void TerminalBufferTests::TestResetClearTabStops()
{
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();

    const auto clearTabStops = L"\033[3g";
    const auto resetToInitialState = L"\033c";

    Log::Comment(L"Default tabs every 8 columns.");
    std::list<til::CoordType> expectedStops{ 8, 16, 24, 32, 40, 48, 56, 64, 72 };
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Clear all tabs.");
    termSm.ProcessString(clearTabStops);
    expectedStops = {};
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"RIS resets tabs to defaults.");
    termSm.ProcessString(resetToInitialState);
    expectedStops = { 8, 16, 24, 32, 40, 48, 56, 64, 72 };
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());
}

void TerminalBufferTests::TestAddTabStop()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    auto& cursor = termTb.GetCursor();

    const auto clearTabStops = L"\033[3g";
    const auto addTabStop = L"\033H";

    Log::Comment(L"Clear all tabs.");
    termSm.ProcessString(clearTabStops);
    std::list<til::CoordType> expectedStops{};
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Add tab to empty list.");
    cursor.SetXPosition(12);
    termSm.ProcessString(addTabStop);
    expectedStops.push_back(12);
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Add tab to head of existing list.");
    cursor.SetXPosition(4);
    termSm.ProcessString(addTabStop);
    expectedStops.push_front(4);
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Add tab to tail of existing list.");
    cursor.SetXPosition(30);
    termSm.ProcessString(addTabStop);
    expectedStops.push_back(30);
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Add tab to middle of existing list.");
    cursor.SetXPosition(24);
    termSm.ProcessString(addTabStop);
    expectedStops.push_back(24);
    expectedStops.sort();
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());

    Log::Comment(L"Add tab that duplicates an item in the existing list.");
    cursor.SetXPosition(24);
    termSm.ProcessString(addTabStop);
    VERIFY_ARE_EQUAL(expectedStops, _GetTabStops());
}

void TerminalBufferTests::TestClearTabStop()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    auto& cursor = termTb.GetCursor();

    const auto clearTabStops = L"\033[3g";
    const auto clearTabStop = L"\033[0g";
    const auto addTabStop = L"\033H";

    Log::Comment(L"Start with all tabs cleared.");
    {
        termSm.ProcessString(clearTabStops);

        VERIFY_IS_TRUE(_GetTabStops().empty());
    }

    Log::Comment(L"Try to clear nonexistent list.");
    {
        cursor.SetXPosition(0);
        termSm.ProcessString(clearTabStop);

        VERIFY_IS_TRUE(_GetTabStops().empty(), L"List should remain empty");
    }

    Log::Comment(L"Allocate 1 list item and clear it.");
    {
        cursor.SetXPosition(0);
        termSm.ProcessString(addTabStop);
        termSm.ProcessString(clearTabStop);

        VERIFY_IS_TRUE(_GetTabStops().empty());
    }

    Log::Comment(L"Allocate 1 list item and clear nonexistent.");
    {
        cursor.SetXPosition(1);
        termSm.ProcessString(addTabStop);

        Log::Comment(L"Free greater");
        cursor.SetXPosition(2);
        termSm.ProcessString(clearTabStop);
        VERIFY_IS_FALSE(_GetTabStops().empty());

        Log::Comment(L"Free less than");
        cursor.SetXPosition(0);
        termSm.ProcessString(clearTabStop);
        VERIFY_IS_FALSE(_GetTabStops().empty());

        // clear all tab stops
        termSm.ProcessString(clearTabStops);
    }

    Log::Comment(L"Allocate many (5) list items and clear head.");
    {
        std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
        _SetTabStops(inputData, false);
        cursor.SetXPosition(inputData.front());
        termSm.ProcessString(clearTabStop);

        inputData.pop_front();
        VERIFY_ARE_EQUAL(inputData, _GetTabStops());

        // clear all tab stops
        termSm.ProcessString(clearTabStops);
    }

    Log::Comment(L"Allocate many (5) list items and clear middle.");
    {
        std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
        _SetTabStops(inputData, false);
        cursor.SetXPosition(*std::next(inputData.begin()));
        termSm.ProcessString(clearTabStop);

        inputData.erase(std::next(inputData.begin()));
        VERIFY_ARE_EQUAL(inputData, _GetTabStops());

        // clear all tab stops
        termSm.ProcessString(clearTabStops);
    }

    Log::Comment(L"Allocate many (5) list items and clear tail.");
    {
        std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
        _SetTabStops(inputData, false);
        cursor.SetXPosition(inputData.back());
        termSm.ProcessString(clearTabStop);

        inputData.pop_back();
        VERIFY_ARE_EQUAL(inputData, _GetTabStops());

        // clear all tab stops
        termSm.ProcessString(clearTabStops);
    }

    Log::Comment(L"Allocate many (5) list items and clear nonexistent item.");
    {
        std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
        _SetTabStops(inputData, false);
        cursor.SetXPosition(0);
        termSm.ProcessString(clearTabStop);

        VERIFY_ARE_EQUAL(inputData, _GetTabStops());

        // clear all tab stops
        termSm.ProcessString(clearTabStops);
    }
}

void TerminalBufferTests::TestGetForwardTab()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    const auto initialView = term->GetViewport();
    auto& cursor = termTb.GetCursor();

    const auto nextForwardTab = L"\033[I";

    std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
    _SetTabStops(inputData, true);

    const auto coordScreenBufferSize = initialView.Dimensions();

    Log::Comment(L"Find next tab from before front.");
    {
        cursor.SetXPosition(0);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = inputData.front();

        termSm.ProcessString(nextForwardTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor advanced to first tab stop from sample list.");
    }

    Log::Comment(L"Find next tab from in the middle.");
    {
        cursor.SetXPosition(6);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = *std::next(inputData.begin(), 3);

        termSm.ProcessString(nextForwardTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor advanced to middle tab stop from sample list.");
    }

    Log::Comment(L"Find next tab from end.");
    {
        cursor.SetXPosition(30);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = coordScreenBufferSize.width - 1;

        termSm.ProcessString(nextForwardTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor advanced to end of screen buffer.");
    }

    Log::Comment(L"Find next tab from rightmost column.");
    {
        cursor.SetXPosition(coordScreenBufferSize.width - 1);

        auto coordCursorExpected = cursor.GetPosition();

        termSm.ProcessString(nextForwardTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor remains in rightmost column.");
    }
}

void TerminalBufferTests::TestGetReverseTab()
{
    auto& termTb = *term->_mainBuffer;
    auto& termSm = *term->_stateMachine;
    auto& cursor = termTb.GetCursor();

    const auto nextReverseTab = L"\033[Z";

    std::list<til::CoordType> inputData = { 3, 5, 6, 10, 15, 17 };
    _SetTabStops(inputData, true);

    Log::Comment(L"Find previous tab from before front.");
    {
        cursor.SetXPosition(1);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = 0;

        termSm.ProcessString(nextReverseTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor adjusted to beginning of the buffer when it started before sample list.");
    }

    Log::Comment(L"Find previous tab from in the middle.");
    {
        cursor.SetXPosition(6);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = *std::next(inputData.begin());

        termSm.ProcessString(nextReverseTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor adjusted back one tab spot from middle of sample list.");
    }

    Log::Comment(L"Find next tab from end.");
    {
        cursor.SetXPosition(30);

        auto coordCursorExpected = cursor.GetPosition();
        coordCursorExpected.x = inputData.back();

        termSm.ProcessString(nextReverseTab);
        const auto coordCursorResult = cursor.GetPosition();
        VERIFY_ARE_EQUAL(coordCursorExpected,
                         coordCursorResult,
                         L"Cursor adjusted to last item in the sample list from position beyond end.");
    }
}

void TerminalBufferTests::TestURLPatternDetection()
{
    using namespace std::string_view_literals;

    Log::Comment(L"Test working-directory reports before buffer creation");
    {
        Terminal preCreateTerminal{ Terminal::TestDummyMarker{} };
        constexpr auto PreCreateWorkingDirectory = LR"(C:\pre-create)"sv;
        preCreateTerminal.SetWorkingDirectory(PreCreateWorkingDirectory);
        DummyRenderer renderer{ &preCreateTerminal };
        preCreateTerminal.Create({ TerminalViewWidth, TerminalViewHeight }, TerminalHistoryLength, renderer);
        VERIFY_ARE_EQUAL(preCreateTerminal._getWorkingDirectoryForRow(0),
                         PreCreateWorkingDirectory,
                         L"A working directory reported before buffer creation is applied after creation.");
    }

    constexpr auto BeforeStr = L"<Before>"sv;
    constexpr auto UrlStr = L"https://www.contoso.com"sv;
    constexpr auto AfterStr = L"<After>"sv;
    constexpr auto urlStartX = BeforeStr.size();
    constexpr auto urlEndX = BeforeStr.size() + UrlStr.size() - 1;

    // This is off by default; turn it on for the test.
    auto originalDetectURLs = term->_detectURLs;
    auto restoreDetectUrls = wil::scope_exit([&]() {
        term->_detectURLs = originalDetectURLs;
    });
    term->_detectURLs = true;

    auto& termSm = *term->_stateMachine;
    termSm.ProcessString(fmt::format(FMT_COMPILE(L"{}{}{}"), BeforeStr, UrlStr, AfterStr));
    term->UpdatePatternsUnderLock();

    std::wstring result;

    result = term->GetHyperlinkAtBufferPosition(til::point{ urlStartX - 1, 0 });
    VERIFY_IS_TRUE(result.empty(), L"URL is not detected before the actual URL.");

    result = term->GetHyperlinkAtBufferPosition(til::point{ urlStartX, 0 });
    VERIFY_IS_TRUE(!result.empty(), L"A URL is detected at the start position.");
    VERIFY_ARE_EQUAL(result, UrlStr, L"Detected URL matches the given URL.");
    const auto urlMatch = term->_resolveClickableMatch(term->_getClickableMatchers().front().id, UrlStr, 0);
    VERIFY_IS_TRUE(urlMatch.has_value(), L"The URL matcher produces a click action.");
    VERIFY_IS_TRUE(urlMatch->action == ClickAction::OpenUri);
    VERIFY_ARE_EQUAL(urlMatch->target, UrlStr);

    result = term->GetHyperlinkAtBufferPosition(til::point{ urlEndX, 0 });
    VERIFY_IS_TRUE(!result.empty(), L"A URL is detected at the end position.");
    VERIFY_ARE_EQUAL(result, UrlStr, L"Detected URL matches the given URL.");

    result = term->GetHyperlinkAtBufferPosition(til::point{ urlEndX + 1, 0 });
    VERIFY_IS_TRUE(result.empty(), L"URL is not detected after the actual URL.");

    constexpr auto ExplicitFileUri = L"file:///C:/src/main.cpp#L12:3"sv;
    termSm.ProcessString(L"\r\n\x1b]8;;");
    termSm.ProcessString(ExplicitFileUri);
    termSm.ProcessString(L"\x1b\\explicit\x1b]8;;\x1b\\");
    const auto explicitFileRow = term->_mainBuffer->GetCursor().GetPosition().y;
    const auto explicitFileHyperlink = term->GetHyperlinkInfoAtBufferPosition(til::point{ 0, explicitFileRow });
    VERIFY_ARE_EQUAL(explicitFileHyperlink.uri, ExplicitFileUri, L"An explicit OSC 8 file URI keeps its fragment.");
    VERIFY_IS_FALSE(explicitFileHyperlink.isAutoDetectedFilePath,
                    L"An explicit OSC 8 file URI is not marked as a generated file-path link.");

    Log::Comment(L"Test file path detection");
    {
        static std::atomic<uint64_t> counter{ 0 };
        const auto scratchRoot = std::filesystem::temp_directory_path() /
                                 fmt::format(FMT_COMPILE(L"TerminalFileLinks-{}-{}-{}"),
                                             ::GetCurrentProcessId(),
                                             ::GetTickCount64(),
                                             counter.fetch_add(1, std::memory_order_relaxed));
        const auto firstDirectory = scratchRoot / L"first";
        const auto secondDirectory = scratchRoot / L"second";
        std::filesystem::create_directories(firstDirectory);
        std::filesystem::create_directories(secondDirectory);
        auto cleanupScratch = wil::scope_exit([&]() {
            std::error_code error;
            std::filesystem::remove_all(scratchRoot, error);
        });

        const auto createFile = [](const std::filesystem::path& path) {
            std::ofstream file{ path };
            file << "test";
        };
        createFile(firstDirectory / L"README.md");
        createFile(firstDirectory / L"release notes.txt");
        createFile(firstDirectory / L"main.cpp");
        createFile(firstDirectory / L"hash#name.cpp");
        createFile(secondDirectory / L"README.md");

        const auto pathUri = [](const std::filesystem::path& path) {
            std::wstring uri(INTERNET_MAX_URL_LENGTH, L'\0');
            auto length = gsl::narrow<DWORD>(uri.size());
            VERIFY_SUCCEEDED(UrlCreateFromPathW(path.c_str(), uri.data(), &length, 0));
            uri.resize(length);
            return uri;
        };

        term->SetWorkingDirectory(firstDirectory.native());

        constexpr auto FilePrefix = L"\r\nfiles: "sv;
        constexpr auto FileName = L"README.md"sv;
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"{}{}"), FilePrefix, FileName));
        term->UpdatePatternsUnderLock();

        const auto row = term->_mainBuffer->GetCursor().GetPosition().y;
        const auto start = FilePrefix.size() - 2;
        result = term->GetHyperlinkAtBufferPosition(til::point{ start, row });
        VERIFY_ARE_EQUAL(result, pathUri(firstDirectory / FileName), L"A listed file name resolves against the working directory.");
        const auto fileHyperlink = term->GetHyperlinkInfoAtBufferPosition(til::point{ start, row });
        VERIFY_IS_TRUE(fileHyperlink.isAutoDetectedFilePath,
                       L"A detected file path is identified for location metadata decoding.");
        const auto fileMatch = term->_resolveClickableMatch(term->_getClickableMatchers().back().id, FileName, row);
        VERIFY_IS_TRUE(fileMatch.has_value(), L"The file matcher produces a click action.");
        VERIFY_IS_TRUE(fileMatch->action == ClickAction::OpenFile);
        VERIFY_ARE_EQUAL(fileMatch->target, pathUri(firstDirectory / FileName));

        std::filesystem::remove(firstDirectory / FileName);
        const auto cachedFileMatch = term->_resolveClickableMatch(term->_getClickableMatchers().back().id, FileName, row);
        VERIFY_IS_TRUE(cachedFileMatch.has_value(), L"Detection reuses a recent positive file result.");
        const auto deletedFileActivation = term->_resolveClickableMatch(term->_getClickableMatchers().back().id,
                                                                        FileName,
                                                                        row,
                                                                        ClickResolveMode::Activate);
        VERIFY_IS_FALSE(deletedFileActivation.has_value(), L"Activation revalidates a cached file before opening it.");
        createFile(firstDirectory / FileName);
        const auto recreatedFileActivation = term->_resolveClickableMatch(term->_getClickableMatchers().back().id,
                                                                          FileName,
                                                                          row,
                                                                          ClickResolveMode::Activate);
        VERIFY_IS_TRUE(recreatedFileActivation.has_value(), L"Activation refreshes the cache after a file is recreated.");

        result = term->GetHyperlinkAtBufferPosition(til::point{ start - 1, row });
        VERIFY_IS_TRUE(result.empty(), L"Text before a file path is not linked.");

        const auto absolutePath = firstDirectory / L"main.cpp";
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"\r\n{}"), absolutePath.native()));
        term->UpdatePatternsUnderLock();
        const auto absoluteRow = term->_mainBuffer->GetCursor().GetPosition().y;
        const auto parsedAbsolutePath = Terminal::_parseFileLocation(absolutePath.native());
        VERIFY_IS_TRUE(parsedAbsolutePath.has_value());
        VERIFY_IS_FALSE(parsedAbsolutePath->line.has_value());
        const auto resolvedAbsolutePath = term->_resolveFilePath(parsedAbsolutePath->path, firstDirectory.native(), L"");
        VERIFY_IS_TRUE(resolvedAbsolutePath.has_value());
        VERIFY_ARE_EQUAL(resolvedAbsolutePath->path.native(), absolutePath.lexically_normal().native());
        const auto directAbsoluteUri = term->_getFilePathUri(parsedAbsolutePath->path, firstDirectory.native(), L"", false);
        VERIFY_ARE_EQUAL(directAbsoluteUri, pathUri(absolutePath), L"An absolute Windows path passes strict path validation.");
        const auto absoluteMatch = term->_resolveClickableMatch(term->_getClickableMatchers().back().id, absolutePath.native(), absoluteRow);
        VERIFY_IS_TRUE(absoluteMatch.has_value(), L"An absolute Windows path resolves independently of pattern scanning.");
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, absoluteRow });
        VERIFY_ARE_EQUAL(result, pathUri(absolutePath), L"An absolute Windows path becomes a file URI.");

        constexpr auto QuotedPath = LR"("release notes.txt")"sv;
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"\r\n{}"), QuotedPath));
        term->UpdatePatternsUnderLock();
        const auto quotedRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 1, quotedRow });
        VERIFY_ARE_EQUAL(result, pathUri(firstDirectory / L"release notes.txt"), L"A quoted path may contain spaces.");
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, quotedRow });
        VERIFY_IS_TRUE(result.empty(), L"Quotes are not part of the file link.");

        const auto verifyLocation = [&](const std::wstring_view displayed,
                                        const til::CoordType clickableStart,
                                        const std::wstring_view fragment,
                                        const std::optional<til::CoordType> firstExcludedColumn = std::nullopt) {
            termSm.ProcessString(fmt::format(FMT_COMPILE(L"\r\n{}"), displayed));
            term->UpdatePatternsUnderLock();
            const auto locationRow = term->_mainBuffer->GetCursor().GetPosition().y;
            result = term->GetHyperlinkAtBufferPosition(til::point{ clickableStart, locationRow });
            auto expected = pathUri(firstDirectory / L"main.cpp");
            expected.append(fragment);
            VERIFY_ARE_EQUAL(result,
                             expected,
                             fmt::format(L"File location grammar '{}' resolves to the expected URI fragment.", displayed).c_str());
            if (firstExcludedColumn)
            {
                result = term->GetHyperlinkAtBufferPosition(til::point{ *firstExcludedColumn, locationRow });
                VERIFY_IS_TRUE(result.empty(), L"Text after the meaningful file location is excluded from the clickable interval.");
            }
        };

        verifyLocation(L"main.cpp:12", 0, L"#L12");
        verifyLocation(L"main.cpp:12:3", 0, L"#L12:3");
        verifyLocation(L"main.cpp(12,3)", 0, L"#L12:3");
        verifyLocation(L"main.cpp[12,3]", 0, L"#L12:3");
        verifyLocation(LR"(File "main.cpp", line 12, in function)", 6, L"#L12", 24);
        verifyLocation(L"main.cpp#L12:3", 0, L"#L12:3");
        verifyLocation(L"main.cpp:12-20", 0, L"#L12");

        termSm.ProcessString(L"\r\n\"release notes.txt:9:2\"");
        term->UpdatePatternsUnderLock();
        const auto quotedLocationRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 1, quotedLocationRow });
        VERIFY_ARE_EQUAL(result, pathUri(firstDirectory / L"release notes.txt") + L"#L9:2", L"A quoted location may contain spaces.");
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, quotedLocationRow });
        VERIFY_IS_TRUE(result.empty(), L"Quotes remain outside a quoted file-location interval.");

        const auto drivePath = Terminal::_parseFileLocation(absolutePath.native());
        VERIFY_IS_TRUE(drivePath.has_value());
        VERIFY_ARE_EQUAL(drivePath->path, std::wstring_view{ absolutePath.native() }, L"A Windows drive colon is not parsed as a line separator.");
        VERIFY_IS_FALSE(drivePath->line.has_value());
        VERIFY_IS_FALSE(Terminal::_parseFileLocation(L"main.cpp:4294967296").has_value(), L"Overflowing line numbers are rejected.");
        VERIFY_IS_FALSE(Terminal::_parseFileLocation(L"main.cpp:0").has_value(), L"Line zero is rejected.");

        termSm.ProcessString(L"\r\nhash#name.cpp:7:2");
        term->UpdatePatternsUnderLock();
        const auto hashPathRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, hashPathRow });
        VERIFY_ARE_EQUAL(result, pathUri(firstDirectory / L"hash#name.cpp") + L"#L7:2", L"A hash in the file name remains escaped separately from the location fragment.");

        termSm.ProcessString(L"\r\n");
        term->SetWorkingDirectory(secondDirectory.native());
        term->UpdatePatternsUnderLock();
        result = term->GetHyperlinkAtBufferPosition(til::point{ start, row });
        VERIFY_ARE_EQUAL(result, pathUri(firstDirectory / FileName), L"An earlier file link retains the working directory active when it was printed.");

        constexpr auto MissingFile = L"missing-file.txt"sv;
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"{}"), MissingFile));
        term->UpdatePatternsUnderLock();
        const auto missingRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, missingRow });
        VERIFY_IS_TRUE(result.empty(), L"A nonexistent file is not linked.");
        createFile(secondDirectory / MissingFile);
        const auto cachedMissingFile = term->_resolveClickableMatch(term->_getClickableMatchers().back().id, MissingFile, missingRow);
        VERIFY_IS_FALSE(cachedMissingFile.has_value(), L"Detection reuses a recent negative file result.");
        const auto createdFileActivation = term->_resolveClickableMatch(term->_getClickableMatchers().back().id,
                                                                        MissingFile,
                                                                        missingRow,
                                                                        ClickResolveMode::Activate);
        VERIFY_IS_TRUE(createdFileActivation.has_value(), L"Activation discovers a file created after negative detection.");

        constexpr auto DottedToken = L"\r\nversion.123"sv;
        termSm.ProcessString(DottedToken);
        term->UpdatePatternsUnderLock();
        const auto dottedTokenRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, dottedTokenRow });
        VERIFY_IS_TRUE(result.empty(), L"A dotted token that is not a file is not linked.");

        const auto currentWorkingDirectoryId = term->_mainBuffer->GetWorkingDirectoryId(secondDirectory.native());
        term->_mainBuffer->GetMutableRowByOffset(0).SetWorkingDirectoryId(currentWorkingDirectoryId);
        term->_mainBuffer->GetMutableRowByOffset(1).SetWorkingDirectoryId(0);
        term->SetShellType(L"wsl:Ubuntu", L"");
        const auto currentShellTypeId = term->_mainBuffer->GetShellTypeId(L"wsl:Ubuntu");
        term->_mainBuffer->GetMutableRowByOffset(0).SetShellTypeId(currentShellTypeId);
        term->_mainBuffer->GetMutableRowByOffset(1).SetShellTypeId(0);
        term->_mainBuffer->IncrementCircularBuffer();
        VERIFY_ARE_EQUAL(term->_mainBuffer->GetRowByOffset(0).GetWorkingDirectoryId(),
                         currentWorkingDirectoryId,
                         L"Circular buffer rotation carries the active working directory forward.");
        VERIFY_ARE_EQUAL(term->_mainBuffer->GetRowByOffset(0).GetShellTypeId(),
                         currentShellTypeId,
                         L"Circular buffer rotation carries the active shell type forward.");

        term->UseAlternateScreenBuffer(TextAttribute{});
        VERIFY_ARE_EQUAL(term->_getWorkingDirectoryForRow(0),
                         secondDirectory.native(),
                         L"The alternate buffer inherits the active working directory from its first row.");
        VERIFY_ARE_EQUAL(term->_getShellTypeForRow(0),
                         L"wsl:Ubuntu"sv,
                         L"The alternate buffer inherits the active shell type from its first row.");
        term->SetWorkingDirectory(firstDirectory.native());
        term->SetShellType(L"bash", L"");
        term->UseMainScreenBuffer();
        VERIFY_ARE_EQUAL(term->_getWorkingDirectoryForRow(term->_mainBuffer->GetCursor().GetPosition().y),
                         firstDirectory.native(),
                         L"The main buffer receives working-directory changes made in the alternate buffer.");
        VERIFY_ARE_EQUAL(term->_getShellTypeForRow(term->_mainBuffer->GetCursor().GetPosition().y),
                         L"bash"sv,
                         L"The main buffer receives shell changes made in the alternate buffer.");

        term->SetShellType(L"bash", L"");
        termSm.ProcessString(L"\r\nhistorical-shell");
        const auto bashRow = term->_mainBuffer->GetCursor().GetPosition().y;
        termSm.ProcessString(L"\r\n");
        term->SetShellType(L"wsl:Ubuntu", L"");
        termSm.ProcessString(L"wsl-shell");
        const auto wslRow = term->_mainBuffer->GetCursor().GetPosition().y;
        termSm.ProcessString(L"\r\n");
        term->SetShellType(L"bash", L"");
        VERIFY_ARE_EQUAL(term->_getShellTypeForRow(bashRow), L"bash"sv, L"Earlier rows retain their historical shell identity.");
        VERIFY_ARE_EQUAL(term->_getShellTypeForRow(wslRow), L"wsl:Ubuntu"sv, L"Shell changes are bound to the row where they were reported.");

        const auto nativeAbsolute = absolutePath.lexically_normal();
        auto posixDrivePath = nativeAbsolute.native();
        const auto driveLetter = static_cast<wchar_t>(std::towlower(posixDrivePath[0]));
        posixDrivePath.erase(0, 2);
        std::replace(posixDrivePath.begin(), posixDrivePath.end(), L'\\', L'/');
        posixDrivePath.insert(0, fmt::format(L"/mnt/{}", driveLetter));

        const auto originalCursorPosition = term->_mainBuffer->GetCursor().GetPosition();
        const auto inheritedAltCursorPosition = til::point{ 0, term->_mutableViewport.Top() + 5 };

        term->SetWorkingDirectory(firstDirectory.native());
        term->SetShellType(L"bash", L"");
        term->_mainBuffer->GetCursor().SetPosition(inheritedAltCursorPosition);
        term->UseAlternateScreenBuffer(TextAttribute{});
        term->_altBuffer->GetCursor().SetPosition({ 0, 1 });
        termSm.ProcessString(L"README.md");
        term->UpdatePatternsUnderLock();
        result = term->GetHyperlinkAtBufferPosition({ 0, 1 });
        VERIFY_ARE_EQUAL(result,
                         pathUri(firstDirectory / L"README.md"),
                         L"A relative path above the inherited alternate-buffer cursor uses the inherited working directory.");
        term->UseMainScreenBuffer();
        term->_mainBuffer->GetCursor().SetPosition(originalCursorPosition);

        auto posixWorkingDirectory = firstDirectory.lexically_normal().native();
        posixWorkingDirectory.erase(0, 2);
        std::replace(posixWorkingDirectory.begin(), posixWorkingDirectory.end(), L'\\', L'/');
        posixWorkingDirectory.insert(0, fmt::format(L"/mnt/{}", driveLetter));
        term->SetWorkingDirectory(posixWorkingDirectory);
        term->SetShellType(L"wsl:Ubuntu", L"");
        term->_mainBuffer->GetCursor().SetPosition(inheritedAltCursorPosition);
        term->UseAlternateScreenBuffer(TextAttribute{});
        term->_altBuffer->GetCursor().SetPosition({ 0, 1 });
        termSm.ProcessString(L"main.cpp");
        term->UpdatePatternsUnderLock();
        result = term->GetHyperlinkAtBufferPosition({ 0, 1 });
        VERIFY_ARE_EQUAL(result,
                         pathUri(nativeAbsolute),
                         L"A relative WSL path above the inherited alternate-buffer cursor uses inherited CWD and shell metadata.");
        term->UseMainScreenBuffer();
        term->_mainBuffer->GetCursor().SetPosition(originalCursorPosition);

        term->SetShellType(L"wsl:Ubuntu", L"");
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"\r\n{}"), posixDrivePath));
        const auto historicalWslRow = term->_mainBuffer->GetCursor().GetPosition().y;
        termSm.ProcessString(L"\r\n");
        term->SetShellType(L"bash", L"");
        term->UpdatePatternsUnderLock();
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, historicalWslRow });
        VERIFY_ARE_EQUAL(result, pathUri(nativeAbsolute), L"Detection resolves a candidate with the historical shell for its row, not the current shell.");

        const auto wslDrive = term->_resolveFilePath(posixDrivePath, L"", L"wsl:Ubuntu");
        VERIFY_IS_TRUE(wslDrive.has_value());
        VERIFY_ARE_EQUAL(wslDrive->path.native(), nativeAbsolute.native(), L"WSL /mnt drive paths map to native Windows paths.");
        VERIFY_IS_FALSE(wslDrive->trustedWslProvider);
        VERIFY_ARE_EQUAL(term->_getFilePathUri(posixDrivePath, L"", L"wsl:Ubuntu", false),
                         pathUri(nativeAbsolute),
                         L"Translated WSL drive paths retain strict filesystem validation.");

        const auto wslUnc = term->_resolveFilePath(L"/home/user/main.cpp", L"", L"wsl:Ubuntu");
        VERIFY_IS_TRUE(wslUnc.has_value());
        VERIFY_ARE_EQUAL(wslUnc->path.native(), LR"(\\wsl$\Ubuntu\home\user\main.cpp)"sv, L"WSL distro paths map to the trusted WSL provider.");
        VERIFY_IS_TRUE(wslUnc->trustedWslProvider);

        const auto wslRelative = term->_resolveFilePath(L"main.cpp", L"/home/user/project", L"wsl:Ubuntu");
        VERIFY_IS_TRUE(wslRelative.has_value());
        VERIFY_ARE_EQUAL(wslRelative->path.native(), LR"(\\wsl$\Ubuntu\home\user\project\main.cpp)"sv, L"Relative WSL paths resolve in POSIX space before conversion.");

        term->SetPathTranslationStyle(PathTranslationStyle::MSYS2);
        auto translatedPath = fmt::format(L"/{}{}", driveLetter, posixDrivePath.substr(6));
        const auto msysPath = term->_resolveFilePath(translatedPath, L"", L"bash");
        VERIFY_IS_TRUE(msysPath.has_value());
        VERIFY_ARE_EQUAL(msysPath->path.native(), nativeAbsolute.native(), L"MSYS2 /c paths map to native Windows paths.");

        term->SetPathTranslationStyle(PathTranslationStyle::Cygwin);
        translatedPath = fmt::format(L"/cygdrive/{}{}", driveLetter, posixDrivePath.substr(6));
        const auto cygwinPath = term->_resolveFilePath(translatedPath, L"", L"bash");
        VERIFY_IS_TRUE(cygwinPath.has_value());
        VERIFY_ARE_EQUAL(cygwinPath->path.native(), nativeAbsolute.native(), L"Cygwin /cygdrive/c paths map to native Windows paths.");

        term->SetPathTranslationStyle(PathTranslationStyle::WSL);
        const auto profileWslDrive = term->_resolveFilePath(posixDrivePath, L"", L"bash");
        VERIFY_IS_TRUE(profileWslDrive.has_value());
        VERIFY_ARE_EQUAL(profileWslDrive->path.native(), nativeAbsolute.native(), L"WSL profile style maps /mnt drives without guessing a distro.");
        VERIFY_IS_FALSE(term->_resolveFilePath(L"/home/user/main.cpp", L"", L"bash").has_value(), L"WSL profile style does not guess a distro for provider paths.");

        term->SetPathTranslationStyle(PathTranslationStyle::MinGW);
        const auto mingwNative = term->_resolveFilePath(nativeAbsolute.generic_wstring(), L"", L"bash");
        VERIFY_IS_TRUE(mingwNative.has_value());
        VERIFY_ARE_EQUAL(mingwNative->path.native(), nativeAbsolute.native(), L"MinGW accepts its native C:/ path form.");
        translatedPath = fmt::format(L"/{}{}", driveLetter, posixDrivePath.substr(6));
        VERIFY_IS_FALSE(term->_resolveFilePath(translatedPath, L"", L"bash").has_value(), L"MinGW does not reinterpret /c paths, matching existing profile semantics.");

        term->SetPathTranslationStyle(PathTranslationStyle::None);
        VERIFY_IS_TRUE(term->_getFilePathUri(LR"(\\server\share\main.cpp)", L"", L"", false).empty(), L"Arbitrary UNC paths remain rejected.");

        constexpr auto UrlWithFileName = L"https://www.contoso.com/?file=README.md"sv;
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"\r\n{}"), UrlWithFileName));
        term->UpdatePatternsUnderLock();
        const auto urlRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ gsl::narrow<til::CoordType>(UrlWithFileName.size() - 1), urlRow });
        VERIFY_ARE_EQUAL(result, UrlWithFileName, L"URL detection takes precedence over an overlapping file name.");
    }

    Log::Comment(L"Test wrapped URL detection");
    {
        // Build a URL longer than the terminal width so it wraps
        // Terminal is 80 cols wide; pad before + URL must exceed 80
        constexpr auto WrapBefore = L"WRAP>"sv;
        const std::wstring longUrl = L"https://www.contoso.com/this-is-a-very-long-path/that-will-wrap-across-multiple-rows-in-the-terminal-buffer";
        const auto wrapUrlStartX = static_cast<til::CoordType>(WrapBefore.size());
        const auto totalLen = WrapBefore.size() + longUrl.size();
        // The URL should wrap to row 1
        VERIFY_IS_TRUE(totalLen > static_cast<size_t>(TerminalViewWidth), L"URL must exceed terminal width to wrap");

        // Move cursor to row 2 and write the wrapped URL
        termSm.ProcessString(L"\r\n\r\n");
        termSm.ProcessString(fmt::format(FMT_COMPILE(L"{}{}"), WrapBefore, longUrl));
        term->UpdatePatternsUnderLock();

        const auto cursorRow = term->_mainBuffer->GetCursor().GetPosition().y;
        const auto wrapRow0 = cursorRow - 1; // first row of wrapped text
        const auto wrapRow1 = cursorRow; // second row

        // Detect from start of URL (first row)
        result = term->GetHyperlinkAtBufferPosition(til::point{ wrapUrlStartX, wrapRow0 });
        VERIFY_IS_TRUE(!result.empty(), L"Wrapped URL is detected on the first row.");
        VERIFY_ARE_EQUAL(result, longUrl, L"Full wrapped URL is returned from first row.");

        // Detect from second row of URL
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, wrapRow1 });
        VERIFY_IS_TRUE(!result.empty(), L"Wrapped URL is detected on the second row.");
        VERIFY_ARE_EQUAL(result, longUrl, L"Full wrapped URL is returned from second row.");

        // Before the URL on the first row
        result = term->GetHyperlinkAtBufferPosition(til::point{ wrapUrlStartX - 1, wrapRow0 });
        VERIFY_IS_TRUE(result.empty(), L"URL is not detected before the wrapped URL.");
    }

    Log::Comment(L"Test URL detection after scrolling into history");
    {
        // Generate enough output to push the URLs into scrollback
        for (auto i = 0; i < TerminalViewHeight + 8; i++)
        {
            termSm.ProcessString(L"filler\r\n");
        }

        // Write a new URL at the current cursor position
        constexpr auto ScrollUrl = L"https://www.example.com/scrolled"sv;
        termSm.ProcessString(ScrollUrl);
        term->UpdatePatternsUnderLock();

        const auto scrollUrlRow = term->_mainBuffer->GetCursor().GetPosition().y;
        result = term->GetHyperlinkAtBufferPosition(til::point{ 0, scrollUrlRow });
        VERIFY_IS_TRUE(!result.empty(), L"URL is detected after scrolling.");
        VERIFY_ARE_EQUAL(result, ScrollUrl, L"Correct URL is returned after scrolling.");
    }

    Log::Comment(L"Test viewport-relative interval coordinates");
    {
        constexpr auto VpUrl = L"https://www.example.com/viewport"sv;
        termSm.ProcessString(L"\r\n");
        termSm.ProcessString(VpUrl);
        term->UpdatePatternsUnderLock();

        const auto vpUrlRow = term->_mainBuffer->GetCursor().GetPosition().y;
        const auto visStart = term->_VisibleStartIndex();
        const auto viewportRow = vpUrlRow - visStart;

        auto interval = term->GetHyperlinkIntervalFromViewportPosition(til::point{ 0, viewportRow });
        VERIFY_IS_TRUE(interval.has_value(), L"Interval is found via viewport position.");
        VERIFY_ARE_EQUAL(interval->start.y, viewportRow, L"Interval start row is viewport-relative.");
        VERIFY_ARE_EQUAL(interval->start.x, 0, L"Interval starts at column 0.");
    }
}

void TerminalBufferTests::TestHoverPathResolution()
{
    static std::atomic<uint64_t> counter{ 0 };
    const auto scratchRoot = std::filesystem::current_path() /
                             fmt::format(FMT_COMPILE(L"TerminalHoverPaths-{}-{}-{}"),
                                         ::GetCurrentProcessId(),
                                         ::GetTickCount64(),
                                         counter.fetch_add(1, std::memory_order_relaxed));
    const auto firstDirectory = scratchRoot / L"first";
    const auto secondDirectory = scratchRoot / L"second";
    std::filesystem::create_directories(firstDirectory / L"Documents");
    std::filesystem::create_directories(firstDirectory / L".vscode");
    std::filesystem::create_directories(firstDirectory / L"Saved Games");
    std::filesystem::create_directories(firstDirectory / L"OneDrive - Microsoft");
    std::filesystem::create_directories(firstDirectory / L"nested" / L"bare" / L"path");
    std::filesystem::create_directories(firstDirectory / L"a" / L"b" / L"c" / L"d" / L"e" / L"f" / L"g" / L"h" / L"i");
    std::filesystem::create_directories(firstDirectory / L"Games");
    std::filesystem::create_directories(secondDirectory);
    auto cleanupScratch = wil::scope_exit([&]() {
        std::error_code error;
        std::filesystem::remove_all(scratchRoot, error);
    });

    const auto createFile = [](const std::filesystem::path& path) {
        std::ofstream file{ path };
        file << "test";
    };
    createFile(firstDirectory / L"README");
    createFile(firstDirectory / L"LICENSE");
    createFile(firstDirectory / L"tool");
    createFile(firstDirectory / L"release notes.txt");

    const auto pathUri = [](const std::filesystem::path& path) {
        std::wstring uri(INTERNET_MAX_URL_LENGTH, L'\0');
        auto length = gsl::narrow<DWORD>(uri.size());
        VERIFY_SUCCEEDED(UrlCreateFromPathW(path.c_str(), uri.data(), &length, 0));
        uri.resize(length);
        return uri;
    };

    term->_detectURLs = true;
    term->SetWorkingDirectory(firstDirectory.native());
    auto& termSm = *term->_stateMachine;
    uint64_t requestId = 1;

    const auto snapshotAt = [&](const til::point bufferPosition) {
        const auto visibleStart = term->_VisibleStartIndex();
        const til::point viewportPosition{ bufferPosition.x, bufferPosition.y - visibleStart };
        VERIFY_IS_TRUE(viewportPosition.y >= 0 && viewportPosition.y < TerminalViewHeight);
        return term->CreateHoverPathRequest(viewportPosition, requestId++);
    };

    const auto writeLine = [&](const std::wstring_view text) {
        termSm.ProcessString(L"\r\n");
        termSm.ProcessString(text);
        return term->_mainBuffer->GetCursor().GetPosition().y;
    };

    const auto verifyResolved = [&](const std::wstring_view displayed,
                                    const til::CoordType hoverColumn,
                                    const std::wstring_view expectedCandidate,
                                    const std::filesystem::path& expectedPath) {
        const auto row = writeLine(displayed);
        const auto request = snapshotAt({ hoverColumn, row });
        VERIFY_IS_TRUE(request.has_value());
        VERIFY_IS_TRUE(request->candidates.size() <= Terminal::HoverPathMaxCandidates);
        for (const auto& candidate : request->candidates)
        {
            VERIFY_IS_TRUE(candidate.interval.start <= request->bufferPosition &&
                               request->bufferPosition < candidate.interval.end,
                           L"Every candidate contains the hovered point.");
        }

        const auto result = Terminal::ResolveHoverPathRequest(*request);
        VERIFY_IS_TRUE(result.has_value());
        VERIFY_ARE_EQUAL(request->candidates[result->candidateIndex].text, expectedCandidate);
        VERIFY_ARE_EQUAL(result->uri, pathUri(expectedPath));
        return std::pair{ *request, *result };
    };

    const auto dottedDirectoryRow = writeLine(L".vscode");
    term->UpdatePatternsUnderLock();
    VERIFY_ARE_EQUAL(term->GetHyperlinkAtBufferPosition({ 1, dottedDirectoryRow }),
                     pathUri(firstDirectory / L".vscode"),
                     L"The existing regex pipeline continues to own extension-like directory names.");

    verifyResolved(L"Documents", 2, L"Documents", firstDirectory / L"Documents");
    verifyResolved(L"README", 2, L"README", firstDirectory / L"README");
    verifyResolved(L"LICENSE", 2, L"LICENSE", firstDirectory / L"LICENSE");
    verifyResolved(L"Saved Games", 7, L"Saved Games", firstDirectory / L"Saved Games");
    verifyResolved(L"OneDrive - Microsoft", 11, L"OneDrive - Microsoft", firstDirectory / L"OneDrive - Microsoft");
    verifyResolved(L"nested\\bare\\path", 8, L"nested\\bare\\path", firstDirectory / L"nested" / L"bare" / L"path");
    verifyResolved(L"a\\b\\c\\d\\e\\f\\g\\h\\i",
                   16,
                   L"a\\b\\c\\d\\e\\f\\g\\h\\i",
                   firstDirectory / L"a" / L"b" / L"c" / L"d" / L"e" / L"f" / L"g" / L"h" / L"i");

    const auto unquotedLocationRow = writeLine(L"release notes.txt:9:2");
    const auto unquotedLocationRequest = snapshotAt({ 5, unquotedLocationRow });
    VERIFY_IS_TRUE(unquotedLocationRequest.has_value());
    const auto unquotedLocationResult = Terminal::ResolveHoverPathRequest(*unquotedLocationRequest);
    VERIFY_IS_TRUE(unquotedLocationResult.has_value());
    VERIFY_ARE_EQUAL(unquotedLocationRequest->candidates[unquotedLocationResult->candidateIndex].text,
                     std::wstring{ L"release notes.txt:9:2" });
    VERIFY_ARE_EQUAL(unquotedLocationResult->uri,
                     pathUri(firstDirectory / L"release notes.txt") + L"#L9:2");

    const auto [longestRequest, longestResult] = verifyResolved(L"Saved Games", 7, L"Saved Games", firstDirectory / L"Saved Games");
    VERIFY_ARE_EQUAL(longestRequest.candidates[longestResult.candidateIndex].text,
                     std::wstring{ L"Saved Games" },
                     L"The longest existing candidate wins even when a shorter suffix also exists.");

    const auto tableRow = writeLine(L"d-----  8/22/2026  12:34 AM       Documents");
    const auto tableRequest = snapshotAt({ 39, tableRow });
    VERIFY_IS_TRUE(tableRequest.has_value());
    const auto tableResult = Terminal::ResolveHoverPathRequest(*tableRequest);
    VERIFY_IS_TRUE(tableResult.has_value());
    VERIFY_ARE_EQUAL(tableRequest->candidates[tableResult->candidateIndex].text,
                     std::wstring{ L"Documents" },
                     L"PowerShell table columns are excluded by aligned whitespace.");

    const auto proseRow = writeLine(L"this arbitrary prose does not exist");
    const auto proseRequest = snapshotAt({ 16, proseRow });
    VERIFY_IS_TRUE(proseRequest.has_value());
    VERIFY_IS_FALSE(Terminal::ResolveHoverPathRequest(*proseRequest).has_value(),
                    L"Arbitrary prose is rejected when no filesystem target exists.");

    termSm.ProcessString(L"\r\n");
    term->_mainBuffer->GetCursor().SetXPosition(75);
    termSm.ProcessString(L"Saved Games");
    const auto wrappedRow = term->_mainBuffer->GetCursor().GetPosition().y;
    const auto wrappedRequest = snapshotAt({ 2, wrappedRow });
    VERIFY_IS_TRUE(wrappedRequest.has_value());
    const auto wrappedResult = Terminal::ResolveHoverPathRequest(*wrappedRequest);
    VERIFY_IS_TRUE(wrappedResult.has_value());
    VERIFY_ARE_EQUAL(wrappedRequest->candidates[wrappedResult->candidateIndex].text,
                     std::wstring{ L"Saved Games" },
                     L"A bare path split across soft-wrapped rows is reconstructed.");
    VERIFY_ARE_EQUAL(wrappedResult->uri, pathUri(firstDirectory / L"Saved Games"));

    term->SetWorkingDirectory(firstDirectory.native());
    const auto historicalRow = writeLine(L"LICENSE");
    termSm.ProcessString(L"\r\n");
    term->SetWorkingDirectory(secondDirectory.native());
    const auto historicalRequest = snapshotAt({ 2, historicalRow });
    VERIFY_IS_TRUE(historicalRequest.has_value());
    const auto historicalResult = Terminal::ResolveHoverPathRequest(*historicalRequest);
    VERIFY_IS_TRUE(historicalResult.has_value());
    VERIFY_ARE_EQUAL(historicalResult->uri,
                     pathUri(firstDirectory / L"LICENSE"),
                     L"A hover snapshot uses the working directory recorded for the historical row.");

    const auto nativeTool = (firstDirectory / L"tool").lexically_normal();
    auto posixWorkingDirectory = firstDirectory.lexically_normal().native();
    const auto driveLetter = static_cast<wchar_t>(std::towlower(posixWorkingDirectory[0]));
    posixWorkingDirectory.erase(0, 2);
    std::replace(posixWorkingDirectory.begin(), posixWorkingDirectory.end(), L'\\', L'/');
    posixWorkingDirectory.insert(0, fmt::format(L"/mnt/{}", driveLetter));
    term->SetWorkingDirectory(posixWorkingDirectory);
    term->SetShellType(L"wsl:Ubuntu", L"");
    const auto historicalWslRow = writeLine(L"tool");
    termSm.ProcessString(L"\r\n");
    term->SetShellType(L"bash", L"");
    const auto historicalWslRequest = snapshotAt({ 1, historicalWslRow });
    VERIFY_IS_TRUE(historicalWslRequest.has_value());
    VERIFY_ARE_EQUAL(historicalWslRequest->candidates.front().shellType,
                     std::wstring{ L"wsl:Ubuntu" },
                     L"The hover snapshot carries the historical WSL identity.");
    const auto historicalWslResult = Terminal::ResolveHoverPathRequest(*historicalWslRequest);
    VERIFY_IS_TRUE(historicalWslResult.has_value());
    VERIFY_ARE_EQUAL(historicalWslResult->uri, pathUri(nativeTool));

    auto nativeToolPosix = nativeTool.native();
    nativeToolPosix.erase(0, 2);
    std::replace(nativeToolPosix.begin(), nativeToolPosix.end(), L'\\', L'/');

    term->SetWorkingDirectory(firstDirectory.native());
    term->SetShellType(L"bash", L"");
    term->SetPathTranslationStyle(PathTranslationStyle::MSYS2);
    const auto msysText = fmt::format(L"/{}{}", driveLetter, nativeToolPosix);
    const auto msysRow = writeLine(msysText);
    const auto msysRequest = snapshotAt({ 1, msysRow });
    VERIFY_IS_TRUE(msysRequest.has_value());
    const auto msysResult = Terminal::ResolveHoverPathRequest(*msysRequest);
    VERIFY_IS_TRUE(msysResult.has_value());
    VERIFY_ARE_EQUAL(msysResult->uri, pathUri(nativeTool));

    term->SetPathTranslationStyle(PathTranslationStyle::Cygwin);
    const auto cygwinText = fmt::format(L"/cygdrive/{}{}", driveLetter, nativeToolPosix);
    const auto cygwinRow = writeLine(cygwinText);
    const auto cygwinRequest = snapshotAt({ 2, cygwinRow });
    VERIFY_IS_TRUE(cygwinRequest.has_value());
    const auto cygwinResult = Terminal::ResolveHoverPathRequest(*cygwinRequest);
    VERIFY_IS_TRUE(cygwinResult.has_value());
    VERIFY_ARE_EQUAL(cygwinResult->uri, pathUri(nativeTool));

    term->SetPathTranslationStyle(PathTranslationStyle::None);
    term->SetWorkingDirectory(firstDirectory.native());
    const auto activationRow = writeLine(L"Documents");
    auto activationRequest = snapshotAt({ 2, activationRow });
    VERIFY_IS_TRUE(activationRequest.has_value());
    auto activationResult = Terminal::ResolveHoverPathRequest(*activationRequest);
    VERIFY_IS_TRUE(activationResult.has_value());
    VERIFY_IS_TRUE(term->ApplyHoverPathResult(*activationRequest, *activationResult));
    const auto activationInfo = term->GetHyperlinkInfoAtBufferPosition({ 2, activationRow });
    VERIFY_IS_TRUE(activationInfo.isAutoDetectedFilePath);
    VERIFY_ARE_EQUAL(activationInfo.uri, pathUri(firstDirectory / L"Documents"));
    const auto activationViewportRow = activationRow - term->_VisibleStartIndex();
    VERIFY_IS_FALSE(term->GetPatternId({ 2, activationViewportRow }).empty(),
                    L"The renderer sees the hover-only path as a pattern and underlines it.");
    VERIFY_IS_TRUE(term->GetPatternId({ 20, activationViewportRow }).empty(),
                   L"Cells outside the hover-only interval remain ordinary text.");
    VERIFY_IS_TRUE(term->ClearHoverPath(), L"Pointer exit clears the resolved hover-only link.");
    VERIFY_IS_TRUE(term->GetHyperlinkInfoAtBufferPosition({ 2, activationRow }).uri.empty());
    VERIFY_IS_TRUE(term->GetPatternId({ 2, activationViewportRow }).empty(),
                   L"Clearing the hover-only link also removes its renderer pattern.");

    activationRequest = snapshotAt({ 2, activationRow });
    activationResult = Terminal::ResolveHoverPathRequest(*activationRequest);
    VERIFY_IS_TRUE(activationResult.has_value());
    termSm.ProcessString(L"x");
    VERIFY_IS_FALSE(term->ApplyHoverPathResult(*activationRequest, *activationResult),
                    L"A buffer mutation makes an asynchronous completion stale.");

    const auto generationRow = writeLine(L"Documents");
    auto generationRequest = snapshotAt({ 2, generationRow });
    VERIFY_IS_TRUE(generationRequest.has_value());
    auto generationResult = Terminal::ResolveHoverPathRequest(*generationRequest);
    VERIFY_IS_TRUE(generationResult.has_value());
    term->SetPathTranslationStyle(PathTranslationStyle::MSYS2);
    VERIFY_IS_FALSE(term->ApplyHoverPathResult(*generationRequest, *generationResult),
                    L"A path-context generation change makes an asynchronous completion stale.");
}

void TerminalBufferTests::TestHoverPathBoundsAndCache()
{
    static std::atomic<uint64_t> counter{ 0 };
    const auto scratchRoot = std::filesystem::current_path() /
                             fmt::format(FMT_COMPILE(L"TerminalHoverCache-{}-{}-{}"),
                                         ::GetCurrentProcessId(),
                                         ::GetTickCount64(),
                                         counter.fetch_add(1, std::memory_order_relaxed));
    std::filesystem::create_directories(scratchRoot / L"Documents");
    auto cleanupScratch = wil::scope_exit([&]() {
        std::error_code error;
        std::filesystem::remove_all(scratchRoot, error);
    });

    const auto createFile = [](const std::filesystem::path& path) {
        std::ofstream file{ path };
        file << "test";
    };
    createFile(scratchRoot / L"README");

    Terminal::HoverPathRequest request{
        .translationStyle = PathTranslationStyle::None,
    };
    request.candidates.emplace_back(Terminal::HoverPathCandidate{
        .text = L"README",
        .workingDirectory = scratchRoot.native(),
    });

    Terminal::HoverPathCache cache;
    const auto now = std::chrono::steady_clock::now();
    const auto cachedPositive = Terminal::ResolveHoverPathRequest(request, &cache, now);
    VERIFY_IS_TRUE(cachedPositive.has_value());
    std::filesystem::remove(scratchRoot / L"README");
    VERIFY_IS_TRUE(Terminal::ResolveHoverPathRequest(request, &cache, now + std::chrono::seconds{ 1 }).has_value(),
                   L"A positive hover result is cached for five seconds.");
    VERIFY_IS_FALSE(Terminal::ResolveHoverPathRequest(request).has_value(),
                    L"Activation bypasses the hover cache and revalidates synchronously.");

    request.candidates.front().text = L"missing";
    VERIFY_IS_FALSE(Terminal::ResolveHoverPathRequest(request, &cache, now).has_value());
    createFile(scratchRoot / L"missing");
    VERIFY_IS_FALSE(Terminal::ResolveHoverPathRequest(request, &cache, now + std::chrono::milliseconds{ 500 }).has_value(),
                    L"A negative hover result is cached for one second.");
    VERIFY_IS_TRUE(Terminal::ResolveHoverPathRequest(request, &cache, now + std::chrono::seconds{ 2 }).has_value(),
                   L"An expired negative cache entry is revalidated.");

    request.candidates.front().text = L"Documents:12";
    VERIFY_IS_FALSE(Terminal::ResolveHoverPathRequest(request).has_value(),
                    L"Directories are not valid line or column targets.");

    for (size_t index = 0; index < 600; ++index)
    {
        cache.Store(std::to_wstring(index), {}, now);
    }
    VERIFY_ARE_EQUAL(cache.Size(), size_t{ 512 }, L"The hover cache remains bounded.");

    term->_detectURLs = true;
    term->SetWorkingDirectory(scratchRoot.native());
    std::wstring longLogicalLine(Terminal::HoverPathMaxScanCells * 3, L'a');
    term->_stateMachine->ProcessString(longLogicalLine);

    const auto cursor = term->_mainBuffer->GetCursor().GetPosition();
    const auto visibleStart = term->_VisibleStartIndex();
    const til::point viewportPosition{ std::max<til::CoordType>(0, cursor.x - 1), cursor.y - visibleStart };
    const auto boundedRequest = term->CreateHoverPathRequest(viewportPosition, 1);
    VERIFY_IS_TRUE(boundedRequest.has_value());
    VERIFY_IS_TRUE(boundedRequest->scannedCellCount <= Terminal::HoverPathMaxScanCells,
                   L"Candidate extraction scans a fixed number of nearby cells.");
    VERIFY_IS_TRUE(boundedRequest->scannedRowCount < gsl::narrow<size_t>(term->_mainBuffer->GetSize().Height()),
                   L"Long scrollback does not cause a whole-buffer scan.");
    VERIFY_IS_TRUE(boundedRequest->contextRowCount <= Terminal::HoverPathMaxScanCells,
                   L"Historical context lookup is bounded.");
    VERIFY_IS_TRUE(boundedRequest->candidates.size() <= Terminal::HoverPathMaxCandidates,
                   L"Candidate combinations have a hard cap.");

    Terminal longScrollbackTerminal{ Terminal::TestDummyMarker{} };
    DummyRenderer longScrollbackRenderer{ &longScrollbackTerminal };
    longScrollbackTerminal.Create({ 20, 2 }, 6000, longScrollbackRenderer);
    longScrollbackTerminal._detectURLs = true;
    longScrollbackTerminal.SetWorkingDirectory(scratchRoot.native());
    VERIFY_IS_TRUE(longScrollbackTerminal._mainBuffer->GetRowByOffset(0).GetWorkingDirectoryId() != 0);
    std::wstring scrollback;
    scrollback.reserve(10000);
    for (size_t index = 0; index < 5000; ++index)
    {
        scrollback.append(L"\r\n");
    }
    longScrollbackTerminal._stateMachine->ProcessString(scrollback);
    longScrollbackTerminal._stateMachine->ProcessString(L"Documents");
    const auto longCursor = longScrollbackTerminal._mainBuffer->GetCursor().GetPosition();
    VERIFY_IS_TRUE(longScrollbackTerminal._mainBuffer->GetRowByOffset(longCursor.y).GetWorkingDirectoryId() != 0,
                   L"Line feeds propagate historical path context to newly produced rows.");
    const til::point longViewportPosition{ 2, longCursor.y - longScrollbackTerminal._VisibleStartIndex() };
    const auto longRequest = longScrollbackTerminal.CreateHoverPathRequest(longViewportPosition, 2);
    VERIFY_IS_TRUE(longRequest.has_value());
    VERIFY_ARE_EQUAL(longRequest->contextRowCount,
                     size_t{ 1 },
                     L"Eager row-context inheritance avoids scanning long scrollback for historical CWD.");
    VERIFY_IS_TRUE(longRequest->scannedRowCount <
                   gsl::narrow<size_t>(longScrollbackTerminal._mainBuffer->GetSize().Height()));
}
