// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// QuoteArgForCommandLine.h
//
// Correct CommandLineToArgvW-compatible quoting for a single argument.
// Eliminates hand-rolled escaping throughout the codebase. Use this
// whenever building a commandline string for CreateProcess/ShellExecute.
//
// Pure Win32 + STL, no WinRT dependency. Non-throwing API — returns
// empty optional on invalid input (embedded NUL, invalid program path).

#pragma once

#include <cwchar>
#include <optional>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::CommandLine
{
    // Quote a single argument for use in a Windows commandline string.
    // The result is always wrapped in double quotes for unambiguous parsing
    // by CommandLineToArgvW. Handles:
    //   - Backslashes before `"` are doubled (2n+1 backslashes + `"`)
    //   - Trailing backslashes before the closing `"` are doubled
    //   - All other characters are passed through literally
    //
    // Returns std::nullopt if the argument contains embedded NUL (which
    // would silently truncate the commandline at the OS level).
    //
    // NOTE: This is for argv[1..n] only. argv[0] (the program path) has
    // different rules — use QuoteProgramPath() for that.
    inline std::optional<std::wstring> QuoteArgForCommandLine(std::wstring_view arg) noexcept
    {
        // Reject embedded NUL — it would truncate the commandline.
        for (const auto ch : arg)
        {
            if (ch == L'\0')
            {
                return std::nullopt;
            }
        }

        std::wstring result;
        result.reserve(arg.size() + 8);
        result.push_back(L'"');

        size_t backslashes = 0;
        for (const auto ch : arg)
        {
            if (ch == L'\\')
            {
                ++backslashes;
            }
            else if (ch == L'"')
            {
                // Double the accumulated backslashes, then emit \"
                result.append(backslashes * 2 + 1, L'\\');
                result.push_back(L'"');
                backslashes = 0;
            }
            else
            {
                // Flush any accumulated backslashes as-is
                result.append(backslashes, L'\\');
                backslashes = 0;
                result.push_back(ch);
            }
        }
        // Trailing backslashes must be doubled (they precede the closing `"`)
        result.append(backslashes * 2, L'\\');
        result.push_back(L'"');

        return result;
    }

    // Quote a program path (argv[0]) for use in a Windows commandline string.
    // argv[0] has simpler rules than argv[1..n]: backslashes are always literal
    // and `"` cannot be escaped inside it. We wrap in quotes (for paths with
    // spaces) and reject paths containing `"` or NUL (which are invalid on
    // Windows file systems anyway).
    //
    // Returns std::nullopt if the path contains `"` or embedded NUL.
    inline std::optional<std::wstring> QuoteProgramPath(std::wstring_view path) noexcept
    {
        for (const auto ch : path)
        {
            if (ch == L'"' || ch == L'\0')
            {
                return std::nullopt;
            }
        }
        std::wstring result;
        result.reserve(path.size() + 2);
        result.push_back(L'"');
        result.append(path);
        result.push_back(L'"');
        return result;
    }

    // Build a JSON-encoded `--agent-config` argument from the given fields.
    // Returns the full fragment: ` --agent-config "<escaped-json>"`
    // Uses manual RFC 8259-compliant JSON construction (no external JSON
    // library dependency) and QuoteArgForCommandLine for the single
    // argument boundary.
    //
    // Usage:
    //   cmdline += BuildAgentConfigArg(agentCli, agentId, delegateAgent,
    //                                  delegateModel, acpModel);
    //
    // Any empty field is omitted from the JSON (the Rust side uses
    // Option<String> and falls back to defaults for missing fields).
    //
    // All control characters in field values (including NUL) are escaped
    // per RFC 8259, so the resulting JSON blob is always valid and safe
    // for commandline transport.
    inline std::wstring BuildAgentConfigArg(
        std::wstring_view agent,
        std::wstring_view agentId,
        std::wstring_view delegateAgent,
        std::wstring_view delegateModel,
        std::wstring_view acpModel)
    {
        // Build a compact JSON object with only non-empty fields.
        // We use manual JSON construction to avoid pulling in JsonCpp here
        // (this header is used in both TerminalApp and TerminalSettingsEditor).
        // The JSON spec is simple enough for known-safe field names: only the
        // VALUES need escaping, and we do it correctly per RFC 8259.
        // NUL characters in values are escaped as \u0000 (valid JSON), so
        // they don't pose a truncation risk in the final commandline.
        auto jsonEscapeValue = [](std::wstring_view val) -> std::wstring {
            std::wstring out;
            out.reserve(val.size() + 4);
            for (const auto ch : val)
            {
                switch (ch)
                {
                case L'"': out += L"\\\""; break;
                case L'\\': out += L"\\\\"; break;
                case L'\b': out += L"\\b"; break;
                case L'\f': out += L"\\f"; break;
                case L'\n': out += L"\\n"; break;
                case L'\r': out += L"\\r"; break;
                case L'\t': out += L"\\t"; break;
                default:
                    if (ch < 0x20)
                    {
                        wchar_t buf[8];
                        swprintf_s(buf, L"\\u%04x", static_cast<unsigned>(ch));
                        out += buf;
                    }
                    else
                    {
                        out.push_back(ch);
                    }
                    break;
                }
            }
            return out;
        };

        std::wstring json = L"{";
        bool first = true;

        auto appendField = [&](const wchar_t* key, std::wstring_view val) {
            if (val.empty())
                return;
            if (!first)
                json += L',';
            first = false;
            json += L'"';
            json += key;
            json += L"\":\"";
            json += jsonEscapeValue(val);
            json += L'"';
        };

        appendField(L"agent", agent);
        appendField(L"agentId", agentId);
        appendField(L"delegateAgent", delegateAgent);
        appendField(L"delegateModel", delegateModel);
        appendField(L"acpModel", acpModel);

        json += L'}';

        // The JSON blob itself won't contain NUL (all control chars are
        // escaped above), so QuoteArgForCommandLine won't fail here.
        auto quoted = QuoteArgForCommandLine(json);
        if (!quoted)
        {
            return {};
        }
        return L" --agent-config " + *quoted;
    }
}
