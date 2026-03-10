// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "TerminalProtocolServer.h"
#include "ProtocolRequestHandler.h"

#include <sddl.h>

ProtocolClientConnection::ProtocolClientConnection()
{
    readEvent.create(wil::EventOptions::ManualReset);
    writeEvent.create(wil::EventOptions::ManualReset);
    readOverlapped.hEvent = readEvent.get();
    writeOverlapped.hEvent = writeEvent.get();
    readBuffer.resize(4096);
}

TerminalProtocolServer::TerminalProtocolServer(std::wstring pipeName, std::string authToken, ProtocolRequestHandler& handler) :
    _pipeName(std::move(pipeName)),
    _authToken(std::move(authToken)),
    _handler(handler)
{
    _stopEvent.create(wil::EventOptions::ManualReset);
}

TerminalProtocolServer::~TerminalProtocolServer()
{
    Stop();
}

void TerminalProtocolServer::Start()
{
    if (_running)
    {
        return;
    }
    _running = true;
    _stopEvent.ResetEvent();
    _listenerThread_ = std::thread([this]() { _listenerThread(); });
}

void TerminalProtocolServer::Stop()
{
    if (!_running)
    {
        return;
    }
    _running = false;
    _stopEvent.SetEvent();

    if (_listenerThread_.joinable())
    {
        _listenerThread_.join();
    }

    std::lock_guard lock{ _clientLock };
    for (auto& t : _clientThreads)
    {
        if (t.joinable())
        {
            t.join();
        }
    }
    _clientThreads.clear();
}

void TerminalProtocolServer::_listenerThread()
{
    while (_running)
    {
        // Create a new pipe instance for the next client.
        // Using nullptr for security attributes applies the default DACL from
        // the process token, which grants access to the current user and SYSTEM.
        wil::unique_hfile pipe{
            CreateNamedPipeW(
                _pipeName.c_str(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                4096, // output buffer size
                4096, // input buffer size
                0, // default timeout
                nullptr)
        };

        if (!pipe)
        {
            LOG_LAST_ERROR();
            break;
        }

        // Wait for a client to connect, or for the stop event.
        OVERLAPPED connectOverlapped{};
        wil::unique_event connectEvent(wil::EventOptions::ManualReset);
        connectOverlapped.hEvent = connectEvent.get();

        const auto connectResult = ConnectNamedPipe(pipe.get(), &connectOverlapped);
        if (!connectResult)
        {
            const auto err = GetLastError();
            if (err == ERROR_IO_PENDING)
            {
                // Wait for either the connection or the stop event.
                HANDLE waitHandles[] = { connectEvent.get(), _stopEvent.get() };
                const auto waitResult = WaitForMultipleObjects(2, waitHandles, FALSE, INFINITE);

                if (waitResult == WAIT_OBJECT_0 + 1)
                {
                    // Stop event signaled - cancel the pending I/O and exit.
                    CancelIoEx(pipe.get(), &connectOverlapped);
                    break;
                }
                else if (waitResult != WAIT_OBJECT_0)
                {
                    LOG_LAST_ERROR();
                    break;
                }

                // Check the overlapped result.
                DWORD bytesTransferred = 0;
                if (!GetOverlappedResult(pipe.get(), &connectOverlapped, &bytesTransferred, FALSE))
                {
                    LOG_LAST_ERROR();
                    continue;
                }
            }
            else if (err == ERROR_PIPE_CONNECTED)
            {
                // Client already connected before ConnectNamedPipe was called.
            }
            else
            {
                LOG_WIN32(err);
                continue;
            }
        }

        // Validate client process identity.
        DWORD clientPid = 0;
        if (!GetNamedPipeClientProcessId(pipe.get(), &clientPid))
        {
            LOG_LAST_ERROR();
            DisconnectNamedPipe(pipe.get());
            continue;
        }

        // For now, we accept connections from any local process. Process identity
        // verification (allowlist check) will be tightened in Phase 4. The token-based
        // auth is the primary security mechanism during initial development.

        auto client = std::make_shared<ProtocolClientConnection>();
        client->pipe = std::move(pipe);
        client->clientPid = clientPid;

        std::lock_guard lock{ _clientLock };
        _clientThreads.emplace_back([this, client]() { _clientThread(client); });
    }
}

void TerminalProtocolServer::_clientThread(std::shared_ptr<ProtocolClientConnection> client)
{
    // Read loop: accumulate bytes until we see a newline, then process the line.
    while (_running)
    {
        DWORD bytesRead = 0;
        const auto readResult = ReadFile(
            client->pipe.get(),
            client->readBuffer.data(),
            static_cast<DWORD>(client->readBuffer.size()),
            nullptr,
            &client->readOverlapped);

        if (!readResult)
        {
            const auto err = GetLastError();
            if (err == ERROR_IO_PENDING)
            {
                HANDLE waitHandles[] = { client->readEvent.get(), _stopEvent.get() };
                const auto waitResult = WaitForMultipleObjects(2, waitHandles, FALSE, INFINITE);

                if (waitResult == WAIT_OBJECT_0 + 1)
                {
                    // Stop event
                    CancelIoEx(client->pipe.get(), &client->readOverlapped);
                    break;
                }
                else if (waitResult != WAIT_OBJECT_0)
                {
                    break;
                }

                if (!GetOverlappedResult(client->pipe.get(), &client->readOverlapped, &bytesRead, FALSE))
                {
                    break; // Client disconnected or error
                }
            }
            else if (err == ERROR_BROKEN_PIPE || err == ERROR_NO_DATA)
            {
                break; // Client disconnected
            }
            else
            {
                LOG_WIN32(err);
                break;
            }
        }
        else
        {
            bytesRead = static_cast<DWORD>(client->readOverlapped.InternalHigh);
        }

        client->readEvent.ResetEvent();

        if (bytesRead == 0)
        {
            break; // EOF
        }

        // Append to line buffer and process complete lines.
        client->lineBuffer.append(client->readBuffer.data(), bytesRead);

        size_t pos = 0;
        while ((pos = client->lineBuffer.find('\n')) != std::string::npos)
        {
            auto line = client->lineBuffer.substr(0, pos);
            client->lineBuffer.erase(0, pos + 1);

            // Strip trailing \r if present.
            if (!line.empty() && line.back() == '\r')
            {
                line.pop_back();
            }

            if (!line.empty())
            {
                _processLine(*client, line);
            }
        }
    }

    // Disconnect and clean up.
    DisconnectNamedPipe(client->pipe.get());
}

void TerminalProtocolServer::_processLine(ProtocolClientConnection& client, const std::string& line)
{
    Json::Value request;
    Json::CharReaderBuilder readerBuilder;
    std::string parseErrors;
    std::istringstream stream(line);

    if (!Json::parseFromStream(readerBuilder, stream, &request, &parseErrors))
    {
        // Send parse error response.
        Json::Value response;
        response["type"] = "response";
        response["id"] = Json::nullValue;
        response["result"] = Json::nullValue;
        Json::Value error;
        error["code"] = "invalid_params";
        error["message"] = "Failed to parse JSON: " + parseErrors;
        response["error"] = error;
        _sendResponse(client, response);
        return;
    }

    // Dispatch to the handler.
    auto response = _handler.HandleRequest(request, client.authenticated);
    _sendResponse(client, response);
}

void TerminalProtocolServer::_sendResponse(ProtocolClientConnection& client, const Json::Value& response)
{
    Json::StreamWriterBuilder writerBuilder;
    writerBuilder["indentation"] = ""; // Compact output (no pretty-print)
    const auto responseStr = Json::writeString(writerBuilder, response) + "\n";

    DWORD bytesWritten = 0;
    client.writeEvent.ResetEvent();

    const auto writeResult = WriteFile(
        client.pipe.get(),
        responseStr.data(),
        static_cast<DWORD>(responseStr.size()),
        nullptr,
        &client.writeOverlapped);

    if (!writeResult)
    {
        const auto err = GetLastError();
        if (err == ERROR_IO_PENDING)
        {
            // Wait for write to complete (with a reasonable timeout).
            const auto waitResult = WaitForSingleObject(client.writeEvent.get(), 5000);
            if (waitResult == WAIT_OBJECT_0)
            {
                GetOverlappedResult(client.pipe.get(), &client.writeOverlapped, &bytesWritten, FALSE);
            }
        }
        else
        {
            LOG_WIN32(err);
        }
    }
}

bool TerminalProtocolServer::_validateClientProcess(DWORD pid) const
{
    // Phase 1: Accept all connections. Process allowlist checking will be
    // implemented in Phase 4 (per-action confirmation). The token-based
    // authentication is the primary security mechanism.
    UNREFERENCED_PARAMETER(pid);
    return true;
}
