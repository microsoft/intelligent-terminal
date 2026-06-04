// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// ShellIntegration.h
//
// Umbrella header for the shell-integration installer. Includes:
//   • ShellIntegrationCommon.h     — types, markers, FlavorDescriptor + generic driver
//   • PowerShellShellIntegration.h — PowerShell 5.1 / 7 flavor
//   • BashShellIntegration.h       — Git Bash flavor
//   • WslShellIntegration.h        — per-distro WSL bash flavor (delegates to Bash)
//
// Also defines the top-level dispatchers (InstallForTarget / UninstallForTarget)
// keyed on Target, and provides flat-namespace aliases for every public
// symbol callers and tests reference (kept to minimize churn in existing
// code — anything `SI::Install`, `SI::InstallBash`, `SI::FindShellIntegrationBlock`
// etc. still resolves here without touching call sites).
//
// Adding a new shell:
//   1. Drop in <NewShell>ShellIntegration.h that builds a FlavorDescriptor
//      and exposes Install / Uninstall / DiscoverProfilePath / etc.
//   2. Include it below.
//   3. Extend Target + the dispatchers if it has a natural single-target shape
//      (like Bash), OR expose a per-instance API (like Wsl::Install(distName)).
//   4. Add per-flavor TEST_METHODs that delegate to the shared
//      _RunScenario_* helpers in ShellIntegrationTests.cpp.

#pragma once

#include "ShellIntegrationCommon.h"
#include "PowerShellShellIntegration.h"
#include "BashShellIntegration.h"
#include "WslShellIntegration.h"

namespace Microsoft::Terminal::ShellIntegration
{
    // ───────────────────────────────────────────────────────────────────
    // Top-level dispatchers — keyed on Target. Bash has no execution
    // policy gate (no equivalent in bash); PS does.
    // ───────────────────────────────────────────────────────────────────

    inline InstallResult InstallForTarget(Target target)
    {
        if (target == Target::Bash)
        {
            return Bash::InstallForTarget();
        }
        return Powershell::InstallForTarget(target);
    }

    inline InstallResult UninstallForTarget(Target target)
    {
        if (target == Target::Bash)
        {
            return Bash::UninstallForTarget();
        }
        return Powershell::UninstallForTarget(target);
    }

    // ═══════════════════════════════════════════════════════════════════
    // Back-compat aliases — flat-namespace names that pre-refactor code
    // and tests reference. New code should call the per-flavor
    // namespaces directly (`Powershell::Install`, `Bash::Install`, etc).
    // ═══════════════════════════════════════════════════════════════════

    // PowerShell.
    inline InstallResult Install(const std::wstring& profilePathW) { return Powershell::Install(profilePathW); }
    inline InstallResult Uninstall(const std::wstring& profilePathW) { return Powershell::Uninstall(profilePathW); }
    inline std::wstring DiscoverProfilePath(Target target) { return Powershell::DiscoverProfilePath(target); }
    inline bool ExecutionPolicyBlocksShellIntegration(Target target) noexcept { return Powershell::ExecutionPolicyBlocksShellIntegration(target); }
    inline constexpr int kShellIntegrationVersion = Powershell::kVersion;
    inline std::wstring ShellIntegrationScriptFileName() { return Powershell::ScriptFileName(); }
    inline std::string ShellIntegrationScriptContent() { return Powershell::ScriptContent(); }
    inline std::string BuildShellIntegrationBlock(std::wstring_view profileSubdir, std::string_view eol)
    {
        return Powershell::BuildBlock(profileSubdir, eol);
    }
    // Free-function wrapper around FindBlockGeneric with the PS flavor
    // (orphan-body recognizer + legacy regex). Used by tests that only
    // care about the parser, not the full Install flow.
    inline std::pair<size_t, size_t> FindShellIntegrationBlock(std::string_view contents)
    {
        FlavorDescriptor d{};
        d.isOrphanBodyLine = &Powershell::IsOrphanBodyLine;
        d.findLegacy = &Powershell::FindLegacyDotSource;
        return FindBlockGeneric(contents, d);
    }

    // Bash.
    inline constexpr int kShellIntegrationBashVersion = Bash::kVersion;
    inline std::wstring ShellIntegrationBashScriptFileName() { return Bash::ScriptFileName(); }
    inline std::wstring BashScriptDir() { return Bash::ScriptDir(); }
    inline std::wstring DiscoverBashProfilePath() { return Bash::DiscoverProfilePath(); }
    inline std::string ShellIntegrationBashScriptContent() { return Bash::ScriptContent(); }
    inline std::string BuildShellIntegrationBashBlock() { return Bash::BuildBlock("\n"); }
    inline std::pair<size_t, size_t> FindShellIntegrationBashBlock(std::string_view contents)
    {
        FlavorDescriptor d{};
        d.isOrphanBodyLine = &Bash::IsOrphanBodyLine;
        return FindBlockGeneric(contents, d);
    }
    inline InstallResult InstallBash(const std::wstring& profilePathW, const std::wstring& scriptDirW)
    {
        return Bash::Install(profilePathW, scriptDirW);
    }
    inline InstallResult UninstallBash(const std::wstring& profilePathW) { return Bash::Uninstall(profilePathW); }

    // WSL.
    inline std::wstring WslUncPath(std::wstring_view distName, std::string_view posixPath) { return Wsl::UncPath(distName, posixPath); }
    inline InstallResult InstallWslBash(const std::wstring& distName) { return Wsl::Install(distName); }
    inline InstallResult UninstallWslBash(const std::wstring& distName) { return Wsl::Uninstall(distName); }

    // Re-expose per-flavor details under the top-level `details::` namespace
    // so existing tests that reference `details::QueryExecutionPolicy(...)`
    // / `details::IsSafeWslDistroName(...)` keep compiling unchanged.
    namespace details
    {
        inline std::wstring QueryExecutionPolicy(LPCWSTR exe) noexcept { return Powershell::details::QueryExecutionPolicy(exe); }
        inline bool PolicyNameBlocksUnsignedScripts(std::wstring_view name) noexcept { return Powershell::details::PolicyNameBlocksUnsignedScripts(name); }
        inline bool IsSafeWslDistroName(std::wstring_view name) noexcept { return Wsl::details::IsSafeDistroName(name); }
        inline bool IsSafeWslHome(std::string_view home) noexcept { return Wsl::details::IsSafeHome(home); }
    }
}
