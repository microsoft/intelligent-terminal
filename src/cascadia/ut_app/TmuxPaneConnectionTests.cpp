// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/TmuxPaneConnection.h"

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
}
