// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxProcess.h
//
// Abstract:
// - Owned, cancellable process stdio transport, independent of backend command syntax.

#pragma once

#include <cstddef>
#include <cstdint>
#include <exception>
#include <functional>
#include <memory>
#include <string>
#include <string_view>

namespace Microsoft::Terminal::Tmux
{
    class TmuxProcess final
    {
    public:
        struct Callbacks
        {
            // Serialized on the transport worker; views last only for the call.
            std::function<void(std::string_view)> output;
            std::function<void(std::string_view)> error;
            // Exactly once after the final output/error callbacks and reaping
            // the owned process. Parser::Finish is safe here: no reads or output
            // callbacks can follow. A normal exit drains buffered data from both
            // pipes. Close cancels reads; inherited idle writers are canceled
            // after the owned process exits rather than waiting for OS pipe EOF.
            std::function<void(uint32_t)> exited;
            // Optional asynchronous failure reporting. Failures are also retained
            // for RethrowFailure, including exceptions thrown by any callback.
            // Without this callback, inspect RethrowFailure after Close or exit.
            std::function<void(std::exception_ptr)> failed;
        };

        static constexpr size_t MaxQueuedBytes = 4 * 1024 * 1024;

        // Construction creates no process or worker. Publish the owner before
        // calling Start so callbacks cannot race publication of this transport.
        explicit TmuxProcess(Callbacks callbacks);
        ~TmuxProcess() noexcept;
        TmuxProcess(const TmuxProcess&) = delete;
        TmuxProcess& operator=(const TmuxProcess&) = delete;
        TmuxProcess(TmuxProcess&&) = delete;
        TmuxProcess& operator=(TmuxProcess&&) = delete;

        // One-shot synchronous launch: call off the UI thread. Pass an explicit
        // directory and an opaque CreateProcessW command line, not shell syntax.
        // Callbacks may start before Start returns. Close can safely race Start;
        // closing before Start prevents launch altogether.
        void Start(std::wstring commandLine, std::wstring workingDirectory);

        // Thread-safe, nonblocking enqueue. Throws on queue overflow, transport
        // failure or a transport that is not running; no bytes are silently discarded.
        void Write(std::string data);

        // Idempotent; call off the UI thread. Closes stdin, cancels I/O, then
        // terminates/reaps only this process if it does not exit within 500 ms.
        // On the callback thread this requests shutdown without waiting for self;
        // worker-owned state remains alive until shutdown completes.
        void Close() noexcept;
        void RethrowFailure() const;

    private:
        struct State;
        std::shared_ptr<State> _state;
    };
}
