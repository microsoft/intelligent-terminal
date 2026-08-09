// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/ProtocolServiceRouting.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class ProtocolServiceRoutingTests
    {
        TEST_CLASS(ProtocolServiceRoutingTests);

        TEST_METHOD(ZeroWindowUsesProcessWideShellSessionService);
        TEST_METHOD(VisibleWindowUsesTerminalPageShellSessionService);
    };

    void ProtocolServiceRoutingTests::ZeroWindowUsesProcessWideShellSessionService()
    {
        auto visibleCalls = 0u;
        auto processCalls = 0u;

        const auto result = ::Microsoft::Terminal::Protocol::details::RouteShellSessionRequest(
            false,
            [&]() {
                ++visibleCalls;
                return 1;
            },
            [&]() {
                ++processCalls;
                return 2;
            });

        VERIFY_ARE_EQUAL(2, result);
        VERIFY_ARE_EQUAL(0u, visibleCalls);
        VERIFY_ARE_EQUAL(1u, processCalls);
    }

    void ProtocolServiceRoutingTests::VisibleWindowUsesTerminalPageShellSessionService()
    {
        auto visibleCalls = 0u;
        auto processCalls = 0u;

        const auto result = ::Microsoft::Terminal::Protocol::details::RouteShellSessionRequest(
            true,
            [&]() {
                ++visibleCalls;
                return 1;
            },
            [&]() {
                ++processCalls;
                return 2;
            });

        VERIFY_ARE_EQUAL(1, result);
        VERIFY_ARE_EQUAL(1u, visibleCalls);
        VERIFY_ARE_EQUAL(0u, processCalls);
    }
}
