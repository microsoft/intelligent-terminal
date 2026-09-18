// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/TmuxPaneConnection.h"
#include "../TerminalApp/TmuxPaneState.h"

using namespace WEX::TestExecution;
using namespace winrt::Microsoft::Terminal::TerminalConnection;
using winrt::TerminalApp::implementation::TmuxPaneConnection;

namespace TerminalAppUnitTests
{
    class TmuxPaneConnectionTests
    {
        TEST_CLASS(TmuxPaneConnectionTests);
        TEST_METHOD(OwnsDistinctStableIdentity);
        TEST_METHOD(BuffersUntilTerminalStarts);
        TEST_METHOD(PreservesUtf8AcrossOutputChunks);
        TEST_METHOD(RejectsInputUntilHydrated);
        TEST_METHOD(ClosedPaneCannotBeReconnectedByLateCapture);
        TEST_METHOD(PreservesSurrogatePairsAcrossInputEvents);
        TEST_METHOD(RealTmuxUnsetCursorSnapshotEnablesInput);
        TEST_METHOD(ViewportReadinessRequiresStartAndExactGrid);
        TEST_METHOD(ResizeRecordsGridBeforeNotifyingOwner);
    };

    void TmuxPaneConnectionTests::OwnsDistinctStableIdentity()
    {
        const auto first = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        const auto second = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        VERIFY_IS_TRUE(first->SessionId() != winrt::guid{});
        VERIFY_IS_TRUE(first->SessionId() != second->SessionId());
        const auto id = first->SessionId();
        first->Start();
        first->Close();
        VERIFY_IS_TRUE(first->SessionId() == id);
    }

    void TmuxPaneConnectionTests::BuffersUntilTerminalStarts()
    {
        const auto connection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        std::wstring output;
        connection->TerminalOutput([&](winrt::array_view<const char16_t> text) {
            output.append(reinterpret_cast<const wchar_t*>(text.data()), text.size());
        });
        connection->WriteOutput("first");
        connection->WriteOutput("-second");
        VERIFY_IS_TRUE(output.empty());
        connection->Start();
        VERIFY_ARE_EQUAL(std::wstring{ L"first-second" }, output);
        connection->Start();
        connection->WriteOutput("-third");
        VERIFY_ARE_EQUAL(std::wstring{ L"first-second-third" }, output);
        VERIFY_IS_TRUE(connection->State() == ConnectionState::Connecting);
    }

    void TmuxPaneConnectionTests::PreservesUtf8AcrossOutputChunks()
    {
        const auto connection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        std::wstring output;
        connection->TerminalOutput([&](winrt::array_view<const char16_t> text) {
            output.append(reinterpret_cast<const wchar_t*>(text.data()), text.size());
        });
        connection->Start();
        connection->WriteOutput("\xe4");
        connection->WriteOutput("\xb8\xad\xf0\x9f");
        connection->WriteOutput("\x98\x80\033[31m");
        VERIFY_ARE_EQUAL(std::wstring{ L"\x4e2d\xd83d\xde00\033[31m" }, output);
    }

    void TmuxPaneConnectionTests::RejectsInputUntilHydrated()
    {
        std::string input;
        const auto connection = winrt::make_self<TmuxPaneConnection>(
            [&](std::string_view text) { input.append(text); }, nullptr);
        const char16_t key[] = { u'x' };
        connection->WriteInput(key);
        VERIFY_IS_TRUE(input.empty());
        connection->SetState(ConnectionState::Connected);
        connection->WriteInput(key);
        VERIFY_ARE_EQUAL(std::string{ "x" }, input);
        connection->SetState(ConnectionState::Failed);
        connection->WriteInput(key);
        VERIFY_ARE_EQUAL(std::string{ "x" }, input);
    }

    void TmuxPaneConnectionTests::ClosedPaneCannotBeReconnectedByLateCapture()
    {
        const auto connection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        uint32_t events = 0;
        connection->StateChanged([&](auto&&, auto&&) { ++events; });
        connection->SetState(ConnectionState::Connected);
        connection->Close();
        connection->SetState(ConnectionState::Connected);
        connection->Close();
        connection->Start();
        VERIFY_IS_TRUE(connection->State() == ConnectionState::Closed);
        VERIFY_ARE_EQUAL(2u, events);
    }

    void TmuxPaneConnectionTests::PreservesSurrogatePairsAcrossInputEvents()
    {
        std::string input;
        const auto connection = winrt::make_self<TmuxPaneConnection>(
            [&](std::string_view text) { input.append(text); }, nullptr);
        connection->SetState(ConnectionState::Connected);
        const char16_t high[] = { 0xd83d };
        const char16_t low[] = { 0xde00 };
        connection->WriteInput(high);
        VERIFY_IS_TRUE(input.empty());
        connection->WriteInput(low);
        VERIFY_ARE_EQUAL(std::string{ "\xf0\x9f\x98\x80" }, input);
    }

    void TmuxPaneConnectionTests::RealTmuxUnsetCursorSnapshotEnablesInput()
    {
        using namespace Microsoft::Terminal::Tmux;
        std::string input;
        const auto connection = winrt::make_self<TmuxPaneConnection>(
            [&](std::string_view text) { input.append(text); }, nullptr);
        const auto state = ParsePaneState("29 1 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1");
        connection->WriteOutput(RestorePaneState(state, {}, "shell prompt"));
        connection->SetState(ConnectionState::Connected);
        const char16_t command[] = { u'l', u's', u' ', u'-', u'l', u'\r' };
        connection->WriteInput(command);
        VERIFY_IS_TRUE(connection->State() == ConnectionState::Connected);
        VERIFY_ARE_EQUAL(std::string{ "ls -l\r" }, input);
    }

    void TmuxPaneConnectionTests::ViewportReadinessRequiresStartAndExactGrid()
    {
        const auto connection = winrt::make_self<TmuxPaneConnection>(nullptr, nullptr);
        connection->Resize(6, 40);
        VERIFY_IS_FALSE(connection->IsViewportReady(6, 40));
        connection->Start();
        VERIFY_IS_TRUE(connection->IsViewportReady(6, 40));
        VERIFY_IS_FALSE(connection->IsViewportReady(6, 80));
        connection->Resize(6, 80);
        VERIFY_IS_TRUE(connection->IsViewportReady(6, 80));
        VERIFY_IS_FALSE(connection->IsViewportReady(7, 80));
        VERIFY_IS_FALSE(connection->IsViewportReady(0, 0));
        connection->Close();
        VERIFY_IS_FALSE(connection->IsViewportReady(6, 80));
    }

    void TmuxPaneConnectionTests::ResizeRecordsGridBeforeNotifyingOwner()
    {
        winrt::com_ptr<TmuxPaneConnection> connection;
        uint32_t resized = 0;
        connection = winrt::make_self<TmuxPaneConnection>(
            nullptr, [&](const uint32_t rows, const uint32_t columns) {
                VERIFY_IS_TRUE(connection->IsViewportReady(rows, columns));
                ++resized;
            });
        connection->Start();
        connection->Resize(24, 80);
        connection->Resize(30, 120);
        VERIFY_ARE_EQUAL(2u, resized);
    }
}
