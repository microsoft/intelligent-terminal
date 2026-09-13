// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <wil/resource.h>
#include <til/ticket_lock.h>
#include <mutex>

namespace IntelligentTerminal::Diagnostics
{
    // Independent of the connection's mutable PROCESS_INFORMATION. Capture must
    // receive an owned, stable handle before the output thread can close it.
    // The pin survives process exit/connection Close until replacement or owner
    // destruction, so acquiring a report handle never reopens a recycled PID.
    class DiagnosticProcessHandle
    {
    public:
        void Capture(HANDLE process) noexcept
        {
            wil::unique_handle copy;
            if (process)
                DuplicateHandle(GetCurrentProcess(), process, GetCurrentProcess(), copy.put(), PROCESS_QUERY_LIMITED_INFORMATION, FALSE, 0);
            const std::lock_guard guard{ _lock };
            _process = std::move(copy);
        }

        wil::unique_handle Duplicate() noexcept
        {
            const std::lock_guard guard{ _lock };
            wil::unique_handle copy;
            if (_process)
                DuplicateHandle(GetCurrentProcess(), _process.get(), GetCurrentProcess(), copy.put(), 0, FALSE, DUPLICATE_SAME_ACCESS);
            // Acquisition failure is reported by the diagnostic collector only;
            // it must not fail process launch or connection teardown.
            return copy;
        }

    private:
        til::ticket_lock _lock;
        wil::unique_handle _process;
    };
}
