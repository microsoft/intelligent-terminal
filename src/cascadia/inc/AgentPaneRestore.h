// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <algorithm>
#include <cctype>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

// How an agent session survives a restart.
//
// Both kinds of agent-bearing pane persist through fields the saved window
// layout already has, so none of this adds a key to `state.json` or an option
// to the `wt` command line:
//
//   * A shell pane that was running an agent CLI is saved with its
//     `commandline` rewritten to that agent's resume invocation.
//   * The agent pane is an ordinary terminal pane hosting a `wta` helper, so it
//     is saved as an ordinary `splitPane` — its geometry rides `SplitPaneArgs`,
//     and its `type` says it is agent-backed.
//
// What an agent pane may NOT persist is the live helper's command line: that
// names the current run's master pipe, this window's id, this tab's id, and a
// CLI path already resolved through GPO `AllowedAgents`. Replaying any of those
// after a restart is either meaningless or a frozen policy decision. So the
// saved form keeps only what cannot be re-derived — which conversation to load,
// which agent owns it, and which view the user was on — and the restore
// re-resolves the rest exactly as it would for a brand new pane.

namespace Microsoft::Terminal::AgentPaneRestore
{
    // The `NewTerminalArgs` content type an agent pane carries in a saved
    // layout. `agent` is one the user had open; `agentStashed` is one they
    // toggled away. Two values rather than a separate persisted flag, because
    // `type` is a key the layout format already has.
    inline constexpr std::wstring_view PaneType{ L"agent" };
    inline constexpr std::wstring_view StashedPaneType{ L"agentStashed" };

    inline bool IsPaneType(const std::wstring_view type) noexcept
    {
        return type == PaneType || type == StashedPaneType;
    }

    // Flags recognised in a persisted agent pane command line. They share
    // spelling with the live helper flags so the saved string reads like a real
    // invocation rather than an encoded blob.
    inline constexpr std::wstring_view SessionIdFlag{ L"--initial-load-session-id" };
    inline constexpr std::wstring_view ViewFlag{ L"--initial-view" };
    inline constexpr std::wstring_view AgentIdentityFlag{ L"--agent-backend" };
    inline constexpr std::wstring_view CustomCommandFlag{ L"--agent-custom-command" };
    inline constexpr std::wstring_view SessionsView{ L"sessions" };
    inline constexpr std::wstring_view ChatView{ L"chat" };

    struct Fields
    {
        std::wstring sessionId;
        std::wstring view;
        std::wstring agentIdentity;
        std::wstring customCommand;
    };

    // Quote a value so that `CommandLineToArgvW` — which is what
    // `ParsePaneCommandline` is fed from — hands it back verbatim.
    //
    // That parser only treats backslashes specially when they run into a
    // quote, so escaping every one of them corrupts ordinary Windows paths:
    // `C:\tools\agent.exe` would come back as `C:\\tools\\agent.exe`. Double
    // only the runs that precede a quote, plus the trailing run that would
    // otherwise escape the closing quote.
    //
    // This deliberately does not escape `;` the way `QuoteAndEscapeCommandlineArg`
    // does: that escape exists for `wt`'s own action separator, and a literal
    // `\;` here would survive into the parsed value.
    inline void AppendQuoted(std::wstring& out, const std::wstring_view value)
    {
        out.push_back(L'"');

        size_t backslashes = 0;
        for (const auto ch : value)
        {
            if (ch == L'\\')
            {
                ++backslashes;
            }
            else
            {
                if (ch == L'"')
                {
                    // 2n+1 backslashes before a quote parse back as n
                    // backslashes plus a literal quote.
                    out.append(backslashes + 1, L'\\');
                }
                backslashes = 0;
            }
            out.push_back(ch);
        }

        out.append(backslashes, L'\\');
        out.push_back(L'"');
    }

    inline void AppendFlag(std::wstring& out, const std::wstring_view flag, const std::wstring_view value)
    {
        if (value.empty())
        {
            return;
        }
        out.push_back(L' ');
        out.append(flag);
        out.push_back(L' ');
        AppendQuoted(out, value);
    }

    // `executable` is cosmetic — the restore re-detects wta rather than
    // trusting a path baked into saved state — but writing it keeps the value
    // readable as a command line in `state.json`.
    inline std::wstring BuildPaneCommandline(const std::wstring_view executable, const Fields& fields)
    {
        std::wstring cmd;
        cmd.reserve(executable.size() + fields.sessionId.size() + fields.customCommand.size() + 128);
        AppendQuoted(cmd, executable);
        AppendFlag(cmd, SessionIdFlag, fields.sessionId);
        AppendFlag(cmd, ViewFlag, fields.view);
        AppendFlag(cmd, AgentIdentityFlag, fields.agentIdentity);
        AppendFlag(cmd, CustomCommandFlag, fields.customCommand);
        return cmd;
    }

    // Read back what `BuildPaneCommandline` wrote. Unknown tokens are skipped
    // rather than rejected: this is inspectable state a user may have edited,
    // and a partially understood pane is still better restored than dropped.
    inline Fields ParsePaneCommandline(const std::vector<std::wstring>& argv)
    {
        Fields fields;
        for (size_t i = 0; i < argv.size(); ++i)
        {
            const std::wstring_view token{ argv[i] };
            const auto next = [&]() -> std::wstring {
                return i + 1 < argv.size() ? argv[i + 1] : std::wstring{};
            };

            if (token == SessionIdFlag)
            {
                fields.sessionId = next();
                ++i;
            }
            else if (token == ViewFlag)
            {
                fields.view = next();
                ++i;
            }
            else if (token == AgentIdentityFlag)
            {
                fields.agentIdentity = next();
                ++i;
            }
            else if (token == CustomCommandFlag)
            {
                fields.customCommand = next();
                ++i;
            }
        }
        return fields;
    }

    // How each agent CLI spells "pick up this conversation again".
    //
    // This mirrors `resume_flag` in `tools/wta/src/agent_registry.rs`, which is
    // the source of truth; an agent missing here simply never gets its shell
    // pane rewritten. Agents whose `resume_flag` is empty do not belong here.
    inline constexpr std::pair<std::wstring_view, std::wstring_view> ResumeInvocations[]{
        { L"copilot", L"--resume" },
        { L"claude", L"--resume" },
        { L"codex", L"resume" },
        { L"gemini", L"--resume" },
        { L"opencode", L"--session" },
    };

    inline constexpr std::wstring_view ResumeShellPrefix{ L"cmd.exe /d /s /c \"" };

    // The command line that resumes `agentSessionId` under `cliSource`, or
    // empty when that is not something we can safely spell.
    //
    // The session id is validated rather than trusted: it may have arrived from
    // an agent hook, and it ends up inside a command line that gets executed.
    inline std::wstring BuildResumeCommandline(const std::wstring_view cliSource,
                                               const std::wstring_view agentSessionId)
    {
        if (agentSessionId.empty() ||
            agentSessionId.starts_with(L"sidekick-") ||
            agentSessionId.size() > 256 ||
            !std::all_of(agentSessionId.begin(), agentSessionId.end(), [](const wchar_t ch) {
                return (ch < 128 && std::isalnum(static_cast<unsigned char>(ch))) ||
                       ch == L'-' || ch == L'_' || ch == L'.' || ch == L':';
            }))
        {
            return {};
        }

        std::wstring cli{ cliSource };
        std::transform(cli.begin(), cli.end(), cli.begin(), [](const wchar_t ch) {
            return ch < 128 ? static_cast<wchar_t>(std::tolower(static_cast<unsigned char>(ch))) : ch;
        });

        for (const auto& [executable, resumeArg] : ResumeInvocations)
        {
            if (cli != executable)
            {
                continue;
            }

            std::wstring cmd{ ResumeShellPrefix };
            cmd.append(executable);
            cmd.push_back(L' ');
            cmd.append(resumeArg);
            cmd.push_back(L' ');
            cmd.append(agentSessionId);
            cmd.push_back(L'"');
            return cmd;
        }

        return {};
    }

    // Whether a command line is one of the invocations `BuildResumeCommandline`
    // produces.
    //
    // A pane launched this way must not have its saved scrollback seeded: the
    // CLI replays its own transcript, so seeding as well shows the conversation
    // twice and compounds it on every save/restore cycle. Asking what the pane
    // is about to run — rather than consulting persisted state — means a pane
    // the user pointed at a resume command themselves behaves the same way.
    struct ResumeTarget
    {
        std::wstring agent;
        std::wstring sessionId;
    };

    // Split a resume invocation back into the agent and session it names.
    // Empty `agent` means the command line is not one of ours.
    inline ResumeTarget ParseResumeCommandline(const std::wstring_view commandline)
    {
        if (!commandline.starts_with(ResumeShellPrefix) || !commandline.ends_with(L'"'))
        {
            return {};
        }

        const auto inner = commandline.substr(ResumeShellPrefix.size(),
                                              commandline.size() - ResumeShellPrefix.size() - 1);
        for (const auto& [executable, resumeArg] : ResumeInvocations)
        {
            std::wstring prefix{ executable };
            prefix.push_back(L' ');
            prefix.append(resumeArg);
            prefix.push_back(L' ');
            if (inner.starts_with(prefix))
            {
                return { std::wstring{ executable }, std::wstring{ inner.substr(prefix.size()) } };
            }
        }
        return {};
    }

    inline bool IsResumeCommandline(const std::wstring_view commandline)
    {
        return !ParseResumeCommandline(commandline).agent.empty();
    }
}
