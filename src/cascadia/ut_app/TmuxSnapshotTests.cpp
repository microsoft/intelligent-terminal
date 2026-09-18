// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/TmuxPaneState.h"
#include "../TerminalApp/TmuxPaneConnection.h"
#include "../TerminalCore/Terminal.hpp"
#include "../../renderer/inc/DummyRenderer.hpp"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;
using Microsoft::Terminal::Core::Terminal;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::string PromptSnapshot(const uint32_t columns, const uint32_t cursorColumn)
        {
            const auto prompt = std::string(67, 'p') + "$ ";
            const auto occupiedRows = (prompt.size() + columns - 1) / columns;
            const auto capture = prompt + std::string(6 - occupiedRows, '\n');
            const auto state = ParsePaneState(std::to_string(cursorColumn) + " " + std::to_string(occupiedRows - 1) +
                                              " 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 5 1");
            return RestorePaneState(state, {}, capture);
        }

        void VerifyPrompt(const Terminal& terminal, const til::point expected)
        {
            const auto actual = terminal.GetViewportRelativeCursorPosition();
            VERIFY_ARE_EQUAL(expected.x, actual.x);
            VERIFY_ARE_EQUAL(expected.y, actual.y);
        }
    }

    class TmuxSnapshotTests
    {
        TEST_CLASS(TmuxSnapshotTests);
        TEST_METHOD(RestoresUnwrappedPromptAtBackendSize);
        TEST_METHOD(RestoresWrappedPromptAtBackendSize);
        TEST_METHOD(ResizingAfterWrongWidthReplayCannotRepairCursor);
        TEST_METHOD(PreservesIntentionalCursorInsidePrompt);
        TEST_METHOD(WaitsForMatchingViewportBeforeReplayingSnapshot);
        TEST_METHOD(RestoresPromptAfterCapturedHistory);
    };

    void TmuxSnapshotTests::RestoresUnwrappedPromptAtBackendSize()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 80, 6 }, 100, renderer);
        terminal.Write(til::u8u16(PromptSnapshot(80, 69)));
        VerifyPrompt(terminal, { 69, 0 });
        const auto text = terminal.GetTextBuffer().GetRowByOffset(terminal.ViewStartIndex()).GetText();
        VERIFY_IS_TRUE(text.starts_with(std::wstring(67, L'p') + L"$ "));
    }

    void TmuxSnapshotTests::RestoresWrappedPromptAtBackendSize()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 40, 6 }, 100, renderer);
        terminal.Write(til::u8u16(PromptSnapshot(40, 29)));
        VerifyPrompt(terminal, { 29, 1 });
        const auto text = terminal.GetTextBuffer().GetRowByOffset(terminal.ViewStartIndex() + 1).GetText();
        VERIFY_IS_TRUE(text.starts_with(std::wstring(27, L'p') + L"$ "));
    }

    void TmuxSnapshotTests::ResizingAfterWrongWidthReplayCannotRepairCursor()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 40, 6 }, 100, renderer);
        terminal.Write(til::u8u16(PromptSnapshot(80, 69)));
        const auto clamped = terminal.GetViewportRelativeCursorPosition();
        VERIFY_ARE_EQUAL(39, clamped.x);
        VERIFY_SUCCEEDED(terminal.UserResize({ 80, 6 }));
        const auto resized = terminal.GetViewportRelativeCursorPosition();
        WEX::Logging::Log::Comment(WEX::Common::NoThrowString().Format(
            L"Expected cursor=(69,0); temporary-grid cursor=(%d,%d); cursor after resize=(%d,%d)",
            clamped.x,
            clamped.y,
            resized.x,
            resized.y));
        VERIFY_IS_TRUE(resized != til::point(69, 0));
    }

    void TmuxSnapshotTests::PreservesIntentionalCursorInsidePrompt()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 80, 6 }, 100, renderer);
        terminal.Write(til::u8u16(PromptSnapshot(80, 12)));
        VerifyPrompt(terminal, { 12, 0 });
    }

    void TmuxSnapshotTests::WaitsForMatchingViewportBeforeReplayingSnapshot()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 40, 6 }, 100, renderer);
        const auto connection = winrt::make_self<winrt::TerminalApp::implementation::TmuxPaneConnection>(nullptr, nullptr);
        uint32_t outputs = 0;
        connection->TerminalOutput([&](winrt::array_view<const char16_t> text) {
            terminal.Write({ reinterpret_cast<const wchar_t*>(text.data()), text.size() });
            ++outputs;
        });
        connection->Resize(6, 40);
        connection->Start();
        VERIFY_IS_FALSE(connection->IsViewportReady(6, 80));
        VERIFY_ARE_EQUAL(0u, outputs);
        VERIFY_SUCCEEDED(terminal.UserResize({ 80, 6 }));
        connection->Resize(6, 80);
        VERIFY_IS_TRUE(connection->IsViewportReady(6, 80));
        connection->WriteOutput(PromptSnapshot(80, 69));
        VERIFY_ARE_EQUAL(1u, outputs);
        VerifyPrompt(terminal, { 69, 0 });
        connection->WriteOutput("x");
        VerifyPrompt(terminal, { 70, 0 });
        const auto text = terminal.GetTextBuffer().GetRowByOffset(terminal.ViewStartIndex()).GetText();
        VERIFY_IS_TRUE(text.starts_with(std::wstring(67, L'p') + L"$ x"));
    }

    void TmuxSnapshotTests::RestoresPromptAfterCapturedHistory()
    {
        Terminal terminal{ Terminal::TestDummyMarker{} };
        DummyRenderer renderer{ &terminal };
        terminal.Create({ 80, 6 }, 100, renderer);
        std::string capture;
        for (size_t line = 0; line < 20; ++line)
        {
            capture.append("previous output\n");
        }
        capture.append(std::string(67, 'p') + "$ ");
        const auto snapshot = ParsePaneSnapshotState("80 6 69 5 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 5 1");
        terminal.Write(til::u8u16(RestorePaneState(snapshot.pane, {}, capture)));
        VerifyPrompt(terminal, { 69, 5 });
        const auto text = terminal.GetTextBuffer().GetRowByOffset(terminal.ViewStartIndex() + 5).GetText();
        VERIFY_IS_TRUE(text.starts_with(std::wstring(67, L'p') + L"$ "));
    }
}
