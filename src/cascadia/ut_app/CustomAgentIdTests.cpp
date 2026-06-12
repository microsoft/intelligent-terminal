// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// CustomAgentIdTests.cpp
//
// Tests for `DeriveCustomAgentId` (src/cascadia/inc/CustomAgentId.h).
//
// This is the function used by the AI Agents settings page to turn a
// user-supplied command line (e.g. `helper.cmd --acp`, `"C:\Program
// Files\helper\helper.cmd" --acp`) into the short token that becomes the
// suffix of the stored agent id (e.g. `custom:helper`). Every downstream
// consumer that keys on the prefixed id (EffectiveAcpAgent policy gate,
// command-line resolver, custom-edit/delete UI gates) depends on this
// derivation; regressing this function silently breaks the save/reload
// round-trip (PR #123) or the launcher.

#include "precomp.h"

#include "../inc/CustomAgentId.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal::Settings::Model;

namespace TerminalAppUnitTests
{
    class CustomAgentIdTests
    {
        TEST_CLASS(CustomAgentIdTests);

        TEST_METHOD(BareName);
        TEST_METHOD(NameWithExe);
        TEST_METHOD(NameWithCmd);
        TEST_METHOD(NameWithBat);
        TEST_METHOD(ExtensionStripIsCaseInsensitive);
        TEST_METHOD(NameWithArgs);
        TEST_METHOD(UnquotedPath);
        TEST_METHOD(QuotedPathWithSpaces);
        TEST_METHOD(QuotedPathWithSpacesAndArgs);
        TEST_METHOD(ForwardSlashPath);
        TEST_METHOD(LeadingWhitespace);
        TEST_METHOD(TabSeparator);
        TEST_METHOD(Empty);
        TEST_METHOD(WhitespaceOnly);
        TEST_METHOD(UnclosedQuote);
        TEST_METHOD(QuoteOnlyIsEmpty);
        TEST_METHOD(EmptyQuotedIsEmpty);
        TEST_METHOD(BuiltInAgentNameStillExtracts);
        TEST_METHOD(PathWithSpacesAndExtensionStrip);
        TEST_METHOD(MixedSlashesUsesLastSeparator);
        TEST_METHOD(NoExtensionStripWhenTokenEqualsExtension);
        TEST_METHOD(DoesNotStripUnknownExtension);

        // Helper: assert DeriveCustomAgentId(input) == expected.
        static void Check(std::wstring_view input, std::wstring_view expected)
        {
            const auto actual = DeriveCustomAgentId(input);
            VERIFY_ARE_EQUAL(winrt::hstring{ expected }, actual,
                             NoThrowString{}.Format(L"input=[%.*s] expected=[%.*s] actual=[%s]",
                                                    static_cast<int>(input.size()), input.data(),
                                                    static_cast<int>(expected.size()), expected.data(),
                                                    actual.c_str()));
        }
    };

    void CustomAgentIdTests::BareName()
    {
        Check(L"helper", L"helper");
    }

    void CustomAgentIdTests::NameWithExe()
    {
        Check(L"helper.exe", L"helper");
    }

    void CustomAgentIdTests::NameWithCmd()
    {
        Check(L"helper.cmd", L"helper");
    }

    void CustomAgentIdTests::NameWithBat()
    {
        Check(L"helper.bat", L"helper");
    }

    void CustomAgentIdTests::ExtensionStripIsCaseInsensitive()
    {
        Check(L"helper.EXE", L"helper");
        Check(L"helper.Cmd", L"helper");
        Check(L"helper.BAT", L"helper");
        Check(L"helper.cMd --acp", L"helper");
    }

    void CustomAgentIdTests::NameWithArgs()
    {
        Check(L"helper.cmd --acp", L"helper");
        Check(L"helper --acp --stdio", L"helper");
    }

    void CustomAgentIdTests::UnquotedPath()
    {
        Check(L"C:\\tools\\helper.cmd", L"helper");
        Check(L"C:\\tools\\helper.cmd --acp", L"helper");
        Check(L"D:\\local-bin\\my-agent.exe", L"my-agent");
    }

    void CustomAgentIdTests::QuotedPathWithSpaces()
    {
        // Full path containing spaces, properly quoted — the whole quoted
        // region is the executable.
        Check(L"\"C:\\Program Files\\helper\\helper.cmd\"", L"helper");
    }

    void CustomAgentIdTests::QuotedPathWithSpacesAndArgs()
    {
        Check(L"\"C:\\Program Files\\helper\\helper.cmd\" --acp", L"helper");
        Check(L"\"C:\\Program Files (x86)\\my agent\\my-agent.exe\" --stdio --acp",
              L"my-agent");
    }

    void CustomAgentIdTests::ForwardSlashPath()
    {
        // POSIX-style forward slashes (some users paste paths like this).
        Check(L"/usr/bin/helper", L"helper");
        Check(L"C:/tools/helper.cmd --acp", L"helper");
    }

    void CustomAgentIdTests::LeadingWhitespace()
    {
        Check(L"   helper", L"helper");
        Check(L"  helper.cmd --acp", L"helper");
        Check(L"\thelper.cmd", L"helper");
    }

    void CustomAgentIdTests::TabSeparator()
    {
        // Tab between exe and args.
        Check(L"helper.cmd\t--acp", L"helper");
    }

    void CustomAgentIdTests::Empty()
    {
        Check(L"", L"");
    }

    void CustomAgentIdTests::WhitespaceOnly()
    {
        Check(L"   ", L"");
        Check(L"\t\t", L"");
        Check(L" \t ", L"");
    }

    void CustomAgentIdTests::UnclosedQuote()
    {
        // Missing closing quote — take everything after the opening quote.
        // Whatever the user typed is at least a recognizable token, not a crash.
        Check(L"\"C:\\Program Files\\helper\\helper.cmd", L"helper");
    }

    void CustomAgentIdTests::QuoteOnlyIsEmpty()
    {
        // Just a single quote: the token after it is empty.
        Check(L"\"", L"");
    }

    void CustomAgentIdTests::EmptyQuotedIsEmpty()
    {
        // "" : empty quoted region.
        Check(L"\"\"", L"");
        Check(L"\"\" --acp", L"");
    }

    void CustomAgentIdTests::BuiltInAgentNameStillExtracts()
    {
        // If a user types a built-in name (`copilot`, `gemini`, ...), the
        // function still extracts it. The caller's responsibility is to
        // notice the collision and append " (custom)" to the display name
        // — DeriveCustomAgentId itself does not enforce uniqueness.
        Check(L"copilot", L"copilot");
        Check(L"gemini.cmd --acp", L"gemini");
    }

    void CustomAgentIdTests::PathWithSpacesAndExtensionStrip()
    {
        // Exercise both the quoted-path branch AND the extension strip.
        Check(L"\"C:\\Program Files\\Tools\\foo.EXE\" arg1", L"foo");
        Check(L"\"C:\\foo bar\\baz.CMD\"", L"baz");
    }

    void CustomAgentIdTests::MixedSlashesUsesLastSeparator()
    {
        // The function strips at the *last* `\` or `/` (find_last_of), so
        // mixed paths are handled.
        Check(L"C:/foo\\bar/helper.cmd", L"helper");
        Check(L"C:\\foo/bar\\helper.exe", L"helper");
    }

    void CustomAgentIdTests::NoExtensionStripWhenTokenEqualsExtension()
    {
        // The strip guard is `token.size() > extLen`, so an extension-only
        // filename (".exe", ".cmd") is returned verbatim, not collapsed to "".
        Check(L".exe", L".exe");
        Check(L".cmd", L".cmd");
    }

    void CustomAgentIdTests::DoesNotStripUnknownExtension()
    {
        // We only strip .exe / .cmd / .bat. Other extensions are part of
        // the id (e.g. PowerShell scripts).
        Check(L"helper.ps1", L"helper.ps1");
        Check(L"helper.py", L"helper.py");
        Check(L"helper.sh", L"helper.sh");
    }
}
