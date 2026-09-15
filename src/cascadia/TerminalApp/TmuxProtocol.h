// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxProtocol.h
//
// Abstract:
// - Bounded, byte-preserving tmux control-mode protocol and layout parsing.

#pragma once

#include <algorithm>
#include <charconv>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_set>
#include <utility>
#include <vector>

namespace Microsoft::Terminal::Tmux
{
    using Id = uint64_t;

    class ProtocolError : public std::runtime_error
    {
    public:
        using std::runtime_error::runtime_error;
    };

    struct LayoutNode
    {
        enum class Kind
        {
            Leaf,
            Columns,
            Rows
        };

        Kind kind{ Kind::Leaf };
        uint32_t width{};
        uint32_t height{};
        uint32_t x{};
        uint32_t y{};
        Id paneId{};
        std::vector<LayoutNode> children;

        bool operator==(const LayoutNode& other) const noexcept;
    };

    // Throws ProtocolError for invalid checksums, grammar, geometry or limits.
    inline LayoutNode ParseLayout(std::string_view layout);
    inline std::string DecodeOctal(std::string_view text);

    // Each returned command includes its newline. Empty input produces no commands.
    // Bytes (including NUL and partial UTF-8) are hexadecimal operands, never syntax.
    inline std::vector<std::string> EncodeSendKeys(Id paneId, std::string_view bytes);

    struct Event
    {
        enum class Kind
        {
            Response,
            Output,
            Notification,
            Exit
        };

        Kind kind{ Kind::Notification };
        uint64_t commandNumber{};
        bool success{};
        Id paneId{};
        // Response: raw body lines joined by LF, without a trailing LF.
        // Output: decoded bytes. Notification: raw arguments. Exit: raw reason.
        std::string text;
        // Notification name without the leading '%'.
        std::string name;
        // Response guard flags. Bit 0 identifies commands from the control client;
        // unsolicited startup responses have this bit clear.
        uint64_t flags{};
    };

    class Parser
    {
    public:
        static constexpr size_t MaxLineBytes = 1024 * 1024;
        static constexpr size_t MaxResponseBytes = 16 * 1024 * 1024;

        // Single-consumer API. A parse error poisons this parser; construct a new
        // parser for a new stream. Call Finish on transport EOF, even after %exit.
        std::vector<Event> Feed(std::string_view bytes);
        void Finish();

    private:
        enum class Framing
        {
            Detect,
            Opening,
            Plain,
            Framed,
            Closing,
            Closed
        };

        struct Guard
        {
            uint64_t time{};
            uint64_t number{};
            uint64_t flags{};
        };

        void _FeedByte(char byte, std::vector<Event>& events);
        void _Line(std::vector<Event>& events);
        static Guard _ParseGuard(std::string_view args);

        Framing _framing{ Framing::Detect };
        size_t _openingLength{};
        std::string _line;
        std::string _response;
        Guard _guard;
        bool _responseOpen{};
        bool _responseHasLine{};
        bool _exited{};
        bool _failed{};
        bool _finished{};
    };

    namespace details
    {
        inline constexpr size_t MaxLayoutBytes = 1024 * 1024;
        inline constexpr size_t MaxLayoutNodes = 4096;
        inline constexpr size_t MaxLayoutDepth = 64;
        inline constexpr uint32_t MaxDimension = 32767;

        inline uint64_t Number(const std::string_view text)
        {
            if (text.empty())
            {
                throw ProtocolError{ "Missing tmux unsigned integer" };
            }
            uint64_t value{};
            const auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), value);
            if (error != std::errc{} || end != text.data() + text.size())
            {
                throw ProtocolError{ "Invalid tmux unsigned integer" };
            }
            return value;
        }

        inline std::string_view TakeWord(std::string_view& text)
        {
            const auto space = text.find(' ');
            const auto word = text.substr(0, space);
            text = space == std::string_view::npos ? std::string_view{} : text.substr(space + 1);
            return word;
        }

        inline Id PaneId(const std::string_view text)
        {
            if (text.size() < 2 || text.front() != '%')
            {
                throw ProtocolError{ "Invalid tmux pane ID" };
            }
            return Number(text.substr(1));
        }

        class LayoutReader
        {
        public:
            explicit LayoutReader(const std::string_view text) :
                _text{ text }
            {
            }

            LayoutNode Read(const size_t depth = 0)
            {
                if (depth >= MaxLayoutDepth || ++_nodes > MaxLayoutNodes)
                {
                    throw ProtocolError{ "Tmux layout exceeds nesting or node limit" };
                }

                LayoutNode node;
                node.width = _Dimension();
                _Expect('x');
                node.height = _Dimension();
                _Expect(',');
                node.x = _Coordinate();
                _Expect(',');
                node.y = _Coordinate();
                if (node.width > MaxDimension - node.x || node.height > MaxDimension - node.y)
                {
                    throw ProtocolError{ "Tmux layout lies outside coordinate limits" };
                }

                if (_Consume(','))
                {
                    node.paneId = _Number();
                    if (!_panes.insert(node.paneId).second)
                    {
                        throw ProtocolError{ "Duplicate tmux pane ID" };
                    }
                    return node;
                }

                char close{};
                if (_Consume('{'))
                {
                    node.kind = LayoutNode::Kind::Columns;
                    close = '}';
                }
                else if (_Consume('['))
                {
                    node.kind = LayoutNode::Kind::Rows;
                    close = ']';
                }
                else
                {
                    throw ProtocolError{ "Missing tmux layout pane or children" };
                }

                do
                {
                    node.children.emplace_back(Read(depth + 1));
                } while (_Consume(','));
                _Expect(close);

                if (node.children.size() < 2)
                {
                    throw ProtocolError{ "Tmux split must have at least two children" };
                }
                const auto columns = node.kind == LayoutNode::Kind::Columns;
                uint64_t cursor = columns ? node.x : node.y;
                for (const auto& child : node.children)
                {
                    if (columns ? child.x != cursor || child.y != node.y || child.height != node.height :
                                  child.y != cursor || child.x != node.x || child.width != node.width)
                    {
                        throw ProtocolError{ "Invalid tmux child position or cross dimension" };
                    }
                    cursor += (columns ? child.width : child.height) + 1ull;
                }
                if (cursor - 1 != (columns ? node.x + node.width : node.y + node.height))
                {
                    throw ProtocolError{ "Tmux children do not fill their parent" };
                }
                return node;
            }

            bool Done() const noexcept
            {
                return _position == _text.size();
            }

        private:
            bool _Consume(const char expected) noexcept
            {
                if (_position < _text.size() && _text[_position] == expected)
                {
                    ++_position;
                    return true;
                }
                return false;
            }

            void _Expect(const char expected)
            {
                if (!_Consume(expected))
                {
                    throw ProtocolError{ "Invalid tmux layout grammar" };
                }
            }

            uint64_t _Number()
            {
                const auto start = _position;
                while (_position < _text.size() && _text[_position] >= '0' && _text[_position] <= '9')
                {
                    ++_position;
                }
                return Number(_text.substr(start, _position - start));
            }

            uint32_t _Coordinate()
            {
                const auto value = _Number();
                if (value > MaxDimension)
                {
                    throw ProtocolError{ "Tmux coordinate exceeds limit" };
                }
                return static_cast<uint32_t>(value);
            }

            uint32_t _Dimension()
            {
                const auto value = _Coordinate();
                if (value == 0)
                {
                    throw ProtocolError{ "Tmux dimension must be positive" };
                }
                return value;
            }

            std::string_view _text;
            size_t _position{};
            size_t _nodes{};
            std::unordered_set<Id> _panes;
        };
    }

    inline bool LayoutNode::operator==(const LayoutNode& other) const noexcept
    {
        return kind == other.kind && width == other.width && height == other.height &&
               x == other.x && y == other.y && paneId == other.paneId && children == other.children;
    }

    inline LayoutNode ParseLayout(const std::string_view layout)
    {
        if (layout.size() < 6 || layout.size() > details::MaxLayoutBytes || layout[4] != ',')
        {
            throw ProtocolError{ "Missing tmux layout checksum or oversized layout" };
        }
        uint16_t expected{};
        const auto [end, error] = std::from_chars(layout.data(), layout.data() + 4, expected, 16);
        if (error != std::errc{} || end != layout.data() + 4)
        {
            throw ProtocolError{ "Invalid tmux layout checksum" };
        }
        uint16_t checksum{};
        for (const auto ch : layout.substr(5))
        {
            checksum = static_cast<uint16_t>((checksum >> 1) | ((checksum & 1) << 15));
            checksum = static_cast<uint16_t>(checksum + static_cast<unsigned char>(ch));
        }
        if (checksum != expected)
        {
            throw ProtocolError{ "Tmux layout checksum mismatch" };
        }
        details::LayoutReader reader{ layout.substr(5) };
        auto node = reader.Read();
        if (!reader.Done())
        {
            throw ProtocolError{ "Trailing data in tmux layout" };
        }
        return node;
    }

    inline std::string DecodeOctal(const std::string_view text)
    {
        if (text.size() > Parser::MaxLineBytes)
        {
            throw ProtocolError{ "Oversized tmux escaped data" };
        }
        std::string result;
        result.reserve(text.size());
        for (size_t i = 0; i < text.size(); ++i)
        {
            auto byte = text[i];
            if (byte == '\\')
            {
                if (text.size() - i < 4 ||
                    text[i + 1] < '0' || text[i + 1] > '3' ||
                    text[i + 2] < '0' || text[i + 2] > '7' ||
                    text[i + 3] < '0' || text[i + 3] > '7')
                {
                    throw ProtocolError{ "Invalid tmux octal escape" };
                }
                byte = static_cast<char>(((text[i + 1] - '0') << 6) |
                                         ((text[i + 2] - '0') << 3) |
                                         (text[i + 3] - '0'));
                i += 3;
            }
            result.push_back(byte);
        }
        return result;
    }

    inline std::vector<std::string> EncodeSendKeys(const Id paneId, const std::string_view bytes)
    {
        // Staying below 4 KiB also accommodates control-mode backends behind
        // line-oriented transports. Never split an individual hexadecimal token.
        constexpr size_t bytesPerCommand = 1024;
        constexpr std::string_view hex{ "0123456789abcdef" };
        std::vector<std::string> commands;
        const auto prefix = "send-keys -H -t %" + std::to_string(paneId);
        for (size_t position = 0; position < bytes.size();)
        {
            const auto count = (std::min)(bytes.size() - position, bytesPerCommand);
            auto& command = commands.emplace_back(prefix);
            command.reserve(prefix.size() + count * 3 + 1);
            for (size_t i = 0; i < count; ++i)
            {
                const auto byte = static_cast<unsigned char>(bytes[position + i]);
                command.push_back(' ');
                command.push_back(hex[byte >> 4]);
                command.push_back(hex[byte & 15]);
            }
            command.push_back('\n');
            position += count;
        }
        return commands;
    }

    inline Parser::Guard Parser::_ParseGuard(std::string_view args)
    {
        Guard guard;
        guard.time = details::Number(details::TakeWord(args));
        guard.number = details::Number(details::TakeWord(args));
        guard.flags = details::Number(args);
        return guard;
    }

    inline void Parser::_Line(std::vector<Event>& events)
    {
        std::string_view line{ _line };
        if (!line.empty() && line.back() == '\r')
        {
            line.remove_suffix(1);
        }
        auto args = line;
        const auto word = details::TakeWord(args);
        if (_responseOpen)
        {
            if (word == "%end" || word == "%error")
            {
                const auto guard = _ParseGuard(args);
                if (guard.time != _guard.time || guard.number != _guard.number || guard.flags != _guard.flags)
                {
                    throw ProtocolError{ "Mismatched tmux response guard" };
                }
                Event event;
                event.kind = Event::Kind::Response;
                event.commandNumber = guard.number;
                event.success = word == "%end";
                event.flags = guard.flags;
                event.text = std::move(_response);
                events.emplace_back(std::move(event));
                _response.clear();
                _responseOpen = false;
                _responseHasLine = false;
            }
            else
            {
                if (word == "%begin")
                {
                    throw ProtocolError{ "Nested tmux response guard" };
                }
                const auto separator = _responseHasLine ? size_t{ 1 } : size_t{ 0 };
                if (line.size() + separator > MaxResponseBytes - _response.size())
                {
                    throw ProtocolError{ "Tmux response exceeds buffer limit" };
                }
                if (_responseHasLine)
                {
                    _response.push_back('\n');
                }
                _response.append(line);
                _responseHasLine = true;
            }
        }
        else if (_exited)
        {
            throw ProtocolError{ "Tmux data after exit" };
        }
        else if (word == "%begin")
        {
            _guard = _ParseGuard(args);
            _responseOpen = true;
        }
        else if (word == "%end" || word == "%error")
        {
            throw ProtocolError{ "Tmux response ended without beginning" };
        }
        else if (word == "%output" || word == "%extended-output")
        {
            Event event;
            event.kind = Event::Kind::Output;
            const auto paneEnd = args.find(' ');
            if (paneEnd == std::string_view::npos)
            {
                throw ProtocolError{ "Missing tmux output payload delimiter" };
            }
            event.paneId = details::PaneId(details::TakeWord(args));
            if (word == "%extended-output")
            {
                details::Number(details::TakeWord(args)); // Age in milliseconds.
                while (true)
                {
                    if (args.empty())
                    {
                        throw ProtocolError{ "Missing tmux extended-output delimiter" };
                    }
                    const auto field = details::TakeWord(args);
                    if (field == ":")
                    {
                        break;
                    }
                    if (field.empty())
                    {
                        throw ProtocolError{ "Invalid tmux extended-output field" };
                    }
                }
            }
            event.text = DecodeOctal(args);
            events.emplace_back(std::move(event));
        }
        else if (word == "%exit")
        {
            Event event;
            event.kind = Event::Kind::Exit;
            event.text = args;
            events.emplace_back(std::move(event));
            _exited = true;
        }
        else
        {
            if (word.size() < 2 || word.front() != '%' ||
                !std::all_of(word.begin() + 1, word.end(), [](const char ch) {
                    return (ch >= 'a' && ch <= 'z') || ch == '-';
                }))
            {
                throw ProtocolError{ "Invalid tmux notification" };
            }
            Event event;
            event.kind = Event::Kind::Notification;
            event.name = word.substr(1);
            event.text = args;
            events.emplace_back(std::move(event));
        }
        _line.clear();
    }

    inline void Parser::_FeedByte(const char byte, std::vector<Event>& events)
    {
        constexpr std::string_view opening{ "\x1bP1000p" };
        if (_framing == Framing::Detect)
        {
            _framing = byte == '\x1b' ? Framing::Opening : Framing::Plain;
        }
        if (_framing == Framing::Opening)
        {
            if (byte != opening[_openingLength++])
            {
                throw ProtocolError{ "Invalid tmux control-mode framing prefix" };
            }
            if (_openingLength == opening.size())
            {
                _framing = Framing::Framed;
            }
            return;
        }
        if (_framing == Framing::Closing)
        {
            if (byte != '\\')
            {
                throw ProtocolError{ "Invalid tmux control-mode framing suffix" };
            }
            _framing = Framing::Closed;
            return;
        }
        if (_framing == Framing::Closed)
        {
            throw ProtocolError{ "Data after tmux framing suffix" };
        }
        if (_framing == Framing::Framed && _line.empty() && !_responseOpen && byte == '\x1b')
        {
            if (!_exited)
            {
                throw ProtocolError{ "Tmux framing ended before exit" };
            }
            _framing = Framing::Closing;
            return;
        }
        if (byte == '\n')
        {
            _Line(events);
        }
        else
        {
            if (_line.size() >= MaxLineBytes && !(_line.size() == MaxLineBytes && byte == '\r'))
            {
                throw ProtocolError{ "Tmux line exceeds buffer limit" };
            }
            _line.push_back(byte);
        }
    }

    inline std::vector<Event> Parser::Feed(const std::string_view bytes)
    {
        if (_failed || _finished)
        {
            throw ProtocolError{ "Tmux parser is failed or finished" };
        }
        try
        {
            std::vector<Event> events;
            for (const auto byte : bytes)
            {
                _FeedByte(byte, events);
            }
            return events;
        }
        catch (...)
        {
            _failed = true;
            throw;
        }
    }

    inline void Parser::Finish()
    {
        if (_failed)
        {
            throw ProtocolError{ "Tmux parser has failed" };
        }
        if (_responseOpen || !_line.empty() ||
            (_framing != Framing::Detect && _framing != Framing::Plain && _framing != Framing::Closed))
        {
            _failed = true;
            throw ProtocolError{ "Truncated tmux control-mode stream" };
        }
        _finished = true;
    }
}
