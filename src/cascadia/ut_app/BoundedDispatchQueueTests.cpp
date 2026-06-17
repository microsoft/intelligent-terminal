// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// BoundedDispatchQueueTests.cpp
//
// Spec-derived tests for src/cascadia/inc/BoundedDispatchQueue.h — the
// dependency-free multi-producer / single-consumer queue that decouples
// protocol event producers (the UI/STA thread, COM MTA threads) from the
// per-subscriber delivery worker in TerminalProtocolComServer (issue #239).
//
// These assert the queue's CONTRACT (FIFO, drop-oldest back-pressure, the
// subscribe gate, stop semantics, and integrity under concurrent producers)
// rather than re-deriving the current implementation — the goal is to surface
// bugs, not to bless the code.

#include "precomp.h"

#include <atomic>
#include <chrono>
#include <set>
#include <string>
#include <thread>
#include <vector>

#include "../inc/BoundedDispatchQueue.h"

using namespace WEX::Logging;
using namespace WEX::TestExecution;
using namespace WEX::Common;
using namespace Microsoft::Terminal;

namespace TerminalAppUnitTests
{
    class BoundedDispatchQueueTests
    {
        TEST_CLASS(BoundedDispatchQueueTests);

        TEST_METHOD(InactiveByDefaultRejectsPush);
        TEST_METHOD(ClampsZeroCapacityToOne);
        TEST_METHOD(PreservesFifoOrder);
        TEST_METHOD(DropsOldestWhenFullAndCountsDrops);
        TEST_METHOD(SetInactiveGatesPushButKeepsBacklog);
        TEST_METHOD(StopDropsBacklogAndWaitPopReturnsFalse);
        TEST_METHOD(ReactivateAfterStopAcceptsAgain);
        TEST_METHOD(WaitPopUnblocksOnStop);
        TEST_METHOD(WaitPopUnblocksOnPush);
        TEST_METHOD(ConcurrentProducersLoseNothingWhenUnbounded);
    };

    // A queue is inactive (closed gate) until set_active(true). This mirrors
    // an instance that authenticated but never Subscribe'd — it must not
    // accumulate work nothing will deliver.
    void BoundedDispatchQueueTests::InactiveByDefaultRejectsPush()
    {
        BoundedDispatchQueue<std::string> q{ 10 };

        VERIFY_IS_FALSE(q.is_active());
        VERIFY_IS_FALSE(q.try_push("x"), L"push must be rejected while inactive");
        VERIFY_ARE_EQUAL(size_t{ 0 }, q.size());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, q.dropped_count(), L"a gated push is not an overflow drop");

        q.set_active(true);
        VERIFY_IS_TRUE(q.try_push("x"));
        VERIFY_ARE_EQUAL(size_t{ 1 }, q.size());
    }

    // A zero capacity must be clamped to 1 — otherwise try_push would pop_front()
    // an empty deque (undefined behavior) on the very first overflow.
    void BoundedDispatchQueueTests::ClampsZeroCapacityToOne()
    {
        BoundedDispatchQueue<std::string> q{ 0 };
        q.set_active(true);

        VERIFY_IS_TRUE(q.try_push("a"));
        VERIFY_IS_TRUE(q.try_push("b")); // would be UB if maxItems stayed 0
        VERIFY_ARE_EQUAL(size_t{ 1 }, q.size());
        VERIFY_ARE_EQUAL(uint64_t{ 1 }, q.dropped_count());

        std::string out;
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "b" }, out, L"newest survives, oldest evicted");
    }

    // Items come out in the exact order they went in.
    void BoundedDispatchQueueTests::PreservesFifoOrder()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);

        VERIFY_IS_TRUE(q.try_push("a"));
        VERIFY_IS_TRUE(q.try_push("b"));
        VERIFY_IS_TRUE(q.try_push("c"));

        std::string out;
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "a" }, out);
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "b" }, out);
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "c" }, out);
        VERIFY_ARE_EQUAL(size_t{ 0 }, q.size());
    }

    // At capacity, the OLDEST item is evicted and counted; size never exceeds
    // the bound; the most-recent items survive.
    void BoundedDispatchQueueTests::DropsOldestWhenFullAndCountsDrops()
    {
        BoundedDispatchQueue<std::string> q{ 3 };
        q.set_active(true);

        for (int i = 1; i <= 5; ++i)
        {
            VERIFY_IS_TRUE(q.try_push(std::to_string(i)), L"push still succeeds when evicting to make room");
        }

        VERIFY_ARE_EQUAL(size_t{ 3 }, q.size(), L"size must never exceed the bound");
        VERIFY_ARE_EQUAL(uint64_t{ 2 }, q.dropped_count(), L"two oldest items were evicted");

        // 1 and 2 were dropped; 3,4,5 remain in order.
        std::string out;
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "3" }, out);
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "4" }, out);
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "5" }, out);
    }

    // Deactivating closes the gate for NEW pushes but — unlike stop() — keeps
    // any already-queued backlog, which remains drainable.
    void BoundedDispatchQueueTests::SetInactiveGatesPushButKeepsBacklog()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);
        VERIFY_IS_TRUE(q.try_push("a"));
        VERIFY_IS_TRUE(q.try_push("b"));

        q.set_active(false);
        VERIFY_IS_FALSE(q.try_push("c"), L"push rejected while inactive");
        VERIFY_ARE_EQUAL(size_t{ 2 }, q.size(), L"deactivate must not drop existing backlog");

        std::string out;
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "a" }, out);
    }

    // stop() drops the backlog and makes wait_pop return false (consumer exit).
    void BoundedDispatchQueueTests::StopDropsBacklogAndWaitPopReturnsFalse()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);
        VERIFY_IS_TRUE(q.try_push("a"));
        VERIFY_IS_TRUE(q.try_push("b"));
        VERIFY_ARE_EQUAL(size_t{ 2 }, q.size());

        q.stop();
        VERIFY_ARE_EQUAL(size_t{ 0 }, q.size(), L"stop must drop the backlog");

        std::string out;
        VERIFY_IS_FALSE(q.wait_pop(out), L"wait_pop must return false once stopped");
    }

    // A queue can be reused after stop(): re-activating clears the stop and
    // re-enables pushes. The backlog that stop() dropped stays gone. (Note:
    // set_active(true) does NOT reset dropped_count(); it isn't exercised here
    // because no overflow drop occurs in this test.)
    void BoundedDispatchQueueTests::ReactivateAfterStopAcceptsAgain()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);
        VERIFY_IS_TRUE(q.try_push("old"));

        q.stop();
        VERIFY_IS_FALSE(q.try_push("rejected"), L"pushes are rejected while stopped");

        q.set_active(true); // clears the prior stop
        VERIFY_IS_TRUE(q.try_push("new"));

        std::string out;
        VERIFY_IS_TRUE(q.wait_pop(out));
        VERIFY_ARE_EQUAL(std::string{ "new" }, out, L"old backlog was dropped by stop; only 'new' survives");
        VERIFY_ARE_EQUAL(size_t{ 0 }, q.size());
    }

    // A consumer blocked in wait_pop on an empty queue is released by stop()
    // and returns false. Timing-independent: even if stop() runs before the
    // consumer reaches wait_pop, the stopped predicate is already satisfied.
    void BoundedDispatchQueueTests::WaitPopUnblocksOnStop()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);

        std::atomic<bool> finished{ false };
        bool popResult = true;
        std::thread consumer([&]() {
            std::string out;
            popResult = q.wait_pop(out);
            finished.store(true);
        });

        std::this_thread::sleep_for(std::chrono::milliseconds(50));
        q.stop();
        consumer.join();

        VERIFY_IS_TRUE(finished.load());
        VERIFY_IS_FALSE(popResult, L"a stopped queue must release a blocked consumer with false");
    }

    // A consumer blocked in wait_pop on an empty queue is released by a push
    // and receives the item. Timing-independent for the same reason.
    void BoundedDispatchQueueTests::WaitPopUnblocksOnPush()
    {
        BoundedDispatchQueue<std::string> q{ 10 };
        q.set_active(true);

        std::string got;
        bool popResult = false;
        std::thread consumer([&]() {
            popResult = q.wait_pop(got);
        });

        std::this_thread::sleep_for(std::chrono::milliseconds(50));
        VERIFY_IS_TRUE(q.try_push("hello"));
        consumer.join();

        VERIFY_IS_TRUE(popResult);
        VERIFY_ARE_EQUAL(std::string{ "hello" }, got);
    }

    // The real integrity test: many producers push concurrently into a queue
    // sized so nothing is ever dropped. Every distinct value must survive
    // exactly once — no loss, no duplication, no corruption — and the dropped
    // counter stays at zero.
    void BoundedDispatchQueueTests::ConcurrentProducersLoseNothingWhenUnbounded()
    {
        constexpr int producers = 4;
        constexpr int perProducer = 1000;
        constexpr size_t total = static_cast<size_t>(producers) * perProducer;

        // Capacity strictly greater than total => no eviction can occur, so we
        // can assert exact set equality.
        BoundedDispatchQueue<std::string> q{ total + 1 };
        q.set_active(true);

        std::atomic<size_t> pushed{ 0 };
        std::vector<std::thread> threads;
        for (int p = 0; p < producers; ++p)
        {
            threads.emplace_back([&q, &pushed, p]() {
                for (int i = 0; i < perProducer; ++i)
                {
                    if (q.try_push(std::to_string(p * perProducer + i)))
                    {
                        pushed.fetch_add(1, std::memory_order_relaxed);
                    }
                }
            });
        }
        for (auto& t : threads)
        {
            t.join();
        }

        VERIFY_ARE_EQUAL(total, pushed.load(), L"every concurrent push must succeed");
        VERIFY_ARE_EQUAL(total, q.size());
        VERIFY_ARE_EQUAL(uint64_t{ 0 }, q.dropped_count(), L"an unbounded run must drop nothing");

        // Drain (all items already present, so no blocking) and confirm we
        // recover exactly the unique set 0..total-1.
        std::set<std::string> seen;
        for (size_t i = 0; i < total; ++i)
        {
            std::string out;
            VERIFY_IS_TRUE(q.wait_pop(out));
            seen.insert(out);
        }
        VERIFY_ARE_EQUAL(total, seen.size(), L"no value may be lost or duplicated under concurrent push");
        VERIFY_ARE_EQUAL(size_t{ 0 }, q.size());
    }
}
