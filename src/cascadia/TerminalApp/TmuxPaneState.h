// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include "TmuxProtocol.h"

#include <array>
#include <cmath>
#include <limits>
#include <optional>

namespace Microsoft::Terminal::Tmux
{
    struct ClientSize
    {
        uint32_t columns;
        uint32_t rows;

        bool operator==(const ClientSize&) const = default;
    };

    struct PaneState
    {
        struct Cursor
        {
            uint32_t x;
            uint32_t y;

            bool operator==(const Cursor&) const = default;
        };

        Cursor cursor;
        bool alternate;
        std::optional<Cursor> savedCursor;
        bool cursorVisible;
        bool insert;
        bool applicationCursor;
        bool applicationKeypad;
        bool mouseStandard;
        bool mouseButton;
        bool mouseAny;
        bool mouseUtf8;
        bool mouseSgr;
        uint32_t scrollTop;
        uint32_t scrollBottom;
        bool wrap;
    };

    inline PaneState ParsePaneState(std::string_view text)
    {
        constexpr std::array names{
            "cursor_x", "cursor_y", "alternate_on", "alternate_saved_x", "alternate_saved_y", "cursor_flag", "insert_flag", "keypad_cursor_flag", "keypad_flag", "mouse_standard_flag", "mouse_button_flag", "mouse_any_flag", "mouse_utf8_flag", "mouse_sgr_flag", "scroll_region_upper", "scroll_region_lower", "wrap_flag"
        };
        std::array<uint64_t, names.size()> fields{};
        for (auto& field : fields)
        {
            const auto start = text.find_first_not_of(" \t\r\n");
            if (start == std::string_view::npos)
            {
                throw ProtocolError{ "Backend does not expose the required tmux pane state fields" };
            }
            text.remove_prefix(start);
            const auto end = text.find_first_of(" \t\r\n");
            field = details::Number(text.substr(0, end));
            text = end == std::string_view::npos ? std::string_view{} : text.substr(end);
        }
        if (text.find_first_not_of(" \t\r\n") != std::string_view::npos)
        {
            throw ProtocolError{ "Unexpected fields in tmux pane state" };
        }
        const auto coordinate = [&](const size_t index) {
            if (fields[index] > 32767)
            {
                throw ProtocolError{ std::string{ "tmux pane " } + names[index] + " exceeds terminal limits" };
            }
            return static_cast<uint32_t>(fields[index]);
        };
        const auto flag = [&](const size_t index) {
            if (fields[index] > 1)
            {
                throw ProtocolError{ std::string{ "Invalid tmux pane flag: " } + names[index] };
            }
            return fields[index] != 0;
        };

        std::optional<PaneState::Cursor> saved;
        constexpr auto unset = std::numeric_limits<uint32_t>::max();
        if (fields[3] != unset || fields[4] != unset)
        {
            saved = PaneState::Cursor{ coordinate(3), coordinate(4) };
        }
        PaneState state{
            { coordinate(0), coordinate(1) },
            flag(2),
            saved,
            flag(5),
            flag(6),
            flag(7),
            flag(8),
            flag(9),
            flag(10),
            flag(11),
            flag(12),
            flag(13),
            coordinate(14),
            coordinate(15),
            flag(16)
        };
        if (state.scrollTop > state.scrollBottom)
        {
            throw ProtocolError{ "Invalid tmux pane scroll region" };
        }
        return state;
    }

    struct PaneSnapshotState
    {
        ClientSize dimensions;
        PaneState pane;
    };

    inline PaneSnapshotState ParsePaneSnapshotState(std::string_view text)
    {
        const auto dimension = [&]() {
            const auto start = text.find_first_not_of(" \t\r\n");
            if (start == std::string_view::npos)
            {
                throw ProtocolError{ "Missing tmux snapshot dimensions" };
            }
            text.remove_prefix(start);
            const auto end = text.find_first_of(" \t\r\n");
            const auto value = details::Number(text.substr(0, end));
            text = end == std::string_view::npos ? std::string_view{} : text.substr(end);
            if (!value || value > 32767)
            {
                throw ProtocolError{ "Invalid tmux snapshot dimensions" };
            }
            return static_cast<uint32_t>(value);
        };
        const auto columns = dimension();
        const auto rows = dimension();
        const auto pane = ParsePaneState(text);
        // tmux allows cursor_x == pane_width for a pending automatic wrap.
        if (pane.cursor.x > columns || pane.cursor.y >= rows || pane.scrollBottom >= rows)
        {
            throw ProtocolError{ "Tmux pane state lies outside its captured dimensions" };
        }
        return { { columns, rows }, pane };
    }

    inline std::string RestorePaneState(const PaneState& state, const std::string_view savedScreen, const std::string_view currentScreen)
    {
        const auto screen = [](const std::string_view captured) {
            const auto decoded = DecodeOctal(captured);
            std::string result;
            result.reserve(decoded.size());
            for (const auto ch : decoded)
            {
                if (ch == '\n')
                {
                    result.push_back('\r');
                }
                result.push_back(ch);
            }
            return result;
        };
        const auto cursor = [](const PaneState::Cursor position) {
            return "\x1b[" + std::to_string(position.y + 1) + ";" + std::to_string(position.x + 1) + "H";
        };

        std::string output = "\x1b"
                             "c\x1b[3J";
        if (state.alternate)
        {
            output.append(screen(savedScreen));
            if (state.savedCursor)
            {
                output.append(cursor(*state.savedCursor)).append("\x1b[?1049h");
            }
            else
            {
                // DECSET 47/1047 may enter the alternate screen without saving a cursor.
                output.append("\x1b[?1047h");
            }
        }
        output.append(screen(currentScreen));
        output.append("\x1b[" + std::to_string(state.scrollTop + 1) + ";" + std::to_string(state.scrollBottom + 1) + "r");
        const auto mode = [&](const bool enabled, const uint32_t code) {
            output.append("\x1b[?" + std::to_string(code)).push_back(enabled ? 'h' : 'l');
        };
        mode(state.cursorVisible, 25);
        output.append(state.insert ? "\x1b[4h" : "\x1b[4l");
        mode(state.applicationCursor, 1);
        output.append(state.applicationKeypad ? "\x1b=" : "\x1b>");
        mode(state.mouseStandard, 1000);
        mode(state.mouseButton, 1002);
        mode(state.mouseAny, 1003);
        mode(state.mouseUtf8, 1005);
        mode(state.mouseSgr, 1006);
        mode(state.wrap, 7);
        output.append(cursor(state.cursor));
        return output;
    }

    inline std::optional<ClientSize> MeasureClientSize(const double width, const double height, const double cellWidth, const double cellHeight)
    {
        if (!std::isfinite(width) || !std::isfinite(height) ||
            !std::isfinite(cellWidth) || !std::isfinite(cellHeight) ||
            width <= 0 || height <= 0 || cellWidth <= 0 || cellHeight <= 0)
        {
            return std::nullopt;
        }
        return ClientSize{
            static_cast<uint32_t>(std::clamp(std::floor(width / cellWidth), 1.0, 32767.0)),
            static_cast<uint32_t>(std::clamp(std::floor(height / cellHeight), 1.0, 32767.0))
        };
    }
}
