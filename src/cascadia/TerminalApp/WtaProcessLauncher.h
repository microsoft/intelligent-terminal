// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// WtaProcessLauncher
// ==================
//
// Spawns a wta.exe child with a private duplex pipe pair inherited via
// STARTUPINFOEX PROC_THREAD_ATTRIBUTE_HANDLE_LIST. The wta-side handles are
// passed to the child as env vars WT_PROTOCOL_PIPE_R and WT_PROTOCOL_PIPE_W
// (decimal HANDLE values). The wt-side handles are returned to the caller for
// use with TerminalProtocolPipeServer.
//
// Anonymous pipes (CreatePipe x2) — no namespace, no DACL, no name to leak.
// Capability rests entirely in the inherited handles; nothing else (e.g. an
// env var that any child of any pane can read) suffices to obtain SendInput.

#pragma once

#include <map>
#include <string>
#include <wil/resource.h>

namespace WtaProcessLauncher
{
    // Output of CreateInheritablePipePair: the duplex anonymous pipe pair,
    // split by ownership. Caller passes the wta-side handles to the spawning
    // process (via STARTUPINFOEX HANDLE_LIST + WT_PROTOCOL_PIPE_R/W env vars)
    // and keeps the wt-side handles to drive a TerminalProtocolPipeServer.
    struct PipePair
    {
        wil::unique_handle wtRead;   // WT reads incoming requests from wta
        wil::unique_handle wtWrite;  // WT writes responses to wta
        wil::unique_handle wtaRead;  // wta reads incoming responses from WT (inheritable)
        wil::unique_handle wtaWrite; // wta writes outgoing requests to WT (inheritable)
    };

    // Creates the duplex anonymous pipe pair (CreatePipe x2). Marks the
    // wt-side ends non-inheritable. The wta-side ends are left inheritable
    // so the spawning code can place them in PROC_THREAD_ATTRIBUTE_HANDLE_LIST.
    PipePair CreateInheritablePipePair();

    // Format a HANDLE as decimal text (matching what the Rust side parses
    // via u64::from_str_radix(.., 10) → cast to HANDLE).
    std::wstring FormatHandleForEnv(HANDLE h);

    struct LaunchOptions
    {
        // Full path to wta.exe (used as lpApplicationName).
        std::wstring exePath;

        // Full command line. Caller is responsible for argv[0] quoting.
        std::wstring commandLine;

        // Optional starting directory; empty = inherit from parent.
        std::wstring startingDirectory;

        // If true, the new process is hidden (SW_HIDE / CREATE_NO_WINDOW).
        bool hidden{ false };

        // Extra env vars to splice into the inherited environment, beyond the
        // two pipe handle vars added automatically.
        std::map<std::wstring, std::wstring> additionalEnv;

        // If non-null, attaches a pseudoconsole to the launched process
        // (agent-pane case). Bumps attribute count from 1 to 2.
        HPCON pseudoConsole{ nullptr };
    };

    struct LaunchResult
    {
        // WT reads JSON-RPC requests from wta on this handle.
        wil::unique_handle wtRead;

        // WT writes JSON-RPC responses to wta on this handle.
        wil::unique_handle wtWrite;

        // The spawned process. Caller takes ownership; close hThread once
        // launch succeeds, hold hProcess for lifetime tracking.
        wil::unique_process_information processInfo;
    };

    // Creates the pipe pair, marks inheritance, builds STARTUPINFOEX, calls
    // CreateProcessW with bInheritHandles=TRUE + EXTENDED_STARTUPINFO_PRESENT.
    // After success, wta-side handles have been closed in this process.
    //
    // Throws (via wil) on any Win32 failure.
    LaunchResult LaunchWta(const LaunchOptions& opts);
}
