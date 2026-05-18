// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// TerminalProtocolPipeServer
// ==========================
//
// Per-wta IO thread that owns the WT-side ends of a duplex anonymous pipe pair
// (created by WtaProcessLauncher) and dispatches JSON-RPC 2.0 requests from
// wta into method handlers.
//
// Wire format: 4-byte little-endian body length, then UTF-8 JSON body.
//
// Initial method registry: "send_input" only. The dispatcher is a string→
// std::function map so adding the next method is one new entry.
//
// Lifetime: one pipe server per launched wta. Owned by the launching
// TerminalPage. Stop() closes the handles to unblock the blocking ReadFile
// in the IO thread, then joins.

#pragma once

#include <atomic>
#include <functional>
#include <string>
#include <thread>
#include <unordered_map>
#include <wil/resource.h>
#include <winrt/Windows.Foundation.h>

namespace winrt::Microsoft::Terminal::Protocol
{
}

namespace TerminalProtocol
{
    // Handler for "send_input" — returns true on success, false on
    // pane-not-found / read-only / etc. Throws on protocol error (caller
    // converts to JSON-RPC error envelope).
    using SendInputHandler = std::function<bool(winrt::guid sessionId, std::wstring_view text)>;

    class PipeServer
    {
    public:
        PipeServer(wil::unique_handle readEnd,
                   wil::unique_handle writeEnd,
                   SendInputHandler sendInput);
        ~PipeServer();

        PipeServer(const PipeServer&) = delete;
        PipeServer& operator=(const PipeServer&) = delete;

        // Spawns the IO thread. Idempotent.
        void Start();

        // Closes handles (unblocks blocking IO) and joins the thread. Idempotent.
        void Stop();

        // Optional: callback invoked from the IO thread when it exits for any
        // reason (peer closed, error, Stop). Called at most once.
        void SetOnShutdown(std::function<void()> cb);

    private:
        void _ioThreadProc() noexcept;
        bool _readFrame(std::string& out) noexcept;
        bool _writeFrame(std::string_view body) noexcept;
        std::string _dispatch(const std::string& requestJson);

        wil::unique_handle _readEnd;
        wil::unique_handle _writeEnd;
        SendInputHandler _sendInput;
        std::function<void()> _onShutdown;

        std::thread _thread;
        std::atomic<bool> _shutdown{ false };
        std::atomic<bool> _started{ false };
    };
}
