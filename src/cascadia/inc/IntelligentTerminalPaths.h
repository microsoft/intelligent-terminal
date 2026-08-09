// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// IntelligentTerminalPaths.h — shared resolver for the Intelligent Terminal /
// WTA runtime log directory.
//
// Mirrors wta's Rust `runtime_paths::intelligent_terminal_local_root()` exactly
// so every writer of the WTA logs — the Rust wta processes, the C++ agent-pane
// logger / bug-report-zip action, and the conpty environment injection that
// lets agent-CLI PowerShell hooks find the dir — all target the same folder.
//
// Header-only `inline` so each translation unit picks up its own copy without
// ODR conflicts. Lives in `src/cascadia/inc/` so both TerminalApp and
// TerminalConnection can include it via `../inc/IntelligentTerminalPaths.h`
// without creating a cross-project dependency.

#pragma once

#include <windows.h>
#include <appmodel.h>

#include <filesystem>
#include <string>
#include <system_error>
#include <vector>

namespace IntelligentTerminal
{
    // Resolve the WTA log directory:
    //
    //   * Packaged:   %LOCALAPPDATA%\Packages\<PackageFamilyName>\LocalCache\Local\IntelligentTerminal\logs
    //   * Unpackaged: %LOCALAPPDATA%\IntelligentTerminal\logs
    //
    // Logs are transient cache, hence `LocalCache\Local` (not `LocalState`,
    // which holds persistent state like the agent-pane session index). Mirrors
    // WTA by falling back to `%APPDATA%`, then the process temp directory, when
    // `%LOCALAPPDATA%` is unavailable.
    inline std::filesystem::path LogDir()
    {
        const auto environmentPath = [](const wchar_t* name) {
            const auto required = GetEnvironmentVariableW(name, nullptr, 0);
            if (required == 0)
            {
                return std::filesystem::path{};
            }

            std::wstring value(required, L'\0');
            const auto copied = GetEnvironmentVariableW(name, value.data(), required);
            if (copied == 0 || copied >= required)
            {
                return std::filesystem::path{};
            }

            value.resize(copied);
            return std::filesystem::path{ std::move(value) };
        };

        auto base = environmentPath(L"LOCALAPPDATA");
        if (base.empty())
        {
            base = environmentPath(L"APPDATA");
        }
        if (base.empty())
        {
            std::error_code error;
            base = std::filesystem::temp_directory_path(error);
            if (error)
            {
                return {};
            }
            return base / L"IntelligentTerminal" / L"logs";
        }

        // Two-call pattern: query the family-name length first. A packaged
        // process returns ERROR_INSUFFICIENT_BUFFER and fills `length`; an
        // unpackaged one returns APPMODEL_ERROR_NO_PACKAGE.
        UINT32 length = 0;
        if (GetCurrentPackageFamilyName(&length, nullptr) == ERROR_INSUFFICIENT_BUFFER && length != 0)
        {
            std::wstring family(length, L'\0');
            if (GetCurrentPackageFamilyName(&length, family.data()) == ERROR_SUCCESS)
            {
                family.resize(::wcslen(family.c_str())); // drop trailing NUL(s)
                return base / L"Packages" / family / L"LocalCache" / L"Local" / L"IntelligentTerminal" / L"logs";
            }
        }
        return base / L"IntelligentTerminal" / L"logs";
    }

    // The current process's package version as `"Major.Minor.Build.Revision"`
    // (e.g. `"0.8.0.2"`), or an empty string when unpackaged. This is the
    // shared per-version key — wta's Rust `logging::package_version()` reads
    // the same value via `GetCurrentPackageId`, so the Rust processes, this
    // C++ logger, and (through `WTA_HOOK_LOG_DIR`) the PowerShell hooks all
    // resolve to the same `logs\<pkgver>\` folder.
    inline std::wstring PackageVersionDir()
    {
        UINT32 length = 0;
        if (GetCurrentPackageId(&length, nullptr) == ERROR_INSUFFICIENT_BUFFER && length != 0)
        {
            // PACKAGE_ID contains a UINT64 + pointers, so back the buffer with
            // UINT64 storage to guarantee 8-byte alignment — a `vector<BYTE>`
            // is only byte-aligned and reinterpreting it as PACKAGE_ID* is UB.
            std::vector<UINT64> buffer((length + sizeof(UINT64) - 1) / sizeof(UINT64));
            const auto bytes = reinterpret_cast<BYTE*>(buffer.data());
            if (GetCurrentPackageId(&length, bytes) == ERROR_SUCCESS)
            {
                const auto id = reinterpret_cast<const PACKAGE_ID*>(bytes);
                return std::to_wstring(id->version.Major) + L"." +
                       std::to_wstring(id->version.Minor) + L"." +
                       std::to_wstring(id->version.Build) + L"." +
                       std::to_wstring(id->version.Revision);
            }
        }
        return {};
    }

    // The per-version log directory: `LogDir()\<pkgver>` when packaged, or the
    // bare `LogDir()` when unpackaged. Use this for the actual log writers
    // (agent-pane logger, hook env injection). `LogDir()` itself stays the
    // root so the bug-report zip can archive every version's logs at once.
    inline std::filesystem::path LogDirVersioned()
    {
        auto root = LogDir();
        if (root.empty())
        {
            return root;
        }
        const auto version = PackageVersionDir();
        return version.empty() ? root : (root / version);
    }
}
