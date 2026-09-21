// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxProtocolTests.cpp
//
// Abstract:
// - Contract tests for byte-oriented tmux control-mode parsing and layouts.

#include "precomp.h"

#include "../TerminalApp/TmuxProtocol.h"

#include <iterator>
#include <limits>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::string WithChecksum(const std::string_view layout)
        {
            uint32_t checksum{};
            for (const auto byte : layout)
            {
                checksum = ((checksum >> 1) + ((checksum & 1) ? 32768 : 0) + static_cast<unsigned char>(byte)) & 65535;
            }
            constexpr std::string_view hex{ "0123456789abcdef" };
            std::string result(4, '0');
            for (size_t i = 0; i < result.size(); ++i)
            {
                result[3 - i] = hex[(checksum >> (i * 4)) & 15];
            }
            result.push_back(',');
            result.append(layout);
            return result;
        }

        void Append(std::vector<Event>& events, std::vector<Event> more)
        {
            events.insert(events.end(), std::make_move_iterator(more.begin()), std::make_move_iterator(more.end()));
        }

        void VerifyEvents(const std::vector<Event>& expected, const std::vector<Event>& actual)
        {
            VERIFY_ARE_EQUAL(expected.size(), actual.size());
            for (size_t i = 0; i < expected.size(); ++i)
            {
                VERIFY_IS_TRUE(expected[i].kind == actual[i].kind);
                VERIFY_ARE_EQUAL(expected[i].commandNumber, actual[i].commandNumber);
                VERIFY_ARE_EQUAL(expected[i].success, actual[i].success);
                VERIFY_ARE_EQUAL(expected[i].flags, actual[i].flags);
                VERIFY_ARE_EQUAL(expected[i].paneId, actual[i].paneId);
                VERIFY_ARE_EQUAL(expected[i].name, actual[i].name);
                VERIFY_ARE_EQUAL(expected[i].text, actual[i].text);
            }
        }

        std::string NestedLayout(const size_t splits)
        {
            auto result = "1x1," + std::to_string(splits * 2) + ",0," + std::to_string(splits);
            for (auto i = splits; i-- > 0;)
            {
                const auto x = std::to_string(i * 2);
                result = std::to_string((splits - i) * 2 + 1) + "x1," + x + ",0{1x1," + x + ",0," + std::to_string(i) + "," + result + "}";
            }
            return WithChecksum(result);
        }

        void FillResponse(Parser& parser)
        {
            parser.Feed("%begin 1 2 0\n");
            size_t remaining = Parser::MaxResponseBytes;
            bool first = true;
            while (remaining)
            {
                if (!first)
                {
                    --remaining;
                }
                const auto count = (std::min)(remaining, Parser::MaxLineBytes);
                parser.Feed(std::string(count, 'x') + "\n");
                remaining -= count;
                first = false;
            }
        }
    }

    class TmuxProtocolTests
    {
        TEST_CLASS(TmuxProtocolTests);

        TEST_METHOD(ParsesGoldenLayouts);
        TEST_METHOD(RejectsBadChecksumsAndGrammar);
        TEST_METHOD(RejectsBadDimensionsAndGeometry);
        TEST_METHOD(RejectsDuplicateAndOverflowingPaneIds);
        TEST_METHOD(BoundsLayoutSizeDepthAndNodeCount);
        TEST_METHOD(ParsesResponsesWithoutInterpretingBodyNotifications);
        TEST_METHOD(ParsesErrorsAndEmptyResponses);
        TEST_METHOD(PreservesUnsolicitedAndBatchedCommandResponseFlags);
        TEST_METHOD(ParsesOutputAndExtendedOutputAsBytes);
        TEST_METHOD(PreservesEveryOctalByte);
        TEST_METHOD(RejectsMalformedOctalAndOutputHeaders);
        TEST_METHOD(FragmentationDoesNotChangeEvents);
        TEST_METHOD(RejectsInvalidResponseGuards);
        TEST_METHOD(RejectsInvalidFramingAndTruncatedStreams);
        TEST_METHOD(HandlesExitAndUnknownNotifications);
        TEST_METHOD(DistinguishesClientExitFromRemoteServerExit);
        TEST_METHOD(BoundsLinesWithoutDroppingBytes);
        TEST_METHOD(BoundsResponseBlocksWithoutDroppingBytes);
        TEST_METHOD(EncodesInputWithoutCommandInterpolation);
        TEST_METHOD(ChunksInputAtBoundedCommandLengths);
        TEST_METHOD(FormatsNamedSocketSessionTitle);
        TEST_METHOD(OmitsDefaultOrMissingSocketFromSessionTitle);
        TEST_METHOD(PreservesSessionIdentityText);
        TEST_METHOD(ParsesSessionInventoryWithStableIds);
        TEST_METHOD(RejectsMalformedSessionInventory);
        TEST_METHOD(BoundsSessionInventory);
        TEST_METHOD(ParsesServerWidePaneInventory);
        TEST_METHOD(RejectsMalformedPaneInventory);
        TEST_METHOD(BoundsPaneInventory);
    };

    void TmuxProtocolTests::FormatsNamedSocketSessionTitle()
    {
        VERIFY_ARE_EQUAL(std::string{ "it-test/demo" }, FormatSessionTitle("/tmp/tmux-1000/it-test", "demo"));
        VERIFY_ARE_EQUAL(std::string{ "it-test/worker" }, FormatSessionTitle("it-test", "worker"));
        VERIFY_ARE_EQUAL(std::string{ "it-test/demo" }, FormatSessionTitle(R"(\\.\pipe\it-test)", "demo"));
    }

    void TmuxProtocolTests::DistinguishesClientExitFromRemoteServerExit()
    {
        for (const auto reason : {
                 "",
                 "exited",
                 "detached",
                 "detached (from session work)",
                 "detached and SIGHUP",
                 "lost tty",
                 "terminated",
                 "unknown reason",
                 "server exited ",
                 "server exited unexpectedly extra",
             })
        {
            Parser parser;
            const auto events = parser.Feed(std::string{ "%exit" } + (std::string_view{ reason }.empty() ? "" : " ") + reason + "\n");
            VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
            VERIFY_IS_FALSE(ExitEndsRemoteServer(events[0].text));
        }
        for (const auto reason : { "server exited", "server exited unexpectedly" })
        {
            Parser parser;
            const auto events = parser.Feed(std::string{ "%exit " } + reason + "\n");
            VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
            VERIFY_IS_TRUE(ExitEndsRemoteServer(events[0].text));
        }
    }

    void TmuxProtocolTests::ParsesServerWidePaneInventory()
    {
        const auto panes = ParsePaneIds("%0\n%42\n%18446744073709551615");
        VERIFY_ARE_EQUAL(size_t{ 3 }, panes.size());
        VERIFY_IS_TRUE(panes.contains(Id{ 0 }));
        VERIFY_IS_TRUE(panes.contains(Id{ 42 }));
        VERIFY_IS_TRUE(panes.contains((std::numeric_limits<Id>::max)()));
        VERIFY_IS_TRUE(ParsePaneIds({}).empty());
        VERIFY_IS_TRUE(ParsePaneIds("%7\n").contains(Id{ 7 }));
        VERIFY_IS_TRUE(ParsePaneIds("%0007").contains(Id{ 7 }));
    }

    void TmuxProtocolTests::RejectsMalformedPaneInventory()
    {
        for (const auto text : {
                 "\n",
                 "%",
                 "1",
                 "$1",
                 "@1",
                 "%-1",
                 "%+1",
                 " %1",
                 "%1 ",
                 "%1 extra",
                 "%1\n\n",
                 "%1\r\n",
                 "%1\n%1",
                 "%1\n%01",
                 "%18446744073709551616",
                 "%1.0",
                 "%0x1",
             })
        {
            VERIFY_THROWS(ParsePaneIds(text), ProtocolError);
        }
        for (int ch = 0; ch <= 32; ++ch)
        {
            VERIFY_THROWS(ParsePaneIds(std::string{ "%1" } + static_cast<char>(ch) + "2"), ProtocolError);
        }
        VERIFY_THROWS(ParsePaneIds("%1\x7f"), ProtocolError);
        VERIFY_THROWS(ParsePaneIds("%1\xff"), ProtocolError);
    }

    void TmuxProtocolTests::BoundsPaneInventory()
    {
        std::string inventory;
        std::string fullBuffer;
        for (size_t id = 0; id < 4096; ++id)
        {
            const auto number = std::to_string(id);
            inventory.append("%").append(number).push_back('\n');
            fullBuffer.append("%").append(14 - number.size(), '0').append(number).push_back('\n');
        }
        VERIFY_ARE_EQUAL(size_t{ 4096 }, ParsePaneIds(inventory).size());
        VERIFY_THROWS(ParsePaneIds(inventory + "%4096"), ProtocolError);
        VERIFY_ARE_EQUAL(size_t{ 64 * 1024 }, fullBuffer.size());
        VERIFY_ARE_EQUAL(size_t{ 4096 }, ParsePaneIds(fullBuffer).size());
        VERIFY_THROWS(ParsePaneIds(fullBuffer + "\n"), ProtocolError);
    }

    void TmuxProtocolTests::ParsesSessionInventoryWithStableIds()
    {
        const auto sessions = ParseSessions("$0 first\n$42 work 'quoted' $name; \xe4\xbc\x9a\xe8\xaf\x9d");
        VERIFY_ARE_EQUAL(size_t{ 2 }, sessions.size());
        VERIFY_ARE_EQUAL(Id{ 0 }, sessions[0].id);
        VERIFY_ARE_EQUAL(std::string{ "first" }, sessions[0].name);
        VERIFY_ARE_EQUAL(Id{ 42 }, sessions[1].id);
        VERIFY_ARE_EQUAL(std::string{ "work 'quoted' $name; \xe4\xbc\x9a\xe8\xaf\x9d" }, sessions[1].name);
        VERIFY_IS_TRUE(ParseSessions({}).empty());
        VERIFY_ARE_EQUAL(size_t{ 1 }, ParseSessions("$7 trailing newline\n").size());
    }

    void TmuxProtocolTests::RejectsMalformedSessionInventory()
    {
        for (const auto text : {
                 "name",
                 "@0 name",
                 "$ name",
                 "$-1 name",
                 "$1 ",
                 "$1",
                 "$1 name\n\n",
                 "$1 name\r",
                 "$1 name\t"
                 "value",
                 "$1 name\x1b",
                 "$1 one\n$1 two",
                 "$18446744073709551616 overflow",
             })
        {
            VERIFY_THROWS(ParseSessions(text), ProtocolError);
        }
        VERIFY_THROWS(ParseSessions(std::string_view{ "$1 a\0b", 6 }), ProtocolError);
    }

    void TmuxProtocolTests::BoundsSessionInventory()
    {
        std::string inventory;
        for (size_t id = 0; id < 256; ++id)
        {
            inventory.append("$").append(std::to_string(id)).append(" name\n");
        }
        VERIFY_ARE_EQUAL(size_t{ 256 }, ParseSessions(inventory).size());
        VERIFY_THROWS(ParseSessions(inventory + "$256 extra"), ProtocolError);
        VERIFY_ARE_EQUAL(size_t{ 1 }, ParseSessions("$1 " + std::string(64 * 1024 - 3, 'a')).size());
        VERIFY_THROWS(ParseSessions("$1 " + std::string(64 * 1024 - 2, 'a')), ProtocolError);
    }

    void TmuxProtocolTests::OmitsDefaultOrMissingSocketFromSessionTitle()
    {
        VERIFY_ARE_EQUAL(std::string{ "demo" }, FormatSessionTitle({}, "demo"));
        VERIFY_ARE_EQUAL(std::string{ "demo" }, FormatSessionTitle("/tmp/tmux-1000/default", "demo"));
        VERIFY_ARE_EQUAL(std::string{ "demo" }, FormatSessionTitle("default", "demo"));
        VERIFY_IS_TRUE(FormatSessionTitle("/tmp/tmux-1000/it-test", {}).empty());
    }

    void TmuxProtocolTests::PreservesSessionIdentityText()
    {
        VERIFY_ARE_EQUAL(std::string{ "my socket/my session" }, FormatSessionTitle("/tmp/my socket", "my session"));
        VERIFY_ARE_EQUAL(std::string{ "\xe7\xbb\x88\xe7\xab\xaf/\xe4\xbc\x9a\xe8\xaf\x9d" },
                         FormatSessionTitle("/tmp/\xe7\xbb\x88\xe7\xab\xaf", "\xe4\xbc\x9a\xe8\xaf\x9d"));
        VERIFY_ARE_EQUAL(std::string{ "it-test/renamed" }, FormatSessionTitle("/tmp/tmux-1000/it-test", "renamed"));
    }

    void TmuxProtocolTests::ParsesGoldenLayouts()
    {
        const auto leaf = ParseLayout("b25d,80x24,0,0,0");
        VERIFY_IS_TRUE(leaf.kind == LayoutNode::Kind::Leaf);
        VERIFY_ARE_EQUAL(uint32_t{ 80 }, leaf.width);
        VERIFY_ARE_EQUAL(uint32_t{ 24 }, leaf.height);
        VERIFY_ARE_EQUAL(Id{ 0 }, leaf.paneId);
        VERIFY_IS_TRUE(leaf.children.empty());
        VERIFY_IS_TRUE(leaf == ParseLayout("B25D,80x24,0,0,0"));

        const auto root = ParseLayout("1589,121x40,0,0{60x40,0,0,7,60x40,61,0[60x19,61,0,8,60x20,61,20,9]}");
        VERIFY_IS_TRUE(root.kind == LayoutNode::Kind::Columns);
        VERIFY_ARE_EQUAL(size_t{ 2 }, root.children.size());
        VERIFY_ARE_EQUAL(Id{ 7 }, root.children[0].paneId);
        const auto& rows = root.children[1];
        VERIFY_IS_TRUE(rows.kind == LayoutNode::Kind::Rows);
        VERIFY_ARE_EQUAL(uint32_t{ 61 }, rows.x);
        VERIFY_ARE_EQUAL(Id{ 9 }, rows.children[1].paneId);
        VERIFY_ARE_EQUAL(uint32_t{ 20 }, rows.children[1].y);
        VERIFY_IS_FALSE(leaf == root);
    }

    void TmuxProtocolTests::RejectsBadChecksumsAndGrammar()
    {
        for (const auto text : { "", "b25d", "b25d;", "zzzz,80x24,0,0,0", "0000,80x24,0,0,0", "80x24,0,0,0" })
        {
            VERIFY_THROWS(ParseLayout(text), ProtocolError);
        }
        for (const auto body : {
                 "80x24,0,0", "80x24,0,0,0junk", "80x24,0,0,0,", "80X24,0,0,0", "80x24,0,0{}", "80x24,0,0[]", "3x1,0,0{1x1,0,0,0,1x1,2,0,1]", "3x1,0,0{1x1,0,0,0,1x1,2,0,1", "1x1,0,0{1x1,0,0,0}", "3x1,0,0{1x1,0,0,0,,1x1,2,0,1}" })
        {
            VERIFY_THROWS(ParseLayout(WithChecksum(body)), ProtocolError);
        }
    }

    void TmuxProtocolTests::RejectsBadDimensionsAndGeometry()
    {
        VERIFY_ARE_EQUAL(uint32_t{ 32767 }, ParseLayout(WithChecksum("32767x1,0,0,1")).width);
        for (const auto body : {
                 "0x1,0,0,1", "1x0,0,0,1", "32768x1,0,0,1", "1x32768,0,0,1", "1x1,32767,0,1", "1x1,0,32767,1", "-1x1,0,0,1", "1x1,-1,0,1", "18446744073709551616x1,0,0,1", "3x1,0,0{1x1,0,0,0,1x1,1,0,1}", "4x1,0,0{1x1,0,0,0,1x1,2,0,1}", "3x2,0,0{1x2,0,0,0,1x1,2,0,1}", "3x2,0,0{1x2,0,0,0,1x2,2,1,1}", "1x3,0,0[1x1,0,0,0,1x1,0,1,1]", "2x3,0,0[2x1,0,0,0,1x1,0,2,1]", "1x3,0,0[1x1,0,0,0,1x1,1,2,1]" })
        {
            VERIFY_THROWS(ParseLayout(WithChecksum(body)), ProtocolError);
        }
    }

    void TmuxProtocolTests::RejectsDuplicateAndOverflowingPaneIds()
    {
        VERIFY_THROWS(ParseLayout(WithChecksum("3x1,0,0{1x1,0,0,9,1x1,2,0,9}")), ProtocolError);
        VERIFY_THROWS(ParseLayout(WithChecksum("1x1,0,0,18446744073709551616")), ProtocolError);
        VERIFY_THROWS(ParseLayout(WithChecksum("1x1,0,0,-1")), ProtocolError);
        VERIFY_THROWS(ParseLayout(WithChecksum("1x1,0,0,%1")), ProtocolError);
        VERIFY_ARE_EQUAL((std::numeric_limits<Id>::max)(), ParseLayout(WithChecksum("1x1,0,0,18446744073709551615")).paneId);
    }

    void TmuxProtocolTests::BoundsLayoutSizeDepthAndNodeCount()
    {
        VERIFY_THROWS(ParseLayout(std::string(1024 * 1024 + 1, '0')), ProtocolError);
        VERIFY_IS_TRUE(ParseLayout(NestedLayout(63)).kind == LayoutNode::Kind::Columns);
        VERIFY_THROWS(ParseLayout(NestedLayout(64)), ProtocolError);

        constexpr size_t leaves = 4096;
        std::string wide = std::to_string(leaves * 2 - 1) + "x1,0,0{";
        for (size_t i = 0; i < leaves; ++i)
        {
            if (i)
            {
                wide.push_back(',');
            }
            wide += "1x1," + std::to_string(i * 2) + ",0," + std::to_string(i);
        }
        wide.push_back('}');
        VERIFY_THROWS(ParseLayout(WithChecksum(wide)), ProtocolError);
    }

    void TmuxProtocolTests::ParsesResponsesWithoutInterpretingBodyNotifications()
    {
        Parser parser;
        auto body = std::string{ "%output %99 \\000not a notification\n%exit still a body\n%layout-change @1 raw" };
        body.append("\n\0raw", 5);
        const auto events = parser.Feed("%begin 123 7 1\r\n" + body + "\r\n%end 123 7 1\r\n");
        VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
        VERIFY_IS_TRUE(events[0].kind == Event::Kind::Response);
        VERIFY_ARE_EQUAL(uint64_t{ 7 }, events[0].commandNumber);
        VERIFY_IS_TRUE(events[0].success);
        VERIFY_ARE_EQUAL(body, events[0].text);
        parser.Finish();
    }

    void TmuxProtocolTests::ParsesErrorsAndEmptyResponses()
    {
        Parser parser;
        const auto events = parser.Feed("%begin 1 1 0\n%end 1 1 0\n"
                                        "%begin 1 2 0\n\n\n%end 1 2 0\n"
                                        "%begin 1 3 0\nbad command\n%error 1 3 0\n");
        VERIFY_ARE_EQUAL(size_t{ 3 }, events.size());
        VERIFY_IS_TRUE(events[0].success);
        VERIFY_IS_TRUE(events[0].text.empty());
        VERIFY_ARE_EQUAL(std::string{ "\n" }, events[1].text);
        VERIFY_IS_FALSE(events[2].success);
        VERIFY_ARE_EQUAL(std::string{ "bad command" }, events[2].text);
        parser.Finish();
    }

    void TmuxProtocolTests::PreservesUnsolicitedAndBatchedCommandResponseFlags()
    {
        Parser parser;
        const auto events = parser.Feed("%begin 123 100 0\n%end 123 100 0\n"
                                        "%begin 123 101 1\nA\n%end 123 101 1\n"
                                        "%begin 123 102 1\nB\n%end 123 102 1\n");
        VERIFY_ARE_EQUAL(size_t{ 3 }, events.size());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, events[0].flags);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, events[1].flags);
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, events[2].flags);
        VERIFY_ARE_EQUAL(uint64_t{ 101 }, events[1].commandNumber);
        VERIFY_ARE_EQUAL(uint64_t{ 102 }, events[2].commandNumber);
        VERIFY_ARE_EQUAL(std::string{ "A" }, events[1].text);
        VERIFY_ARE_EQUAL(std::string{ "B" }, events[2].text);
        parser.Finish();
    }

    void TmuxProtocolTests::ParsesOutputAndExtendedOutputAsBytes()
    {
        Parser parser;
        const auto events = parser.Feed("%output %42 A\\000\\015\\012\\134\\377\xe2\x82\xac\n"
                                        "%extended-output %7 125 future-field : two  spaces\\011\n"
                                        "%output %1 \n%extended-output %2 0 : \n");
        VERIFY_ARE_EQUAL(size_t{ 4 }, events.size());
        VERIFY_IS_TRUE(events[0].kind == Event::Kind::Output);
        VERIFY_ARE_EQUAL(Id{ 42 }, events[0].paneId);
        VERIFY_ARE_EQUAL(std::string("A\0\r\n\\\xff\xe2\x82\xac", 9), events[0].text);
        VERIFY_ARE_EQUAL(Id{ 7 }, events[1].paneId);
        VERIFY_ARE_EQUAL(std::string{ "two  spaces\t" }, events[1].text);
        VERIFY_IS_TRUE(events[2].text.empty());
        VERIFY_IS_TRUE(events[3].text.empty());
        parser.Finish();
    }

    void TmuxProtocolTests::PreservesEveryOctalByte()
    {
        std::string bytes;
        std::string encoded;
        for (unsigned int i = 0; i < 256; ++i)
        {
            bytes.push_back(static_cast<char>(i));
            encoded.push_back('\\');
            encoded.push_back(static_cast<char>('0' + (i >> 6)));
            encoded.push_back(static_cast<char>('0' + ((i >> 3) & 7)));
            encoded.push_back(static_cast<char>('0' + (i & 7)));
        }
        VERIFY_ARE_EQUAL(bytes, DecodeOctal(encoded));
        VERIFY_ARE_EQUAL(std::string("raw\0utf8\xf0\x9f\x98\x80", 12), DecodeOctal(std::string_view("raw\0utf8\xf0\x9f\x98\x80", 12)));
        VERIFY_IS_TRUE(DecodeOctal("").empty());
    }

    void TmuxProtocolTests::RejectsMalformedOctalAndOutputHeaders()
    {
        for (const auto text : { "\\", "\\0", "\\00", "\\008", "\\400", "\\777", "\\\\", "\\xyz", "ok\\x00" })
        {
            VERIFY_THROWS(DecodeOctal(text), ProtocolError);
        }
        for (const auto line : {
                 "%output\n", "%output %1\n", "%output 1 text\n", "%output %-1 text\n", "%output %18446744073709551616 text\n", "%output %1 \\400\n", "%extended-output %1 -1 : x\n", "%extended-output %1 0 x\n", "%extended-output %1 0\n", "%extended-output %1 0  : x\n" })
        {
            Parser parser;
            VERIFY_THROWS(parser.Feed(line), ProtocolError);
        }
    }

    void TmuxProtocolTests::FragmentationDoesNotChangeEvents()
    {
        const std::string body = "%begin 1234 99 1\r\nfirst\r\n%output %99 body\r\n%end 1234 99 1\r\n"
                                 "%output %42 \\000\\134\\377\xe2\x82\xac\r\n"
                                 "%extended-output %7 1 : hi\\012\r\n"
                                 "%session-changed $2 a session\r\n%exit detached\r\n";
        for (const auto& wire : { body, "\x1bP1000p" + body + "\x1b\\" })
        {
            Parser whole;
            const auto expected = whole.Feed(wire);
            whole.Finish();
            for (size_t split = 0; split <= wire.size(); ++split)
            {
                Parser parser;
                auto actual = parser.Feed(std::string_view{ wire }.substr(0, split));
                Append(actual, parser.Feed(std::string_view{ wire }.substr(split)));
                parser.Finish();
                VerifyEvents(expected, actual);
            }
            for (const size_t chunk : { 1, 2, 3, 7, 31 })
            {
                Parser parser;
                std::vector<Event> actual;
                for (size_t i = 0; i < wire.size(); i += chunk)
                {
                    Append(actual, parser.Feed(std::string_view{ wire }.substr(i, chunk)));
                }
                parser.Finish();
                VerifyEvents(expected, actual);
            }
        }
    }

    void TmuxProtocolTests::RejectsInvalidResponseGuards()
    {
        for (const auto wire : {
                 "%end 1 2 0\n", "%error 1 2 0\n", "%begin\n", "%begin 1 2\n", "%begin -1 2 0\n", "%begin 1 2 0 extra\n", "%begin 1  2 0\n", "%begin 18446744073709551616 2 0\n", "%begin 1 2 0\n%end 2 2 0\n", "%begin 1 2 0\n%end 1 3 0\n", "%begin 1 2 0\n%error 1 2 1\n", "%begin 1 2 0\n%end broken\n", "%begin 1 2 0\n%begin 1 3 0\n" })
        {
            Parser parser;
            VERIFY_THROWS(parser.Feed(wire), ProtocolError);
            VERIFY_THROWS(parser.Feed("%exit\n"), ProtocolError);
            VERIFY_THROWS(parser.Finish(), ProtocolError);
        }
    }

    void TmuxProtocolTests::RejectsInvalidFramingAndTruncatedStreams()
    {
        for (const auto wire : {
                 "\x1bP1001p", "\x1b[", "\x1bP1000p\x1b\\", "\x1bP1000p%exit\n\x1bx", "\x1bP1000p%exit\n\x1b\\extra", "not a control notification\n", "%\n" })
        {
            Parser parser;
            VERIFY_THROWS(parser.Feed(wire), ProtocolError);
        }
        const auto unfinishedResponse = "%begin 1 2 0\n"
                                        "body\n";
        for (const auto wire : {
                 "\x1b", "\x1bP1000", "\x1bP1000p", "\x1bP1000p%exit\n", "\x1bP1000p%exit\n\x1b", "%begin 1 2 0\n", unfinishedResponse, "%output %1 partial", "%exit\r" })
        {
            Parser parser;
            parser.Feed(wire);
            VERIFY_THROWS(parser.Finish(), ProtocolError);
        }
        Parser empty;
        empty.Finish();
        empty.Finish();
        VERIFY_THROWS(empty.Feed(""), ProtocolError);
    }

    void TmuxProtocolTests::HandlesExitAndUnknownNotifications()
    {
        Parser parser;
        const auto events = parser.Feed("%future-notification @9  raw args \r\n%exit a reason\r\n");
        VERIFY_ARE_EQUAL(size_t{ 2 }, events.size());
        VERIFY_IS_TRUE(events[0].kind == Event::Kind::Notification);
        VERIFY_ARE_EQUAL(std::string{ "future-notification" }, events[0].name);
        VERIFY_ARE_EQUAL(std::string{ "@9  raw args " }, events[0].text);
        VERIFY_IS_TRUE(events[1].kind == Event::Kind::Exit);
        VERIFY_ARE_EQUAL(std::string{ "a reason" }, events[1].text);
        VERIFY_THROWS(parser.Feed("%output %0 late\n"), ProtocolError);

        Parser noReason;
        const auto exit = noReason.Feed("%exit\n");
        VERIFY_IS_TRUE(exit[0].text.empty());
        noReason.Finish();
    }

    void TmuxProtocolTests::BoundsLinesWithoutDroppingBytes()
    {
        const std::string prefix{ "%output %1 " };
        const std::string payload(Parser::MaxLineBytes - prefix.size(), 'x');
        Parser exact;
        const auto events = exact.Feed(prefix + payload + "\r\n");
        VERIFY_ARE_EQUAL(payload, events[0].text);
        exact.Finish();

        Parser tooLong;
        tooLong.Feed(prefix);
        tooLong.Feed(payload);
        VERIFY_THROWS(tooLong.Feed("x"), ProtocolError);
        VERIFY_THROWS(DecodeOctal(std::string(Parser::MaxLineBytes + 1, 'x')), ProtocolError);
    }

    void TmuxProtocolTests::BoundsResponseBlocksWithoutDroppingBytes()
    {
        Parser exact;
        FillResponse(exact);
        const auto events = exact.Feed("%end 1 2 0\n");
        VERIFY_ARE_EQUAL(size_t{ 1 }, events.size());
        VERIFY_ARE_EQUAL(Parser::MaxResponseBytes, events[0].text.size());
        exact.Finish();

        Parser tooLong;
        FillResponse(tooLong);
        VERIFY_THROWS(tooLong.Feed("\n"), ProtocolError);
    }

    void TmuxProtocolTests::EncodesInputWithoutCommandInterpolation()
    {
        VERIFY_IS_TRUE(EncodeSendKeys(1, "").empty());
        const auto commands = EncodeSendKeys(42, std::string_view{ "\0\n;\"\\\xe2\x82\xac", 8 });
        VERIFY_ARE_EQUAL(size_t{ 1 }, commands.size());
        VERIFY_ARE_EQUAL(std::string{ "send-keys -H -t %42 00 0a 3b 22 5c e2 82 ac\n" }, commands[0]);
        VERIFY_ARE_EQUAL(std::string{ "send-keys -H -t %18446744073709551615 ff\n" },
                         EncodeSendKeys((std::numeric_limits<Id>::max)(), "\xff")[0]);
    }

    void TmuxProtocolTests::ChunksInputAtBoundedCommandLengths()
    {
        std::string original;
        for (size_t i = 0; i < 5000; ++i)
        {
            original.push_back(static_cast<char>(i));
        }
        const auto commands = EncodeSendKeys(9, original);
        VERIFY_IS_TRUE(commands.size() > 1);
        std::string recovered;
        constexpr std::string_view prefix{ "send-keys -H -t %9" };
        for (const auto& command : commands)
        {
            VERIFY_IS_TRUE(command.size() < 4096);
            VERIFY_ARE_EQUAL(prefix, std::string_view{ command }.substr(0, prefix.size()));
            VERIFY_ARE_EQUAL('\n', command.back());
            for (size_t position = prefix.size(); position + 3 < command.size(); position += 3)
            {
                VERIFY_ARE_EQUAL(' ', command[position]);
                unsigned int value{};
                const auto result = std::from_chars(command.data() + position + 1, command.data() + position + 3, value, 16);
                VERIFY_IS_TRUE(result.ec == std::errc{});
                recovered.push_back(static_cast<char>(value));
            }
        }
        VERIFY_ARE_EQUAL(original, recovered);
    }
}
