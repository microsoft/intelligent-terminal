// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"

#include "../inc/AgentSourceUtils.h"

using namespace WEX::TestExecution;

namespace TerminalAppUnitTests
{
    class AgentSourceUtilsTests
    {
        TEST_CLASS(AgentSourceUtilsTests);

        TEST_METHOD(PrefersPaneCwdOverWindowLaunchCwd);
    };

    void AgentSourceUtilsTests::PrefersPaneCwdOverWindowLaunchCwd()
    {
        namespace AgentSource = Microsoft::Terminal::AgentSource;
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\work" },
            AgentSource::ResolveCwd(
                L"C:\\work",
                L"C:\\Windows\\System32",
                L"C:\\profile",
                L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\window" },
            AgentSource::ResolveCwd({}, L"C:\\window", L"C:\\profile", L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\profile" },
            AgentSource::ResolveCwd({}, {}, L"C:\\profile", L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(
            std::wstring{ L"C:\\Users\\user" },
            AgentSource::ResolveCwd({}, {}, {}, L"C:\\Users\\user"));
        VERIFY_ARE_EQUAL(std::wstring{}, AgentSource::ResolveCwd({}, {}, {}, {}));
    }
}