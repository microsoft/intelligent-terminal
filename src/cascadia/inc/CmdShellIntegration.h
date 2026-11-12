// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "ShellIntegrationProfileGate.h"

#include <til/env.h>

namespace Microsoft::Terminal::ShellIntegration
{
    inline void ApplyCmdCwdReporting(til::env& environment, const std::wstring_view commandline)
    {
        if (!details::CommandlineHasExeToken(commandline, L"cmd"))
        {
            return;
        }

        auto& prompt = environment.as_map()[L"PROMPT"];
        if (prompt.empty())
        {
            prompt = L"$P$G";
        }

        // CMD expands $P at each prompt; the OSC prefix is invisible and stays
        // local to the child environment, including any nested CMD processes.
        constexpr std::wstring_view prefix{ L"$e]9;9;$P$e\\" };
        if (!details::EqualsCi(std::wstring_view{ prompt }.substr(0, prefix.size()), prefix))
        {
            prompt.insert(0, prefix);
        }
    }
}
