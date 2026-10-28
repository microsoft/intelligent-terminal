// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "../adapter/ProgramStatus.hpp"

#include <algorithm>
#include <array>
#include <climits>

using namespace Microsoft::Console::VirtualTerminal::ProgramStatus;

namespace
{
    bool alphaNumeric(const wchar_t ch) noexcept
    {
        return (ch >= L'a' && ch <= L'z') || (ch >= L'A' && ch <= L'Z') || (ch >= L'0' && ch <= L'9');
    }

    bool nameCharacter(const wchar_t ch) noexcept
    {
        return alphaNumeric(ch) || ch == L'_' || ch == L'.' || ch == L'+' || ch == L'-';
    }

    std::wstring_view trim(std::wstring_view value) noexcept
    {
        const auto whitespace = [](const wchar_t ch) { return ch == L' ' || ch == L'\t' || ch == L'\r' || ch == L'\n'; };
        while (!value.empty() && whitespace(value.front()))
        {
            value.remove_prefix(1);
        }
        while (!value.empty() && whitespace(value.back()))
        {
            value.remove_suffix(1);
        }
        return value;
    }

    bool validId(const std::wstring_view id) noexcept
    {
        if (id.empty() || id.size() > 128)
        {
            return false;
        }
        size_t segments = 1;
        size_t length = 0;
        for (const auto ch : id)
        {
            if (ch == L'/')
            {
                if (length == 0 || ++segments > 8)
                {
                    return false;
                }
                length = 0;
            }
            else if (!nameCharacter(ch) || ++length > 32)
            {
                return false;
            }
        }
        return length != 0;
    }

    std::string ascii(const std::wstring_view value)
    {
        std::string result;
        result.reserve(value.size());
        for (const auto ch : value)
        {
            result.push_back(static_cast<char>(ch));
        }
        return result;
    }

    int base64Digit(const wchar_t ch) noexcept
    {
        if (ch >= L'A' && ch <= L'Z')
        {
            return ch - L'A';
        }
        if (ch >= L'a' && ch <= L'z')
        {
            return ch - L'a' + 26;
        }
        if (ch >= L'0' && ch <= L'9')
        {
            return ch - L'0' + 52;
        }
        if (ch == L'+')
        {
            return 62;
        }
        return ch == L'/' ? 63 : -1;
    }

    // The shared clipboard decoder deliberately accepts URL-safe Base64,
    // excessive padding and replacement UTF-8. Program status requires strict
    // standard Base64 and UTF-8, plus byte limits and control rejection.
    std::optional<std::wstring> decodeText(const std::wstring_view encoded, const size_t maximumBytes)
    {
        auto length = encoded.find(L'=');
        if (length == std::wstring_view::npos)
        {
            length = encoded.size();
        }
        const auto padding = encoded.size() - length;
        if (length % 4 == 1 || (padding && (padding > 2 || encoded.size() % 4 != 0 || padding != (4 - length % 4))) ||
            !std::all_of(encoded.begin() + length, encoded.end(), [](const auto ch) { return ch == L'='; }))
        {
            return std::nullopt;
        }

        std::string bytes;
        bytes.reserve(length * 3 / 4);
        uint32_t bits = 0;
        unsigned available = 0;
        for (size_t i = 0; i < length; ++i)
        {
            const auto digit = base64Digit(encoded[i]);
            if (digit < 0)
            {
                return std::nullopt;
            }
            bits = (bits << 6) | static_cast<uint32_t>(digit);
            available += 6;
            if (available >= 8)
            {
                available -= 8;
                bytes.push_back(static_cast<char>(bits >> available));
            }
        }
        if (bytes.size() > maximumBytes || (available && (bits & ((1u << available) - 1)) != 0))
        {
            return std::nullopt;
        }

        std::wstring text;
        text.reserve(bytes.size());
        for (size_t i = 0; i < bytes.size();)
        {
            auto codepoint = static_cast<uint32_t>(static_cast<unsigned char>(bytes[i++]));
            size_t remaining = 0;
            uint32_t minimum = 0;
            if (codepoint >= 0xc2 && codepoint <= 0xdf)
            {
                remaining = 1;
                codepoint &= 0x1f;
                minimum = 0x80;
            }
            else if (codepoint >= 0xe0 && codepoint <= 0xef)
            {
                remaining = 2;
                codepoint &= 0x0f;
                minimum = 0x800;
            }
            else if (codepoint >= 0xf0 && codepoint <= 0xf4)
            {
                remaining = 3;
                codepoint &= 0x07;
                minimum = 0x10000;
            }
            else if (codepoint >= 0x80)
            {
                return std::nullopt;
            }
            if (remaining > bytes.size() - i)
            {
                return std::nullopt;
            }
            while (remaining--)
            {
                const auto continuation = static_cast<unsigned char>(bytes[i++]);
                if ((continuation & 0xc0) != 0x80)
                {
                    return std::nullopt;
                }
                codepoint = (codepoint << 6) | (continuation & 0x3f);
            }
            if (codepoint < minimum || codepoint > 0x10ffff || (codepoint >= 0xd800 && codepoint <= 0xdfff) ||
                codepoint < 0x20 || (codepoint >= 0x7f && codepoint <= 0x9f))
            {
                return std::nullopt;
            }
            if constexpr (WCHAR_MAX <= 0xffff)
            {
                if (codepoint > 0xffff)
                {
                    codepoint -= 0x10000;
                    text.push_back(static_cast<wchar_t>(0xd800 + (codepoint >> 10)));
                    text.push_back(static_cast<wchar_t>(0xdc00 + (codepoint & 0x3ff)));
                    continue;
                }
            }
            text.push_back(static_cast<wchar_t>(codepoint));
        }
        return text;
    }
}

std::optional<Command> Microsoft::Console::VirtualTerminal::ProgramStatus::Parse(const std::wstring_view body)
{
    if (body.size() > MaximumBodyLength || std::any_of(body.begin(), body.end(), [](const auto ch) { return ch > 0x7f; }))
    {
        return std::nullopt;
    }
    if (body == L"?")
    {
        return Command{};
    }
    if (body.starts_with(L"?:"))
    {
        return std::nullopt;
    }

    Command command;
    command.operation = Command::Operation::Replace;
    std::optional<std::wstring_view> state;
    std::optional<std::wstring_view> kind;
    std::optional<std::wstring_view> progress;
    std::wstring_view remaining = body;
    for (;;)
    {
        const auto colon = remaining.find(L':');
        const auto pair = remaining.substr(0, colon);
        const auto equals = pair.find(L'=');
        if (equals != std::wstring_view::npos)
        {
            const auto key = trim(pair.substr(0, equals));
            const auto value = trim(pair.substr(equals + 1));
            // Check every occurrence before duplicate/malformed-pair filtering.
            // Otherwise a malformed explicit id could silently target the root.
            if (key.size() > 16 || (key == L"id" && !validId(value)) ||
                (key == L"app" && value.size() > 32) ||
                (key == L"title" && value.size() > 256) ||
                (key == L"msg" && value.size() > 2732))
            {
                return std::nullopt;
            }
            if (key == L"title" || key == L"msg")
            {
                auto text = decodeText(value, key == L"title" ? 192 : 2048);
                if (!text)
                {
                    return std::nullopt;
                }
                (key == L"title" ? command.record.title : command.record.message) = std::move(text);
            }
            else if (!key.empty() &&
                     std::all_of(key.begin(), key.end(), [](const auto ch) { return alphaNumeric(ch) || ch == L'_' || ch == L'-'; }) &&
                     std::all_of(value.begin(), value.end(), [](const auto ch) { return nameCharacter(ch) || ch == L'/' || ch == L'='; }))
            {
                if (key == L"state")
                {
                    state = value;
                }
                else if (key == L"id")
                {
                    command.record.id = ascii(value);
                }
                else if (key == L"app")
                {
                    command.record.app = !value.empty() && std::all_of(value.begin(), value.end(), nameCharacter) ?
                                             std::optional{ ascii(value) } :
                                             std::nullopt;
                }
                else if (key == L"kind")
                {
                    kind = value;
                }
                else if (key == L"progress")
                {
                    progress = value;
                }
            }
        }
        if (colon == std::wstring_view::npos)
        {
            break;
        }
        remaining.remove_prefix(colon + 1);
    }

    if (!state)
    {
        return std::nullopt;
    }
    if (*state == L"clear")
    {
        command.operation = Command::Operation::Clear;
        return command;
    }
    constexpr std::array states{ L"idle", L"working", L"blocked", L"done", L"error" };
    const auto match = std::find(states.begin(), states.end(), *state);
    if (match == states.end())
    {
        return std::nullopt;
    }
    command.record.state = static_cast<State>(match - states.begin());
    if (command.record.state == State::Blocked && kind)
    {
        if (*kind == L"permission")
        {
            command.record.kind = BlockKind::Permission;
        }
        else if (*kind == L"question")
        {
            command.record.kind = BlockKind::Question;
        }
        else if (*kind == L"auth")
        {
            command.record.kind = BlockKind::Auth;
        }
    }
    if ((command.record.state == State::Working || command.record.state == State::Blocked) && progress && !progress->empty())
    {
        uint32_t number = 0;
        bool valid = true;
        for (const auto ch : *progress)
        {
            if (ch < L'0' || ch > L'9' || number > 100)
            {
                valid = false;
                break;
            }
            number = number * 10 + static_cast<uint32_t>(ch - L'0');
        }
        if (valid && number <= 100)
        {
            command.record.progress = number;
        }
    }
    return command;
}
