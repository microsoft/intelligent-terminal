// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

// CustomAgentId.h — derive a short, stable identifier from a user-supplied
// command line for a "custom" AI agent (the ACP / delegate agent slot).
//
// The settings UI lets the user paste an arbitrary command (e.g. `helper.cmd
// --acp`, `"C:\Program Files\helper\helper.cmd" --acp`, or just `helper`).
// The settings model stores this command verbatim in AcpCustomCommand /
// DelegateCustomCommand. But the agent *id* itself (AcpAgent /
// DelegateAgent) needs to be a single short token so that the rest of the
// pipeline (policy allowlist, display name) has something stable to key
// on.
//
// `DeriveCustomAgentId` performs that extraction:
//   1. Trim leading whitespace.
//   2. Take the first whitespace-separated token, or if the command begins
//      with a double-quote take the contents of the quoted region (so
//      paths containing spaces work).
//   3. Strip any directory portion (last `/` or `\`).
//   4. Strip a trailing `.exe`, `.cmd`, or `.bat` extension
//      (case-insensitive).
//
// Header-only and pure: no settings-model or registry access. Callers must
// always prefix the returned id with "custom:" before storing it in the
// AcpAgent / DelegateAgent setting — that prefix is the system-wide
// discriminator used by EffectiveAcpAgent, the command-line resolver, and
// the custom-edit/delete UI gates. (Telemetry collapses every non-built-in
// id to literal `custom` via `sanitizeProviderId` and does not key on the
// prefix.) Storing a bare id silently breaks the consumers above; see
// PR #123.

#pragma once

#include <string_view>
#include <winrt/base.h>

namespace Microsoft::Terminal::Settings::Model
{
    inline winrt::hstring DeriveCustomAgentId(std::wstring_view command)
    {
        // Trim leading whitespace.
        const auto firstNonSpace = command.find_first_not_of(L" \t");
        if (firstNonSpace == std::wstring_view::npos)
        {
            return winrt::hstring{};
        }
        command.remove_prefix(firstNonSpace);

        // Pull out the executable token.
        std::wstring_view token;
        if (!command.empty() && command.front() == L'"')
        {
            command.remove_prefix(1);
            const auto closing = command.find(L'"');
            token = (closing != std::wstring_view::npos) ? command.substr(0, closing) : command;
        }
        else
        {
            const auto pos = command.find_first_of(L" \t");
            token = (pos != std::wstring_view::npos) ? command.substr(0, pos) : command;
        }

        // Strip directory portion.
        const auto slash = token.find_last_of(L"\\/");
        if (slash != std::wstring_view::npos)
        {
            token = token.substr(slash + 1);
        }

        // Case-insensitive trailing-extension strip. CompareStringOrdinal
        // works on non-null-terminated buffers and matches the style of
        // AgentPolicy.h.
        for (const auto* ext : { L".exe", L".cmd", L".bat" })
        {
            const auto extLen = static_cast<size_t>(4); // all three are 4 wide chars
            if (token.size() > extLen &&
                CompareStringOrdinal(
                    token.data() + token.size() - extLen, static_cast<int>(extLen),
                    ext, static_cast<int>(extLen),
                    TRUE) == CSTR_EQUAL)
            {
                token = token.substr(0, token.size() - extLen);
                break;
            }
        }

        return winrt::hstring{ token };
    }
}
