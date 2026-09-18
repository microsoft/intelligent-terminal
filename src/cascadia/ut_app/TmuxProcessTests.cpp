// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxProcessTests.cpp
//
// Abstract:
// - Owned-process stdio, cancellation, callback-failure and lifetime tests.

#include "precomp.h"

#include "../TerminalApp/TmuxProcess.h"

#include <chrono>
#include <condition_variable>
#include <mutex>
#include <stdexcept>
#include <thread>

using namespace WEX::TestExecution;
using namespace Microsoft::Terminal::Tmux;
using namespace std::chrono_literals;

namespace TerminalAppUnitTests
{
    namespace
    {
        std::wstring SystemDirectory()
        {
            std::wstring path(32768, L'\0');
            const auto length = GetSystemDirectoryW(path.data(), static_cast<UINT>(path.size()));
            THROW_LAST_ERROR_IF(length == 0 || length >= path.size());
            path.resize(length);
            return path;
        }

        std::wstring Cmd(const std::wstring_view script)
        {
            // The shell is explicitly part of this test's opaque backend command.
            // TmuxProcess itself must never insert one.
            return L"\"" + SystemDirectory() + L"\\cmd.exe\" /d /v:on /s /c \"" + std::wstring{ script } + L"\"";
        }

        struct ProcessResult
        {
            TmuxProcess::Callbacks Callbacks()
            {
                return {
                    [this](const std::string_view bytes) {
                        std::lock_guard lock{ mutex };
                        output.append(bytes);
                    },
                    [this](const std::string_view bytes) {
                        std::lock_guard lock{ mutex };
                        error.append(bytes);
                    },
                    [this](const uint32_t code) {
                        {
                            std::lock_guard lock{ mutex };
                            exitCode = code;
                            ++exits;
                        }
                        ready.notify_all();
                    },
                    [this](const std::exception_ptr& exception) {
                        std::lock_guard lock{ mutex };
                        failure = exception;
                    }
                };
            }

            bool Wait()
            {
                std::unique_lock lock{ mutex };
                return ready.wait_for(lock, 10s, [&]() { return exits != 0; });
            }

            std::mutex mutex;
            std::condition_variable ready;
            std::string output;
            std::string error;
            std::exception_ptr failure;
            uint32_t exitCode{};
            size_t exits{};
        };
    }

    class TmuxProcessTests
    {
        TEST_CLASS(TmuxProcessTests);

        TEST_METHOD(SeparatesStreamsAndReportsExitAfterOutput);
        TEST_METHOD(UsesExplicitWorkingDirectory);
        TEST_METHOD(EnqueuesInputInOrder);
        TEST_METHOD(BoundsAndCancelsStalledWrites);
        TEST_METHOD(SurfacesCallbackFailures);
        TEST_METHOD(AllowsDestructionFromOutputCallback);
        TEST_METHOD(RejectsInvalidLaunchArguments);
        TEST_METHOD(RejectsWritesBeforeStartAndRepeatedStart);
        TEST_METHOD(CloseBeforeStartPreventsLaunch);
        TEST_METHOD(CloseCanRaceStart);
    };

    void TmuxProcessTests::SeparatesStreamsAndReportsExitAfterOutput()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        process.Start(Cmd(L"echo stdout-sentinel&echo stderr-sentinel 1>&2&exit /b 23"), SystemDirectory());
        VERIFY_IS_TRUE(result.Wait());
        process.Close();
        process.Close();
        process.RethrowFailure();
        VERIFY_ARE_EQUAL(uint32_t{ 23 }, result.exitCode);
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.exits);
        VERIFY_IS_TRUE(result.output.find("stdout-sentinel") != std::string::npos);
        VERIFY_IS_TRUE(result.output.find("stderr-sentinel") == std::string::npos);
        VERIFY_IS_TRUE(result.error.find("stderr-sentinel") != std::string::npos);
        VERIFY_IS_TRUE(result.error.find("stdout-sentinel") == std::string::npos);
        VERIFY_THROWS(process.Write("too late"), std::runtime_error);
    }

    void TmuxProcessTests::UsesExplicitWorkingDirectory()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        process.Start(Cmd(L"if exist cmd.exe (echo cwd-ok) else (echo cwd-bad)"), SystemDirectory());
        VERIFY_IS_TRUE(result.Wait());
        process.Close();
        process.RethrowFailure();
        VERIFY_IS_TRUE(result.output.find("cwd-ok") != std::string::npos);
        VERIFY_IS_TRUE(result.output.find("cwd-bad") == std::string::npos);
    }

    void TmuxProcessTests::EnqueuesInputInOrder()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        process.Start(Cmd(L"set /p value=&echo !value!"), SystemDirectory());
        process.Write("first-");
        process.Write("second\r\n");
        VERIFY_IS_TRUE(result.Wait());
        process.Close();
        process.RethrowFailure();
        VERIFY_IS_TRUE(result.output.find("first-second") != std::string::npos);
    }

    void TmuxProcessTests::BoundsAndCancelsStalledWrites()
    {
        ProcessResult result;
        const auto command = L"\"" + SystemDirectory() +
                             L"\\WindowsPowerShell\\v1.0\\powershell.exe\" -NoLogo -NoProfile -NonInteractive -Command \"[Threading.Thread]::Sleep(60000)\"";
        TmuxProcess process{ result.Callbacks() };
        process.Start(command, SystemDirectory());
        VERIFY_THROWS(process.Write(std::string(TmuxProcess::MaxQueuedBytes + 1, 'x')), std::length_error);

        const auto enqueueStart = std::chrono::steady_clock::now();
        process.Write(std::string(TmuxProcess::MaxQueuedBytes, 'x'));
        VERIFY_IS_TRUE(std::chrono::steady_clock::now() - enqueueStart < 2s);

        const auto closeStart = std::chrono::steady_clock::now();
        process.Close();
        VERIFY_IS_TRUE(std::chrono::steady_clock::now() - closeStart < 5s);
        process.RethrowFailure();
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.exits);
        VERIFY_ARE_EQUAL(uint32_t{ ERROR_PROCESS_ABORTED }, result.exitCode);
    }

    void TmuxProcessTests::SurfacesCallbackFailures()
    {
        ProcessResult result;
        auto callbacks = result.Callbacks();
        callbacks.output = [](std::string_view) {
            throw std::runtime_error{ "callback failure sentinel" };
        };
        TmuxProcess process{ std::move(callbacks) };
        process.Start(Cmd(L"echo output"), SystemDirectory());
        VERIFY_IS_TRUE(result.Wait());
        process.Close();
        VERIFY_IS_TRUE(result.failure != nullptr);
        VERIFY_THROWS(process.RethrowFailure(), std::runtime_error);
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.exits);
    }

    void TmuxProcessTests::AllowsDestructionFromOutputCallback()
    {
        const auto result = std::make_shared<ProcessResult>();
        std::unique_ptr<TmuxProcess> process;
        auto callbacks = result->Callbacks();
        callbacks.output = [&, result](const std::string_view bytes) {
            bool destroy{};
            {
                std::lock_guard lock{ result->mutex };
                result->output.append(bytes);
                destroy = result->output.find("ready") != std::string::npos;
            }
            if (destroy)
            {
                process.reset();
            }
        };
        process = std::make_unique<TmuxProcess>(std::move(callbacks));
        process->Start(Cmd(L"echo ready&set /p again="), SystemDirectory());
        VERIFY_IS_TRUE(result->Wait());
        VERIFY_IS_TRUE(process == nullptr);
        VERIFY_ARE_EQUAL(size_t{ 1 }, result->exits);
    }

    void TmuxProcessTests::RejectsInvalidLaunchArguments()
    {
        const auto launch = [](std::wstring commandLine, std::wstring workingDirectory) {
            TmuxProcess process{ TmuxProcess::Callbacks{} };
            process.Start(std::move(commandLine), std::move(workingDirectory));
        };
        VERIFY_THROWS(launch(L"", SystemDirectory()), std::invalid_argument);
        VERIFY_THROWS(launch(Cmd(L"echo unexpected"), L""), std::invalid_argument);
        VERIFY_THROWS(launch(std::wstring{ L"cmd\0.exe", 8 }, SystemDirectory()), std::invalid_argument);
        VERIFY_THROWS(launch(std::wstring(32767, L'x'), SystemDirectory()), std::invalid_argument);
        VERIFY_THROWS(launch(Cmd(L"echo unexpected"), SystemDirectory() + L"\\TmuxProcessTests-absent-{5A646982-D24C-40A3-B6E2-211BF9279E85}"),
                      wil::ResultException);
    }

    void TmuxProcessTests::RejectsWritesBeforeStartAndRepeatedStart()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        VERIFY_THROWS(process.Write("before start"), std::runtime_error);
        process.Start(Cmd(L"echo started"), SystemDirectory());
        VERIFY_THROWS(process.Start(Cmd(L"echo started again"), SystemDirectory()), std::logic_error);
        VERIFY_IS_TRUE(result.Wait());
        process.Close();
        process.RethrowFailure();
        VERIFY_ARE_EQUAL(size_t{ 1 }, result.exits);
    }

    void TmuxProcessTests::CloseBeforeStartPreventsLaunch()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        process.Close();
        VERIFY_THROWS(process.Start(Cmd(L"echo must not launch"), SystemDirectory()), std::logic_error);
        VERIFY_THROWS(process.Write("closed"), std::runtime_error);
        VERIFY_ARE_EQUAL(size_t{ 0 }, result.exits);
        VERIFY_IS_TRUE(result.output.empty());
    }

    void TmuxProcessTests::CloseCanRaceStart()
    {
        ProcessResult result;
        TmuxProcess process{ result.Callbacks() };
        std::exception_ptr startupFailure;
        std::thread starter{ [&]() {
            try
            {
                process.Start(Cmd(L"set /p wait="), SystemDirectory());
            }
            catch (const std::logic_error&)
            {
                // Close won before Start began.
            }
            catch (...)
            {
                startupFailure = std::current_exception();
            }
        } };
        process.Close();
        starter.join();
        process.Close();
        if (startupFailure)
        {
            std::rethrow_exception(startupFailure);
        }
        VERIFY_IS_TRUE(result.exits <= 1);
    }
}
