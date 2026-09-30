// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <atomic>
#include <chrono>
#include <functional>
#include <mutex>
#include <unordered_map>
#include <winrt/base.h>

namespace winrt::TerminalApp::implementation
{
    struct TerminalPage;

    // Pending requests own no terminal content. Only the receiver's UI-thread
    // claim may suspend the source, and it must commit or roll back synchronously.
    class ContentTransfer
    {
    public:
        using Attach = std::function<bool(TerminalPage&, uint32_t)>;

        static uint64_t Register(Attach attach)
        {
            const auto id = ++_nextId;
            THROW_HR_IF(E_UNEXPECTED, id == 0);
            auto& store = _store();
            std::lock_guard lock{ store.mutex };
            store.requests.emplace(id, Request{ GetCurrentThreadId(), std::chrono::steady_clock::now() + std::chrono::minutes{ 2 }, std::move(attach) });
            return id;
        }

        static bool Receive(uint64_t id, TerminalPage& target, uint32_t tabIndex)
        {
            Attach attach;
            {
                auto& store = _store();
                std::lock_guard lock{ store.mutex };
                const auto it = store.requests.find(id);
                if (it == store.requests.end())
                {
                    return false;
                }
                THROW_HR_IF(RPC_E_WRONG_THREAD, it->second.threadId != GetCurrentThreadId());
                if (std::chrono::steady_clock::now() < it->second.deadline)
                {
                    attach = std::move(it->second.attach);
                }
                store.requests.erase(it);
            }
            return attach && attach(target, tabIndex);
        }

        static void Expire(uint64_t id)
        {
            auto& store = _store();
            std::lock_guard lock{ store.mutex };
            store.requests.erase(id);
        }

        static winrt::fire_and_forget ExpireAfterTimeout(uint64_t id)
        {
            try
            {
                co_await winrt::resume_after(std::chrono::minutes{ 2 });
            }
            CATCH_LOG()
            Expire(id);
        }

    private:
        struct Request
        {
            DWORD threadId;
            std::chrono::steady_clock::time_point deadline;
            Attach attach;
        };
        struct Store
        {
            std::mutex mutex;
            std::unordered_map<uint64_t, Request> requests;
        };
        static Store& _store()
        {
            // Expiry callbacks may outlive the last window.
            static auto* const store = new Store;
            return *store;
        }
        inline static std::atomic<uint64_t> _nextId{ 0 };
    };
}
