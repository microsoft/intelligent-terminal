// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#pragma once

#include <json/json.h>
#include <mutex>

class ProtocolRequestHandler;

// Represents a single client connection to the protocol server.
// Each connection has its own read/write state and authentication status.
struct ProtocolClientConnection
{
    wil::unique_hfile pipe;
    OVERLAPPED readOverlapped{};
    OVERLAPPED writeOverlapped{};
    wil::unique_event readEvent;
    wil::unique_event writeEvent;
    std::string readBuffer;
    std::string lineBuffer;
    bool authenticated = false;
    DWORD clientPid = 0;

    ProtocolClientConnection();
};

// The TerminalProtocolServer exposes a JSON-based protocol over a Windows named pipe.
// It is owned by WindowEmperor and provides the transport layer for AI CLI tool integration.
//
// Architecture:
//   - A single named pipe (\\.\pipe\WindowsTerminal-<instance-id>) handles all connections.
//   - Each client connection runs its I/O on a background thread.
//   - Protocol requests are parsed and forwarded to ProtocolRequestHandler.
//   - The handler dispatches work to the appropriate UI thread as needed.
class TerminalProtocolServer
{
public:
    TerminalProtocolServer(std::wstring pipeName, std::string authToken, ProtocolRequestHandler& handler);
    ~TerminalProtocolServer();

    // Start accepting connections. Non-blocking; spawns a listener thread.
    void Start();

    // Stop the server and close all connections.
    void Stop();

    // Returns the pipe name for this server instance.
    const std::wstring& PipeName() const noexcept { return _pipeName; }

private:
    void _listenerThread();
    void _clientThread(std::shared_ptr<ProtocolClientConnection> client);
    void _processLine(ProtocolClientConnection& client, const std::string& line);
    void _sendResponse(ProtocolClientConnection& client, const Json::Value& response);
    bool _validateClientProcess(DWORD pid) const;

    std::wstring _pipeName;
    std::string _authToken;
    ProtocolRequestHandler& _handler;

    wil::unique_event _stopEvent;
    std::thread _listenerThread_;
    std::vector<std::thread> _clientThreads;
    std::mutex _clientLock;
    bool _running = false;
};
