// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

namespace winrt::TerminalApp::implementation
{
    struct PromptInputSnapshot
    {
        std::wstring text;
        size_t cursor{ 0 };
        uint64_t version{ 0 };
        bool trusted{ true };
    };

    class PromptInputModel
    {
    public:
        void ApplyKey(uint16_t vkey, uint32_t modifiers, bool keyDown);
        void ApplyCharacter(wchar_t character);
        void ApplyString(std::wstring_view text);
        void ApplyCompletion(size_t replacementIndex, size_t replacementLength, std::wstring_view text);
        PromptInputSnapshot Snapshot() const;

    private:
        void _insert(wchar_t character);
        void _reset() noexcept;

        std::wstring _text;
        size_t _cursor{ 0 };
        uint64_t _version{ 0 };
        bool _trusted{ true };
    };
}
