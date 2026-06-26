// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
//
// BashShellIntegration.h
//
// Bash flavor (Git Bash on Windows) of the shell integration installer.
// Exposes BashFlavor — a concrete IShellFlavor that the orchestrator
// drives.
//
// Key differences from PowerShell:
//   • No execution-policy gate (bash has no equivalent concept).
//   • LF line endings only (bash files are never CRLF).
//   • Script lives in a dedicated dir (~/.intelligent-terminal/),
//     NOT next to .bashrc. Uninstall removes the sourced BLOCK from
//     ~/.bashrc — it intentionally leaves the versioned script files
//     in ~/.intelligent-terminal/ in place to support side-by-side
//     rollback (matches PowerShell's per-version $PROFILE-adjacent
//     files). Users who want a full sweep can delete the directory
//     by hand; the doc covers this.
//   • The block guards on $BASH_VERSION + interactive-shell + script
//     existence so the same .bashrc roams safely to machines that
//     don't have IT installed, or to non-bash shells that happen to
//     source .bashrc.
//   • Every variable read in the script body uses ${VAR:-} defaulting
//     so it sources cleanly even when the user has `set -u` earlier.
//
// WSL bash uses this same flavor — see WslShellIntegration.h. Per-distro
// installation just resolves the in-distro $HOME and writes via the
// \\wsl$\<distro>\ UNC path.

#pragma once

#include "ShellIntegrationCommon.h"

namespace Microsoft::Terminal::ShellIntegration::Bash
{
    // v2: added OSC 9001;ShellType emission (bash/WSL self-reports identity
    // each prompt). Bumped from v1 so existing users — whose ~/.bashrc
    // already references the v1 script byte-for-byte — get the new script
    // rewritten in; without the bump the orchestrator's block-match early-
    // out would leave the stale v1 script (no ShellType) in place. WSL
    // inherits this version via WslBashFlavor.
    inline constexpr int kVersion = 2;

    inline std::wstring ScriptFileName()
    {
        return L"shell-integration_v" + std::to_wstring(kVersion) + L".sh";
    }

    // Where the bash script lives on disk by default.
    // %USERPROFILE%\.intelligent-terminal\ — resolved via
    // SHGetKnownFolderPath(FOLDERID_Profile) so it follows any
    // group-policy profile redirection. A dedicated subdir keeps
    // uninstall trivial and avoids polluting %USERPROFILE% root.
    inline std::wstring ScriptDir()
    {
        wil::unique_cotaskmem_string profileFolder;
        if (FAILED(SHGetKnownFolderPath(FOLDERID_Profile, 0, nullptr, &profileFolder)) || !profileFolder)
        {
            return {};
        }
        std::filesystem::path p{ profileFolder.get() };
        p /= L".intelligent-terminal";
        return p.wstring();
    }

    // Discover ~/.bashrc. We standardize on .bashrc (NOT .bash_profile):
    //   • Git Bash on Windows creates a default .bash_profile that
    //     sources .bashrc, so .bashrc runs in both login and non-login
    //     shells out of the box.
    //   • .bashrc is the documented per-user interactive-shell rc; it's
    //     what every shell-integration guide (including ours) targets.
    inline std::wstring DiscoverProfilePath()
    {
        wil::unique_cotaskmem_string profileFolder;
        if (FAILED(SHGetKnownFolderPath(FOLDERID_Profile, 0, nullptr, &profileFolder)) || !profileFolder)
        {
            return {};
        }
        std::filesystem::path p{ profileFolder.get() };
        p /= L".bashrc";
        return p.wstring();
    }

    // The bash script content. Compatible with bash 3.2+ (POSIX-leaning
    // where possible). Safe to source multiple times. Silently no-ops in
    // non-interactive shells and non-bash shells.
    //
    // The script is intentionally tiny — same OSC sequences the PS
    // script emits, so the autofix / VT-event pipeline downstream is
    // shell-agnostic and needs no changes.
    inline std::string ScriptContent()
    {
        return std::string{
            R"(# Shell Integration for Intelligent Terminal — bash
# Emits OSC 133 (command marks / exit code) and OSC 9;9 (CWD) sequences
# WITHOUT altering the visual appearance of the user's prompt.
#
# Compatible with bash 3.2+. Safe to source multiple times.
# Silently no-ops in non-interactive shells and non-bash shells.
# Every variable read uses ${VAR:-} defaulting so the script is safe
# even when the user has `set -u` earlier in their .bashrc.

# Guard: bash only, interactive only, idempotent.
[ -z "${BASH_VERSION:-}" ] && return 0 2>/dev/null
case "${-:-}" in *i*) ;; *) return 0 2>/dev/null ;; esac
[ -n "${__IT_SHELLINTEG_INSTALLED:-}" ] && return 0 2>/dev/null
__IT_SHELLINTEG_INSTALLED=1

# Snapshot the user's PROMPT_COMMAND once; we re-run it from our wrapper
# so we don't clobber any existing hook (starship, oh-my-bash, etc).
# Walk the array element-by-element and join the NON-EMPTY entries
# with ';'. Joining with IFS via ${ARR[*]} would include empty array
# slots verbatim, producing leading or embedded ';' tokens that eval
# rejects with "syntax error near unexpected token \;'". Ubuntu's
# /etc/profile.d/80-systemd-osc-context.sh deliberately reserves
# PROMPT_COMMAND[0]='' before appending its hook, and that empty slot
# was the trigger.
#
# "${PROMPT_COMMAND[@]}" expands to one element on a scalar var, zero
# elements when unset, and N elements on an array -- the same loop
# handles bash 3.2+ scalar and bash 5.1+ array uniformly. The
# ${PROMPT_COMMAND[@]+...} alternate-value guard keeps it `set -u`
# safe: when PROMPT_COMMAND is unset the whole expansion collapses to
# nothing instead of tripping "unbound variable" on bash 3.2 (a plain
# "${PROMPT_COMMAND[@]}" errors there). ${PROMPT_COMMAND:-} can't be
# used: scalar ${VAR:-} defaulting only sees element [0] of an array,
# which is exactly the empty-slot case this loop exists to handle.
__IT_SHELLINTEG_USER_PC=""
for __it_pc_entry in ${PROMPT_COMMAND[@]+"${PROMPT_COMMAND[@]}"}; do
    [ -z "$__it_pc_entry" ] && continue
    if [ -z "$__IT_SHELLINTEG_USER_PC" ]; then
        __IT_SHELLINTEG_USER_PC="$__it_pc_entry"
    else
        __IT_SHELLINTEG_USER_PC="$__IT_SHELLINTEG_USER_PC;$__it_pc_entry"
    fi
done
unset __it_pc_entry

__it_shellinteg_prompt() {
    local __ec=$?
    # OSC 133;D;<ec>  — previous command finished
    # OSC 133;A       — prompt start
    # OSC 9;9;"cwd"   — current working directory
    # OSC 9;9 unquoted form (no surrounding double quotes around the
    # path). Linux paths can contain `"` (only `/` and NUL are
    # forbidden), and Terminal's 9;9 parser rejects the quoted form
    # when the payload contains embedded quotes — silently dropping
    # CWD reporting for those directories. The unquoted form parses
    # cleanly regardless of path contents.
    printf '\033]133;D;%s\007\033]133;A\007\033]9;9;%s\007' "$__ec" "${PWD:-}"
    # OSC 9001;ShellType — report shell identity each prompt so the terminal
    # always knows which shell owns the pane, even after a nested shell exits.
    # Under WSL, $WSL_DISTRO_NAME is set so we report "wsl:<distro>"; plain
    # (Git) bash reports "bash".
    if [ -n "${WSL_DISTRO_NAME:-}" ]; then
        printf '\033]9001;ShellType;wsl:%s;%s\007' "$WSL_DISTRO_NAME" "${BASH_VERSION:-}"
    else
        printf '\033]9001;ShellType;bash;%s\007' "${BASH_VERSION:-}"
    fi
    if [ -n "$__IT_SHELLINTEG_USER_PC" ]; then
        # Restore $? for the user's PROMPT_COMMAND so hooks like
        # `local ec=$?` at its top still see the real exit code
        # instead of printf's success status.
        (exit "$__ec"); eval "$__IT_SHELLINTEG_USER_PC"
    fi
}
PROMPT_COMMAND=__it_shellinteg_prompt

# Append OSC 133;B (command-input start) to PS1, AFTER the user prompt
# expands. The \[ \] brackets tell readline these bytes are zero-width
# so line-wrap math stays correct. The case guard makes re-sourcing
# idempotent. ${PS1:-} defaulting handles the rare `set -u` case.
__it_shellinteg_b=$'\033]133;B\007'
case "${PS1:-}" in
    *"$__it_shellinteg_b"*) ;;
    *) PS1="${PS1:-}\[${__it_shellinteg_b}\]" ;;
esac
unset __it_shellinteg_b
)"
        };
    }

    // Build the .bashrc block. The eol parameter is ignored (bash files
    // are always LF; the BashFlavor advertises LineEndingPolicy::Lf so
    // the orchestrator passes us "\n" regardless of what's already in
    // the file). Preexisting CRLF in the user's content OUTSIDE our
    // block is preserved as-is; the installer never normalizes the
    // whole file.
    inline std::string BuildBlock(std::string_view /*eol*/)
    {
        const auto fileName = til::u16u8(ScriptFileName());

        std::string block;
        block += kShellIntegrationBlockOpenMarker;                                          block += "\n";
        block += "# Auto-generated by Intelligent Terminal. Do not edit between markers.";  block += "\n";
        block += "# Sources a versioned script under $HOME so this is machine-portable";    block += "\n";
        block += "# and a silent no-op when the script file is missing.";                   block += "\n";
        block += "if [ -n \"${BASH_VERSION:-}\" ]; then";                                  block += "\n";
        block += "    __it_si=\"${HOME:-}/.intelligent-terminal/";
        block += fileName;
        block += "\"";                                                                      block += "\n";
        block += "    [ -f \"$__it_si\" ] && . \"$__it_si\"";                               block += "\n";
        block += "    unset __it_si";                                                       block += "\n";
        block += "fi";                                                                      block += "\n";
        block += kShellIntegrationBlockCloseMarker;
        return block;
    }

    namespace details
    {
        // Body-line recognizer for orphan-marker recovery. Matches the
        // exact set of lines BuildBlock emits, plus the closing `fi`
        // line (treated as an exact match — short common token, would
        // be too promiscuous as a prefix).
        inline bool IsOrphanBodyLine(std::string_view candidate) noexcept
        {
            constexpr std::array<std::string_view, 7> bodyPrefixes = {
                std::string_view{ "# Auto-generated by Intelligent Terminal" },
                std::string_view{ "# Sources a versioned script under $HOME" },
                std::string_view{ "# and a silent no-op when the script file is missing." },
                std::string_view{ "if [ -n \"${BASH_VERSION:-}\" ]; then" },
                std::string_view{ "    __it_si=" },
                std::string_view{ "    [ -f \"$__it_si\" ]" },
                std::string_view{ "    unset __it_si" },
            };
            for (const auto& prefix : bodyPrefixes)
            {
                if (candidate.size() >= prefix.size() &&
                    candidate.substr(0, prefix.size()) == prefix)
                {
                    return true;
                }
            }
            return candidate == std::string_view{ "fi" };
        }
    }

    // Concrete IShellFlavor for bash. Two paths in the ctor:
    //   • profilePath  — the .bashrc to inject into. For native Git
    //                    Bash this is DiscoverProfilePath(); for WSL
    //                    it's a \\wsl$\<distro>\…\.bashrc UNC path.
    //   • scriptDir    — where the versioned .sh file is written.
    //                    For native: ScriptDir(). For WSL: a UNC path
    //                    into ~/.intelligent-terminal/ inside the
    //                    distro.
    class BashFlavor : public IShellFlavor
    {
    public:
        BashFlavor(std::wstring profilePath, std::filesystem::path scriptDir) :
            _profilePath{ std::move(profilePath) },
            _scriptDir{ std::move(scriptDir) }
        {
        }

        std::wstring          ProfilePath() const override          { return _profilePath; }
        std::filesystem::path ScriptDir() const override            { return _scriptDir; }
        std::wstring          ScriptFileName() const override       { return Bash::ScriptFileName(); }
        std::string           ScriptContent() const override        { return Bash::ScriptContent(); }
        std::wstring          ProfileFriendlyName() const override  { return L".bashrc"; }
        LineEndingPolicy      LineEndings() const override          { return LineEndingPolicy::Lf; }

        std::string ScriptBlock(std::string_view eol) const override
        {
            return Bash::BuildBlock(eol);
        }

        std::optional<std::pair<size_t, size_t>>
        FindExistingScriptBlock(std::string_view contents) const override
        {
            return ::Microsoft::Terminal::ShellIntegration::details::FindBlock(
                contents,
                &details::IsOrphanBodyLine,
                nullptr); // bash v1 is the first release — no legacy form to migrate
        }

    private:
        std::wstring _profilePath;
        std::filesystem::path _scriptDir;
    };

    // Path-taking convenience. Used by both the umbrella InstallBash /
    // UninstallBash flat aliases (tests) and the WSL flow (which
    // resolves UNC paths first).
    inline InstallResult Install(const std::wstring& profilePathW, const std::wstring& scriptDirW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        if (scriptDirW.empty())
        {
            return { false, false, L"Script directory is empty" };
        }
        BashFlavor flavor{ profilePathW, std::filesystem::path{ scriptDirW } };
        return orchestrator::Install(flavor);
    }

    inline InstallResult Uninstall(const std::wstring& profilePathW)
    {
        if (profilePathW.empty())
        {
            return { false, false, L"Profile path is empty" };
        }
        // ScriptDir is only consulted by Install (for writing the
        // script). Uninstall just strips the block, so we pass any
        // directory — use the default so the flavor is coherent.
        BashFlavor flavor{ profilePathW, std::filesystem::path{ ScriptDir() } };
        return orchestrator::Uninstall(flavor);
    }

    // Convenience: discover + install. Target::Bash dispatches here
    // from the umbrella ShellIntegration.h InstallForTarget.
    inline InstallResult InstallForTarget()
    {
        auto profilePath = DiscoverProfilePath();
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover bash profile path (.bashrc)" };
        }
        return Install(profilePath, ScriptDir());
    }

    inline InstallResult UninstallForTarget()
    {
        auto profilePath = DiscoverProfilePath();
        if (profilePath.empty())
        {
            return { false, false, L"Could not discover bash profile path (.bashrc)" };
        }
        return Uninstall(profilePath);
    }
}
