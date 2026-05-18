// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

#include "pch.h"

#include "WtaProcessLauncher.h"

#include <til/env.h>

namespace WtaProcessLauncher
{
    namespace
    {
        // Create a one-way anonymous pipe with both ends inheritable. Caller
        // then clears HANDLE_FLAG_INHERIT on the side WT keeps.
        void _createInheritablePipe(wil::unique_handle& read,
                                    wil::unique_handle& write)
        {
            SECURITY_ATTRIBUTES sa{};
            sa.nLength = sizeof(sa);
            sa.bInheritHandle = TRUE;
            sa.lpSecurityDescriptor = nullptr;

            HANDLE r{};
            HANDLE w{};
            THROW_IF_WIN32_BOOL_FALSE(CreatePipe(&r, &w, &sa, 0));
            read.reset(r);
            write.reset(w);
        }

        // Strip HANDLE_FLAG_INHERIT so this handle does not propagate to any
        // future child processes spawned by WT.
        void _markNonInheritable(HANDLE h)
        {
            THROW_IF_WIN32_BOOL_FALSE(SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0));
        }
    }

    std::wstring FormatHandleForEnv(HANDLE h)
    {
        return std::to_wstring(reinterpret_cast<uintptr_t>(h));
    }

    PipePair CreateInheritablePipePair()
    {
        // Pipe topology (duplex via two one-way pipes):
        //   Pipe 1 — wta → wt (request flow):
        //     wtRead   = wt-side READ end  (kept by WT, non-inheritable)
        //     wtaWrite = wta-side WRITE end (inherited into wta)
        //   Pipe 2 — wt → wta (response flow):
        //     wtaRead = wta-side READ end  (inherited into wta)
        //     wtWrite = wt-side WRITE end  (kept by WT, non-inheritable)
        PipePair p;
        _createInheritablePipe(p.wtRead, p.wtaWrite);
        _createInheritablePipe(p.wtaRead, p.wtWrite);

        // Lock WT-side ends to non-inheritable BEFORE any subsequent spawn
        // so they cannot leak even via legacy bInheritHandles=TRUE paths.
        _markNonInheritable(p.wtRead.get());
        _markNonInheritable(p.wtWrite.get());
        return p;
    }

    LaunchResult LaunchWta(const LaunchOptions& opts)
    {
        auto pipes = CreateInheritablePipePair();

        // Build env block: parent env + PIPE_R/PIPE_W + caller extras.
        // The wta-side READ handle is what wta reads incoming responses from
        //   — that's WT_PROTOCOL_PIPE_R.
        // The wta-side WRITE handle is what wta writes outgoing requests to
        //   — that's WT_PROTOCOL_PIPE_W.
        auto env = til::env::from_current_environment();
        env.as_map().insert_or_assign(L"WT_PROTOCOL_PIPE_R",
                                      FormatHandleForEnv(pipes.wtaRead.get()));
        env.as_map().insert_or_assign(L"WT_PROTOCOL_PIPE_W",
                                      FormatHandleForEnv(pipes.wtaWrite.get()));
        for (const auto& [k, v] : opts.additionalEnv)
        {
            env.as_map().insert_or_assign(k, v);
        }
        auto envBlock = env.to_string();

        // STARTUPINFOEX: HANDLE_LIST always; PSEUDOCONSOLE optionally.
        STARTUPINFOEXW siEx{};
        siEx.StartupInfo.cb = sizeof(STARTUPINFOEXW);

        if (opts.hidden)
        {
            siEx.StartupInfo.dwFlags |= STARTF_USESHOWWINDOW;
            siEx.StartupInfo.wShowWindow = SW_HIDE;
        }

        const DWORD attrCount = opts.pseudoConsole ? 2 : 1;
        SIZE_T attrSize{};
        // First call returns ERROR_INSUFFICIENT_BUFFER by design.
        InitializeProcThreadAttributeList(nullptr, attrCount, 0, &attrSize);
#pragma warning(suppress : 26414)
        auto attrBuffer = std::make_unique<std::byte[]>(attrSize);
#pragma warning(suppress : 26490)
        siEx.lpAttributeList = reinterpret_cast<PPROC_THREAD_ATTRIBUTE_LIST>(attrBuffer.get());
        THROW_IF_WIN32_BOOL_FALSE(InitializeProcThreadAttributeList(
            siEx.lpAttributeList, attrCount, 0, &attrSize));

        // wta inherits exactly these two handles.
        HANDLE handlesToInherit[2] = { pipes.wtaWrite.get(), pipes.wtaRead.get() };
        THROW_IF_WIN32_BOOL_FALSE(UpdateProcThreadAttribute(
            siEx.lpAttributeList,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
            handlesToInherit,
            sizeof(handlesToInherit),
            nullptr,
            nullptr));

        if (opts.pseudoConsole)
        {
            THROW_IF_WIN32_BOOL_FALSE(UpdateProcThreadAttribute(
                siEx.lpAttributeList,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                opts.pseudoConsole,
                sizeof(HPCON),
                nullptr,
                nullptr));
        }

        // mutable command line (CreateProcessW requires writable buffer)
        std::wstring mutableCmdline = opts.commandLine;

        DWORD creationFlags = EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT;
        if (opts.hidden)
        {
            creationFlags |= CREATE_NO_WINDOW;
        }

        const wchar_t* startingDir = opts.startingDirectory.empty()
            ? nullptr
            : opts.startingDirectory.c_str();

        const wchar_t* exePath = opts.exePath.empty()
            ? nullptr
            : opts.exePath.c_str();

        LaunchResult result;
        const auto cleanupAttrList = wil::scope_exit([&]() {
            DeleteProcThreadAttributeList(siEx.lpAttributeList);
        });

        THROW_IF_WIN32_BOOL_FALSE(CreateProcessW(
            exePath,
            mutableCmdline.data(),
            nullptr,
            nullptr,
            TRUE, // bInheritHandles — required by HANDLE_LIST. The list
                  // CONSTRAINS inheritance to just the listed handles, so
                  // this is strictly safer than legacy raw-TRUE inheritance.
            creationFlags,
            envBlock.empty() ? nullptr : envBlock.data(),
            startingDir,
            &siEx.StartupInfo,
            &result.processInfo));

        // wta now owns its inherited copies; release ours so EOF semantics
        // work correctly when wta exits.
        pipes.wtaWrite.reset();
        pipes.wtaRead.reset();

        result.wtRead = std::move(pipes.wtRead);
        result.wtWrite = std::move(pipes.wtWrite);
        return result;
    }
}
