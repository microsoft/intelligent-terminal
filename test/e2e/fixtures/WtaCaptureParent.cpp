// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include <windows.h>
#include <objbase.h>
#include <algorithm>
#include <atomic>
#include <chrono>
#include <filesystem>
#include <iostream>
#include <string>
#include <thread>
#include <wil/resource.h>
#include <wil/stl.h>
#include <wil/win32_helpers.h>

#include "../../../src/cascadia/inc/WtaProcess.h"

int wmain(const int argc, wchar_t** argv)
{
    if (argc != 5)
    {
        std::cerr << "Expected WTA executable, command arguments, timeout and ready file.\n";
        return 2;
    }
    const auto timeout = wcstoul(argv[3], nullptr, 10);
    if (timeout == 0 || timeout > 60000)
    {
        std::cerr << "Fixture timeout must be within 1..60000 ms.\n";
        return 2;
    }
    std::atomic<bool> cancelled{ false };
    std::atomic<bool> finished{ false };
    std::thread observer;
    if (std::wstring_view{ argv[4] } != L"-")
    {
        observer = std::thread([&]() {
            while (!finished.load())
            {
                std::error_code error;
                if (std::filesystem::exists(argv[4], error))
                {
                    cancelled.store(true);
                    break;
                }
                std::this_thread::sleep_for(std::chrono::milliseconds{ 10 });
            }
        });
    }
    const auto result = Microsoft::Terminal::WtaProcess::RunWtaCapture(
        argv[1], argv[2], timeout, nullptr, false, &cancelled);
    finished.store(true);
    if (observer.joinable())
    {
        observer.join();
    }
    std::cout << std::boolalpha
              << "{\"completed\":" << result.completed
              << ",\"cancelled\":" << result.cancelled
              << ",\"timedOut\":" << result.timedOut
              << ",\"exitCode\":" << result.exitCode << "}\n";
    return 0;
}
