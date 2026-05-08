// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - InlineSuggestionController.h
//
// Abstract:
// - State machine that manages inline ghost-text suggestions.
//   Subscribes to edit-line state changes, debounces, calls provider,
//   and manages preview text lifecycle. All public methods must be
//   called on the UI thread.

#pragma once

#include "IInlineSuggestionProvider.h"
#include "../../cascadia/TerminalCore/ControlKeyStates.hpp"

#include <chrono>
#include <memory>
#include <string>
#include <functional>

namespace winrt::Microsoft::Terminal::Control::implementation
{
    struct ControlCore;

    // Return values from HandleChar
    enum class CharResult
    {
        NotHandled, // Controller not showing or char didn't match
        PrefixEaten, // Char matched first ghost char; suggestion shrunk
        Diverged, // Char diverged; suggestion dismissed
    };

    class InlineSuggestionController
    {
    public:
        InlineSuggestionController(
            ControlCore* core,
            winrt::Windows::System::DispatcherQueue dispatcher);

        ~InlineSuggestionController() = default;

        // Called on UI thread when edit line state changes (marshaled from IO thread)
        void OnEditLineStateChanged() noexcept;

        // Called on UI thread when shell completion menu opens
        void OnShellCompletionMenuOpened() noexcept;

        // Called on UI thread when shell completion menu closes
        void OnShellCompletionMenuClosed() noexcept;

        // Try to handle a key press. Returns true if handled (consumed).
        // Only call on keyDown for Tab, RightArrow, Esc, Ctrl+Esc.
        bool TryHandleKey(WORD vkey, ::Microsoft::Terminal::Core::ControlKeyStates modifiers) noexcept;

        // Handle a committed character. Returns how the char relates to
        // the current suggestion (if showing).
        CharResult HandleChar(wchar_t ch) noexcept;

        // Whether a suggestion is currently being displayed
        bool IsShowing() const noexcept { return _state == State::Showing; }

        // Set the provider (call on UI thread)
        void SetProvider(std::unique_ptr<IInlineSuggestionProvider> provider) noexcept;

        // Enable/disable the controller
        void SetEnabled(bool enabled) noexcept;

        // Notify that focus was lost
        void OnFocusLost() noexcept;

    private:
        enum class State
        {
            Idle,
            Debouncing,
            Fetching,
            Showing,
        };

        enum class DismissReason
        {
            Typing,
            Escape,
            Enter,
            FocusLoss,
            ShellCompletion,
            Preempted,
            Disabled,
        };

        void _dismiss(DismissReason reason) noexcept;
        void _startDebounce() noexcept;
        void _onDebounceTimerFired() noexcept;
        void _requestSuggestion() noexcept;
        void _onProviderResult(SuggestionResult result) noexcept;
        void _showSuggestion(std::wstring_view suffix) noexcept;
        void _acceptSuggestion() noexcept;
        void _clearPreview() noexcept;
        bool _checkSuppression() const noexcept;

        // Non-owning pointer to the ControlCore (lives longer than us)
        ControlCore* _core = nullptr;
        winrt::Windows::System::DispatcherQueue _dispatcher{ nullptr };

        State _state = State::Idle;
        bool _enabled = false;
        bool _lineSuppressed = false; // Esc pressed on this line
        bool _sessionPaused = false; // Ctrl+Esc pressed

        // Current suggestion being shown
        std::wstring _currentSuggestion;
        std::wstring _currentPrefix; // The prefix that generated this suggestion

        // Generation ID: monotonically increasing, used to reject stale results
        uint64_t _generationId = 0;

        // Debounce timer
        winrt::Windows::System::DispatcherQueueTimer _debounceTimer{ nullptr };
        std::chrono::milliseconds _debounceMs{ 300 };

        // Shell completion menu state
        bool _shellCompletionMenuVisible = false;
        std::chrono::steady_clock::time_point _shellCompletionMenuLastVisibleAt{};

        // Provider
        std::unique_ptr<IInlineSuggestionProvider> _provider;
    };
}
