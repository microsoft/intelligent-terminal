// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// BoundedDispatchQueue.h
//
// A small, COM-free, header-only multi-producer / single-consumer queue used
// to decouple event producers from a slow or blocked event consumer.
//
// It exists so that the protocol event fan-out (TerminalProtocolComServer)
// never makes its synchronous, cross-process OnEvent callbacks on the thread
// that produced the event — in particular the UI/STA thread (see issue #239).
// Producers call try_push() and return immediately; a dedicated consumer
// thread drains the queue via wait_pop() and performs the (potentially
// blocking) delivery on its own thread.
//
// The COM/threading/marshaling concerns live in the owner; THIS type is pure
// data + synchronization so the back-pressure and lifecycle behavior can be
// unit-tested without a COM apartment. Behavior summary:
//
//   * Bounded: at most `maxItems` are retained. When full, the OLDEST item is
//     evicted (drop-oldest) and a dropped counter is incremented — this caps
//     memory if the consumer stops draining (e.g. a subscriber whose pipe is
//     full) and prefers the most recent events. Producers never block.
//   * Active gate: try_push is a no-op while inactive (mirrors "not
//     subscribed"), so producers don't accumulate work nothing will consume.
//   * Stop: wakes the consumer so wait_pop() returns false and drops any
//     backlog — used for teardown. Re-activating clears a prior stop so one
//     queue instance can be reused across subscribe/unsubscribe cycles.
//
// Ordering is FIFO per queue. There is a single logical consumer (wait_pop is
// not designed to be called concurrently from multiple threads); any number of
// producers may call try_push concurrently.

#pragma once

#include <condition_variable>
#include <cstdint>
#include <deque>
#include <mutex>
#include <utility>

namespace Microsoft::Terminal
{
    template<typename T>
    class BoundedDispatchQueue
    {
    public:
        explicit BoundedDispatchQueue(size_t maxItems) noexcept :
            _maxItems{ maxItems > 0 ? maxItems : 1 } // clamp: 0 would pop_front() an empty deque (UB)
        {
        }

        BoundedDispatchQueue(const BoundedDispatchQueue&) = delete;
        BoundedDispatchQueue& operator=(const BoundedDispatchQueue&) = delete;

        // Producer side. Returns true if the item was queued, false if it was
        // dropped because the queue is inactive or stopped. When the queue is
        // already at capacity the OLDEST item is evicted (and dropped_count is
        // incremented) to make room; the push still succeeds. Never blocks.
        bool try_push(T item)
        {
            {
                std::lock_guard lock{ _mutex };
                if (!_active || _stopped)
                {
                    return false;
                }
                if (_queue.size() >= _maxItems)
                {
                    _queue.pop_front();
                    ++_droppedCount;
                }
                _queue.push_back(std::move(item));
            }
            _cv.notify_one();
            return true;
        }

        // Consumer side. Blocks until an item is available or the queue is
        // stopped. On stop returns false (backlog already dropped by stop()).
        // Otherwise moves the front item into `out` and returns true.
        bool wait_pop(T& out)
        {
            std::unique_lock lock{ _mutex };
            _cv.wait(lock, [this]() { return _stopped || !_queue.empty(); });
            if (_stopped)
            {
                return false;
            }
            out = std::move(_queue.front());
            _queue.pop_front();
            return true;
        }

        // Enable/disable producer pushes (the "subscribed" gate). Re-activating
        // also clears a prior stop() so the queue can be reused.
        void set_active(bool active)
        {
            std::lock_guard lock{ _mutex };
            _active = active;
            if (active)
            {
                _stopped = false;
            }
        }

        // Wake the consumer so wait_pop() returns false, and drop any backlog.
        void stop()
        {
            {
                std::lock_guard lock{ _mutex };
                _stopped = true;
                _queue.clear();
            }
            _cv.notify_all();
        }

        // ── Observers (test / diagnostics) ──
        size_t size() const
        {
            std::lock_guard lock{ _mutex };
            return _queue.size();
        }

        uint64_t dropped_count() const
        {
            std::lock_guard lock{ _mutex };
            return _droppedCount;
        }

        bool is_active() const
        {
            std::lock_guard lock{ _mutex };
            return _active;
        }

    private:
        mutable std::mutex _mutex;
        std::condition_variable _cv;
        std::deque<T> _queue;
        const size_t _maxItems;
        uint64_t _droppedCount{ 0 };
        bool _active{ false };
        bool _stopped{ false };
    };
}
