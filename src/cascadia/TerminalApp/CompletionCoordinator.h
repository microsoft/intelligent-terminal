// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <array>
#include <deque>
#include <unordered_map>

#include <ThrottledFunc.h>

namespace winrt::TerminalApp::implementation
{
    enum class CompletionInputAction
    {
        Ignore,
        Invalidate,
        Request,
    };

    enum class CompletionRequestKind
    {
        Manual,
        Automatic,
    };

    enum class CompletionRequestGate
    {
        Allowed,
        InactiveTarget,
        MenuDisabled,
        OutsidePrompt,
        UnsupportedShell,
        AutoDisabled,
        PrefixShort,
    };

    struct CompletionRequestGateResult
    {
        CompletionRequestGate gate;
        uint32_t detail{ 0 };
    };

    class CompletionInputClassifier
    {
    public:
        static CompletionInputAction ClassifyKey(uint16_t vkey, bool keyDown) noexcept;
        static CompletionInputAction ClassifyCharacter(wchar_t character) noexcept;
        static CompletionInputAction ClassifyString(std::wstring_view text) noexcept;
    };

    class CompletionRequestState
    {
    public:
        enum class Phase
        {
            Idle,
            Scheduled,
            AwaitingResponse,
            Displayed,
        };

        uint64_t Schedule(uintptr_t target) noexcept;
        bool Dispatch(uint64_t generation, uintptr_t target);
        bool AcceptResponse(uintptr_t target);
        bool ExpireResponse(uint64_t generation, uintptr_t target) noexcept;
        bool Invalidate(uintptr_t target) noexcept;
        bool RemoveTarget(uintptr_t target) noexcept;
        bool HasPendingResponse(uintptr_t target) const noexcept;

        Phase CurrentPhase() const noexcept;
        uint64_t Generation() const noexcept;
        uintptr_t Target() const noexcept;

    private:
        uint64_t _generation{ 0 };
        uintptr_t _target{ 0 };
        Phase _phase{ Phase::Idle };
        std::unordered_map<uintptr_t, std::deque<uint64_t>> _dispatchedRequests;
    };

    class CompletionCoordinator
    {
    public:
        using CanSendRequest = std::function<CompletionRequestGateResult(const winrt::Microsoft::Terminal::Control::TermControl&, CompletionRequestKind)>;
        using SendRequest = std::function<void(const winrt::Microsoft::Terminal::Control::TermControl&, uint64_t)>;

        CompletionCoordinator(const winrt::Windows::System::DispatcherQueue& dispatcher,
                              CanSendRequest canSendRequest,
                              SendRequest sendRequest);

        uint64_t Request(const winrt::Microsoft::Terminal::Control::TermControl& target, CompletionRequestKind kind);
        bool AcceptResponse(const winrt::Microsoft::Terminal::Control::TermControl& target);
        bool Invalidate(const winrt::Microsoft::Terminal::Control::TermControl& target) noexcept;
        bool RemoveTarget(const winrt::Microsoft::Terminal::Control::TermControl& target) noexcept;

    private:
        static uintptr_t _targetKey(const winrt::Microsoft::Terminal::Control::TermControl& target) noexcept;
        void _dispatchPending();
        void _reschedulePending();
        void _trace(const char* event, uintptr_t target, uint64_t generation, uint32_t detail = 0) noexcept;
        void _flushTrace(const char* reason) noexcept;

        struct TraceRecord
        {
            uint64_t sequence;
            uint64_t tick;
            const char* event;
            uintptr_t target;
            uint64_t generation;
            uint32_t detail;
        };

        CompletionRequestState _state;
        winrt::weak_ref<winrt::Microsoft::Terminal::Control::TermControl> _pendingTarget;
        uint64_t _pendingGeneration{ 0 };
        CompletionRequestKind _pendingKind{ CompletionRequestKind::Manual };
        std::array<TraceRecord, 256> _traceRecords{};
        size_t _traceWriteIndex{ 0 };
        size_t _traceCount{ 0 };
        uint64_t _traceSequence{ 0 };
        uint64_t _dispatchTick{ 0 };
        uintptr_t _dispatchTarget{ 0 };
        uint64_t _dispatchGeneration{ 0 };
        CanSendRequest _canSendRequest;
        SendRequest _sendRequest;
        std::shared_ptr<ThrottledFunc<>> _debouncedRequest;
    };
}
