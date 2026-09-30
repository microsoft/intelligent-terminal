// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"
#include "AgentPaneLifetime.h"
#include "AgentPaneLog.h"

using namespace winrt::Microsoft::Terminal::Control;
using namespace winrt::Microsoft::Terminal::TerminalConnection;

namespace winrt::TerminalApp::implementation
{
    namespace
    {
        constexpr auto AgentHelperExitTimeout{ std::chrono::seconds{ 3 } };

        safe_void_coroutine _EnsureAgentHelperExited(wil::unique_handle process, const DWORD pid)
        {
            co_await winrt::resume_background();

            const auto waitResult = WaitForSingleObject(process.get(), static_cast<DWORD>(std::chrono::duration_cast<std::chrono::milliseconds>(AgentHelperExitTimeout).count()));
            const auto waitError = waitResult == WAIT_FAILED ? GetLastError() : ERROR_SUCCESS;
            if (waitResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper exited after pane close pid=" + std::to_string(pid));
                co_return;
            }

            if (waitResult == WAIT_TIMEOUT)
            {
                _agentPaneLog("wta-helper did not exit after pane close; checking before termination pid=" + std::to_string(pid));
            }
            else if (waitResult == WAIT_FAILED)
            {
                LOG_WIN32_MSG(waitError, "Waiting for wta-helper after pane close failed (pid=%lu)", pid);
                _agentPaneLog("waiting for wta-helper after pane close failed error=" + std::to_string(waitError) + "; checking before termination pid=" + std::to_string(pid));
            }
            else
            {
                _agentPaneLog("waiting for wta-helper after pane close returned unexpected result=" + std::to_string(waitResult) + "; checking before termination pid=" + std::to_string(pid));
            }

            // The helper may have exited between the initial wait and forced cleanup.
            const auto preTerminateWaitResult = WaitForSingleObject(process.get(), 0);
            const auto preTerminateWaitError = preTerminateWaitResult == WAIT_FAILED ? GetLastError() : ERROR_SUCCESS;
            if (preTerminateWaitResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper exited before forced termination pid=" + std::to_string(pid));
                co_return;
            }
            if (preTerminateWaitResult == WAIT_FAILED)
            {
                LOG_WIN32_MSG(preTerminateWaitError, "Rechecking wta-helper before forced termination failed (pid=%lu)", pid);
                _agentPaneLog("rechecking wta-helper before forced termination failed error=" + std::to_string(preTerminateWaitError) + " pid=" + std::to_string(pid));

                DWORD exitCode = STILL_ACTIVE;
                if (GetExitCodeProcess(process.get(), &exitCode))
                {
                    if (exitCode != STILL_ACTIVE)
                    {
                        _agentPaneLog("wta-helper had already exited before forced termination pid=" + std::to_string(pid));
                        co_return;
                    }
                }
                else
                {
                    const auto exitCodeError = GetLastError();
                    LOG_WIN32_MSG(exitCodeError, "Querying wta-helper exit code before forced termination failed (pid=%lu)", pid);
                    _agentPaneLog("querying wta-helper exit code before forced termination failed error=" + std::to_string(exitCodeError) + " pid=" + std::to_string(pid));
                }
            }

            _agentPaneLog("terminating wta-helper after pane close pid=" + std::to_string(pid));
            if (!TerminateProcess(process.get(), 1))
            {
                const auto terminateError = GetLastError();
                LOG_WIN32_MSG(terminateError, "Terminating wta-helper after pane close failed (pid=%lu)", pid);
                _agentPaneLog("terminating wta-helper after pane close failed error=" + std::to_string(terminateError) + " pid=" + std::to_string(pid));
            }

            const auto reapResult = WaitForSingleObject(process.get(), 5000);
            if (reapResult == WAIT_OBJECT_0)
            {
                _agentPaneLog("wta-helper reaped after forced termination pid=" + std::to_string(pid));
            }
            else if (reapResult == WAIT_TIMEOUT)
            {
                _agentPaneLog("timed out waiting to reap wta-helper after forced termination pid=" + std::to_string(pid));
            }
            else if (reapResult == WAIT_FAILED)
            {
                const auto reapError = GetLastError();
                LOG_WIN32_MSG(reapError, "Waiting to reap wta-helper after forced termination failed (pid=%lu)", pid);
                _agentPaneLog("waiting to reap wta-helper after forced termination failed error=" + std::to_string(reapError) + " pid=" + std::to_string(pid));
            }
            else
            {
                _agentPaneLog("waiting to reap wta-helper after forced termination returned unexpected result=" + std::to_string(reapResult) + " pid=" + std::to_string(pid));
            }
        }

        wil::unique_handle _DuplicateAgentHelperProcess(const ControlInteractivity& content)
        {
            const auto connection = content ? content.Core().Connection() : nullptr;
            const auto conpty = connection.try_as<ConptyConnection>();
            const auto processValue = conpty ? conpty.RootProcessHandle() : 0;
            if (!processValue)
            {
                return {};
            }

            wil::unique_handle duplicate;
            LOG_IF_WIN32_BOOL_FALSE(DuplicateHandle(
                GetCurrentProcess(),
                reinterpret_cast<HANDLE>(processValue),
                GetCurrentProcess(),
                duplicate.addressof(),
                SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                FALSE,
                0));
            return duplicate;
        }
    }

    AgentPaneLifetime::AgentPaneLifetime(SharedWtaLease lease, ControlInteractivity content) noexcept :
        _lease{ std::move(lease) },
        _content{ std::move(content) }
    {
    }

    AgentPaneLifetime::AgentPaneLifetime(AgentPaneLifetime&& other) noexcept :
        _lease{ std::move(other._lease) },
        _content{ std::exchange(other._content, nullptr) },
        _helperProcess{ std::move(other._helperProcess) }
    {
    }

    AgentPaneLifetime& AgentPaneLifetime::operator=(AgentPaneLifetime&& other) noexcept
    {
        if (this != &other)
        {
            _Abandon();
            _lease = std::move(other._lease);
            _content = std::exchange(other._content, nullptr);
            _helperProcess = std::move(other._helperProcess);
        }
        return *this;
    }

    AgentPaneLifetime::~AgentPaneLifetime()
    {
        _Abandon();
    }

    void AgentPaneLifetime::CaptureHelperProcess() noexcept
    {
        try
        {
            if (auto process = _DuplicateAgentHelperProcess(_content))
            {
                _helperProcess = std::move(process);
            }
        }
        CATCH_LOG()
    }

    void AgentPaneLifetime::Close() noexcept
    {
        CaptureHelperProcess();
        auto content = std::exchange(_content, nullptr);
        auto process = std::move(_helperProcess);
        _lease.Retire();
        if (!content && !process)
        {
            return;
        }
        auto enforceExit = wil::scope_exit([&]() noexcept {
            if (process)
            {
                const auto pid = GetProcessId(process.get());
                try
                {
                    _EnsureAgentHelperExited(std::move(process), pid);
                }
                CATCH_LOG()
            }
        });
        try
        {
            if (content)
            {
                content.Close();
            }
        }
        CATCH_LOG()
    }

    void AgentPaneLifetime::RetireClosedContent() noexcept
    {
        // The owning TermControl has already closed this core.
        _content = nullptr;
        Close();
    }

    void AgentPaneLifetime::_Abandon() noexcept
    {
        CaptureHelperProcess();
        _lease.Retire();
        auto content = std::exchange(_content, nullptr);
        auto process = std::move(_helperProcess);
        if (content || process)
        {
            try
            {
                _CloseDetached(std::move(content), std::move(process));
            }
            CATCH_LOG()
        }
    }

    winrt::fire_and_forget AgentPaneLifetime::_CloseDetached(ControlInteractivity content, wil::unique_handle process)
    {
        AgentPaneLifetime detached{ {}, std::move(content) };
        detached._helperProcess = std::move(process);
        try
        {
            co_await winrt::resume_background();
        }
        CATCH_LOG()
        detached.Close();
    }
}
