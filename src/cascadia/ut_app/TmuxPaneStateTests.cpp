// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "precomp.h"
#include "../TerminalApp/TmuxPaneState.h"

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;

namespace TerminalAppUnitTests
{
    class TmuxPaneStateTests
    {
        TEST_CLASS(TmuxPaneStateTests);
        TEST_METHOD(AcceptsUnsetAlternateCursorFromRealTmux);
        TEST_METHOD(RestoresAlternateScreenWithSavedCursor);
        TEST_METHOD(RestoresAlternateScreenWithoutSavedCursor);
        TEST_METHOD(RejectsOversizedRealCoordinates);
        TEST_METHOD(RejectsInvalidFlagsRegionsAndFields);
        TEST_METHOD(PreservesCapturedTextAndTerminalModes);
        TEST_METHOD(MeasuresFullClientGridWithoutInventedOuterBorders);
        TEST_METHOD(WaitsForFinitePositiveClientAndFontDimensions);
        TEST_METHOD(ClampsClientGridToSupportedLimits);
        TEST_METHOD(ParsesSnapshotDimensionsAlongsideCursor);
        TEST_METHOD(RejectsInconsistentSnapshotDimensions);
    };

    void TmuxPaneStateTests::AcceptsUnsetAlternateCursorFromRealTmux()
    {
        const auto state = ParsePaneState("29 1 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1");
        VERIFY_ARE_EQUAL(29u, state.cursor.x);
        VERIFY_ARE_EQUAL(1u, state.cursor.y);
        VERIFY_IS_FALSE(state.alternate);
        VERIFY_IS_FALSE(state.savedCursor.has_value());
        const auto output = RestorePaneState(state, {}, "shell prompt");
        VERIFY_IS_TRUE(output.find("shell prompt") != std::string::npos);
        VERIFY_IS_TRUE(output.ends_with("\x1b[2;30H"));
        VERIFY_IS_TRUE(output.find("4294967295") == std::string::npos);
        VERIFY_IS_TRUE(output.find("?1049h") == std::string::npos);
    }

    void TmuxPaneStateTests::RestoresAlternateScreenWithSavedCursor()
    {
        const auto state = ParsePaneState("4 5 1 7 8 1 0 1 1 0 1 0 0 1 2 22 1");
        VERIFY_IS_TRUE(state.savedCursor.has_value());
        VERIFY_ARE_EQUAL(7u, state.savedCursor->x);
        VERIFY_ARE_EQUAL(8u, state.savedCursor->y);
        const auto output = RestorePaneState(state, "main", "alternate");
        VERIFY_IS_TRUE(output.find("main\x1b[9;8H\x1b[?1049halternate") != std::string::npos);
        VERIFY_IS_TRUE(output.ends_with("\x1b[6;5H"));
    }

    void TmuxPaneStateTests::RestoresAlternateScreenWithoutSavedCursor()
    {
        const auto state = ParsePaneState("0 0 1 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1");
        const auto output = RestorePaneState(state, "main", "alternate");
        VERIFY_IS_TRUE(output.find("main\x1b[?1047halternate") != std::string::npos);
        VERIFY_IS_TRUE(output.find("?1049h") == std::string::npos);
        VERIFY_IS_TRUE(output.find("4294967295") == std::string::npos);
    }

    void TmuxPaneStateTests::RejectsOversizedRealCoordinates()
    {
        VERIFY_THROWS(ParsePaneState("4294967295 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 32768 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 1 32768 0 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 4294967296 4294967296 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 4294967295 0 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 0 0 1 0 0 0 0 0 0 0 0 0 32768 1"), ProtocolError);
    }

    void TmuxPaneStateTests::RejectsInvalidFlagsRegionsAndFields()
    {
        VERIFY_THROWS(ParsePaneState("0 0 2 0 0 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 0 0 1 0 0 0 0 0 0 0 2 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 0 0 1 0 0 0 0 0 0 0 0 24 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("0 0 0 0 0 1 0 0 0 0 0 0 0 0 0 23 1 extra"), ProtocolError);
        VERIFY_THROWS(ParsePaneState("-1 0 0 0 0 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
    }

    void TmuxPaneStateTests::PreservesCapturedTextAndTerminalModes()
    {
        const auto state = ParsePaneState(" \t2 3 0 0 0 0 1 1 1 1 0 1 0 1 1 20 0\r\n");
        const auto output = RestorePaneState(state, {}, "\\033[32mhello\n\\344\\270\\255");
        VERIFY_IS_TRUE(output.find("\x1b[32mhello\r\n\xe4\xb8\xad") != std::string::npos);
        VERIFY_IS_TRUE(output.find("\x1b[2;21r") != std::string::npos);
        VERIFY_IS_TRUE(output.find("\x1b[?25l\x1b[4h\x1b[?1h\x1b=") != std::string::npos);
        VERIFY_IS_TRUE(output.find("\x1b[?1000h\x1b[?1002l\x1b[?1003h\x1b[?1005l\x1b[?1006h") != std::string::npos);
        VERIFY_IS_TRUE(output.ends_with("\x1b[?7l\x1b[4;3H"));
    }

    void TmuxPaneStateTests::MeasuresFullClientGridWithoutInventedOuterBorders()
    {
        const auto full = MeasureClientSize(1320, 840, 10, 20);
        VERIFY_IS_TRUE(full.has_value());
        VERIFY_ARE_EQUAL(132u, full->columns);
        VERIFY_ARE_EQUAL(42u, full->rows);
        const auto partial = MeasureClientSize(1319.9, 839.9, 10, 20);
        VERIFY_ARE_EQUAL(131u, partial->columns);
        VERIFY_ARE_EQUAL(41u, partial->rows);
        const auto scaled = MeasureClientSize(880, 560, 10.0 / 1.5, 20.0 / 1.5);
        VERIFY_ARE_EQUAL(132u, scaled->columns);
        VERIFY_ARE_EQUAL(42u, scaled->rows);
    }

    void TmuxPaneStateTests::WaitsForFinitePositiveClientAndFontDimensions()
    {
        const auto nan = std::numeric_limits<double>::quiet_NaN();
        const auto inf = std::numeric_limits<double>::infinity();
        VERIFY_IS_FALSE(MeasureClientSize(0, 840, 10, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, 0, 10, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, 840, 0, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, 840, 10, 0).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, 840, nan, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, 840, 10, inf).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(inf, 840, 10, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(1320, nan, 10, 20).has_value());
        VERIFY_IS_FALSE(MeasureClientSize(-1, 840, 10, 20).has_value());
    }

    void TmuxPaneStateTests::ClampsClientGridToSupportedLimits()
    {
        const auto large = MeasureClientSize(1e9, 1e9, 1, 1);
        VERIFY_ARE_EQUAL(32767u, large->columns);
        VERIFY_ARE_EQUAL(32767u, large->rows);
        const auto tiny = MeasureClientSize(1, 1, 10, 20);
        VERIFY_ARE_EQUAL(1u, tiny->columns);
        VERIFY_ARE_EQUAL(1u, tiny->rows);
    }

    void TmuxPaneStateTests::ParsesSnapshotDimensionsAlongsideCursor()
    {
        const auto snapshot = ParsePaneSnapshotState("80 24 69 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1");
        VERIFY_ARE_EQUAL(80u, snapshot.dimensions.columns);
        VERIFY_ARE_EQUAL(24u, snapshot.dimensions.rows);
        VERIFY_ARE_EQUAL(69u, snapshot.pane.cursor.x);
        const auto wrapped = ParsePaneSnapshotState("80 24 80 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1");
        VERIFY_ARE_EQUAL(80u, wrapped.pane.cursor.x);
    }

    void TmuxPaneStateTests::RejectsInconsistentSnapshotDimensions()
    {
        VERIFY_THROWS(ParsePaneSnapshotState("0 24 0 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneSnapshotState("80 0 0 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneSnapshotState("32768 24 0 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneSnapshotState("80 24 81 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneSnapshotState("80 24 0 24 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 23 1"), ProtocolError);
        VERIFY_THROWS(ParsePaneSnapshotState("80 24 0 0 0 4294967295 4294967295 1 0 0 0 0 0 0 0 0 0 24 1"), ProtocolError);
    }
}
