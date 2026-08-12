// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "CompletionCoordinator.h"
#include "AgentPaneLog.h"

namespace winrt::TerminalApp::implementation
{
    CompletionInputAction CompletionInputClassifier::ClassifyKey(const uint16_t vkey, const bool keyDown) noexcept
    {
        if (!keyDown)
        {
            return CompletionInputAction::Ignore;
        }

        return vkey == VK_BACK || vkey == VK_DELETE ?
                   CompletionInputAction::Request :
                   CompletionInputAction::Invalidate;
    }

    CompletionInputAction CompletionInputClassifier::ClassifyCharacter(const wchar_t character) noexcept
    {
        return character > L' ' && character != L'\x7f' ?
                   CompletionInputAction::Request :
                   CompletionInputAction::Invalidate;
    }

    CompletionInputAction CompletionInputClassifier::ClassifyString(const std::wstring_view text) noexcept
    {
        if (text.empty() || text.starts_with(L"\x1b[24~b") || text.starts_with(L"\x1b[24~c"))
        {
            return CompletionInputAction::Ignore;
        }

        for (const auto character : text)
        {
            if (character < L' ' || character == L'\x7f')
            {
                return CompletionInputAction::Invalidate;
            }
        }

        return text.back() == L' ' ?
                   CompletionInputAction::Invalidate :
                   CompletionInputAction::Request;
    }

    uint64_t CompletionRequestState::Schedule(const uintptr_t target) noexcept
    {
        _target = target;
        _phase = Phase::Scheduled;
        return ++_generation;
    }

    bool CompletionRequestState::Dispatch(const uint64_t generation, const uintptr_t target)
    {
        if (_phase != Phase::Scheduled || generation != _generation || target != _target || HasPendingResponse(target))
        {
            return false;
        }

        _phase = Phase::AwaitingResponse;
        _dispatchedRequests[target].push_back(generation);
        return true;
    }

    bool CompletionRequestState::ExpireResponse(const uint64_t generation, const uintptr_t target) noexcept
    {
        const auto requests = _dispatchedRequests.find(target);
        if (requests == _dispatchedRequests.end())
        {
            return false;
        }

        const auto request = std::find(requests->second.begin(), requests->second.end(), generation);
        if (request == requests->second.end())
        {
            return false;
        }

        requests->second.erase(request);
        if (requests->second.empty())
        {
            _dispatchedRequests.erase(requests);
        }

        if (_phase == Phase::AwaitingResponse && target == _target && generation == _generation)
        {
            ++_generation;
            _target = 0;
            _phase = Phase::Idle;
        }
        return true;
    }

    bool CompletionRequestState::AcceptResponse(const uintptr_t target)
    {
        const auto requests = _dispatchedRequests.find(target);
        if (requests == _dispatchedRequests.end() || requests->second.empty())
        {
            return false;
        }

        const auto generation = requests->second.front();
        requests->second.pop_front();
        if (requests->second.empty())
        {
            _dispatchedRequests.erase(requests);
        }

        if (_phase != Phase::AwaitingResponse || target != _target || generation != _generation)
        {
            return false;
        }

        _phase = Phase::Displayed;
        return true;
    }

    bool CompletionRequestState::Invalidate(const uintptr_t target) noexcept
    {
        if (_phase == Phase::Idle || target != _target)
        {
            return false;
        }

        ++_generation;
        _target = 0;
        _phase = Phase::Idle;
        return true;
    }

    bool CompletionRequestState::RemoveTarget(const uintptr_t target) noexcept
    {
        const auto removed = _dispatchedRequests.erase(target) != 0;
        if (_target == target)
        {
            ++_generation;
            _target = 0;
            _phase = Phase::Idle;
            return true;
        }
        return removed;
    }

    bool CompletionRequestState::HasPendingResponse(const uintptr_t target) const noexcept
    {
        const auto requests = _dispatchedRequests.find(target);
        return requests != _dispatchedRequests.end() && !requests->second.empty();
    }

    CompletionRequestState::Phase CompletionRequestState::CurrentPhase() const noexcept
    {
        return _phase;
    }

    uint64_t CompletionRequestState::Generation() const noexcept
    {
        return _generation;
    }

    uintptr_t CompletionRequestState::Target() const noexcept
    {
        return _target;
    }

    CompletionCoordinator::CompletionCoordinator(const Windows::System::DispatcherQueue& dispatcher,
                                                 CanSendRequest canSendRequest,
                                                 SendRequest sendRequest) :
        _canSendRequest{ std::move(canSendRequest) },
        _sendRequest{ std::move(sendRequest) }
    {
        _debouncedRequest = std::make_shared<ThrottledFunc<>>(
            dispatcher,
            til::throttled_func_options{
                .delay = std::chrono::milliseconds{ 150 },
                .debounce = true,
                .trailing = true,
            },
            [this]() {
                _dispatchPending();
            });

        _intelliSenseLog("event=coordinator_initialized debounce_ms=150 trace_capacity=256");
    }

    uint64_t CompletionCoordinator::Request(const Microsoft::Terminal::Control::TermControl& target,
                                            const CompletionRequestKind kind)
    {
        const auto generation = _state.Schedule(_targetKey(target));
        _trace(kind == CompletionRequestKind::Automatic ? "schedule_auto" : "schedule_manual",
               _targetKey(target),
               generation);
        _pendingTarget = target;
        _pendingGeneration = generation;
        _pendingKind = kind;
        _debouncedRequest->Run();
        return generation;
    }

    bool CompletionCoordinator::AcceptResponse(const Microsoft::Terminal::Control::TermControl& target)
    {
        const auto targetKey = _targetKey(target);
        const auto matchesDispatch = _dispatchTarget == targetKey;
        const auto responseGeneration = matchesDispatch ? _dispatchGeneration : 0;
        const auto elapsed = matchesDispatch && _dispatchTick != 0 ?
                                 gsl::narrow_cast<uint32_t>(std::min<uint64_t>(GetTickCount64() - _dispatchTick, UINT32_MAX)) :
                                 0;
        _trace("response_received", targetKey, responseGeneration, elapsed);
        const auto accepted = _state.AcceptResponse(targetKey);
        _trace(accepted ? "response_accepted" : "response_stale", targetKey, responseGeneration, elapsed);
        if (!_state.HasPendingResponse(targetKey))
        {
            if (matchesDispatch)
            {
                _dispatchTick = 0;
                _dispatchTarget = 0;
                _dispatchGeneration = 0;
            }
            _reschedulePending();
        }
        _flushTrace("response");
        return accepted;
    }

    bool CompletionCoordinator::Invalidate(const Microsoft::Terminal::Control::TermControl& target) noexcept
    {
        const auto invalidatedGeneration = _state.Generation();
        const auto invalidated = _state.Invalidate(_targetKey(target));
        _trace(invalidated ? "invalidate_active" : "invalidate_idle",
               _targetKey(target),
               invalidatedGeneration);
        if (invalidated)
        {
            _pendingTarget = {};
        }
        return invalidated;
    }

    bool CompletionCoordinator::RemoveTarget(const Microsoft::Terminal::Control::TermControl& target) noexcept
    {
        const auto targetKey = _targetKey(target);
        const auto removedGeneration = _state.Generation();
        const auto removed = _state.RemoveTarget(targetKey);
        _trace(removed ? "target_removed" : "target_remove_idle", targetKey, removedGeneration);
        if (_dispatchTarget == targetKey)
        {
            _dispatchTick = 0;
            _dispatchTarget = 0;
            _dispatchGeneration = 0;
        }
        if (const auto pendingTarget = _pendingTarget.get();
            pendingTarget && _targetKey(pendingTarget) == targetKey)
        {
            _pendingTarget = {};
        }
        else
        {
            _reschedulePending();
        }
        _flushTrace("target_removed");
        return removed;
    }

    uintptr_t CompletionCoordinator::_targetKey(const Microsoft::Terminal::Control::TermControl& target) noexcept
    {
        return reinterpret_cast<uintptr_t>(winrt::get_abi(target));
    }

    void CompletionCoordinator::_dispatchPending()
    {
        const auto target = _pendingTarget.get();
        if (!target)
        {
            _trace("dispatch_target_gone", 0, _pendingGeneration);
            _flushTrace("dispatch");
            return;
        }

        const auto generation = _pendingGeneration;
        if (_state.HasPendingResponse(_targetKey(target)))
        {
            _trace("dispatch_deferred", _targetKey(target), generation);
            _flushTrace("dispatch");
            return;
        }

        const auto gate = _canSendRequest(target, _pendingKind);
        if (gate.gate != CompletionRequestGate::Allowed)
        {
            const auto event = [&]() {
                switch (gate.gate)
                {
                case CompletionRequestGate::InactiveTarget:
                    return "gate_inactive_target";
                case CompletionRequestGate::MenuDisabled:
                    return "gate_menu_disabled";
                case CompletionRequestGate::OutsidePrompt:
                    return "gate_outside_prompt";
                case CompletionRequestGate::UnsupportedShell:
                    return "gate_unsupported_shell";
                case CompletionRequestGate::AutoDisabled:
                    return "gate_auto_disabled";
                case CompletionRequestGate::PrefixShort:
                    return "gate_prefix_short";
                default:
                    return "gate_rejected";
                }
            }();
            _trace(event, _targetKey(target), generation, gate.detail);
            _state.Invalidate(_targetKey(target));
            _pendingTarget = {};
            _flushTrace("dispatch");
            return;
        }

        _trace(_pendingKind == CompletionRequestKind::Automatic ? "gate_auto_allowed" : "gate_manual_allowed",
               _targetKey(target),
               generation,
               gate.detail);
        if (_state.Dispatch(generation, _targetKey(target)))
        {
            _dispatchTick = GetTickCount64();
            _dispatchTarget = _targetKey(target);
            _dispatchGeneration = generation;
            _trace("dispatch_sent", _dispatchTarget, generation);
            try
            {
                _sendRequest(target, generation);
            }
            catch (...)
            {
                _trace("dispatch_failed", _dispatchTarget, generation);
                _state.ExpireResponse(generation, _targetKey(target));
                _dispatchTick = 0;
                _dispatchTarget = 0;
                _dispatchGeneration = 0;
                _reschedulePending();
                _flushTrace("dispatch");
                throw;
            }
        }
        _flushTrace("dispatch");
    }

    void CompletionCoordinator::_reschedulePending()
    {
        if (_state.CurrentPhase() == CompletionRequestState::Phase::Scheduled && _pendingTarget)
        {
            _debouncedRequest->Run();
        }
    }

    void CompletionCoordinator::_trace(const char* event,
                                       const uintptr_t target,
                                       const uint64_t generation,
                                       const uint32_t detail) noexcept
    {
        auto& record = _traceRecords[_traceWriteIndex];
        record = TraceRecord{
            .sequence = ++_traceSequence,
            .tick = GetTickCount64(),
            .event = event,
            .target = target,
            .generation = generation,
            .detail = detail,
        };
        _traceWriteIndex = (_traceWriteIndex + 1) % _traceRecords.size();
        _traceCount = std::min(_traceCount + 1, _traceRecords.size());
    }

    void CompletionCoordinator::_flushTrace(const char* reason) noexcept
    try
    {
        if (_traceCount == 0)
        {
            return;
        }

        const auto start = (_traceWriteIndex + _traceRecords.size() - _traceCount) % _traceRecords.size();
        std::string batch;
        batch.reserve(_traceCount * 120);
        fmt::format_to(std::back_inserter(batch),
                       FMT_COMPILE("flush={} count={}\n"),
                       reason,
                       _traceCount);
        for (size_t i = 0; i < _traceCount; ++i)
        {
            const auto& record = _traceRecords[(start + i) % _traceRecords.size()];
            fmt::format_to(std::back_inserter(batch),
                           FMT_COMPILE("seq={} tick_ms={} event={} target=0x{:x} generation={} detail={}\n"),
                           record.sequence,
                           record.tick,
                           record.event,
                           record.target,
                           record.generation,
                           record.detail);
        }
        _traceCount = 0;
        _intelliSenseLog(batch);
    }
    catch (...)
    {
        _traceCount = 0;
    }
}
