// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - InlineSuggestionController.cpp
//
// Abstract:
// - Implementation of the inline suggestion state machine.

#include "pch.h"
#include "InlineSuggestionController.h"
#include "ControlCore.h"

using namespace std::chrono_literals;
namespace winrt
{
    using namespace Windows::System;
}

namespace winrt::Microsoft::Terminal::Control::implementation
{
    InlineSuggestionController::InlineSuggestionController(
        ControlCore* core,
        winrt::DispatcherQueue dispatcher) :
        _core{ core },
        _dispatcher{ std::move(dispatcher) }
    {
        // Create the debounce timer on the UI thread
        _debounceTimer = _dispatcher.CreateTimer();
        _debounceTimer.Interval(std::chrono::duration_cast<winrt::Windows::Foundation::TimeSpan>(_debounceMs));
        _debounceTimer.IsRepeating(false);
        _debounceTimer.Tick([this](auto&&, auto&&) {
            _onDebounceTimerFired();
        });
    }

    void InlineSuggestionController::SetProvider(std::unique_ptr<IInlineSuggestionProvider> provider) noexcept
    {
        _provider = std::move(provider);
    }

    void InlineSuggestionController::SetEnabled(bool enabled) noexcept
    {
        if (_enabled == enabled)
            return;

        _enabled = enabled;
        if (!enabled)
        {
            _dismiss(DismissReason::Disabled);
        }
    }

    void InlineSuggestionController::OnEditLineStateChanged() noexcept
    {
        if (!_enabled || !_provider || !_provider->IsAvailable())
            return;

        // Reset line-local suppression if prefix is now empty (new line)
        const auto editLine = _core->GetEditLineState();
        if (editLine.CursorPrefix.empty())
        {
            _lineSuppressed = false;
        }

        // If currently showing, check if prefix still starts with our base
        if (_state == State::Showing)
        {
            // The edit line changed (maybe cursor moved, maybe text changed).
            // We'll re-evaluate by restarting the debounce.
            _dismiss(DismissReason::Typing);
        }

        _startDebounce();
    }

    void InlineSuggestionController::OnShellCompletionMenuOpened() noexcept
    {
        _shellCompletionMenuVisible = true;
        _shellCompletionMenuLastVisibleAt = std::chrono::steady_clock::now();
        if (_state == State::Showing)
        {
            _dismiss(DismissReason::ShellCompletion);
        }
    }

    void InlineSuggestionController::OnShellCompletionMenuClosed() noexcept
    {
        _shellCompletionMenuVisible = false;
        _shellCompletionMenuLastVisibleAt = std::chrono::steady_clock::now();
    }

    void InlineSuggestionController::OnFocusLost() noexcept
    {
        if (_state != State::Idle)
        {
            _dismiss(DismissReason::FocusLoss);
        }
    }

    bool InlineSuggestionController::TryHandleKey(WORD vkey, ::Microsoft::Terminal::Core::ControlKeyStates modifiers) noexcept
    {
        // Ctrl+Esc: toggle session pause
        if (vkey == VK_ESCAPE && modifiers.IsCtrlPressed() && !modifiers.IsAltPressed() && !modifiers.IsShiftPressed())
        {
            _sessionPaused = !_sessionPaused;
            if (_sessionPaused && _state != State::Idle)
            {
                _dismiss(DismissReason::Disabled);
            }
            return true;
        }

        if (_state != State::Showing)
            return false;

        const bool noModifiers = !modifiers.IsCtrlPressed() && !modifiers.IsAltPressed() && !modifiers.IsShiftPressed();

        // Tab or Right arrow at end of line: accept
        if ((vkey == VK_TAB || vkey == VK_RIGHT) && noModifiers)
        {
            // Guard against Tab race with shell completion menu
            if (vkey == VK_TAB && _shellCompletionMenuVisible)
                return false;

            // 100ms safety window after completion menu dismissed
            if (vkey == VK_TAB)
            {
                const auto elapsed = std::chrono::steady_clock::now() - _shellCompletionMenuLastVisibleAt;
                if (elapsed < 100ms)
                    return false;
            }

            _acceptSuggestion();
            return true;
        }

        // Esc: dismiss and suppress further suggestions on this line
        if (vkey == VK_ESCAPE && noModifiers)
        {
            _lineSuppressed = true;
            _dismiss(DismissReason::Escape);
            return true;
        }

        return false;
    }

    CharResult InlineSuggestionController::HandleChar(wchar_t ch) noexcept
    {
        if (_state != State::Showing || _currentSuggestion.empty())
            return CharResult::NotHandled;

        // Check if the typed character matches the first character of the suggestion
        if (ch == _currentSuggestion[0])
        {
            // Prefix-eat: shrink the suggestion by one char
            _currentSuggestion.erase(0, 1);
            _currentPrefix += ch;

            if (_currentSuggestion.empty())
            {
                // Entire suggestion was consumed character by character
                _clearPreview();
                _state = State::Idle;
            }
            else
            {
                // Update the displayed ghost text
                _showSuggestion(_currentSuggestion);
            }
            return CharResult::PrefixEaten;
        }
        else
        {
            // Character diverges from suggestion — dismiss and restart
            _dismiss(DismissReason::Typing);
            _startDebounce();
            return CharResult::Diverged;
        }
    }

    void InlineSuggestionController::_dismiss(DismissReason /*reason*/) noexcept
    {
        _debounceTimer.Stop();

        if (_state == State::Showing)
        {
            _clearPreview();
        }

        _currentSuggestion.clear();
        _currentPrefix.clear();
        _state = State::Idle;
    }

    void InlineSuggestionController::_startDebounce() noexcept
    {
        if (!_enabled || _sessionPaused || _lineSuppressed)
        {
            _state = State::Idle;
            return;
        }

        if (_checkSuppression())
        {
            _state = State::Idle;
            return;
        }

        _debounceTimer.Stop();
        _debounceTimer.Start();
        _state = State::Debouncing;
    }

    void InlineSuggestionController::_onDebounceTimerFired() noexcept
    {
        if (_state != State::Debouncing)
            return;

        if (_checkSuppression())
        {
            _state = State::Idle;
            return;
        }

        _requestSuggestion();
    }

    void InlineSuggestionController::_requestSuggestion() noexcept
    {
        if (!_provider || !_provider->IsAvailable())
        {
            _state = State::Idle;
            return;
        }

        const auto editLine = _core->GetEditLineState();
        const auto prefix = std::wstring{ editLine.CursorPrefix };
        if (prefix.empty())
        {
            _state = State::Idle;
            return;
        }

        // Bump generation ID
        const auto genId = ++_generationId;

        SuggestionRequest request{
            .cursorPrefix = prefix,
            .cwd = L"", // TODO: populate from working directory when available
            .shell = L"pwsh", // TODO: detect actual shell
            .generationId = genId,
        };

        _state = State::Fetching;

        // Launch async provider call and marshal result back to UI thread
        auto future = _provider->SuggestAsync(std::move(request));

        // Move the future into a background thread that waits and posts result back
        std::thread([this, fut = std::move(future), genId, dispatcher = _dispatcher]() mutable {
            try
            {
                auto result = fut.get();
                dispatcher.TryEnqueue([this, result = std::move(result)]() {
                    _onProviderResult(std::move(result));
                });
            }
            catch (...)
            {
                dispatcher.TryEnqueue([this, genId]() {
                    if (_generationId == genId && _state == State::Fetching)
                    {
                        _state = State::Idle;
                    }
                });
            }
        }).detach();
    }

    void InlineSuggestionController::_onProviderResult(SuggestionResult result) noexcept
    {
        // Reject stale results
        if (result.generationId != _generationId)
            return;

        // If we're no longer in Fetching state (got dismissed), ignore
        if (_state != State::Fetching)
            return;

        if (result.kind == SuggestionKind::None || result.suggestion.empty())
        {
            _state = State::Idle;
            return;
        }

        // Final suppression check (viewport width, etc.)
        if (_checkSuppression())
        {
            _state = State::Idle;
            return;
        }

        // Store and display
        const auto editLine = _core->GetEditLineState();
        _currentPrefix = std::wstring{ editLine.CursorPrefix };
        _currentSuggestion = std::move(result.suggestion);
        _state = State::Showing;
        _showSuggestion(_currentSuggestion);
    }

    void InlineSuggestionController::_showSuggestion(std::wstring_view suffix) noexcept
    {
        _core->PreviewInput(suffix, ControlCore::PreviewSource::InlineSuggestion);
    }

    void InlineSuggestionController::_clearPreview() noexcept
    {
        _core->PreviewInput(L"", ControlCore::PreviewSource::InlineSuggestion);
    }

    void InlineSuggestionController::_acceptSuggestion() noexcept
    {
        if (_currentSuggestion.empty())
            return;

        // Send suggestion text to the terminal
        const auto& text = _currentSuggestion;
        for (size_t i = 0; i < text.size();)
        {
            wchar_t ch = text[i];
            if (IS_HIGH_SURROGATE(ch) && i + 1 < text.size() && IS_LOW_SURROGATE(text[i + 1]))
            {
                // Send surrogate pair together
                _core->SendInput(std::wstring_view{ &text[i], 2 });
                i += 2;
            }
            else
            {
                _core->SendCharEvent(ch, 0, ::Microsoft::Terminal::Core::ControlKeyStates{});
                i += 1;
            }
        }

        _clearPreview();
        _currentSuggestion.clear();
        _currentPrefix.clear();
        _state = State::Idle;
    }

    bool InlineSuggestionController::_checkSuppression() const noexcept
    {
        const auto editLine = _core->GetEditLineState();

        // Rule 1: Cursor not at end
        if (!editLine.CursorAtEnd)
            return true;

        // Rule 2: Alt screen buffer
        if (editLine.InAltBuffer)
            return true;

        // Rule 3: Command running
        if (editLine.CommandRunning)
            return true;

        // Rule 4: No prompt mark (shell integration not active)
        if (!editLine.HasPromptMark)
            return true;

        // Rule 5: Paste in progress or recently finished
        if (_core->IsPasteInProgress())
            return true;

        // Rule 9: Empty prefix
        if (editLine.CursorPrefix.empty())
            return true;

        // Rule 10: Selection active
        if (_core->HasSelection())
            return true;

        // Rule 12: Line-local suppressed (Esc pressed)
        if (_lineSuppressed)
            return true;

        // Rule 13: Session paused
        if (_sessionPaused)
            return true;

        return false;
    }
}
