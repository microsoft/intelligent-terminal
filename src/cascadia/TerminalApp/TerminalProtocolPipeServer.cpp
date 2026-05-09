// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "TerminalProtocolPipeServer.h"

#include <json/json.h>

#include <cstdint>
#include <sstream>

namespace TerminalProtocol
{
    namespace
    {
        constexpr uint32_t kMaxFrameBytes = 64u * 1024u; // 64 KiB

        // Read exactly `n` bytes from `h`. Returns false on EOF or error.
        bool _readExact(HANDLE h, void* buf, DWORD n) noexcept
        {
            auto p = static_cast<uint8_t*>(buf);
            DWORD remaining = n;
            while (remaining > 0)
            {
                DWORD got = 0;
                if (!ReadFile(h, p, remaining, &got, nullptr))
                {
                    return false;
                }
                if (got == 0)
                {
                    return false; // peer closed
                }
                p += got;
                remaining -= got;
            }
            return true;
        }

        bool _writeExact(HANDLE h, const void* buf, DWORD n) noexcept
        {
            auto p = static_cast<const uint8_t*>(buf);
            DWORD remaining = n;
            while (remaining > 0)
            {
                DWORD wrote = 0;
                if (!WriteFile(h, p, remaining, &wrote, nullptr))
                {
                    return false;
                }
                if (wrote == 0)
                {
                    return false;
                }
                p += wrote;
                remaining -= wrote;
            }
            return true;
        }

        bool _parseJson(const std::string& s, Json::Value& out) noexcept
        try
        {
            Json::CharReaderBuilder rb;
            std::istringstream ss{ s };
            std::string errs;
            return Json::parseFromStream(rb, ss, &out, &errs);
        }
        catch (...)
        {
            return false;
        }

        std::string _serializeJson(const Json::Value& v)
        {
            Json::StreamWriterBuilder wb;
            wb["indentation"] = "";
            return Json::writeString(wb, v);
        }

        Json::Value _makeError(const Json::Value& id, int code, const std::string& msg)
        {
            Json::Value resp;
            resp["jsonrpc"] = "2.0";
            resp["id"] = id;
            Json::Value err;
            err["code"] = code;
            err["message"] = msg;
            resp["error"] = err;
            return resp;
        }

        Json::Value _makeResult(const Json::Value& id, Json::Value&& result)
        {
            Json::Value resp;
            resp["jsonrpc"] = "2.0";
            resp["id"] = id;
            resp["result"] = std::move(result);
            return resp;
        }

        std::wstring _utf8ToWide(const std::string& s)
        {
            if (s.empty())
            {
                return {};
            }
            const int wlen = MultiByteToWideChar(CP_UTF8, 0, s.data(),
                                                  static_cast<int>(s.size()),
                                                  nullptr, 0);
            std::wstring w;
            w.resize(wlen);
            MultiByteToWideChar(CP_UTF8, 0, s.data(),
                                static_cast<int>(s.size()),
                                w.data(), wlen);
            return w;
        }
    }

    PipeServer::PipeServer(wil::unique_handle readEnd,
                            wil::unique_handle writeEnd,
                            SendInputHandler sendInput) :
        _readEnd{ std::move(readEnd) },
        _writeEnd{ std::move(writeEnd) },
        _sendInput{ std::move(sendInput) }
    {
    }

    PipeServer::~PipeServer()
    {
        Stop();
    }

    void PipeServer::SetOnShutdown(std::function<void()> cb)
    {
        _onShutdown = std::move(cb);
    }

    void PipeServer::Start()
    {
        bool expected = false;
        if (!_started.compare_exchange_strong(expected, true))
        {
            return;
        }
        _thread = std::thread{ [this]() { _ioThreadProc(); } };
    }

    void PipeServer::Stop()
    {
        const bool wasShuttingDown = _shutdown.exchange(true);
        if (wasShuttingDown)
        {
            if (_thread.joinable() && _thread.get_id() != std::this_thread::get_id())
            {
                _thread.join();
            }
            return;
        }

        // Closing the handles unblocks any blocking ReadFile/WriteFile in
        // the IO thread (returns FALSE / ERROR_BROKEN_PIPE).
        _readEnd.reset();
        _writeEnd.reset();

        if (_thread.joinable() && _thread.get_id() != std::this_thread::get_id())
        {
            _thread.join();
        }
    }

    bool PipeServer::_readFrame(std::string& out) noexcept
    {
        if (!_readEnd)
        {
            return false;
        }
        uint32_t lenLE{ 0 };
        if (!_readExact(_readEnd.get(), &lenLE, sizeof(lenLE)))
        {
            return false;
        }
        const uint32_t len = lenLE; // little-endian on Windows x64/ARM64 by definition
        if (len == 0 || len > kMaxFrameBytes)
        {
            return false;
        }
        out.resize(len);
        return _readExact(_readEnd.get(), out.data(), len);
    }

    bool PipeServer::_writeFrame(std::string_view body) noexcept
    {
        if (!_writeEnd)
        {
            return false;
        }
        if (body.size() > kMaxFrameBytes)
        {
            return false;
        }
        const uint32_t lenLE = static_cast<uint32_t>(body.size());
        if (!_writeExact(_writeEnd.get(), &lenLE, sizeof(lenLE)))
        {
            return false;
        }
        return _writeExact(_writeEnd.get(), body.data(),
                           static_cast<DWORD>(body.size()));
    }

    std::string PipeServer::_dispatch(const std::string& requestJson)
    {
        Json::Value req;
        if (!_parseJson(requestJson, req))
        {
            return _serializeJson(_makeError(Json::nullValue, -32700, "Parse error"));
        }
        const auto id = req.isMember("id") ? req["id"] : Json::Value{ Json::nullValue };
        if (!req.isMember("method") || !req["method"].isString())
        {
            return _serializeJson(_makeError(id, -32600, "Invalid Request: missing method"));
        }
        const auto method = req["method"].asString();
        const auto& params = req.isMember("params") ? req["params"] : Json::Value{ Json::nullValue };

        try
        {
            if (method == "hello")
            {
                Json::Value result;
                result["server"] = "wt";
                result["version"] = "1";
                Json::Value caps{ Json::arrayValue };
                caps.append("send_input");
                result["capabilities"] = caps;
                return _serializeJson(_makeResult(id, std::move(result)));
            }

            if (method == "send_input")
            {
                // session_id is required and must be a string-formatted GUID
                // (the WT_SESSION env value of the target pane). Pane ids are
                // GUIDs end-to-end since the PaneId→SessionId migration.
                winrt::guid sessionId{};
                if (!params.isMember("session_id") || !params["session_id"].isString())
                {
                    return _serializeJson(_makeError(id, -32602, "Invalid params: session_id"));
                }
                try
                {
                    sessionId = winrt::guid{ params["session_id"].asString() };
                }
                catch (...)
                {
                    return _serializeJson(_makeError(id, -32602, "Invalid params: session_id"));
                }
                if (sessionId == winrt::guid{})
                {
                    return _serializeJson(_makeError(id, -32602, "Invalid params: session_id"));
                }
                if (!params.isMember("text") || !params["text"].isString())
                {
                    return _serializeJson(_makeError(id, -32602, "Invalid params: text"));
                }
                const auto text = params["text"].asString();
                if (!_sendInput)
                {
                    return _serializeJson(_makeError(id, -32603, "send_input not wired"));
                }
                const auto wide = _utf8ToWide(text);
                const bool ok = _sendInput(sessionId, wide);
                Json::Value r;
                r["ok"] = ok;
                return _serializeJson(_makeResult(id, std::move(r)));
            }

            return _serializeJson(_makeError(id, -32601, "Method not found"));
        }
        catch (const std::exception& e)
        {
            return _serializeJson(_makeError(id, -32603, e.what()));
        }
        catch (...)
        {
            return _serializeJson(_makeError(id, -32603, "Internal error"));
        }
    }

    void PipeServer::_ioThreadProc() noexcept
    {
        // Initialize MTA so it's safe to invoke WinRT projections from this
        // thread (the SendInput handler calls TerminalPage::SendProtocolInput,
        // which marshals to the UI dispatcher and blocks via .get()).
        const auto coInit = wil::CoInitializeEx(COINIT_MULTITHREADED);

        try
        {
            std::string frame;
            while (!_shutdown.load(std::memory_order_relaxed))
            {
                frame.clear();
                if (!_readFrame(frame))
                {
                    break;
                }
                const auto resp = _dispatch(frame);
                if (!_writeFrame(resp))
                {
                    break;
                }
            }
        }
        catch (...)
        {
            // swallow — IO thread must not throw
        }

        if (_onShutdown)
        {
            try
            {
                _onShutdown();
            }
            catch (...)
            {
            }
        }
    }
}
