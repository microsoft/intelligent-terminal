// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// Module Name:
// - TmuxSessionQuery.h
//
// Abstract:
// - Bounded, cancellable one-shot default-server session discovery.

#pragma once

#include <winrt/Windows.Foundation.h>

namespace Microsoft::Terminal::Tmux
{
    // Runs an opaque command off the UI thread with a ten-second deadline.
    // stdout (64 KiB) and stderr (16 KiB) must be valid UTF-8. Only an exact
    // tmux no-default-server diagnostic is treated as an empty successful list.
    // Cancel signals the coroutine; owned-process shutdown never runs on the
    // caller's thread. The result is read only after transport callbacks finish.
    winrt::Windows::Foundation::IAsyncOperation<winrt::hstring> QuerySessionListAsync(winrt::hstring commandline, winrt::hstring workingDirectory);
}
