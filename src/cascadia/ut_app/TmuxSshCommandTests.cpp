// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxSshCommandTests.cpp
//
// Abstract:
// - Tests the two quoting boundaries of explicit default-server SSH launches.

#include "precomp.h"

#include "../inc/TmuxSshCommand.h"

#include <shellapi.h>
#include <wil/resource.h>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::wstring RemoteTarget(const std::wstring_view destination, const std::wstring_view session, const uint16_t port = 0)
        {
            const auto commandline = BuildSshCommandline(destination, session, port);
            int argc = 0;
            const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(commandline.c_str(), &argc) };
            VERIFY_IS_NOT_NULL(argv.get());
            const auto offset = port == 0 ? 0 : 2;
            VERIFY_ARE_EQUAL(12 + offset, argc);
            VERIFY_ARE_EQUAL(std::wstring{ L"ssh.exe" }, std::wstring{ argv[0] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-T" }, std::wstring{ argv[1] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-o" }, std::wstring{ argv[2] });
            VERIFY_ARE_EQUAL(std::wstring{ L"BatchMode=yes" }, std::wstring{ argv[3] });
            if (port != 0)
            {
                VERIFY_ARE_EQUAL(std::wstring{ L"-p" }, std::wstring{ argv[4] });
                VERIFY_ARE_EQUAL(std::to_wstring(port), std::wstring{ argv[5] });
            }
            VERIFY_ARE_EQUAL(std::wstring{ destination }, std::wstring{ argv[4 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"tmux" }, std::wstring{ argv[5 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-L" }, std::wstring{ argv[6 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"default" }, std::wstring{ argv[7 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-C" }, std::wstring{ argv[8 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"attach-session" }, std::wstring{ argv[9 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-t" }, std::wstring{ argv[10 + offset] });
            return argv[11 + offset];
        }

        std::wstring RemoteListFormat(const std::wstring_view destination, const uint16_t port = 0)
        {
            const auto commandline = BuildSshListCommandline(destination, port);
            int argc = 0;
            const wil::unique_hlocal_ptr<PWSTR[]> argv{ CommandLineToArgvW(commandline.c_str(), &argc) };
            VERIFY_IS_NOT_NULL(argv.get());
            const auto offset = port == 0 ? 0 : 2;
            VERIFY_ARE_EQUAL(13 + offset, argc);
            VERIFY_ARE_EQUAL(std::wstring{ L"ssh.exe" }, std::wstring{ argv[0] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-T" }, std::wstring{ argv[1] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-n" }, std::wstring{ argv[2] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-o" }, std::wstring{ argv[3] });
            VERIFY_ARE_EQUAL(std::wstring{ L"BatchMode=yes" }, std::wstring{ argv[4] });
            if (port != 0)
            {
                VERIFY_ARE_EQUAL(std::wstring{ L"-p" }, std::wstring{ argv[5] });
                VERIFY_ARE_EQUAL(std::to_wstring(port), std::wstring{ argv[6] });
            }
            VERIFY_ARE_EQUAL(std::wstring{ destination }, std::wstring{ argv[5 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"LC_ALL=C" }, std::wstring{ argv[6 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"tmux" }, std::wstring{ argv[7 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-L" }, std::wstring{ argv[8 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"default" }, std::wstring{ argv[9 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"list-sessions" }, std::wstring{ argv[10 + offset] });
            VERIFY_ARE_EQUAL(std::wstring{ L"-F" }, std::wstring{ argv[11 + offset] });
            return argv[12 + offset];
        }

        // Decode only single-quoted POSIX spans and escaped apostrophes outside
        // them. No unquoted expansion, operator, or argument separator is allowed.
        std::wstring UnquoteRemoteTarget(const std::wstring_view argument)
        {
            bool quoted = false;
            std::wstring result;
            for (size_t i = 0; i < argument.size(); ++i)
            {
                const auto ch = argument[i];
                if (ch == L'\'')
                {
                    quoted = !quoted;
                }
                else if (!quoted && ch == L'\\')
                {
                    VERIFY_IS_TRUE(i + 1 < argument.size());
                    VERIFY_ARE_EQUAL(L'\'', argument[++i]);
                    result.push_back(L'\'');
                }
                else
                {
                    VERIFY_IS_TRUE(quoted);
                    result.push_back(ch);
                }
            }
            VERIFY_IS_FALSE(quoted);
            return result;
        }

        std::wstring ParsedTmuxTarget(const std::wstring_view argument)
        {
            auto target = UnquoteRemoteTarget(argument);
            if (!target.empty() && target.back() == L';')
            {
                // tmux's argv parser removes this escape; an unescaped final
                // semicolon would terminate the command and truncate the name.
                VERIFY_IS_TRUE(target.size() >= 2);
                VERIFY_ARE_EQUAL(L'\\', target[target.size() - 2]);
                target.erase(target.size() - 2, 1);
            }
            return target;
        }
    }

    class TmuxSshCommandTests
    {
        TEST_CLASS(TmuxSshCommandTests);

        TEST_METHOD(UsesDefaultServerAndPreservesAliases);
        TEST_METHOD(ListUsesDefaultServerWithoutInteractiveInput);
        TEST_METHOD(PreservesPortsForAttachAndList);
        TEST_METHOD(NamesAreExactAndSessionIdsRemainIds);
        TEST_METHOD(PreservesUnicodeAndAdversarialSessionNames);
        TEST_METHOD(QuotesEachShellBoundary);
        TEST_METHOD(RejectsInvalidDestinationsAndEmptySessions);
        TEST_METHOD(RejectsControlsAndEmbeddedNulls);
        TEST_METHOD(RejectsMalformedUtf16);
        TEST_METHOD(BoundsInputsAndExpandedCommandline);
    };

    void TmuxSshCommandTests::UsesDefaultServerAndPreservesAliases()
    {
        VERIFY_ARE_EQUAL(std::wstring{ L"ssh.exe -T -o BatchMode=yes \"ubuntu\" tmux -L default -C attach-session -t \"'=test'\"" },
                         BuildSshCommandline(L"ubuntu", L"test"));
        for (const auto destination : { L"ubuntu", L"user@My-SSH_Alias", L"user@[fe80::1%12]", L"user@192.0.2.1", L"\u4e3b\u673a" })
        {
            VERIFY_ARE_EQUAL(std::wstring{ L"'=test'" }, RemoteTarget(destination, L"test"));
        }
    }

    void TmuxSshCommandTests::NamesAreExactAndSessionIdsRemainIds()
    {
        for (const auto session : { L"test", L"test-prefix", L"*?[abc]", L"=literal", L"{last}", L"-Lnamed", L"$", L"$1suffix", L"$-1" })
        {
            VERIFY_ARE_EQUAL(std::wstring{ L"=" } + session, UnquoteRemoteTarget(RemoteTarget(L"ubuntu", session)));
        }

        for (const auto session : { L"$0", L"$1", L"$1234567890", L"$01" })
        {
            VERIFY_ARE_EQUAL(std::wstring{ session }, UnquoteRemoteTarget(RemoteTarget(L"ubuntu", session)));
        }
    }

    void TmuxSshCommandTests::ListUsesDefaultServerWithoutInteractiveInput()
    {
        VERIFY_ARE_EQUAL(std::wstring{ L"ssh.exe -T -n -o BatchMode=yes \"ubuntu\" LC_ALL=C tmux -L default list-sessions -F \"'#{session_id} #{session_name}'\"" },
                         BuildSshListCommandline(L"ubuntu"));
        for (const auto destination : { L"ubuntu", L"user@My-SSH_Alias", L"user@[fe80::1%12]", L"user@192.0.2.1", L"\u4e3b\u673a" })
        {
            VERIFY_ARE_EQUAL(std::wstring{ L"#{session_id} #{session_name}" }, UnquoteRemoteTarget(RemoteListFormat(destination)));
        }
    }

    void TmuxSshCommandTests::PreservesPortsForAttachAndList()
    {
        VERIFY_ARE_EQUAL(BuildSshCommandline(L"ubuntu", L"test"), BuildSshCommandline(L"ubuntu", L"test", 0));
        VERIFY_ARE_EQUAL(BuildSshListCommandline(L"ubuntu"), BuildSshListCommandline(L"ubuntu", 0));
        for (const auto port : { uint16_t{ 1 }, uint16_t{ 22 }, uint16_t{ 2222 }, uint16_t{ 65535 } })
        {
            VERIFY_ARE_EQUAL(std::wstring{ L"$42" }, ParsedTmuxTarget(RemoteTarget(L"user@[::1]", L"$42", port)));
            VERIFY_ARE_EQUAL(std::wstring{ L"=a'\\\"b;" }, ParsedTmuxTarget(RemoteTarget(L"alias", L"a'\\\"b;", port)));
            VERIFY_ARE_EQUAL(std::wstring{ L"#{session_id} #{session_name}" }, UnquoteRemoteTarget(RemoteListFormat(L"user@alias", port)));
        }
    }

    void TmuxSshCommandTests::PreservesUnicodeAndAdversarialSessionNames()
    {
        for (const auto session : {
                 L"\u5f00\u53d1 \U0001f680",
                 L" leading and trailing ",
                 L"'",
                 L"\"",
                 L"a\\\"b\\\\",
                 L"\\",
                 L"semi;",
                 L"semi\\;",
                 L"semi\\\\;",
                 L";;;",
                 L"$(touch x); & | `whoami` > out < in",
                 L"a'; touch x; echo 'b",
                 L"\"; new-session -s unexpected; \"",
                 L"%PATH% !HOME! $HOME ${HOME} #{session_id}",
             })
        {
            VERIFY_ARE_EQUAL(std::wstring{ L"=" } + session, ParsedTmuxTarget(RemoteTarget(L"ubuntu", session)));
        }
    }

    void TmuxSshCommandTests::QuotesEachShellBoundary()
    {
        VERIFY_ARE_EQUAL(std::wstring{ L"'=a'\\''b'" }, RemoteTarget(L"ubuntu", L"a'b"));
        VERIFY_ARE_EQUAL(std::wstring{ L"'=a\\\"b\\\\'" }, RemoteTarget(L"ubuntu", L"a\\\"b\\\\"));
        VERIFY_ARE_EQUAL(std::wstring{ L"'=$HOME; `whoami`'" }, RemoteTarget(L"ubuntu", L"$HOME; `whoami`"));
        VERIFY_ARE_EQUAL(std::wstring{ L"'$1'" }, RemoteTarget(L"ubuntu", L"$1"));
        VERIFY_ARE_EQUAL(std::wstring{ L"'=semi\\;'" }, RemoteTarget(L"ubuntu", L"semi;"));

        // Exercise Windows runs of backslashes before both quotes and literal
        // characters, including an apostrophe's POSIX quoting boundary.
        for (size_t count = 0; count < 8; ++count)
        {
            for (const auto suffix : { L"\"", L"'", L"x", L"" })
            {
                const auto session = L"test" + std::wstring(count, L'\\') + suffix;
                VERIFY_ARE_EQUAL(L"=" + session, UnquoteRemoteTarget(RemoteTarget(L"ubuntu", session)));
            }
        }
    }

    void TmuxSshCommandTests::RejectsInvalidDestinationsAndEmptySessions()
    {
        for (const auto destination : {
                 L"",
                 L" ",
                 L"-oProxyCommand=anything",
                 L"--",
                 L"host name",
                 L" host",
                 L"host ",
                 L"bad;command",
                 L"$(command)",
                 L"\"host\"",
                 L"host'quoted",
                 L"host\\path",
                 L"user@",
                 L"@host",
                 L"user@@host",
                 L"user@-host",
                 L"/etc/config",
             })
        {
            VERIFY_THROWS(BuildSshCommandline(destination, L"test"), std::invalid_argument);
            VERIFY_THROWS(BuildSshCommandline(destination, L"test", 2222), std::invalid_argument);
            VERIFY_THROWS(BuildSshListCommandline(destination), std::invalid_argument);
            VERIFY_THROWS(BuildSshListCommandline(destination, 2222), std::invalid_argument);
        }
        VERIFY_THROWS(BuildSshCommandline(L"ubuntu", L""), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(L"ubuntu", L"   "), std::invalid_argument);
    }

    void TmuxSshCommandTests::RejectsControlsAndEmbeddedNulls()
    {
        for (wchar_t ch = 0; ch <= L'\x9f'; ++ch)
        {
            if (ch >= L' ' && ch < L'\x7f')
            {
                continue;
            }
            const auto value = std::wstring{ L"before" } + ch + L"after";
            VERIFY_IS_FALSE(IsValidSshLaunchText(value));
            VERIFY_THROWS(BuildSshCommandline(value, L"test"), std::invalid_argument);
            VERIFY_THROWS(BuildSshCommandline(L"ubuntu", value), std::invalid_argument);
            VERIFY_THROWS(BuildSshListCommandline(value), std::invalid_argument);
        }
    }

    void TmuxSshCommandTests::BoundsInputsAndExpandedCommandline()
    {
        const auto oversized = std::wstring(MaxSshLaunchTextLength + 1, L'x');
        VERIFY_IS_FALSE(IsValidSshLaunchText(oversized));
        VERIFY_THROWS(BuildSshCommandline(oversized, L"test"), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(L"ubuntu", oversized), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(L"ubuntu", std::wstring(MaxSshLaunchTextLength / 2, L'\'')), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(L"ubuntu", std::wstring(MaxSshLaunchTextLength / 2, L'"')), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(std::wstring(18000, L'h'), std::wstring(18000, L's')), std::invalid_argument);
        VERIFY_THROWS(BuildSshListCommandline(oversized), std::invalid_argument);

        const auto overhead = BuildSshCommandline(L"h", L"x").size() - 1;
        const auto boundary = std::wstring(MaxSshLaunchTextLength - overhead, L'x');
        VERIFY_ARE_EQUAL(MaxSshLaunchTextLength, BuildSshCommandline(L"h", boundary).size());
        VERIFY_THROWS(BuildSshCommandline(L"h", boundary + L"x"), std::invalid_argument);
        VERIFY_THROWS(BuildSshCommandline(L"h", boundary, 65535), std::invalid_argument);

        const auto listOverhead = BuildSshListCommandline(L"h", 65535).size() - 1;
        const auto listBoundary = std::wstring(MaxSshLaunchTextLength - listOverhead, L'h');
        VERIFY_ARE_EQUAL(MaxSshLaunchTextLength, BuildSshListCommandline(listBoundary, 65535).size());
        VERIFY_THROWS(BuildSshListCommandline(listBoundary + L"h", 65535), std::invalid_argument);
    }

    void TmuxSshCommandTests::RejectsMalformedUtf16()
    {
        for (const auto value : { L"\xd800", L"\xdc00", L"\xd800x", L"x\xdc00", L"\xd800\xd800", L"\xdc00\xd800" })
        {
            VERIFY_THROWS(BuildSshCommandline(value, L"test"), std::invalid_argument);
            VERIFY_THROWS(BuildSshCommandline(L"ubuntu", value), std::invalid_argument);
            VERIFY_THROWS(BuildSshListCommandline(value), std::invalid_argument);
        }
    }
}
