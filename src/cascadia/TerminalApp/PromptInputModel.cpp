// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "PromptInputModel.h"

namespace winrt::TerminalApp::implementation
{
    void PromptInputModel::ApplyKey(const uint16_t vkey, const uint32_t modifiers, const bool keyDown)
    {
        if (!keyDown)
        {
            return;
        }

        if (vkey == VK_CONTROL || vkey == VK_LCONTROL || vkey == VK_RCONTROL ||
            vkey == VK_MENU || vkey == VK_LMENU || vkey == VK_RMENU ||
            vkey == VK_SHIFT || vkey == VK_LSHIFT || vkey == VK_RSHIFT)
        {
            return;
        }

        constexpr uint32_t ctrl = LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED;
        if ((modifiers & ctrl) != 0)
        {
            if (vkey == L'C')
            {
                _reset();
            }
            else
            {
                _text.clear();
                _cursor = 0;
                _trusted = false;
                ++_version;
            }
            return;
        }

        switch (vkey)
        {
        case VK_BACK:
            if (_trusted && _cursor > 0)
            {
                _text.erase(--_cursor, 1);
                ++_version;
            }
            break;
        case VK_DELETE:
            if (_trusted && _cursor < _text.size())
            {
                _text.erase(_cursor, 1);
                ++_version;
            }
            break;
        case VK_LEFT:
            if (_trusted && _cursor > 0)
            {
                --_cursor;
                ++_version;
            }
            break;
        case VK_RIGHT:
            if (_trusted && _cursor < _text.size())
            {
                ++_cursor;
                ++_version;
            }
            break;
        case VK_HOME:
            if (_trusted)
            {
                _cursor = 0;
                ++_version;
            }
            break;
        case VK_END:
            if (_trusted)
            {
                _cursor = _text.size();
                ++_version;
            }
            break;
        case VK_RETURN:
            _reset();
            break;
        case VK_UP:
        case VK_DOWN:
        case VK_ESCAPE:
            _text.clear();
            _cursor = 0;
            _trusted = false;
            ++_version;
            break;
        default:
            break;
        }
    }

    void PromptInputModel::ApplyCharacter(const wchar_t character)
    {
        if (character >= L' ' && character != L'\x7f')
        {
            _insert(character);
        }
    }

    void PromptInputModel::ApplyString(const std::wstring_view text)
    {
        for (const auto character : text)
        {
            if (character == L'\r' || character == L'\n')
            {
                _reset();
            }
            else if (character == L'\b')
            {
                ApplyKey(VK_BACK, 0, true);
            }
            else if (character >= L' ' && character != L'\x7f')
            {
                _insert(character);
            }
            else
            {
                _trusted = false;
                ++_version;
            }
        }
    }

    void PromptInputModel::ApplyCompletion(const size_t replacementIndex,
                                           const size_t replacementLength,
                                           const std::wstring_view text)
    {
        if (!_trusted || replacementIndex > _text.size() || replacementLength > _text.size() - replacementIndex)
        {
            _trusted = false;
            ++_version;
            return;
        }

        _text.replace(replacementIndex, replacementLength, text);
        _cursor = replacementIndex + text.size();
        ++_version;
    }

    PromptInputSnapshot PromptInputModel::Snapshot() const
    {
        return PromptInputSnapshot{
            .text = _text,
            .cursor = _cursor,
            .version = _version,
            .trusted = _trusted,
        };
    }

    void PromptInputModel::_insert(const wchar_t character)
    {
        if (!_trusted)
        {
            return;
        }

        _text.insert(_cursor++, 1, character);
        ++_version;
    }

    void PromptInputModel::_reset() noexcept
    {
        _text.clear();
        _cursor = 0;
        _trusted = true;
        ++_version;
    }
}
